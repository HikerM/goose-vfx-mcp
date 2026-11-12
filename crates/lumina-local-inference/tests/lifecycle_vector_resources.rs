use lumina_local_inference::lifecycle::vector_runtime::VectorRuntimeProfile;
use lumina_local_inference::lifecycle::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[path = "support/vector_plan_cases.rs"]
mod vector_plan_cases;

const MIB: u64 = 1024 * 1024;
const PROFILES: [VectorRuntimeProfile; 2] = [
    VectorRuntimeProfile::Qwen3Embedding06bQ8,
    VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
];

fn number(value: u64) -> GgufValue {
    GgufValue::Scalar {
        type_id: 4,
        value: GgufScalar::Unsigned(value),
    }
}

fn text(value: &str) -> GgufValue {
    GgufValue::Scalar {
        type_id: 8,
        value: GgufScalar::String(value.into()),
    }
}

fn component(profile: VectorRuntimeProfile) -> ComponentObservation {
    // Synthetic observations bind measured public content identities and cache
    // dimensions; these fixtures do not claim to read or install the real models.
    let source = parse_modelscope_inventory(
        &serde_json::to_vec(&json!({"Code":200,"Data":{"Files":[{
            "Path":"model.gguf","Type":"blob","Size":profile.model_bytes(),
            "Sha256":profile.model_sha256(),
        }]}}))
        .unwrap(),
        RepositorySnapshot {
            repository: ModelScopeRepository::parse("unit/vector-resource-fixture").unwrap(),
            requested_revision: "a".repeat(40),
            resolved_revision: "a".repeat(40),
            files_digest: Sha256Digest::of_bytes(b"synthetic inventory"),
            fetched_at: 100,
        },
    )
    .unwrap();
    let artifact = source
        .artifact(
            &RepositoryPath::parse("model.gguf").unwrap(),
            FilePurpose::GgufWeight,
        )
        .unwrap();
    let architecture = if profile == VectorRuntimeProfile::Qwen3Embedding06bQ8 {
        "qwen3"
    } else {
        "bert"
    };
    let mut metadata = BTreeMap::from([("general.architecture".into(), text(architecture))]);
    if profile == VectorRuntimeProfile::Qwen3Embedding06bQ8 {
        for (key, value) in [
            ("block_count", 28),
            ("embedding_length", 1024),
            ("attention.head_count", 16),
            ("attention.head_count_kv", 8),
            ("context_length", 32768),
            ("attention.key_length", 128),
            ("attention.value_length", 128),
        ] {
            metadata.insert(format!("qwen3.{key}"), number(value));
        }
    }
    ComponentObservation {
        role: ComponentRole::Main,
        files: vec![SourceGgufObservation {
            artifact,
            snapshot: source.snapshot,
            header: GgufInspection {
                version: 3,
                byte_order: GgufByteOrder::LittleEndian,
                file_bytes: profile.model_bytes(),
                header_bytes: 128,
                header_digest: Sha256Digest::of_bytes(b"synthetic vector resource header"),
                data_offset: 128,
                alignment: 32,
                metadata,
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
            },
        }],
    }
}

fn hardware() -> HardwareResourceSnapshot {
    HardwareResourceSnapshot {
        observed_at: 100,
        devices: vec![HardwareMemoryDevice {
            index: 0,
            backend: "CPU".into(),
            name: "synthetic-unit-cpu".into(),
            description: "Deterministic memory bounds, not a hardware observation".into(),
            kind: "cpu".into(),
            total_bytes: 16 * 1024 * MIB,
            free_bytes: 8 * 1024 * MIB,
        }],
        storage_volume: "synthetic-unit-volume".into(),
        disk_free_bytes: 16 * 1024 * MIB,
    }
}

fn admission(profile: VectorRuntimeProfile) -> PlanAdmission {
    let components = [component(profile)];
    let envelope = baseline_vector_resource_envelope(profile, &components).unwrap();
    let hardware = hardware();
    let budget = assess_resource_budget(&components, &envelope, &hardware).unwrap();
    PlanAdmission {
        runtime_profile: baseline_vector_install_profile(profile).unwrap(),
        envelope,
        hardware,
        budget,
    }
}

fn rebind(envelope: &mut AdapterResourceEnvelope, components: &[ComponentObservation]) {
    envelope.component_digests = components
        .iter()
        .map(|component| component.digest().unwrap())
        .collect();
}

fn assert_unknown(components: &[ComponentObservation], envelope: &AdapterResourceEnvelope) {
    let budget = assess_resource_budget(components, envelope, &hardware()).unwrap();
    assert_eq!(budget.compatibility, HardwareCompatibility::Unknown);
    assert_eq!(budget.kv_cache_bytes, None);
    assert_eq!(budget.host_memory_required_bytes, None);
    assert!(!budget.blocking_reasons.is_empty());
}

#[test]
fn measured_profiles_charge_full_weights_and_distinct_cache_and_repack_allowances() {
    for (profile, kv, auxiliary, total) in [
        (PROFILES[0], 56 * MIB, 256 * MIB, 1_033_415_168),
        (PROFILES[1], 0, 512 * MIB, 1_042_356_640),
    ] {
        let admission = admission(profile);
        assert_eq!(validate_vector_admission(&admission).unwrap(), profile);
        let budget = &admission.budget;
        assert_eq!(budget.rules_version, VECTOR_RESOURCE_RULES_VERSION);
        assert_eq!(budget.compatibility, HardwareCompatibility::Runnable);
        assert_eq!(budget.weight_bytes, profile.model_bytes());
        assert_eq!(budget.kv_cache_bytes, Some(kv));
        assert_eq!(budget.recurrent_cache_bytes, Some(0));
        assert_eq!(budget.dynamic_cache_bytes, Some(0));
        assert_eq!(budget.compute_buffer_bytes, 64 * MIB);
        assert_eq!(budget.auxiliary_buffer_bytes, auxiliary);
        assert_eq!(budget.host_memory_required_bytes, Some(total));
        assert_eq!(
            budget.peak_incremental_disk_bytes,
            3 * profile.model_bytes() + 16 * MIB
        );
        assert_eq!(
            admission.runtime_profile.capabilities,
            BTreeSet::from([profile.capability()])
        );
        assert_eq!(
            admission.runtime_profile.runtime,
            baseline_pairing_runtime().unwrap()
        );
    }
}

#[test]
fn exact_memory_and_disk_boundaries_are_required_and_extra_headroom_is_charged() {
    for profile in PROFILES {
        let components = [component(profile)];
        let mut envelope = baseline_vector_resource_envelope(profile, &components).unwrap();
        let mut hardware = hardware();
        let original = assess_resource_budget(&components, &envelope, &hardware).unwrap();
        hardware.devices[0].free_bytes = original.host_memory_required_bytes.unwrap();
        hardware.disk_free_bytes = original.peak_incremental_disk_bytes;
        assert_eq!(
            assess_resource_budget(&components, &envelope, &hardware)
                .unwrap()
                .compatibility,
            HardwareCompatibility::Runnable
        );
        hardware.devices[0].free_bytes -= 1;
        assert_eq!(
            assess_resource_budget(&components, &envelope, &hardware)
                .unwrap()
                .compatibility,
            HardwareCompatibility::Incompatible
        );
        hardware.devices[0].free_bytes += 1;
        hardware.disk_free_bytes -= 1;
        assert_eq!(
            assess_resource_budget(&components, &envelope, &hardware)
                .unwrap()
                .compatibility,
            HardwareCompatibility::Incompatible
        );
        hardware.disk_free_bytes += 1;
        envelope.auxiliary_buffer_bytes += MIB;
        let increased = assess_resource_budget(&components, &envelope, &hardware).unwrap();
        assert_eq!(
            increased.host_memory_required_bytes,
            original.host_memory_required_bytes.map(|bytes| bytes + MIB)
        );
        assert_eq!(increased.compatibility, HardwareCompatibility::Incompatible);
        assert_ne!(increased.binding_digest, original.binding_digest);
        hardware.devices.clear();
        assert_eq!(
            assess_resource_budget(&components, &envelope, &hardware)
                .unwrap()
                .compatibility,
            HardwareCompatibility::Unknown
        );
    }
}

#[test]
fn frozen_vector_workload_matches_all_execution_and_input_limits() {
    for profile in PROFILES {
        let workload = profile.resource_workload();
        let vector = workload.vector.as_ref().unwrap();
        assert_eq!(
            validate_measured_vector_workload(&workload).unwrap(),
            profile
        );
        let RuntimeParameters::Specialized { settings, .. } = profile.launch_parameters() else {
            unreachable!()
        };
        let fields = [
            ("context_length", workload.context_tokens),
            ("batch_size", workload.batch_tokens),
            ("parallel_sequences", workload.parallel_sequences),
            ("microbatch_size", vector.microbatch_tokens),
            ("cpu_threads", vector.cpu_threads),
            ("batch_cpu_threads", vector.batch_cpu_threads),
            ("gpu_layers", vector.gpu_layers),
            ("cache_reuse", vector.cache_reuse_tokens),
            ("max_batch_items", vector.max_batch_items),
            ("max_input_bytes", vector.max_input_bytes),
            ("max_batch_bytes", vector.max_batch_bytes),
            ("max_input_tokens", vector.max_input_tokens),
        ];
        for (key, expected) in fields {
            assert_eq!(settings[key], expected.to_string(), "{key}");
        }
        for (key, expected) in [
            ("cache_prompt", vector.cache_prompt),
            ("cache_idle_slots", vector.cache_idle_slots),
            ("auto_fit", vector.auto_fit),
            ("warmup", vector.warmup),
        ] {
            assert_eq!(settings[key], expected.to_string(), "{key}");
        }
        assert!(vector.flash_attention);
        assert_eq!(settings["flash_attention"], "on");
        assert!(!vector.speculative_decoding);
        assert_eq!(settings["speculative_type"], "none");
        assert_eq!(settings["cache_type_k"], "f16");
        assert_eq!(settings["cache_type_v"], "f16");
        assert_eq!(settings["prompt_cache_mib"], "0");
        assert_eq!(settings["context_checkpoints"], "0");
    }
}

#[test]
fn every_vector_parameter_change_is_unmeasured_instead_of_using_a_generic_fallback() {
    for profile in PROFILES {
        let components = [component(profile)];
        let baseline = baseline_vector_resource_envelope(profile, &components).unwrap();
        let serialized = serde_json::to_value(baseline.workload.vector.as_ref().unwrap()).unwrap();
        for (key, value) in serialized.as_object().unwrap() {
            let mut changed = serialized.clone();
            changed[key] = match value {
                Value::Bool(value) => json!(!*value),
                Value::Number(value) => json!(value.as_u64().unwrap() + 1),
                Value::String(_) if key == "profile" => {
                    serde_json::to_value(if profile == PROFILES[0] {
                        PROFILES[1]
                    } else {
                        PROFILES[0]
                    })
                    .unwrap()
                }
                _ => panic!("uncovered vector workload field {key}"),
            };
            let mut envelope = baseline.clone();
            envelope.workload.vector = Some(serde_json::from_value(changed).unwrap());
            assert_unknown(&components, &envelope);
        }
        let mutations: &[fn(&mut AdapterResourceEnvelope)] = &[
            |e| e.runtime.binary_sha256 = Sha256Digest::of_bytes(b"other build"),
            |e| e.runtime.engine_revision = "other".into(),
            |e| e.runtime.platform = "other".into(),
            |e| e.runtime.features.clear(),
            |e| e.runtime.binding_version = "other".into(),
            |e| e.evidence_id = "other".into(),
            |e| e.compute_buffer_bytes -= 1,
            |e| e.auxiliary_buffer_bytes -= 1,
            |e| e.workload.context_tokens *= 2,
            |e| e.workload.batch_tokens /= 2,
            |e| e.workload.parallel_sequences = 2,
            |e| e.workload.recurrent_rollback_slots = 1,
            |e| e.workload.kv_key_type = ResourceCacheType::F32,
            |e| e.workload.kv_value_type = ResourceCacheType::F32,
            |e| e.workload.prompt_cache_bytes = MIB,
            |e| e.workload.context_checkpoint_slots = 1,
            |e| e.workload.vision = Some(VisionResourceWorkload::measured_qwen35_cpu()),
        ];
        for mutate in mutations {
            let mut changed = baseline.clone();
            mutate(&mut changed);
            assert_unknown(&components, &changed);
        }
    }
}

#[test]
fn content_role_size_architecture_and_decoder_cache_metadata_cannot_escape_the_profile() {
    for profile in PROFILES {
        let original = component(profile);
        let baseline =
            baseline_vector_resource_envelope(profile, std::slice::from_ref(&original)).unwrap();
        let mutations: &[fn(&mut ComponentObservation)] = &[
            |c| {
                c.files[0].artifact.source_sha256 =
                    Some(Sha256Digest::of_bytes(b"unmeasured model"))
            },
            |c| c.files[0].artifact.source_sha256 = None,
            |c| c.role = ComponentRole::Dflash,
            |c| {
                c.files[0].header.file_bytes += 32;
                c.files[0].artifact.size_bytes = Some(c.files[0].header.file_bytes);
            },
            |c| {
                c.files[0]
                    .header
                    .metadata
                    .insert("general.architecture".into(), text("unknown-encoder"));
            },
        ];
        for mutate in mutations {
            let mut changed = original.clone();
            mutate(&mut changed);
            let mut envelope = baseline.clone();
            rebind(&mut envelope, std::slice::from_ref(&changed));
            assert!(
                baseline_vector_resource_envelope(profile, std::slice::from_ref(&changed)).is_err()
            );
            assert_unknown(&[changed], &envelope);
        }
        let other = if profile == PROFILES[0] {
            PROFILES[1]
        } else {
            PROFILES[0]
        };
        assert!(baseline_vector_resource_envelope(other, &[original]).is_err());
    }
    let mut changed = component(PROFILES[0]);
    let mut envelope =
        baseline_vector_resource_envelope(PROFILES[0], std::slice::from_ref(&changed)).unwrap();
    changed.files[0]
        .header
        .metadata
        .insert("qwen3.attention.key_length".into(), number(64));
    rebind(&mut envelope, std::slice::from_ref(&changed));
    assert_unknown(&[changed], &envelope);
}

#[test]
fn zero_encoder_cache_is_not_available_without_the_measured_vector_profile() {
    let component = component(PROFILES[1]);
    let mut envelope =
        baseline_vector_resource_envelope(PROFILES[1], std::slice::from_ref(&component)).unwrap();
    envelope.workload.vector = None;
    assert_unknown(std::slice::from_ref(&component), &envelope);
    let mut unknown = component;
    unknown.files[0]
        .header
        .metadata
        .insert("general.architecture".into(), text("future-encoder"));
    rebind(&mut envelope, std::slice::from_ref(&unknown));
    assert_unknown(&[unknown], &envelope);
}

#[test]
fn frozen_admission_rejects_budget_profile_and_hardware_tampering_even_with_a_new_digest() {
    let mutations: &[fn(&mut PlanAdmission)] = &[
        |a| a.runtime_profile.id = "different profile".into(),
        |a| {
            a.runtime_profile.capabilities.insert(ModelCapability::Text);
        },
        |a| a.envelope.component_digests.clear(),
        |a| a.envelope.compute_buffer_bytes -= 1,
        |a| a.envelope.auxiliary_buffer_bytes -= 1,
        |a| a.envelope.workload.vector.as_mut().unwrap().max_batch_items = 32,
        |a| a.budget.rules_version = RESOURCE_RULES_VERSION.into(),
        |a| a.budget.compatibility = HardwareCompatibility::Limited,
        |a| a.budget.blocking_reasons.push("unverified".into()),
        |a| a.budget.weight_bytes -= 1,
        |a| a.budget.kv_cache_bytes = Some(1),
        |a| a.budget.recurrent_cache_bytes = Some(1),
        |a| a.budget.dynamic_cache_bytes = Some(1),
        |a| a.budget.compute_buffer_bytes -= 1,
        |a| a.budget.auxiliary_buffer_bytes -= 1,
        |a| a.budget.host_memory_required_bytes = Some(1),
        |a| a.budget.host_memory_available_bytes = Some(u64::MAX),
        |a| a.budget.peak_incremental_disk_bytes -= 1,
        |a| a.hardware.devices[0].free_bytes = 1,
        |a| a.hardware.disk_free_bytes = 1,
    ];
    for profile in PROFILES {
        let original = admission(profile);
        for mutate in mutations {
            let mut changed = original.clone();
            mutate(&mut changed);
            changed.budget.binding_digest = Sha256Digest::of_serializable(&(
                VECTOR_RESOURCE_RULES_VERSION,
                &changed.envelope,
                &changed.hardware,
            ))
            .unwrap();
            assert!(validate_vector_admission(&changed).is_err());
        }
        let mut changed = original;
        changed.budget.binding_digest = Sha256Digest::of_bytes(b"stale");
        assert!(validate_vector_admission(&changed).is_err());
    }
}

#[test]
fn existing_frozen_text_plan_serialization_and_digest_remain_identical() {
    let fixture: Value = serde_json::from_str(include_str!(
        "fixtures/lifecycle-vision/frozen-text-v2-plan.json"
    ))
    .unwrap();
    let plan: InstallPlan = serde_json::from_value(fixture["plan"].clone()).unwrap();
    assert!(plan
        .admission
        .as_ref()
        .unwrap()
        .envelope
        .workload
        .vector
        .is_none());
    assert_eq!(serde_json::to_value(&plan).unwrap(), fixture["plan"]);
    assert_eq!(
        plan.digest().unwrap().as_str(),
        fixture["expected_plan_digest"].as_str().unwrap()
    );
}

fn catalog(component: &ComponentObservation) -> ModelCatalogSnapshot {
    let file = &component.files[0];
    ModelCatalogSnapshot {
        model: ModelDefinition {
            id: "unit-vector-model".into(),
            display_name: "Synthetic vector resource fixture".into(),
            snapshot_ids: BTreeSet::from([file.artifact.snapshot_id.clone()]),
            architecture: ArchitectureKnowledge::Unknown,
            structure: ModelStructure::Unknown,
            task_families: BTreeSet::from([ModelTaskFamily::Unknown]),
            declared_capabilities: BTreeSet::new(),
        },
        snapshots: BTreeMap::from([(file.artifact.snapshot_id.clone(), file.snapshot.clone())]),
        artifacts: BTreeMap::from([(file.artifact.id.clone(), file.artifact.clone())]),
        variants: BTreeMap::from([(
            "unit-variant".into(),
            ModelVariant {
                id: "unit-variant".into(),
                model_id: "unit-vector-model".into(),
                artifact_ids: BTreeSet::from([file.artifact.id.clone()]),
                quantization_label: "unit".into(),
                tensor_types: BTreeSet::from([0]),
                evidence_ids: BTreeSet::from(["synthetic-fixture".into()]),
            },
        )]),
        components: BTreeMap::from([(
            "unit-main".into(),
            Component {
                id: "unit-main".into(),
                role: ComponentRole::Main,
                artifact_ids: BTreeSet::from([file.artifact.id.clone()]),
                compatibility: ComponentCompatibility::Verified,
                evidence_ids: BTreeSet::from(["synthetic-fixture".into()]),
            },
        )]),
    }
}

#[test]
fn production_profiles_prepare_and_persist_only_resource_admissible_vector_plans() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/vector-resource-tests");
    std::fs::create_dir_all(&root).unwrap();
    for profile in PROFILES {
        let temp = tempfile::tempdir_in(&root).unwrap();
        let repo = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
        let components = [component(profile)];
        let catalog = catalog(&components[0]);
        repo.save_catalog(&catalog).unwrap();
        repo.save_gguf_observation(&components[0].files[0], 100)
            .unwrap();
        let request = ObservedPlanRequest {
            id: "unit-vector-plan".into(),
            model_id: catalog.model.id.clone(),
            variant_id: "unit-variant".into(),
            target_capabilities: BTreeSet::from([profile.capability()]),
            dependencies: DependencyGraph {
                root: "unit-main".into(),
                components: catalog.components.clone(),
                edges: vec![],
            },
        };
        let runtime = baseline_vector_install_profile(profile).unwrap();
        let envelope = baseline_vector_resource_envelope(profile, &components).unwrap();
        let assessment = repo
            .prepare_observed_plan(&request, &runtime, &envelope, &hardware(), 100)
            .unwrap();
        assert!(
            assessment.blocking_reasons.is_empty(),
            "{:?}",
            assessment.blocking_reasons
        );
        let plan = assessment.plan.unwrap();
        assert_eq!(
            validate_vector_admission(plan.admission.as_ref().unwrap()).unwrap(),
            profile
        );
        assert_eq!(repo.read_plan(&plan.id).unwrap(), Some(plan.clone()));
        for fail in ["memory", "disk", "workload", "content"] {
            let mut request = request.clone();
            request.id = format!("blocked-{fail}");
            let mut hardware = hardware();
            let mut envelope = envelope.clone();
            match fail {
                "memory" => hardware.devices[0].free_bytes = 1,
                "disk" => hardware.disk_free_bytes = 1,
                "workload" => envelope.workload.vector.as_mut().unwrap().cache_prompt = true,
                "content" => {
                    envelope.workload.vector.as_mut().unwrap().profile = if profile == PROFILES[0] {
                        PROFILES[1]
                    } else {
                        PROFILES[0]
                    }
                }
                _ => unreachable!(),
            }
            let blocked = repo
                .prepare_observed_plan(&request, &runtime, &envelope, &hardware, 100)
                .unwrap();
            assert!(blocked.plan.is_none(), "{fail}");
            assert!(!blocked.blocking_reasons.is_empty(), "{fail}");
            assert!(repo.read_plan(&request.id).unwrap().is_none());
        }
        assert_eq!(repo.read_plan(&plan.id).unwrap(), Some(plan));
    }
}
