use fs2::FileExt;
use lumina_local_inference::lifecycle::*;
use serde_json::json;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};

struct Fixture {
    temp: tempfile::TempDir,
    repository: LifecycleRepository,
    store: InstallationStore,
    request: CacheInstallRequest,
    bytes: Vec<u8>,
}

fn gguf_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend((value.len() as u64).to_le_bytes());
    bytes.extend(value.as_bytes());
}

fn unit_weight() -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(6u64.to_le_bytes());
    gguf_string(&mut bytes, "general.architecture");
    bytes.extend(8u32.to_le_bytes());
    gguf_string(&mut bytes, "qwen3");
    for (key, value) in [
        ("block_count", 2u32),
        ("embedding_length", 4),
        ("attention.head_count", 2),
        ("attention.head_count_kv", 1),
        ("context_length", 512),
    ] {
        gguf_string(&mut bytes, &format!("qwen3.{key}"));
        bytes.extend(4u32.to_le_bytes());
        bytes.extend(value.to_le_bytes());
    }
    gguf_string(&mut bytes, "unit.weight");
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.resize(1024, 0);
    bytes
}

impl Fixture {
    fn new(source_checksum: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repository = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
        let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
        let bytes = unit_weight();
        let source_path = temp.path().join("original.gguf");
        std::fs::write(&source_path, &bytes).unwrap();
        let inventory = parse_modelscope_inventory(
            &serde_json::to_vec(&json!({"Code":200,"Data":{"Files":[{
                "Path":"unit.gguf", "Type":"blob", "Size":bytes.len(),
                "Sha256":source_checksum.then(|| Sha256Digest::of_bytes(&bytes))
            }]}}))
            .unwrap(),
            RepositorySnapshot {
                repository: ModelScopeRepository::parse("unit/cache-install").unwrap(),
                requested_revision: "master".into(),
                resolved_revision: "a".repeat(40),
                files_digest: Sha256Digest::of_bytes(b"placeholder"),
                fetched_at: 10,
            },
        )
        .unwrap();
        let artifact = inventory
            .artifact(
                &RepositoryPath::parse("unit.gguf").unwrap(),
                FilePurpose::GgufWeight,
            )
            .unwrap();
        let observation = SourceGgufObservation {
            artifact: artifact.clone(),
            snapshot: inventory.snapshot.clone(),
            header: inspect_gguf(
                Cursor::new(&bytes),
                bytes.len() as u64,
                GgufReadLimits::default(),
            )
            .unwrap(),
        };
        let component = Component {
            id: "unit-main".into(),
            role: ComponentRole::Main,
            artifact_ids: BTreeSet::from([artifact.id.clone()]),
            compatibility: ComponentCompatibility::Verified,
            evidence_ids: BTreeSet::from(["unit-only-format-fixture".into()]),
        };
        let catalog = ModelCatalogSnapshot {
            model: ModelDefinition {
                id: "unit-model".into(),
                display_name: "Unit cache fixture".into(),
                snapshot_ids: BTreeSet::from([artifact.snapshot_id.clone()]),
                architecture: ArchitectureKnowledge::Unknown,
                structure: ModelStructure::Unknown,
                task_families: BTreeSet::from([ModelTaskFamily::Language]),
                declared_capabilities: BTreeSet::from([ModelCapability::Text]),
            },
            snapshots: BTreeMap::from([(artifact.snapshot_id.clone(), inventory.snapshot)]),
            artifacts: BTreeMap::from([(artifact.id.clone(), artifact.clone())]),
            variants: BTreeMap::from([(
                "unit-variant".into(),
                ModelVariant {
                    id: "unit-variant".into(),
                    model_id: "unit-model".into(),
                    artifact_ids: BTreeSet::from([artifact.id.clone()]),
                    quantization_label: "unit F32".into(),
                    tensor_types: BTreeSet::from([0]),
                    evidence_ids: BTreeSet::from(["unit-fixture".into()]),
                },
            )]),
            components: BTreeMap::from([(component.id.clone(), component.clone())]),
        };
        repository.save_catalog(&catalog).unwrap();
        repository.save_gguf_observation(&observation, 20).unwrap();
        let observed = ComponentObservation {
            role: ComponentRole::Main,
            files: vec![observation],
        };
        let runtime = baseline_pairing_runtime().unwrap();
        let envelope = AdapterResourceEnvelope {
            runtime: runtime.clone(),
            component_digests: BTreeSet::from([observed.digest().unwrap()]),
            workload: ResourceWorkload {
                context_tokens: 256,
                batch_tokens: 32,
                parallel_sequences: 1,
                recurrent_rollback_slots: 0,
                kv_key_type: ResourceCacheType::F16,
                kv_value_type: ResourceCacheType::F16,
                prompt_cache_bytes: 0,
                context_checkpoint_slots: 0,
            },
            compute_buffer_bytes: 1024,
            auxiliary_buffer_bytes: 1024,
            evidence_id: "unit-only-envelope".into(),
        };
        let profile = RuntimeInstallProfile {
            id: "unit-profile".into(),
            rules_version: RUNTIME_INSTALL_PROFILE_VERSION.into(),
            runtime,
            architectures: BTreeSet::from(["qwen3".into()]),
            capabilities: BTreeSet::from([ModelCapability::Text]),
            required_roles: BTreeSet::from([ComponentRole::Main]),
            evidence_ids: BTreeSet::from(["unit-profile".into()]),
        };
        let hardware = HardwareResourceSnapshot {
            observed_at: 10,
            devices: vec![HardwareMemoryDevice {
                index: 0,
                backend: "unit".into(),
                name: "unit".into(),
                description: "unit".into(),
                kind: "cpu".into(),
                total_bytes: 1 << 30,
                free_bytes: 1 << 30,
            }],
            storage_volume: "unit".into(),
            disk_free_bytes: 1 << 30,
        };
        let assessment = repository
            .prepare_observed_plan(
                &ObservedPlanRequest {
                    id: "unit-plan".into(),
                    model_id: "unit-model".into(),
                    variant_id: "unit-variant".into(),
                    target_capabilities: BTreeSet::from([ModelCapability::Text]),
                    dependencies: DependencyGraph {
                        root: component.id.clone(),
                        components: BTreeMap::from([(component.id.clone(), component)]),
                        edges: vec![],
                    },
                },
                &profile,
                &envelope,
                &hardware,
                30,
            )
            .unwrap();
        assert!(
            assessment.blocking_reasons.is_empty(),
            "{:?}",
            assessment.blocking_reasons
        );
        let plan = assessment.plan.unwrap();
        repository
            .enqueue(
                "unit-job",
                "unit-install",
                &plan.id,
                &plan.digest().unwrap(),
                31,
                1 << 30,
            )
            .unwrap();
        let request = CacheInstallRequest {
            job_id: "unit-job".into(),
            installation_id: "unit-installation".into(),
            version_id: "unit-version".into(),
            source_files: BTreeMap::from([(artifact.id, source_path)]),
        };
        Self {
            temp,
            repository,
            store,
            request,
            bytes,
        }
    }

    fn run(&self, now: i64) -> anyhow::Result<InstalledVersion> {
        install_from_local_cache(
            &self.repository,
            &self.store,
            &self.request,
            "unit-worker",
            || now,
        )
    }

    fn transfer_path(&self) -> PathBuf {
        let record = self
            .repository
            .file_transfers(&self.request.job_id)
            .unwrap()
            .remove(0);
        self.store
            .root()
            .join("transfers")
            .join(Sha256Digest::of_bytes(self.request.job_id.as_bytes()).as_str())
            .join(record.relative_path.as_str())
    }

    fn prepare_partial(&self, bytes: &[u8]) {
        let lease = self
            .repository
            .claim(&self.request.job_id, 0, "paused-worker", 32, 120)
            .unwrap();
        self.repository
            .advance(&lease, 1, LifecycleStage::Downloading, 33)
            .unwrap();
        let target = self.transfer_path();
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
        self.repository
            .checkpoint_file(
                &lease,
                self.request.source_files.keys().next().unwrap(),
                0,
                TransferCheckpoint {
                    completed_bytes: bytes.len() as u64,
                    prefix_sha256: Sha256Digest::of_bytes(bytes),
                    resume_validator: ResumeValidator::Unknown,
                },
                34,
            )
            .unwrap();
        let job = self
            .repository
            .read_job(&self.request.job_id)
            .unwrap()
            .unwrap();
        let paused = self
            .repository
            .transition(&job.id, job.version, JobStatus::Paused, 35)
            .unwrap();
        self.repository
            .transition(&job.id, paused.version, JobStatus::Queued, 36)
            .unwrap();
    }
}

#[test]
fn cache_install_publishes_verified_content_without_changing_original_or_claiming_ready() {
    let fixture = Fixture::new(true);
    let installed = fixture.run(500).unwrap();
    assert_eq!(
        fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap()
            .status,
        JobStatus::Completed
    );
    assert_eq!(fixture.repository.active_installations().unwrap().len(), 1);
    assert!(fixture
        .repository
        .evidence(&fixture.request.installation_id)
        .unwrap()
        .is_empty());
    assert_eq!(
        std::fs::read(fixture.request.source_files.values().next().unwrap()).unwrap(),
        fixture.bytes
    );
    assert!(std::fs::metadata(
        fixture
            .store
            .root()
            .join(
                installation_directory(&installed.manifest, true)
                    .unwrap()
                    .as_str()
            )
            .join(
                installed
                    .manifest
                    .installed_files
                    .values()
                    .next()
                    .unwrap()
                    .relative_path
                    .as_str()
            )
    )
    .unwrap()
    .permissions()
    .readonly());
    let events = fixture
        .repository
        .events(&fixture.request.job_id, 0, 100)
        .unwrap();
    assert_eq!(fixture.run(1000).unwrap(), installed);
    assert_eq!(
        fixture
            .repository
            .events(&fixture.request.job_id, 0, 100)
            .unwrap(),
        events
    );
    assert_eq!(
        fixture
            .repository
            .blob_reference_count(&Sha256Digest::of_bytes(&fixture.bytes))
            .unwrap(),
        1
    );
}

#[test]
fn cache_install_rejects_same_size_wrong_content_and_retains_failed_file_code() {
    let fixture = Fixture::new(true);
    let mut wrong = fixture.bytes.clone();
    wrong[1000] = 1;
    std::fs::write(fixture.request.source_files.values().next().unwrap(), wrong).unwrap();
    let error = fixture.run(40).unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::HashMismatch
    );
    assert_eq!(
        fixture
            .repository
            .file_transfers(&fixture.request.job_id)
            .unwrap()[0]
            .state,
        FileTransferState::Failed {
            code: LifecycleErrorCode::HashMismatch
        }
    );
    assert!(fixture
        .repository
        .active_installations()
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap()
            .status,
        JobStatus::Failed
    );
    std::fs::write(
        fixture.request.source_files.values().next().unwrap(),
        &fixture.bytes,
    )
    .unwrap();
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    fixture
        .repository
        .transition(&job.id, job.version, JobStatus::Queued, 41)
        .unwrap();
    fixture.run(42).unwrap();
}

#[test]
fn cache_import_without_pinned_hash_or_exact_source_selection_does_not_claim_job() {
    let mut fixture = Fixture::new(false);
    assert!(fixture.run(40).is_err());
    assert_eq!(
        fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap()
            .version,
        0
    );
    fixture.request.source_files.clear();
    assert!(fixture.run(40).is_err());
    assert_eq!(
        fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap()
            .version,
        0
    );
}

#[test]
fn cache_resume_verifies_saved_prefix_and_explicitly_restarts_corrupt_checkpoint() {
    for corrupt in [false, true] {
        let fixture = Fixture::new(true);
        fixture.prepare_partial(&fixture.bytes[..512]);
        if corrupt {
            std::fs::write(fixture.transfer_path(), vec![1; 512]).unwrap();
        }
        fixture.run(500).unwrap();
        let record = fixture
            .repository
            .file_transfers(&fixture.request.job_id)
            .unwrap()
            .remove(0);
        assert_eq!(
            record.restart_reason,
            corrupt.then_some(FileRestartReason::PartialContentMismatch)
        );
        assert_eq!(
            std::fs::read(fixture.transfer_path()).unwrap(),
            fixture.bytes
        );
    }
}

#[test]
fn pause_after_download_preserves_completed_bytes_and_resumes_without_external_source() {
    let fixture = Fixture::new(true);
    let paused = Cell::new(false);
    assert!(install_from_local_cache(
        &fixture.repository,
        &fixture.store,
        &fixture.request,
        "first",
        || {
            let job = fixture
                .repository
                .read_job(&fixture.request.job_id)
                .unwrap()
                .unwrap();
            if !paused.get()
                && job.status == JobStatus::Running
                && job.stage == LifecycleStage::Downloading
                && fixture
                    .repository
                    .file_transfers(&job.id)
                    .unwrap()
                    .iter()
                    .all(|file| file.state == FileTransferState::Downloaded)
            {
                fixture
                    .repository
                    .transition(&job.id, job.version, JobStatus::Paused, 40)
                    .unwrap();
                paused.set(true);
            }
            40
        }
    )
    .is_err());
    assert!(paused.get());
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, JobStatus::Paused);
    std::fs::remove_file(fixture.request.source_files.values().next().unwrap()).unwrap();
    fixture
        .repository
        .transition(&job.id, job.version, JobStatus::Queued, 41)
        .unwrap();
    fixture.run(500).unwrap();
}

#[test]
fn cancellation_fences_worker_and_keeps_user_source() {
    let fixture = Fixture::new(true);
    let cancelled = Cell::new(false);
    assert!(install_from_local_cache(
        &fixture.repository,
        &fixture.store,
        &fixture.request,
        "worker",
        || {
            let job = fixture
                .repository
                .read_job(&fixture.request.job_id)
                .unwrap()
                .unwrap();
            if !cancelled.get()
                && job.status == JobStatus::Running
                && job.stage == LifecycleStage::Downloading
            {
                fixture
                    .repository
                    .transition(&job.id, job.version, JobStatus::Cancelling, 40)
                    .unwrap();
                cancelled.set(true);
            }
            40
        }
    )
    .is_err());
    assert!(cancelled.get());
    assert_eq!(
        fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap()
            .status,
        JobStatus::Cancelled
    );
    assert!(fixture
        .repository
        .active_installations()
        .unwrap()
        .is_empty());
    assert!(fixture
        .request
        .source_files
        .values()
        .next()
        .unwrap()
        .exists());
}

#[test]
fn restart_acknowledges_pending_cancellation_without_importing_content() {
    let fixture = Fixture::new(true);
    fixture
        .repository
        .claim(&fixture.request.job_id, 0, "stopped-worker", 32, 120)
        .unwrap();
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    fixture
        .repository
        .transition(&job.id, job.version, JobStatus::Cancelling, 33)
        .unwrap();
    assert_eq!(
        fixture.repository.recoverable_jobs(500, None, 100).unwrap()[0].status,
        JobStatus::Cancelling
    );
    assert_eq!(
        fixture
            .run(500)
            .unwrap_err()
            .downcast_ref::<ModelSourceError>()
            .unwrap()
            .code,
        LifecycleErrorCode::Cancelled
    );
    assert_eq!(
        fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap()
            .status,
        JobStatus::Cancelled
    );
    assert!(fixture
        .repository
        .recoverable_jobs(501, None, 100)
        .unwrap()
        .is_empty());
    assert!(!fixture.transfer_path().exists());
    assert!(fixture
        .request
        .source_files
        .values()
        .next()
        .unwrap()
        .exists());
}

#[test]
fn cache_transfer_rejects_hardlink_to_external_content_without_changing_it() {
    let fixture = Fixture::new(true);
    fixture.prepare_partial(&[]);
    let external = fixture.temp.path().join("external-user-file.bin");
    let original = b"external content must stay unchanged";
    std::fs::write(&external, original).unwrap();
    let target = fixture.transfer_path();
    std::fs::remove_file(&target).unwrap();
    std::fs::hard_link(&external, &target).unwrap();
    let result = fixture.run(500);
    assert_eq!(std::fs::read(&external).unwrap(), original);
    assert!(result.is_err());
    assert!(fixture
        .repository
        .active_installations()
        .unwrap()
        .is_empty());
}

#[test]
fn cache_install_rejects_external_blob_alias_without_changing_its_permissions() {
    let fixture = Fixture::new(true);
    let external = fixture.temp.path().join("external-cached-model.gguf");
    std::fs::write(&external, &fixture.bytes).unwrap();
    let blob = fixture.store.root().join(
        InstallationStore::blob_path(&Sha256Digest::of_bytes(&fixture.bytes))
            .unwrap()
            .as_str(),
    );
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::hard_link(&external, &blob).unwrap();
    let result = fixture.run(500);
    let became_read_only = std::fs::metadata(&external)
        .unwrap()
        .permissions()
        .readonly();
    if became_read_only {
        allow_test_write(&external);
    }
    assert!(
        !became_read_only,
        "Installation changed permissions through an external filesystem alias"
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(external).unwrap(), fixture.bytes);
    assert!(fixture
        .repository
        .active_installations()
        .unwrap()
        .is_empty());
}

#[test]
fn transfer_lock_prevents_expired_worker_reclaim_from_writing_or_failing_job() {
    let fixture = Fixture::new(true);
    fixture
        .repository
        .claim(&fixture.request.job_id, 0, "expired", 32, 1)
        .unwrap();
    let path = fixture.store.root().join("locks/transfers").join(format!(
        "{}.lock",
        Sha256Digest::of_bytes(fixture.request.job_id.as_bytes()).as_str()
    ));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = std::fs::File::create(path).unwrap();
    lock.try_lock_exclusive().unwrap();
    let before = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    assert!(fixture.run(500).is_err());
    assert_eq!(
        fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap(),
        before
    );
    drop(lock);
    fixture.run(500).unwrap();
}

#[cfg_attr(
    windows,
    allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows clears FILE_ATTRIBUTE_READONLY; Unix uses explicit owner permissions"
    )
)]
fn allow_test_write(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[cfg(windows)]
    permissions.set_readonly(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    std::fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn completed_replay_rejects_different_version_and_rechecks_installed_files() {
    let mut fixture = Fixture::new(true);
    let installed = fixture.run(40).unwrap();
    fixture.request.version_id = "other-version".into();
    assert!(fixture.run(41).is_err());
    fixture.request.version_id = installed.manifest.version_id.clone();
    let relative = installed
        .manifest
        .installed_files
        .values()
        .next()
        .unwrap()
        .relative_path
        .as_str();
    let path = fixture
        .store
        .root()
        .join(
            installation_directory(&installed.manifest, true)
                .unwrap()
                .as_str(),
        )
        .join(relative);
    allow_test_write(&path);
    let mut wrong = fixture.bytes.clone();
    wrong[1000] = 1;
    std::fs::write(path, wrong).unwrap();
    assert!(fixture.run(42).is_err());
    assert!(fixture.temp.path().exists());
}

#[test]
fn durable_job_pages_find_queued_and_expired_workers_without_resuming_paused_or_cancelled_jobs() {
    let fixture = Fixture::new(true);
    let repository = &fixture.repository;
    let plan = repository.read_plan("unit-plan").unwrap().unwrap();
    for id in ["a-expired", "b-live", "c-paused", "d-cancelled", "e-failed"] {
        let job = repository
            .enqueue(id, id, &plan.id, &plan.digest().unwrap(), 31, 1 << 30)
            .unwrap();
        if id == "d-cancelled" {
            repository
                .transition(id, job.version, JobStatus::Cancelled, 32)
                .unwrap();
        } else {
            repository
                .claim(
                    id,
                    job.version,
                    "old-worker",
                    32,
                    if id == "a-expired" { 1 } else { 1000 },
                )
                .unwrap();
            if id == "c-paused" || id == "e-failed" {
                let job = repository.read_job(id).unwrap().unwrap();
                repository
                    .transition(
                        id,
                        job.version,
                        if id == "c-paused" {
                            JobStatus::Paused
                        } else {
                            JobStatus::Failed
                        },
                        33,
                    )
                    .unwrap();
            }
        }
    }
    let reopened = LifecycleRepository::open(&fixture.temp.path().join("models.db")).unwrap();
    let mut cursor = None;
    let mut ids = Vec::new();
    loop {
        let page = reopened.list_jobs(cursor.as_deref(), 2).unwrap();
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|job| job.id.clone());
        ids.extend(page.into_iter().map(|job| job.id));
    }
    assert_eq!(
        ids,
        [
            "a-expired",
            "b-live",
            "c-paused",
            "d-cancelled",
            "e-failed",
            "unit-job"
        ]
    );
    assert_eq!(
        reopened
            .recoverable_jobs(500, None, 100)
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect::<Vec<_>>(),
        ["a-expired", "unit-job"]
    );
    assert_eq!(
        reopened
            .recoverable_jobs(32, None, 100)
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect::<Vec<_>>(),
        ["unit-job"]
    );
    assert_eq!(
        reopened
            .recoverable_jobs(500, Some("a-expired"), 1)
            .unwrap()[0]
            .id,
        "unit-job"
    );
    assert!(reopened.list_jobs(None, 0).is_err());
    assert!(reopened.recoverable_jobs(500, None, 1001).is_err());
}
