use lumina_local_inference::lifecycle::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const RESULT: &str = "019a1111-2222-8333-8444-555555555555";
const INPUT: &str = "019a1111-2222-8333-8444-666666666666";
const SD_PROMPT: &str =
    "a photograph of a red apple on a wooden table, studio lighting, high quality";
const H3_PROMPT: &str = "A single red apple on a wooden table slowly rolls to the right. Fixed camera, natural daylight. Soft rolling sound, no speech.";

fn binding(profile: GenerationProfile) -> GenerationModelBinding {
    GenerationModelBinding {
        installation_id: "fixture-installation".into(),
        version_id: "fixture-version".into(),
        manifest_digest: Sha256Digest::of_bytes(b"fixture manifest; no file/model execution"),
        runtime: profile.runtime_build(),
        target_capabilities: profile.required_capabilities(),
        assets: profile
            .recorded_assets()
            .into_iter()
            .map(|(role, (sha256, size_bytes))| {
                (
                    role,
                    GenerationAssetBinding {
                        artifact_id: format!("fixture-{role:?}"),
                        sha256,
                        size_bytes,
                    },
                )
            })
            .collect(),
    }
}
fn service(profile: GenerationProfile) -> GenerationParameterService {
    GenerationParameterService::new(profile, binding(profile)).unwrap()
}
fn request(parameters: GenerationParameters) -> GenerationTaskRequest {
    GenerationTaskRequest {
        session_id: "session-a".into(),
        job_id: "job-1".into(),
        parameters,
    }
}
fn image() -> GenerationParameters {
    GenerationParameters::Image(Sd15ImageParameters {
        prompt: Some(SD_PROMPT.into()),
        ..Default::default()
    })
}
fn video() -> GenerationParameters {
    GenerationParameters::Video(H3VideoParameters {
        prompt: Some(H3_PROMPT.into()),
        ..Default::default()
    })
}
fn input() -> GenerationMediaInput {
    GenerationMediaInput {
        session_id: "session-a".into(),
        result_id: INPUT.into(),
        sha256: Sha256Digest::parse(
            "2370e2963c2bbf8186f899684d7b60985f0196ac5c620c08c6e7522d1d49a034",
        )
        .unwrap(),
        mime_type: "image/png".into(),
        size_bytes: 467_603,
        width: 512,
        height: 512,
    }
}
fn record(prepared: &PreparedGenerationTask) -> GenerationResultRecord {
    GenerationResultRecord {
        snapshot: prepared.snapshot().clone(),
        snapshot_digest: prepared.snapshot().digest().unwrap(),
        outputs: vec![GenerationOutputReference {
            session_id: "session-a".into(),
            result_id: RESULT.into(),
            sha256: Sha256Digest::of_bytes(b"fixture output"),
        }],
    }
}
fn literal_after<'a>(prepared: &'a PreparedGenerationTask, flag: &str) -> &'a str {
    let args = prepared.arguments();
    let index = args
        .iter()
        .position(|value| value == &NativeGenerationArgument::Literal(flag.into()))
        .unwrap();
    match &args[index + 1] {
        NativeGenerationArgument::Literal(value) => value,
        _ => panic!("expected literal flag value"),
    }
}

#[test]
fn disabled_diffusion_cache_uses_the_native_default_for_images_and_video() {
    for (profile, params) in [
        (GenerationProfile::Sd15CpuTextToImage, image()),
        (GenerationProfile::H3CudaTextToVideoAudio, video()),
    ] {
        let prepared = service(profile).compile(request(params)).unwrap();
        assert_eq!(prepared.snapshot().engine.cache_mode, "disabled");
        assert!(!prepared
            .arguments()
            .contains(&NativeGenerationArgument::Literal("--cache-mode".into())));
    }
}

#[test]
fn pinned_profiles_expose_only_bound_schedulers_and_first_frame_inputs() {
    let wan = GenerationProfile::installation_profile("wan21-13b-cpu").unwrap();
    for profile in [
        GenerationProfile::Sd15CpuTextToImage,
        GenerationProfile::H3CudaTextToVideoAudio,
        GenerationProfile::H3CudaFirstFrameToVideoAudio,
        wan,
    ] {
        let engine = service(profile);
        let desc = engine.descriptor();
        assert!(desc
            .limitations
            .iter()
            .all(|message| !message.contains("PocketTTS")));
        if profile == wan {
            let first = &desc.fields["firstFrame"];
            assert_eq!(first.support, GenerationParameterSupport::Unsupported);
            assert!(first.constraints[0].contains("no registered"));
        }
        if profile == GenerationProfile::H3CudaTextToVideoAudio {
            assert!(desc.fields["firstFrame"].constraints[0].contains("separate"));
        }
        if profile == GenerationProfile::H3CudaFirstFrameToVideoAudio {
            assert_eq!(
                desc.fields["firstFrame"].support,
                GenerationParameterSupport::NativeSupported
            );
        }
        let mut req = if profile.intent() == NativeTaskIntent::TextToImage {
            image()
        } else {
            video()
        };
        if let GenerationParameters::Video(v) = &mut req {
            if profile == GenerationProfile::H3CudaFirstFrameToVideoAudio {
                v.first_frame = Some(input());
            }
        }
        let accepted = &desc.fields["scheduler"];
        let task = engine.compile(request(req.clone())).unwrap();
        assert_eq!(
            accepted.default,
            Some(json!(literal_after(&task, "--scheduler")))
        );
        for value in &accepted.enum_values {
            let mut json = serde_json::to_value(request(req.clone())).unwrap();
            json["parameters"]["parameters"]["scheduler"] = value.clone();
            let task = engine.compile_json(json).unwrap();
            assert_eq!(Some(literal_after(&task, "--scheduler")), value.as_str());
        }
        assert!(!accepted.enum_values.contains(&json!("karras")));
        let mut unbound = serde_json::to_value(request(req)).unwrap();
        unbound["parameters"]["parameters"]["scheduler"] = json!("karras");
        let error = engine.compile_json(unbound).unwrap_err();
        assert_eq!(error.issues[0].field, "scheduler");
        assert_eq!(error.issues[0].code, "unsupported_by_profile");
    }
}

#[test]
fn persisted_v1_snapshot_digests_reopen_for_the_exact_model_and_variant() {
    // Frozen by compiling these requests against the earlier Windows build.
    for (profile, req, old_descriptor, old_snapshot) in [
        (
            GenerationProfile::Sd15CpuTextToImage,
            image(),
            "f5e1ad23bbbec1d4652ea8f3a3b5fcd44a468c89f783ae0e9e2a59e1324779c9",
            "e6097f45510d154a1e2313f6a643528383ed47d16afd81bb4015e6b88defd2d2",
        ),
        (
            GenerationProfile::H3CudaTextToVideoAudio,
            video(),
            "bf3fd491360ef7d5d9748fe557b46341ff3cdedc6ae7162ddd075e7fee0fcb7d",
            "3b9e439cf8906ab09f696390e6896da3f7b65f88795512b2cf12a00451935155",
        ),
        (
            GenerationProfile::H3CudaFirstFrameToVideoAudio,
            GenerationParameters::Video(H3VideoParameters {
                prompt: Some(H3_PROMPT.into()),
                first_frame: Some(input()),
                ..Default::default()
            }),
            "1b079cf7e2e285d4565e15657835e01cdce814a02142443f6a428c32d7b8017a",
            "83a3dddff65c40fbbf9e5437198d6faf160690216d45b868817c6bccc233f7b0",
        ),
    ] {
        let engine = service(profile);
        let task = engine.compile(request(req)).unwrap();
        let mut stored = record(&task);
        let digest = Sha256Digest::parse(old_descriptor).unwrap();
        assert_ne!(digest, stored.snapshot.descriptor_digest);
        assert!(engine.matches_descriptor_digest(&digest).unwrap());
        assert!(!engine
            .matches_descriptor_digest(&Sha256Digest::of_bytes(b"other contract"))
            .unwrap());
        stored.snapshot.descriptor_digest = digest;
        stored.snapshot_digest = Sha256Digest::parse(old_snapshot).unwrap();
        assert_eq!(stored.snapshot.digest().unwrap(), stored.snapshot_digest);
        let patch = if profile.intent() == NativeTaskIntent::TextToImage {
            GenerationParameters::Image(Default::default())
        } else {
            GenerationParameters::Video(Default::default())
        };
        let child = engine
            .reuse_result("session-a", &stored, RESULT, "job-2", patch)
            .unwrap();
        assert_eq!(child.snapshot().parameters, stored.snapshot.parameters);
        assert_eq!(
            child.snapshot().descriptor_digest,
            task.snapshot().descriptor_digest
        );
        assert_eq!(
            child.snapshot().parent.as_ref().unwrap().snapshot_digest,
            stored.snapshot_digest
        );
    }
}

#[test]
fn installed_video_models_use_distinct_timing_audio_and_encoder_contracts() {
    let wan = GenerationProfile::installation_profile("wan21-13b-cpu").unwrap();
    let h3 = GenerationProfile::H3CudaTextToVideoAudio;
    for (profile, fps, frames, audio, encoder) in [
        (wan, 8, 17, GenerationAudioMode::VideoOnly, "--t5xxl"),
        (h3, 24, 22, GenerationAudioMode::JointAudioVideo, "--llm"),
    ] {
        let engine = service(profile);
        let prepared = engine.compile(request(video())).unwrap();
        let EffectiveGenerationParameters::Video(parameters) = &prepared.snapshot().parameters
        else {
            panic!("expected video")
        };
        assert_eq!(
            (parameters.fps, parameters.frames, parameters.audio_mode),
            (fps, frames, audio)
        );
        assert!(prepared.arguments().windows(2).any(|args| args
            == [
                NativeGenerationArgument::Literal(encoder.into()),
                NativeGenerationArgument::Component(GenerationAssetRole::TextEncoder)
            ]));
        let mut wrong = H3VideoParameters {
            prompt: Some("A dog runs".into()),
            ..Default::default()
        };
        wrong.fps = Some(if fps == 8 { 24 } else { 8 });
        assert!(engine
            .compile(request(GenerationParameters::Video(wrong.clone())))
            .is_err());
        wrong.fps = None;
        wrong.audio_mode = Some(if audio == GenerationAudioMode::VideoOnly {
            GenerationAudioMode::JointAudioVideo
        } else {
            GenerationAudioMode::VideoOnly
        });
        assert!(engine
            .compile(request(GenerationParameters::Video(wrong)))
            .is_err());
    }
    assert_ne!(wan.required_capabilities(), h3.required_capabilities());
    assert!(!wan
        .recorded_assets()
        .contains_key(&GenerationAssetRole::AudioVae));
}

#[test]
fn wan_resource_request_and_observed_arguments_are_bound_to_the_cpu_video_model() {
    let evidence: Value = serde_json::from_str(include_str!("../../../documentation/docs/lumina-architecture/model-lifecycle-evidence/2026-11-01-native-video-feasibility.json")).unwrap();
    let argv = evidence["argv"].as_array().unwrap();
    let prompt = argv[argv.iter().position(|a| a == "-p").unwrap() + 1]
        .as_str()
        .unwrap();
    let profile = GenerationProfile::installation_profile("wan21-13b-cpu").unwrap();
    let engine = service(profile);
    let input = H3VideoParameters {
        prompt: Some(prompt.into()),
        ..Default::default()
    };
    let task = engine
        .compile(request(GenerationParameters::Video(input.clone())))
        .unwrap();
    assert_eq!(evidence["status"], "QUALITY_FAILED");
    assert!(task.snapshot().observation.matches_observed_request);
    assert!(!task.snapshot().observation.quality_acceptance_complete);
    let mut changed = input;
    changed.seed = Some(GenerationSeed::Value { value: 999 });
    assert!(
        !engine
            .compile(request(GenerationParameters::Video(changed)))
            .unwrap()
            .snapshot()
            .observation
            .matches_observed_request
    );
    let hardware = HardwareResourceSnapshot {
        observed_at: 1000,
        disk_free_bytes: 1 << 40,
        storage_volume: "test-volume".into(),
        devices: vec![HardwareMemoryDevice {
            index: 0,
            backend: "CPU".into(),
            name: "CPU".into(),
            description: "test CPU".into(),
            kind: "cpu".into(),
            total_bytes: 1 << 35,
            free_bytes: 1 << 34,
        }],
    };
    let TaskResourceAssessment::NeedsProfiling(detail) =
        assess_task_resources(&task.into(), &hardware, 1000).unwrap()
    else {
        panic!("unmeasured CPU profile should require profiling")
    };
    assert!(!detail.key.requires_gpu);
    assert_eq!(detail.evidence, "2026-11-01-native-video-feasibility.json");
    assert!(detail.missing.iter().all(|entry| !entry.contains("CUDA")));
}

#[test]
fn profiles_match_independent_published_engine_and_weight_evidence() {
    let sd:Value=serde_json::from_str(include_str!("../../../documentation/docs/lumina-architecture/model-lifecycle-evidence/2026-11-01-native-image-feasibility.json")).unwrap();
    let h3:Value=serde_json::from_str(include_str!("../../../documentation/docs/lumina-architecture/model-lifecycle-evidence/2026-11-12-h3-native-video-smoke.json")).unwrap();
    let profile = GenerationProfile::Sd15CpuTextToImage;
    assert_eq!(
        profile.runtime_build().binary_sha256.as_str(),
        sd["binarySha256"].as_str().unwrap()
    );
    let assets = profile.recorded_assets();
    assert_eq!(
        assets[&GenerationAssetRole::Main].0.as_str(),
        sd["source"]["sha256"].as_str().unwrap()
    );
    assert_eq!(
        assets[&GenerationAssetRole::Main].1,
        sd["source"]["sizeBytes"].as_u64().unwrap()
    );
    assert_eq!(
        GenerationProfile::H3CudaTextToVideoAudio
            .runtime_build()
            .binary_sha256
            .as_str(),
        h3["cudaBuild"]["binary"]["sha256"].as_str().unwrap()
    );
    let weights: Value = serde_json::from_str(include_str!("../../../documentation/docs/lumina-architecture/model-lifecycle-evidence/2026-11-12-h3-full-weight-downloads.json")).unwrap();
    for (_, (sha, size)) in GenerationProfile::H3CudaTextToVideoAudio.recorded_assets() {
        assert!(weights["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["sha256"].as_str() == Some(sha.as_str())
                && file["sizeBytes"].as_u64() == Some(size)));
    }
    let prepared = service(GenerationProfile::H3CudaTextToVideoAudio)
        .compile(request(video()))
        .unwrap();
    let measured = h3["cases"][0]["argv"].as_array().unwrap();
    for flag in [
        "--backend",
        "--params-backend",
        "--rng",
        "--steps",
        "--sampling-method",
        "--scheduler",
        "--flow-shift",
        "--fps",
        "--video-frames",
        "--max-vram",
        "-W",
        "-H",
        "-s",
        "-p",
    ] {
        let index = measured
            .iter()
            .position(|value| value.as_str() == Some(flag))
            .unwrap();
        assert_eq!(
            literal_after(&prepared, flag),
            measured[index + 1].as_str().unwrap(),
            "{flag}"
        );
    }
}

#[test]
fn defaults_and_advanced_values_reach_native_arguments_without_resource_promotion() {
    let service = service(GenerationProfile::Sd15CpuTextToImage);
    let baseline = service.compile(request(image())).unwrap();
    assert!(baseline.snapshot().observation.matches_observed_request);
    assert!(!baseline.snapshot().observation.quality_acceptance_complete);
    assert_eq!(
        baseline.ensure_execution_admitted().unwrap_err().issues[0].field,
        "resourceAdmission"
    );
    let adjusted = service
        .compile(request(GenerationParameters::Image(Sd15ImageParameters {
            prompt: Some("a blue vase".into()),
            width: Some(768),
            height: Some(512),
            steps: Some(30),
            cfg_scale: Some(6.5),
            batch_count: Some(2),
            sampler: Some(GenerationSampler::Euler),
            seed: Some(GenerationSeed::Value { value: 99 }),
            ..Default::default()
        })))
        .unwrap();
    for (flag, value) in [
        ("-W", "768"),
        ("-H", "512"),
        ("--steps", "30"),
        ("--cfg-scale", "6.5"),
        ("--batch-count", "2"),
        ("--sampling-method", "euler"),
        ("-s", "99"),
    ] {
        assert_eq!(literal_after(&adjusted, flag), value);
    }
    assert!(!adjusted.snapshot().observation.matches_observed_request);
    assert_eq!(
        adjusted.snapshot().execution_status,
        GenerationExecutionStatus::NeedsResourceAdmission
    );
    assert!(adjusted.ensure_execution_admitted().is_err());
}

#[test]
fn model_switch_selects_a_new_schema_and_defaults_instead_of_reusing_image_options() {
    let mut registry = TaskParameterRegistry::default();
    let image_key = registry
        .register(service(GenerationProfile::Sd15CpuTextToImage))
        .unwrap();
    let video_key = registry
        .register(service(GenerationProfile::H3CudaTextToVideoAudio))
        .unwrap();
    assert_ne!(image_key.parameter_schema_id, video_key.parameter_schema_id);
    assert_eq!(
        registry.resolve(&image_key).unwrap().describe().fields["steps"].default,
        Some(json!(20))
    );
    assert_eq!(
        registry.resolve(&video_key).unwrap().describe().fields["steps"].default,
        Some(json!(8))
    );
    assert_eq!(
        registry.resolve(&image_key).unwrap().describe().fields["batchCount"].support,
        GenerationParameterSupport::NativeSupported
    );
    assert_eq!(
        registry.resolve(&video_key).unwrap().describe().fields["batchCount"].support,
        GenerationParameterSupport::Unsupported
    );
    assert!(registry.prepare(&video_key, request(image())).is_err());
    assert!(registry.prepare(&image_key, request(video())).is_err());
    assert!(registry
        .register(service(GenerationProfile::Sd15CpuTextToImage))
        .is_err());
    let mut changed = video_key.clone();
    changed.version_id = "new-version".into();
    assert!(registry.resolve(&changed).is_err());
    changed = video_key.clone();
    changed.runtime_digest = "f".repeat(64);
    assert!(registry.resolve(&changed).is_err());
    changed = video_key;
    changed.component_binding_digest = "e".repeat(64);
    assert!(registry.resolve(&changed).is_err());
}

#[test]
fn variant_and_component_runtime_binding_cannot_be_swapped() {
    let profile = GenerationProfile::H3CudaTextToVideoAudio;
    let mut altered = binding(profile);
    altered
        .target_capabilities
        .remove(&ModelCapability::AudioGeneration);
    assert!(GenerationParameterService::new(profile, altered).is_err());
    let mut altered = binding(profile);
    altered.runtime.binary_sha256 = Sha256Digest::of_bytes(b"other binary");
    assert!(GenerationParameterService::new(profile, altered).is_err());
    let mut altered = binding(profile);
    altered
        .assets
        .get_mut(&GenerationAssetRole::AudioVae)
        .unwrap()
        .sha256 = Sha256Digest::of_bytes(b"other audio vae");
    assert!(GenerationParameterService::new(profile, altered).is_err());
    let mut altered = binding(profile);
    altered.assets.remove(&GenerationAssetRole::TextEncoder);
    assert!(GenerationParameterService::new(profile, altered).is_err());
    let t2v = service(profile);
    let i2v = service(GenerationProfile::H3CudaFirstFrameToVideoAudio);
    assert_ne!(
        t2v.identity().unwrap().task_variant,
        i2v.identity().unwrap().task_variant
    );
    assert!(i2v.compile(request(video())).is_err());
    let mut params = H3VideoParameters {
        prompt: Some(H3_PROMPT.into()),
        first_frame: Some(input()),
        ..Default::default()
    };
    assert!(t2v
        .compile(request(GenerationParameters::Video(params.clone())))
        .is_err());
    let prepared = i2v
        .compile(request(GenerationParameters::Video(params.clone())))
        .unwrap();
    assert!(prepared
        .arguments()
        .contains(&NativeGenerationArgument::InputResult(INPUT.into())));
    params.first_frame.as_mut().unwrap().session_id = "session-b".into();
    assert!(i2v
        .compile(request(GenerationParameters::Video(params)))
        .is_err());
}

#[test]
fn unknown_options_and_unsupported_model_controls_are_not_silently_ignored() {
    let image_service = service(GenerationProfile::Sd15CpuTextToImage);
    for extra in ["speed", "strength", "mask", "command", "gpuLayers"] {
        let mut parameters = json!({"prompt":"test"});
        parameters[extra] = json!(1);
        assert!(image_service.compile_json(json!({"sessionId":"s","jobId":"j","parameters":{"task":"image","parameters":parameters}})).is_err(),"{extra}");
    }
    let video_service = service(GenerationProfile::H3CudaTextToVideoAudio);
    assert!(video_service.compile_json(json!({"sessionId":"s","jobId":"j","parameters":{"task":"video","parameters":{"prompt":"test","sampler":"euler_a"}}})).is_err());
    assert!(video_service.compile_json(json!({"sessionId":"s","jobId":"j","parameters":{"task":"speech","parameters":{"text":"test","speed":1.2}}})).is_err());
}

#[test]
fn h3_native_fps_frame_alignment_and_duration_conflicts_are_rejected_without_clamping() {
    let service = service(GenerationProfile::H3CudaTextToVideoAudio);
    for (fps, frames) in [(30, 22), (24, 24), (24, 1), (0, 22)] {
        assert!(service
            .compile(request(GenerationParameters::Video(H3VideoParameters {
                prompt: Some("test".into()),
                fps: Some(fps),
                frames: Some(frames),
                ..Default::default()
            })))
            .is_err());
    }
    assert!(service
        .compile(request(GenerationParameters::Video(H3VideoParameters {
            prompt: Some("test".into()),
            duration_seconds: Some(1.0),
            ..Default::default()
        })))
        .is_err());
    assert!(service
        .compile(request(GenerationParameters::Video(H3VideoParameters {
            prompt: Some("test".into()),
            duration_seconds: Some(39. / 24.),
            frames: Some(22),
            ..Default::default()
        })))
        .is_err());
    let advanced = service
        .compile(request(GenerationParameters::Video(H3VideoParameters {
            prompt: Some("test".into()),
            duration_seconds: Some(39. / 24.),
            width: Some(512),
            height: Some(288),
            steps: Some(12),
            cfg_scale: Some(2.),
            ..Default::default()
        })))
        .unwrap();
    assert_eq!(literal_after(&advanced, "--video-frames"), "39");
    assert_eq!(literal_after(&advanced, "--steps"), "12");
    assert_eq!(literal_after(&advanced, "-H"), "288");
    assert!(advanced.ensure_execution_admitted().is_err());
}

#[test]
fn invalid_geometry_nonfinite_cfg_and_implicit_asset_loading_fail_at_named_fields() {
    let service = service(GenerationProfile::Sd15CpuTextToImage);
    for cfg in [f32::NAN, f32::INFINITY, -1.] {
        let error = service
            .compile(request(GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("test".into()),
                cfg_scale: Some(cfg),
                ..Default::default()
            })))
            .unwrap_err();
        assert_eq!(error.issues[0].field, "cfgScale");
    }
    for width in [0, 513, u32::MAX] {
        assert!(service
            .compile(request(GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("test".into()),
                width: Some(width),
                ..Default::default()
            })))
            .is_err());
    }
    for prompt in ["", "bad\0text", "a <lora:other-file:1> picture"] {
        assert!(service
            .compile(request(GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some(prompt.into()),
                ..Default::default()
            })))
            .is_err());
    }
    assert!(service
        .compile(request(GenerationParameters::Image(Sd15ImageParameters {
            prompt: Some("test".into()),
            aspect_ratio: Some(GenerationAspectRatio {
                width: 16,
                height: 9
            }),
            ..Default::default()
        })))
        .is_err());
}

#[test]
fn snapshot_roundtrip_and_single_field_regeneration_preserve_inputs_seed_and_other_values() {
    let service = service(GenerationProfile::H3CudaFirstFrameToVideoAudio);
    let initial = service
        .compile(request(GenerationParameters::Video(H3VideoParameters {
            prompt: Some(H3_PROMPT.into()),
            first_frame: Some(input()),
            ..Default::default()
        })))
        .unwrap();
    let bytes = serde_json::to_vec(&record(&initial)).unwrap();
    let restored: GenerationResultRecord = serde_json::from_slice(&bytes).unwrap();
    let regenerated = service
        .reuse_result(
            "session-a",
            &restored,
            RESULT,
            "job-2",
            GenerationParameters::Video(H3VideoParameters {
                steps: Some(12),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(
        regenerated.snapshot().changes.keys().collect::<Vec<_>>(),
        vec!["steps"]
    );
    assert_eq!(literal_after(&regenerated, "--steps"), "12");
    assert_eq!(literal_after(&regenerated, "-s"), "42");
    assert!(regenerated
        .arguments()
        .contains(&NativeGenerationArgument::InputResult(INPUT.into())));
    assert_eq!(
        regenerated.snapshot().parent.as_ref().unwrap().result_id,
        RESULT
    );
    assert_eq!(
        regenerated.snapshot().origins["prompt"],
        GenerationParameterOrigin::Parent {
            result_id: RESULT.into()
        }
    );
    assert_eq!(
        regenerated.snapshot().origins["steps"],
        GenerationParameterOrigin::Request
    );
    assert_eq!(initial.snapshot().job_id, "job-1");
    assert_eq!(
        restored.snapshot_digest,
        initial.snapshot().digest().unwrap()
    );
}

#[test]
fn explicit_new_seed_is_recorded_once_and_inherited_on_later_regeneration() {
    let service = service(GenerationProfile::Sd15CpuTextToImage);
    let initial = service.compile(request(image())).unwrap();
    let changed = service
        .reuse_result(
            "session-a",
            &record(&initial),
            RESULT,
            "job-2",
            GenerationParameters::Image(Sd15ImageParameters {
                seed: Some(GenerationSeed::New),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_ne!(literal_after(&changed, "-s"), "42");
    assert_eq!(changed.snapshot().changes.len(), 1);
    assert_eq!(
        changed.snapshot().origins["seed"],
        GenerationParameterOrigin::GeneratedSeed
    );
    let later = service
        .reuse_result(
            "session-a",
            &record(&changed),
            RESULT,
            "job-3",
            GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("new prompt".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(literal_after(&later, "-s"), literal_after(&changed, "-s"));
}

#[test]
fn duration_only_patch_updates_frames_but_keeps_seed_and_resolution() {
    let service = service(GenerationProfile::H3CudaTextToVideoAudio);
    let initial = service.compile(request(video())).unwrap();
    let changed = service
        .reuse_result(
            "session-a",
            &record(&initial),
            RESULT,
            "job-2",
            GenerationParameters::Video(H3VideoParameters {
                duration_seconds: Some(39. / 24.),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(literal_after(&changed, "--video-frames"), "39");
    assert_eq!(literal_after(&changed, "-W"), "256");
    assert_eq!(literal_after(&changed, "-s"), "42");
    assert_eq!(
        changed.snapshot().changes.keys().collect::<Vec<_>>(),
        vec!["duration", "frames"]
    );
}

#[test]
fn reuse_rejects_nonfinite_cfg_patches_for_both_models_instead_of_inheriting() {
    for (profile, parameters) in [
        (GenerationProfile::Sd15CpuTextToImage, image()),
        (GenerationProfile::H3CudaTextToVideoAudio, video()),
    ] {
        let service = service(profile);
        let initial = service.compile(request(parameters)).unwrap();
        let parent = record(&initial);
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let patch = match profile {
                GenerationProfile::Sd15CpuTextToImage => {
                    GenerationParameters::Image(Sd15ImageParameters {
                        cfg_scale: Some(value),
                        ..Default::default()
                    })
                }
                _ => GenerationParameters::Video(H3VideoParameters {
                    cfg_scale: Some(value),
                    ..Default::default()
                }),
            };
            let error = service
                .reuse_result("session-a", &parent, RESULT, "job-2", patch)
                .unwrap_err();
            assert_eq!(error.issues[0].field, "cfgScale");
            assert_eq!(error.issues[0].code, "out_of_range");
        }
        assert_eq!(parent.snapshot.digest().unwrap(), parent.snapshot_digest);
        let patch = match profile {
            GenerationProfile::Sd15CpuTextToImage => {
                GenerationParameters::Image(Sd15ImageParameters::default())
            }
            _ => GenerationParameters::Video(H3VideoParameters::default()),
        };
        let inherited = service
            .reuse_result("session-a", &parent, RESULT, "job-2", patch)
            .unwrap();
        assert_eq!(
            literal_after(&inherited, "--cfg-scale"),
            literal_after(&initial, "--cfg-scale")
        );
    }
}

#[test]
fn reuse_rejects_nonfinite_duration_patches_instead_of_inheriting_parent_frames() {
    let service = service(GenerationProfile::H3CudaTextToVideoAudio);
    let initial = service.compile(request(video())).unwrap();
    let parent = record(&initial);
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let error = service
            .reuse_result(
                "session-a",
                &parent,
                RESULT,
                "job-2",
                GenerationParameters::Video(H3VideoParameters {
                    duration_seconds: Some(value),
                    ..Default::default()
                }),
            )
            .unwrap_err();
        assert_eq!(error.issues[0].field, "durationSeconds");
        assert_eq!(error.issues[0].code, "out_of_range");
    }
    assert_eq!(parent.snapshot.digest().unwrap(), parent.snapshot_digest);
    assert_eq!(literal_after(&initial, "--video-frames"), "22");
}

#[test]
fn cross_session_model_change_unknown_target_and_tampered_snapshot_cannot_be_reused() {
    let service = service(GenerationProfile::Sd15CpuTextToImage);
    let initial = service.compile(request(image())).unwrap();
    let original = record(&initial);
    let patch = || GenerationParameters::Image(Sd15ImageParameters::default());
    assert!(service
        .reuse_result("session-b", &original, RESULT, "job-2", patch())
        .is_err());
    assert!(service
        .reuse_result("session-a", &original, INPUT, "job-2", patch())
        .is_err());
    assert!(service
        .reuse_result("session-a", &original, RESULT, "job-1", patch())
        .is_err());
    let mut changed_binding = binding(GenerationProfile::Sd15CpuTextToImage);
    changed_binding.version_id = "v2".into();
    let changed =
        GenerationParameterService::new(GenerationProfile::Sd15CpuTextToImage, changed_binding)
            .unwrap();
    assert!(changed
        .reuse_result("session-a", &original, RESULT, "job-2", patch())
        .is_err());
    let mut tampered = original.clone();
    tampered.snapshot.engine.cpu_threads = 99;
    assert!(service
        .reuse_result("session-a", &tampered, RESULT, "job-2", patch())
        .is_err());
    tampered.snapshot_digest = tampered.snapshot.digest().unwrap();
    assert!(service
        .reuse_result("session-a", &tampered, RESULT, "job-2", patch())
        .is_err());
}

#[test]
fn native_plan_has_symbolic_paths_and_never_interprets_prompt_as_shell() {
    let service = service(GenerationProfile::Sd15CpuTextToImage);
    let prompt = "a sign saying --steps 999; $(echo hello)";
    let prepared = service
        .compile(request(GenerationParameters::Image(Sd15ImageParameters {
            prompt: Some(prompt.into()),
            ..Default::default()
        })))
        .unwrap();
    assert_eq!(literal_after(&prepared, "-p"), prompt);
    assert_eq!(literal_after(&prepared, "--steps"), "20");
    assert!(prepared
        .arguments()
        .contains(&NativeGenerationArgument::Component(
            GenerationAssetRole::Main
        )));
    assert!(matches!(
        prepared.arguments().last(),
        Some(NativeGenerationArgument::Output)
    ));
    let snapshot = serde_json::to_value(prepared.snapshot()).unwrap();
    assert!(snapshot.get("argv").is_none());
    assert!(!serde_json::to_string(&snapshot).unwrap().contains("D:\\"));
}

#[test]
fn a_different_task_adapter_does_not_inherit_diffusion_request_or_descriptor_types() {
    struct TestOnlyAdapter;
    impl ModelTaskParameterAdapter for TestOnlyAdapter {
        type Request = Vec<String>;
        type Prepared = usize;
        type Descriptor = BTreeMap<String, String>;
        fn identity(&self) -> GenerationParameterResult<TaskParameterIdentity> {
            Ok(TaskParameterIdentity {
                installation_id: "test-only".into(),
                version_id: "v1".into(),
                manifest_digest: "a".repeat(64),
                runtime_digest: "b".repeat(64),
                component_binding_digest: "c".repeat(64),
                task_variant: "test-only/count".into(),
                parameter_schema_id: "test-only-count-v1".into(),
            })
        }
        fn describe(&self) -> &Self::Descriptor {
            unreachable!("This test exercises typed dispatch, not a product capability")
        }
        fn prepare(&self, request: Self::Request) -> GenerationParameterResult<Self::Prepared> {
            Ok(request.len())
        }
    }
    let mut registry = TaskParameterRegistry::default();
    let key = registry.register(TestOnlyAdapter).unwrap();
    assert_eq!(
        registry
            .prepare(&key, vec!["a".into(), "b".into()])
            .unwrap(),
        2
    );
}
