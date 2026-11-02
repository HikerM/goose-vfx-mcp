use fs2::FileExt;
use lumina_local_inference::lifecycle::*;
use serde_json::json;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

fn network_request(cache: &CacheInstallRequest) -> ModelScopeInstallRequest {
    ModelScopeInstallRequest {
        job_id: cache.job_id.clone(),
        installation_id: cache.installation_id.clone(),
        version_id: cache.version_id.clone(),
    }
}

fn network_run(
    repository: Arc<LifecycleRepository>,
    store: Arc<InstallationStore>,
    request: ModelScopeInstallRequest,
    token: Option<String>,
) -> anyhow::Result<InstalledVersion> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(install_from_modelscope(
            repository,
            store,
            request,
            token,
            "network-test-worker".into(),
            || 500,
        ))
}

#[test]
fn network_preflight_rejects_invalid_credentials_before_claiming_or_contacting_source() {
    let fixture = Fixture::new(true);
    let request = network_request(&fixture.request);
    let repository = Arc::new(fixture.repository);
    let store = Arc::new(fixture.store);
    let error = network_run(
        repository.clone(),
        store,
        request.clone(),
        Some("invalid\ntoken".into()),
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::SourceAuthRequired
    );
    let job = repository.read_job(&request.job_id).unwrap().unwrap();
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.version, 0);
    assert!(repository.active_installations().unwrap().is_empty());
}

#[test]
fn network_worker_acknowledges_cancelled_recovery_without_network_io() {
    let fixture = Fixture::new(true);
    let request = network_request(&fixture.request);
    fixture
        .repository
        .claim(&request.job_id, 0, "stopped", 32, 120)
        .unwrap();
    let job = fixture
        .repository
        .read_job(&request.job_id)
        .unwrap()
        .unwrap();
    fixture
        .repository
        .transition(&job.id, job.version, JobStatus::Cancelling, 33)
        .unwrap();
    let repository = Arc::new(fixture.repository);
    let error = network_run(
        repository.clone(),
        Arc::new(fixture.store),
        request.clone(),
        None,
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::Cancelled
    );
    assert_eq!(
        repository
            .read_job(&request.job_id)
            .unwrap()
            .unwrap()
            .status,
        JobStatus::Cancelled
    );
    assert!(repository.active_installations().unwrap().is_empty());
}

#[test]
fn network_completed_replay_uses_the_same_manifest_identity_and_ownership_checks() {
    let fixture = Fixture::new(true);
    let installed = fixture.run(40).unwrap();
    let request = network_request(&fixture.request);
    let repository = Arc::new(fixture.repository);
    let store = Arc::new(fixture.store);
    let events = repository.events(&request.job_id, 0, 100).unwrap();
    assert_eq!(
        network_run(repository.clone(), store.clone(), request.clone(), None).unwrap(),
        installed
    );
    assert_eq!(repository.events(&request.job_id, 0, 100).unwrap(), events);
    let mut wrong = request.clone();
    wrong.version_id = "other-version".into();
    assert!(network_run(repository.clone(), store, wrong, None).is_err());
    assert!(repository
        .evidence(&request.installation_id)
        .unwrap()
        .is_empty());
}

#[test]
fn network_worker_respects_the_same_transfer_writer_lock_before_claiming() {
    let fixture = Fixture::new(true);
    let request = network_request(&fixture.request);
    let path = fixture.store.root().join("locks/transfers").join(format!(
        "{}.lock",
        Sha256Digest::of_bytes(request.job_id.as_bytes()).as_str()
    ));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = std::fs::File::create(path).unwrap();
    lock.try_lock_exclusive().unwrap();
    let repository = Arc::new(fixture.repository);
    assert!(network_run(
        repository.clone(),
        Arc::new(fixture.store),
        request.clone(),
        None
    )
    .is_err());
    assert_eq!(
        repository
            .read_job(&request.job_id)
            .unwrap()
            .unwrap()
            .version,
        0
    );
}

#[test]
fn transient_file_retry_preserves_checkpoint_while_hash_mismatch_requires_restart() {
    for code in [
        LifecycleErrorCode::SourceTimeout,
        LifecycleErrorCode::HashMismatch,
    ] {
        let fixture = Fixture::new(true);
        fixture.prepare_partial(&fixture.bytes[..512]);
        let job = fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap();
        let lease = fixture
            .repository
            .claim(&job.id, job.version, "retry-worker", 40, 120)
            .unwrap();
        let file = fixture
            .repository
            .file_transfers(&job.id)
            .unwrap()
            .remove(0);
        let failed = fixture
            .repository
            .fail_file(&lease, &file.artifact_id, file.version, code, 41)
            .unwrap();
        let result =
            fixture
                .repository
                .resume_failed_file(&lease, &file.artifact_id, failed.version, 42);
        if code == LifecycleErrorCode::HashMismatch {
            assert!(result.is_err());
            assert_eq!(
                fixture
                    .repository
                    .read_file_transfer(&job.id, &file.artifact_id)
                    .unwrap()
                    .unwrap(),
                failed
            );
        } else {
            let resumed = result.unwrap();
            assert_eq!(resumed.state, FileTransferState::Downloading);
            assert_eq!(resumed.checkpoint, file.checkpoint);
        }
    }
}

fn cache_input(fixture: &Fixture) -> InstallationInput {
    InstallationInput::new(
        &fixture.store,
        fixture.request.installation_id.clone(),
        fixture.request.version_id.clone(),
        InstallationSource::LocalCache {
            files: fixture.request.source_files.clone(),
        },
    )
    .unwrap()
}

fn acceptance(fixture: &Fixture, id: &str) -> JobAcceptance {
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    JobAcceptance {
        id: id.into(),
        idempotency_key: format!("key:{id}"),
        plan_id: job.plan_id,
        plan_digest: job.plan_digest,
    }
}

#[test]
fn durable_acceptance_reopens_the_exact_inputs_and_is_idempotent_after_expiry() {
    let fixture = Fixture::new(true);
    let input = cache_input(&fixture);
    let mut request = acceptance(&fixture, "durable-job");
    let job = fixture
        .repository
        .enqueue_installation(&request, &input, 32, 1 << 30)
        .unwrap();
    let intent = fixture
        .repository
        .read_installation_input(&job.id)
        .unwrap()
        .unwrap();
    assert_eq!(intent.input, input);
    assert_eq!(intent.plan_digest, request.plan_digest);
    assert_eq!(intent.bound_at, 32);
    request.id = "duplicate-id-must-not-be-created".into();
    assert_eq!(
        fixture
            .repository
            .enqueue_installation(&request, &input, 500, 0)
            .unwrap(),
        job
    );
    assert!(fixture.repository.read_job(&request.id).unwrap().is_none());
    drop(fixture.repository);
    let reopened = LifecycleRepository::open(&fixture.temp.path().join("models.db")).unwrap();
    assert_eq!(
        reopened.read_installation_input(&job.id).unwrap(),
        Some(intent)
    );
    assert_eq!(reopened.file_transfers(&job.id).unwrap().len(), 1);
    assert_eq!(reopened.events(&job.id, 0, 100).unwrap().len(), 1);
}

#[test]
fn input_insert_failure_rolls_back_the_job_files_and_events() {
    let fixture = Fixture::new(true);
    let request = acceptance(&fixture, "failed-atomic-job");
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute_batch("CREATE TRIGGER reject_inputs BEFORE INSERT ON installation_inputs BEGIN SELECT RAISE(ABORT, 'injected input failure'); END;").unwrap();
    assert!(fixture
        .repository
        .enqueue_installation(&request, &cache_input(&fixture), 32, 1 << 30)
        .is_err());
    assert!(fixture.repository.read_job(&request.id).unwrap().is_none());
    assert!(fixture
        .repository
        .file_transfers(&request.id)
        .unwrap()
        .is_empty());
    assert!(fixture
        .repository
        .events(&request.id, 0, 100)
        .unwrap()
        .is_empty());
}

#[test]
fn immutable_inputs_reject_version_store_transport_and_cache_selection_changes() {
    let fixture = Fixture::new(true);
    let request = acceptance(&fixture, "durable-job");
    let input = cache_input(&fixture);
    let job = fixture
        .repository
        .enqueue_installation(&request, &input, 32, 1 << 30)
        .unwrap();
    let events = fixture.repository.events(&job.id, 0, 100).unwrap();
    let mut candidates = Vec::new();
    let mut changed = input.clone();
    changed.version_id = "different-version".into();
    candidates.push(changed);
    let mut changed = input.clone();
    changed.installation_id = "different-installation".into();
    candidates.push(changed);
    let mut changed = input.clone();
    changed.store_root = fixture.temp.path().join("other-store");
    candidates.push(changed);
    let mut changed = input.clone();
    changed.source = InstallationSource::ModelScope {};
    candidates.push(changed);
    let mut changed = input.clone();
    if let InstallationSource::LocalCache { files } = &mut changed.source {
        *files.values_mut().next().unwrap() = fixture.temp.path().join("different-source.gguf");
    }
    candidates.push(changed);
    for changed in candidates {
        assert!(fixture
            .repository
            .enqueue_installation(&request, &changed, 33, 1 << 30)
            .is_err());
        assert!(fixture
            .repository
            .adopt_installation_input(&job.id, &changed, 33)
            .is_err());
    }
    assert_eq!(
        fixture.repository.read_job(&job.id).unwrap(),
        Some(job.clone())
    );
    assert_eq!(fixture.repository.events(&job.id, 0, 100).unwrap(), events);
}

#[test]
fn version_reservation_prevents_two_jobs_from_publishing_the_same_version() {
    let fixture = Fixture::new(true);
    let input = cache_input(&fixture);
    fixture
        .repository
        .enqueue_installation(&acceptance(&fixture, "first"), &input, 32, 1 << 30)
        .unwrap();
    assert!(fixture
        .repository
        .enqueue_installation(&acceptance(&fixture, "second"), &input, 32, 1 << 30)
        .is_err());
    assert!(fixture.repository.read_job("second").unwrap().is_none());
}

#[test]
fn legacy_job_requires_explicit_input_adoption_before_recovery() {
    let fixture = Fixture::new(true);
    let before = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    let request = JobAcceptance {
        id: before.id.clone(),
        idempotency_key: before.idempotency_key.clone(),
        plan_id: before.plan_id.clone(),
        plan_digest: before.plan_digest.clone(),
    };
    let input = cache_input(&fixture);
    assert!(fixture
        .repository
        .enqueue_installation(&request, &input, 32, 1 << 30)
        .is_err());
    let repository = Arc::new(fixture.repository);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(runtime
        .block_on(resume_installation(
            repository.clone(),
            Arc::new(fixture.store),
            before.id.clone(),
            None,
            "legacy-recovery".into(),
            || 500
        ))
        .is_err());
    assert_eq!(
        repository.read_job(&before.id).unwrap(),
        Some(before.clone())
    );
    repository
        .adopt_installation_input(&before.id, &input, 32)
        .unwrap();
    assert_eq!(repository.read_job(&before.id).unwrap(), Some(before));
}

#[test]
fn recovery_after_database_reopen_needs_only_the_job_id_and_keeps_ready_unverified() {
    let fixture = Fixture::new(true);
    let input = cache_input(&fixture);
    let request = acceptance(&fixture, "recovery-job");
    fixture
        .repository
        .enqueue_installation(&request, &input, 32, 1 << 30)
        .unwrap();
    drop(fixture.repository);
    let repository =
        Arc::new(LifecycleRepository::open(&fixture.temp.path().join("models.db")).unwrap());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let store = Arc::new(fixture.store);
    let installed = runtime
        .block_on(resume_installation(
            repository.clone(),
            store.clone(),
            request.id.clone(),
            None,
            "recovery-worker".into(),
            || 500,
        ))
        .unwrap();
    assert_eq!(installed.manifest.installation_id, input.installation_id);
    assert_eq!(installed.manifest.version_id, input.version_id);
    let events = repository.events(&request.id, 0, 100).unwrap();
    assert_eq!(
        runtime
            .block_on(resume_installation(
                repository.clone(),
                store,
                request.id.clone(),
                None,
                "replay".into(),
                || 600
            ))
            .unwrap(),
        installed
    );
    assert_eq!(repository.events(&request.id, 0, 100).unwrap(), events);
    assert!(repository
        .evidence(&input.installation_id)
        .unwrap()
        .is_empty());
}

#[test]
fn recovery_rejects_another_store_before_claiming_or_creating_installations() {
    let fixture = Fixture::new(true);
    let request = acceptance(&fixture, "recovery-job");
    fixture
        .repository
        .enqueue_installation(&request, &cache_input(&fixture), 32, 1 << 30)
        .unwrap();
    let wrong_store =
        Arc::new(InstallationStore::open(&fixture.temp.path().join("wrong-store")).unwrap());
    let repository = Arc::new(fixture.repository);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(runtime
        .block_on(resume_installation(
            repository.clone(),
            wrong_store,
            request.id.clone(),
            None,
            "wrong-root".into(),
            || 500
        ))
        .is_err());
    assert_eq!(
        repository.read_job(&request.id).unwrap().unwrap().version,
        0
    );
    assert!(repository.active_installations().unwrap().is_empty());
}

#[test]
fn network_recovery_uses_ephemeral_credentials_and_keeps_invalid_tokens_out_of_storage() {
    let fixture = Fixture::new(true);
    let request = acceptance(&fixture, "network-recovery");
    let mut input = cache_input(&fixture);
    input.source = InstallationSource::ModelScope {};
    fixture
        .repository
        .enqueue_installation(&request, &input, 32, 1 << 30)
        .unwrap();
    let repository = Arc::new(fixture.repository);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(resume_installation(
            repository.clone(),
            Arc::new(fixture.store),
            request.id.clone(),
            Some("unit-secret\ninvalid".into()),
            "auth-recovery".into(),
            || 500,
        ))
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::SourceAuthRequired
    );
    assert_eq!(
        repository.read_job(&request.id).unwrap().unwrap().version,
        0
    );
    let intent = repository
        .read_installation_input(&request.id)
        .unwrap()
        .unwrap();
    let payload = serde_json::to_string(&intent).unwrap();
    assert!(!payload.contains("secret") && !payload.contains("token"));
    assert_eq!(intent.input, input);
}

#[test]
fn serialized_installation_inputs_reject_tokens_urls_and_unknown_fields() {
    let fixture = Fixture::new(true);
    let original = serde_json::to_value(cache_input(&fixture)).unwrap();
    for field in ["token", "url", "authorization"] {
        let mut value = original.clone();
        value[field] = "must-not-be-accepted".into();
        assert!(serde_json::from_value::<InstallationInput>(value).is_err());
        let mut value = original.clone();
        value["source"] = json!({"kind":"model_scope"});
        value["source"][field] = "must-not-be-accepted".into();
        assert!(serde_json::from_value::<InstallationInput>(value).is_err());
    }
}

#[test]
fn corrupted_input_payload_is_rejected_before_recovery_or_worker_claim() {
    let fixture = Fixture::new(true);
    let request = acceptance(&fixture, "corrupt-job");
    fixture
        .repository
        .enqueue_installation(&request, &cache_input(&fixture), 32, 1 << 30)
        .unwrap();
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute(
        "UPDATE installation_inputs SET payload=json_set(payload,'$.input.version_id','forged')",
        [],
    )
    .unwrap();
    assert!(fixture
        .repository
        .read_installation_input(&request.id)
        .is_err());
    let mut cache_request = fixture.request.clone();
    cache_request.job_id = request.id.clone();
    assert!(install_from_local_cache(
        &fixture.repository,
        &fixture.store,
        &cache_request,
        "corrupt-input",
        || 500
    )
    .is_err());
    assert_eq!(
        fixture
            .repository
            .read_job(&request.id)
            .unwrap()
            .unwrap()
            .version,
        0
    );
}

#[test]
fn input_admission_rejects_noncanonical_roots_and_unmatched_cache_artifacts_atomically() {
    let fixture = Fixture::new(true);
    for (id, mode) in [("noncanonical", 0), ("wrong-artifact", 1)] {
        let mut input = cache_input(&fixture);
        if mode == 0 {
            input.store_root = fixture.temp.path().join("managed/../managed");
        } else if let InstallationSource::LocalCache { files } = &mut input.source {
            files.insert("unplanned".into(), fixture.temp.path().join("source.gguf"));
        }
        assert!(fixture
            .repository
            .enqueue_installation(&acceptance(&fixture, id), &input, 32, 1 << 30)
            .is_err());
        assert!(fixture.repository.read_job(id).unwrap().is_none());
    }
}

#[test]
fn schema_six_upgrade_preserves_legacy_jobs_without_inventing_installation_inputs() {
    for collision in [false, true] {
        let fixture = Fixture::new(true);
        let path = fixture.temp.path().join("models.db");
        let before = fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap();
        drop(fixture.repository);
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch("DROP TABLE installation_inputs; PRAGMA user_version=6;")
            .unwrap();
        if collision {
            raw.execute_batch("CREATE TABLE installation_inputs (legacy_value TEXT);")
                .unwrap();
            assert!(LifecycleRepository::open(&path).is_err());
            assert_eq!(
                raw.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
                    .unwrap(),
                6
            );
            assert_eq!(
                raw.query_row::<u32, _, _>("SELECT count(*) FROM lifecycle_jobs", [], |r| r.get(0))
                    .unwrap(),
                1
            );
        } else {
            let reopened = LifecycleRepository::open(&path).unwrap();
            assert_eq!(reopened.read_job(&before.id).unwrap(), Some(before.clone()));
            assert!(reopened
                .read_installation_input(&before.id)
                .unwrap()
                .is_none());
            assert_eq!(
                raw.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
                    .unwrap(),
                MODEL_DATABASE_SCHEMA_VERSION
            );
        }
    }
}

#[test]
fn concurrent_connections_accept_one_durable_job_for_the_same_idempotency_key() {
    let fixture = Fixture::new(true);
    let path = fixture.temp.path().join("models.db");
    let left = LifecycleRepository::open(&path).unwrap();
    let right = LifecycleRepository::open(&path).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let input = cache_input(&fixture);
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for (repository, id) in [(left, "candidate-left"), (right, "candidate-right")] {
            let mut request = acceptance(&fixture, id);
            request.idempotency_key = "shared-install-key".into();
            let barrier = barrier.clone();
            let input = input.clone();
            handles.push(scope.spawn(move || {
                barrier.wait();
                repository
                    .enqueue_installation(&request, &input, 32, 1 << 30)
                    .unwrap()
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results[0], results[1]);
    assert_eq!(fixture.repository.list_jobs(None, 100).unwrap().len(), 2);
    assert!(fixture
        .repository
        .read_installation_input(&results[0].id)
        .unwrap()
        .is_some());
    assert_eq!(
        fixture
            .repository
            .events(&results[0].id, 0, 100)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn workers_cannot_change_a_bound_transport_or_destination_before_claiming() {
    let fixture = Fixture::new(true);
    fixture
        .repository
        .adopt_installation_input(&fixture.request.job_id, &cache_input(&fixture), 32)
        .unwrap();
    let mut wrong = fixture.request.clone();
    wrong.version_id = "other-version".into();
    assert!(install_from_local_cache(
        &fixture.repository,
        &fixture.store,
        &wrong,
        "wrong-version",
        || 500
    )
    .is_err());
    let request = network_request(&fixture.request);
    let repository = Arc::new(fixture.repository);
    let error = network_run(
        repository.clone(),
        Arc::new(fixture.store),
        request.clone(),
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("different immutable inputs"));
    assert_eq!(
        repository
            .read_job(&request.job_id)
            .unwrap()
            .unwrap()
            .version,
        0
    );
}

#[test]
fn a_registered_legacy_version_cannot_be_reserved_by_a_new_job() {
    let fixture = Fixture::new(true);
    let input = cache_input(&fixture);
    fixture.run(40).unwrap();
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute("DELETE FROM installation_inputs", []).unwrap();
    assert!(fixture
        .repository
        .enqueue_installation(&acceptance(&fixture, "collision"), &input, 41, 1 << 30)
        .is_err());
    assert!(fixture.repository.read_job("collision").unwrap().is_none());
    assert!(fixture
        .repository
        .installed_version(&fixture.request.version_id)
        .unwrap()
        .is_some());
}

#[test]
fn bounded_recovery_pages_resume_bound_jobs_and_preserve_paused_live_and_legacy_jobs() {
    let fixture = Fixture::new(true);
    for id in [
        "a-bound",
        "b-other",
        "c-corrupt",
        "d-paused",
        "e-cancelled",
        "f-live",
        "g-cancelling",
    ] {
        let mut input = cache_input(&fixture);
        input.version_id = format!("version:{id}");
        if id == "b-other" {
            input.store_root = InstallationStore::open(&fixture.temp.path().join("other-store"))
                .unwrap()
                .root()
                .to_path_buf();
        }
        let job = fixture
            .repository
            .enqueue_installation(&acceptance(&fixture, id), &input, 32, 1 << 30)
            .unwrap();
        if id == "e-cancelled" {
            fixture
                .repository
                .transition(id, job.version, JobStatus::Cancelled, 33)
                .unwrap();
        }
        if matches!(id, "d-paused" | "f-live" | "g-cancelling") {
            fixture
                .repository
                .claim(id, job.version, "old-worker", 33, 1000)
                .unwrap();
            let job = fixture.repository.read_job(id).unwrap().unwrap();
            if id != "f-live" {
                fixture
                    .repository
                    .transition(
                        id,
                        job.version,
                        if id == "d-paused" {
                            JobStatus::Paused
                        } else {
                            JobStatus::Cancelling
                        },
                        34,
                    )
                    .unwrap();
            }
        }
    }
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute(
        "UPDATE installation_inputs SET digest='invalid' WHERE job_id='c-corrupt'",
        [],
    )
    .unwrap();
    drop(raw);
    let legacy_id = fixture.request.job_id.clone();
    let repository = Arc::new(fixture.repository);
    let store = Arc::new(fixture.store);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let credentials: InstallationCredentials =
        Arc::new(|| panic!("Cache and skipped recovery jobs must not request credentials"));
    let mut after_id = None;
    let mut outcomes = Vec::new();
    loop {
        let page = runtime
            .block_on(recover_installations_page(
                repository.clone(),
                store.clone(),
                RecoveryPageRequest {
                    after_id,
                    limit: 2,
                    worker_name: "paged-worker".into(),
                },
                credentials.clone(),
                || 500,
            ))
            .unwrap();
        outcomes.extend(page.outcomes);
        match page.next_after_id {
            Some(cursor) => after_id = Some(cursor),
            None => break,
        }
    }
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| outcome.job_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "a-bound",
            "b-other",
            "c-corrupt",
            "g-cancelling",
            legacy_id.as_str()
        ]
    );
    assert_eq!(
        outcomes[0].disposition,
        RecoveryDisposition::Completed {
            version_id: "version:a-bound".into()
        }
    );
    assert_eq!(outcomes[1].disposition, RecoveryDisposition::DifferentStore);
    assert_eq!(outcomes[2].disposition, RecoveryDisposition::InvalidInput);
    assert_eq!(outcomes[3].disposition, RecoveryDisposition::Cancelled);
    assert_eq!(
        outcomes[4].disposition,
        RecoveryDisposition::NeedsInputAdoption
    );
    for (id, status) in [
        ("d-paused", JobStatus::Paused),
        ("e-cancelled", JobStatus::Cancelled),
        ("f-live", JobStatus::Running),
    ] {
        assert_eq!(repository.read_job(id).unwrap().unwrap().status, status);
    }
    assert!(repository
        .read_installation_input(&legacy_id)
        .unwrap()
        .is_none());
}

#[test]
fn recovery_credential_provider_failures_leave_the_durable_job_queued() {
    let fixture = Fixture::new(true);
    let request = acceptance(&fixture, "network-job");
    let mut input = cache_input(&fixture);
    input.source = InstallationSource::ModelScope {};
    fixture
        .repository
        .enqueue_installation(&request, &input, 32, 1 << 30)
        .unwrap();
    let repository = Arc::new(fixture.repository);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let page = runtime
        .block_on(recover_installations_page(
            repository.clone(),
            Arc::new(fixture.store),
            RecoveryPageRequest {
                after_id: None,
                limit: 100,
                worker_name: "credentials-worker".into(),
            },
            Arc::new(|| anyhow::bail!("Unit credential provider unavailable")),
            || 500,
        ))
        .unwrap();
    assert_eq!(page.outcomes[0].job_id, request.id);
    assert_eq!(
        page.outcomes[0].disposition,
        RecoveryDisposition::CredentialsUnavailable
    );
    assert_eq!(
        repository.read_job(&request.id).unwrap().unwrap().version,
        0
    );
}

fn installing_fixture(fixture: &Fixture) -> (WorkerLease, ModelManifest, PathBuf) {
    let repository = &fixture.repository;
    repository
        .adopt_installation_input(&fixture.request.job_id, &cache_input(fixture), 32)
        .unwrap();
    let lease = repository
        .claim(
            &fixture.request.job_id,
            0,
            "direct-install-worker",
            33,
            1200,
        )
        .unwrap();
    repository
        .advance(&lease, 1, LifecycleStage::Downloading, 34)
        .unwrap();
    let file = repository
        .file_transfers(&fixture.request.job_id)
        .unwrap()
        .remove(0);
    let path = fixture.transfer_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &fixture.bytes).unwrap();
    let downloaded = repository
        .checkpoint_file(
            &lease,
            &file.artifact_id,
            file.version,
            TransferCheckpoint {
                completed_bytes: fixture.bytes.len() as u64,
                prefix_sha256: Sha256Digest::of_bytes(&fixture.bytes),
                resume_validator: ResumeValidator::Unknown,
            },
            35,
        )
        .unwrap();
    let job = repository.read_job(&lease.job_id).unwrap().unwrap();
    repository
        .advance(&lease, job.version, LifecycleStage::Verifying, 36)
        .unwrap();
    let plan = repository.read_plan(&job.plan_id).unwrap().unwrap();
    let transfer_root = path.parent().unwrap().to_path_buf();
    let verified = verify_artifact_file(
        &plan,
        &file.artifact_id,
        &transfer_root,
        &file.relative_path,
    )
    .unwrap();
    repository
        .record_verified_file(&lease, &file.artifact_id, downloaded.version, &verified, 37)
        .unwrap();
    let job = repository.read_job(&lease.job_id).unwrap().unwrap();
    repository
        .advance(&lease, job.version, LifecycleStage::Installing, 38)
        .unwrap();
    let manifest = build_installation_manifest(
        fixture.request.installation_id.clone(),
        fixture.request.version_id.clone(),
        plan,
        &repository.file_transfers(&lease.job_id).unwrap(),
        job.accepted_at,
        40,
    )
    .unwrap();
    (lease, manifest, transfer_root)
}

#[test]
fn direct_repository_commits_cannot_override_the_durable_destination() {
    let fixture = Fixture::new(true);
    let (lease, manifest, _) = installing_fixture(&fixture);
    let job = fixture.repository.read_job(&lease.job_id).unwrap().unwrap();
    for change_version in [false, true] {
        let mut wrong = manifest.clone();
        if change_version {
            wrong.version_id = "unexpected-version".into();
        } else {
            wrong.installation_id = "unexpected-installation".into();
        }
        assert!(fixture
            .repository
            .begin_installation_commit(&lease, job.version, &wrong, 40)
            .is_err());
        assert!(fixture
            .repository
            .installation_commit(&lease.job_id)
            .unwrap()
            .is_none());
        assert_eq!(
            fixture.repository.read_job(&lease.job_id).unwrap(),
            Some(job.clone())
        );
    }
}

#[test]
fn prepare_commit_and_registered_replay_reject_another_store_before_writing_files() {
    let fixture = Fixture::new(true);
    let (lease, manifest, transfer_root) = installing_fixture(&fixture);
    let wrong_store = InstallationStore::open(&fixture.temp.path().join("wrong-store")).unwrap();
    let job = fixture.repository.read_job(&lease.job_id).unwrap().unwrap();
    assert!(wrong_store
        .prepare(
            &fixture.repository,
            &lease,
            job.version,
            &manifest,
            &transfer_root,
            || 40
        )
        .is_err());
    assert!(fixture
        .repository
        .installation_commit(&lease.job_id)
        .unwrap()
        .is_none());
    assert_eq!(std::fs::read_dir(wrong_store.root()).unwrap().count(), 0);
    fixture
        .store
        .prepare(
            &fixture.repository,
            &lease,
            job.version,
            &manifest,
            &transfer_root,
            || 40,
        )
        .unwrap();
    let journal = fixture
        .repository
        .installation_commit(&lease.job_id)
        .unwrap()
        .unwrap();
    assert!(wrong_store
        .commit(&fixture.repository, &lease, || 40)
        .is_err());
    assert_eq!(
        fixture
            .repository
            .installation_commit(&lease.job_id)
            .unwrap(),
        Some(journal)
    );
    assert_eq!(std::fs::read_dir(wrong_store.root()).unwrap().count(), 0);
    fixture
        .store
        .commit(&fixture.repository, &lease, || 40)
        .unwrap();
    assert!(wrong_store
        .commit(&fixture.repository, &lease, || 40)
        .is_err());
    assert_eq!(std::fs::read_dir(wrong_store.root()).unwrap().count(), 0);
}

#[test]
fn repository_rejects_a_valid_file_witness_from_another_store() {
    let fixture = Fixture::new(true);
    let (lease, manifest, transfer_root) = installing_fixture(&fixture);
    let job = fixture.repository.read_job(&lease.job_id).unwrap().unwrap();
    fixture
        .store
        .prepare(
            &fixture.repository,
            &lease,
            job.version,
            &manifest,
            &transfer_root,
            || 40,
        )
        .unwrap();
    let wrong_store = InstallationStore::open(&fixture.temp.path().join("foreign-store")).unwrap();
    let directory = wrong_store
        .root()
        .join(installation_directory(&manifest, false).unwrap().as_str());
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    for file in manifest.installed_files.values() {
        let path = directory.join(file.relative_path.as_str());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &fixture.bytes).unwrap();
    }
    let witness = verify_installation_directory(wrong_store.root(), &manifest, false).unwrap();
    let job = fixture.repository.read_job(&lease.job_id).unwrap().unwrap();
    let journal = fixture
        .repository
        .installation_commit(&lease.job_id)
        .unwrap()
        .unwrap();
    assert!(fixture
        .repository
        .advance_installation_commit(
            &lease,
            job.version,
            &witness,
            InstallationCommitStage::Prepared,
            40
        )
        .is_err());
    assert_eq!(
        fixture
            .repository
            .installation_commit(&lease.job_id)
            .unwrap(),
        Some(journal)
    );
    assert_eq!(
        fixture.repository.read_job(&lease.job_id).unwrap(),
        Some(job)
    );
}
