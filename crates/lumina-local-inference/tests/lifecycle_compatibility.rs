use lumina_local_inference::lifecycle::*;
use std::collections::{BTreeMap, BTreeSet};

fn scalar(value: GgufScalar) -> GgufValue {
    GgufValue::Scalar { type_id: 0, value }
}
fn unsigned(value: u64) -> GgufValue {
    scalar(GgufScalar::Unsigned(value))
}
fn text(value: &str) -> GgufValue {
    scalar(GgufScalar::String(value.into()))
}

fn component(
    role: ComponentRole,
    repository: &str,
    architecture: &str,
    content: &str,
) -> ComponentObservation {
    let snapshot = RepositorySnapshot {
        repository: ModelScopeRepository::parse(repository).unwrap(),
        requested_revision: "master".into(),
        resolved_revision: "a".repeat(40),
        files_digest: Sha256Digest::of_bytes(b"unit-only source inventory"),
        fetched_at: 10,
    };
    let artifact = Artifact {
        id: format!("unit-{content}"),
        snapshot_id: snapshot.content_digest().unwrap().as_str().into(),
        path: RepositoryPath::parse("weights.gguf").unwrap(),
        purpose: FilePurpose::GgufWeight,
        size_bytes: Some(1024),
        source_sha256: Some(Sha256Digest::of_bytes(content.as_bytes())),
    };
    let header = GgufInspection {
        version: 3,
        byte_order: GgufByteOrder::LittleEndian,
        file_bytes: 1024,
        header_bytes: 128,
        header_digest: Sha256Digest::of_bytes(format!("unit-header-{architecture}").as_bytes()),
        data_offset: 128,
        alignment: 32,
        metadata: BTreeMap::from([("general.architecture".into(), text(architecture))]),
        tensors: vec![GgufTensor {
            name: "weight".into(),
            dimensions: vec![4, 4],
            type_id: 0,
            quantization: "F32".into(),
            relative_offset: 0,
            encoded_bytes: 64,
            elements: 16,
        }],
        stored_tensor_elements: 16,
        encoded_tensor_bytes: 64,
        shard: None,
    };
    ComponentObservation {
        role,
        files: vec![SourceGgufObservation {
            artifact,
            snapshot,
            header,
        }],
    }
}

fn runtime() -> RuntimeBuild {
    RuntimeBuild {
        adapter_id: "unit-only-adapter".into(),
        engine_revision: "unit-engine".into(),
        binding_version: "unit-binding".into(),
        platform: "unit-platform".into(),
        features: BTreeSet::from(["cpu".into()]),
        binary_sha256: Sha256Digest::of_bytes(b"unit-only executable identity"),
    }
}

fn profile(
    main: &ComponentObservation,
    dependency: &ComponentObservation,
) -> ComponentPairingProfile {
    ComponentPairingProfile {
        id: "unit-profile".into(),
        version: "1".into(),
        runtime: runtime(),
        dependency_role: dependency.role,
        main_content: main
            .files
            .iter()
            .map(|file| file.artifact.source_sha256.clone().unwrap())
            .collect(),
        dependency_content: dependency
            .files
            .iter()
            .map(|file| file.artifact.source_sha256.clone().unwrap())
            .collect(),
        constraints: vec![
            PairingConstraint::Text {
                side: PairingSide::Main,
                key: "general.architecture".into(),
                value: main.files[0].header.architecture().unwrap().into(),
            },
            PairingConstraint::Text {
                side: PairingSide::Dependency,
                key: "general.architecture".into(),
                value: dependency.files[0].header.architecture().unwrap().into(),
            },
        ],
        evidence_ids: BTreeSet::from(["unit-fixture-not-runtime-evidence".into()]),
    }
}

#[test]
fn dimensions_missing_facts_and_wrong_roles_have_distinct_results() {
    let mut main = component(ComponentRole::Main, "unit/main", "qwen35", "main");
    let mut dependency = component(
        ComponentRole::VisionProjector,
        "unit/projector",
        "clip",
        "vision",
    );
    main.files[0]
        .header
        .metadata
        .insert("qwen35.embedding_length".into(), unsigned(1024));
    dependency.files[0]
        .header
        .metadata
        .insert("clip.vision.projection_dim".into(), unsigned(1024));
    let mut profile = profile(&main, &dependency);
    profile.constraints.push(PairingConstraint::UnsignedEqual {
        left_side: PairingSide::Main,
        left_key: "qwen35.embedding_length".into(),
        right_side: PairingSide::Dependency,
        right_key: "clip.vision.projection_dim".into(),
    });
    assert_eq!(
        assess_component_pairing(&main, &dependency, &runtime(), &profile)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Verified
    );
    dependency.files[0]
        .header
        .metadata
        .remove("clip.vision.projection_dim");
    assert_eq!(
        assess_component_pairing(&main, &dependency, &runtime(), &profile)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Unverified
    );
    dependency.files[0]
        .header
        .metadata
        .insert("clip.vision.projection_dim".into(), unsigned(2048));
    assert_eq!(
        assess_component_pairing(&main, &dependency, &runtime(), &profile)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Incompatible
    );
    dependency.role = ComponentRole::Main;
    assert!(
        assess_component_pairing(&main, &dependency, &runtime(), &profile)
            .unwrap()
            .issues
            .contains(&PairingIssue::RoleMismatch)
    );
}

#[test]
fn source_content_and_runtime_proofs_are_required_beyond_compatible_shapes() {
    let main = component(ComponentRole::Main, "unit/main", "qwen35", "main");
    let mut dependency = component(
        ComponentRole::VisionProjector,
        "unit/projector",
        "clip",
        "vision",
    );
    let profile = profile(&main, &dependency);
    let mut other_runtime = runtime();
    other_runtime
        .features
        .insert("unverified-build-feature".into());
    assert_eq!(
        assess_component_pairing(&main, &dependency, &other_runtime, &profile)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Unverified
    );
    dependency.files[0].artifact.source_sha256 = None;
    assert_eq!(
        assess_component_pairing(&main, &dependency, &runtime(), &profile)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Unverified
    );
    dependency.files[0].artifact.source_sha256 =
        Some(Sha256Digest::of_bytes(b"different unverified content"));
    assert_eq!(
        assess_component_pairing(&main, &dependency, &runtime(), &profile)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Unverified
    );
}

#[test]
fn identical_verified_content_can_come_from_another_pinned_modelscope_repository() {
    let main = component(ComponentRole::Main, "unit/main", "qwen35", "main");
    let original = component(
        ComponentRole::VisionProjector,
        "unit/projector",
        "clip",
        "vision",
    );
    let relocated = component(
        ComponentRole::VisionProjector,
        "unit/other-repository",
        "clip",
        "vision",
    );
    let profile = profile(&main, &original);
    let first = assess_component_pairing(&main, &original, &runtime(), &profile).unwrap();
    let other = assess_component_pairing(&main, &relocated, &runtime(), &profile).unwrap();
    assert_eq!(other.compatibility, ComponentCompatibility::Verified);
    assert_ne!(
        first.dependency_observation_digest,
        other.dependency_observation_digest
    );
    assert_ne!(first.digest().unwrap(), other.digest().unwrap());
}

fn tokenizer(header: &mut GgufInspection) {
    header
        .metadata
        .insert("tokenizer.ggml.model".into(), text("gpt2"));
    header.metadata.insert(
        "tokenizer.ggml.tokens".into(),
        GgufValue::Array {
            element_type: 8,
            count: 3,
            retained_values: vec![],
            encoded_digest: Sha256Digest::of_bytes(b"unit vocabulary content"),
        },
    );
}

#[test]
fn dflash_mask_policy_never_accepts_an_ordinary_draft_or_a_changed_tokenizer() {
    let mut main = component(ComponentRole::Main, "unit/main", "qwen35", "main");
    let mut draft = component(ComponentRole::Dflash, "unit/draft", "dflash", "draft");
    tokenizer(&mut main.files[0].header);
    tokenizer(&mut draft.files[0].header);
    draft.files[0]
        .header
        .metadata
        .insert("tokenizer.ggml.mask_token_id".into(), unsigned(2));
    let mut profile = profile(&main, &draft);
    profile
        .constraints
        .push(PairingConstraint::TokenizerIdentity {
            policy: TokenizerIdentityPolicy::DflashMask,
        });
    assert_eq!(
        assess_component_pairing(&main, &draft, &runtime(), &profile)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Verified
    );
    draft.files[0]
        .header
        .metadata
        .insert("tokenizer.ggml.mask_token_id".into(), unsigned(3));
    assert_eq!(
        assess_component_pairing(&main, &draft, &runtime(), &profile)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Incompatible
    );
    draft.files[0]
        .header
        .metadata
        .insert("tokenizer.ggml.mask_token_id".into(), unsigned(2));
    draft.files[0]
        .header
        .metadata
        .insert("tokenizer.ggml.model".into(), text("different-model"));
    assert_eq!(
        assess_component_pairing(&main, &draft, &runtime(), &profile)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Incompatible
    );
    draft.files[0]
        .header
        .metadata
        .insert("general.architecture".into(), text("qwen35"));
    let mut ordinary = profile.clone();
    ordinary.constraints.retain(|constraint| {
        !matches!(
            constraint,
            PairingConstraint::Text {
                side: PairingSide::Dependency,
                ..
            }
        )
    });
    assert_eq!(
        assess_component_pairing(&main, &draft, &runtime(), &ordinary)
            .unwrap()
            .compatibility,
        ComponentCompatibility::Incompatible
    );
}

#[test]
fn dflash_target_layers_must_be_complete_unique_and_within_the_actual_target() {
    let mut main = component(ComponentRole::Main, "unit/main", "qwen35", "main");
    let mut draft = component(ComponentRole::Dflash, "unit/draft", "dflash", "draft");
    main.files[0]
        .header
        .metadata
        .insert("qwen35.block_count".into(), unsigned(32));
    let mut profile = profile(&main, &draft);
    profile.constraints.push(PairingConstraint::LayerIndices {
        side: PairingSide::Dependency,
        key: "dflash.target_layers".into(),
        target_side: PairingSide::Main,
        target_block_count_key: "qwen35.block_count".into(),
    });
    for (values, count, expected) in [
        (vec![2, 30], 2, ComponentCompatibility::Verified),
        (vec![2, 32], 2, ComponentCompatibility::Incompatible),
        (vec![2, 2], 2, ComponentCompatibility::Incompatible),
        (vec![-1, 2], 2, ComponentCompatibility::Incompatible),
        (vec![2], 2, ComponentCompatibility::Unverified),
    ] {
        draft.files[0].header.metadata.insert(
            "dflash.target_layers".into(),
            GgufValue::Array {
                element_type: 5,
                count,
                retained_values: values.into_iter().map(GgufScalar::Signed).collect(),
                encoded_digest: Sha256Digest::of_bytes(b"unit target layers"),
            },
        );
        assert_eq!(
            assess_component_pairing(&main, &draft, &runtime(), &profile)
                .unwrap()
                .compatibility,
            expected
        );
    }
}

#[test]
fn physical_shard_completeness_precedes_compatibility_and_evidence_digest_is_order_independent() {
    let mut main = component(ComponentRole::Main, "unit/main", "qwen35", "main-0");
    main.files[0].header.shard = Some(GgufShard {
        index: 0,
        count: 2,
        total_tensors: 2,
    });
    let mut second = main.files[0].clone();
    second.artifact.id = "unit-second".into();
    second.artifact.path = RepositoryPath::parse("second.gguf").unwrap();
    second.artifact.source_sha256 = Some(Sha256Digest::of_bytes(b"unit second shard"));
    second.header.shard.as_mut().unwrap().index = 1;
    second.header.tensors[0].name = "other-weight".into();
    assert!(main.validate().is_err());
    main.files.push(second);
    main.validate().unwrap();
    let digest = main.digest().unwrap();
    main.files.reverse();
    assert_eq!(main.digest().unwrap(), digest);
    main.files[0].header.shard.as_mut().unwrap().index = 0;
    assert!(main.validate().is_err());
}

#[test]
fn rule_updates_change_evidence_even_when_the_same_component_pair_stays_compatible() {
    let main = component(ComponentRole::Main, "unit/main", "qwen35", "main");
    let dependency = component(
        ComponentRole::VisionProjector,
        "unit/projector",
        "clip",
        "vision",
    );
    let mut profile = profile(&main, &dependency);
    let before = assess_component_pairing(&main, &dependency, &runtime(), &profile).unwrap();
    profile.version = "2".into();
    let after = assess_component_pairing(&main, &dependency, &runtime(), &profile).unwrap();
    assert_eq!(after.compatibility, ComponentCompatibility::Verified);
    assert_ne!(before.digest().unwrap(), after.digest().unwrap());
}
