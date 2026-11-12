use super::*;
use futures::channel::oneshot;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Write};
use std::rc::Rc;

struct TransferFixture {
    _temp: tempfile::TempDir,
    repository: LifecycleRepository,
    store: InstallationStore,
    lease: WorkerLease,
    sources: BTreeMap<String, Vec<u8>>,
}

fn gguf_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend((value.len() as u64).to_le_bytes());
    bytes.extend(value.as_bytes());
}

fn shard_bytes(index: u32, count: u32) -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(9u64.to_le_bytes());
    gguf_string(&mut bytes, "general.architecture");
    bytes.extend(8u32.to_le_bytes());
    gguf_string(&mut bytes, "qwen3");
    for (key, value) in [
        ("qwen3.block_count", 2),
        ("qwen3.embedding_length", 4),
        ("qwen3.attention.head_count", 2),
        ("qwen3.attention.head_count_kv", 1),
        ("qwen3.context_length", 512),
        ("split.no", index),
        ("split.count", count),
        ("split.tensors.count", count),
    ] {
        gguf_string(&mut bytes, key);
        bytes.extend(4u32.to_le_bytes());
        bytes.extend(value.to_le_bytes());
    }
    gguf_string(&mut bytes, &format!("unit.{index}.weight"));
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.resize(1024, 0);
    bytes
}

impl TransferFixture {
    fn new(count: u32) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
        let repository = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
        let files: Vec<_> = (0..count)
            .map(|index| (format!("part-{index}.gguf"), shard_bytes(index, count)))
            .collect();
        let inventory = parse_modelscope_inventory(
            &serde_json::to_vec(&serde_json::json!({
                "Code": 200,
                "Data": {"Files": files.iter().map(|(path, bytes)| serde_json::json!({
                    "Path": path, "Type": "blob", "Size": bytes.len(),
                    "Sha256": Sha256Digest::of_bytes(bytes)
                })).collect::<Vec<_>>()}
            }))
            .unwrap(),
            RepositorySnapshot {
                repository: ModelScopeRepository::parse("unit/parallel-files").unwrap(),
                requested_revision: "master".into(),
                resolved_revision: "a".repeat(40),
                files_digest: Sha256Digest::of_bytes(b"unit inventory"),
                fetched_at: 10,
            },
        )
        .unwrap();
        let mut artifacts = BTreeMap::new();
        let mut observations = Vec::new();
        let mut sources = BTreeMap::new();
        for (path, bytes) in files {
            let artifact = inventory
                .artifact(
                    &RepositoryPath::parse(path).unwrap(),
                    FilePurpose::GgufWeight,
                )
                .unwrap();
            observations.push(SourceGgufObservation {
                artifact: artifact.clone(),
                snapshot: inventory.snapshot.clone(),
                header: inspect_gguf(
                    Cursor::new(&bytes),
                    bytes.len() as u64,
                    GgufReadLimits::default(),
                )
                .unwrap(),
            });
            sources.insert(artifact.id.clone(), bytes);
            artifacts.insert(artifact.id.clone(), artifact);
        }
        let component = Component {
            id: "main".into(),
            role: ComponentRole::Main,
            artifact_ids: artifacts.keys().cloned().collect(),
            compatibility: ComponentCompatibility::Verified,
            evidence_ids: BTreeSet::from(["unit-sharded-format-fixture".into()]),
        };
        let snapshot_id = artifacts.values().next().unwrap().snapshot_id.clone();
        repository
            .save_catalog(&ModelCatalogSnapshot {
                model: ModelDefinition {
                    id: "unit-model".into(),
                    display_name: "Unit transfer shards".into(),
                    snapshot_ids: BTreeSet::from([snapshot_id.clone()]),
                    architecture: ArchitectureKnowledge::Unknown,
                    structure: ModelStructure::Unknown,
                    task_families: BTreeSet::from([ModelTaskFamily::Language]),
                    declared_capabilities: BTreeSet::from([ModelCapability::Text]),
                },
                variants: BTreeMap::from([(
                    "unit-variant".into(),
                    ModelVariant {
                        id: "unit-variant".into(),
                        model_id: "unit-model".into(),
                        artifact_ids: artifacts.keys().cloned().collect(),
                        quantization_label: "unit F32".into(),
                        tensor_types: BTreeSet::from([0]),
                        evidence_ids: BTreeSet::from(["unit-fixture".into()]),
                    },
                )]),
                snapshots: BTreeMap::from([(snapshot_id, inventory.snapshot)]),
                artifacts,
                components: BTreeMap::from([(component.id.clone(), component.clone())]),
            })
            .unwrap();
        for observation in &observations {
            repository.save_gguf_observation(observation, 20).unwrap();
        }
        let observation = ComponentObservation {
            role: ComponentRole::Main,
            files: observations,
        };
        let runtime = baseline_pairing_runtime().unwrap();
        let envelope = AdapterResourceEnvelope {
            runtime: runtime.clone(),
            component_digests: BTreeSet::from([observation.digest().unwrap()]),
            workload: ResourceWorkload {
                context_tokens: 256,
                batch_tokens: 32,
                parallel_sequences: 1,
                recurrent_rollback_slots: 0,
                kv_key_type: ResourceCacheType::F16,
                kv_value_type: ResourceCacheType::F16,
                prompt_cache_bytes: 0,
                context_checkpoint_slots: 0,
                vision: None,
            },
            compute_buffer_bytes: 1024,
            auxiliary_buffer_bytes: 1024,
            evidence_id: "unit-transfer-envelope".into(),
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
        assert!(assessment.blocking_reasons.is_empty(), "{assessment:?}");
        let plan = assessment.plan.unwrap();
        let job = repository
            .enqueue_installation(
                &JobAcceptance {
                    id: "unit-job".into(),
                    idempotency_key: "unit-install".into(),
                    plan_id: plan.id.clone(),
                    plan_digest: plan.digest().unwrap(),
                },
                &InstallationInput::new(
                    &store,
                    "unit-installation".into(),
                    "unit-version".into(),
                    InstallationSource::ModelScope {},
                )
                .unwrap(),
                31,
                1 << 30,
            )
            .unwrap();
        let lease = repository
            .claim(&job.id, job.version, "unit-worker", 40, 120)
            .unwrap();
        let job = repository.read_job(&job.id).unwrap().unwrap();
        repository
            .advance(&lease, job.version, LifecycleStage::Downloading, 40)
            .unwrap();
        Self {
            _temp: temp,
            repository,
            store,
            lease,
            sources,
        }
    }

    fn checkpoint_complete_file(&self, file: &FileTransferRecord) -> Result<()> {
        let bytes = &self.sources[&file.artifact_id];
        let path = self
            .store
            .transfer_file_path(&self.lease.job_id, &file.relative_path)?;
        let mut output = crate::lifecycle::files::open_exclusive_transfer(&path)?;
        output.write_all(bytes)?;
        output.sync_data()?;
        self.repository.checkpoint_synced_file(
            &self.lease,
            &file.artifact_id,
            file.version,
            TransferCheckpoint {
                completed_bytes: bytes.len() as u64,
                prefix_sha256: Sha256Digest::of_bytes(bytes),
                resume_validator: ResumeValidator::StrongEtag {
                    value: "\"stable\"".into(),
                },
            },
            50,
        )?;
        Ok(())
    }
}

struct PendingFileGuard(Rc<Cell<bool>>);

impl Drop for PendingFileGuard {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

#[test]
fn failed_file_cancels_its_sibling_and_preserves_the_siblings_synced_prefix_and_space_credit() {
    let fixture = TransferFixture::new(2);
    let execution = InstallExecution {
        repository: &fixture.repository,
        store: &fixture.store,
        lease: fixture.lease.clone(),
        clock: || 50,
        control: Arc::new(InstallationWorkerControl::default()),
    };
    let files = fixture
        .repository
        .file_transfers(&fixture.lease.job_id)
        .unwrap();
    let first_id = files[0].artifact_id.clone();
    let failing_id = files[1].artifact_id.clone();
    let prefix = fixture.sources[&first_id][..512].to_vec();
    let before = fixture
        .repository
        .storage_reservation(&fixture.lease.job_id)
        .unwrap()
        .unwrap();
    let (sender, receiver) = oneshot::channel::<()>();
    let sender = RefCell::new(Some(sender));
    let receiver = RefCell::new(Some(receiver));
    let sibling_dropped = Rc::new(Cell::new(false));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime.block_on(async {
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            download_files(&execution, files, |file| {
                let first = file.artifact_id == first_id;
                let sender = first.then(|| sender.borrow_mut().take().unwrap());
                let receiver = (!first).then(|| receiver.borrow_mut().take().unwrap());
                let prefix = prefix.clone();
                let sibling_dropped = sibling_dropped.clone();
                let execution = &execution;
                async move {
                    if let Some(sender) = sender {
                        let _guard = PendingFileGuard(sibling_dropped);
                        let path = execution
                            .store
                            .transfer_file_path(&execution.lease.job_id, &file.relative_path)?;
                        let mut output = crate::lifecycle::files::open_exclusive_transfer(&path)?;
                        output.write_all(&prefix)?;
                        output.sync_data()?;
                        execution.repository.checkpoint_synced_file(
                            &execution.lease,
                            &file.artifact_id,
                            file.version,
                            TransferCheckpoint {
                                completed_bytes: prefix.len() as u64,
                                prefix_sha256: Sha256Digest::of_bytes(&prefix),
                                resume_validator: ResumeValidator::StrongEtag {
                                    value: "\"stable\"".into(),
                                },
                            },
                            50,
                        )?;
                        sender.send(()).unwrap();
                        std::future::pending::<Result<()>>().await
                    } else {
                        receiver.unwrap().await?;
                        assert!(!sibling_dropped.get());
                        Err(source_error(ManagedTransportError {
                            kind: ManagedTransportErrorKind::Unavailable,
                            http_status: Some(503),
                            retry_after_seconds: None,
                        }))
                    }
                }
            }),
        )
        .await
        .unwrap()
        .unwrap_err();
        execution.settle_failure(&error).unwrap();
        error
    });
    assert_eq!(
        error.downcast_ref::<ManagedTransportError>().unwrap().kind,
        ManagedTransportErrorKind::Unavailable
    );
    assert!(sibling_dropped.get());
    let first = fixture
        .repository
        .read_file_transfer(&fixture.lease.job_id, &first_id)
        .unwrap()
        .unwrap();
    assert_eq!(first.checkpoint.completed_bytes, 512);
    assert_eq!(
        first.checkpoint.prefix_sha256,
        Sha256Digest::of_bytes(&prefix)
    );
    assert_eq!(
        std::fs::read(
            fixture
                .store
                .transfer_file_path(&fixture.lease.job_id, &first.relative_path)
                .unwrap()
        )
        .unwrap(),
        prefix
    );
    let reservation = fixture
        .repository
        .storage_reservation(&fixture.lease.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(reservation.state, StorageReservationState::Held);
    assert_eq!(reservation.reserved_bytes, before.reserved_bytes - 512);
    let failed = fixture
        .repository
        .latest_installation_failure(&fixture.lease.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        failed.details.artifact_id.as_deref(),
        Some(failing_id.as_str())
    );
    assert_eq!(failed.failure.code, LifecycleErrorCode::SourceUnavailable);
    let job = fixture
        .repository
        .read_job(&fixture.lease.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, JobStatus::Queued);
    assert!(job.generation > fixture.lease.generation);
    assert!(LifecycleRepository::validate_lease(&job, &fixture.lease, 50).is_err());
    let failures: u64 = rusqlite::Connection::open(fixture._temp.path().join("models.db"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM installation_failures WHERE job_id=?1",
            [&fixture.lease.job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(failures, 1);
    let permits: Vec<_> = (0..4)
        .map(|_| {
            fixture
                .store
                .try_transfer_slot(TransferSlot::File)
                .unwrap()
                .unwrap()
        })
        .collect();
    assert!(fixture
        .store
        .try_transfer_slot(TransferSlot::File)
        .unwrap()
        .is_none());
    drop(permits);
}

#[test]
fn completed_files_are_skipped_and_finished_transfers_admit_the_next_waiting_file() {
    let fixture = TransferFixture::new(4);
    let mut files = fixture
        .repository
        .file_transfers(&fixture.lease.job_id)
        .unwrap();
    let skipped_id = files[0].artifact_id.clone();
    fixture
        .repository
        .begin_file_attempt(&fixture.lease, &skipped_id, 50)
        .unwrap();
    fixture.checkpoint_complete_file(&files[0]).unwrap();
    files = fixture
        .repository
        .file_transfers(&fixture.lease.job_id)
        .unwrap();
    assert!(matches!(files[0].state, FileTransferState::Downloaded));
    let blocked_id = files[1].artifact_id.clone();
    let release_id = files[3].artifact_id.clone();
    let expected_started: Vec<_> = files[1..]
        .iter()
        .map(|file| file.artifact_id.clone())
        .collect();
    let (sender, receiver) = oneshot::channel::<()>();
    let sender = RefCell::new(Some(sender));
    let receiver = RefCell::new(Some(receiver));
    let started = RefCell::new(Vec::new());
    let execution = InstallExecution {
        repository: &fixture.repository,
        store: &fixture.store,
        lease: fixture.lease.clone(),
        clock: || 50,
        control: Arc::new(InstallationWorkerControl::default()),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(5),
            download_files(&execution, files, |file| {
                let sender =
                    (file.artifact_id == release_id).then(|| sender.borrow_mut().take().unwrap());
                let receiver =
                    (file.artifact_id == blocked_id).then(|| receiver.borrow_mut().take().unwrap());
                let fixture = &fixture;
                started.borrow_mut().push(file.artifact_id.clone());
                async move {
                    if let Some(receiver) = receiver {
                        receiver.await?;
                    }
                    if let Some(sender) = sender {
                        sender.send(()).unwrap();
                    }
                    fixture.checkpoint_complete_file(&file)
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
    });
    assert_eq!(*started.borrow(), expected_started);
    let files = fixture
        .repository
        .file_transfers(&fixture.lease.job_id)
        .unwrap();
    assert!(files
        .iter()
        .all(|file| matches!(file.state, FileTransferState::Downloaded)));
    let budgets = fixture
        .repository
        .file_retry_budgets(&fixture.lease.job_id)
        .unwrap();
    assert!(budgets.iter().all(|budget| budget.recorded_attempts == 1));
    let reservation = fixture
        .repository
        .storage_reservation(&fixture.lease.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(reservation.peak_bytes - reservation.reserved_bytes, 4096);
    assert!(fixture
        .repository
        .latest_installation_failure(&fixture.lease.job_id)
        .unwrap()
        .is_none());
}
