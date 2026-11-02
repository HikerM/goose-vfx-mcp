use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{JobStatus, LifecycleJob, LifecycleStage};
use anyhow::{ensure, Context, Result};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LifecycleErrorCode {
    SourceNotFound,
    SourceAuthRequired,
    SourceForbidden,
    SourceTimeout,
    SourceRateLimited,
    SourceUnavailable,
    SourcePartialFailure,
    RevisionUnresolved,
    RevisionChanged,
    NoGguf,
    GgufInvalid,
    FormatForbidden,
    ShardIncomplete,
    DependencyMissing,
    DependencyIncompatible,
    DependencyUnverified,
    RuntimeUnsupported,
    HardwareInsufficient,
    DiskInsufficient,
    HashMismatch,
    DestinationLocked,
    PlanStale,
    StateConflict,
    DownloadFailed,
    InstallFailed,
    StartFailed,
    ValidationFailed,
    Cancelled,
}

impl LifecycleErrorCode {
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::SourceTimeout | Self::SourceRateLimited | Self::SourceUnavailable
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleFailure {
    pub code: LifecycleErrorCode,
    pub stage: LifecycleStage,
    pub message_key: String,
    pub message_args: BTreeMap<String, String>,
    pub recoverable: bool,
    pub job_id: Option<String>,
    pub cause_id: Option<String>,
}

pub const INSTALLATION_FAILURE_RULES_VERSION: &str = "installation-failure-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationFailureOrigin {
    Credentials,
    Admission,
    Transfer,
    Verification,
    Commit,
}

impl InstallationFailureOrigin {
    pub fn for_stage(stage: LifecycleStage) -> Self {
        match stage {
            LifecycleStage::Downloading => Self::Transfer,
            LifecycleStage::Verifying => Self::Verification,
            LifecycleStage::Installing => Self::Commit,
            _ => Self::Admission,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationFailureDetails {
    pub code: LifecycleErrorCode,
    pub origin: InstallationFailureOrigin,
    pub artifact_id: Option<String>,
    pub automatic_retry_at: Option<i64>,
}

impl InstallationFailureDetails {
    pub fn operation(code: LifecycleErrorCode, origin: InstallationFailureOrigin) -> Self {
        Self {
            code,
            origin,
            artifact_id: None,
            automatic_retry_at: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationFailureRecord {
    pub rules_version: String,
    pub job: LifecycleJob,
    pub details: InstallationFailureDetails,
    pub failure: LifecycleFailure,
    pub occurred_at: i64,
}

impl InstallationFailureRecord {
    pub(crate) fn new(
        job: LifecycleJob,
        details: InstallationFailureDetails,
        now: i64,
    ) -> Result<Self> {
        let code = serde_json::to_value(details.code)?;
        let name = code
            .as_str()
            .context("Installation failure code is not a string")?;
        let failure = LifecycleFailure {
            code: details.code,
            stage: job.stage,
            message_key: format!("models.installation.failure.{}", name.to_ascii_lowercase()),
            message_args: details
                .artifact_id
                .as_ref()
                .map(|id| ("artifactId".into(), id.clone()))
                .into_iter()
                .collect(),
            recoverable: matches!(
                details.code,
                LifecycleErrorCode::SourceAuthRequired
                    | LifecycleErrorCode::SourceForbidden
                    | LifecycleErrorCode::SourceTimeout
                    | LifecycleErrorCode::SourceRateLimited
                    | LifecycleErrorCode::SourceUnavailable
                    | LifecycleErrorCode::DiskInsufficient
                    | LifecycleErrorCode::DestinationLocked
                    | LifecycleErrorCode::HashMismatch
                    | LifecycleErrorCode::DownloadFailed
                    | LifecycleErrorCode::InstallFailed
            ),
            job_id: Some(job.id.clone()),
            cause_id: Some(format!("installation-failure:{}:{}", job.id, job.version)),
        };
        Ok(Self {
            rules_version: INSTALLATION_FAILURE_RULES_VERSION.into(),
            job,
            details,
            failure,
            occurred_at: now,
        })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.rules_version == INSTALLATION_FAILURE_RULES_VERSION
                && self.occurred_at == self.job.updated_at
                && self.occurred_at >= self.job.accepted_at
                && matches!(
                    self.job.status,
                    JobStatus::Failed | JobStatus::Blocked | JobStatus::Queued
                ),
            "Invalid installation failure identity"
        );
        ensure!(
            self == &Self::new(self.job.clone(), self.details.clone(), self.occurred_at)?,
            "Installation failure details do not match their canonical fields"
        );
        ensure!(
            self.details.automatic_retry_at.is_some() == (self.job.status == JobStatus::Queued),
            "Installation failure retry state mismatch"
        );
        if let Some(at) = self.details.automatic_retry_at {
            ensure!(
                self.details.code.retryable()
                    && at > self.occurred_at
                    && self.details.artifact_id.is_some(),
                "Invalid automatic installation failure retry"
            );
        }
        if self.details.artifact_id.is_some() {
            ensure!(
                self.details.origin == InstallationFailureOrigin::for_stage(self.job.stage)
                    && matches!(
                        self.job.stage,
                        LifecycleStage::Downloading | LifecycleStage::Verifying
                    ),
                "Installation file failure has a different stage"
            );
        } else {
            ensure!(
                self.details.automatic_retry_at.is_none(),
                "Non-file failure cannot schedule a file retry"
            );
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct InstallationOperationError {
    pub code: LifecycleErrorCode,
    pub origin: InstallationFailureOrigin,
}

impl Display for InstallationOperationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Installation {:?}: {:?}", self.origin, self.code)
    }
}

impl std::error::Error for InstallationOperationError {}

pub(crate) fn installation_failure_code(error: &anyhow::Error) -> LifecycleErrorCode {
    error
        .downcast_ref::<InstallationOperationError>()
        .map(|error| error.code)
        .or_else(|| {
            error
                .downcast_ref::<super::ModelSourceError>()
                .map(|error| error.code)
        })
        .unwrap_or(LifecycleErrorCode::InstallFailed)
}

pub fn validate_installation_admission(
    plan: &super::InstallPlan,
    accepted_at: i64,
    available_bytes: u64,
) -> Result<()> {
    if available_bytes < plan.required_disk_bytes {
        return Err(InstallationOperationError {
            code: LifecycleErrorCode::DiskInsufficient,
            origin: InstallationFailureOrigin::Admission,
        }
        .into());
    }
    plan.validate_for_install(accepted_at, available_bytes)
        .map_err(|_| {
            InstallationOperationError {
                code: LifecycleErrorCode::InstallFailed,
                origin: InstallationFailureOrigin::Admission,
            }
            .into()
        })
}

pub(crate) fn validate_installation_store_admission(
    repository: &super::LifecycleRepository,
    store: &super::InstallationStore,
    job: &LifecycleJob,
    plan: &super::InstallPlan,
    now: i64,
) -> Result<()> {
    let result = fs2::available_space(store.root())
        .map_err(|_| {
            anyhow::Error::new(InstallationOperationError {
                code: LifecycleErrorCode::InstallFailed,
                origin: InstallationFailureOrigin::Admission,
            })
        })
        .and_then(|available| validate_installation_admission(plan, job.accepted_at, available));
    if let Err(error) = &result {
        repository.block_installation(
            job,
            store,
            InstallationFailureDetails::operation(
                installation_failure_code(error),
                InstallationFailureOrigin::Admission,
            ),
            now,
        )?;
    }
    result
}
