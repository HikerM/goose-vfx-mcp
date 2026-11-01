use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{
    ArchitectureKnowledge, Artifact, ComponentRole, FilePurpose, GgufInspection, GgufScalar,
    GgufShard, GgufValue, ModelCapability, ModelStructure, ModelTaskFamily, RepositoryPath,
    RepositorySnapshot, Sha256Digest, MODEL_LIFECYCLE_SCHEMA_VERSION,
};

pub const CLASSIFICATION_RULES_VERSION: &str = "gguf-content-v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTaskDeclaration {
    pub family: ModelTaskFamily,
    pub snapshot_digest: Sha256Digest,
    pub subject_header_digest: Sha256Digest,
    pub document_path: RepositoryPath,
    pub document_sha256: Sha256Digest,
}

impl SourceTaskDeclaration {
    fn validate(&self, snapshot: &Sha256Digest, header: &Sha256Digest) -> Result<()> {
        ensure!(
            self.family != ModelTaskFamily::Unknown,
            "Unknown task is not a source declaration"
        );
        ensure!(
            &self.snapshot_digest == snapshot && &self.subject_header_digest == header,
            "Task declaration belongs to a different source or GGUF header"
        );
        ensure!(
            super::metadata::task_document_kind(&self.document_path).is_some(),
            "Task declaration requires an allowed source document"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BooleanObservation {
    Unknown,
    Present,
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufModelFacts {
    pub stored_tensor_elements: u64,
    pub encoded_tensor_bytes: u64,
    pub tensor_count: u64,
    pub tensor_types: BTreeSet<u32>,
    pub expert_count: Option<u64>,
    pub experts_used: Option<u64>,
    pub embedding_length: Option<u64>,
    pub pooling_type: Option<u64>,
    pub classifier_output_width: Option<u64>,
    pub embedded_nextn_layers: Option<u64>,
    pub vision_encoder: BooleanObservation,
    pub projector_type: Option<String>,
    pub projection_length: Option<u64>,
    pub shard: Option<GgufShard>,
    pub has_expert_tensors: bool,
    pub has_chat_template: bool,
    pub packaged_runtime_family: Option<String>,
    pub packaged_spec_version: Option<u64>,
    #[serde(default)]
    pub tensor_layout_hints: BTreeSet<GgufTensorLayoutHint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GgufTensorLayoutHint {
    PackedLatentDiffusion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClassificationIssue {
    InvalidMetadata { key: String },
    InvalidExpertSelection,
    MissingExpertCount,
    ExpertTensorCountMismatch,
    UnknownPoolingType { value: u64 },
    InvalidClassifierHead,
    InvalidProjectorDeclaration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufClassification {
    pub schema_version: u32,
    pub rules_version: String,
    pub artifact_id: String,
    pub source_path: RepositoryPath,
    pub source_sha256: Option<Sha256Digest>,
    pub file_bytes: u64,
    pub snapshot_digest: Sha256Digest,
    pub header_digest: Sha256Digest,
    pub architecture: ArchitectureKnowledge,
    pub structure: ModelStructure,
    pub facts: GgufModelFacts,
    pub task_candidates: BTreeSet<ModelTaskFamily>,
    pub declared_tasks: BTreeSet<ModelTaskFamily>,
    pub role_hints: BTreeSet<ComponentRole>,
    pub acceleration_hints: BTreeSet<ModelCapability>,
    pub source_declarations: Vec<SourceTaskDeclaration>,
    pub issues: Vec<ClassificationIssue>,
}

impl GgufClassification {
    pub fn digest(&self) -> Result<Sha256Digest> {
        Sha256Digest::of_serializable(self)
    }
}

fn unsigned(
    inspection: &GgufInspection,
    key: &str,
    issues: &mut Vec<ClassificationIssue>,
) -> Option<u64> {
    let value = inspection.metadata.get(key)?;
    match value.as_u64() {
        Some(value) => Some(value),
        None => {
            issues.push(ClassificationIssue::InvalidMetadata {
                key: key.to_string(),
            });
            None
        }
    }
}

fn scoped_unsigned(
    inspection: &GgufInspection,
    architecture: Option<&str>,
    suffix: &str,
    issues: &mut Vec<ClassificationIssue>,
) -> Option<u64> {
    unsigned(inspection, &format!("{}.{suffix}", architecture?), issues)
}

fn text(
    inspection: &GgufInspection,
    key: &str,
    issues: &mut Vec<ClassificationIssue>,
) -> Option<String> {
    let value = inspection.metadata.get(key)?;
    match value.as_str().filter(|value| !value.trim().is_empty()) {
        Some(value) => Some(value.to_string()),
        None => {
            issues.push(ClassificationIssue::InvalidMetadata {
                key: key.to_string(),
            });
            None
        }
    }
}

fn boolean(
    inspection: &GgufInspection,
    key: &str,
    issues: &mut Vec<ClassificationIssue>,
) -> BooleanObservation {
    match inspection.metadata.get(key) {
        None => BooleanObservation::Unknown,
        Some(GgufValue::Scalar {
            value: GgufScalar::Bool(true),
            ..
        }) => BooleanObservation::Present,
        Some(GgufValue::Scalar {
            value: GgufScalar::Bool(false),
            ..
        }) => BooleanObservation::Absent,
        Some(_) => {
            issues.push(ClassificationIssue::InvalidMetadata {
                key: key.to_string(),
            });
            BooleanObservation::Unknown
        }
    }
}

fn classifier_width(
    inspection: &GgufInspection,
    issues: &mut Vec<ClassificationIssue>,
) -> Option<u64> {
    let weight = inspection
        .tensors
        .iter()
        .find(|tensor| tensor.name == "cls.output.weight")?;
    let width = match weight.dimensions.as_slice() {
        [input] if *input > 0 => 1,
        [input, output] if *input > 0 && *output > 0 => *output,
        _ => {
            issues.push(ClassificationIssue::InvalidClassifierHead);
            return None;
        }
    };
    if inspection
        .tensors
        .iter()
        .find(|tensor| tensor.name == "cls.output.bias")
        .is_some_and(|bias| bias.dimensions.as_slice() != [width])
    {
        issues.push(ClassificationIssue::InvalidClassifierHead);
        return None;
    }
    Some(width)
}

fn model_structure(
    inspection: &GgufInspection,
    architecture: Option<&str>,
    facts: &GgufModelFacts,
    issues: &mut Vec<ClassificationIssue>,
) -> ModelStructure {
    if issues.iter().any(|issue| {
        matches!(issue, ClassificationIssue::InvalidMetadata { key }
        if key.ends_with(".expert_count") || key.ends_with(".expert_used_count"))
    }) {
        return ModelStructure::Unknown;
    }
    if facts.experts_used.is_some_and(|used| {
        (used == 0 && facts.expert_count.is_some_and(|count| count > 0))
            || used > facts.expert_count.unwrap_or(0)
    }) {
        issues.push(ClassificationIssue::InvalidExpertSelection);
        return ModelStructure::Unknown;
    }
    if let Some(count) = facts.expert_count.filter(|count| *count > 0) {
        let inconsistent = inspection
            .tensors
            .iter()
            .filter(|tensor| {
                tensor.name.ends_with("ffn_gate_exps.weight")
                    || tensor.name.ends_with("ffn_down_exps.weight")
                    || tensor.name.ends_with("ffn_up_exps.weight")
            })
            .any(|tensor| tensor.dimensions.len() != 3 || tensor.dimensions.last() != Some(&count));
        if inconsistent {
            issues.push(ClassificationIssue::ExpertTensorCountMismatch);
            return ModelStructure::Unknown;
        }
        return ModelStructure::Moe;
    }
    if facts.has_expert_tensors || matches!(architecture, Some("qwen35moe" | "qwen2moe")) {
        if architecture.is_none()
            && inspection
                .shard
                .as_ref()
                .is_some_and(|shard| shard.index > 0)
        {
            return ModelStructure::Unknown;
        }
        issues.push(ClassificationIssue::MissingExpertCount);
        return ModelStructure::Unknown;
    }
    if matches!(
        architecture,
        Some("llama" | "qwen2" | "qwen3" | "qwen35" | "bert" | "whisper")
    ) {
        return ModelStructure::Dense;
    }
    ModelStructure::Unknown
}

pub fn classify_gguf(
    artifact: &Artifact,
    snapshot_id: &str,
    snapshot: &RepositorySnapshot,
    inspection: &GgufInspection,
    declarations: &[SourceTaskDeclaration],
) -> Result<GgufClassification> {
    artifact.validate()?;
    snapshot.validate()?;
    ensure!(
        artifact.purpose == FilePurpose::GgufWeight && artifact.snapshot_id == snapshot_id,
        "Classification source does not match a GGUF artifact"
    );
    ensure!(
        artifact.size_bytes == Some(inspection.file_bytes) && !inspection.tensors.is_empty(),
        "GGUF observation does not match the pinned artifact size or tensor content"
    );
    let snapshot_digest = snapshot.content_digest()?;
    for declaration in declarations {
        declaration.validate(&snapshot_digest, &inspection.header_digest)?;
    }
    let mut issues = Vec::new();
    let architecture = text(inspection, "general.architecture", &mut issues);
    let prefix = architecture.as_deref();
    let mut facts = GgufModelFacts {
        stored_tensor_elements: inspection.stored_tensor_elements,
        encoded_tensor_bytes: inspection.encoded_tensor_bytes,
        tensor_count: inspection.tensors.len() as u64,
        tensor_types: inspection
            .tensors
            .iter()
            .map(|tensor| tensor.type_id)
            .collect(),
        expert_count: scoped_unsigned(inspection, prefix, "expert_count", &mut issues),
        experts_used: scoped_unsigned(inspection, prefix, "expert_used_count", &mut issues),
        embedding_length: scoped_unsigned(inspection, prefix, "embedding_length", &mut issues),
        pooling_type: scoped_unsigned(inspection, prefix, "pooling_type", &mut issues),
        classifier_output_width: classifier_width(inspection, &mut issues),
        embedded_nextn_layers: scoped_unsigned(
            inspection,
            prefix,
            "nextn_predict_layers",
            &mut issues,
        ),
        vision_encoder: boolean(inspection, "clip.has_vision_encoder", &mut issues),
        projector_type: text(inspection, "clip.projector_type", &mut issues),
        projection_length: unsigned(inspection, "clip.vision.projection_dim", &mut issues),
        shard: inspection.shard.clone(),
        has_expert_tensors: inspection.tensors.iter().any(|tensor| {
            tensor.name.ends_with("ffn_gate_exps.weight")
                || tensor.name.ends_with("ffn_down_exps.weight")
                || tensor.name.ends_with("ffn_up_exps.weight")
        }),
        has_chat_template: false,
        packaged_runtime_family: None,
        packaged_spec_version: None,
        tensor_layout_hints: BTreeSet::new(),
    };
    if architecture.as_deref() == Some("audiocpp") {
        facts.packaged_runtime_family = text(inspection, "audiocpp.model_spec.family", &mut issues);
        facts.packaged_spec_version =
            unsigned(inspection, "audiocpp.model_spec.version", &mut issues);
    }
    if [
        "model.diffusion_model.",
        "first_stage_model.",
        "cond_stage_model.",
    ]
    .iter()
    .all(|prefix| {
        inspection
            .tensors
            .iter()
            .any(|tensor| tensor.name.starts_with(prefix))
    }) {
        facts
            .tensor_layout_hints
            .insert(GgufTensorLayoutHint::PackedLatentDiffusion);
    }
    facts.has_chat_template = text(inspection, "tokenizer.chat_template", &mut issues).is_some();
    let structure = model_structure(inspection, architecture.as_deref(), &facts, &mut issues);
    let mut task_candidates = BTreeSet::new();
    let mut role_hints = BTreeSet::new();
    let mut acceleration_hints = BTreeSet::new();
    if matches!(
        architecture.as_deref(),
        Some("llama" | "qwen2" | "qwen3" | "qwen35" | "qwen35moe" | "qwen2moe")
    ) {
        task_candidates.insert(ModelTaskFamily::Language);
    }
    match facts.pooling_type {
        Some(1..=3) => {
            task_candidates.insert(ModelTaskFamily::Embedding);
        }
        Some(4) => {
            task_candidates.insert(ModelTaskFamily::Reranking);
        }
        Some(value) if value > 4 => {
            issues.push(ClassificationIssue::UnknownPoolingType { value });
        }
        _ => {}
    }
    if facts.classifier_output_width == Some(1) {
        task_candidates.insert(ModelTaskFamily::Reranking);
    }
    if architecture.as_deref() == Some("wan") {
        task_candidates.insert(ModelTaskFamily::VideoDiffusion);
    }
    if architecture.as_deref() == Some("audiocpp") {
        task_candidates.insert(ModelTaskFamily::Audio);
        if facts.packaged_spec_version == Some(1) {
            match facts.packaged_runtime_family.as_deref() {
                Some("pocket_tts") => {
                    task_candidates.insert(ModelTaskFamily::Tts);
                }
                Some("qwen3_asr") => {
                    task_candidates.insert(ModelTaskFamily::Asr);
                }
                _ => {}
            }
        }
    }
    if architecture.as_deref() == Some("whisper") {
        task_candidates.insert(ModelTaskFamily::Asr);
    }
    if facts
        .tensor_layout_hints
        .contains(&GgufTensorLayoutHint::PackedLatentDiffusion)
    {
        task_candidates.insert(ModelTaskFamily::ImageDiffusion);
    }
    if architecture.as_deref() == Some("dflash") {
        role_hints.insert(ComponentRole::Dflash);
        acceleration_hints.insert(ModelCapability::Dflash);
    }
    if facts.embedded_nextn_layers.is_some_and(|count| count > 0) {
        acceleration_hints.insert(ModelCapability::Mtp);
    }
    if facts.projector_type.is_some() {
        if architecture.as_deref() == Some("clip")
            && facts.vision_encoder == BooleanObservation::Present
        {
            role_hints.insert(ComponentRole::VisionProjector);
        } else {
            issues.push(ClassificationIssue::InvalidProjectorDeclaration);
        }
    }
    let declared_type = text(inspection, "general.type", &mut issues);
    if role_hints.is_empty()
        && (declared_type.as_deref() == Some("model")
            || (declared_type.is_none()
                && facts
                    .tensor_layout_hints
                    .contains(&GgufTensorLayoutHint::PackedLatentDiffusion)))
    {
        role_hints.insert(ComponentRole::Main);
    }
    let evidence_id = format!("gguf-header:{}", inspection.header_digest.as_str());
    let architecture = match architecture {
        Some(architecture) => ArchitectureKnowledge::Observed {
            architecture,
            evidence_ids: BTreeSet::from([evidence_id]),
        },
        None => ArchitectureKnowledge::Unknown,
    };
    let mut source_declarations = declarations.to_vec();
    source_declarations.sort_by(|left, right| {
        (
            &left.family,
            left.document_path.as_str(),
            &left.document_sha256,
        )
            .cmp(&(
                &right.family,
                right.document_path.as_str(),
                &right.document_sha256,
            ))
    });
    source_declarations.dedup();
    Ok(GgufClassification {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        rules_version: CLASSIFICATION_RULES_VERSION.to_string(),
        artifact_id: artifact.id.clone(),
        snapshot_digest,
        source_path: artifact.path.clone(),
        source_sha256: artifact.source_sha256.clone(),
        file_bytes: inspection.file_bytes,
        header_digest: inspection.header_digest.clone(),
        architecture,
        structure,
        facts,
        task_candidates,
        declared_tasks: source_declarations
            .iter()
            .map(|declaration| declaration.family)
            .collect(),
        role_hints,
        acceleration_hints,
        source_declarations,
        issues,
    })
}
