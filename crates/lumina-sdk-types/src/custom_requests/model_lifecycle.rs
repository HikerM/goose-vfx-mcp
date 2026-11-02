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
    pub next_after_id: Option<String>,
    pub service_state: ModelInstallationServiceState,
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
