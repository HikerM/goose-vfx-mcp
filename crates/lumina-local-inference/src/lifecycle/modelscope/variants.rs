use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use super::{source_artifact_id, ObservedRepositoryPath, RepositoryFile, RepositoryInventory};
use crate::lifecycle::{FilePurpose, Sha256Digest};

pub const VARIANT_FILENAME_HINTS_VERSION: &str = "variant-filename-hints-v1";

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum FilenameGroupKey {
    Single(String),
    Shards { stem: String, count: u32 },
}

struct FilenameEntry<'a> {
    file: &'a RepositoryFile,
    index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CandidateMaterialization {
    EligibleForInspection { artifact_id: String },
    UnsupportedPath,
    EmptyFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufCandidateFile {
    pub path: ObservedRepositoryPath,
    pub size_bytes: Option<u64>,
    pub source_sha256: Option<Sha256Digest>,
    pub materialization: CandidateMaterialization,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GgufFilenameGroupHint {
    SingleFile,
    ShardPattern {
        stem: String,
        advertised_count: u32,
        observed_indices: BTreeSet<u32>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GgufCandidateIssue {
    UnknownFileSize {
        path: ObservedRepositoryPath,
    },
    UnsupportedPath {
        path: ObservedRepositoryPath,
    },
    EmptyFile {
        path: ObservedRepositoryPath,
    },
    InvalidFilenameOrdinal,
    IncompleteFilenameGroup {
        advertised_count: u32,
        observed_count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GgufCandidateStatus {
    NeedsContentInspection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufVariantCandidate {
    pub id: Sha256Digest,
    pub snapshot_digest: Sha256Digest,
    pub hints_version: String,
    pub status: GgufCandidateStatus,
    pub filename_hint: GgufFilenameGroupHint,
    pub files: Vec<GgufCandidateFile>,
    pub issues: Vec<GgufCandidateIssue>,
}

fn filename_shard(path: &str) -> Option<(String, u32, u32)> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        Regex::new(r"^(.*)-([0-9]{5})-of-([0-9]{5})\.(?i:gguf)$")
            .expect("Static GGUF filename pattern is valid")
    });
    let captures = pattern.captures(path)?;
    Some((
        captures.get(1)?.as_str().to_string(),
        captures.get(2)?.as_str().parse().ok()?,
        captures.get(3)?.as_str().parse().ok()?,
    ))
}

fn file_candidate(
    file: &RepositoryFile,
    snapshot_id: &str,
    issues: &mut Vec<GgufCandidateIssue>,
) -> Result<GgufCandidateFile> {
    let materialization = match file.path.materializable_path() {
        Err(_) => {
            issues.push(GgufCandidateIssue::UnsupportedPath {
                path: file.path.clone(),
            });
            CandidateMaterialization::UnsupportedPath
        }
        Ok(_) if file.size_bytes == Some(0) => {
            issues.push(GgufCandidateIssue::EmptyFile {
                path: file.path.clone(),
            });
            CandidateMaterialization::EmptyFile
        }
        Ok(path) => CandidateMaterialization::EligibleForInspection {
            artifact_id: source_artifact_id(snapshot_id, &path, FilePurpose::GgufWeight)?,
        },
    };
    if file.size_bytes.is_none() {
        issues.push(GgufCandidateIssue::UnknownFileSize {
            path: file.path.clone(),
        });
    }
    Ok(GgufCandidateFile {
        path: file.path.clone(),
        size_bytes: file.size_bytes,
        source_sha256: file.sha256.clone(),
        materialization,
    })
}

pub fn discover_gguf_variant_candidates(
    inventory: &RepositoryInventory,
) -> Result<Vec<GgufVariantCandidate>> {
    inventory.validate()?;
    let snapshot_digest = inventory.snapshot.content_digest()?;
    let mut groups: BTreeMap<FilenameGroupKey, Vec<FilenameEntry<'_>>> = BTreeMap::new();
    for file in inventory
        .files
        .values()
        .filter(|file| file.path.as_str().to_ascii_lowercase().ends_with(".gguf"))
    {
        let (key, index) = match filename_shard(file.path.as_str()) {
            Some((stem, index, count)) => (FilenameGroupKey::Shards { stem, count }, Some(index)),
            None => (
                FilenameGroupKey::Single(file.path.as_str().to_string()),
                None,
            ),
        };
        groups
            .entry(key)
            .or_default()
            .push(FilenameEntry { file, index });
    }
    let mut candidates = Vec::with_capacity(groups.len());
    for (key, entries) in groups {
        let mut issues = Vec::new();
        let indices = entries
            .iter()
            .filter_map(|entry| entry.index)
            .collect::<BTreeSet<_>>();
        let filename_hint = match key {
            FilenameGroupKey::Single(_) => GgufFilenameGroupHint::SingleFile,
            FilenameGroupKey::Shards { stem, count } => {
                if count == 0
                    || indices.first() == Some(&0)
                    || indices.last().is_some_and(|last| *last > count)
                {
                    issues.push(GgufCandidateIssue::InvalidFilenameOrdinal);
                }
                if count as usize != indices.len()
                    || indices.first() != Some(&1)
                    || indices.last() != Some(&count)
                {
                    issues.push(GgufCandidateIssue::IncompleteFilenameGroup {
                        advertised_count: count,
                        observed_count: indices.len(),
                    });
                }
                GgufFilenameGroupHint::ShardPattern {
                    stem,
                    advertised_count: count,
                    observed_indices: indices,
                }
            }
        };
        let files = entries
            .into_iter()
            .map(|entry| file_candidate(entry.file, snapshot_digest.as_str(), &mut issues))
            .collect::<Result<Vec<_>>>()?;
        let paths = files.iter().map(|file| &file.path).collect::<Vec<_>>();
        candidates.push(GgufVariantCandidate {
            id: Sha256Digest::of_serializable(&(&snapshot_digest, paths))?,
            snapshot_digest: snapshot_digest.clone(),
            hints_version: VARIANT_FILENAME_HINTS_VERSION.into(),
            status: GgufCandidateStatus::NeedsContentInspection,
            filename_hint,
            files,
            issues,
        });
    }
    candidates.sort_by(|left, right| left.files[0].path.cmp(&right.files[0].path));
    Ok(candidates)
}
