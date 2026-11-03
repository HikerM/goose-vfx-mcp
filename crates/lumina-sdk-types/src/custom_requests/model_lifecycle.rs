use agent_client_protocol::{JsonRpcRequest, JsonRpcResponse};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationServiceState {
    Dormant,
    Recovering,
    Ready,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationJobStatus {
    Queued,
    Running,
    Paused,
    Cancelling,
    Cancelled,
    Completed,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationStage {
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationJobDto {
    pub id: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub stage: ModelInstallationStage,
    pub status: ModelInstallationJobStatus,
    pub version: u64,
    pub generation: u64,
    pub accepted_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationJobAction {
    #[default]
    Pause,
    Resume,
    Cancel,
    Retry,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method="_lumina/unstable/local-inference/installations/submit",response=ModelInstallationSubmitResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelInstallationSubmitRequest {
    pub plan_id: String,
    pub plan_digest: String,
    pub installation_id: String,
    pub idempotency_key: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationSubmitResponse {
    pub job: ModelInstallationJobDto,
    pub version_id: String,
}

fn default_limit() -> u32 {
    64
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method="_lumina/unstable/local-inference/installations/jobs/events",response=ModelInstallationJobEventsResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelInstallationJobEventsRequest {
    pub job_id: String,
    #[serde(default)]
    pub after_job_sequence: u64,
    #[serde(default)]
    pub after_file_sequence: u64,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

impl Default for ModelInstallationJobEventsRequest {
    fn default() -> Self {
        Self {
            job_id: String::new(),
            after_job_sequence: 0,
            after_file_sequence: 0,
            limit: default_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationJobEventsResponse {
    pub job_events: Vec<ModelInstallationJobEventDto>,
    pub file_events: Vec<ModelInstallationFileEventDto>,
    pub next_job_sequence: u64,
    pub next_file_sequence: u64,
    pub has_more_job_events: bool,
    pub has_more_file_events: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationJobEventDto {
    pub sequence: u64,
    pub job: ModelInstallationJobDto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationFileStatus {
    Pending,
    Downloading,
    Downloaded,
    Verified,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationFileRestartReason {
    RangeUnavailable,
    ValidatorChanged,
    PartialContentMismatch,
    HashMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationFileEventDto {
    pub sequence: u64,
    pub artifact_id: String,
    pub version: u64,
    pub generation: u64,
    pub completed_bytes: u64,
    pub expected_bytes: u64,
    pub status: ModelInstallationFileStatus,
    pub failure_code: Option<ModelInstallationFailureCode>,
    pub restart_reason: Option<ModelInstallationFileRestartReason>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method="_lumina/unstable/local-inference/installations/jobs/list",response=ModelInstallationJobsListResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelInstallationJobsListRequest {
    pub after_id: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}
impl Default for ModelInstallationJobsListRequest {
    fn default() -> Self {
        Self {
            after_id: None,
            limit: default_limit(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationJobsListResponse {
    pub jobs: Vec<ModelInstallationJobDto>,
    #[serde(default)]
    pub file_retries: Vec<ModelInstallationFileRetryDto>,
    #[serde(default)]
    pub failures: Vec<ModelInstallationFailureDto>,
    #[serde(default)]
    pub storage_reservations: Vec<ModelInstallationStorageDto>,
    pub next_after_id: Option<String>,
    pub service_state: ModelInstallationServiceState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationFileRetryState {
    Ready,
    Active,
    Waiting,
    Exhausted,
    NeedsAction,
    Verified,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ModelInstallationFailureCode {
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationRetryHistoryOrigin {
    FromEnqueue,
    LegacyUntracked,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationFileFailureDto {
    pub code: ModelInstallationFailureCode,
    pub stage: ModelInstallationStage,
    pub failed_at: i64,
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationFailureOrigin {
    Credentials,
    Admission,
    Transfer,
    Verification,
    Commit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelInstallationStorageState {
    NeedsAdmission,
    Held,
    Released,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationStorageDto {
    pub job_id: String,
    pub plan_digest: String,
    pub volume_id: Option<String>,
    pub peak_bytes: u64,
    pub reserved_bytes: u64,
    pub state: ModelInstallationStorageState,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationFailureDto {
    pub job_id: String,
    pub job_version: u64,
    pub code: ModelInstallationFailureCode,
    pub stage: ModelInstallationStage,
    pub origin: ModelInstallationFailureOrigin,
    pub artifact_id: Option<String>,
    pub message_key: String,
    pub message_args: std::collections::BTreeMap<String, String>,
    pub retryable: bool,
    pub recoverable: bool,
    pub cause_id: String,
    pub automatic_retry_at: Option<i64>,
    pub is_current: bool,
    pub occurred_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationFileRetryDto {
    pub job_id: String,
    pub artifact_id: String,
    pub policy_version: String,
    pub max_attempts: u32,
    pub cycle: u64,
    pub cycle_attempts: u32,
    pub recorded_attempts: u64,
    pub history_origin: ModelInstallationRetryHistoryOrigin,
    pub tracking_started_at: i64,
    pub state: ModelInstallationFileRetryState,
    pub next_retry_at: Option<i64>,
    pub last_failure: Option<ModelInstallationFileFailureDto>,
    pub version: u64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method="_lumina/unstable/local-inference/installations/jobs/control",response=ModelInstallationJobControlResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelInstallationJobControlRequest {
    pub job_id: String,
    pub expected_version: u64,
    pub action: ModelInstallationJobAction,
    pub idempotency_key: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallationJobControlResponse {
    pub job: ModelInstallationJobDto,
    pub replayed: bool,
}
