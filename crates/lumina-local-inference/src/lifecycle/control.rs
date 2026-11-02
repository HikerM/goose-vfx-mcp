use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationJobAction {
    Pause,
    Resume,
    Cancel,
    Retry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationControlRequest {
    pub job_id: String,
    pub expected_version: u64,
    pub action: InstallationJobAction,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationControlOutcome {
    pub job: super::LifecycleJob,
    pub replayed: bool,
}
