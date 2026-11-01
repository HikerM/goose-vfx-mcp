use lumina_local_inference::lifecycle::*;
use std::collections::{BTreeMap, BTreeSet};

fn scalar(value: GgufScalar) -> GgufValue {
    GgufValue::Scalar { type_id: 0, value }
}

fn tensor(name: &str, dimensions: Vec<u64>) -> GgufTensor {
    let elements = dimensions.iter().product();
    GgufTensor {
        name: name.into(),
        dimensions,
        type_id: 0,
        quantization: "F32".into(),
        relative_offset: 0,
        encoded_bytes: elements * 4,
        elements,
    }
}

fn inspection(architecture: Option<&str>) -> GgufInspection {
    let mut metadata = BTreeMap::new();
    if let Some(architecture) = architecture {
        metadata.insert(
            "general.architecture".into(),
            scalar(GgufScalar::String(architecture.into())),
        );
    }
    metadata.insert(
        "general.type".into(),
        scalar(GgufScalar::String("model".into())),
    );
    GgufInspection {
        version: 3,
        byte_order: GgufByteOrder::LittleEndian,
        file_bytes: 1024,
        header_bytes: 128,
        header_digest: Sha256Digest::of_bytes(b"classification fixture header"),
        data_offset: 128,
        alignment: 32,
        metadata,
        tensors: vec![tensor("token_embd.weight", vec![4, 4])],
        stored_tensor_elements: 16,
        encoded_tensor_bytes: 64,
        shard: None,
    }
}

fn source() -> (Artifact, RepositorySnapshot) {
    (
        Artifact {
            id: "artifact".into(),
            snapshot_id: "snapshot".into(),
            path: RepositoryPath::parse("weights.gguf").unwrap(),
            purpose: FilePurpose::GgufWeight,
            size_bytes: Some(1024),
            source_sha256: None,
        },
        RepositorySnapshot {
            repository: ModelScopeRepository::parse("owner/model").unwrap(),
            requested_revision: "master".into(),
            resolved_revision: "a".repeat(40),
            files_digest: Sha256Digest::of_bytes(b"file inventory"),
            fetched_at: 100,
        },
    )
}

fn classify(inspection: &GgufInspection) -> GgufClassification {
    let (artifact, snapshot) = source();
    classify_gguf(&artifact, "snapshot", &snapshot, inspection, &[]).unwrap()
}

#[test]
fn names_and_chat_templates_do_not_grant_task_or_runtime_capabilities() {
    let mut original = inspection(Some("qwen3"));
    let before = classify(&original);
    original.metadata.insert(
        "general.name".into(),
        scalar(GgufScalar::String("Vision-Embedding-TTS-MTP".into())),
    );
    original.metadata.insert(
        "tokenizer.chat_template".into(),
        scalar(GgufScalar::String(
            "{% if tools %}{{ tools }}{% endif %}".into(),
        )),
    );
    let mut artifact = source().0;
    artifact.path = RepositoryPath::parse("vision-reranker-dflash-audio.gguf").unwrap();
    let after = classify_gguf(&artifact, "snapshot", &source().1, &original, &[]).unwrap();
    assert_eq!(after.task_candidates, before.task_candidates);
    assert_eq!(
        after.task_candidates,
        BTreeSet::from([ModelTaskFamily::Language])
    );
    assert!(after.declared_tasks.is_empty());
    assert!(after.acceleration_hints.is_empty());
    assert!(after.facts.has_chat_template);
}

#[test]
fn pooling_and_single_score_heads_are_candidates_not_training_or_runtime_proof() {
    let mut embed = inspection(Some("qwen3"));
    embed
        .metadata
        .insert("qwen3.pooling_type".into(), scalar(GgufScalar::Unsigned(3)));
    let embedding = classify(&embed);
    assert!(embedding
        .task_candidates
        .contains(&ModelTaskFamily::Embedding));
    assert!(embedding.declared_tasks.is_empty());
    let mut rank = inspection(Some("bert"));
    rank.tensors
        .push(tensor("cls.output.weight", vec![1024, 1]));
    rank.tensors.push(tensor("cls.output.bias", vec![1]));
    assert_eq!(classify(&rank).facts.classifier_output_width, Some(1));
    assert!(classify(&rank)
        .task_candidates
        .contains(&ModelTaskFamily::Reranking));
    rank.tensors[1].dimensions = vec![1024];
    assert_eq!(classify(&rank).facts.classifier_output_width, Some(1));
    assert!(classify(&rank).issues.is_empty());
    rank.tensors[1].dimensions = vec![1024, 2];
    rank.tensors[2].dimensions = vec![2];
    assert!(!classify(&rank)
        .task_candidates
        .contains(&ModelTaskFamily::Reranking));
    rank.tensors[2].dimensions = vec![1];
    assert!(classify(&rank)
        .issues
        .contains(&ClassificationIssue::InvalidClassifierHead));
}

#[test]
fn moe_preserves_resident_tensor_count_without_dividing_by_active_experts() {
    let mut model = inspection(Some("qwen35moe"));
    model.metadata.insert(
        "qwen35moe.expert_count".into(),
        scalar(GgufScalar::Unsigned(256)),
    );
    model.metadata.insert(
        "qwen35moe.expert_used_count".into(),
        scalar(GgufScalar::Unsigned(8)),
    );
    model
        .tensors
        .push(tensor("blk.0.ffn_gate_exps.weight", vec![4, 8, 256]));
    model.stored_tensor_elements = 16 + 4 * 8 * 256;
    let result = classify(&model);
    assert_eq!(result.structure, ModelStructure::Moe);
    assert_eq!(result.facts.expert_count, Some(256));
    assert_eq!(result.facts.experts_used, Some(8));
    assert_eq!(
        result.facts.stored_tensor_elements,
        model.stored_tensor_elements
    );
    model.tensors[1].dimensions[2] = 128;
    let inconsistent = classify(&model);
    assert_eq!(inconsistent.structure, ModelStructure::Unknown);
    assert!(inconsistent
        .issues
        .contains(&ClassificationIssue::ExpertTensorCountMismatch));
}

#[test]
fn invalid_or_missing_expert_metadata_does_not_become_dense() {
    let mut model = inspection(Some("qwen3"));
    model.metadata.insert(
        "qwen3.expert_count".into(),
        scalar(GgufScalar::String("256".into())),
    );
    assert_eq!(classify(&model).structure, ModelStructure::Unknown);
    model = inspection(Some("qwen35moe"));
    assert_eq!(classify(&model).structure, ModelStructure::Unknown);
    model.metadata.insert(
        "qwen35moe.expert_count".into(),
        scalar(GgufScalar::Unsigned(8)),
    );
    model.metadata.insert(
        "qwen35moe.expert_used_count".into(),
        scalar(GgufScalar::Unsigned(9)),
    );
    assert!(classify(&model)
        .issues
        .contains(&ClassificationIssue::InvalidExpertSelection));
    model = inspection(Some("qwen3"));
    model
        .metadata
        .insert("qwen3.expert_count".into(), scalar(GgufScalar::Unsigned(0)));
    model.metadata.insert(
        "qwen3.expert_used_count".into(),
        scalar(GgufScalar::Unsigned(0)),
    );
    assert_eq!(classify(&model).structure, ModelStructure::Dense);
}

#[test]
fn projector_embedded_mtp_and_dflash_keep_distinct_component_meanings() {
    let mut projector = inspection(Some("clip"));
    projector.metadata.insert(
        "clip.has_vision_encoder".into(),
        scalar(GgufScalar::Bool(true)),
    );
    projector.metadata.insert(
        "clip.projector_type".into(),
        scalar(GgufScalar::String("qwen3vl_merger".into())),
    );
    projector.metadata.insert(
        "clip.vision.projection_dim".into(),
        scalar(GgufScalar::Unsigned(2048)),
    );
    let classified = classify(&projector);
    assert_eq!(
        classified.role_hints,
        BTreeSet::from([ComponentRole::VisionProjector])
    );
    assert_eq!(classified.facts.projection_length, Some(2048));
    assert!(classified.declared_tasks.is_empty());
    let mut mtp = inspection(Some("qwen35moe"));
    mtp.metadata.insert(
        "qwen35moe.nextn_predict_layers".into(),
        scalar(GgufScalar::Unsigned(1)),
    );
    let classified = classify(&mtp);
    assert!(classified
        .acceleration_hints
        .contains(&ModelCapability::Mtp));
    assert!(!classified.role_hints.contains(&ComponentRole::Mtp));
    assert!(classified.role_hints.contains(&ComponentRole::Main));
    let dflash = classify(&inspection(Some("dflash")));
    assert_eq!(dflash.role_hints, BTreeSet::from([ComponentRole::Dflash]));
    assert_eq!(
        dflash.acceleration_hints,
        BTreeSet::from([ModelCapability::Dflash])
    );
}

#[test]
fn unknown_architecture_and_later_shard_metadata_remain_explicitly_unknown() {
    let mut unknown = inspection(None);
    unknown
        .metadata
        .insert(".pooling_type".into(), scalar(GgufScalar::Unsigned(3)));
    assert_eq!(
        classify(&unknown).architecture,
        ArchitectureKnowledge::Unknown
    );
    assert!(classify(&unknown).task_candidates.is_empty());
    unknown.shard = Some(GgufShard {
        index: 1,
        count: 2,
        total_tensors: 2,
    });
    unknown
        .tensors
        .push(tensor("blk.29.ffn_down_exps.weight", vec![4, 8, 256]));
    let partial = classify(&unknown);
    assert_eq!(partial.structure, ModelStructure::Unknown);
    assert!(!partial
        .issues
        .contains(&ClassificationIssue::MissingExpertCount));
    assert_eq!(partial.facts.shard.unwrap().index, 1);
}

#[test]
fn declarations_are_bound_to_pinned_snapshot_document_and_subject_header() {
    let model = inspection(Some("llama"));
    let (artifact, snapshot) = source();
    let declaration = SourceTaskDeclaration {
        family: ModelTaskFamily::Tts,
        snapshot_digest: snapshot.content_digest().unwrap(),
        subject_header_digest: model.header_digest.clone(),
        document_path: RepositoryPath::parse("README.md").unwrap(),
        document_sha256: Sha256Digest::of_bytes(b"actual task declaration fixture"),
    };
    let declared = classify_gguf(
        &artifact,
        "snapshot",
        &snapshot,
        &model,
        std::slice::from_ref(&declaration),
    )
    .unwrap();
    assert_eq!(
        declared.declared_tasks,
        BTreeSet::from([ModelTaskFamily::Tts])
    );
    assert!(declared
        .task_candidates
        .contains(&ModelTaskFamily::Language));
    let mut wrong = declaration.clone();
    wrong.snapshot_digest = Sha256Digest::of_bytes(b"different revision");
    assert!(classify_gguf(&artifact, "snapshot", &snapshot, &model, &[wrong]).is_err());
    wrong = declaration.clone();
    wrong.subject_header_digest = Sha256Digest::of_bytes(b"different weights");
    assert!(classify_gguf(&artifact, "snapshot", &snapshot, &model, &[wrong]).is_err());
    wrong = declaration;
    wrong.document_path = RepositoryPath::parse("custom_model.py").unwrap();
    assert!(classify_gguf(&artifact, "snapshot", &snapshot, &model, &[wrong]).is_err());
}

#[test]
fn source_mismatch_is_rejected_and_declaration_order_does_not_change_digest() {
    let model = inspection(Some("qwen3"));
    let (mut artifact, snapshot) = source();
    assert!(classify_gguf(&artifact, "wrong-snapshot", &snapshot, &model, &[]).is_err());
    artifact.size_bytes = Some(2048);
    assert!(classify_gguf(&artifact, "snapshot", &snapshot, &model, &[]).is_err());
    artifact.size_bytes = Some(1024);
    let first = SourceTaskDeclaration {
        family: ModelTaskFamily::Embedding,
        snapshot_digest: snapshot.content_digest().unwrap(),
        subject_header_digest: model.header_digest.clone(),
        document_path: RepositoryPath::parse("README.md").unwrap(),
        document_sha256: Sha256Digest::of_bytes(b"declaration"),
    };
    let mut second = first.clone();
    second.family = ModelTaskFamily::Language;
    let a = classify_gguf(
        &artifact,
        "snapshot",
        &snapshot,
        &model,
        &[first.clone(), second.clone()],
    )
    .unwrap();
    let b = classify_gguf(
        &artifact,
        "snapshot",
        &snapshot,
        &model,
        &[second, first.clone(), first],
    )
    .unwrap();
    assert_eq!(a.digest().unwrap(), b.digest().unwrap());
}

#[test]
fn all_task_families_can_be_declared_without_defaulting_unknown_containers_to_chat() {
    let model = inspection(None);
    let (artifact, snapshot) = source();
    for family in [
        ModelTaskFamily::Language,
        ModelTaskFamily::VisionLanguage,
        ModelTaskFamily::Asr,
        ModelTaskFamily::Tts,
        ModelTaskFamily::Audio,
        ModelTaskFamily::Embedding,
        ModelTaskFamily::Reranking,
        ModelTaskFamily::ImageDiffusion,
        ModelTaskFamily::VideoDiffusion,
        ModelTaskFamily::Specialized,
    ] {
        let declaration = SourceTaskDeclaration {
            family,
            snapshot_digest: snapshot.content_digest().unwrap(),
            subject_header_digest: model.header_digest.clone(),
            document_path: RepositoryPath::parse("config.json").unwrap(),
            document_sha256: Sha256Digest::of_bytes(b"explicit source task fixture"),
        };
        let classified =
            classify_gguf(&artifact, "snapshot", &snapshot, &model, &[declaration]).unwrap();
        assert_eq!(classified.declared_tasks, BTreeSet::from([family]));
        assert!(classified.task_candidates.is_empty());
        assert_eq!(classified.architecture, ArchitectureKnowledge::Unknown);
    }
}

#[test]
fn malformed_pooling_and_projector_metadata_preserve_unknown_instead_of_coercing_values() {
    let mut model = inspection(Some("qwen3"));
    model.metadata.insert(
        "qwen3.pooling_type".into(),
        scalar(GgufScalar::Unsigned(99)),
    );
    let classified = classify(&model);
    assert!(classified
        .issues
        .contains(&ClassificationIssue::UnknownPoolingType { value: 99 }));
    assert!(!classified
        .task_candidates
        .contains(&ModelTaskFamily::Embedding));
    model = inspection(Some("clip"));
    model.metadata.insert(
        "clip.has_vision_encoder".into(),
        scalar(GgufScalar::String("true".into())),
    );
    model.metadata.insert(
        "clip.projector_type".into(),
        scalar(GgufScalar::String("qwen3vl_merger".into())),
    );
    let classified = classify(&model);
    assert_eq!(classified.facts.vision_encoder, BooleanObservation::Unknown);
    assert!(!classified
        .role_hints
        .contains(&ComponentRole::VisionProjector));
    assert!(classified
        .issues
        .contains(&ClassificationIssue::InvalidProjectorDeclaration));
}
