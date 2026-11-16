use super::*;
use crate::lifecycle::*;

#[cfg(windows)]
#[path = "pocket_probe.rs"]
mod pocket_probe;

#[cfg(windows)]
#[path = "sd15_probe.rs"]
mod sd15_probe;

#[cfg(windows)]
#[path = "music_probe.rs"]
mod music_probe;

fn plan(video: bool, version: &str, seed: u32) -> NativeTaskPlan {
    plan_with_prompt(video, version, seed, "test prompt")
}

fn plan_with_prompt(video: bool, version: &str, seed: u32, prompt: &str) -> NativeTaskPlan {
    let profile = if video {
        GenerationProfile::H3CudaTextToVideoAudio
    } else {
        GenerationProfile::Sd15CpuTextToImage
    };
    let binding = GenerationModelBinding {
        installation_id: "synthetic-installation".into(),
        version_id: version.into(),
        manifest_digest: Sha256Digest::of_bytes(version.as_bytes()),
        runtime: profile.runtime_build(),
        target_capabilities: profile.required_capabilities(),
        assets: profile
            .recorded_assets()
            .into_iter()
            .map(|(role, (sha256, size_bytes))| {
                (
                    role,
                    GenerationAssetBinding {
                        artifact_id: format!("{role:?}"),
                        sha256,
                        size_bytes,
                    },
                )
            })
            .collect(),
    };
    GenerationParameterService::new(profile, binding)
        .unwrap()
        .compile(GenerationTaskRequest {
            session_id: "synthetic-session".into(),
            job_id: "synthetic-job".into(),
            parameters: if video {
                GenerationParameters::Video(H3VideoParameters {
                    prompt: Some(prompt.into()),
                    seed: Some(GenerationSeed::Value { value: seed }),
                    ..Default::default()
                })
            } else {
                GenerationParameters::Image(Sd15ImageParameters {
                    prompt: Some(prompt.into()),
                    seed: Some(GenerationSeed::Value { value: seed }),
                    ..Default::default()
                })
            },
        })
        .unwrap()
        .into()
}

fn hardware() -> HardwareResourceSnapshot {
    HardwareResourceSnapshot {
        observed_at: 1000,
        storage_volume: "synthetic-output-root".into(),
        disk_free_bytes: 220,
        devices: vec![
            HardwareMemoryDevice {
                index: 0,
                backend: "CPU".into(),
                name: "CPU".into(),
                description: "test CPU".into(),
                kind: "cpu".into(),
                total_bytes: 4096,
                free_bytes: 1000,
            },
            HardwareMemoryDevice {
                index: 1,
                backend: "CUDA".into(),
                name: "CUDA0".into(),
                description: "test GPU".into(),
                kind: "gpu".into(),
                total_bytes: 4096,
                free_bytes: 2000,
            },
        ],
    }
}

fn profile(plan: &NativeTaskPlan) -> TaskResourceProfile {
    let key = TaskResourceKey::from_plan(plan).unwrap();
    let gpu_memory = if key.requires_gpu {
        vec![TaskGpuMemoryBudget {
            device: TaskResourceDevice::from(&hardware().devices[1]),
            required_bytes: 2000,
        }]
    } else {
        vec![]
    };
    TaskResourceProfile {
        key,
        host: TaskResourceDevice::from(&hardware().devices[0]),
        budget: TaskResourceBudget {
            host_ram_bytes: 500,
            gpu_memory,
            temporary_output_bytes: 100,
            publication_copy_bytes: 100,
            publication_scratch_bytes: 20,
            output_byte_limit: 100,
            timeout_seconds: 5,
        },
        evidence_sha256: Sha256Digest::of_bytes(
            b"synthetic evaluator fixture, never trusted production evidence",
        ),
    }
}

fn publication_plans() -> Vec<NativeTaskPlan> {
    let binding = |profile: AudioTaskProfile| {
        let (sha256, size_bytes) = profile.recorded_model();
        let (spec_sha256, spec_bytes) = profile.recorded_model_spec();
        AudioTaskModelBinding {
            installation_id: "synthetic-publication-installation".into(),
            version_id: "synthetic-publication-version".into(),
            manifest_digest: Sha256Digest::of_bytes(b"synthetic publication manifest"),
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
    };
    let media = serde_json::json!({
        "sessionId":"synthetic-session", "resultId":"019a1111-2222-8333-8444-555555555555",
        "sha256":Sha256Digest::of_bytes(b"synthetic audio"), "mimeType":"audio/wav",
        "sizeBytes":165006, "sampleRate":16000, "channels":1, "sampleFrames":82480
    });
    vec![
        plan(false, "v1", 42),
        plan(true, "v1", 42),
        PocketTtsParameterService::new(binding(AudioTaskProfile::PocketTtsEnglishCpu))
            .unwrap()
            .compile_json(
                serde_json::json!({"sessionId":"synthetic-session", "jobId":"synthetic-tts",
                "text":"The red apple is on the wooden table.", "voiceReference":media, "seed":42}),
            )
            .unwrap()
            .into(),
        Qwen3AsrParameterService::new(binding(AudioTaskProfile::Qwen3Asr06bCpu))
            .unwrap()
            .compile_json(
                serde_json::json!({"sessionId":"synthetic-session", "jobId":"synthetic-asr",
                "audio":media}),
            )
            .unwrap()
            .into(),
        StableAudioMusicParameterService::new(binding(AudioTaskProfile::StableAudioSmallMusicCpu))
            .unwrap()
            .compile_json(
                serde_json::json!({"sessionId":"synthetic-session", "jobId":"synthetic-music",
                "prompt":"instrumental piano", "seed":42}),
            )
            .unwrap()
            .into(),
    ]
}

fn project_publication(source: &[u8]) -> TaskPublicationContext {
    TaskPublicationContext::ProjectParameterReuseV1 {
        implementation_sources_digest: Sha256Digest::of_bytes(source),
    }
}

fn project_publication_v2(tier: ProjectReusePayloadTier) -> TaskPublicationContext {
    TaskPublicationContext::ProjectParameterReuseV2 {
        implementation_sources_digest: Sha256Digest::of_bytes(
            b"project publication implementation A",
        ),
        payload_tier: tier,
    }
}

fn music_domain_plan(prompt: &str, seed: u32, steps: u32) -> NativeTaskPlan {
    let NativeTaskPlan::StableAudioMusic(base) = publication_plans().pop().unwrap() else {
        unreachable!()
    };
    StableAudioMusicParameterService::new(base.snapshot().metadata.binding.clone())
        .unwrap()
        .compile_json(serde_json::json!({
            "sessionId":"synthetic-session", "jobId":"synthetic-music-domain",
            "prompt":prompt, "seed":seed, "steps":steps
        }))
        .unwrap()
        .into()
}

fn measured_music_project_small() -> (TaskResourceMeasurementRecord, TaskPublicationContext) {
    let record =
        serde_json::from_str(include_str!("stable-audio-music-project-small-v1.json")).unwrap();
    let context = TaskPublicationContext::ProjectParameterReuseV2 {
        implementation_sources_digest: Sha256Digest::parse(
            "1e3ef4cb1e10415c8389b9d7203a1da23b9bcbb0bc5e854dcac174c7cb32ef67",
        )
        .unwrap(),
        payload_tier: ProjectReusePayloadTier::Small,
    };
    (record, context)
}

#[test]
fn registered_music_small_domain_admits_varied_prompts_and_seeds_on_measured_hardware() {
    let (record, context) = measured_music_project_small();
    let evidence =
        Sha256Digest::of_bytes(include_bytes!("stable-audio-music-project-small-v1.json"));
    let unicode_boundary = format!("{}a", "琴".repeat(341));
    for (prompt, seed) in [
        ("a", 0),
        ("Gentle strings with a piano melody", 2718281828),
        (unicode_boundary.as_str(), u32::MAX),
        ("A \"soft\" piano \\ calm strings", 1),
    ] {
        let plan = music_domain_plan(prompt, seed, 8);
        assert_eq!(
            TaskResourceKey::from_plan_with_publication(&plan, &context).unwrap(),
            record.key
        );
        let TaskResourceAssessment::Admitted(mut receipt) = assess_task_resources_with_publication(
            &plan,
            &record.hardware,
            record.hardware.observed_at,
            &context,
        )
        .unwrap() else {
            panic!("Registered Small music domain did not admit {prompt:?}, seed {seed}")
        };
        assert_eq!(receipt.evidence_sha256(), &evidence);
        assert_eq!(Some(receipt.budget()), record.reviewed_budget.as_ref());
        receipt
            .verify_now(&plan, &record.hardware, record.hardware.observed_at)
            .unwrap();
    }
}

#[test]
fn registered_music_domain_receipt_remains_bound_to_the_full_execution_snapshot() {
    let (record, context) = measured_music_project_small();
    let first = music_domain_plan("Warm strings", 42, 8);
    let NativeTaskPlan::StableAudioMusic(base) = &first else {
        unreachable!()
    };
    let other_session: NativeTaskPlan =
        StableAudioMusicParameterService::new(base.snapshot().metadata.binding.clone())
            .unwrap()
            .compile_json(serde_json::json!({
                "sessionId":"another-session", "jobId":"another-job",
                "prompt":"Warm strings", "seed":42, "steps":8
            }))
            .unwrap()
            .into();
    let TaskResourceAssessment::Admitted(mut receipt) = assess_task_resources_with_publication(
        &first,
        &record.hardware,
        record.hardware.observed_at,
        &context,
    )
    .unwrap() else {
        panic!("Registered Small music domain did not admit the original snapshot")
    };
    for changed in [
        music_domain_plan("Different piano melody", 42, 8),
        music_domain_plan("Warm strings", 43, 8),
        other_session,
    ] {
        assert_eq!(
            TaskResourceKey::from_plan_with_publication(&changed, &context).unwrap(),
            record.key
        );
        assert_ne!(
            first.snapshot_digest().unwrap(),
            changed.snapshot_digest().unwrap()
        );
        assert!(receipt
            .verify_now(&changed, &record.hardware, record.hardware.observed_at)
            .is_err());
    }
    receipt
        .verify_now(&first, &record.hardware, record.hardware.observed_at)
        .unwrap();
}

#[test]
fn registered_music_small_budget_rejects_unmeasured_parameters_and_publication_contexts() {
    let (record, context) = measured_music_project_small();
    for outside in [
        music_domain_plan(&"a".repeat(1025), 42, 8),
        music_domain_plan("Warm strings", 42, 9),
    ] {
        assert!(matches!(
            assess_task_resources_with_publication(
                &outside,
                &record.hardware,
                record.hardware.observed_at,
                &context,
            )
            .unwrap(),
            TaskResourceAssessment::NeedsProfiling(_)
        ));
    }

    let evidence =
        Sha256Digest::of_bytes(include_bytes!("stable-audio-music-project-small-v1.json"));
    let registered = record.clone().into_profile(evidence.clone()).unwrap();
    let first = music_domain_plan(
        "A gentle solo piano melody, clear notes, calm instrumental music, no vocals.",
        42,
        8,
    );
    let TaskPublicationContext::ProjectParameterReuseV2 {
        implementation_sources_digest,
        ..
    } = context
    else {
        unreachable!()
    };
    for other in [
        TaskPublicationContext::Session,
        TaskPublicationContext::ProjectParameterReuseV1 {
            implementation_sources_digest: implementation_sources_digest.clone(),
        },
        TaskPublicationContext::ProjectParameterReuseV2 {
            implementation_sources_digest,
            payload_tier: ProjectReusePayloadTier::Standard,
        },
        project_publication_v2(ProjectReusePayloadTier::Small),
    ] {
        assert!(matches!(
            assess_with_profiles_and_publication(
                &first,
                &record.hardware,
                record.hardware.observed_at,
                std::slice::from_ref(&registered),
                &other,
            )
            .unwrap(),
            TaskResourceAssessment::NeedsProfiling(_)
        ));
        match assess_task_resources_with_publication(
            &first,
            &record.hardware,
            record.hardware.observed_at,
            &other,
        )
        .unwrap()
        {
            TaskResourceAssessment::Admitted(receipt) => {
                assert!(matches!(other, TaskPublicationContext::Session));
                assert_ne!(receipt.evidence_sha256(), &evidence);
            }
            TaskResourceAssessment::NeedsProfiling(_) => {}
            TaskResourceAssessment::Unavailable(reason) => {
                panic!("Measured hardware unexpectedly unavailable: {reason:?}")
            }
        }
    }
}

#[test]
fn music_v2_domain_reuses_cost_keys_but_not_execution_receipts_or_legacy_keys() {
    let first = music_domain_plan("Warm strings", 0, 8);
    let other = music_domain_plan("钢琴与弦乐", u32::MAX, 8);
    let context = project_publication_v2(ProjectReusePayloadTier::Small);
    let key = TaskResourceKey::from_plan_with_publication(&first, &context).unwrap();
    assert_eq!(
        key,
        TaskResourceKey::from_plan_with_publication(&other, &context).unwrap()
    );
    for legacy in [
        TaskPublicationContext::Session,
        project_publication(b"legacy host"),
    ] {
        assert_ne!(
            TaskResourceKey::from_plan_with_publication(&first, &legacy).unwrap(),
            TaskResourceKey::from_plan_with_publication(&other, &legacy).unwrap()
        );
    }
    let mut measured = profile(&first);
    measured.key = key;
    let TaskResourceAssessment::Admitted(mut receipt) = assess_with_profiles_and_publication(
        &first,
        &hardware(),
        1000,
        std::slice::from_ref(&measured),
        &context,
    )
    .unwrap() else {
        panic!("Synthetic matching candidate domain profile was rejected")
    };
    receipt.verify_now(&first, &hardware(), 1000).unwrap();
    assert!(receipt.verify_now(&other, &hardware(), 1000).is_err());
    assert!(matches!(
        assess_with_profiles_and_publication(&other, &hardware(), 1000, &[measured], &context)
            .unwrap(),
        TaskResourceAssessment::Admitted(_)
    ));
}

#[test]
fn music_v2_domain_requires_new_evidence_and_outside_requests_keep_exact_keys() {
    let first = music_domain_plan("Warm strings", 42, 8);
    let context = project_publication_v2(ProjectReusePayloadTier::Small);
    let exact_project_key = |plan: &NativeTaskPlan| {
        let mut key = TaskResourceKey::from_plan(plan).unwrap();
        key.workload_digest = Sha256Digest::of_serializable(&(
            "task-publication-context-v1",
            &key.workload_digest,
            &context,
        ))
        .unwrap();
        key
    };
    let mut old_exact = profile(&first);
    old_exact.key = exact_project_key(&first);
    assert!(matches!(
        assess_with_profiles_and_publication(&first, &hardware(), 1000, &[old_exact], &context)
            .unwrap(),
        TaskResourceAssessment::NeedsProfiling(_)
    ));
    let mut domain = profile(&first);
    domain.key = TaskResourceKey::from_plan_with_publication(&first, &context).unwrap();
    for outside in [
        music_domain_plan(&"a".repeat(1025), 42, 8),
        music_domain_plan("Warm strings", 42, 9),
    ] {
        assert_eq!(
            TaskResourceKey::from_plan_with_publication(&outside, &context).unwrap(),
            exact_project_key(&outside)
        );
        assert!(matches!(
            assess_with_profiles_and_publication(
                &outside,
                &hardware(),
                1000,
                std::slice::from_ref(&domain),
                &context,
            )
            .unwrap(),
            TaskResourceAssessment::NeedsProfiling(_)
        ));
    }
    assert!(matches!(
        assess_with_profiles_and_publication(
            &first,
            &hardware(),
            1000,
            &trusted_profiles().unwrap(),
            &context,
        )
        .unwrap(),
        TaskResourceAssessment::NeedsProfiling(_)
    ));
}

#[test]
fn project_payload_tiers_serialize_and_define_candidate_host_ceilings() {
    for (tier, name, source, record, output) in [
        (
            ProjectReusePayloadTier::Small,
            "small",
            256 * 1024,
            256 * 1024,
            1024 * 1024,
        ),
        (
            ProjectReusePayloadTier::Standard,
            "standard",
            64 * 1024 * 1024,
            1024 * 1024,
            32 * 1024 * 1024,
        ),
    ] {
        assert_eq!(serde_json::to_value(tier).unwrap(), serde_json::json!(name));
        assert_eq!(
            serde_json::from_value::<ProjectReusePayloadTier>(serde_json::json!(name)).unwrap(),
            tier
        );
        assert_eq!(tier.source_assets_limit(), source);
        assert_eq!(tier.record_limit(), record);
        assert_eq!(tier.output_archive_limit(), output);
    }
    assert!(
        serde_json::from_value::<ProjectReusePayloadTier>(serde_json::json!("unbounded")).is_err()
    );
}

#[test]
fn project_v2_versions_and_payload_tiers_have_distinct_keys_for_every_task_kind() {
    let v1 = project_publication(b"project publication implementation A");
    let small = project_publication_v2(ProjectReusePayloadTier::Small);
    let standard = project_publication_v2(ProjectReusePayloadTier::Standard);
    for plan in publication_plans() {
        let session = TaskResourceKey::from_plan(&plan).unwrap();
        let old = TaskResourceKey::from_plan_with_publication(&plan, &v1).unwrap();
        let small_key = TaskResourceKey::from_plan_with_publication(&plan, &small).unwrap();
        let standard_key = TaskResourceKey::from_plan_with_publication(&plan, &standard).unwrap();
        assert_ne!(old, small_key);
        assert_ne!(old, standard_key);
        assert_ne!(small_key, standard_key);
        assert_ne!(session, small_key);
        assert_ne!(session, standard_key);
        assert_eq!(small_key.runtime_digest, standard_key.runtime_digest);
        assert_eq!(small_key.components_digest, standard_key.components_digest);
        assert_eq!(small_key.kind, standard_key.kind);
        assert_eq!(small_key.requires_gpu, standard_key.requires_gpu);
    }
}

#[test]
fn project_v2_identity_is_workload_tier_and_implementation_without_parent_identifiers() {
    for tier in [
        ProjectReusePayloadTier::Small,
        ProjectReusePayloadTier::Standard,
    ] {
        let context = project_publication_v2(tier);
        assert_eq!(
            serde_json::to_value(&context).unwrap(),
            serde_json::json!({
                "kind":"project_parameter_reuse_v2",
                "implementationSourcesDigest":Sha256Digest::of_bytes(b"project publication implementation A"),
                "payloadTier":tier,
            })
        );
        for (first, independently_prepared) in
            publication_plans().iter().zip(publication_plans().iter())
        {
            assert_eq!(
                TaskResourceKey::from_plan_with_publication(first, &context).unwrap(),
                TaskResourceKey::from_plan_with_publication(
                    independently_prepared,
                    &project_publication_v2(tier)
                )
                .unwrap()
            );
        }
    }
}

#[test]
fn project_v2_cannot_borrow_session_v1_or_existing_production_profiles() {
    let v1 = project_publication(b"project publication implementation A");
    let production = trusted_profiles().unwrap();
    for plan in publication_plans() {
        let session = profile(&plan);
        let mut prior_project = session.clone();
        prior_project.key = TaskResourceKey::from_plan_with_publication(&plan, &v1).unwrap();
        let mut profiles = production.clone();
        profiles.extend([session, prior_project]);
        for tier in [
            ProjectReusePayloadTier::Small,
            ProjectReusePayloadTier::Standard,
        ] {
            let context = project_publication_v2(tier);
            let assessment =
                assess_with_profiles_and_publication(&plan, &hardware(), 1000, &profiles, &context)
                    .unwrap();
            let TaskResourceAssessment::NeedsProfiling(requirement) = assessment else {
                panic!("Unmeasured V2 payload tier borrowed an existing resource profile")
            };
            assert_eq!(
                requirement.key,
                TaskResourceKey::from_plan_with_publication(&plan, &context).unwrap()
            );
            assert!(requirement
                .missing
                .contains(&"measured project parameter reuse publication path"));
        }
    }
}

#[test]
fn project_v2_receipt_recheck_rejects_a_different_payload_tier() {
    let small = project_publication_v2(ProjectReusePayloadTier::Small);
    let standard = project_publication_v2(ProjectReusePayloadTier::Standard);
    let plan = plan(false, "v1", 42);
    let mut measured = profile(&plan);
    measured.key = TaskResourceKey::from_plan_with_publication(&plan, &small).unwrap();
    let TaskResourceAssessment::Admitted(mut receipt) = assess_with_profiles_and_publication(
        &plan,
        &hardware(),
        1000,
        std::slice::from_ref(&measured),
        &small,
    )
    .unwrap() else {
        panic!("Synthetic matching V2 profile was rejected")
    };
    receipt.verify_now(&plan, &hardware(), 1000).unwrap();
    assert!(matches!(
        assess_with_profiles_and_publication(&plan, &hardware(), 1000, &[measured], &standard)
            .unwrap(),
        TaskResourceAssessment::NeedsProfiling(_)
    ));
    receipt.publication = standard;
    assert!(receipt.verify_now(&plan, &hardware(), 1000).is_err());
}

#[test]
fn explicit_session_publication_preserves_every_task_kind_legacy_key_and_profile() {
    for plan in publication_plans() {
        let legacy = TaskResourceKey::from_plan(&plan).unwrap();
        let explicit =
            TaskResourceKey::from_plan_with_publication(&plan, &TaskPublicationContext::Session)
                .unwrap();
        assert_eq!(
            serde_json::to_vec(&legacy).unwrap(),
            serde_json::to_vec(&explicit).unwrap()
        );
        let profiles = [profile(&plan)];
        assert!(matches!(
            assess_with_profiles(&plan, &hardware(), 1000, &profiles).unwrap(),
            TaskResourceAssessment::Admitted(_)
        ));
        assert!(matches!(
            assess_with_profiles_and_publication(
                &plan,
                &hardware(),
                1000,
                &profiles,
                &TaskPublicationContext::Session
            )
            .unwrap(),
            TaskResourceAssessment::Admitted(_)
        ));
    }
}

#[test]
fn project_publication_cannot_borrow_a_session_profile_for_any_task_kind() {
    let publication = project_publication(b"project publication implementation A");
    for plan in publication_plans() {
        let session = TaskResourceKey::from_plan(&plan).unwrap();
        let project = TaskResourceKey::from_plan_with_publication(&plan, &publication).unwrap();
        assert_eq!(session.kind, project.kind);
        assert_eq!(session.runtime_digest, project.runtime_digest);
        assert_eq!(session.components_digest, project.components_digest);
        assert_eq!(session.requires_gpu, project.requires_gpu);
        assert_ne!(session.workload_digest, project.workload_digest);
        let outcome = assess_with_profiles_and_publication(
            &plan,
            &hardware(),
            1000,
            &[profile(&plan)],
            &publication,
        )
        .unwrap();
        let TaskResourceAssessment::NeedsProfiling(requirement) = outcome else {
            panic!("Project task reused a session profile")
        };
        assert_eq!(requirement.key, project);
    }
}

#[test]
fn matching_project_profile_receipt_retains_publication_context_during_owner_recheck() {
    let publication = project_publication(b"project publication implementation A");
    for plan in publication_plans() {
        let mut measured = profile(&plan);
        measured.key = TaskResourceKey::from_plan_with_publication(&plan, &publication).unwrap();
        let TaskResourceAssessment::Admitted(mut receipt) = assess_with_profiles_and_publication(
            &plan,
            &hardware(),
            1000,
            &[measured.clone()],
            &publication,
        )
        .unwrap() else {
            panic!("Synthetic matching project profile was rejected")
        };
        receipt.verify_now(&plan, &hardware(), 1000).unwrap();
        assert!(matches!(
            assess_with_profiles(&plan, &hardware(), 1000, &[measured]).unwrap(),
            TaskResourceAssessment::NeedsProfiling(_)
        ));
        receipt.publication = TaskPublicationContext::Session;
        assert!(receipt.verify_now(&plan, &hardware(), 1000).is_err());
    }
}

#[test]
fn changed_project_publication_sources_invalidate_profiles_and_existing_receipts() {
    let publication = project_publication(b"project publication implementation A");
    let changed = project_publication(b"project publication implementation B");
    let plan = plan(false, "v1", 42);
    let mut measured = profile(&plan);
    measured.key = TaskResourceKey::from_plan_with_publication(&plan, &publication).unwrap();
    let TaskResourceAssessment::Admitted(mut receipt) = assess_with_profiles_and_publication(
        &plan,
        &hardware(),
        1000,
        std::slice::from_ref(&measured),
        &publication,
    )
    .unwrap() else {
        panic!("Synthetic matching project profile was rejected")
    };
    assert!(matches!(
        assess_with_profiles_and_publication(&plan, &hardware(), 1000, &[measured], &changed)
            .unwrap(),
        TaskResourceAssessment::NeedsProfiling(_)
    ));
    receipt.publication = changed;
    assert!(receipt.verify_now(&plan, &hardware(), 1000).is_err());
}

#[cfg(windows)]
pub(crate) fn controlled_test_admission() -> (TaskResourceAdmission, Sha256Digest) {
    let plan = plan(false, "controlled-child", 42);
    let TaskResourceAssessment::Admitted(receipt) =
        assess_with_profiles(&plan, &hardware(), 1000, &[profile(&plan)]).unwrap()
    else {
        panic!("controlled child needs a valid synthetic admission");
    };
    (receipt, plan.snapshot_digest().unwrap())
}

#[test]
fn synthetic_profile_checks_ram_gpu_and_all_three_disk_costs() {
    let plan = plan(true, "v1", 42);
    let profile = profile(&plan);
    assert!(matches!(
        assess_with_profiles(&plan, &hardware(), 1000, std::slice::from_ref(&profile)).unwrap(),
        TaskResourceAssessment::Admitted(_)
    ));
    for (ram, gpu, disk) in [(499, 2000, 220), (1000, 1999, 220), (1000, 2000, 219)] {
        let mut observed = hardware();
        observed.devices[0].free_bytes = ram;
        observed.devices[1].free_bytes = gpu;
        observed.disk_free_bytes = disk;
        assert!(matches!(
            assess_with_profiles(&plan, &observed, 1000, std::slice::from_ref(&profile)).unwrap(),
            TaskResourceAssessment::Unavailable(_)
        ));
    }
}

#[test]
fn nominal_free_vram_on_another_device_does_not_satisfy_profile() {
    let plan = plan(true, "v1", 42);
    let mut observed = hardware();
    observed.devices[1].description = "different GPU".into();
    assert!(matches!(
        assess_with_profiles(&plan, &observed, 1000, &[profile(&plan)]).unwrap(),
        TaskResourceAssessment::Unavailable(_)
    ));
}

#[test]
fn admission_uses_a_later_measured_profile_when_earlier_hardware_or_capacity_differs() {
    let plan = plan(true, "v1", 42);
    let mut other_gpu = profile(&plan);
    other_gpu.budget.gpu_memory[0].device.description = "another measured GPU".into();
    let mut expensive = profile(&plan);
    expensive.budget.host_ram_bytes = hardware().devices[0].free_bytes + 1;
    let mut usable = profile(&plan);
    usable.evidence_sha256 = Sha256Digest::of_bytes(b"measured for this host and GPU");
    let profiles = [other_gpu, expensive, usable.clone()];
    let TaskResourceAssessment::Admitted(receipt) =
        assess_with_profiles(&plan, &hardware(), 1000, &profiles).unwrap()
    else {
        panic!("a matching measured profile has adequate capacity");
    };
    assert_eq!(receipt.evidence_sha256(), &usable.evidence_sha256);
    assert_eq!(receipt.budget(), &usable.budget);
    let TaskResourceAssessment::Unavailable(reason) =
        assess_with_profiles(&plan, &hardware(), 1000, &profiles[..2]).unwrap()
    else {
        panic!("known profiles cannot run on this observation");
    };
    assert_eq!(reason.code, "resource_capacity_or_device_mismatch");
    assert!(reason.detail.contains("free VRAM") && reason.detail.contains("free RAM"));
}

#[test]
fn receipt_rechecks_identity_expiration_storage_and_current_capacity() {
    let first = plan(false, "v1", 42);
    let TaskResourceAssessment::Admitted(mut receipt) =
        assess_with_profiles(&first, &hardware(), 1000, &[profile(&first)]).unwrap()
    else {
        panic!()
    };
    assert!(receipt.verify_now(&first, &hardware(), 1060).is_ok());
    assert!(receipt.verify_now(&first, &hardware(), 1061).is_err());
    assert!(receipt
        .verify_now(&plan(false, "v2", 42), &hardware(), 1000)
        .is_err());
    assert!(receipt
        .verify_now(&plan(false, "v1", 43), &hardware(), 1000)
        .is_err());
    let mut changed = hardware();
    changed.storage_volume = "other-output-root".into();
    assert!(receipt.verify_now(&first, &changed, 1000).is_err());
    changed = hardware();
    changed.devices[0].free_bytes = 1;
    assert!(receipt.verify_now(&first, &changed, 1000).is_err());
}

#[test]
fn rehashing_past_ttl_requires_new_observation_before_execution() {
    let first = plan(false, "v1", 42);
    let TaskResourceAssessment::Admitted(mut receipt) =
        assess_with_profiles(&first, &hardware(), 1000, &[profile(&first)]).unwrap()
    else {
        panic!()
    };
    let original = receipt.observation_digest().clone();
    assert!(receipt.verify_now(&first, &hardware(), 1075).is_err());
    let mut refreshed = hardware();
    refreshed.observed_at = 1075;
    refreshed.disk_free_bytes = 219;
    assert!(receipt.verify_now(&first, &refreshed, 1075).is_err());
    assert_eq!(receipt.observation_digest(), &original);
    refreshed.disk_free_bytes = 220;
    receipt.verify_now(&first, &refreshed, 1075).unwrap();
    assert_eq!(
        receipt.observation_digest(),
        &Sha256Digest::of_serializable(&refreshed).unwrap()
    );
    refreshed.observed_at -= 1;
    assert!(receipt.verify_now(&first, &refreshed, 1075).is_err());
}

#[test]
fn another_task_family_or_unmeasured_effective_parameters_need_profiling() {
    let exact_prompt = "sky BREAK forest";
    let original = plan_with_prompt(false, "v1", 42, exact_prompt);
    for changed in [
        plan_with_prompt(false, "v1", 43, exact_prompt),
        plan(true, "v1", 42),
    ] {
        assert!(matches!(
            assess_with_profiles(&changed, &hardware(), 1000, &[profile(&original)]).unwrap(),
            TaskResourceAssessment::NeedsProfiling(_)
        ));
    }
}

#[test]
fn a_candidate_clip_condition_cannot_borrow_the_measured_exact_image_budget() {
    let reference: TaskResourceMeasurementRecord =
        serde_json::from_str(include_str!("sd15-cpu-512-euler-a.json")).unwrap();
    let exact_profile = reference
        .clone()
        .into_profile(Sha256Digest::of_bytes(include_bytes!(
            "sd15-cpu-512-euler-a.json"
        )))
        .unwrap();
    let hardware = &reference.hardware;
    let exact = plan_with_prompt(
        false,
        "v1",
        42,
        "a photograph of a red apple on a wooden table, studio lighting, high quality",
    );
    assert!(matches!(
        assess_with_profiles(
            &exact,
            hardware,
            hardware.observed_at,
            std::slice::from_ref(&exact_profile)
        )
        .unwrap(),
        TaskResourceAssessment::Admitted(_)
    ));
    let first = plan_with_prompt(false, "v1", 43, "A blue sailboat.");
    let other = plan_with_prompt(false, "v1", u32::MAX, "木桌上的苹果");
    assert_eq!(
        TaskResourceKey::from_plan(&first).unwrap(),
        TaskResourceKey::from_plan(&other).unwrap()
    );
    assert_ne!(
        first.snapshot_digest().unwrap(),
        other.snapshot_digest().unwrap()
    );
    for changed in [first, other] {
        assert!(matches!(
            assess_with_profiles(
                &changed,
                hardware,
                hardware.observed_at,
                std::slice::from_ref(&exact_profile)
            )
            .unwrap(),
            TaskResourceAssessment::NeedsProfiling(_)
        ));
    }
}

#[test]
fn asr_resource_key_ignores_session_media_identity_but_keeps_model_workload_bound() {
    let profile = AudioTaskProfile::Qwen3Asr06bCpu;
    let (sha256, size_bytes) = profile.recorded_model();
    let (spec_sha256, spec_bytes) = profile.recorded_model_spec();
    let binding = |version: &str| AudioTaskModelBinding {
        installation_id: "synthetic-asr-installation".into(),
        version_id: version.into(),
        manifest_digest: Sha256Digest::of_bytes(version.as_bytes()),
        runtime: profile.runtime_build(),
        target_capabilities: BTreeSet::from([profile.capability()]),
        model: AudioTaskAssetBinding {
            artifact_id: "model".into(),
            sha256: sha256.clone(),
            size_bytes,
        },
        model_spec: AudioTaskAssetBinding {
            artifact_id: "spec".into(),
            sha256: spec_sha256.clone(),
            size_bytes: spec_bytes,
        },
    };
    let compile = |version: &str, session: &str, result: &str| {
        Qwen3AsrParameterService::new(binding(version))
            .unwrap()
            .compile(Qwen3AsrRequest {
                session_id: session.into(),
                job_id: "asr-resource-key".into(),
                audio: AudioTaskMediaReference {
                    session_id: session.into(),
                    result_id: result.into(),
                    sha256: Sha256Digest::of_bytes(b"same-audio"),
                    mime_type: "audio/wav".into(),
                    size_bytes: 16_044,
                    sample_rate: 16_000,
                    channels: 1,
                    sample_frames: 8_000,
                },
                context: Some("same context".into()),
                language: Some(Qwen3AsrLanguage::English),
                max_tokens: Some(128),
                chunking: Some(Qwen3AsrChunking::WholeFile {}),
            })
            .unwrap()
            .into()
    };
    let first = compile(
        "asr-version-a",
        "session-a",
        "019a1111-2222-8333-8444-555555555555",
    );
    let second = compile(
        "asr-version-b",
        "session-b",
        "019a1111-2222-8333-8444-666666666666",
    );
    assert_eq!(
        TaskResourceKey::from_plan(&first).unwrap(),
        TaskResourceKey::from_plan(&second).unwrap()
    );
    assert_ne!(
        first.snapshot_digest().unwrap(),
        second.snapshot_digest().unwrap()
    );
}

fn record() -> TaskResourceMeasurementRecord {
    let plan = plan(false, "v1", 42);
    let phase = TaskProcessResourceMeasurement {
        pid: 1,
        peak_working_set_bytes: 100,
        peak_commit_bytes: 200,
        elapsed_millis: 1000,
        exit_code: 0,
        process_tree_exited: true,
        memory_samples_sha256: Sha256Digest::of_bytes(b"synthetic samples"),
        observed_output_peak_bytes: 100,
    };
    TaskResourceMeasurementRecord {
        schema_version: 2,
        rules_version: TASK_RESOURCE_RULES_VERSION.into(),
        provenance: TaskResourceMeasurementProvenance {
            runner_sha256: Sha256Digest::of_bytes(b"synthetic runner"),
            fixture_sha256: Sha256Digest::of_bytes(b"synthetic fixture"),
            execution_evidence_sha256: Sha256Digest::of_bytes(b"synthetic evidence"),
        },
        key: TaskResourceKey::from_plan(&plan).unwrap(),
        snapshot_digest: plan.snapshot_digest().unwrap(),
        hardware: hardware(),
        controller_peak_commit_bytes: 50,
        lifecycle_tree: TaskLifecycleTreeMeasurement {
            process_ids: vec![1, 2],
            peak_job_commit_bytes: 250,
            summed_peak_working_set_bytes: 200,
            samples_sha256: Sha256Digest::of_bytes(b"synthetic tree samples"),
        },
        native: phase.clone(),
        publication: Some(TaskPublicationMeasurement {
            kind: TaskResultPersistenceKind::Media,
            process: TaskProcessResourceMeasurement {
                pid: 2,
                peak_commit_bytes: 250,
                ..phase
            },
            implementation: "media::task_output::import_readers".into(),
            implementation_sources_sha256: Sha256Digest::of_bytes(b"source"),
            helper_binary_sha256: Sha256Digest::of_bytes(b"helper"),
            input_sha256: Sha256Digest::of_bytes(b"wav"),
            published_sha256: Sha256Digest::of_bytes(b"wav"),
            published_bytes: 100,
            peak_directory_bytes: 120,
            reopened_verified: true,
        }),
        gpu: TaskVramMeasurement::CpuOnly,
        native_exited_before_publication: true,
        reviewed_budget: Some(profile(&plan).budget),
    }
}

#[test]
fn measured_record_requires_actual_publication_and_accounts_for_resident_controller() {
    let mut record = record();
    assert_eq!(record.measured_host_peak_bytes().unwrap(), 300);
    record.validate_for_registration().unwrap();
    record.reviewed_budget.as_mut().unwrap().host_ram_bytes = 299;
    assert!(record.validate_for_registration().is_err());
    record = self::record();
    record.publication = None;
    assert!(record.validate_for_registration().is_err());
    record = self::record();
    record.native_exited_before_publication = false;
    assert!(record.validate_for_registration().is_err());
    record = self::record();
    record.lifecycle_tree.process_ids.pop();
    assert!(record.validate_for_registration().is_err());
    record = self::record();
    record.lifecycle_tree.summed_peak_working_set_bytes = 199;
    assert!(record.validate_for_registration().is_err());
}

#[test]
fn cuda_measurement_cannot_be_replaced_with_cpu_or_missing_vram() {
    let mut record = record();
    record.key.requires_gpu = true;
    assert!(record.validate_for_registration().is_err());
    record.gpu = TaskVramMeasurement::Missing;
    assert!(record.validate_for_registration().is_err());
}

#[test]
fn record_is_not_authority_and_unreviewed_data_cannot_be_registered() {
    let mut record = record();
    record.validate_for_registration().unwrap();
    assert!(matches!(
        assess_task_resources(&plan(false, "v1", 42), &hardware(), 1000).unwrap(),
        TaskResourceAssessment::NeedsProfiling(_)
    ));
    record.reviewed_budget = None;
    assert!(record.validate_for_registration().is_err());
}

#[test]
fn disk_arithmetic_overflow_is_rejected() {
    let mut budget = profile(&plan(false, "v1", 42)).budget;
    budget.temporary_output_bytes = u64::MAX;
    assert!(budget.peak_disk_bytes().is_err());
}

#[test]
fn watchdog_limit_covers_native_peak_before_final_file_shrinks() {
    let mut measured = record();
    measured.native.observed_output_peak_bytes = 125;
    let budget = measured.reviewed_budget.as_mut().unwrap();
    budget.temporary_output_bytes = 125;
    budget.publication_copy_bytes = 125;
    assert!(measured.validate_for_registration().is_err());
    measured.reviewed_budget.as_mut().unwrap().output_byte_limit = 125;
    measured.validate_for_registration().unwrap();
}
