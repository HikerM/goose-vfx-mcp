use agent_client_protocol::{JsonRpcRequest, JsonRpcResponse};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProfileDto {
    #[serde(default)]
    pub manifests: Vec<String>,
    #[serde(default)]
    pub detected_stacks: Vec<String>,
    pub git_repository: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub canonical_root: String,
    pub profile: ProjectProfileDto,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_opened_at_ms: i64,
    pub archived_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectWorkItemStatus {
    #[default]
    Draft,
    Planned,
    Running,
    Validating,
    Review,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkItemDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    pub status: ProjectWorkItemStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRunStatus {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRunPhase {
    #[default]
    Discovering,
    Planning,
    AwaitingApproval,
    Executing,
    Validating,
    Reviewing,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRunDto {
    pub id: String,
    pub work_item_id: String,
    pub session_id: Option<String>,
    pub status: ProjectRunStatus,
    pub phase: ProjectRunPhase,
    pub started_at_ms: Option<i64>,
    pub updated_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRunStepStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRunStepDto {
    pub id: String,
    pub run_id: String,
    pub sequence: i64,
    pub kind: String,
    pub title: String,
    pub status: ProjectRunStepStatus,
    pub detail: serde_json::Value,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSnapshotDto {
    pub project: ProjectDto,
    #[serde(default)]
    pub work_items: Vec<ProjectWorkItemDto>,
    #[serde(default)]
    pub runs: Vec<ProjectRunDto>,
    #[serde(default)]
    pub run_steps: Vec<ProjectRunStepDto>,
    #[serde(default)]
    pub checkpoints: Vec<ProjectCheckpointDto>,
    pub interrupted_run_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCheckpointDto {
    pub id: String,
    pub run_id: String,
    pub sequence: i64,
    pub phase: ProjectRunPhase,
    pub state: serde_json::Value,
    pub safe_to_resume: bool,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectChangeSetStatus {
    #[default]
    Open,
    Conflict,
    ReadyForReview,
    Accepted,
    Reverted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChangeSetDto {
    pub id: String,
    pub run_id: String,
    pub base_revision: Option<String>,
    pub status: ProjectChangeSetStatus,
    pub summary: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_lumina/project/open", response = ProjectOpenResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOpenRequest {
    pub root_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOpenResponse {
    pub project: ProjectDto,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_lumina/project/list", response = ProjectListResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListRequest {
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListResponse {
    #[serde(default)]
    pub projects: Vec<ProjectDto>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_lumina/project/snapshot", response = ProjectSnapshotResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSnapshotRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSnapshotResponse {
    pub snapshot: ProjectSnapshotDto,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "_lumina/project/work-items/create",
    response = ProjectWorkItemResponse
)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkItemCreateRequest {
    pub project_id: String,
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkItemResponse {
    pub work_item: ProjectWorkItemDto,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "_lumina/project/work-items/complete",
    response = ProjectWorkItemResponse
)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkItemCompleteRequest {
    pub work_item_id: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_lumina/project/runs/start", response = ProjectRunResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRunStartRequest {
    pub work_item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRunResponse {
    pub run: ProjectRunDto,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_lumina/project/runs/phase", response = ProjectRunResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRunPhaseRequest {
    pub run_id: String,
    pub phase: ProjectRunPhase,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_lumina/project/runs/finish", response = ProjectRunResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRunFinishRequest {
    pub run_id: String,
    pub status: ProjectRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "_lumina/project/checkpoints/create",
    response = ProjectCheckpointResponse
)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCheckpointCreateRequest {
    pub run_id: String,
    pub phase: ProjectRunPhase,
    #[serde(default)]
    pub state: serde_json::Value,
    pub safe_to_resume: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCheckpointResponse {
    pub checkpoint: ProjectCheckpointDto,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "_lumina/project/change-sets/get",
    response = ProjectChangeSetResponse
)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChangeSetGetRequest {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChangeSetResponse {
    pub change_set: ProjectChangeSetDto,
}
