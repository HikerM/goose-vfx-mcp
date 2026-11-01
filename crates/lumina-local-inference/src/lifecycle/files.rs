use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use super::{
    FilePurpose, GgufReadLimits, InstallPlan, LifecycleErrorCode, RepositoryPath, Sha256Digest,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResumeValidator {
    Unknown,
    StrongEtag { value: String },
}

impl ResumeValidator {
    pub fn validate(&self) -> Result<()> {
        if let Self::StrongEtag { value } = self {
            ensure!(
                value.len() >= 2
                    && value.len() <= 8192
                    && value.starts_with('"')
                    && value.ends_with('"'),
                "Invalid strong HTTP ETag"
            );
            ensure!(
                value.as_bytes()[1..value.len() - 1]
                    .iter()
                    .all(|byte| *byte >= 0x21 && *byte != b'"' && *byte != 0x7f),
                "Invalid strong HTTP ETag content"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferCheckpoint {
    pub completed_bytes: u64,
    pub prefix_sha256: Sha256Digest,
    pub resume_validator: ResumeValidator,
}

impl TransferCheckpoint {
    pub fn empty() -> Self {
        Self {
            completed_bytes: 0,
            prefix_sha256: Sha256Digest::of_bytes(b""),
            resume_validator: ResumeValidator::Unknown,
        }
    }

    pub fn validate(&self, expected_bytes: u64) -> Result<()> {
        ensure!(
            self.completed_bytes <= expected_bytes,
            "File checkpoint exceeds pinned size"
        );
        ensure!(
            self.completed_bytes != 0 || self.prefix_sha256 == Sha256Digest::of_bytes(b""),
            "Empty file checkpoint has a nonempty digest"
        );
        self.resume_validator.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FileTransferState {
    Pending,
    Downloading,
    Downloaded,
    Verified {
        computed_sha256: Sha256Digest,
        evidence_digest: Sha256Digest,
    },
    Failed {
        code: LifecycleErrorCode,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRestartReason {
    RangeUnavailable,
    ValidatorChanged,
    PartialContentMismatch,
    HashMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTransferRecord {
    pub job_id: String,
    pub artifact_id: String,
    pub relative_path: RepositoryPath,
    pub expected_bytes: u64,
    pub checkpoint: TransferCheckpoint,
    pub state: FileTransferState,
    pub restart_reason: Option<FileRestartReason>,
    pub version: u64,
    pub generation: u64,
    pub updated_at: i64,
}

impl FileTransferRecord {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.job_id.is_empty() && !self.artifact_id.is_empty(),
            "Missing file transfer identity"
        );
        self.checkpoint.validate(self.expected_bytes)?;
        match &self.state {
            FileTransferState::Pending => ensure!(
                self.checkpoint.completed_bytes == 0,
                "Pending file has completed bytes"
            ),
            FileTransferState::Downloaded => ensure!(
                self.checkpoint.completed_bytes == self.expected_bytes,
                "Downloaded file is incomplete"
            ),
            FileTransferState::Verified {
                computed_sha256, ..
            } => ensure!(
                self.checkpoint.completed_bytes == self.expected_bytes
                    && computed_sha256 == &self.checkpoint.prefix_sha256,
                "Verified file checkpoint does not match its content"
            ),
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct VerifiedArtifactFile {
    pub(crate) artifact_id: String,
    pub(crate) relative_path: RepositoryPath,
    pub(crate) plan_digest: Sha256Digest,
    pub(crate) size_bytes: u64,
    pub(crate) computed_sha256: Sha256Digest,
    pub(crate) evidence_digest: Sha256Digest,
    pub(crate) gguf: Option<super::GgufInspection>,
}

impl VerifiedArtifactFile {
    pub fn sha256(&self) -> &Sha256Digest {
        &self.computed_sha256
    }
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
    pub fn evidence_digest(&self) -> &Sha256Digest {
        &self.evidence_digest
    }
}

pub fn verify_artifact_file(
    plan: &InstallPlan,
    artifact_id: &str,
    root: &Path,
    relative_path: &RepositoryPath,
) -> Result<VerifiedArtifactFile> {
    verify_artifact_file_with_progress(plan, artifact_id, root, relative_path, |_| Ok(()))
}

pub(crate) fn verify_artifact_file_with_progress(
    plan: &InstallPlan,
    artifact_id: &str,
    root: &Path,
    relative_path: &RepositoryPath,
    mut progress: impl FnMut(u64) -> Result<()>,
) -> Result<VerifiedArtifactFile> {
    let artifact = plan
        .artifacts
        .get(artifact_id)
        .context("Artifact is absent from the installation plan")?;
    artifact.validate()?;
    let path = checked_local_path(root, relative_path)?;
    let mut file = open_for_verification(&path)?;
    let before = file.metadata()?;
    ensure!(before.is_file(), "Artifact is not a regular file");
    let expected_bytes = artifact.size_bytes.context("Artifact size is unknown")?;
    if artifact.purpose != FilePurpose::GgufWeight {
        ensure!(
            expected_bytes <= 64 * 1024 * 1024,
            "Artifact metadata exceeds the content inspection budget"
        );
    }
    ensure!(
        before.len() == expected_bytes,
        "Artifact size differs from the pinned source"
    );
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut bytes = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .context("Artifact byte count overflow")?;
        ensure!(bytes <= expected_bytes, "Artifact grew during verification");
        hasher.update(&buffer[..read]);
        progress(bytes)?;
    }
    let after = file.metadata()?;
    ensure!(
        bytes == expected_bytes
            && after.len() == before.len()
            && after.modified()? == before.modified()?,
        "Artifact changed during verification"
    );
    let computed_sha256 = Sha256Digest::from_sha256(hasher);
    ensure!(
        artifact
            .source_sha256
            .as_ref()
            .is_none_or(|expected| expected == &computed_sha256),
        "Artifact SHA256 differs from the pinned source"
    );
    let gguf = if artifact.purpose == FilePurpose::GgufWeight {
        file.rewind()?;
        let limits = if plan.runtime.adapter_id == "llamacpp" {
            GgufReadLimits::for_llama_cpp()
        } else {
            GgufReadLimits::default()
        };
        let inspection = super::inspect_gguf(BufReader::new(&mut file), expected_bytes, limits)?;
        ensure!(
            !inspection.tensors.is_empty(),
            "GGUF weight contains no tensors"
        );
        if matches!(
            plan.rules_version.as_str(),
            super::OBSERVED_GGUF_PLAN_RULES_VERSION | super::PREPARED_GGUF_PLAN_RULES_VERSION
        ) {
            let evidence = super::gguf_header_evidence_id(artifact_id, &inspection.header_digest);
            ensure!(
                plan.dependencies
                    .components
                    .values()
                    .any(|component| component.artifact_ids.contains(artifact_id)),
                "Observed GGUF artifact is not assigned to a component"
            );
            ensure!(
                plan.dependencies
                    .components
                    .values()
                    .filter(|component| component.artifact_ids.contains(artifact_id))
                    .all(|component| component.evidence_ids.contains(&evidence)),
                "Downloaded GGUF header differs from the accepted source observation"
            );
        }
        Some(inspection)
    } else {
        file.rewind()?;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .context("Artifact metadata is not UTF-8 text")?;
        if artifact.path.as_str().ends_with(".json") {
            serde_json::from_str::<serde_json::Value>(&text)
                .context("Artifact metadata is not valid JSON")?;
        }
        None
    };
    let inspected = file.metadata()?;
    ensure!(
        inspected.len() == before.len() && inspected.modified()? == before.modified()?,
        "Artifact changed during structure inspection"
    );
    let plan_digest = plan.digest()?;
    let evidence_digest = Sha256Digest::of_serializable(&(
        "artifact-verification-v1",
        &plan_digest,
        artifact_id,
        relative_path,
        expected_bytes,
        &computed_sha256,
        gguf.as_ref().map(|inspection| &inspection.header_digest),
    ))?;
    Ok(VerifiedArtifactFile {
        artifact_id: artifact_id.to_string(),
        relative_path: relative_path.clone(),
        plan_digest,
        size_bytes: expected_bytes,
        computed_sha256,
        evidence_digest,
        gguf,
    })
}

pub fn verify_partial_file(
    root: &Path,
    relative_path: &RepositoryPath,
    checkpoint: &TransferCheckpoint,
    expected_bytes: u64,
) -> Result<()> {
    checkpoint.validate(expected_bytes)?;
    let path = checked_local_path(root, relative_path)?;
    let file = open_for_verification(&path)?;
    ensure!(
        file.metadata()?.len() >= checkpoint.completed_bytes
            && file.metadata()?.len() <= expected_bytes,
        "Partial artifact length is inconsistent with its checkpoint"
    );
    let mut hasher = Sha256::new();
    let mut remaining = file.take(checkpoint.completed_bytes);
    let mut buffer = vec![0; 1024 * 1024];
    let mut count = 0u64;
    loop {
        let read = remaining.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        count += read as u64;
    }
    ensure!(
        count == checkpoint.completed_bytes
            && Sha256Digest::from_sha256(hasher) == checkpoint.prefix_sha256,
        "Partial artifact checkpoint hash mismatch"
    );
    Ok(())
}

pub(crate) fn checked_local_path(root: &Path, relative: &RepositoryPath) -> Result<PathBuf> {
    ensure!(root.is_absolute(), "Artifact root must be absolute");
    let mut path = root.canonicalize()?;
    for segment in relative.as_str().split('/') {
        path.push(segment);
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            !is_redirect(&metadata),
            "Artifact path contains a filesystem redirect"
        );
    }
    Ok(path)
}

pub(crate) fn open_for_verification(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    Ok(options.open(path)?)
}

#[cfg(windows)]
fn is_redirect(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_redirect(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}
