use lumina_local_inference::lifecycle::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn component(id: &str, artifact: &str) -> Component {
    Component {
        id: id.to_string(),
        role: ComponentRole::Main,
        artifact_ids: BTreeSet::from([artifact.to_string()]),
        compatibility: ComponentCompatibility::Verified,
        evidence_ids: BTreeSet::from(["rule-1".to_string()]),
    }
}

fn plan() -> InstallPlan {
    InstallPlan {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        id: "plan-1".to_string(),
        model_id: "model-1".to_string(),
        variant_id: "variant-1".to_string(),
        target_capabilities: BTreeSet::from([ModelCapability::Text]),
        snapshots: BTreeMap::from([(
            "snapshot-1".to_string(),
            RepositorySnapshot {
                repository: ModelScopeRepository::parse("test-owner/test-model").unwrap(),
                requested_revision: "master".to_string(),
                resolved_revision: "a".repeat(40),
                files_digest: Sha256Digest::of_bytes(b"test file inventory"),
                fetched_at: 100,
            },
        )]),
        artifacts: BTreeMap::from([(
            "main-file".to_string(),
            Artifact {
                id: "main-file".to_string(),
                snapshot_id: "snapshot-1".to_string(),
                path: RepositoryPath::parse("weights/main.gguf").unwrap(),
                purpose: FilePurpose::GgufWeight,
                size_bytes: Some(1024),
                source_sha256: None,
            },
        )]),
        dependencies: DependencyGraph {
            root: "main".to_string(),
            components: BTreeMap::from([("main".to_string(), component("main", "main-file"))]),
            edges: vec![],
        },
        runtime: RuntimeBuild {
            adapter_id: "test-adapter".to_string(),
            engine_revision: "test-build".to_string(),
            binding_version: "test-binding".to_string(),
            platform: "test-platform".to_string(),
            features: BTreeSet::new(),
            binary_sha256: Sha256Digest::of_bytes(b"test engine identity"),
        },
        runtime_verified: true,
        hardware: HardwareCompatibility::Runnable,
        required_disk_bytes: 2048,
        rules_version: "test-rules-v1".to_string(),
        expires_at: 1000,
        admission: None,
    }
}

fn catalog() -> ModelCatalogSnapshot {
    let plan = plan();
    ModelCatalogSnapshot {
        model: ModelDefinition {
            id: "model-1".to_string(),
            display_name: "test model".to_string(),
            snapshot_ids: plan.snapshots.keys().cloned().collect(),
            architecture: ArchitectureKnowledge::Unknown,
            structure: ModelStructure::Unknown,
            task_families: BTreeSet::from([ModelTaskFamily::Language]),
            declared_capabilities: BTreeSet::from([ModelCapability::Text]),
        },
        variants: BTreeMap::from([(
            "variant-1".to_string(),
            ModelVariant {
                id: "variant-1".to_string(),
                model_id: "model-1".to_string(),
                artifact_ids: plan.artifacts.keys().cloned().collect(),
                quantization_label: "test quantization".to_string(),
                tensor_types: BTreeSet::new(),
                evidence_ids: BTreeSet::new(),
            },
        )]),
        snapshots: plan.snapshots,
        artifacts: plan.artifacts,
        components: plan.dependencies.components,
    }
}

#[test]
fn source_identity_and_paths_reject_ambiguous_or_unsafe_values() {
    for value in [
        "repo",
        "a/b/c",
        "../model",
        "owner/..",
        "https://modelscope.cn/a/b",
        "owner/model?revision=main",
        "owner/model\\file",
    ] {
        assert!(ModelScopeRepository::parse(value).is_err(), "{value}");
    }
    for value in [
        "",
        "../model.gguf",
        "/model.gguf",
        "a//b.gguf",
        "C:/model.gguf",
        "a\\b.gguf",
        "a/./b.gguf",
        "NUL.gguf",
        "a/COM1.json",
        "a/file.gguf.",
        "a/file.gguf ",
        "a/file?.gguf",
    ] {
        assert!(RepositoryPath::parse(value).is_err(), "{value}");
    }
    assert!(RepositoryPath::parse("weights/model with spaces.gguf").is_ok());
    assert!(serde_json::from_str::<ModelScopeRepository>("\"owner/model?x=1\"").is_err());
    assert!(serde_json::from_str::<Sha256Digest>("\"invalid\"").is_err());
    assert_eq!(
        Sha256Digest::parse("A".repeat(64)).unwrap().as_str(),
        "a".repeat(64)
    );
}

#[test]
fn install_guards_reject_mutable_revisions_unknown_hardware_and_missing_artifacts() {
    let original = plan();
    original.validate_for_install(101, 4096).unwrap();
    let mut modified = original.clone();
    modified
        .snapshots
        .get_mut("snapshot-1")
        .unwrap()
        .resolved_revision = "master".to_string();
    assert!(modified.validate_for_install(101, 4096).is_err());
    modified = original.clone();
    modified.hardware = HardwareCompatibility::Unknown;
    assert!(modified.validate_for_install(101, 4096).is_err());
    modified = original.clone();
    modified.artifacts.clear();
    assert!(modified.validate_for_install(101, 4096).is_err());
    modified = original.clone();
    modified.artifacts.get_mut("main-file").unwrap().size_bytes = None;
    assert!(modified.validate_for_install(101, 4096).is_err());
    assert!(original.validate_for_install(1000, 4096).is_err());
    assert!(original.validate_for_install(101, 1024).is_err());
    modified = original.clone();
    modified.model_id.clear();
    assert!(modified.validate_for_install(101, 4096).is_err());
    modified = original;
    modified.variant_id = " ".to_string();
    assert!(modified.validate_for_install(101, 4096).is_err());
}

#[test]
fn forbidden_weights_cannot_be_relabelled_as_metadata() {
    let mut artifact = plan().artifacts.remove("main-file").unwrap();
    artifact.path = RepositoryPath::parse("model.safetensors").unwrap();
    assert!(artifact.validate().is_err());
    artifact.purpose = FilePurpose::RuntimeConfig;
    assert!(artifact.validate().is_err());
    artifact.path = RepositoryPath::parse("config.json").unwrap();
    artifact.validate().unwrap();
    let mut modified = plan();
    modified.artifacts.insert("main-file".to_string(), artifact);
    assert!(modified.validate_for_install(101, 4096).is_err());
}

#[test]
fn dependency_graph_distinguishes_missing_unknown_incompatible_and_selected_optional() {
    let mut graph = plan().dependencies;
    graph.edges.push(DependencyEdge {
        from: "main".to_string(),
        to: "projector".to_string(),
        requirement: DependencyRequirement::Required,
    });
    assert_eq!(
        graph.completeness().unwrap(),
        ComponentCompleteness::Incomplete
    );
    graph.edges[0].requirement = DependencyRequirement::Conditional {
        condition: "vision".to_string(),
        resolution: ConditionResolution::Unknown,
    };
    assert_eq!(
        graph.completeness().unwrap(),
        ComponentCompleteness::Unverified
    );
    graph.edges[0].requirement = DependencyRequirement::Optional;
    assert_eq!(
        graph.completeness().unwrap(),
        ComponentCompleteness::Complete
    );
    let mut projector = component("projector", "projector-file");
    projector.compatibility = ComponentCompatibility::Unverified;
    graph.components.insert("projector".to_string(), projector);
    assert_eq!(
        graph.completeness().unwrap(),
        ComponentCompleteness::Unverified
    );
    graph.components.get_mut("projector").unwrap().compatibility =
        ComponentCompatibility::Incompatible;
    assert_eq!(
        graph.completeness().unwrap(),
        ComponentCompleteness::Incompatible
    );
    graph.components.get_mut("projector").unwrap().compatibility = ComponentCompatibility::Verified;
    assert_eq!(
        graph.completeness().unwrap(),
        ComponentCompleteness::Complete
    );
    graph.edges.push(DependencyEdge {
        from: "projector".to_string(),
        to: "main".to_string(),
        requirement: DependencyRequirement::Required,
    });
    assert!(graph.completeness().is_err());
}

#[test]
fn lifecycle_does_not_skip_install_verification_or_merge_capability_results() {
    assert!(LifecycleStage::Downloading
        .advance(LifecycleStage::Installed)
        .is_err());
    assert!(LifecycleStage::Downloading
        .advance(LifecycleStage::Verifying)
        .is_ok());
    assert!(JobStatus::Cancelled.transition(JobStatus::Running).is_err());
    let states = BTreeMap::from([
        (ModelCapability::Text, CapabilityStatus::Ready),
        (ModelCapability::Vision, CapabilityStatus::Failed),
    ]);
    assert!(profile_ready(&[ModelCapability::Text], &states));
    assert!(!profile_ready(
        &[ModelCapability::Text, ModelCapability::Vision],
        &states
    ));
    assert!(!profile_ready(&[], &states));
    assert!(LifecycleErrorCode::SourceTimeout.retryable());
    assert!(!LifecycleErrorCode::HashMismatch.retryable());
    assert!(!LifecycleErrorCode::DependencyUnverified.retryable());
}

#[test]
fn immutable_plan_digest_changes_with_revision_components_runtime_and_profile() {
    let original = plan();
    let digest = original.digest().unwrap();
    let roundtrip: InstallPlan =
        serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(digest, roundtrip.digest().unwrap());
    let mut modified = original.clone();
    modified.runtime.features.insert("cuda".to_string());
    assert_ne!(digest, modified.digest().unwrap());
    modified = original.clone();
    modified
        .snapshots
        .get_mut("snapshot-1")
        .unwrap()
        .resolved_revision = "b".repeat(40);
    assert_ne!(digest, modified.digest().unwrap());
    modified = original;
    modified.target_capabilities.insert(ModelCapability::Vision);
    assert_ne!(digest, modified.digest().unwrap());
}

#[test]
fn manifest_rejects_incomplete_changed_or_unassigned_files() {
    let original = plan();
    let mut manifest = ModelManifest {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        installation_id: "installation".to_string(),
        version_id: "version-1".to_string(),
        plan: original,
        installed_files: BTreeMap::from([(
            "main-file".to_string(),
            InstalledArtifact {
                artifact_id: "main-file".to_string(),
                relative_path: RepositoryPath::parse("files/main.gguf").unwrap(),
                computed_sha256: Sha256Digest::of_bytes(b"test installed file"),
                size_bytes: 1024,
                verification_evidence: Sha256Digest::of_bytes(b"test structure verification"),
            },
        )]),
        plan_accepted_at: 102,
        installed_at: 1100,
    };
    manifest.validate().unwrap();
    let mut collisions = manifest.clone();
    let mut artifact = collisions.plan.artifacts["main-file"].clone();
    artifact.id = "second-file".to_string();
    artifact.path = RepositoryPath::parse("weights/second.gguf").unwrap();
    collisions
        .plan
        .artifacts
        .insert(artifact.id.clone(), artifact);
    collisions
        .plan
        .dependencies
        .components
        .get_mut("main")
        .unwrap()
        .artifact_ids
        .insert("second-file".to_string());
    let mut installed = collisions.installed_files["main-file"].clone();
    installed.artifact_id = "second-file".to_string();
    installed.relative_path = RepositoryPath::parse("FILES/MAIN.GGUF").unwrap();
    collisions
        .installed_files
        .insert(installed.artifact_id.clone(), installed);
    collisions.plan.validate_for_install(102, 4096).unwrap();
    assert!(collisions.validate().is_err());
    manifest
        .installed_files
        .get_mut("main-file")
        .unwrap()
        .size_bytes = 1;
    assert!(manifest.validate().is_err());
    manifest
        .installed_files
        .get_mut("main-file")
        .unwrap()
        .size_bytes = 1024;
    manifest
        .plan
        .artifacts
        .get_mut("main-file")
        .unwrap()
        .source_sha256 = Some(Sha256Digest::of_bytes(b"other"));
    assert!(manifest.validate().is_err());
    manifest.installed_files.clear();
    assert!(manifest.validate().is_err());
}

#[test]
fn shared_component_files_are_deduplicated_without_allowing_ambiguous_source_paths() {
    let mut original = plan();
    let mut encoder = component("encoder", "main-file");
    encoder.role = ComponentRole::AudioEncoder;
    original
        .dependencies
        .components
        .insert(encoder.id.clone(), encoder);
    original.dependencies.edges.push(DependencyEdge {
        from: "main".to_string(),
        to: "encoder".to_string(),
        requirement: DependencyRequirement::Required,
    });
    original.validate_for_install(101, 4096).unwrap();
    let mut duplicate = original.artifacts["main-file"].clone();
    duplicate.id = "duplicate".to_string();
    original.artifacts.insert(duplicate.id.clone(), duplicate);
    assert!(original.validate_for_install(101, 4096).is_err());
}

#[test]
fn catalog_records_remain_separate_immutable_and_recoverable_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let original = catalog();
    repo.save_catalog(&original).unwrap();
    repo.save_catalog(&original).unwrap();
    let mut refreshed = original.clone();
    refreshed
        .snapshots
        .get_mut("snapshot-1")
        .unwrap()
        .fetched_at += 1;
    repo.save_catalog(&refreshed).unwrap();
    let mut changed = original.clone();
    changed
        .variants
        .get_mut("variant-1")
        .unwrap()
        .quantization_label = "changed".to_string();
    assert!(repo.save_catalog(&changed).is_err());
    drop(repo);
    let restarted = LifecycleRepository::open(&path).unwrap();
    assert_eq!(restarted.read_catalog("model-1").unwrap(), Some(original));
    assert!(restarted.read_catalog("absent").unwrap().is_none());
    let raw = rusqlite::Connection::open(&path).unwrap();
    for table in [
        "catalog_models",
        "catalog_variants",
        "catalog_components",
        "catalog_artifacts",
    ] {
        let count: u32 = raw
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[test]
fn catalog_rejects_cross_model_variants_and_dangling_file_references() {
    let mut modified = catalog();
    modified.variants.get_mut("variant-1").unwrap().model_id = "other".to_string();
    assert!(modified.validate().is_err());
    modified = catalog();
    modified.artifacts.clear();
    assert!(modified.validate().is_err());
    modified = catalog();
    modified.components.get_mut("main").unwrap().role = ComponentRole::Metadata;
    assert!(modified.validate().is_err());
}

#[test]
fn catalog_links_preserve_shared_files_and_detect_missing_records() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let original = catalog();
    repo.save_catalog(&original).unwrap();
    let mut second = original.clone();
    second.model.id = "model-2".to_string();
    let mut variant = second.variants.remove("variant-1").unwrap();
    variant.id = "variant-2".to_string();
    variant.model_id = second.model.id.clone();
    second.variants.insert(variant.id.clone(), variant);
    repo.save_catalog(&second).unwrap();
    assert_eq!(repo.read_catalog("model-2").unwrap(), Some(second));
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.pragma_update(None, "foreign_keys", true).unwrap();
    assert!(raw
        .execute("DELETE FROM catalog_artifacts WHERE id = 'main-file'", [])
        .is_err());
    raw.execute("DELETE FROM model_artifacts WHERE model_id = 'model-1'", [])
        .unwrap();
    assert!(repo.read_catalog("model-1").is_err());
    assert!(repo.read_catalog("model-2").unwrap().is_some());
}

#[test]
fn version_one_snapshot_records_migrate_without_changing_source_identity() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch(include_str!(
        "../src/lifecycle/repository/migrations/0001.sql"
    ))
    .unwrap();
    let snapshot = plan().snapshots.remove("snapshot-1").unwrap();
    raw.execute("INSERT INTO repository_snapshots (id, repository, revision, digest, payload) VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params!["snapshot-1", snapshot.repository.as_str(), snapshot.resolved_revision, Sha256Digest::of_serializable(&snapshot).unwrap().as_str(), serde_json::to_string(&snapshot).unwrap()]).unwrap();
    drop(raw);
    let repo = LifecycleRepository::open(&path).unwrap();
    assert_eq!(repo.read_snapshot("snapshot-1").unwrap(), Some(snapshot));
    repo.save_catalog(&catalog()).unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    let version: u32 = raw
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, MODEL_DATABASE_SCHEMA_VERSION);
}

#[test]
fn corrupt_version_one_snapshot_rolls_back_the_entire_schema_upgrade() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch(include_str!(
        "../src/lifecycle/repository/migrations/0001.sql"
    ))
    .unwrap();
    let snapshot = plan().snapshots.remove("snapshot-1").unwrap();
    raw.execute("INSERT INTO repository_snapshots (id, repository, revision, digest, payload) VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params!["snapshot-1", snapshot.repository.as_str(), snapshot.resolved_revision, "corrupt", serde_json::to_string(&snapshot).unwrap()]).unwrap();
    assert!(LifecycleRepository::open(&path).is_err());
    let version: u32 = raw
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let tables: u32 = raw
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'catalog_models'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}

#[test]
fn repository_persists_immutable_plans_and_rejects_id_reuse() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let original = plan();
    let repo = LifecycleRepository::open(&path).unwrap();
    let digest = repo.save_plan(&original, 101, 4096).unwrap();
    assert_eq!(digest, repo.save_plan(&original, 101, 4096).unwrap());
    let mut modified = original.clone();
    modified.expires_at += 1;
    assert!(repo.save_plan(&modified, 101, 4096).is_err());
    drop(repo);
    let repo = LifecycleRepository::open(&path).unwrap();
    assert_eq!(repo.read_plan("plan-1").unwrap(), Some(original.clone()));
    assert_eq!(
        repo.read_snapshot("snapshot-1").unwrap(),
        original.snapshots.get("snapshot-1").cloned()
    );
}

#[test]
fn concurrent_idempotent_requests_create_one_job_and_durable_event() {
    let temp = tempfile::tempdir().unwrap();
    let repo = Arc::new(LifecycleRepository::open(&temp.path().join("models.db")).unwrap());
    let digest = repo.save_plan(&plan(), 101, 4096).unwrap();
    let threads: Vec<_> = (0..8)
        .map(|index| {
            let repo = repo.clone();
            let digest = digest.clone();
            std::thread::spawn(move || {
                repo.enqueue(
                    &format!("job-{index}"),
                    "install-key",
                    "plan-1",
                    &digest,
                    102,
                    4096,
                )
                .unwrap()
            })
        })
        .collect();
    let jobs: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(jobs.iter().all(|job| job == &jobs[0]));
    let events = repo.events(&jobs[0].id, 0, 100).unwrap();
    assert_eq!(events.len(), 1);
    assert!(repo
        .events(&jobs[0].id, events[0].sequence, 100)
        .unwrap()
        .is_empty());
    assert!(repo
        .enqueue(
            "another",
            "install-key",
            "plan-1",
            &Sha256Digest::of_bytes(b"wrong"),
            102,
            4096
        )
        .is_err());
}

#[test]
fn expired_worker_is_fenced_across_database_connections_and_restarts() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let first = LifecycleRepository::open(&path).unwrap();
    let digest = first.save_plan(&plan(), 101, 4096).unwrap();
    first
        .enqueue("job", "key", "plan-1", &digest, 102, 4096)
        .unwrap();
    let old_lease = first.claim("job", 0, "worker-a", 103, 10).unwrap();
    let second = LifecycleRepository::open(&path).unwrap();
    assert!(second.claim("job", 1, "worker-b", 112, 10).is_err());
    let new_lease = second.claim("job", 1, "worker-b", 113, 10).unwrap();
    assert!(first
        .advance(&old_lease, 2, LifecycleStage::Downloading, 114)
        .is_err());
    let job = second
        .advance(&new_lease, 2, LifecycleStage::Downloading, 114)
        .unwrap();
    assert_eq!(job.version, 3);
    let paused = first.transition("job", 3, JobStatus::Paused, 115).unwrap();
    assert!(second
        .advance(&new_lease, paused.version, LifecycleStage::Verifying, 116)
        .is_err());
    let queued = first
        .transition("job", paused.version, JobStatus::Queued, 117)
        .unwrap();
    drop(first);
    drop(second);
    let restarted = LifecycleRepository::open(&path).unwrap();
    assert_eq!(restarted.read_job("job").unwrap().unwrap(), queued);
    assert_eq!(restarted.events("job", 0, 100).unwrap().len(), 6);
}

#[test]
fn stored_plan_corruption_and_future_database_schema_are_detected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    repo.save_plan(&plan(), 101, 4096).unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE installation_plans SET payload = replace(payload, 'test-rules-v1', 'altered')",
        [],
    )
    .unwrap();
    assert!(repo.read_plan("plan-1").is_err());
    assert!(repo.save_plan(&plan(), 101, 4096).is_err());
    raw.pragma_update(None, "user_version", MODEL_DATABASE_SCHEMA_VERSION + 1)
        .unwrap();
    assert!(LifecycleRepository::open(&path).is_err());
}

#[test]
fn worker_heartbeat_extends_ownership_but_cannot_revive_an_expired_lease() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let first = LifecycleRepository::open(&path).unwrap();
    let digest = first.save_plan(&plan(), 101, 4096).unwrap();
    first
        .enqueue("job", "key", "plan-1", &digest, 102, 4096)
        .unwrap();
    let lease = first.claim("job", 0, "worker-a", 103, 10).unwrap();
    let renewed = first.renew_lease(&lease, 1, 110, 30).unwrap();
    assert_eq!(renewed.lease_until, Some(140));
    let second = LifecycleRepository::open(&path).unwrap();
    assert!(second
        .claim("job", renewed.version, "worker-b", 114, 10)
        .is_err());
    assert!(first.renew_lease(&lease, renewed.version, 140, 30).is_err());
    let replacement = second
        .claim("job", renewed.version, "worker-b", 140, 30)
        .unwrap();
    let current = second.read_job("job").unwrap().unwrap();
    assert!(first.renew_lease(&lease, current.version, 141, 30).is_err());
    assert_eq!(replacement.generation, 2);
}

#[test]
fn capability_evidence_is_independent_and_invalidated_by_changed_identity() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let identity = ValidationIdentity {
        manifest_digest: Sha256Digest::of_bytes(b"test manifest"),
        runtime: plan().runtime,
        launch_digest: Sha256Digest::of_bytes(b"test launch"),
        suite_version: "test-suite-1".to_string(),
    };
    let mut evidence = CapabilityEvidence {
        installation_id: "installation".to_string(),
        capability: ModelCapability::Text,
        identity: identity.clone(),
        status: CapabilityStatus::Ready,
        input_digest: Sha256Digest::of_bytes(b"test input identity"),
        output_digest: None,
        evidence_path: String::new(),
        validated_at: 100,
    };
    assert!(repo.save_evidence(&evidence).is_err());
    evidence.output_digest = Some(Sha256Digest::of_bytes(b"test output identity"));
    evidence.evidence_path = "test-evidence.json".to_string();
    repo.save_evidence(&evidence).unwrap();
    assert_eq!(
        repo.evidence("installation").unwrap(),
        vec![evidence.clone()]
    );
    assert_eq!(evidence.status_for(&identity), CapabilityStatus::Ready);
    let mut changed = identity;
    changed.launch_digest = Sha256Digest::of_bytes(b"changed launch");
    assert_eq!(evidence.status_for(&changed), CapabilityStatus::Stale);
}

fn synthetic_gguf_weight() -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.push(b'x');
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.resize(1024, 0);
    bytes
}

fn checkpoint(bytes: &[u8], etag: &str) -> TransferCheckpoint {
    TransferCheckpoint {
        completed_bytes: bytes.len() as u64,
        prefix_sha256: Sha256Digest::of_bytes(bytes),
        resume_validator: ResumeValidator::StrongEtag {
            value: format!("\"{etag}\""),
        },
    }
}

#[test]
fn accepted_job_recovers_after_plan_expiry_but_new_acceptance_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let original = plan();
    let repo = LifecycleRepository::open(&path).unwrap();
    let digest = repo.save_plan(&original, 101, 4096).unwrap();
    let job = repo
        .enqueue("job", "key", &original.id, &digest, 102, 4096)
        .unwrap();
    assert_eq!(job.accepted_at, 102);
    assert!(repo
        .enqueue("new-job", "new-key", &original.id, &digest, 1001, 4096)
        .is_err());
    drop(repo);
    let repo = LifecycleRepository::open(&path).unwrap();
    assert!(repo.claim("job", 0, "recovered-worker", 1001, 30).is_ok());
    let files = repo.file_transfers("job").unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].checkpoint.completed_bytes, 0);
    assert_eq!(repo.events("job", 0, 100).unwrap()[0].job.accepted_at, 102);
}

#[test]
fn file_progress_is_durable_fenced_and_requires_physical_verification() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let original = plan();
    let bytes = synthetic_gguf_weight();
    let first = LifecycleRepository::open(&path).unwrap();
    let digest = first.save_plan(&original, 101, 4096).unwrap();
    first
        .enqueue("job", "key", &original.id, &digest, 102, 4096)
        .unwrap();
    let initial = first
        .read_file_transfer("job", "main-file")
        .unwrap()
        .unwrap();
    std::fs::write(temp.path().join(initial.relative_path.as_str()), &bytes).unwrap();
    let old = first.claim("job", 0, "worker-a", 103, 30).unwrap();
    assert!(first
        .checkpoint_file(&old, "main-file", 0, checkpoint(&bytes[..512], "etag"), 104)
        .is_err());
    first
        .advance(&old, 1, LifecycleStage::Downloading, 104)
        .unwrap();
    assert!(first
        .advance(&old, 2, LifecycleStage::Verifying, 104)
        .is_err());
    let partial = first
        .checkpoint_file(&old, "main-file", 0, checkpoint(&bytes[..512], "etag"), 105)
        .unwrap();
    verify_partial_file(
        temp.path(),
        &partial.relative_path,
        &partial.checkpoint,
        1024,
    )
    .unwrap();
    let job = first.read_job("job").unwrap().unwrap();
    let paused = first
        .transition("job", job.version, JobStatus::Paused, 106)
        .unwrap();
    assert!(first
        .checkpoint_file(
            &old,
            "main-file",
            partial.version,
            checkpoint(&bytes, "etag"),
            107
        )
        .is_err());
    let queued = first
        .transition("job", paused.version, JobStatus::Queued, 107)
        .unwrap();
    drop(first);
    let repo = LifecycleRepository::open(&path).unwrap();
    assert_eq!(
        repo.read_file_transfer("job", "main-file").unwrap(),
        Some(partial.clone())
    );
    let lease = repo
        .claim("job", queued.version, "worker-b", 108, 30)
        .unwrap();
    assert!(repo
        .checkpoint_file(
            &old,
            "main-file",
            partial.version,
            checkpoint(&bytes, "etag"),
            109
        )
        .is_err());
    let downloaded = repo
        .checkpoint_file(
            &lease,
            "main-file",
            partial.version,
            checkpoint(&bytes, "etag"),
            109,
        )
        .unwrap();
    assert_eq!(downloaded.state, FileTransferState::Downloaded);
    let job = repo.read_job("job").unwrap().unwrap();
    let verifying = repo
        .advance(&lease, job.version, LifecycleStage::Verifying, 110)
        .unwrap();
    assert!(repo
        .advance(&lease, verifying.version, LifecycleStage::Installing, 111)
        .is_err());
    let verified = verify_artifact_file(
        &original,
        "main-file",
        temp.path(),
        &downloaded.relative_path,
    )
    .unwrap();
    let record = repo
        .record_verified_file(&lease, "main-file", downloaded.version, &verified, 111)
        .unwrap();
    assert!(matches!(record.state, FileTransferState::Verified { .. }));
    assert_eq!(
        record.checkpoint.prefix_sha256,
        Sha256Digest::of_bytes(&bytes)
    );
    assert!(repo
        .record_verified_file(&lease, "main-file", record.version, &verified, 112)
        .is_err());
    let job = repo.read_job("job").unwrap().unwrap();
    let installing = repo
        .advance(&lease, job.version, LifecycleStage::Installing, 112)
        .unwrap();
    assert!(repo
        .advance(&lease, installing.version, LifecycleStage::Installed, 113)
        .is_err());
    let events = repo.file_transfer_events("job", 0, 100).unwrap();
    assert_eq!(events.len(), 4);
    assert!(events
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence
            && pair[0].file.version + 1 == pair[1].file.version));
    assert!(repo
        .file_transfer_events("job", events.last().unwrap().sequence, 100)
        .unwrap()
        .is_empty());
    drop(repo);
    let reopened = LifecycleRepository::open(&path).unwrap();
    assert_eq!(
        reopened.read_file_transfer("job", "main-file").unwrap(),
        Some(record)
    );
}

#[test]
fn changed_identity_or_regressing_download_requires_explicit_file_restart() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let digest = repo.save_plan(&plan(), 101, 4096).unwrap();
    repo.enqueue("job", "key", "plan-1", &digest, 102, 4096)
        .unwrap();
    let lease = repo.claim("job", 0, "worker", 103, 30).unwrap();
    repo.advance(&lease, 1, LifecycleStage::Downloading, 104)
        .unwrap();
    let bytes = synthetic_gguf_weight();
    let partial = repo
        .checkpoint_file(
            &lease,
            "main-file",
            0,
            checkpoint(&bytes[..512], "old"),
            105,
        )
        .unwrap();
    assert!(repo
        .checkpoint_file(
            &lease,
            "main-file",
            partial.version,
            checkpoint(&bytes[..256], "old"),
            106
        )
        .is_err());
    assert!(repo
        .checkpoint_file(
            &lease,
            "main-file",
            partial.version,
            checkpoint(&bytes, "new"),
            106
        )
        .is_err());
    assert!(repo
        .checkpoint_file(&lease, "main-file", 0, checkpoint(&bytes, "old"), 106)
        .is_err());
    let restarted = repo
        .restart_file(
            &lease,
            "main-file",
            partial.version,
            FileRestartReason::ValidatorChanged,
            106,
        )
        .unwrap();
    assert_eq!(restarted.checkpoint.completed_bytes, 0);
    assert_eq!(
        restarted.restart_reason,
        Some(FileRestartReason::ValidatorChanged)
    );
    let downloaded = repo
        .checkpoint_file(
            &lease,
            "main-file",
            restarted.version,
            checkpoint(&bytes, "new"),
            107,
        )
        .unwrap();
    assert_eq!(downloaded.state, FileTransferState::Downloaded);
}

#[test]
fn artifact_verification_detects_same_size_corruption_and_invalid_gguf() {
    let temp = tempfile::tempdir().unwrap();
    let path = RepositoryPath::parse("actual.part").unwrap();
    let mut original = plan();
    let bytes = synthetic_gguf_weight();
    original
        .artifacts
        .get_mut("main-file")
        .unwrap()
        .source_sha256 = Some(Sha256Digest::of_bytes(&bytes));
    std::fs::write(temp.path().join(path.as_str()), &bytes).unwrap();
    let proof = verify_artifact_file(&original, "main-file", temp.path(), &path).unwrap();
    assert_eq!(proof.sha256(), &Sha256Digest::of_bytes(&bytes));
    assert_eq!(proof.size_bytes(), 1024);
    verify_partial_file(temp.path(), &path, &checkpoint(&bytes[..512], "etag"), 1024).unwrap();
    let mut changed = bytes.clone();
    changed[100] = 1;
    std::fs::write(temp.path().join(path.as_str()), &changed).unwrap();
    assert!(verify_artifact_file(&original, "main-file", temp.path(), &path).is_err());
    assert!(
        verify_partial_file(temp.path(), &path, &checkpoint(&bytes[..512], "etag"), 1024).is_err()
    );
    original
        .artifacts
        .get_mut("main-file")
        .unwrap()
        .source_sha256 = None;
    changed[..4].copy_from_slice(b"BAD!");
    std::fs::write(temp.path().join(path.as_str()), &changed).unwrap();
    assert!(verify_artifact_file(&original, "main-file", temp.path(), &path).is_err());
    std::fs::write(temp.path().join(path.as_str()), &bytes[..512]).unwrap();
    assert!(verify_artifact_file(&original, "main-file", temp.path(), &path).is_err());
    let artifact = original.artifacts.get_mut("main-file").unwrap();
    artifact.path = RepositoryPath::parse("config.json").unwrap();
    artifact.purpose = FilePurpose::RuntimeConfig;
    std::fs::write(temp.path().join(path.as_str()), &bytes).unwrap();
    assert!(verify_artifact_file(&original, "main-file", temp.path(), &path).is_err());
    let mut json = b"{\"actual_metadata\":true}".to_vec();
    json.resize(1024, b' ');
    std::fs::write(temp.path().join(path.as_str()), &json).unwrap();
    assert!(verify_artifact_file(&original, "main-file", temp.path(), &path).is_ok());
}

#[test]
fn observed_plan_rechecks_downloaded_header_even_when_source_has_no_sha256() {
    let temp = tempfile::tempdir().unwrap();
    let path = RepositoryPath::parse("observed.gguf").unwrap();
    let bytes = synthetic_gguf_weight();
    let header = inspect_gguf(
        std::io::Cursor::new(&bytes),
        bytes.len() as u64,
        GgufReadLimits::default(),
    )
    .unwrap();
    let mut plan = plan();
    plan.rules_version = OBSERVED_GGUF_PLAN_RULES_VERSION.into();
    plan.dependencies
        .components
        .get_mut("main")
        .unwrap()
        .evidence_ids
        .insert(gguf_header_evidence_id("main-file", &header.header_digest));
    std::fs::write(temp.path().join(path.as_str()), &bytes).unwrap();
    verify_artifact_file(&plan, "main-file", temp.path(), &path).unwrap();
    let mut changed = bytes;
    changed[32] = b'y';
    std::fs::write(temp.path().join(path.as_str()), &changed).unwrap();
    let error = verify_artifact_file(&plan, "main-file", temp.path(), &path).unwrap_err();
    assert!(error.to_string().contains("accepted source observation"));
}

#[test]
fn persisted_file_records_and_events_detect_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let digest = repo.save_plan(&plan(), 101, 4096).unwrap();
    repo.enqueue("job", "key", "plan-1", &digest, 102, 4096)
        .unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE job_file_events SET payload = replace(payload, 'main-file', 'tampered')",
        [],
    )
    .unwrap();
    assert!(repo.file_transfer_events("job", 0, 100).is_err());
    raw.execute(
        "UPDATE job_files SET payload = replace(payload, 'main-file', 'tampered')",
        [],
    )
    .unwrap();
    assert!(repo.file_transfers("job").is_err());
}

#[test]
fn weak_etags_and_control_characters_are_not_resume_validators() {
    for value in ["W/\"abc\"", "unquoted", "\"a\r\nb\"", "\"a\"b\""] {
        assert!(ResumeValidator::StrongEtag {
            value: value.to_string()
        }
        .validate()
        .is_err());
    }
    assert!(ResumeValidator::StrongEtag {
        value: "\"abc\"".to_string()
    }
    .validate()
    .is_ok());
    let invalid = TransferCheckpoint {
        completed_bytes: 0,
        prefix_sha256: Sha256Digest::of_bytes(b"nonempty"),
        resume_validator: ResumeValidator::Unknown,
    };
    assert!(invalid.validate(1024).is_err());
}

#[cfg(windows)]
#[test]
fn artifact_verifier_rejects_junctions_outside_its_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("actual.part"), synthetic_gguf_weight()).unwrap();
    let link = root.join("redirect");
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction creation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(verify_artifact_file(
        &plan(),
        "main-file",
        &root,
        &RepositoryPath::parse("redirect/actual.part").unwrap()
    )
    .is_err());
}

fn seed_version_two_job(path: &std::path::Path) {
    let raw = rusqlite::Connection::open(path).unwrap();
    raw.execute_batch(include_str!(
        "../src/lifecycle/repository/migrations/0001.sql"
    ))
    .unwrap();
    raw.execute_batch(include_str!(
        "../src/lifecycle/repository/migrations/0002.sql"
    ))
    .unwrap();
    let original = plan();
    let digest = original.digest().unwrap();
    raw.execute(
        "INSERT INTO installation_plans (id, digest, payload, expires_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            original.id,
            digest.as_str(),
            serde_json::to_string(&original).unwrap(),
            original.expires_at
        ],
    )
    .unwrap();
    raw.execute("INSERT INTO lifecycle_jobs (id, idempotency_key, plan_id, plan_digest, stage, status, updated_at) VALUES ('old-job', 'old-key', ?1, ?2, ?3, ?4, 108)", rusqlite::params![original.id, digest.as_str(), serde_json::to_string(&LifecycleStage::ReadyToInstall).unwrap(), serde_json::to_string(&JobStatus::Queued).unwrap()]).unwrap();
    let payload = serde_json::json!({
        "id":"old-job", "idempotency_key":"old-key", "plan_id":original.id,
        "plan_digest":digest, "stage":"ready_to_install", "status":"queued",
        "version":0,"generation":0,"lease_owner":null,"lease_until":null,"updated_at":102
    });
    raw.execute("INSERT INTO job_events (job_id, version, payload, created_at) VALUES ('old-job', 0, ?1, 102)", [serde_json::to_string(&payload).unwrap()]).unwrap();
}

#[test]
fn version_two_jobs_preserve_acceptance_time_and_gain_recoverable_file_records() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    seed_version_two_job(&path);
    let repo = LifecycleRepository::open(&path).unwrap();
    let job = repo.read_job("old-job").unwrap().unwrap();
    assert_eq!(job.accepted_at, 102);
    assert_eq!(job.updated_at, 108);
    let event = repo.events("old-job", 0, 100).unwrap().remove(0);
    assert_eq!(event.job.accepted_at, 102);
    assert!(repo.claim("old-job", 0, "worker", 1001, 30).is_ok());
    assert_eq!(repo.file_transfers("old-job").unwrap().len(), 1);
    drop(repo);
    let reopened = LifecycleRepository::open(&path).unwrap();
    assert_eq!(
        reopened.read_job("old-job").unwrap().unwrap().accepted_at,
        102
    );
    assert_eq!(
        reopened
            .file_transfer_events("old-job", 0, 100)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn invalid_historical_job_event_rolls_back_version_three_migration() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    seed_version_two_job(&path);
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute("UPDATE job_events SET payload = 'invalid-json'", [])
        .unwrap();
    assert!(LifecycleRepository::open(&path).is_err());
    let version: u32 = raw
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let tables: u32 = raw
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'job_files'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
    let columns: u32 = raw
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('lifecycle_jobs') WHERE name = 'accepted_at'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 0);
}

fn installation_fixture(
    repo: &LifecycleRepository,
    root: &std::path::Path,
    job_id: &str,
    version: &str,
) -> (WorkerLease, ModelManifest) {
    let original = plan();
    repo.save_catalog(&catalog()).unwrap();
    let digest = repo.save_plan(&original, 101, 4096).unwrap();
    repo.enqueue(job_id, job_id, &original.id, &digest, 102, 4096)
        .unwrap();
    let lease = repo.claim(job_id, 0, "installer", 103, 100).unwrap();
    repo.advance(&lease, 1, LifecycleStage::Downloading, 104)
        .unwrap();
    let bytes = synthetic_gguf_weight();
    let directory = root.join(format!("transfers/{job_id}"));
    std::fs::create_dir_all(&directory).unwrap();
    let file = repo
        .read_file_transfer(job_id, "main-file")
        .unwrap()
        .unwrap();
    std::fs::write(directory.join(file.relative_path.as_str()), &bytes).unwrap();
    let downloaded = repo
        .checkpoint_file(&lease, "main-file", 0, checkpoint(&bytes, "source"), 105)
        .unwrap();
    let job = repo.read_job(job_id).unwrap().unwrap();
    repo.advance(&lease, job.version, LifecycleStage::Verifying, 106)
        .unwrap();
    let verified = verify_artifact_file(
        &original,
        "main-file",
        &directory,
        &downloaded.relative_path,
    )
    .unwrap();
    repo.record_verified_file(&lease, "main-file", downloaded.version, &verified, 107)
        .unwrap();
    let job = repo.read_job(job_id).unwrap().unwrap();
    repo.advance(&lease, job.version, LifecycleStage::Installing, 108)
        .unwrap();
    let manifest = build_installation_manifest(
        "installation".into(),
        version.into(),
        original,
        &repo.file_transfers(job_id).unwrap(),
        102,
        109,
    )
    .unwrap();
    (lease, manifest)
}

fn write_staged_installation(
    root: &std::path::Path,
    manifest: &ModelManifest,
) -> std::path::PathBuf {
    let directory = root.join(installation_directory(manifest, false).unwrap().as_str());
    std::fs::create_dir_all(directory.join("files")).unwrap();
    for file in manifest.installed_files.values() {
        std::fs::write(
            directory.join(file.relative_path.as_str()),
            synthetic_gguf_weight(),
        )
        .unwrap();
    }
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec(manifest).unwrap(),
    )
    .unwrap();
    directory
}

fn split_gguf_weight(index: u16, count: u16, tensor_name: u8) -> Vec<u8> {
    let mut bytes = synthetic_gguf_weight();
    bytes[16..24].copy_from_slice(&3u64.to_le_bytes());
    let mut metadata = Vec::new();
    for (key, value) in [
        ("split.no", index),
        ("split.count", count),
        ("split.tensors.count", count),
    ] {
        metadata.extend((key.len() as u64).to_le_bytes());
        metadata.extend(key.as_bytes());
        metadata.extend(2u32.to_le_bytes());
        metadata.extend(value.to_le_bytes());
    }
    bytes[32] = tensor_name;
    bytes.splice(24..24, metadata);
    bytes.resize(1024, 0);
    bytes
}

fn physical_weights_manifest(root: &std::path::Path, weights: &[Vec<u8>]) -> ModelManifest {
    let mut plan = plan();
    plan.artifacts.clear();
    plan.dependencies
        .components
        .get_mut("main")
        .unwrap()
        .artifact_ids
        .clear();
    let mut installed_files = BTreeMap::new();
    for (index, bytes) in weights.iter().enumerate() {
        let id = format!("weight-{index}");
        plan.artifacts.insert(
            id.clone(),
            Artifact {
                id: id.clone(),
                snapshot_id: "snapshot-1".into(),
                path: RepositoryPath::parse(format!("{index}.gguf")).unwrap(),
                purpose: FilePurpose::GgufWeight,
                size_bytes: Some(bytes.len() as u64),
                source_sha256: Some(Sha256Digest::of_bytes(bytes)),
            },
        );
        plan.dependencies
            .components
            .get_mut("main")
            .unwrap()
            .artifact_ids
            .insert(id.clone());
        installed_files.insert(
            id.clone(),
            InstalledArtifact {
                artifact_id: id,
                relative_path: RepositoryPath::parse(format!("files/{index}.gguf")).unwrap(),
                computed_sha256: Sha256Digest::of_bytes(bytes),
                size_bytes: bytes.len() as u64,
                verification_evidence: Sha256Digest::of_bytes(b"test file evidence"),
            },
        );
    }
    let manifest = ModelManifest {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        installation_id: "installation".into(),
        version_id: "version".into(),
        plan,
        installed_files,
        plan_accepted_at: 102,
        installed_at: 109,
    };
    let directory = write_staged_installation(root, &manifest);
    for (index, bytes) in weights.iter().enumerate() {
        std::fs::write(directory.join(format!("files/{index}.gguf")), bytes).unwrap();
    }
    manifest
}

#[test]
fn installation_rechecks_complete_physical_shard_groups_before_issuing_witness() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = physical_weights_manifest(
        temp.path(),
        &[split_gguf_weight(0, 2, b'x'), split_gguf_weight(1, 2, b'y')],
    );
    verify_installation_directory(temp.path(), &manifest, false).unwrap();
}

#[test]
fn installation_rejects_missing_duplicate_and_unrelated_weights_despite_valid_file_hashes() {
    for weights in [
        vec![split_gguf_weight(0, 2, b'x')],
        vec![split_gguf_weight(0, 2, b'x'), split_gguf_weight(0, 2, b'y')],
        vec![split_gguf_weight(0, 2, b'x'), split_gguf_weight(1, 2, b'x')],
        vec![synthetic_gguf_weight(), synthetic_gguf_weight()],
    ] {
        let temp = tempfile::tempdir().unwrap();
        let manifest = physical_weights_manifest(temp.path(), &weights);
        let directory = temp
            .path()
            .join(installation_directory(&manifest, false).unwrap().as_str());
        for installed in manifest.installed_files.values() {
            verify_artifact_file(
                &manifest.plan,
                &installed.artifact_id,
                &directory,
                &installed.relative_path,
            )
            .unwrap();
        }
        let error = verify_installation_directory(temp.path(), &manifest, false).unwrap_err();
        assert!(
            error.to_string().contains("Invalid GGUF component"),
            "{error:#}"
        );
    }
}

fn advance_commit(
    repo: &LifecycleRepository,
    lease: &WorkerLease,
    witness: &InstallationFilesWitness,
    next: InstallationCommitStage,
    now: i64,
) -> InstallationCommitJournal {
    let version = repo.read_job(&lease.job_id).unwrap().unwrap().version;
    repo.advance_installation_commit(lease, version, witness, next, now)
        .unwrap()
}

fn finish_installation_fixture(
    repo: &LifecycleRepository,
    root: &std::path::Path,
    lease: &WorkerLease,
    manifest: &ModelManifest,
) {
    let version = repo.read_job(&lease.job_id).unwrap().unwrap().version;
    repo.begin_installation_commit(lease, version, manifest, 110)
        .unwrap();
    let staging = write_staged_installation(root, manifest);
    let witness = verify_installation_directory(root, manifest, false).unwrap();
    advance_commit(
        repo,
        lease,
        &witness,
        InstallationCommitStage::Prepared,
        111,
    );
    drop(witness);
    let committed = root.join(installation_directory(manifest, true).unwrap().as_str());
    std::fs::create_dir_all(committed.parent().unwrap()).unwrap();
    std::fs::rename(staging, committed).unwrap();
    let witness = verify_installation_directory(root, manifest, true).unwrap();
    advance_commit(
        repo,
        lease,
        &witness,
        InstallationCommitStage::FilesCommitted,
        112,
    );
    advance_commit(
        repo,
        lease,
        &witness,
        InstallationCommitStage::Registered,
        113,
    );
}

#[test]
fn installation_journal_recovers_each_boundary_and_only_registers_verified_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "version-1");
    let version = repo.read_job("job").unwrap().unwrap().version;
    let journal = repo
        .begin_installation_commit(&lease, version, &manifest, 110)
        .unwrap();
    assert_eq!(journal.stage, InstallationCommitStage::Staging);
    assert!(repo.active_installations().unwrap().is_empty());
    assert_eq!(
        repo.pending_installation_commits(10).unwrap(),
        vec![journal.clone()]
    );
    let current = repo.read_job("job").unwrap().unwrap().version;
    assert_eq!(
        repo.begin_installation_commit(&lease, current, &manifest, 110)
            .unwrap(),
        journal
    );
    assert_eq!(repo.read_job("job").unwrap().unwrap().version, current);
    assert!(verify_installation_directory(temp.path(), &manifest, false).is_err());
    let staging = write_staged_installation(temp.path(), &manifest);
    let witness = verify_installation_directory(temp.path(), &manifest, false).unwrap();
    assert!(repo
        .advance_installation_commit(
            &lease,
            current,
            &witness,
            InstallationCommitStage::FilesCommitted,
            111
        )
        .is_err());
    let prepared = advance_commit(
        &repo,
        &lease,
        &witness,
        InstallationCommitStage::Prepared,
        111,
    );
    drop(witness);
    drop(repo);
    let repo = LifecycleRepository::open(&path).unwrap();
    assert_eq!(repo.installation_commit("job").unwrap().unwrap(), prepared);
    assert!(repo.installed_version("version-1").unwrap().is_none());
    let version = repo.read_job("job").unwrap().unwrap().version;
    let recovered = repo.claim("job", version, "recovery", 1001, 100).unwrap();
    let committed = temp
        .path()
        .join(installation_directory(&manifest, true).unwrap().as_str());
    std::fs::create_dir_all(committed.parent().unwrap()).unwrap();
    std::fs::rename(staging, committed).unwrap();
    let witness = verify_installation_directory(temp.path(), &manifest, true).unwrap();
    let current = repo.read_job("job").unwrap().unwrap().version;
    assert!(repo
        .advance_installation_commit(
            &lease,
            current,
            &witness,
            InstallationCommitStage::FilesCommitted,
            1002
        )
        .is_err());
    let files_committed = advance_commit(
        &repo,
        &recovered,
        &witness,
        InstallationCommitStage::FilesCommitted,
        1002,
    );
    drop(repo);
    let repo = LifecycleRepository::open(&path).unwrap();
    assert_eq!(
        repo.installation_commit("job").unwrap().unwrap(),
        files_committed
    );
    assert!(repo.active_installations().unwrap().is_empty());
    let registered = advance_commit(
        &repo,
        &recovered,
        &witness,
        InstallationCommitStage::Registered,
        1003,
    );
    assert_eq!(registered.stage, InstallationCommitStage::Registered);
    let job = repo.read_job("job").unwrap().unwrap();
    assert_eq!(
        (job.stage, job.status),
        (LifecycleStage::Installed, JobStatus::Completed)
    );
    assert!(job.lease_owner.is_none());
    assert_eq!(
        repo.installed_version("version-1")
            .unwrap()
            .unwrap()
            .manifest,
        manifest
    );
    assert_eq!(
        repo.active_installations().unwrap()[0].active_version_id,
        "version-1"
    );
    assert!(repo.pending_installation_commits(10).unwrap().is_empty());
    assert_eq!(
        advance_commit(
            &repo,
            &recovered,
            &witness,
            InstallationCommitStage::Registered,
            2000
        ),
        registered
    );
    assert_eq!(repo.read_job("job").unwrap().unwrap().version, job.version);
}

#[test]
fn installation_versions_preserve_old_binding_and_share_content_references() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let (first, manifest) = installation_fixture(&repo, temp.path(), "first", "v1");
    finish_installation_fixture(&repo, temp.path(), &first, &manifest);
    let old = repo.installed_version("v1").unwrap().unwrap();
    let (second, updated) = installation_fixture(&repo, temp.path(), "second", "v2");
    finish_installation_fixture(&repo, temp.path(), &second, &updated);
    assert_eq!(repo.installation_versions("installation").unwrap().len(), 2);
    assert_eq!(
        repo.active_installations().unwrap()[0].active_version_id,
        "v1"
    );
    assert_eq!(
        repo.blob_reference_count(&Sha256Digest::of_bytes(&synthetic_gguf_weight()))
            .unwrap(),
        2
    );
    assert!(repo
        .activate_installation_version("installation", "v2", "v2")
        .is_err());
    assert!(repo
        .activate_installation_version("other-installation", "v1", "v2")
        .is_err());
    assert_eq!(
        repo.activate_installation_version("installation", "v1", "v2")
            .unwrap()
            .manifest,
        updated
    );
    assert_eq!(repo.installed_version("v1").unwrap().unwrap(), old);
    drop(repo);
    let repo = LifecycleRepository::open(&path).unwrap();
    assert_eq!(
        repo.active_installations().unwrap()[0].active_version_id,
        "v2"
    );
    repo.activate_installation_version("installation", "v2", "v1")
        .unwrap();
    assert_eq!(
        repo.active_installations().unwrap()[0].manifest_digest,
        old.manifest_digest
    );
}

#[test]
fn installation_commit_rejects_forged_ledgers_and_wrong_filesystem_identity() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let current = repo.read_job("job").unwrap().unwrap().version;
    let mut forged = manifest.clone();
    forged
        .installed_files
        .get_mut("main-file")
        .unwrap()
        .computed_sha256 = Sha256Digest::of_bytes(b"different content");
    assert!(repo
        .begin_installation_commit(&lease, current, &forged, 110)
        .is_err());
    forged = manifest.clone();
    forged.plan_accepted_at += 1;
    assert!(repo
        .begin_installation_commit(&lease, current, &forged, 110)
        .is_err());
    assert!(repo.installation_commit("job").unwrap().is_none());
    repo.begin_installation_commit(&lease, current, &manifest, 110)
        .unwrap();
    let staging = write_staged_installation(temp.path(), &manifest);
    let file = staging.join(manifest.installed_files["main-file"].relative_path.as_str());
    let mut bytes = synthetic_gguf_weight();
    bytes[1000] = 1;
    std::fs::write(&file, bytes).unwrap();
    assert!(verify_installation_directory(temp.path(), &manifest, false).is_err());
    std::fs::write(&file, synthetic_gguf_weight()).unwrap();
    let mut other = manifest.clone();
    other.version_id = "different-version".into();
    write_staged_installation(temp.path(), &other);
    let other_witness = verify_installation_directory(temp.path(), &other, false).unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    assert!(repo
        .advance_installation_commit(
            &lease,
            version,
            &other_witness,
            InstallationCommitStage::Prepared,
            111
        )
        .is_err());
    let witness = verify_installation_directory(temp.path(), &manifest, false).unwrap();
    assert!(repo
        .advance_installation_commit(
            &lease,
            version - 1,
            &witness,
            InstallationCommitStage::Prepared,
            111
        )
        .is_err());
    assert_eq!(
        repo.installation_commit("job").unwrap().unwrap().stage,
        InstallationCommitStage::Staging
    );
    #[cfg(windows)]
    assert!(std::fs::write(&file, synthetic_gguf_weight()).is_err());
}

#[test]
fn registration_failure_rolls_back_version_activation_references_and_job_completion() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let current = repo.read_job("job").unwrap().unwrap().version;
    repo.begin_installation_commit(&lease, current, &manifest, 110)
        .unwrap();
    let staging = write_staged_installation(temp.path(), &manifest);
    let witness = verify_installation_directory(temp.path(), &manifest, false).unwrap();
    advance_commit(
        &repo,
        &lease,
        &witness,
        InstallationCommitStage::Prepared,
        111,
    );
    drop(witness);
    let committed = temp
        .path()
        .join(installation_directory(&manifest, true).unwrap().as_str());
    std::fs::create_dir_all(committed.parent().unwrap()).unwrap();
    std::fs::rename(staging, committed).unwrap();
    let witness = verify_installation_directory(temp.path(), &manifest, true).unwrap();
    advance_commit(
        &repo,
        &lease,
        &witness,
        InstallationCommitStage::FilesCommitted,
        112,
    );
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TRIGGER inject_failure BEFORE INSERT ON installation_blob_refs BEGIN SELECT RAISE(ABORT, 'injected disk commit failure'); END;").unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    assert!(repo
        .advance_installation_commit(
            &lease,
            version,
            &witness,
            InstallationCommitStage::Registered,
            113
        )
        .is_err());
    assert!(repo.installed_version("v1").unwrap().is_none());
    assert!(repo.active_installations().unwrap().is_empty());
    assert_eq!(
        repo.blob_reference_count(&Sha256Digest::of_bytes(&synthetic_gguf_weight()))
            .unwrap(),
        0
    );
    assert_eq!(repo.read_job("job").unwrap().unwrap().version, version);
    assert_eq!(
        repo.installation_commit("job").unwrap().unwrap().stage,
        InstallationCommitStage::FilesCommitted
    );
    raw.execute_batch("DROP TRIGGER inject_failure;").unwrap();
    advance_commit(
        &repo,
        &lease,
        &witness,
        InstallationCommitStage::Registered,
        114,
    );
    assert_eq!(repo.active_installations().unwrap().len(), 1);
}

#[test]
fn installation_records_detect_manifest_and_journal_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    finish_installation_fixture(&repo, temp.path(), &lease, &manifest);
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE installation_versions SET payload = json_set(payload, '$.version_id', 'forged')",
        [],
    )
    .unwrap();
    assert!(repo.installed_version("v1").is_err());
    assert!(repo.active_installations().is_err());
    raw.execute("UPDATE installation_commit_journals SET payload = json_set(payload, '$.stage', 'prepared')", []).unwrap();
    assert!(repo.installation_commit("job").is_err());
}

#[test]
fn schema_three_upgrade_preserves_jobs_and_rolls_back_on_installation_table_collision() {
    let temp = tempfile::tempdir().unwrap();
    for collision in [false, true] {
        let path = temp.path().join(format!("migration-{collision}.db"));
        seed_version_two_job(&path);
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch(include_str!(
            "../src/lifecycle/repository/migrations/0003.sql"
        ))
        .unwrap();
        if collision {
            raw.execute_batch("CREATE TABLE managed_blobs (legacy_data TEXT);")
                .unwrap();
            assert!(LifecycleRepository::open(&path).is_err());
            assert_eq!(
                raw.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                    .unwrap(),
                3
            );
            let count: u32 = raw
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = 'installation_versions'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0);
        } else {
            let repo = LifecycleRepository::open(&path).unwrap();
            assert_eq!(repo.read_job("old-job").unwrap().unwrap().accepted_at, 102);
            assert_eq!(
                raw.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                    .unwrap(),
                MODEL_DATABASE_SCHEMA_VERSION
            );
            assert!(repo.active_installations().unwrap().is_empty());
        }
    }
}

#[test]
fn launch_binding_cannot_retarget_installed_version_runtime_capabilities_or_components() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (_, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let launch = LaunchConfig {
        installation_id: manifest.installation_id.clone(),
        version_id: manifest.version_id.clone(),
        manifest_digest: manifest.digest().unwrap(),
        runtime: manifest.plan.runtime.clone(),
        capabilities: BTreeSet::from([ModelCapability::Text]),
        component_files: BTreeMap::from([(
            ComponentRole::Main,
            vec![manifest.installed_files["main-file"].relative_path.clone()],
        )]),
        parameters: RuntimeParameters::Specialized {
            schema_id: "test-adapter-schema".into(),
            settings: BTreeMap::new(),
        },
        mode: RuntimeExecutionMode::InProcess,
    };
    launch.validate_binding(&manifest).unwrap();
    let original_digest = launch.digest().unwrap();
    let mut changed = launch.clone();
    changed.version_id = "v2".into();
    assert!(changed.validate_binding(&manifest).is_err());
    assert_ne!(changed.digest().unwrap(), original_digest);
    changed = launch.clone();
    changed.runtime.features.insert("different-build".into());
    assert!(changed.validate_binding(&manifest).is_err());
    changed = launch.clone();
    changed.capabilities.insert(ModelCapability::Vision);
    assert!(changed.validate_binding(&manifest).is_err());
    changed = launch.clone();
    changed.component_files.clear();
    assert!(changed.validate_binding(&manifest).is_err());
    changed = launch.clone();
    changed
        .component_files
        .get_mut(&ComponentRole::Main)
        .unwrap()
        .push(RepositoryPath::parse("files/unplanned.gguf").unwrap());
    assert!(changed.validate_binding(&manifest).is_err());
    changed = launch;
    let paths = changed
        .component_files
        .get_mut(&ComponentRole::Main)
        .unwrap();
    paths.push(paths[0].clone());
    assert!(changed.validate_binding(&manifest).is_err());
}

#[test]
fn filesystem_installer_publishes_read_only_independent_files_and_replays_completion() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    let prepared = store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            || 110,
        )
        .unwrap();
    assert_eq!(prepared.stage, InstallationCommitStage::Prepared);
    assert!(repo.active_installations().unwrap().is_empty());
    let installed = store.commit(&repo, &lease, || 111).unwrap();
    assert_eq!(installed.manifest, manifest);
    assert_eq!(store.commit(&repo, &lease, || 1001).unwrap(), installed);
    let original = synthetic_gguf_weight();
    let blob = store.root().join(
        InstallationStore::blob_path(&Sha256Digest::of_bytes(&original))
            .unwrap()
            .as_str(),
    );
    let file = store
        .root()
        .join(installation_directory(&manifest, true).unwrap().as_str())
        .join(manifest.installed_files["main-file"].relative_path.as_str());
    assert!(std::fs::metadata(&blob).unwrap().permissions().readonly());
    assert!(std::fs::metadata(&file).unwrap().permissions().readonly());
    let transfer = repo
        .read_file_transfer("job", "main-file")
        .unwrap()
        .unwrap();
    let source = temp
        .path()
        .join("transfers/job")
        .join(transfer.relative_path.as_str());
    std::fs::set_permissions(&file, std::fs::metadata(source).unwrap().permissions()).unwrap();
    let mut changed = original.clone();
    changed[1000] = 1;
    std::fs::write(&file, changed).unwrap();
    assert_eq!(std::fs::read(blob).unwrap(), original);
    assert!(store.commit(&repo, &lease, || 1002).is_err());
    assert_eq!(
        repo.blob_reference_count(&Sha256Digest::of_bytes(&synthetic_gguf_weight()))
            .unwrap(),
        1
    );
}

#[test]
fn filesystem_installer_recovers_rename_before_journal_update_without_transfer_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            || 110,
        )
        .unwrap();
    let staging = store
        .root()
        .join(installation_directory(&manifest, false).unwrap().as_str());
    let final_path = store
        .root()
        .join(installation_directory(&manifest, true).unwrap().as_str());
    std::fs::create_dir_all(final_path.parent().unwrap()).unwrap();
    std::fs::rename(staging, &final_path).unwrap();
    let transfer = repo
        .read_file_transfer("job", "main-file")
        .unwrap()
        .unwrap();
    std::fs::remove_file(
        temp.path()
            .join("transfers/job")
            .join(transfer.relative_path.as_str()),
    )
    .unwrap();
    drop(repo);
    let repo = LifecycleRepository::open(&path).unwrap();
    assert!(repo.active_installations().unwrap().is_empty());
    let version = repo.read_job("job").unwrap().unwrap().version;
    let recovery = repo.claim("job", version, "recovery", 1001, 100).unwrap();
    assert!(store.commit(&repo, &lease, || 1002).is_err());
    let installed = store.commit(&repo, &recovery, || 1002).unwrap();
    assert_eq!(installed.manifest, manifest);
    assert_eq!(
        repo.installation_commit("job").unwrap().unwrap().stage,
        InstallationCommitStage::Registered
    );
}

#[test]
fn filesystem_installer_recovers_failed_registration_with_published_directory_intact() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            || 110,
        )
        .unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TRIGGER inject_failure BEFORE INSERT ON installation_versions BEGIN SELECT RAISE(ABORT, 'injected registration failure'); END;").unwrap();
    assert!(store.commit(&repo, &lease, || 111).is_err());
    assert!(store
        .root()
        .join(installation_directory(&manifest, true).unwrap().as_str())
        .join("manifest.json")
        .is_file());
    assert_eq!(
        repo.installation_commit("job").unwrap().unwrap().stage,
        InstallationCommitStage::FilesCommitted
    );
    assert!(repo.active_installations().unwrap().is_empty());
    raw.execute_batch("DROP TRIGGER inject_failure;").unwrap();
    drop(repo);
    let repo = LifecycleRepository::open(&path).unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    let recovered = repo.claim("job", version, "recovery", 1001, 100).unwrap();
    store.commit(&repo, &recovered, || 1002).unwrap();
    assert_eq!(repo.active_installations().unwrap().len(), 1);
}

#[test]
fn filesystem_installer_preserves_corrupt_cache_and_never_registers_it() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let blob = store.root().join(
        InstallationStore::blob_path(&Sha256Digest::of_bytes(&synthetic_gguf_weight()))
            .unwrap()
            .as_str(),
    );
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    let mut corrupt = synthetic_gguf_weight();
    corrupt[1000] = 1;
    std::fs::write(&blob, &corrupt).unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    assert!(store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            || 110
        )
        .is_err());
    assert_eq!(std::fs::read(blob).unwrap(), corrupt);
    assert_eq!(
        repo.installation_commit("job").unwrap().unwrap().stage,
        InstallationCommitStage::Staging
    );
    assert!(repo.active_installations().unwrap().is_empty());
}

#[test]
fn filesystem_installer_preserves_unknown_published_directory_and_respects_version_lock() {
    use fs2::FileExt;
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let lock_path = store.root().join(format!(
        "locks/versions/{}.lock",
        Sha256Digest::of_bytes(b"v1").as_str()
    ));
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    assert!(store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            || 110
        )
        .is_err());
    assert!(repo.installation_commit("job").unwrap().is_none());
    drop(lock);
    let final_path = store
        .root()
        .join(installation_directory(&manifest, true).unwrap().as_str());
    std::fs::create_dir_all(&final_path).unwrap();
    std::fs::write(final_path.join("unknown.txt"), "preserve").unwrap();
    assert!(store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            || 110
        )
        .is_err());
    assert_eq!(
        std::fs::read_to_string(final_path.join("unknown.txt")).unwrap(),
        "preserve"
    );
    assert!(repo.active_installations().unwrap().is_empty());
}

#[test]
fn filesystem_installer_renews_live_lease_during_copy_and_verification() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let ticks = std::cell::Cell::new(190);
    let clock = || {
        let now = ticks.get();
        ticks.set(now + 1);
        now
    };
    let version = repo.read_job("job").unwrap().unwrap().version;
    store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            clock,
        )
        .unwrap();
    let prepared_job = repo.read_job("job").unwrap().unwrap();
    assert!(prepared_job.lease_until.unwrap() > 203);
    assert_eq!(prepared_job.generation, lease.generation);
    store.commit(&repo, &lease, clock).unwrap();
    assert_eq!(
        repo.read_job("job").unwrap().unwrap().status,
        JobStatus::Completed
    );
}

#[test]
fn filesystem_installer_pause_preserves_verified_blob_for_resume_without_source() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let blob = store.root().join(
        InstallationStore::blob_path(&Sha256Digest::of_bytes(&synthetic_gguf_weight()))
            .unwrap()
            .as_str(),
    );
    let paused = std::cell::Cell::new(false);
    let clock = || {
        if blob.exists() && !paused.replace(true) {
            let job = repo.read_job("job").unwrap().unwrap();
            repo.transition("job", job.version, JobStatus::Paused, 110)
                .unwrap();
        }
        110
    };
    let version = repo.read_job("job").unwrap().unwrap().version;
    assert!(store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            clock
        )
        .is_err());
    assert!(paused.get());
    assert!(blob.is_file());
    assert!(repo.active_installations().unwrap().is_empty());
    let transfer = repo
        .read_file_transfer("job", "main-file")
        .unwrap()
        .unwrap();
    std::fs::remove_file(
        temp.path()
            .join("transfers/job")
            .join(transfer.relative_path.as_str()),
    )
    .unwrap();
    let job = repo.read_job("job").unwrap().unwrap();
    let queued = repo
        .transition("job", job.version, JobStatus::Queued, 111)
        .unwrap();
    let recovered = repo
        .claim("job", queued.version, "resumed", 112, 100)
        .unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    store
        .prepare(
            &repo,
            &recovered,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            || 113,
        )
        .unwrap();
    store.commit(&repo, &recovered, || 114).unwrap();
    assert_eq!(repo.active_installations().unwrap().len(), 1);
}

#[cfg(windows)]
#[test]
fn filesystem_installer_rejects_redirected_output_directory() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let status = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(store.root().join("blobs"))
        .arg(&outside)
        .output()
        .unwrap();
    assert!(status.status.success());
    let version = repo.read_job("job").unwrap().unwrap().version;
    assert!(store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            || 110
        )
        .is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
    assert!(repo.active_installations().unwrap().is_empty());
}

#[cfg(windows)]
#[test]
fn filesystem_installer_preserves_broken_redirect_at_published_destination() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (lease, manifest) = installation_fixture(&repo, temp.path(), "job", "v1");
    let store = InstallationStore::open(&temp.path().join("managed")).unwrap();
    let version = repo.read_job("job").unwrap().unwrap().version;
    store
        .prepare(
            &repo,
            &lease,
            version,
            &manifest,
            &temp.path().join("transfers/job"),
            || 110,
        )
        .unwrap();
    let final_path = store
        .root()
        .join(installation_directory(&manifest, true).unwrap().as_str());
    std::fs::create_dir_all(final_path.parent().unwrap()).unwrap();
    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let output = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&final_path)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(output.status.success());
    std::fs::remove_dir(&outside).unwrap();
    assert!(std::fs::symlink_metadata(&final_path).is_ok());
    assert!(store.commit(&repo, &lease, || 111).is_err());
    assert!(std::fs::symlink_metadata(&final_path).is_ok());
    assert!(store
        .root()
        .join(installation_directory(&manifest, false).unwrap().as_str())
        .is_dir());
    assert_eq!(
        repo.installation_commit("job").unwrap().unwrap().stage,
        InstallationCommitStage::Prepared
    );
    assert!(repo.active_installations().unwrap().is_empty());
}

#[test]
fn filesystem_installer_recovers_after_forced_process_termination_at_commit_boundaries() {
    const TEST_NAME: &str =
        "filesystem_installer_recovers_after_forced_process_termination_at_commit_boundaries";
    if let Ok(child_root) = std::env::var("LUMINA_INSTALLER_CRASH_CHILD_ROOT") {
        let root = std::path::PathBuf::from(child_root);
        assert!(root.is_absolute());
        let phase = std::env::var("LUMINA_INSTALLER_CRASH_PHASE").unwrap();
        let repo = LifecycleRepository::open(&root.join("models.db")).unwrap();
        let (lease, manifest) = installation_fixture(&repo, &root, "job", "v1");
        let store = InstallationStore::open(&root.join("managed")).unwrap();
        let version = repo.read_job("job").unwrap().unwrap().version;
        store
            .prepare(
                &repo,
                &lease,
                version,
                &manifest,
                &root.join("transfers/job"),
                || 110,
            )
            .unwrap();
        if phase == "renamed" {
            let staged = store
                .root()
                .join(installation_directory(&manifest, false).unwrap().as_str());
            let published = store
                .root()
                .join(installation_directory(&manifest, true).unwrap().as_str());
            std::fs::create_dir_all(published.parent().unwrap()).unwrap();
            std::fs::rename(staged, published).unwrap();
        } else if phase == "registered" {
            store.commit(&repo, &lease, || 111).unwrap();
        } else {
            assert_eq!(phase, "prepared");
        }
        std::fs::write(root.join("checkpoint-ready"), &phase).unwrap();
        loop {
            std::thread::park();
        }
    }

    let temp = tempfile::tempdir().unwrap();
    for phase in ["prepared", "renamed", "registered"] {
        let root = temp.path().join(phase);
        std::fs::create_dir(&root).unwrap();
        let log = std::fs::File::create(root.join("child.log")).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env("LUMINA_INSTALLER_CRASH_CHILD_ROOT", &root)
            .env("LUMINA_INSTALLER_CRASH_PHASE", phase)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let ready = loop {
            if std::fs::read_to_string(root.join("checkpoint-ready"))
                .ok()
                .as_deref()
                == Some(phase)
            {
                break true;
            }
            if child.try_wait().unwrap().is_some() {
                break false;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        if !ready {
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
            }
            child.wait().unwrap();
            panic!(
                "Child did not reach {phase}: {}",
                std::fs::read_to_string(root.join("child.log")).unwrap()
            );
        }
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        let repo = LifecycleRepository::open(&root.join("models.db")).unwrap();
        let store = InstallationStore::open(&root.join("managed")).unwrap();
        let journal = repo.installation_commit("job").unwrap().unwrap();
        let lease = if phase == "registered" {
            assert_eq!(repo.active_installations().unwrap().len(), 1);
            WorkerLease {
                job_id: "job".into(),
                owner: "completed-response".into(),
                generation: 0,
            }
        } else {
            assert!(repo.active_installations().unwrap().is_empty());
            assert_eq!(journal.stage, InstallationCommitStage::Prepared);
            let job = repo.read_job("job").unwrap().unwrap();
            repo.claim("job", job.version, "recovery", 1001, 100)
                .unwrap()
        };
        let recovered = store.commit(&repo, &lease, || 1002).unwrap();
        assert_eq!(recovered.manifest, journal.manifest);
        assert_eq!(repo.installation_versions("installation").unwrap().len(), 1);
        assert_eq!(
            repo.blob_reference_count(&Sha256Digest::of_bytes(&synthetic_gguf_weight()))
                .unwrap(),
            1
        );
        assert_eq!(
            repo.read_job("job").unwrap().unwrap().status,
            JobStatus::Completed
        );
        assert!(repo.pending_installation_commits(10).unwrap().is_empty());
    }
}
