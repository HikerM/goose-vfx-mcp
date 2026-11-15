use lumina_local_inference::lifecycle::*;
use std::collections::BTreeSet;

fn pocket_binding() -> AudioTaskModelBinding {
    audio_binding(AudioTaskProfile::PocketTtsEnglishCpu)
}

fn audio_binding(profile: AudioTaskProfile) -> AudioTaskModelBinding {
    let (sha256, size_bytes) = profile.recorded_model();
    let (spec_sha256, spec_bytes) = profile.recorded_model_spec();
    AudioTaskModelBinding {
        installation_id: "profiling-source-assets-not-installed".into(),
        version_id: "pocket-tts-fixed-resource-probe-v1".into(),
        manifest_digest: Sha256Digest::of_bytes(
            b"Profiling source identities; not an installed manifest",
        ),
        runtime: profile.runtime_build(),
        target_capabilities: BTreeSet::from([profile.capability()]),
        model: AudioTaskAssetBinding {
            artifact_id: "model".into(),
            sha256,
            size_bytes,
        },
        model_spec: AudioTaskAssetBinding {
            artifact_id: "spec".into(),
            sha256: spec_sha256,
            size_bytes: spec_bytes,
        },
    }
}

fn pocket_request() -> PocketTtsRequest {
    PocketTtsRequest {
        session_id: "task-resource-profiling".into(),
        job_id: "pocket-tts-fixed-v1".into(),
        text: "The red apple is on the wooden table.".into(),
        voice_reference: AudioTaskMediaReference {
            session_id: "task-resource-profiling".into(),
            result_id: "019a1111-2222-8333-8444-555555555555".into(),
            sha256: Sha256Digest::parse(
                "b29307e631d805d7079910c8c9c5ee14777549954b737e15a5770db7ac2da2e9",
            )
            .unwrap(),
            mime_type: "audio/wav".into(),
            size_bytes: 165_006,
            sample_rate: 16_000,
            channels: 1,
            sample_frames: 82_480,
        },
        seed: Some(42),
        temperature: None,
        max_frames: None,
        text_chunk_tokens: None,
        frames_after_eos: None,
    }
}

fn pocket() -> NativeTaskPlan {
    PocketTtsParameterService::new(pocket_binding())
        .unwrap()
        .compile(pocket_request())
        .unwrap()
        .into()
}

fn hardware() -> HardwareResourceSnapshot {
    HardwareResourceSnapshot {
        observed_at: 1_000,
        devices: vec![HardwareMemoryDevice {
            index: 0,
            backend: "CPU".into(),
            name: "CPU".into(),
            description: "Synthetic test device".into(),
            kind: "cpu".into(),
            total_bytes: 128 * 1024 * 1024 * 1024,
            free_bytes: 100 * 1024 * 1024 * 1024,
        }],
        storage_volume: "test-output-root".into(),
        disk_free_bytes: 1024 * 1024 * 1024 * 1024,
    }
}

#[test]
fn audio_smoke_and_arbitrarily_large_free_memory_cannot_issue_receipt() {
    let request = PocketTtsRequest {
        text: "A separate workload without a reviewed resource measurement.".into(),
        ..pocket_request()
    };
    let plan = PocketTtsParameterService::new(pocket_binding())
        .unwrap()
        .compile(request)
        .unwrap()
        .into();
    let assessment = assess_task_resources(&plan, &hardware(), 1_000).unwrap();
    let TaskResourceAssessment::NeedsProfiling(requirement) = assessment else {
        panic!("Smoke must not grant admission")
    };
    assert_eq!(requirement.key.kind, TaskResourceKind::Tts);
    assert_eq!(
        requirement.evidence,
        "2026-11-01-audio-asr-tts-feasibility.json"
    );
    assert!(requirement
        .missing
        .iter()
        .any(|reason| reason.contains("publication")));
}

#[test]
fn music_requires_its_own_measured_runtime_components_and_effective_workload() {
    let service = StableAudioMusicParameterService::new(audio_binding(
        AudioTaskProfile::StableAudioSmallMusicCpu,
    ))
    .unwrap();
    let input = serde_json::json!({"sessionId":"music-session","jobId":"music-job","prompt":"instrumental piano","seed":42});
    let prepare = |input| NativeTaskPlan::from(service.compile_json(input).unwrap());
    let original = prepare(input.clone());
    let key = TaskResourceKey::from_plan(&original).unwrap();
    assert_eq!(key.kind, TaskResourceKind::Music);
    for (field, value) in [
        ("prompt", serde_json::json!("percussion")),
        ("seed", serde_json::json!(43)),
    ] {
        let mut request = input.clone();
        request[field] = value;
        let other = prepare(request);
        let changed = TaskResourceKey::from_plan(&other).unwrap();
        assert_eq!(changed.runtime_digest, key.runtime_digest);
        assert_eq!(changed.components_digest, key.components_digest);
        assert_ne!(changed.workload_digest, key.workload_digest);
        let TaskResourceAssessment::NeedsProfiling(requirement) =
            assess_task_resources(&other, &hardware(), 1000).unwrap()
        else {
            panic!("Unmeasured music must request its own resource profile")
        };
        assert_eq!(
            requirement.evidence,
            "2026-11-01-audio-generation-feasibility.json"
        );
    }
}

#[test]
fn stale_future_and_impossible_observations_are_rejected_before_profile_lookup() {
    for (time, free) in [(939, 1), (1_001, 1), (1_000, u64::MAX)] {
        let mut hardware = hardware();
        hardware.observed_at = time;
        hardware.devices[0].free_bytes = free;
        assert!(matches!(
            assess_task_resources(&pocket(), &hardware, 1_000).unwrap(),
            TaskResourceAssessment::Unavailable(_)
        ));
    }
}

#[test]
fn reviewed_music_record_admits_only_its_measured_request_and_capacity() {
    let source = include_str!("../src/lifecycle/task_resources/stable-audio-music-cpu-case-1.json");
    let record: TaskResourceMeasurementRecord = serde_json::from_str(source).unwrap();
    record.validate_for_registration().unwrap();
    let service = StableAudioMusicParameterService::new(audio_binding(
        AudioTaskProfile::StableAudioSmallMusicCpu,
    ))
    .unwrap();
    let request = serde_json::json!({
        "sessionId": "music-resource-regression",
        "jobId": "measured-request",
        "prompt": "A gentle solo piano melody, clear notes, calm instrumental music, no vocals.",
        "steps": 8,
        "seed": 42
    });
    let prepare = |input| NativeTaskPlan::from(service.compile_json(input).unwrap());
    let original = prepare(request.clone());
    assert_eq!(TaskResourceKey::from_plan(&original).unwrap(), record.key);
    let now = record.hardware.observed_at;
    let TaskResourceAssessment::Admitted(admission) =
        assess_task_resources(&original, &record.hardware, now).unwrap()
    else {
        panic!("The reviewed exact music workload must be admitted")
    };
    let budget = record.reviewed_budget.as_ref().unwrap();
    assert_eq!(admission.budget(), budget);
    assert_eq!(
        admission.evidence_sha256(),
        &Sha256Digest::of_bytes(source.as_bytes())
    );
    let measured_peak = record.measured_host_peak_bytes().unwrap();
    assert_eq!(measured_peak, 7_033_057_280);
    let quantum = 128 * 1024 * 1024;
    assert_eq!(
        budget.host_ram_bytes,
        (measured_peak * 5).div_ceil(4 * quantum) * quantum
    );
    // The pinned execution evidence includes GGUF sidecars and simultaneous WAV copies.
    let native_directory_high_water_bytes = 41_587_688;
    assert!(budget.temporary_output_bytes >= native_directory_high_water_bytes);
    assert_eq!(budget.temporary_output_bytes, 64 * 1024 * 1024);
    assert_eq!(budget.output_byte_limit, 2 * 1024 * 1024);
    assert_eq!(budget.publication_copy_bytes, 2 * 1024 * 1024);
    assert_eq!(budget.publication_scratch_bytes, 4 * 1024 * 1024);
    assert_eq!(budget.timeout_seconds, 120);
    for (field, value) in [
        (
            "prompt",
            serde_json::json!("Gentle flute music without vocals."),
        ),
        ("seed", serde_json::json!(43)),
        ("steps", serde_json::json!(9)),
    ] {
        let mut changed = request.clone();
        changed[field] = value;
        let plan = prepare(changed);
        assert_ne!(TaskResourceKey::from_plan(&plan).unwrap(), record.key);
        assert!(
            matches!(
                assess_task_resources(&plan, &record.hardware, now).unwrap(),
                TaskResourceAssessment::NeedsProfiling(_)
            ),
            "Unmeasured {field} must not inherit the exact music budget"
        );
    }
    let mut insufficient_ram = record.hardware.clone();
    insufficient_ram.devices[0].free_bytes = budget.host_ram_bytes - 1;
    let mut insufficient_disk = record.hardware.clone();
    insufficient_disk.disk_free_bytes = budget.peak_disk_bytes().unwrap() - 1;
    let mut other_device = record.hardware.clone();
    other_device.devices[0].description = "Unmeasured CPU".into();
    for hardware in [insufficient_ram, insufficient_disk, other_device] {
        assert!(matches!(
            assess_task_resources(&original, &hardware, now).unwrap(),
            TaskResourceAssessment::Unavailable(_)
        ));
    }
}

#[test]
fn reviewed_pocket_records_admit_the_measured_domains_and_reject_insufficient_ram() {
    let sources = [
        (
            include_str!("../src/lifecycle/task_resources/pocket-tts-cpu-case-1.json"),
            "The red apple is on the wooden table.",
            false,
        ),
        (
            include_str!("../src/lifecycle/task_resources/pocket-tts-cpu-case-2.json"),
            "Please read the short note aloud.",
            false,
        ),
        (
            include_str!("../src/lifecycle/task_resources/pocket-tts-cpu-case-3.json"),
            "The morning train leaves at nine. We can walk to the station after breakfast.",
            false,
        ),
        (
            include_str!("../src/lifecycle/task_resources/pocket-tts-cpu-case-4.json"),
            "Please read the short note aloud.",
            true,
        ),
    ];
    let service = PocketTtsParameterService::new(pocket_binding()).unwrap();
    for (source, text, fixed_steps) in sources {
        let mut evidence: TaskResourceMeasurementRecord = serde_json::from_str(source).unwrap();
        let request = PocketTtsRequest {
            text: text.into(),
            seed: Some(if fixed_steps { 17 } else { 42 }),
            max_frames: fixed_steps.then_some(32),
            frames_after_eos: fixed_steps.then_some(32),
            ..pocket_request()
        };
        let plan: NativeTaskPlan = service.compile(request).unwrap().into();
        let now = evidence.hardware.observed_at;
        assert_eq!(TaskResourceKey::from_plan(&plan).unwrap(), evidence.key);
        let TaskResourceAssessment::Admitted(admission) =
            assess_task_resources(&plan, &evidence.hardware, now).unwrap()
        else {
            panic!("Measured workload must be admitted")
        };
        assert_eq!(
            admission.budget(),
            evidence.reviewed_budget.as_ref().unwrap()
        );
        let cpu = evidence
            .hardware
            .devices
            .iter_mut()
            .find(|device| device.kind == "cpu")
            .unwrap();
        cpu.free_bytes = admission.budget().host_ram_bytes - 1;
        assert!(matches!(
            assess_task_resources(&plan, &evidence.hardware, now).unwrap(),
            TaskResourceAssessment::Unavailable(_)
        ));
    }
}

#[test]
fn fixed_step_seed_coverage_preserves_receipt_identity_and_all_other_controls() {
    let service = PocketTtsParameterService::new(pocket_binding()).unwrap();
    let request = PocketTtsRequest {
        text: "Please read the short note aloud.".into(),
        max_frames: Some(32),
        frames_after_eos: Some(32),
        ..pocket_request()
    };
    let plan = |r| NativeTaskPlan::from(service.compile(r).unwrap());
    let reference = plan(request.clone());
    let other_seed = plan(PocketTtsRequest {
        seed: Some(17),
        ..request.clone()
    });
    let key = TaskResourceKey::from_plan(&reference).unwrap();
    assert_eq!(TaskResourceKey::from_plan(&other_seed).unwrap(), key);
    assert_ne!(
        reference.snapshot_digest().unwrap(),
        other_seed.snapshot_digest().unwrap()
    );
    for different in [
        PocketTtsRequest {
            text: "A new sentence.".into(),
            ..request.clone()
        },
        PocketTtsRequest {
            temperature: Some(0.5),
            ..request.clone()
        },
        PocketTtsRequest {
            max_frames: Some(31),
            ..request.clone()
        },
        PocketTtsRequest {
            frames_after_eos: Some(31),
            ..request.clone()
        },
        PocketTtsRequest {
            voice_reference: AudioTaskMediaReference {
                sha256: Sha256Digest::of_bytes(b"different voice"),
                ..request.voice_reference.clone()
            },
            ..request.clone()
        },
    ] {
        assert_ne!(TaskResourceKey::from_plan(&plan(different)).unwrap(), key);
    }
    let default = plan(PocketTtsRequest {
        max_frames: None,
        frames_after_eos: None,
        ..request.clone()
    });
    let other_default = plan(PocketTtsRequest {
        max_frames: None,
        frames_after_eos: None,
        seed: Some(17),
        ..request
    });
    assert_ne!(
        TaskResourceKey::from_plan(&default).unwrap(),
        TaskResourceKey::from_plan(&other_default).unwrap()
    );
}

#[test]
fn measured_single_chunk_shape_keeps_the_full_requested_text_in_each_snapshot() {
    let service = PocketTtsParameterService::new(pocket_binding()).unwrap();
    let request = |text: &str| PocketTtsRequest {
        text: text.into(),
        max_frames: Some(32),
        frames_after_eos: Some(32),
        ..pocket_request()
    };
    let plan = |text| NativeTaskPlan::from(service.compile(request(text)).unwrap());
    let anchor = plan("Hello there.");
    let key = TaskResourceKey::from_plan(&anchor).unwrap();
    let record: TaskResourceMeasurementRecord = serde_json::from_str(include_str!(
        "../src/lifecycle/task_resources/pocket-tts-cpu-case-10.json"
    ))
    .unwrap();
    assert_eq!(key, record.key);
    let assess = |plan: &NativeTaskPlan| {
        assess_task_resources(plan, &record.hardware, record.hardware.observed_at).unwrap()
    };
    for text in [
        "A.",
        "Fresh blue rain falls now.",
        "QQKWKWKKWQJXWZWJXKWWJXQZQZWZWK.",
        "A cloud floats by.",
    ] {
        let next = plan(text);
        assert_eq!(TaskResourceKey::from_plan(&next).unwrap(), key);
        assert!(matches!(assess(&next), TaskResourceAssessment::Admitted(_)));
        assert_ne!(
            next.snapshot_digest().unwrap(),
            anchor.snapshot_digest().unwrap()
        );
    }
    for text in [
        "QQKWKWKKWQJXWZWJXKWWJXQZQZWZWKQ.",
        "Hello. Go.",
        "Bonjour été.",
        "Wait, then go.",
        "Twelve  words.",
    ] {
        let outside = plan(text);
        assert_ne!(TaskResourceKey::from_plan(&outside).unwrap(), key);
        assert!(matches!(
            assess(&outside),
            TaskResourceAssessment::NeedsProfiling(_)
        ));
    }
}

#[test]
fn an_unmeasured_runtime_keeps_seed_and_text_exact_under_the_same_frame_limits() {
    let mut binding = pocket_binding();
    binding.runtime = AudioTaskProfile::PocketTtsEnglishCpu.legacy_runtime_build();
    let service = PocketTtsParameterService::new(binding).unwrap();
    let key = |text: &str, seed| {
        let request = PocketTtsRequest {
            text: text.into(),
            seed: Some(seed),
            max_frames: Some(32),
            frames_after_eos: Some(32),
            ..pocket_request()
        };
        let plan: NativeTaskPlan = service.compile(request).unwrap().into();
        assert!(matches!(
            assess_task_resources(&plan, &hardware(), 1_000).unwrap(),
            TaskResourceAssessment::NeedsProfiling(_)
        ));
        TaskResourceKey::from_plan(&plan).unwrap()
    };
    assert_ne!(key("Hello there.", 0), key("Hello there.", 17));
    assert_ne!(key("Hello there.", 0), key("Go home.", 0));
}

#[test]
fn legitimate_temperature_and_seed_changes_create_distinct_measurement_domains() {
    let original = TaskResourceKey::from_plan(&pocket()).unwrap();
    let service = PocketTtsParameterService::new(pocket_binding()).unwrap();
    for request in [
        PocketTtsRequest {
            seed: Some(43),
            ..pocket_request()
        },
        PocketTtsRequest {
            temperature: Some(0.6),
            ..pocket_request()
        },
    ] {
        let plan = NativeTaskPlan::from(service.compile(request).unwrap());
        let changed = TaskResourceKey::from_plan(&plan).unwrap();
        assert_ne!(changed.workload_digest, original.workload_digest);
        assert_eq!(changed.components_digest, original.components_digest);
        assert!(matches!(
            assess_task_resources(&plan, &hardware(), 1_000).unwrap(),
            TaskResourceAssessment::NeedsProfiling(_)
        ));
    }
}
