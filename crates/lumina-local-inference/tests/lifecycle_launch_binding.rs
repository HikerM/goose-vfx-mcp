use lumina_local_inference::lifecycle::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const TEXT_FIXTURE: &str = include_str!("fixtures/lifecycle-vision/frozen-text-v2-plan.json");

fn manifest(plan: InstallPlan, accepted_at: i64) -> ModelManifest {
    let installed_files = plan
        .artifacts
        .iter()
        .map(|(id, artifact)| {
            (
                id.clone(),
                InstalledArtifact {
                    artifact_id: id.clone(),
                    relative_path: installed_artifact_path(id, artifact.purpose, &artifact.path)
                        .unwrap(),
                    computed_sha256: artifact.source_sha256.clone().unwrap(),
                    size_bytes: artifact.size_bytes.unwrap(),
                    verification_evidence: Sha256Digest::of_bytes(
                        b"unit fixture, no physical installation",
                    ),
                },
            )
        })
        .collect();
    let manifest = ModelManifest {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        installation_id: "launch-test-installation".into(),
        version_id: "launch-test-version".into(),
        plan,
        installed_files,
        plan_accepted_at: accepted_at,
        installed_at: accepted_at,
    };
    manifest.validate().unwrap();
    manifest
}

fn text_manifest() -> ModelManifest {
    let fixture: Value = serde_json::from_str(TEXT_FIXTURE).unwrap();
    manifest(
        serde_json::from_value(fixture["plan"].clone()).unwrap(),
        fixture["accepted_at"].as_i64().unwrap(),
    )
}

fn vision_manifest() -> ModelManifest {
    let recorded = |input: &str| {
        let value: Value = serde_json::from_str(input).unwrap();
        serde_json::from_value::<ComponentObservation>(value["observation"].clone()).unwrap()
    };
    let main = recorded(include_str!(
        "fixtures/lifecycle-vision/main-observation.json"
    ));
    let mut projector = recorded(include_str!(
        "fixtures/lifecycle-vision/projector-observation.json"
    ));
    projector.role = ComponentRole::VisionProjector;
    let observations = [main, projector];
    let envelope = baseline_vision_resource_envelope(&observations).unwrap();
    let profile = baseline_pairing_profiles()
        .unwrap()
        .into_iter()
        .find(|profile| profile.id == "qwen35-08b-vision-content-baseline")
        .unwrap();
    let pairing = assess_component_pairing(
        &observations[0],
        &observations[1],
        &envelope.runtime,
        &profile,
    )
    .unwrap();
    assert_eq!(pairing.compatibility, ComponentCompatibility::Verified);
    let mut snapshots = BTreeMap::new();
    let mut artifacts = BTreeMap::new();
    let mut components = BTreeMap::new();
    for (id, observation) in ["main", "projector"].into_iter().zip(&observations) {
        for file in &observation.files {
            snapshots.insert(file.artifact.snapshot_id.clone(), file.snapshot.clone());
            artifacts.insert(file.artifact.id.clone(), file.artifact.clone());
        }
        components.insert(
            id.to_string(),
            Component {
                id: id.into(),
                role: observation.role,
                artifact_ids: observation
                    .files
                    .iter()
                    .map(|file| file.artifact.id.clone())
                    .collect(),
                compatibility: pairing.compatibility,
                evidence_ids: BTreeSet::from([format!(
                    "component-pairing:{}",
                    pairing.digest().unwrap().as_str()
                )]),
            },
        );
    }
    let primary = &observations[0].files[0];
    let classification = classify_gguf(
        &primary.artifact,
        &primary.artifact.snapshot_id,
        &primary.snapshot,
        &primary.header,
        &[],
    )
    .unwrap();
    let catalog = ModelCatalogSnapshot {
        model: ModelDefinition {
            id: "vision-model".into(),
            display_name: "Recorded vision pairing".into(),
            snapshot_ids: snapshots.keys().cloned().collect(),
            architecture: classification.architecture,
            structure: classification.structure,
            task_families: classification.task_candidates,
            declared_capabilities: BTreeSet::new(),
        },
        snapshots,
        artifacts,
        components: components.clone(),
        variants: BTreeMap::from([(
            "vision-variant".into(),
            ModelVariant {
                id: "vision-variant".into(),
                model_id: "vision-model".into(),
                artifact_ids: observations[0]
                    .files
                    .iter()
                    .map(|file| file.artifact.id.clone())
                    .collect(),
                quantization_label: "Q8_0".into(),
                tensor_types: primary
                    .header
                    .tensors
                    .iter()
                    .map(|tensor| tensor.type_id)
                    .collect(),
                evidence_ids: BTreeSet::from([VISION_RESOURCE_EVIDENCE.into()]),
            },
        )]),
    };
    let profile = RuntimeInstallProfile {
        id: "measured-qwen35-08b-vision-cpu".into(),
        rules_version: RUNTIME_INSTALL_PROFILE_VERSION.into(),
        runtime: envelope.runtime.clone(),
        architectures: BTreeSet::from(["qwen35".into()]),
        capabilities: BTreeSet::from([ModelCapability::Text, ModelCapability::Vision]),
        required_roles: BTreeSet::from([ComponentRole::Main, ComponentRole::VisionProjector]),
        evidence_ids: BTreeSet::from([VISION_RESOURCE_EVIDENCE.into()]),
    };
    let request = ObservedPlanRequest {
        id: "vision-plan".into(),
        model_id: catalog.model.id.clone(),
        variant_id: "vision-variant".into(),
        target_capabilities: profile.capabilities.clone(),
        dependencies: DependencyGraph {
            root: "main".into(),
            components,
            edges: vec![DependencyEdge {
                from: "main".into(),
                to: "projector".into(),
                requirement: DependencyRequirement::Required,
            }],
        },
    };
    let temp = tempfile::tempdir().unwrap();
    let repository = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    repository.save_catalog(&catalog).unwrap();
    let hardware = text_manifest().plan.admission.unwrap().hardware;
    let now = hardware.observed_at;
    for component in observations {
        for file in component.files {
            repository.save_gguf_observation(&file, now).unwrap();
        }
    }
    let assessment = repository
        .prepare_observed_plan(&request, &profile, &envelope, &hardware, now)
        .unwrap();
    assert!(
        assessment.blocking_reasons.is_empty(),
        "{:?}",
        assessment.blocking_reasons
    );
    let plan = assessment.plan.unwrap();
    assert_eq!(repository.read_plan(&plan.id).unwrap(), Some(plan.clone()));
    manifest(plan, now)
}

fn launch(manifest: &ModelManifest) -> LaunchConfig {
    let workload = manifest
        .plan
        .admission
        .as_ref()
        .unwrap()
        .envelope
        .workload
        .clone();
    let mut component_files = BTreeMap::<_, Vec<_>>::new();
    for component in manifest.plan.dependencies.components.values() {
        component_files.entry(component.role).or_default().extend(
            component
                .artifact_ids
                .iter()
                .map(|id| manifest.installed_files[id].relative_path.clone()),
        );
    }
    LaunchConfig {
        installation_id: manifest.installation_id.clone(),
        version_id: manifest.version_id.clone(),
        manifest_digest: manifest.digest().unwrap(),
        runtime: manifest.plan.runtime.clone(),
        capabilities: manifest.plan.target_capabilities.clone(),
        component_files,
        parameters: RuntimeParameters::LlamaCpp {
            chat_template: LaunchChatTemplate::Embedded,
            context_length: workload.context_tokens,
            gpu_layers: 0,
            cpu_threads: 4,
            kv_key_type: "f16".into(),
            kv_value_type: "f16".into(),
            batch_size: workload.batch_tokens,
            flash_attention: true,
            workload: Some(workload),
        },
        mode: RuntimeExecutionMode::Helper { port: 32123 },
    }
}

fn edit_launch(original: &LaunchConfig, edit: impl FnOnce(&mut Value)) -> LaunchConfig {
    let mut value = serde_json::to_value(original).unwrap();
    edit(&mut value);
    serde_json::from_value(value).unwrap()
}

fn reseal_admission(manifest: &mut ModelManifest) {
    let admission = manifest.plan.admission.as_mut().unwrap();
    admission.budget.binding_digest = Sha256Digest::of_serializable(&(
        resource_rules_version(&admission.envelope.workload),
        &admission.envelope,
        &admission.hardware,
    ))
    .unwrap();
    let profile_digest = Sha256Digest::of_serializable(&admission.runtime_profile).unwrap();
    let root = manifest
        .plan
        .dependencies
        .components
        .get_mut(&manifest.plan.dependencies.root)
        .unwrap();
    root.evidence_ids.retain(|id| {
        !id.starts_with("runtime-install-profile:") && !id.starts_with("resource-budget:")
    });
    root.evidence_ids.extend([
        format!("runtime-install-profile:{}", profile_digest.as_str()),
        format!(
            "resource-budget:{}",
            admission.budget.binding_digest.as_str()
        ),
    ]);
    manifest.validate().unwrap();
}

#[test]
fn recorded_text_v2_digest_and_complete_launch_binding_remain_compatible() {
    let manifest = text_manifest();
    let fixture: Value = serde_json::from_str(TEXT_FIXTURE).unwrap();
    assert_eq!(
        manifest.plan.digest().unwrap().as_str(),
        fixture["expected_plan_digest"].as_str().unwrap()
    );
    let original = launch(&manifest);
    original.validate_binding(&manifest).unwrap();
    let decoded: LaunchConfig =
        serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
    assert_eq!(decoded.digest().unwrap(), original.digest().unwrap());
    decoded.validate_binding(&manifest).unwrap();
}

#[test]
fn paired_vision_plan_prepared_through_repository_accepts_exact_cpu_helper_launch() {
    let manifest = vision_manifest();
    assert_eq!(manifest.installed_files.len(), 2);
    assert_eq!(
        manifest
            .plan
            .admission
            .as_ref()
            .unwrap()
            .budget
            .rules_version,
        VISION_RESOURCE_RULES_VERSION
    );
    launch(&manifest).validate_binding(&manifest).unwrap();
}

#[test]
fn context_batch_precision_threads_and_gpu_cannot_drift_after_admission() {
    for manifest in [text_manifest(), vision_manifest()] {
        let original = launch(&manifest);
        for (key, value) in [
            ("context_length", json!(8192)),
            ("context_length", json!(0)),
            ("batch_size", json!(1024)),
            ("kv_key_type", json!("f32")),
            ("kv_value_type", json!("q8_0")),
            ("cpu_threads", json!(8)),
            ("cpu_threads", json!(0)),
            ("gpu_layers", json!(1)),
            ("flash_attention", json!(false)),
        ] {
            let changed = edit_launch(&original, |launch| launch["parameters"][key] = value);
            assert!(
                changed.validate_binding(&manifest).is_err(),
                "accepted changed {key}"
            );
            assert_ne!(changed.digest().unwrap(), original.digest().unwrap());
        }
    }
}

#[test]
fn complete_workload_cannot_be_omitted_or_increase_caches_parallelism_and_rollback() {
    let manifest = text_manifest();
    let original = launch(&manifest);
    let missing = edit_launch(&original, |launch| {
        launch["parameters"]
            .as_object_mut()
            .unwrap()
            .remove("workload");
    });
    assert!(missing
        .validate_binding(&manifest)
        .unwrap_err()
        .to_string()
        .contains("complete frozen"));
    for (key, value) in [
        ("parallel_sequences", json!(2)),
        ("prompt_cache_bytes", json!(8_589_934_592u64)),
        ("context_checkpoint_slots", json!(32)),
        ("recurrent_rollback_slots", json!(1)),
    ] {
        let changed = edit_launch(&original, |launch| {
            launch["parameters"]["workload"][key] = value
        });
        assert!(changed
            .validate_binding(&manifest)
            .unwrap_err()
            .to_string()
            .contains("differs from its frozen admission"));
    }
}

#[test]
fn vision_input_and_encoder_execution_limits_are_bound_even_for_text_only_requests() {
    let manifest = vision_manifest();
    let mut original = launch(&manifest);
    original.capabilities = BTreeSet::from([ModelCapability::Text]);
    original.validate_binding(&manifest).unwrap();
    for (key, value) in [
        ("max_images", json!(2)),
        ("max_input_width", json!(2048)),
        ("max_input_height", json!(2048)),
        ("max_input_bytes", json!(16_777_216)),
        ("native_min_image_tokens", json!(64)),
        ("native_max_image_tokens", json!(4096)),
        ("max_encoded_image_tokens", json!(4096)),
        ("microbatch_tokens", json!(1024)),
        ("image_batch_tokens", json!(2048)),
        ("cpu_threads", json!(8)),
    ] {
        let changed = edit_launch(&original, |launch| {
            launch["parameters"]["workload"]["vision"][key] = value
        });
        assert!(
            changed.validate_binding(&manifest).is_err(),
            "accepted changed vision {key}"
        );
    }
    let stripped = edit_launch(&original, |launch| {
        launch["parameters"]["workload"]
            .as_object_mut()
            .unwrap()
            .remove("vision");
    });
    assert!(stripped.validate_binding(&manifest).is_err());
}

#[test]
fn admitted_launch_cannot_switch_execution_mode_or_use_an_unconstrained_adapter() {
    for manifest in [text_manifest(), vision_manifest()] {
        let original = launch(&manifest);
        for mode in [
            RuntimeExecutionMode::InProcess,
            RuntimeExecutionMode::Helper { port: 0 },
        ] {
            let mut changed = original.clone();
            changed.mode = mode;
            assert!(changed
                .validate_binding(&manifest)
                .unwrap_err()
                .to_string()
                .contains("native CPU helper"));
        }
        for parameters in [
            RuntimeParameters::Whisper {
                cpu_threads: 4,
                use_gpu: false,
            },
            RuntimeParameters::Audio {
                cpu_threads: 4,
                use_gpu: false,
            },
            RuntimeParameters::Diffusion {
                cpu_threads: 4,
                use_gpu: false,
                max_width: 512,
                max_height: 512,
                max_frames: 1,
            },
            RuntimeParameters::Specialized {
                schema_id: "llama-helper".into(),
                settings: BTreeMap::new(),
            },
        ] {
            let mut changed = original.clone();
            changed.parameters = parameters;
            assert!(changed
                .validate_binding(&manifest)
                .unwrap_err()
                .to_string()
                .contains("cannot enforce"));
        }
    }
}

#[test]
fn self_consistent_manifest_cannot_relabel_an_unmeasured_runtime_or_resource_profile() {
    let original = vision_manifest();
    for mutate in 0..3 {
        let mut changed = original.clone();
        match mutate {
            0 => changed.plan.runtime.adapter_id = "llama-inprocess".into(),
            1 => changed.plan.runtime.binary_sha256 = Sha256Digest::of_bytes(b"another binary"),
            _ => {
                changed.plan.runtime.features.insert("cuda".into());
            }
        }
        let admission = changed.plan.admission.as_mut().unwrap();
        admission.runtime_profile.runtime = changed.plan.runtime.clone();
        admission.envelope.runtime = changed.plan.runtime.clone();
        reseal_admission(&mut changed);
        assert!(launch(&changed)
            .validate_binding(&changed)
            .unwrap_err()
            .to_string()
            .contains("native CPU helper"));
    }
    let mut changed = text_manifest();
    changed
        .plan
        .admission
        .as_mut()
        .unwrap()
        .envelope
        .evidence_id = "diagnostic-unmeasured".into();
    reseal_admission(&mut changed);
    assert!(launch(&changed)
        .validate_binding(&changed)
        .unwrap_err()
        .to_string()
        .contains("matching measured"));
}

#[test]
fn copied_unmeasured_workload_and_speculative_components_are_not_a_launch_approval() {
    let mut manifest = text_manifest();
    manifest
        .plan
        .admission
        .as_mut()
        .unwrap()
        .envelope
        .workload
        .prompt_cache_bytes = 8_589_934_592;
    reseal_admission(&mut manifest);
    assert!(launch(&manifest)
        .validate_binding(&manifest)
        .unwrap_err()
        .to_string()
        .contains("cache or speculative"));

    let mut manifest = text_manifest();
    let mut mtp = manifest.plan.dependencies.components[&manifest.plan.dependencies.root].clone();
    mtp.id = "unapproved-mtp".into();
    mtp.role = ComponentRole::Mtp;
    manifest.plan.dependencies.edges.push(DependencyEdge {
        from: manifest.plan.dependencies.root.clone(),
        to: mtp.id.clone(),
        requirement: DependencyRequirement::Required,
    });
    manifest
        .plan
        .dependencies
        .components
        .insert(mtp.id.clone(), mtp);
    manifest.validate().unwrap();
    assert!(launch(&manifest)
        .validate_binding(&manifest)
        .unwrap_err()
        .to_string()
        .contains("speculative components"));
}

#[test]
fn old_text_weights_cannot_be_relabelled_as_the_measured_four_billion_parameter_model() {
    let mut manifest = text_manifest();
    let admission = manifest.plan.admission.as_mut().unwrap();
    let measurement =
        "cpu-text-measurement-v1:78776f846ad80bf0d548ba1219d6c253217b42010a7d66a077359d63582ffd24";
    admission.runtime_profile.id = "measured-qwen35-4b-q4km-text-cpu".into();
    admission
        .runtime_profile
        .evidence_ids
        .insert(measurement.into());
    admission.envelope.evidence_id = measurement.into();
    admission.envelope.compute_buffer_bytes = 128 * 1024 * 1024;
    admission.envelope.auxiliary_buffer_bytes = 2560 * 1024 * 1024;
    reseal_admission(&mut manifest);
    let error = launch(&manifest).validate_binding(&manifest).unwrap_err();
    assert!(error
        .to_string()
        .contains("no matching measured CPU profile"));
}

#[test]
fn historical_contract_without_admission_retains_identity_only_binding() {
    let mut manifest = text_manifest();
    let mut original = launch(&manifest);
    manifest.plan.admission = None;
    manifest.plan.rules_version = "historical-test-plan".into();
    manifest.validate().unwrap();
    original.manifest_digest = manifest.digest().unwrap();
    original.mode = RuntimeExecutionMode::InProcess;
    let legacy_parameters = edit_launch(&original, |launch| {
        launch["parameters"]
            .as_object_mut()
            .unwrap()
            .remove("workload");
    });
    legacy_parameters.validate_binding(&manifest).unwrap();
    assert!(
        serde_json::to_value(&legacy_parameters).unwrap()["parameters"]
            .get("workload")
            .is_none()
    );
    original.parameters = RuntimeParameters::Specialized {
        schema_id: "historical-test-adapter".into(),
        settings: BTreeMap::new(),
    };
    original.validate_binding(&manifest).unwrap();
    original.version_id = "another-version".into();
    assert!(original.validate_binding(&manifest).is_err());
}
