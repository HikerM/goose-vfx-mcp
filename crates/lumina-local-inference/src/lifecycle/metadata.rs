use anyhow::{ensure, Result};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Debug, Formatter};

use super::{
    LifecycleErrorCode, ModelScopeProvider, ModelSourceError, ModelTaskFamily, RepositoryInventory,
    RepositoryPath, Sha256Digest, MODEL_LIFECYCLE_SCHEMA_VERSION,
};

pub const MAX_SOURCE_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const TASK_DECLARATION_RULES_VERSION: &str = "source-task-fields-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceDocumentKind {
    ModelCard,
    JsonConfig,
    ModelIndex,
}

pub(crate) fn task_document_kind(path: &RepositoryPath) -> Option<SourceDocumentKind> {
    let filename = path.as_str().rsplit('/').next()?.to_ascii_lowercase();
    match filename.as_str() {
        "config.json" | "configuration.json" => Some(SourceDocumentKind::JsonConfig),
        "model-index.yaml" | "model-index.yml" => Some(SourceDocumentKind::ModelIndex),
        value if value.starts_with("readme") && value.ends_with(".md") => {
            Some(SourceDocumentKind::ModelCard)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceDocumentEvidence {
    pub schema_version: u32,
    pub snapshot_digest: Sha256Digest,
    pub path: RepositoryPath,
    pub kind: SourceDocumentKind,
    pub size_bytes: u64,
    pub source_sha256: Option<Sha256Digest>,
    pub computed_sha256: Sha256Digest,
}

pub struct ModelSourceDocument {
    evidence: SourceDocumentEvidence,
    text: String,
}

impl Debug for ModelSourceDocument {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelSourceDocument")
            .field("evidence", &self.evidence)
            .finish_non_exhaustive()
    }
}

impl ModelSourceDocument {
    pub fn evidence(&self) -> &SourceDocumentEvidence {
        &self.evidence
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

fn document_error(code: LifecycleErrorCode) -> ModelSourceError {
    ModelSourceError {
        code,
        http_status: None,
    }
}

pub fn source_document_size(inventory: &RepositoryInventory, path: &RepositoryPath) -> Result<u64> {
    inventory.validate()?;
    ensure!(
        task_document_kind(path).is_some(),
        document_error(LifecycleErrorCode::FormatForbidden)
    );
    let source = inventory
        .files
        .get(path.as_str())
        .ok_or_else(|| document_error(LifecycleErrorCode::SourceNotFound))?;
    let size = source
        .size_bytes
        .ok_or_else(|| document_error(LifecycleErrorCode::SourcePartialFailure))?;
    ensure!(
        size <= MAX_SOURCE_DOCUMENT_BYTES as u64,
        document_error(LifecycleErrorCode::SourcePartialFailure)
    );
    Ok(size)
}

pub fn verify_source_document(
    inventory: &RepositoryInventory,
    path: &RepositoryPath,
    bytes: Vec<u8>,
) -> Result<ModelSourceDocument> {
    let size = source_document_size(inventory, path)?;
    ensure!(
        bytes.len() as u64 == size,
        document_error(LifecycleErrorCode::SourcePartialFailure)
    );
    let computed_sha256 = Sha256Digest::of_bytes(&bytes);
    let source = &inventory.files[path.as_str()];
    ensure!(
        source
            .sha256
            .as_ref()
            .is_none_or(|expected| expected == &computed_sha256),
        document_error(LifecycleErrorCode::HashMismatch)
    );
    let text = String::from_utf8(bytes)
        .map_err(|_| document_error(LifecycleErrorCode::SourcePartialFailure))?;
    Ok(ModelSourceDocument {
        evidence: SourceDocumentEvidence {
            schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
            snapshot_digest: inventory.snapshot.content_digest()?,
            path: path.clone(),
            kind: task_document_kind(path).expect("Document kind was checked"),
            size_bytes: size,
            source_sha256: source.sha256.clone(),
            computed_sha256,
        },
        text,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDeclarationIssue {
    InvalidJson,
    InvalidFrontmatter,
    AmbiguousTaskField,
    UnsupportedTaskValue,
    DeclarationLimitExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceTaskScope {
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentTaskObservation {
    pub rules_version: String,
    pub document: SourceDocumentEvidence,
    pub scope: SourceTaskScope,
    pub task_families: BTreeSet<ModelTaskFamily>,
    pub unrecognized_tags: BTreeSet<String>,
    pub issues: Vec<TaskDeclarationIssue>,
}

fn task_family(tag: &str) -> Option<ModelTaskFamily> {
    match tag {
        "text-generation" | "conversational" => Some(ModelTaskFamily::Language),
        "image-text-to-text" | "visual-question-answering" => Some(ModelTaskFamily::VisionLanguage),
        "automatic-speech-recognition" => Some(ModelTaskFamily::Asr),
        "text-to-speech" => Some(ModelTaskFamily::Tts),
        "audio-text-to-text" | "audio-to-audio" | "text-to-audio" => Some(ModelTaskFamily::Audio),
        "text-embedding" => Some(ModelTaskFamily::Embedding),
        "text-ranking" | "text-reranking" => Some(ModelTaskFamily::Reranking),
        "text-to-image" | "image-to-image" => Some(ModelTaskFamily::ImageDiffusion),
        "text-to-video" | "image-to-video" => Some(ModelTaskFamily::VideoDiffusion),
        _ => None,
    }
}

struct UniqueRootObject(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for UniqueRootObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = UniqueRootObject;
            fn expecting(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object with unique root keys")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("Duplicate configuration key"));
                    }
                }
                Ok(UniqueRootObject(values))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

fn task_key(key: &str) -> bool {
    matches!(key, "task" | "task_type" | "pipeline_tag")
}

fn json_tags(text: &str, issues: &mut Vec<TaskDeclarationIssue>) -> Vec<String> {
    let Ok(UniqueRootObject(values)) = serde_json::from_str(text) else {
        issues.push(TaskDeclarationIssue::InvalidJson);
        return Vec::new();
    };
    let mut tags = Vec::new();
    for (key, value) in values {
        if !task_key(&key) {
            continue;
        }
        match value {
            Value::String(value) => tags.push(value),
            Value::Array(values) if values.len() <= 32 => {
                for value in values {
                    if let Value::String(value) = value {
                        tags.push(value);
                    } else {
                        issues.push(TaskDeclarationIssue::UnsupportedTaskValue);
                    }
                }
            }
            _ => issues.push(TaskDeclarationIssue::UnsupportedTaskValue),
        }
    }
    tags
}

fn yaml_tags(text: &str, card: bool, issues: &mut Vec<TaskDeclarationIssue>) -> Vec<String> {
    let mut lines = text.lines();
    if card && lines.next() != Some("---") {
        return Vec::new();
    }
    let mut tags = Vec::new();
    let mut keys = BTreeSet::new();
    let mut closed = !card;
    let mut bytes = 0usize;
    for (index, line) in lines.enumerate() {
        bytes += line.len();
        if index >= 256 || bytes > 16 * 1024 {
            issues.push(TaskDeclarationIssue::DeclarationLimitExceeded);
            break;
        }
        if line == "---" {
            if !card && index == 0 {
                continue;
            }
            if !card {
                issues.push(TaskDeclarationIssue::InvalidFrontmatter);
            }
            closed = true;
            break;
        }
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim_end();
        if !task_key(key) {
            continue;
        }
        if !keys.insert(key) {
            issues.push(TaskDeclarationIssue::AmbiguousTaskField);
        }
        let value = value.trim();
        let parsed = if value.starts_with('"') {
            serde_json::from_str::<String>(value).ok()
        } else if let Some(value) = value
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
        {
            Some(value.replace("''", "'"))
        } else if value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
            && !value.is_empty()
        {
            Some(value.to_string())
        } else {
            None
        };
        if let Some(value) = parsed {
            tags.push(value);
        } else {
            issues.push(TaskDeclarationIssue::UnsupportedTaskValue);
        }
    }
    if !closed {
        issues.push(TaskDeclarationIssue::InvalidFrontmatter);
    }
    tags
}

pub fn observe_document_tasks(document: &ModelSourceDocument) -> DocumentTaskObservation {
    let mut issues = Vec::new();
    let text = document
        .text
        .strip_prefix('\u{feff}')
        .unwrap_or(&document.text);
    let tags = match document.evidence.kind {
        SourceDocumentKind::JsonConfig => json_tags(text, &mut issues),
        SourceDocumentKind::ModelCard => yaml_tags(text, true, &mut issues),
        SourceDocumentKind::ModelIndex => yaml_tags(text, false, &mut issues),
    };
    if tags.len() > 32
        || tags
            .iter()
            .any(|tag| tag.len() > 128 || tag.chars().any(char::is_control))
    {
        issues.push(TaskDeclarationIssue::DeclarationLimitExceeded);
    }
    if tags.iter().any(|tag| tag.trim().is_empty()) {
        issues.push(TaskDeclarationIssue::UnsupportedTaskValue);
    }
    let mut families = BTreeSet::new();
    let mut unrecognized_tags = BTreeSet::new();
    if issues.is_empty() {
        for tag in tags {
            let tag = tag.trim().to_ascii_lowercase();
            if let Some(family) = task_family(&tag) {
                families.insert(family);
            } else {
                unrecognized_tags.insert(tag);
            }
        }
    }
    DocumentTaskObservation {
        rules_version: TASK_DECLARATION_RULES_VERSION.to_string(),
        document: document.evidence.clone(),
        scope: SourceTaskScope::Document,
        task_families: families,
        unrecognized_tags,
        issues,
    }
}

impl ModelScopeProvider {
    pub async fn read_source_document(
        &self,
        inventory: &RepositoryInventory,
        path: &RepositoryPath,
        token: Option<&str>,
    ) -> Result<ModelSourceDocument> {
        let expected = source_document_size(inventory, path)?;
        let bytes = self
            .fetch_source_document(inventory, path, expected, token)
            .await?;
        verify_source_document(inventory, path, bytes)
    }
}
