use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::LifecycleStage;

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
pub struct LifecycleFailure {
    pub code: LifecycleErrorCode,
    pub stage: LifecycleStage,
    pub message_key: String,
    pub message_args: BTreeMap<String, String>,
    pub recoverable: bool,
    pub job_id: Option<String>,
    pub cause_id: Option<String>,
}
