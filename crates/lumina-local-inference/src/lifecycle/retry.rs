use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use super::{LifecycleErrorCode, LifecycleStage};

pub const FILE_RETRY_POLICY_VERSION: &str = "installation-file-retry-v1";
pub const FILE_RETRY_MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryHistoryOrigin {
    FromEnqueue,
    LegacyUntracked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileRetryState {
    Ready,
    Active,
    Waiting { retry_at: i64 },
    Exhausted,
    NeedsAction,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationFileFailure {
    pub code: LifecycleErrorCode,
    pub stage: LifecycleStage,
    pub failed_at: i64,
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub struct FileAttemptFailure {
    pub code: LifecycleErrorCode,
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRetryBudget {
    pub job_id: String,
    pub artifact_id: String,
    pub policy_version: String,
    pub max_attempts: u32,
    pub cycle: u64,
    pub cycle_attempts: u32,
    pub recorded_attempts: u64,
    pub history_origin: RetryHistoryOrigin,
    pub tracking_started_at: i64,
    pub state: FileRetryState,
    pub last_failure: Option<InstallationFileFailure>,
    pub version: u64,
    pub updated_at: i64,
}

impl FileRetryBudget {
    pub fn next_retry_at(&self) -> Option<i64> {
        match self.state {
            FileRetryState::Waiting { retry_at } => Some(retry_at),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.job_id.is_empty()
                && !self.artifact_id.is_empty()
                && self.policy_version == FILE_RETRY_POLICY_VERSION
                && self.max_attempts == FILE_RETRY_MAX_ATTEMPTS
                && self.cycle_attempts <= self.max_attempts
                && self.recorded_attempts >= u64::from(self.cycle_attempts)
                && self.tracking_started_at >= 0
                && self.updated_at >= self.tracking_started_at,
            "Invalid installation file retry budget"
        );
        if let Some(failure) = &self.last_failure {
            ensure!(
                (self.tracking_started_at..=self.updated_at).contains(&failure.failed_at),
                "Invalid installation failure time"
            );
        }
        match self.state {
            FileRetryState::Waiting { retry_at } => ensure!(
                self.cycle_attempts > 0
                    && self.cycle_attempts < self.max_attempts
                    && retry_at > self.updated_at
                    && self
                        .last_failure
                        .as_ref()
                        .is_some_and(|f| f.code.retryable()),
                "Invalid scheduled installation retry"
            ),
            FileRetryState::Exhausted => ensure!(
                self.cycle_attempts == self.max_attempts
                    && self
                        .last_failure
                        .as_ref()
                        .is_some_and(|f| f.code.retryable()),
                "Invalid exhausted installation retry budget"
            ),
            FileRetryState::NeedsAction => ensure!(
                self.last_failure
                    .as_ref()
                    .is_some_and(|f| !f.code.retryable()),
                "Installation retry requires an actionable failure"
            ),
            FileRetryState::Active => ensure!(
                self.cycle_attempts > 0,
                "Active installation file has no attempt"
            ),
            FileRetryState::Ready | FileRetryState::Verified => {}
        }
        Ok(())
    }
}
