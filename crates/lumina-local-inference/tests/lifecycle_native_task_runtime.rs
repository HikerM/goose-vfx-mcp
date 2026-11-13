use lumina_local_inference::lifecycle::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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
        version_id: "test-version".into(),
        manifest_digest: Sha256Digest::of_bytes(b"synthetic binding; no model execution"),
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

fn audio() -> AudioTaskMediaReference {
    AudioTaskMediaReference {
        session_id: "session-a".into(),
        result_id: "019a1111-2222-8333-8444-555555555555".into(),
        sha256: Sha256Digest::of_bytes(b"synthetic verified media reference"),
        mime_type: "audio/wav".into(),
        size_bytes: 32_044,
        sample_rate: 16_000,
        channels: 1,
        sample_frames: 16_000,
    }
}

fn asr() -> Qwen3AsrRequest {
    Qwen3AsrRequest {
        session_id: "session-a".into(),
        job_id: "job-asr".into(),
        audio: audio(),
        context: None,
        language: None,
        max_tokens: None,
        chunking: None,
    }
}

#[test]
fn native_audio_plans_preserve_exact_model_session_and_snapshot_identity() {
    let tts = PocketTtsParameterService::new(binding(AudioTaskProfile::PocketTtsEnglishCpu))
        .unwrap()
        .compile(PocketTtsRequest {
            session_id: "session-a".into(),
            job_id: "job-tts".into(),
            text: "A red apple.".into(),
            voice_reference: audio(),
            seed: Some(42),
            temperature: None,
            max_frames: None,
            text_chunk_tokens: None,
            frames_after_eos: None,
        })
        .unwrap();
    let asr = Qwen3AsrParameterService::new(binding(AudioTaskProfile::Qwen3Asr06bCpu))
        .unwrap()
        .compile(asr())
        .unwrap();
    assert!(tts.ensure_execution_admitted().is_err());
    assert!(asr.ensure_execution_admitted().is_err());
    let tts_digest = tts.snapshot().digest().unwrap();
    let asr_digest = asr.snapshot().digest().unwrap();
    for (plan, digest, job, profile) in [
        (
            NativeTaskPlan::from(tts),
            tts_digest,
            "job-tts",
            AudioTaskProfile::PocketTtsEnglishCpu,
        ),
        (
            NativeTaskPlan::from(asr),
            asr_digest,
            "job-asr",
            AudioTaskProfile::Qwen3Asr06bCpu,
        ),
    ] {
        assert_eq!(plan.snapshot_digest().unwrap(), digest);
        assert_eq!(plan.runtime(), &profile.runtime_build());
        assert_eq!(plan.installation_id(), "test-installation");
        assert_eq!(plan.version_id(), "test-version");
        assert_eq!(plan.session_id(), "session-a");
        assert_eq!(plan.job_id(), job);
        assert_eq!(
            plan.input_references(),
            vec![NativeTaskInputReference::Audio(audio())]
        );
    }
}

#[test]
fn owner_snapshot_changes_for_version_session_input_or_effective_parameter_changes() {
    let original = binding(AudioTaskProfile::Qwen3Asr06bCpu);
    let service = Qwen3AsrParameterService::new(original.clone()).unwrap();
    let baseline = NativeTaskPlan::from(service.compile(asr()).unwrap())
        .snapshot_digest()
        .unwrap();
    let mut version = original;
    version.version_id = "test-version-2".into();
    let changed_version = Qwen3AsrParameterService::new(version)
        .unwrap()
        .compile(asr())
        .unwrap();
    let mut session = asr();
    session.session_id = "session-b".into();
    session.audio.session_id = "session-b".into();
    let mut input = asr();
    input.audio.sha256 = Sha256Digest::of_bytes(b"other content with identical length");
    let mut parameter = asr();
    parameter.max_tokens = Some(96);
    for prepared in [
        changed_version,
        service.compile(session).unwrap(),
        service.compile(input).unwrap(),
        service.compile(parameter).unwrap(),
    ] {
        assert_ne!(
            NativeTaskPlan::from(prepared).snapshot_digest().unwrap(),
            baseline
        );
    }
}

#[test]
fn generation_owner_plan_preserves_its_compiler_snapshot_without_audio_fields() {
    let profile = GenerationProfile::Sd15CpuTextToImage;
    let binding = GenerationModelBinding {
        installation_id: "image-installation".into(),
        version_id: "image-version".into(),
        manifest_digest: Sha256Digest::of_bytes(b"synthetic image manifest"),
        runtime: profile.runtime_build(),
        target_capabilities: profile.required_capabilities(),
        assets: profile
            .recorded_assets()
            .into_iter()
            .map(|(role, (sha256, size_bytes))| {
                (
                    role,
                    GenerationAssetBinding {
                        artifact_id: format!("image-{role:?}"),
                        sha256,
                        size_bytes,
                    },
                )
            })
            .collect(),
    };
    let prepared = GenerationParameterService::new(profile, binding)
        .unwrap()
        .compile(GenerationTaskRequest {
            session_id: "session-image".into(),
            job_id: "image-job".into(),
            parameters: GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("A red apple on a table.".into()),
                ..Default::default()
            }),
        })
        .unwrap();
    let digest = prepared.snapshot().digest().unwrap();
    assert!(prepared.ensure_execution_admitted().is_err());
    let plan = NativeTaskPlan::from(prepared);
    assert_eq!(plan.snapshot_digest().unwrap(), digest);
    assert_eq!(plan.session_id(), "session-image");
    assert!(plan.input_references().is_empty());
}

#[test]
fn runtime_registry_rejects_request_defined_builds_before_accessing_a_path() {
    let mut unknown = AudioTaskProfile::PocketTtsEnglishCpu.runtime_build();
    unknown.engine_revision = "request-chosen-revision".into();
    let error = NativeTaskEngineRegistry::default()
        .register(&unknown, Path::new("does-not-exist.exe"))
        .unwrap_err();
    assert!(error.to_string().contains("not a pinned supported build"));
}

#[test]
fn runtime_registry_checks_actual_bytes_even_for_a_pinned_runtime_identity() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/native-task-registry");
    std::fs::create_dir_all(&base).unwrap();
    let temp = tempfile::tempdir_in(base.canonicalize().unwrap()).unwrap();
    let binary = temp.path().join("not-the-recorded-helper.exe");
    std::fs::write(
        &binary,
        b"not executable; registration must only read and reject",
    )
    .unwrap();
    let error = NativeTaskEngineRegistry::default()
        .register(
            &AudioTaskProfile::PocketTtsEnglishCpu.runtime_build(),
            &binary,
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("differs from the frozen runtime"),
        "{error}"
    );
    std::fs::write(&binary, b"all verification guards were released").unwrap();
}
