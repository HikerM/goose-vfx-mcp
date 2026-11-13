use lumina_local_inference::lifecycle::*;
use serde_json::{json, Value};
use std::collections::BTreeSet;

const RESULT: &str = "019a1111-2222-8333-8444-555555555555";

#[path = "support/audio_task_reuse.rs"]
mod audio_task_reuse;

fn binding(profile: AudioTaskProfile) -> AudioTaskModelBinding {
    let (sha256, size_bytes) = profile.recorded_model();
    let model = AudioTaskAssetBinding {
        artifact_id: "model".into(),
        sha256,
        size_bytes,
    };
    let (sha256, size_bytes) = profile.recorded_model_spec();
    AudioTaskModelBinding {
        installation_id: "test-installation".into(),
        version_id: "test-v1".into(),
        manifest_digest: Sha256Digest::of_bytes(b"synthetic manifest; no model execution"),
        runtime: profile.runtime_build(),
        target_capabilities: BTreeSet::from([profile.capability()]),
        model,
        model_spec: AudioTaskAssetBinding {
            artifact_id: "spec".into(),
            sha256,
            size_bytes,
        },
    }
}

fn input() -> AudioTaskMediaReference {
    AudioTaskMediaReference {
        session_id: "session-a".into(),
        result_id: RESULT.into(),
        sha256: Sha256Digest::parse(
            "b29307e631d805d7079910c8c9c5ee14777549954b737e15a5770db7ac2da2e9",
        )
        .unwrap(),
        mime_type: "audio/wav".into(),
        size_bytes: 165_004,
        sample_rate: 16_000,
        channels: 1,
        sample_frames: 82_480,
    }
}

fn tts() -> PocketTtsRequest {
    PocketTtsRequest {
        session_id: "session-a".into(),
        job_id: "job-tts".into(),
        text: "The red apple is on the wooden table.".into(),
        voice_reference: input(),
        seed: Some(42),
        temperature: None,
        max_frames: None,
        text_chunk_tokens: None,
        frames_after_eos: None,
    }
}

fn asr() -> Qwen3AsrRequest {
    Qwen3AsrRequest {
        session_id: "session-a".into(),
        job_id: "job-asr".into(),
        audio: input(),
        context: None,
        language: None,
        max_tokens: None,
        chunking: None,
    }
}

fn tts_service() -> PocketTtsParameterService {
    PocketTtsParameterService::new(binding(AudioTaskProfile::PocketTtsEnglishCpu)).unwrap()
}
fn asr_service() -> Qwen3AsrParameterService {
    Qwen3AsrParameterService::new(binding(AudioTaskProfile::Qwen3Asr06bCpu)).unwrap()
}
fn flag<'a>(args: &'a [NativeAudioArgument], name: &str) -> &'a NativeAudioArgument {
    let index = args
        .iter()
        .position(|v| v == &NativeAudioArgument::Literal(name.into()))
        .unwrap();
    &args[index + 1]
}
fn string(value: &str) -> NativeAudioArgument {
    NativeAudioArgument::Literal(value.into())
}

#[test]
fn external_audio_models_select_their_own_language_options_and_defaults() {
    const TEST: &str = "external_audio_models_select_their_own_language_options_and_defaults";
    if std::env::var_os("LUMINA_AUDIO_CONTRACT_CHILD").is_none() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = dir.path().join("catalog.json");
        let models = [
            ("speech-fr", json!({"protocol":"pocket_tts_offline","language":"french","temperature":0.7})),
            ("speech-es", json!({"protocol":"pocket_tts_offline","language":"spanish","temperature":0.5})),
            ("asr-zh", json!({"protocol":"qwen3_asr_offline","languages":["chinese"],"defaultLanguage":"chinese","maxTokens":128})),
        ].map(|(name, parameters)| json!({
            "name":name, "repository":"example/audio", "revision":"a".repeat(40),
            "file":format!("{name}.gguf"), "sha256":Sha256Digest::of_bytes(name.as_bytes()),
            "sizeBytes":1024, "parameters":parameters
        }));
        std::fs::write(
            &catalog,
            serde_json::to_vec(&json!({"schemaVersion":1, "models":models})).unwrap(),
        )
        .unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("LUMINA_AUDIO_CATALOG", &catalog)
            .env("LUMINA_AUDIO_CONTRACT_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success() && String::from_utf8_lossy(&result.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let profile = |name| {
        AudioTaskProfile::registered()
            .find(|p| p.installation_name() == name)
            .unwrap()
    };
    for (name, language, temperature) in [
        ("speech-fr", "french", 0.7_f32),
        ("speech-es", "spanish", 0.5),
    ] {
        let selected = profile(name);
        let service = PocketTtsParameterService::new(binding(selected)).unwrap();
        let descriptor = service.descriptor();
        assert_eq!(descriptor.model_language, language);
        assert_eq!(
            descriptor.fields["temperature"].default,
            Some(json!(temperature))
        );
        let task = service.compile(tts()).unwrap();
        assert_eq!(
            flag(task.arguments(), "--load-option"),
            &string(&format!("language={language}"))
        );
        assert_eq!(task.snapshot().parameters.temperature, temperature);
        assert_eq!(
            serde_json::from_value::<AudioTaskProfile>(json!(selected)).unwrap(),
            selected
        );
    }
    let service = Qwen3AsrParameterService::new(binding(profile("asr-zh"))).unwrap();
    assert_eq!(
        service.descriptor().recognition_languages,
        [Qwen3AsrLanguage::Chinese]
    );
    let task = service.compile(asr()).unwrap();
    assert_eq!(
        task.snapshot().parameters.language,
        Qwen3AsrLanguage::Chinese
    );
    assert_eq!(task.snapshot().parameters.max_tokens, 128);
    let mut request = asr();
    request.language = Some(Qwen3AsrLanguage::English);
    assert!(service.compile(request.clone()).is_err());
    assert!(asr_service().compile(request).is_ok());
    assert_ne!(
        profile("speech-fr").runtime_build().binding_version,
        profile("speech-es").runtime_build().binding_version
    );
    assert!(serde_json::from_value::<AudioTaskProfile>(json!("unknown")).is_err());
}

#[test]
fn protocol_dispatch_rejects_cross_family_registration_and_preserves_old_versions() {
    let tts = AudioTaskProfile::PocketTtsEnglishCpu;
    let asr = AudioTaskProfile::Qwen3Asr06bCpu;
    assert_eq!(tts.protocol(), AudioTaskProtocol::PocketTtsOffline);
    assert_eq!(asr.protocol(), AudioTaskProtocol::Qwen3AsrOffline);
    for profile in AudioTaskProfile::registered() {
        assert_eq!(
            AudioTaskProfile::for_parameter_schema(&profile.runtime_build().binding_version),
            Some(profile)
        );
    }
    assert!(AudioTaskProfile::for_parameter_schema("unsupported-schema").is_none());
    assert!(PocketTtsParameterService::for_profile(asr, binding(asr)).is_err());
    assert!(Qwen3AsrParameterService::for_profile(tts, binding(tts)).is_err());
    for runtime in [tts.legacy_runtime_build(), tts.runtime_build()] {
        let mut installed = binding(tts);
        installed.runtime = runtime;
        let from_catalog = PocketTtsParameterService::for_profile(tts, installed.clone()).unwrap();
        assert_eq!(
            from_catalog.descriptor(),
            PocketTtsParameterService::new(installed)
                .unwrap()
                .descriptor()
        );
    }
    for runtime in [asr.legacy_runtime_build(), asr.runtime_build()] {
        let mut installed = binding(asr);
        installed.runtime = runtime;
        let from_catalog = Qwen3AsrParameterService::for_profile(asr, installed.clone()).unwrap();
        assert_eq!(
            from_catalog.descriptor(),
            Qwen3AsrParameterService::new(installed)
                .unwrap()
                .descriptor()
        );
    }
}

#[test]
fn audio_profiles_match_independent_recorded_binary_weights_and_explicit_specs() {
    let evidence: Value = serde_json::from_str(include_str!("../../../documentation/docs/lumina-architecture/model-lifecycle-evidence/2026-11-01-audio-asr-tts-feasibility.json")).unwrap();
    let long_paths: Value = serde_json::from_str(include_str!("../../../documentation/docs/lumina-architecture/model-lifecycle-evidence/2026-11-13-m4-audio-windows-longpath.json")).unwrap();
    for (profile, family) in [
        (AudioTaskProfile::PocketTtsEnglishCpu, "pocket_tts"),
        (AudioTaskProfile::Qwen3Asr06bCpu, "qwen3_asr"),
    ] {
        let model = evidence["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["family"] == family)
            .unwrap();
        assert_eq!(
            profile.legacy_runtime_build().binary_sha256.as_str(),
            evidence["binarySha256"].as_str().unwrap()
        );
        assert_eq!(
            profile.runtime_build().binary_sha256.as_str(),
            long_paths["package"]["independentRepackageSha256"]
                .as_str()
                .unwrap()
        );
        assert_eq!(
            profile.runtime_build().engine_revision,
            evidence["engineRevision"].as_str().unwrap()
        );
        assert_eq!(
            profile.recorded_model().0.as_str(),
            model["source"]["sha256"].as_str().unwrap()
        );
        assert_eq!(
            profile.recorded_model().1,
            model["source"]["sizeBytes"].as_u64().unwrap()
        );
        assert_eq!(
            profile.recorded_model_spec().0.as_str(),
            model["specSha256"].as_str().unwrap()
        );
    }
    assert_eq!(
        AudioTaskProfile::PocketTtsEnglishCpu
            .recorded_model_spec()
            .1,
        15_862
    );
    assert_eq!(
        AudioTaskProfile::Qwen3Asr06bCpu.recorded_model_spec().1,
        14_197
    );
    assert_eq!(
        AudioTaskProfile::PocketTtsEnglishCpu.capability(),
        ModelCapability::SpeechSynthesis
    );
    assert_eq!(
        AudioTaskProfile::Qwen3Asr06bCpu.capability(),
        ModelCapability::Transcription
    );
}

#[test]
fn typed_registries_select_independent_model_schemas_and_cannot_cross_resolve() {
    let mut speech = TaskParameterRegistry::default();
    let mut recognition = TaskParameterRegistry::default();
    let tts_id = speech.register(tts_service()).unwrap();
    let asr_id = recognition.register(asr_service()).unwrap();
    assert_ne!(tts_id.parameter_schema_id, asr_id.parameter_schema_id);
    assert!(speech.resolve(&asr_id).is_err());
    assert!(recognition.resolve(&tts_id).is_err());
    assert!(speech.prepare(&tts_id, tts()).is_ok());
    assert!(recognition.prepare(&asr_id, asr()).is_ok());
    assert!(speech.register(tts_service()).is_err());
    let mut changed = asr_id.clone();
    changed.version_id = "v2".into();
    assert!(recognition.resolve(&changed).is_err());
    changed = asr_id.clone();
    changed.runtime_digest = "0".repeat(64);
    assert!(recognition.resolve(&changed).is_err());
    changed = asr_id;
    changed.component_binding_digest = "0".repeat(64);
    assert!(recognition.resolve(&changed).is_err());
}

#[test]
fn changed_model_spec_runtime_capability_or_duplicate_artifact_is_rejected() {
    let profile = AudioTaskProfile::PocketTtsEnglishCpu;
    let mut cases = Vec::new();
    let mut b = binding(profile);
    b.model.sha256 = Sha256Digest::of_bytes(b"other-model");
    cases.push(b);
    let mut b = binding(profile);
    b.model_spec.sha256 = Sha256Digest::of_bytes(b"embedded-or-new-spec");
    cases.push(b);
    let mut b = binding(profile);
    b.model_spec.size_bytes += 1;
    cases.push(b);
    let mut b = binding(profile);
    b.model_spec.artifact_id = b.model.artifact_id.clone();
    cases.push(b);
    let mut b = binding(profile);
    b.runtime.binary_sha256 = Sha256Digest::of_bytes(b"other-build");
    cases.push(b);
    let mut b = binding(profile);
    b.runtime.features.insert("cuda".into());
    cases.push(b);
    let mut b = binding(profile);
    b.target_capabilities
        .insert(ModelCapability::AudioGeneration);
    cases.push(b);
    for b in cases {
        assert!(PocketTtsParameterService::new(b).is_err());
    }
    assert!(PocketTtsParameterService::new(binding(AudioTaskProfile::Qwen3Asr06bCpu)).is_err());
    assert!(Qwen3AsrParameterService::new(binding(profile)).is_err());
}

#[test]
fn pocket_settings_are_passed_before_native_preparation_without_resource_promotion() {
    let mut request = tts();
    request.temperature = Some(0.8);
    request.seed = Some(u32::MAX);
    request.max_frames = Some(200);
    request.text_chunk_tokens = Some(40);
    request.frames_after_eos = Some(8);
    let prepared = tts_service().compile(request).unwrap();
    let args = prepared.arguments();
    for (name, value) in [
        ("--task", "tts"),
        ("--family", "pocket_tts"),
        ("--load-option", "language=english"),
        ("--seed", "4294967295"),
        ("--temperature", "0.8"),
        ("--out-format", "pcm16"),
        ("--mode", "offline"),
    ] {
        assert_eq!(flag(args, name), &string(value));
    }
    for value in [
        "max_frames=200",
        "text_chunk_tokens=40",
        "frames_after_eos=8",
        "noise_clamp=-1",
        "eos_threshold=-4",
        "truncate_clone_audio=false",
    ] {
        assert!(args
            .windows(2)
            .any(|w| w == [string("--request-option"), string(value)]));
    }
    assert_eq!(
        flag(args, "--voice-ref"),
        &NativeAudioArgument::Input(input())
    );
    assert_eq!(
        flag(args, "--model-spec-override"),
        &NativeAudioArgument::ModelSpec
    );
    assert!(prepared.snapshot().metadata.requires_new_native_preparation);
    assert_eq!(
        prepared.ensure_execution_admitted().unwrap_err().issues[0].field,
        "resourceAdmission"
    );
}

#[test]
fn pocket_default_seed_is_concrete_and_snapshot_roundtrip_preserves_voice_and_settings() {
    let mut request = tts();
    request.seed = None;
    let prepared = tts_service().compile(request).unwrap();
    let snapshot = prepared.snapshot();
    assert_eq!(
        flag(prepared.arguments(), "--seed"),
        &string(&snapshot.parameters.seed.to_string())
    );
    assert_eq!(snapshot.parameters.temperature, 0.3);
    assert_eq!(
        (
            snapshot.parameters.max_frames,
            snapshot.parameters.text_chunk_tokens,
            snapshot.parameters.frames_after_eos
        ),
        (0, 50, -1)
    );
    let restored: PocketTtsSnapshot =
        serde_json::from_slice(&serde_json::to_vec(snapshot).unwrap()).unwrap();
    assert_eq!(&restored, snapshot);
    assert_eq!(restored.digest().unwrap(), snapshot.digest().unwrap());
    assert!(tts_service()
        .descriptor()
        .limitations
        .iter()
        .any(|s| s.contains("Create only")));
}

#[test]
fn qwen_language_context_token_limit_and_chunking_have_asr_specific_arguments() {
    let mut request = asr();
    request.language = Some(Qwen3AsrLanguage::Chinese);
    request.context = Some("术语：木桌，苹果".into());
    request.max_tokens = Some(768);
    request.chunking = Some(Qwen3AsrChunking::Fixed { seconds: 12.5 });
    let prepared = asr_service().compile(request).unwrap();
    for (name, value) in [
        ("--task", "asr"),
        ("--family", "qwen3_asr"),
        ("--language", "Chinese"),
        ("--text", "术语：木桌，苹果"),
        ("--max-tokens", "768"),
        ("--audio-chunk-mode", "fixed"),
        ("--audio-chunk-seconds", "12.5"),
    ] {
        assert_eq!(flag(prepared.arguments(), name), &string(value));
    }
    assert_eq!(
        flag(prepared.arguments(), "--audio"),
        &NativeAudioArgument::Input(input())
    );
    assert_eq!(
        flag(prepared.arguments(), "--text-out"),
        &NativeAudioArgument::Output
    );
    assert!(!prepared.arguments().contains(&string("--voice-ref")));
    assert!(!prepared.snapshot().parameters.return_timestamps);
    assert!(prepared.ensure_execution_admitted().is_err());
    assert_eq!(asr_service().descriptor().recognition_languages.len(), 31);
}

#[test]
fn qwen_whole_file_mode_has_no_ignored_duration_and_default_language_is_auto() {
    let mut request = asr();
    request.chunking = Some(Qwen3AsrChunking::WholeFile {});
    let prepared = asr_service().compile(request).unwrap();
    assert_eq!(
        flag(prepared.arguments(), "--audio-chunk-mode"),
        &string("none")
    );
    assert!(!prepared
        .arguments()
        .contains(&string("--audio-chunk-seconds")));
    assert_eq!(flag(prepared.arguments(), "--language"), &string("Auto"));
    assert_eq!(flag(prepared.arguments(), "--max-tokens"), &string("512"));
    let mut request = serde_json::to_value(asr()).unwrap();
    request["chunking"] = json!({"kind":"whole_file"});
    assert_eq!(
        asr_service()
            .compile_json(request.clone())
            .unwrap()
            .snapshot(),
        prepared.snapshot()
    );
    request["chunking"] = json!({"kind":"whole_file","seconds":30});
    assert!(asr_service().compile_json(request.clone()).is_err());
    for chunking in [
        json!({"kind":"whole_file","unexpected":null}),
        json!({"kind":"fixed","seconds":30,"unexpected":null}),
        json!({"kind":"fixed"}),
    ] {
        request["chunking"] = chunking;
        assert!(asr_service().compile_json(request.clone()).is_err());
    }
}

#[test]
fn unsupported_unknown_and_cross_model_options_fail_instead_of_disappearing() {
    for name in [
        "language",
        "emotion",
        "speed",
        "speakingRate",
        "voiceId",
        "streaming",
        "gpuLayers",
        "maxTokens",
        "width",
    ] {
        for value in [Value::Null, json!(1)] {
            let mut request = serde_json::to_value(tts()).unwrap();
            request[name] = value;
            assert!(tts_service().compile_json(request).is_err(), "{name}");
        }
    }
    for name in [
        "temperature",
        "seed",
        "voiceReference",
        "returnTimestamps",
        "speed",
        "emotion",
        "streaming",
        "framesAfterEos",
    ] {
        let mut request = serde_json::to_value(asr()).unwrap();
        request[name] = json!(true);
        assert!(asr_service().compile_json(request).is_err(), "{name}");
    }
    assert!(tts_service()
        .compile_json(serde_json::to_value(asr()).unwrap())
        .is_err());
    assert!(asr_service()
        .compile_json(serde_json::to_value(tts()).unwrap())
        .is_err());
    let mut request = serde_json::to_value(asr()).unwrap();
    request["language"] = json!("Klingon");
    assert!(asr_service().compile_json(request).is_err());
}

#[test]
fn inputs_are_session_scoped_canonical_and_bounded_without_claiming_file_verification() {
    let mut inputs = Vec::new();
    let mut i = input();
    i.session_id = "other-session".into();
    inputs.push(i);
    let mut i = input();
    i.result_id = "../arbitrary.wav".into();
    inputs.push(i);
    let mut i = input();
    i.mime_type = "audio/mpeg".into();
    inputs.push(i);
    let mut i = input();
    i.channels = 0;
    inputs.push(i);
    let mut i = input();
    i.sample_rate = 0;
    inputs.push(i);
    let mut i = input();
    i.sample_frames = 0;
    inputs.push(i);
    let mut i = input();
    i.sample_frames = u64::MAX;
    inputs.push(i);
    let mut i = input();
    i.size_bytes = 256 * 1024 * 1024 + 1;
    inputs.push(i);
    for i in inputs {
        let mut request = asr();
        request.audio = i.clone();
        assert!(asr_service().compile(request).is_err());
        let mut request = tts();
        request.voice_reference = i;
        assert!(tts_service().compile(request).is_err());
    }
    let mut request = serde_json::to_value(asr()).unwrap();
    request["audio"]["path"] = json!("D:/outside.wav");
    assert!(asr_service().compile_json(request).is_err());
}

#[test]
fn pocket_voice_limit_rejects_excess_instead_of_enabling_native_truncation() {
    let mut request = tts();
    request.voice_reference.sample_frames = 30 * 16_000;
    let prepared = tts_service().compile(request.clone()).unwrap();
    assert!(!prepared.snapshot().parameters.truncate_clone_audio);
    request.voice_reference.sample_frames += 1;
    assert_eq!(
        tts_service().compile(request).unwrap_err().issues[0].field,
        "voiceReference"
    );
}

#[test]
fn nonfinite_and_invalid_numeric_requests_fail_before_serialization() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0., -1.] {
        let mut request = tts();
        request.temperature = Some(value);
        assert_eq!(
            tts_service().compile(request).unwrap_err().issues[0].field,
            "temperature"
        );
        let mut request = asr();
        request.chunking = Some(Qwen3AsrChunking::Fixed { seconds: value });
        assert_eq!(
            asr_service().compile(request).unwrap_err().issues[0].field,
            "chunking.seconds"
        );
    }
    let mut request = tts();
    request.text_chunk_tokens = Some(0);
    assert!(tts_service().compile(request).is_err());
    let mut request = tts();
    request.max_frames = Some(u32::MAX);
    assert!(tts_service().compile(request).is_err());
    let mut request = tts();
    request.frames_after_eos = Some(-2);
    assert!(tts_service().compile(request).is_err());
    let mut request = asr();
    request.max_tokens = Some(0);
    assert!(asr_service().compile(request).is_err());
    let mut request = asr();
    request.chunking = Some(Qwen3AsrChunking::Fixed { seconds: f32::MAX });
    assert!(asr_service().compile(request).is_err());
}

#[test]
fn different_version_and_voice_produce_distinct_frozen_snapshots() {
    let initial = tts_service().compile(tts()).unwrap();
    let mut next_binding = binding(AudioTaskProfile::PocketTtsEnglishCpu);
    next_binding.version_id = "v2".into();
    let next = PocketTtsParameterService::new(next_binding)
        .unwrap()
        .compile(tts())
        .unwrap();
    assert_ne!(
        initial.snapshot().metadata.identity,
        next.snapshot().metadata.identity
    );
    assert_ne!(
        initial.snapshot().digest().unwrap(),
        next.snapshot().digest().unwrap()
    );
    let mut request = tts();
    request.job_id = "next-job".into();
    request.voice_reference.result_id = "019a1111-2222-8333-8444-666666666666".into();
    request.voice_reference.sha256 = Sha256Digest::of_bytes(b"different voice");
    let next_voice = tts_service().compile(request.clone()).unwrap();
    assert_eq!(
        flag(next_voice.arguments(), "--voice-ref"),
        &NativeAudioArgument::Input(request.voice_reference)
    );
    assert_eq!(initial.snapshot().parameters.voice_reference, input());
    assert_ne!(
        initial.snapshot().digest().unwrap(),
        next_voice.snapshot().digest().unwrap()
    );
}

#[test]
fn native_option_like_text_is_rejected_but_shell_text_remains_one_literal() {
    for value in [
        "--inspect",
        "--load-option",
        "--model-spec-override",
        "\0bad",
    ] {
        let mut request = tts();
        request.text = value.into();
        assert!(tts_service().compile(request).is_err());
        let mut request = asr();
        request.context = Some(value.into());
        assert!(asr_service().compile(request).is_err());
    }
    let text = "Say --inspect; $(echo hello) literally";
    let mut request = tts();
    request.text = text.into();
    let prepared = tts_service().compile(request).unwrap();
    assert_eq!(flag(prepared.arguments(), "--text"), &string(text));
    assert_eq!(
        flag(prepared.arguments(), "--model"),
        &NativeAudioArgument::Model
    );
    assert!(prepared
        .arguments()
        .iter()
        .all(|arg| !matches!(arg, NativeAudioArgument::Literal(value) if value.contains("D:/"))));
    let mut negative_number = tts();
    negative_number.text = "-5".into();
    assert!(tts_service().compile(negative_number).is_ok());
}
