use super::*;
use crate::lifecycle::*;

#[cfg(windows)]
#[path = "pocket_probe.rs"]
mod pocket_probe;

#[cfg(windows)]
#[path = "sd15_probe.rs"]
mod sd15_probe;

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
