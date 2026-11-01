use lumina_local_inference::lifecycle::*;
use rusqlite::{params, Connection};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn fixture() -> (ModelCatalogSnapshot, SourceGgufObservation) {
    let source=parse_modelscope_inventory(&serde_json::to_vec(&json!({"Code":200,"Data":{"Files":[{"Path":"weights.gguf","Type":"blob","Size":1024,"Sha256":Sha256Digest::of_bytes(b"unit-only expected bytes")} ]}})).unwrap(),RepositorySnapshot {repository:ModelScopeRepository::parse("unit/observation-fixture").unwrap(),requested_revision:"master".into(),resolved_revision:"a".repeat(40),files_digest:Sha256Digest::of_bytes(b"placeholder"),fetched_at:10}).unwrap();
    let artifact = source
        .artifact(
            &RepositoryPath::parse("weights.gguf").unwrap(),
            FilePurpose::GgufWeight,
        )
        .unwrap();
    let header = GgufInspection {
        version: 3,
        byte_order: GgufByteOrder::LittleEndian,
        file_bytes: 1024,
        header_bytes: 128,
        header_digest: Sha256Digest::of_bytes(b"unit-only header"),
        data_offset: 128,
        alignment: 32,
        metadata: BTreeMap::new(),
        tensors: vec![GgufTensor {
            name: "unit.weight".into(),
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
    };
    let observation = SourceGgufObservation {
        artifact: artifact.clone(),
        snapshot: source.snapshot.clone(),
        header,
    };
    let catalog = ModelCatalogSnapshot {
        model: ModelDefinition {
            id: "unit-model".into(),
            display_name: "Unit format fixture".into(),
            snapshot_ids: BTreeSet::from([artifact.snapshot_id.clone()]),
            architecture: ArchitectureKnowledge::Unknown,
            structure: ModelStructure::Unknown,
            task_families: BTreeSet::from([ModelTaskFamily::Unknown]),
            declared_capabilities: BTreeSet::new(),
        },
        snapshots: BTreeMap::from([(artifact.snapshot_id.clone(), source.snapshot)]),
        artifacts: BTreeMap::from([(artifact.id.clone(), artifact.clone())]),
        variants: BTreeMap::from([(
            "unit-variant".into(),
            ModelVariant {
                id: "unit-variant".into(),
                model_id: "unit-model".into(),
                artifact_ids: BTreeSet::from([artifact.id.clone()]),
                quantization_label: "Unit F32 fixture".into(),
                tensor_types: BTreeSet::from([0]),
                evidence_ids: BTreeSet::from(["unit-fixture".into()]),
            },
        )]),
        components: BTreeMap::from([(
            "unit-main".into(),
            Component {
                id: "unit-main".into(),
                role: ComponentRole::Main,
                artifact_ids: BTreeSet::from([artifact.id]),
                compatibility: ComponentCompatibility::Unverified,
                evidence_ids: BTreeSet::new(),
            },
        )]),
    };
    (catalog, observation)
}

#[test]
fn observations_require_registered_artifacts_and_survive_reopening_without_fetch_time_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let (catalog, mut observation) = fixture();
    let repo = LifecycleRepository::open(&path).unwrap();
    assert!(repo.save_gguf_observation(&observation, 20).is_err());
    repo.save_catalog(&catalog).unwrap();
    let first = repo.save_gguf_observation(&observation, 20).unwrap();
    observation.snapshot.fetched_at = 30;
    let second = repo.save_gguf_observation(&observation, 31).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        repo.gguf_observations_for_artifact(&observation.artifact.id)
            .unwrap()
            .len(),
        1
    );
    drop(repo);
    let reopened = LifecycleRepository::open(&path).unwrap();
    assert_eq!(
        reopened.read_gguf_observation(&first.id).unwrap(),
        Some(first)
    );
}

#[test]
fn changed_header_at_the_same_immutable_artifact_is_rejected_without_overwriting_proof() {
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (catalog, mut observation) = fixture();
    repo.save_catalog(&catalog).unwrap();
    let first = repo.save_gguf_observation(&observation, 20).unwrap();
    observation.header.header_digest = Sha256Digest::of_bytes(b"changed fixed-source header");
    let error = repo.save_gguf_observation(&observation, 21).unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::RevisionChanged
    );
    assert_eq!(repo.read_gguf_observation(&first.id).unwrap(), Some(first));
}

#[test]
fn payload_and_binding_corruption_cannot_become_valid_cached_metadata() {
    for column in ["payload_digest", "header_digest", "observation_digest"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.db");
        let repo = LifecycleRepository::open(&path).unwrap();
        let (catalog, observation) = fixture();
        repo.save_catalog(&catalog).unwrap();
        let record = repo.save_gguf_observation(&observation, 20).unwrap();
        let raw = Connection::open(&path).unwrap();
        raw.execute(
            &format!("UPDATE gguf_observations SET {column} = ?1 WHERE id = ?2"),
            params!["0".repeat(64), record.id.as_str()],
        )
        .unwrap();
        assert!(repo.read_gguf_observation(&record.id).is_err());
        assert!(repo.save_gguf_observation(&observation, 21).is_err());
    }
}

#[test]
fn schema_four_upgrades_atomically_and_conflicting_tables_leave_the_prior_schema_intact() {
    for conflict in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.db");
        let raw = Connection::open(&path).unwrap();
        for schema in [
            include_str!("../src/lifecycle/repository/migrations/0001.sql"),
            include_str!("../src/lifecycle/repository/migrations/0002.sql"),
            include_str!("../src/lifecycle/repository/migrations/0003.sql"),
            include_str!("../src/lifecycle/repository/migrations/0004.sql"),
        ] {
            raw.execute_batch(schema).unwrap();
        }
        if conflict {
            raw.execute_batch("CREATE TABLE gguf_observations (sentinel TEXT); INSERT INTO gguf_observations VALUES ('preserve');").unwrap();
        }
        drop(raw);
        let opened = LifecycleRepository::open(&path);
        let raw = Connection::open(&path).unwrap();
        let version: u32 = raw
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        if conflict {
            assert!(opened.is_err());
            assert_eq!(version, 4);
            assert_eq!(
                raw.query_row("SELECT sentinel FROM gguf_observations", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
                "preserve"
            );
        } else {
            assert!(opened.is_ok());
            assert_eq!(version, MODEL_DATABASE_SCHEMA_VERSION);
        }
    }
}

#[test]
fn concurrent_identical_observations_return_the_same_immutable_record() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let (catalog, observation) = fixture();
    let repo = LifecycleRepository::open(&path).unwrap();
    repo.save_catalog(&catalog).unwrap();
    drop(repo);
    let threads = (0..2)
        .map(|index| {
            let path = path.clone();
            let observation = observation.clone();
            std::thread::spawn(move || {
                LifecycleRepository::open(&path)
                    .unwrap()
                    .save_gguf_observation(&observation, 20 + index)
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let records = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records[0], records[1]);
}

#[test]
fn streaming_identity_hash_keeps_the_existing_canonical_json_encoding() {
    let value = BTreeMap::from([("key", json!("量化\nGGUF")), ("array", json!([1, 2, 3]))]);
    let canonical = r#"{"array":[1,2,3],"key":"量化\nGGUF"}"#;
    assert_eq!(
        Sha256Digest::of_serializable(&value).unwrap(),
        Sha256Digest::of_bytes(canonical.as_bytes())
    );
}

fn observed_plan(catalog: &ModelCatalogSnapshot, record: &GgufObservationRecord) -> InstallPlan {
    let mut components = catalog.components.clone();
    for component in components.values_mut() {
        component.compatibility = ComponentCompatibility::Verified;
        component.evidence_ids.extend([
            gguf_observation_evidence_id(&record.id),
            gguf_header_evidence_id(
                &record.observation.artifact.id,
                &record.observation.header.header_digest,
            ),
        ]);
    }
    InstallPlan {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        id: "unit-observed-plan".into(),
        model_id: catalog.model.id.clone(),
        variant_id: "unit-variant".into(),
        target_capabilities: BTreeSet::from([ModelCapability::Text]),
        snapshots: catalog.snapshots.clone(),
        artifacts: catalog.artifacts.clone(),
        dependencies: DependencyGraph {
            root: "unit-main".into(),
            components,
            edges: vec![],
        },
        runtime: RuntimeBuild {
            adapter_id: "unit-only".into(),
            engine_revision: "unit-build".into(),
            binding_version: "unit".into(),
            platform: "unit".into(),
            features: BTreeSet::new(),
            binary_sha256: Sha256Digest::of_bytes(b"unit"),
        },
        runtime_verified: true,
        hardware: HardwareCompatibility::Runnable,
        required_disk_bytes: 4096,
        rules_version: OBSERVED_GGUF_PLAN_RULES_VERSION.into(),
        expires_at: 1000,
        admission: None,
    }
}

#[test]
fn observed_plan_requires_exact_cached_bindings_and_survives_reopening() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let repo = LifecycleRepository::open(&path).unwrap();
    let (catalog, observation) = fixture();
    repo.save_catalog(&catalog).unwrap();
    let record = repo.save_gguf_observation(&observation, 20).unwrap();
    let plan = observed_plan(&catalog, &record);
    let bindings = BTreeMap::from([(observation.artifact.id.clone(), record.id.clone())]);
    assert!(repo.save_plan(&plan, 30, 8192).is_err());
    assert!(repo
        .save_observed_plan(&plan, &BTreeMap::new(), 30, 8192)
        .is_err());
    assert!(repo.read_plan(&plan.id).unwrap().is_none());
    let mut wrong = bindings.clone();
    wrong.insert(
        observation.artifact.id.clone(),
        Sha256Digest::of_bytes(b"missing"),
    );
    assert!(repo.save_observed_plan(&plan, &wrong, 30, 8192).is_err());
    let mut unbound = plan.clone();
    unbound
        .dependencies
        .components
        .get_mut("unit-main")
        .unwrap()
        .evidence_ids
        .remove(&gguf_observation_evidence_id(&record.id));
    assert!(repo
        .save_observed_plan(&unbound, &bindings, 30, 8192)
        .is_err());
    let digest = repo.save_observed_plan(&plan, &bindings, 30, 8192).unwrap();
    assert_eq!(
        repo.save_observed_plan(&plan, &bindings, 31, 8192).unwrap(),
        digest
    );
    drop(repo);
    let repo = LifecycleRepository::open(&path).unwrap();
    assert_eq!(repo.read_plan(&plan.id).unwrap(), Some(plan.clone()));
    repo.enqueue("unit-job", "unit-key", &plan.id, &digest, 40, 8192)
        .unwrap();
}

#[test]
fn deleted_or_corrupted_observation_bindings_prevent_new_job_acceptance() {
    for corruption in [
        "DELETE FROM plan_gguf_observations",
        "UPDATE gguf_observations SET header_digest = 'corrupt'",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("models.db");
        let repo = LifecycleRepository::open(&path).unwrap();
        let (catalog, observation) = fixture();
        repo.save_catalog(&catalog).unwrap();
        let record = repo.save_gguf_observation(&observation, 20).unwrap();
        let plan = observed_plan(&catalog, &record);
        let bindings = BTreeMap::from([(observation.artifact.id.clone(), record.id)]);
        let digest = repo.save_observed_plan(&plan, &bindings, 30, 8192).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch(corruption)
            .unwrap();
        assert!(repo.read_plan(&plan.id).is_err());
        assert!(repo
            .enqueue("unit-job", "unit-key", &plan.id, &digest, 40, 8192)
            .is_err());
        assert!(repo.read_job("unit-job").unwrap().is_none());
    }
}

#[test]
fn schema_five_binding_table_conflict_rolls_back_without_changing_prior_observations() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("models.db");
    let raw = Connection::open(&path).unwrap();
    for schema in [
        include_str!("../src/lifecycle/repository/migrations/0001.sql"),
        include_str!("../src/lifecycle/repository/migrations/0002.sql"),
        include_str!("../src/lifecycle/repository/migrations/0003.sql"),
        include_str!("../src/lifecycle/repository/migrations/0004.sql"),
        include_str!("../src/lifecycle/repository/migrations/0005.sql"),
    ] {
        raw.execute_batch(schema).unwrap();
    }
    raw.execute_batch("CREATE TABLE plan_gguf_observations (sentinel TEXT); INSERT INTO plan_gguf_observations VALUES ('preserve');").unwrap();
    drop(raw);
    assert!(LifecycleRepository::open(&path).is_err());
    let raw = Connection::open(&path).unwrap();
    assert_eq!(
        raw.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        5
    );
    assert_eq!(
        raw.query_row("SELECT sentinel FROM plan_gguf_observations", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
}

fn resource_fixture(
    architecture: &str,
) -> (
    ComponentObservation,
    AdapterResourceEnvelope,
    HardwareResourceSnapshot,
) {
    let (_, mut observation) = fixture();
    observation.header.metadata.insert(
        "general.architecture".into(),
        GgufValue::Scalar {
            type_id: 8,
            value: GgufScalar::String(architecture.into()),
        },
    );
    for (key, value) in [
        ("block_count", 24),
        ("embedding_length", 1024),
        ("attention.head_count", 8),
        ("attention.head_count_kv", 2),
        ("context_length", 32768),
        ("full_attention_interval", 4),
        ("ssm.conv_kernel", 4),
        ("ssm.inner_size", 2048),
        ("ssm.state_size", 128),
        ("ssm.group_count", 16),
    ] {
        observation.header.metadata.insert(
            format!("{architecture}.{key}"),
            GgufValue::Scalar {
                type_id: 4,
                value: GgufScalar::Unsigned(value),
            },
        );
    }
    let component = ComponentObservation {
        role: ComponentRole::Main,
        files: vec![observation],
    };
    let envelope = AdapterResourceEnvelope {
        runtime: baseline_pairing_runtime().unwrap(),
        component_digests: BTreeSet::from([component.digest().unwrap()]),
        workload: ResourceWorkload {
            context_tokens: 2048,
            batch_tokens: 512,
            parallel_sequences: 1,
            recurrent_rollback_slots: 0,
            kv_key_type: ResourceCacheType::F16,
            kv_value_type: ResourceCacheType::F16,
            prompt_cache_bytes: 0,
            context_checkpoint_slots: 0,
        },
        compute_buffer_bytes: 128 * 1024 * 1024,
        auxiliary_buffer_bytes: 64 * 1024 * 1024,
        evidence_id: "unit-only-workload-envelope".into(),
    };
    let hardware = HardwareResourceSnapshot {
        observed_at: 10,
        devices: vec![HardwareMemoryDevice {
            index: 0,
            backend: "unit".into(),
            name: "unit".into(),
            description: "unit".into(),
            kind: "cpu".into(),
            total_bytes: 8 * 1024 * 1024 * 1024,
            free_bytes: 4 * 1024 * 1024 * 1024,
        }],
        storage_volume: "unit-test".into(),
        disk_free_bytes: 16 * 1024 * 1024 * 1024,
    };
    (component, envelope, hardware)
}

#[test]
fn resource_budget_counts_weights_kv_runtime_buffers_and_three_disk_copies() {
    let (component, envelope, hardware) = resource_fixture("qwen3");
    let budget =
        assess_resource_budget(std::slice::from_ref(&component), &envelope, &hardware).unwrap();
    assert_eq!(budget.compatibility, HardwareCompatibility::Runnable);
    assert_eq!(budget.kv_cache_bytes, Some(24 * 2 * (128 + 128) * 2 * 2048));
    assert_eq!(
        budget.peak_incremental_disk_bytes,
        3 * 1024 + 16 * 1024 * 1024
    );
    assert_eq!(
        budget.host_memory_required_bytes,
        Some(1024 + budget.kv_cache_bytes.unwrap() + 192 * 1024 * 1024)
    );
    let mut low = hardware.clone();
    low.devices[0].free_bytes = 1024;
    assert_eq!(
        assess_resource_budget(std::slice::from_ref(&component), &envelope, &low)
            .unwrap()
            .compatibility,
        HardwareCompatibility::Incompatible
    );
    low = hardware;
    low.disk_free_bytes = 1024;
    assert_eq!(
        assess_resource_budget(&[component], &envelope, &low)
            .unwrap()
            .compatibility,
        HardwareCompatibility::Incompatible
    );
}

#[test]
fn hybrid_budget_keeps_recurrent_state_and_grows_with_parallel_rollback() {
    let (component, mut envelope, hardware) = resource_fixture("qwen35");
    let one =
        assess_resource_budget(std::slice::from_ref(&component), &envelope, &hardware).unwrap();
    assert_eq!(one.kv_cache_bytes, Some(6 * 2 * (128 + 128) * 2 * 2048));
    assert_eq!(
        one.recurrent_cache_bytes,
        Some(18 * ((4 - 1) * (2048 + 2 * 16 * 128) + 128 * 2048) * 4)
    );
    envelope.workload.parallel_sequences = 2;
    envelope.workload.recurrent_rollback_slots = 3;
    let many = assess_resource_budget(&[component], &envelope, &hardware).unwrap();
    assert_eq!(many.kv_cache_bytes, one.kv_cache_bytes.map(|n| n * 2));
    assert_eq!(
        many.recurrent_cache_bytes,
        one.recurrent_cache_bytes.map(|n| n * 8)
    );
    assert_ne!(one.binding_digest, many.binding_digest);
}

#[test]
fn cache_precision_prompt_cache_and_context_checkpoints_are_part_of_memory_and_identity() {
    let (component, mut envelope, hardware) = resource_fixture("qwen35");
    let original =
        assess_resource_budget(std::slice::from_ref(&component), &envelope, &hardware).unwrap();
    envelope.workload.kv_key_type = ResourceCacheType::F32;
    envelope.workload.kv_value_type = ResourceCacheType::F32;
    envelope.workload.prompt_cache_bytes = 8 * 1024 * 1024 * 1024;
    envelope.workload.context_checkpoint_slots = 32;
    let changed = assess_resource_budget(&[component], &envelope, &hardware).unwrap();
    assert_eq!(
        changed.kv_cache_bytes,
        original.kv_cache_bytes.map(|n| n * 2)
    );
    assert_eq!(changed.compatibility, HardwareCompatibility::Incompatible);
    assert!(changed.dynamic_cache_bytes.unwrap() > 8 * 1024 * 1024 * 1024);
    assert_ne!(original.binding_digest, changed.binding_digest);
}

#[test]
fn unknown_architecture_absent_hardware_or_different_profile_cannot_pass_resource_admission() {
    let (component, envelope, hardware) = resource_fixture("future-unknown");
    let unknown =
        assess_resource_budget(std::slice::from_ref(&component), &envelope, &hardware).unwrap();
    assert_eq!(unknown.compatibility, HardwareCompatibility::Unknown);
    assert_eq!(unknown.host_memory_required_bytes, None);
    let mut wrong = envelope;
    wrong.component_digests.clear();
    assert!(assess_resource_budget(&[component], &wrong, &hardware).is_err());
    let (component, envelope, mut hardware) = resource_fixture("qwen3");
    hardware.devices.clear();
    assert_eq!(
        assess_resource_budget(&[component], &envelope, &hardware)
            .unwrap()
            .compatibility,
        HardwareCompatibility::Unknown
    );
}

#[tokio::test]
async fn managed_weight_policy_and_explicit_legacy_mlx_entry_reject_other_sources_without_network()
{
    require_managed_weight_policy(ManagedWeightOrigin::ModelScope, ManagedWeightFormat::Gguf)
        .unwrap();
    for origin in [
        ManagedWeightOrigin::ModelScope,
        ManagedWeightOrigin::HuggingFace,
        ManagedWeightOrigin::Other,
    ] {
        for format in [
            ManagedWeightFormat::Gguf,
            ManagedWeightFormat::Safetensors,
            ManagedWeightFormat::Other,
        ] {
            if origin == ManagedWeightOrigin::ModelScope && format == ManagedWeightFormat::Gguf {
                continue;
            }
            let error = require_managed_weight_policy(origin, format).unwrap_err();
            assert_eq!(
                error.downcast_ref::<ModelSourceError>().unwrap().code,
                LifecycleErrorCode::FormatForbidden
            );
        }
    }
    let error = lumina_local_inference::hf_models::resolve_local_model_selection(
        "unit/no-network",
        "mlx",
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::FormatForbidden
    );
}

#[test]
fn plan_assembly_binds_cached_source_runtime_resource_profile_and_persists_only_admissible_results()
{
    let temp = tempfile::tempdir().unwrap();
    let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    let (catalog, _) = fixture();
    repo.save_catalog(&catalog).unwrap();
    let (component, envelope, hardware) = resource_fixture("qwen3");
    let record = repo.save_gguf_observation(&component.files[0], 20).unwrap();
    let original = observed_plan(&catalog, &record);
    let request = ObservedPlanRequest {
        id: "assembled-unit-plan".into(),
        model_id: original.model_id,
        variant_id: original.variant_id,
        target_capabilities: original.target_capabilities,
        dependencies: original.dependencies,
    };
    let profile = RuntimeInstallProfile {
        id: "unit-profile".into(),
        rules_version: RUNTIME_INSTALL_PROFILE_VERSION.into(),
        runtime: envelope.runtime.clone(),
        architectures: BTreeSet::from(["qwen3".into()]),
        capabilities: BTreeSet::from([ModelCapability::Text]),
        required_roles: BTreeSet::from([ComponentRole::Main]),
        evidence_ids: BTreeSet::from(["unit-only-declared-runtime-support".into()]),
    };
    let assessment = repo
        .prepare_observed_plan(&request, &profile, &envelope, &hardware, 30)
        .unwrap();
    assert!(assessment.blocking_reasons.is_empty());
    let plan = assessment.plan.unwrap();
    assert_eq!(repo.read_plan(&plan.id).unwrap(), Some(plan.clone()));
    assert_eq!(plan.expires_at, 330);
    assert_eq!(plan.rules_version, PREPARED_GGUF_PLAN_RULES_VERSION);
    assert_eq!(
        plan.admission.as_ref().unwrap().envelope.workload,
        envelope.workload
    );
    let mut changed = plan.clone();
    changed.id = "changed-cache-plan".into();
    changed
        .admission
        .as_mut()
        .unwrap()
        .envelope
        .workload
        .context_checkpoint_slots = 32;
    assert!(changed
        .validate_for_install(30, hardware.disk_free_bytes)
        .is_err());
    let mut changed = plan.clone();
    changed.id = "changed-budget-plan".into();
    changed
        .admission
        .as_mut()
        .unwrap()
        .budget
        .host_memory_required_bytes = Some(1);
    assert!(repo
        .save_observed_plan(
            &changed,
            &assessment.observation_bindings,
            30,
            hardware.disk_free_bytes
        )
        .is_err());
    assert!(repo.read_plan(&changed.id).unwrap().is_none());
    assert!(plan.dependencies.components["unit-main"]
        .evidence_ids
        .iter()
        .any(|e| e.starts_with("resource-budget:")));
    assert!(repo
        .prepare_observed_plan(&request, &profile, &envelope, &hardware, 31)
        .is_err());
    assert_eq!(repo.read_plan(&plan.id).unwrap(), Some(plan));
    for failure in [
        "stale",
        "unverified-dependency",
        "unsupported-runtime",
        "disk",
    ] {
        let mut request = request.clone();
        request.id = format!("blocked-{failure}");
        let mut profile = profile.clone();
        let mut hardware = hardware.clone();
        match failure {
            "stale" => hardware.observed_at = -100,
            "unverified-dependency" => {
                request
                    .dependencies
                    .components
                    .get_mut("unit-main")
                    .unwrap()
                    .compatibility = ComponentCompatibility::Unverified
            }
            "unsupported-runtime" => profile.architectures.clear(),
            "disk" => hardware.disk_free_bytes = 1,
            _ => unreachable!(),
        }
        let assessment = repo
            .prepare_observed_plan(&request, &profile, &envelope, &hardware, 30)
            .unwrap();
        assert!(assessment.plan.is_none() && !assessment.blocking_reasons.is_empty());
        assert!(repo.read_plan(&request.id).unwrap().is_none());
    }
}
