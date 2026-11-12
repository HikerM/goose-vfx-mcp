use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::storage::{digest, immutable_file, migration_directory, read_registry, registry_lock};
use super::{LocalModelEntry, LocalModelStorage};

pub const LEGACY_IMPORT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyVerificationState {
    ImportedUnverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacySourceTraceability {
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyAssetRole {
    Main,
    Shard,
    Mmproj,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyAssetOwnership {
    ReferencedOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyAssetReference {
    pub role: LegacyAssetRole,
    pub path: PathBuf,
    pub declared_source: Option<String>,
    pub ownership: LegacyAssetOwnership,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyAlias {
    pub migration_id: String,
    pub legacy_id: String,
    pub record_index: usize,
    pub original_record: Value,
    pub declared_storage: Option<LocalModelStorage>,
    pub verification_state: LegacyVerificationState,
    pub source_traceability: LegacySourceTraceability,
    pub assets: Vec<LegacyAssetReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyImportDiagnosticCode {
    InvalidJson,
    InvalidRegistry,
    InvalidRecord,
    DuplicateId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyImportDiagnostic {
    pub code: LegacyImportDiagnosticCode,
    pub record_index: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyImportReport {
    pub schema_version: u32,
    pub migration_id: String,
    pub source_registry: PathBuf,
    pub source_sha256: String,
    pub backup_path: PathBuf,
    pub ledger_path: PathBuf,
    pub aliases: Vec<LegacyAlias>,
    pub diagnostics: Vec<LegacyImportDiagnostic>,
}

impl LegacyImportReport {
    pub fn require_valid(&self) -> Result<()> {
        ensure!(
            self.diagnostics.is_empty(),
            "Legacy registry import {} needs repair: {}; original bytes preserved at {}",
            self.migration_id,
            self.diagnostics
                .iter()
                .map(|item| item.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
            self.backup_path.display(),
        );
        Ok(())
    }
}

/// Snapshot only: model files and the original registry are never modified.
pub fn migrate_legacy_registry(data_dir: &Path) -> Result<Option<LegacyImportReport>> {
    let data_dir = super::storage::data_directory(data_dir)?;
    let path = data_dir.join("models/registry.json");
    let _lock = registry_lock(&path)?;
    read_registry(&path)?
        .map(|bytes| preserve_snapshot(&data_dir, &path, &bytes))
        .transpose()
}

pub(super) fn preserve_snapshot(
    data_dir: &Path,
    source_registry: &Path,
    bytes: &[u8],
) -> Result<LegacyImportReport> {
    let source_sha256 = digest(bytes);
    let migration_id = digest(&serde_json::to_vec(&(
        LEGACY_IMPORT_VERSION,
        source_registry,
        &source_sha256,
    ))?);
    let directory = migration_directory(data_dir, &migration_id)?;
    let backup_path = directory.join("registry.json");
    immutable_file(&backup_path, bytes)?;
    let mut report = LegacyImportReport {
        schema_version: LEGACY_IMPORT_VERSION,
        migration_id,
        source_registry: source_registry.to_path_buf(),
        source_sha256,
        backup_path,
        ledger_path: directory.join("import.json"),
        aliases: Vec::new(),
        diagnostics: Vec::new(),
    };
    match serde_json::from_slice::<Value>(bytes) {
        Err(error) => report.diagnostics.push(LegacyImportDiagnostic {
            code: LegacyImportDiagnosticCode::InvalidJson,
            record_index: None,
            message: format!("Registry JSON is invalid: {error}"),
        }),
        Ok(raw) => match raw.get("models").and_then(Value::as_array) {
            None => report.diagnostics.push(LegacyImportDiagnostic {
                code: LegacyImportDiagnosticCode::InvalidRegistry,
                record_index: None,
                message: "Registry must contain a models array".to_string(),
            }),
            Some(records) => {
                let mut ids = BTreeMap::new();
                for (index, raw_record) in records.iter().enumerate() {
                    match serde_json::from_value::<LocalModelEntry>(raw_record.clone()) {
                        Err(error) => report.diagnostics.push(LegacyImportDiagnostic {
                            code: LegacyImportDiagnosticCode::InvalidRecord,
                            record_index: Some(index),
                            message: format!("Registry record {index} is invalid: {error}"),
                        }),
                        Ok(entry) => {
                            if entry.id.trim().is_empty() {
                                report.diagnostics.push(LegacyImportDiagnostic {
                                    code: LegacyImportDiagnosticCode::InvalidRecord,
                                    record_index: Some(index),
                                    message: format!(
                                        "Registry record {index} has an empty model ID"
                                    ),
                                });
                            }
                            if let Some(first) = ids.insert(entry.id.clone(), index) {
                                report.diagnostics.push(LegacyImportDiagnostic {
                                    code: LegacyImportDiagnosticCode::DuplicateId,
                                    record_index: Some(index),
                                    message: format!("Registry records {first} and {index} have the same model ID"),
                                });
                            }
                            let mut assets = vec![LegacyAssetReference {
                                role: LegacyAssetRole::Main,
                                path: entry.local_path,
                                declared_source: Some(entry.source_url),
                                ownership: LegacyAssetOwnership::ReferencedOnly,
                            }];
                            assets.extend(entry.shard_files.into_iter().map(|shard| {
                                LegacyAssetReference {
                                    role: LegacyAssetRole::Shard,
                                    path: shard.local_path,
                                    declared_source: Some(shard.source_url),
                                    ownership: LegacyAssetOwnership::ReferencedOnly,
                                }
                            }));
                            if let Some(path) = entry.mmproj_path {
                                assets.push(LegacyAssetReference {
                                    role: LegacyAssetRole::Mmproj,
                                    path,
                                    declared_source: entry.mmproj_source_url,
                                    ownership: LegacyAssetOwnership::ReferencedOnly,
                                });
                            }
                            report.aliases.push(LegacyAlias {
                                migration_id: report.migration_id.clone(),
                                legacy_id: entry.id,
                                record_index: index,
                                original_record: raw_record.clone(),
                                declared_storage: raw_record.get("storage").map(|_| entry.storage),
                                verification_state: LegacyVerificationState::ImportedUnverified,
                                source_traceability: LegacySourceTraceability::Unverified,
                                assets,
                            });
                        }
                    }
                }
            }
        },
    }
    let ledger = serde_json::to_vec_pretty(&report)?;
    immutable_file(&report.ledger_path, &ledger).context("Legacy import ledger is unavailable")?;
    Ok(report)
}
