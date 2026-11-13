use super::*;
use crate::lifecycle::cache_worker::InstallExecution;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

fn observation(profile: AudioTaskProfile) -> ComponentObservation {
    // Only inventory identities match the real weights. The header and disk database are fixtures.
    let (sha, size) = profile.recorded_model();
    let path = RepositoryPath::parse(profile.weight_repository_path()).unwrap();
    let inventory = parse_modelscope_inventory(&serde_json::to_vec(&json!({
        "Code":200,"Data":{"Files":[{"Path":path.as_str(),"Type":"blob","Size":size,"Sha256":sha}]}
    })).unwrap(), RepositorySnapshot {
        repository: ModelScopeRepository::parse(WEIGHTS_REPOSITORY).unwrap(),
        requested_revision: WEIGHTS_REVISION.into(), resolved_revision: WEIGHTS_REVISION.into(),
        files_digest: Sha256Digest::of_bytes(b"uninitialized test inventory"), fetched_at: 10
    }).unwrap();
    let artifact = inventory.artifact(&path, FilePurpose::GgufWeight).unwrap();
    ComponentObservation {
        role: ComponentRole::Main,
        files: vec![SourceGgufObservation {
            artifact,
            snapshot: inventory.snapshot,
            header: GgufInspection {
                version: 3,
                byte_order: GgufByteOrder::LittleEndian,
                file_bytes: size,
                header_bytes: 96,
                header_digest: Sha256Digest::of_bytes(b"unit header only"),
                data_offset: 96,
                alignment: 32,
                metadata: BTreeMap::new(),
                tensors: vec![GgufTensor {
                    name: "fixture.weight".into(),
                    dimensions: vec![1],
                    type_id: 0,
                    quantization: "F32".into(),
                    relative_offset: 0,
                    encoded_bytes: 4,
                    elements: 1,
                }],
                stored_tensor_elements: 1,
                encoded_tensor_bytes: 4,
                shard: None,
            },
        }],
    }
}

fn fixture() -> (tempfile::TempDir, LifecycleRepository) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/audio-install-tests");
    std::fs::create_dir_all(&root).unwrap();
    let temp = tempfile::tempdir_in(&root).unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    (temp, repo)
}

#[test]
fn only_the_two_exact_distribution_members_are_materializable() {
    for profile in [
        AudioTaskProfile::PocketTtsEnglishCpu,
        AudioTaskProfile::Qwen3Asr06bCpu,
    ] {
        let source = observation(profile);
        let snapshot = &source.files[0].snapshot;
        let spec = profile.runtime_spec_artifact(snapshot).unwrap();
        assert_eq!(
            Sha256Digest::of_bytes(profile.runtime_spec_bytes().unwrap()),
            spec.source_sha256.clone().unwrap()
        );
        assert!(crate::lifecycle::modelscope::pinned_artifact_url(&spec, snapshot).is_err());
        let mut wrong = spec.clone();
        wrong.purpose = FilePurpose::RuntimeConfig;
        assert!(wrong.validate().is_err());
        for path in [
            "sub/config.json",
            "model_specs/pocket_tts.py",
            "download.json",
        ] {
            let mut wrong = spec.clone();
            wrong.path = RepositoryPath::parse(path).unwrap();
            assert!(wrong.validate().is_err());
        }
        let mut modified = spec.clone();
        modified.source_sha256 = Some(Sha256Digest::of_bytes(b"{}"));
        assert!(modified.validate().is_err());
        modified = spec.clone();
        modified.size_bytes = None;
        assert!(modified.validate().is_err());
        let mut other_rev = snapshot.clone();
        other_rev.resolved_revision = "a".repeat(40);
        assert!(profile.runtime_spec_artifact(&other_rev).is_err());
        let mut other_repo = snapshot.clone();
        other_repo.repository = ModelScopeRepository::parse("other/audio").unwrap();
        assert!(profile.runtime_spec_artifact(&other_repo).is_err());
    }
}

#[test]
fn both_fixed_weight_plans_reopen_from_sqlite_without_runtime_admission() {
    let (temp, repo) = fixture();
    for profile in [
        AudioTaskProfile::PocketTtsEnglishCpu,
        AudioTaskProfile::Qwen3Asr06bCpu,
    ] {
        let observation = observation(profile);
        let plan = repo
            .prepare_audio_install_plan(&observation, profile.install_peak_bytes(), 100)
            .unwrap();
        assert!(plan.admission.is_none());
        assert_eq!(plan.runtime, profile.runtime_build());
        let mut legacy = plan.clone();
        legacy.runtime = profile.legacy_runtime_build();
        assert!(legacy.validate_for_install(100, u64::MAX).is_err());
        legacy.rules_version = LEGACY_AUDIO_INSTALL_RULES_VERSION.into();
        legacy.validate_for_install(100, u64::MAX).unwrap();
        legacy.runtime = profile.runtime_build();
        assert!(legacy.validate_for_install(100, u64::MAX).is_err());
        assert_eq!(plan.hardware, HardwareCompatibility::Unknown);
        let reopened = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
        assert_eq!(reopened.read_plan(&plan.id).unwrap(), Some(plan.clone()));
        assert!(plan
            .validate_for_install(100, profile.install_peak_bytes() - 1)
            .is_err());
        assert!(reopened.save_plan(&plan, 100, u64::MAX).is_err());
        let mut forged = plan.clone();
        forged.required_disk_bytes -= 1;
        assert!(forged.validate_for_install(100, u64::MAX).is_err());
        forged = plan.clone();
        forged.dependencies.edges[0].requirement = DependencyRequirement::Optional;
        assert!(forged.validate_for_install(100, u64::MAX).is_err());
        forged = plan.clone();
        forged.runtime.binary_sha256 = Sha256Digest::of_bytes(b"other executable");
        assert!(forged.validate_for_install(100, u64::MAX).is_err());
        forged = plan.clone();
        forged.rules_version = "diagnostic-only".into();
        forged.hardware = HardwareCompatibility::Runnable;
        assert!(forged.validate_for_install(100, u64::MAX).is_err());
        let mut wrong = observation.clone();
        wrong.files[0].artifact.path = RepositoryPath::parse("wrong.gguf").unwrap();
        assert!(repo
            .prepare_audio_install_plan(&wrong, u64::MAX, 100)
            .is_err());
    }
}

#[test]
fn cached_weight_input_cannot_supply_a_spec_and_worker_writes_real_pinned_bytes() {
    let (temp, repo) = fixture();
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let profile = AudioTaskProfile::PocketTtsEnglishCpu;
    let source = observation(profile);
    let plan = repo
        .prepare_audio_install_plan(&source, u64::MAX, 10)
        .unwrap();
    let spec = profile
        .runtime_spec_artifact(&source.files[0].snapshot)
        .unwrap();
    let weights = BTreeMap::from([(
        source.files[0].artifact.id.clone(),
        temp.path().join("weight.gguf"),
    )]);
    let cache_input = |files| {
        InstallationInput::new(
            &store,
            "installation".into(),
            "version".into(),
            InstallationSource::LocalCache { files },
        )
        .unwrap()
    };
    let mut injected = weights.clone();
    injected.insert(spec.id.clone(), temp.path().join("arbitrary.json"));
    assert!(cache_input(injected).validate_plan(&plan).is_err());
    let input = cache_input(weights);
    let job = repo
        .enqueue_installation(
            &JobAcceptance {
                id: "job".into(),
                idempotency_key: "audio-test".into(),
                plan_id: plan.id.clone(),
                plan_digest: plan.digest().unwrap(),
            },
            &input,
            11,
            u64::MAX,
        )
        .unwrap();
    repo.admit_installation_storage(&job, &store, 12).unwrap();
    let lease = repo
        .claim(&job.id, job.version, "audio-test-worker", 12, 120)
        .unwrap();
    let job = repo.read_job(&job.id).unwrap().unwrap();
    repo.advance(&lease, job.version, LifecycleStage::Downloading, 12)
        .unwrap();
    let record = repo
        .file_transfers(&job.id)
        .unwrap()
        .into_iter()
        .find(|f| f.artifact_id == spec.id)
        .unwrap();
    let execution = InstallExecution {
        repository: &repo,
        store: &store,
        lease,
        clock: || 13,
        control: Arc::new(InstallationWorkerControl::default()),
    };
    execution
        .copy_runtime_spec(&plan, &record, profile)
        .unwrap();
    let bytes = std::fs::read(
        store
            .transfer_file_path(&job.id, &record.relative_path)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(bytes.as_slice(), profile.runtime_spec_bytes().unwrap());
    assert_eq!(
        Sha256Digest::of_bytes(&bytes),
        profile.recorded_model_spec().0
    );
    let saved = repo
        .file_transfers(&job.id)
        .unwrap()
        .into_iter()
        .find(|f| f.artifact_id == spec.id)
        .unwrap();
    assert_eq!(saved.state, FileTransferState::Downloaded);
    assert_eq!(
        saved.checkpoint.completed_bytes,
        profile.recorded_model_spec().1
    );
}

#[test]
fn language_weights_share_the_distribution_record_in_either_install_order() {
    let test = "lifecycle::audio_task_parameters::install::tests::language_weights_share_the_distribution_record_in_either_install_order";
    const CHILD: &str = "LUMINA_TEST_SHARED_AUDIO_SPEC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let (tmp, _) = fixture();
        let path = tmp.path().join("catalog.json");
        let definition = json!({
            "name":"fr-test","repository":WEIGHTS_REPOSITORY,"revision":WEIGHTS_REVISION,
            "file":"french/model.gguf","sha256":"1".repeat(64),"sizeBytes":8192,
            "parameters":{"protocol":"pocket_tts_offline","language":"french","temperature":0.7}
        });
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"schemaVersion":1,"models":[definition]})).unwrap(),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, "1")
            .env("LUMINA_AUDIO_CATALOG", path)
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let english = AudioTaskProfile::PocketTtsEnglishCpu;
    let french = AudioTaskProfile::registered()
        .find(|p| p.installation_name() == "fr-test")
        .unwrap();
    // Both weight observations originate in the same immutable repository inventory.
    let rows: Vec<_> = [english, french]
        .iter()
        .map(|p| {
            let (sha, size) = p.recorded_model();
            json!({"Path":p.weight_repository_path(),"Type":"blob","Size":size,"Sha256":sha})
        })
        .collect();
    let inventory = parse_modelscope_inventory(
        &serde_json::to_vec(&json!({"Code":200,"Data":{"Files":rows}})).unwrap(),
        observation(english).files[0].snapshot.clone(),
    )
    .unwrap();
    let selected = |profile: AudioTaskProfile| {
        let mut obs = observation(profile);
        obs.files[0].snapshot = inventory.snapshot.clone();
        obs.files[0].artifact = inventory
            .artifact(
                &RepositoryPath::parse(profile.weight_repository_path()).unwrap(),
                FilePurpose::GgufWeight,
            )
            .unwrap();
        obs
    };
    for (order, legacy) in [
        ([english, french], false),
        ([french, english], false),
        ([french, english], true),
    ] {
        let (dir, repo) = fixture();
        if legacy {
            let (_dir, seed) = fixture();
            let old = seed
                .prepare_audio_install_plan(&selected(french), u64::MAX, 10)
                .unwrap();
            let mut catalog = seed.read_catalog(&old.model_id).unwrap().unwrap();
            let spec = catalog
                .artifacts
                .values_mut()
                .find(|a| matches!(a.purpose, FilePurpose::RuntimeDistributionConfig(_)))
                .unwrap();
            spec.purpose = FilePurpose::RuntimeDistributionConfig(french);
            repo.save_catalog(&catalog).unwrap();
        }
        let mut plans = Vec::new();
        for profile in order {
            let plan = repo
                .prepare_audio_install_plan(&selected(profile), u64::MAX, 10)
                .unwrap();
            assert_eq!(
                AudioTaskProfile::for_installation_assets(&plan).unwrap(),
                profile
            );
            plans.push(plan);
        }
        drop(repo);
        let reopened = LifecycleRepository::open(&dir.path().join("models.db")).unwrap();
        let shared = if legacy { french } else { english }
            .distribution_artifact(&inventory.snapshot)
            .unwrap();
        for plan in plans {
            let recovered = reopened.read_plan(&plan.id).unwrap().unwrap();
            assert_eq!(recovered, plan);
            assert_eq!(recovered.artifacts.get(&shared.id), Some(&shared));
        }
    }
}
