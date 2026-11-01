use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{ModelCapability, RuntimeBuild, Sha256Digest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStage {
    Discovered,
    RepositoryVerified,
    FilesDiscovered,
    ModelClassified,
    DependenciesResolved,
    CompatibilityChecked,
    ReadyToInstall,
    Downloading,
    Verifying,
    Installing,
    Installed,
    Starting,
    Validating,
}

impl LifecycleStage {
    pub fn advance(self, next: Self) -> Result<Self> {
        let allowed = matches!(
            (self, next),
            (Self::Discovered, Self::RepositoryVerified)
                | (Self::RepositoryVerified, Self::FilesDiscovered)
                | (Self::FilesDiscovered, Self::ModelClassified)
                | (Self::ModelClassified, Self::DependenciesResolved)
                | (Self::DependenciesResolved, Self::CompatibilityChecked)
                | (Self::CompatibilityChecked, Self::ReadyToInstall)
                | (Self::ReadyToInstall, Self::Downloading)
                | (Self::Downloading, Self::Verifying)
                | (Self::Verifying, Self::Installing)
                | (Self::Installing, Self::Installed)
                | (Self::Installed, Self::Starting)
                | (Self::Starting, Self::Validating)
        );
        ensure!(
            allowed,
            "Illegal model lifecycle transition: {self:?} -> {next:?}"
        );
        Ok(next)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Paused,
    Cancelling,
    Cancelled,
    Completed,
    Blocked,
    Failed,
}

impl JobStatus {
    pub fn transition(self, next: Self) -> Result<Self> {
        let allowed = matches!(
            (self, next),
            (
                Self::Queued,
                Self::Running | Self::Cancelled | Self::Blocked
            ) | (
                Self::Running,
                Self::Paused | Self::Cancelling | Self::Completed | Self::Blocked | Self::Failed
            ) | (Self::Paused, Self::Queued | Self::Cancelled)
                | (Self::Cancelling, Self::Cancelled | Self::Failed)
                | (Self::Failed | Self::Blocked, Self::Queued)
        );
        ensure!(allowed, "Illegal job transition: {self:?} -> {next:?}");
        Ok(next)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Unverified,
    Validating,
    Ready,
    Failed,
    Unsupported,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIdentity {
    pub manifest_digest: Sha256Digest,
    pub runtime: RuntimeBuild,
    pub launch_digest: Sha256Digest,
    pub suite_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEvidence {
    pub installation_id: String,
    pub capability: ModelCapability,
    pub identity: ValidationIdentity,
    pub status: CapabilityStatus,
    pub input_digest: Sha256Digest,
    pub output_digest: Option<Sha256Digest>,
    pub evidence_path: String,
    pub validated_at: i64,
}

impl CapabilityEvidence {
    pub fn status_for(&self, identity: &ValidationIdentity) -> CapabilityStatus {
        if &self.identity != identity {
            CapabilityStatus::Stale
        } else {
            self.status
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.installation_id.is_empty(),
            "Missing installation identity"
        );
        ensure!(
            !self.identity.suite_version.is_empty(),
            "Missing validation suite version"
        );
        if self.status == CapabilityStatus::Ready {
            ensure!(
                self.output_digest.is_some() && !self.evidence_path.is_empty(),
                "READY requires actual output evidence"
            );
        }
        Ok(())
    }
}

pub fn profile_ready(
    required: &[ModelCapability],
    states: &BTreeMap<ModelCapability, CapabilityStatus>,
) -> bool {
    !required.is_empty()
        && required
            .iter()
            .all(|capability| states.get(capability) == Some(&CapabilityStatus::Ready))
}
