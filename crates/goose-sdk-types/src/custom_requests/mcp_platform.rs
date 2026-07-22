use agent_client_protocol::{JsonRpcRequest, JsonRpcResponse};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MCP_CATALOG_LIST_METHOD: &str = "goose.mcpCatalogList_unstable";
pub const MCP_CATALOG_DETAIL_METHOD: &str = "goose.mcpCatalogDetail_unstable";
pub const MCP_PLAN_CREATE_METHOD: &str = "goose.mcpPlanCreate_unstable";
pub const MCP_INSTALL_CONFIRM_METHOD: &str = "goose.mcpInstallConfirm_unstable";
pub const MCP_TASK_GET_METHOD: &str = "goose.mcpTaskGet_unstable";
pub const MCP_TASK_CANCEL_METHOD: &str = "goose.mcpTaskCancel_unstable";
pub const MCP_TASK_RETRY_METHOD: &str = "goose.mcpTaskRetry_unstable";
pub const MCP_EVENTS_RESUME_METHOD: &str = "goose.mcpEventsResume_unstable";
pub const MCP_LIST_METHOD: &str = "goose.mcpList_unstable";
pub const MCP_GET_METHOD: &str = "goose.mcpGet_unstable";
pub const MCP_HEALTH_RUN_METHOD: &str = "goose.mcpHealthRun_unstable";
pub const MCP_HEALTH_GET_METHOD: &str = "goose.mcpHealthGet_unstable";
pub const MCP_SET_DEFAULT_ENABLED_METHOD: &str = "goose.mcpSetDefaultEnabled_unstable";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpPlatformOutcome<T> {
    Success { value: T },
    Error { error: McpPlatformErrorEnvelope },
}

impl<T> McpPlatformOutcome<T> {
    pub fn success(value: T) -> Self {
        Self::Success { value }
    }

    pub fn error(error: McpPlatformErrorEnvelope) -> Self {
        Self::Error { error }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpPlatformErrorCodeDto {
    InvalidRequest,
    NotFound,
    IntegrityError,
    PolicyDenied,
    NotImplementedForPhase,
    OperationNotSupported,
    PlanStale,
    PlanExpired,
    IdempotencyConflict,
    RevisionConflict,
    InvalidTransition,
    RepositoryUnavailable,
    ProjectionConflict,
    CredentialMissing,
    HealthFailed,
    TaskNotCancellable,
    RollbackIncomplete,
    AdapterIncompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPlatformErrorEnvelope {
    pub code: McpPlatformErrorCodeDto,
    pub message: String,
    pub retryable: bool,
    pub correlation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<McpPlatformErrorDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpPlatformErrorDetails {
    RequestRejected {},
    RecordMissing {},
    IntegrityValidationFailed {},
    PolicyDecision { reason_codes: Vec<String> },
    PhaseUnavailable { phase: String, operation: String },
    PlanMismatch {},
    PlanExpired {},
    IdempotencyConflict {},
    RevisionConflict {},
    TransitionRejected {},
    RepositoryTemporarilyUnavailable {},
    ProjectionConflict {},
    CredentialMissing {},
    HealthGateFailed {},
    CancellationDeferred {},
    RecoveryRequired {},
    AdapterVersionIncompatible {},
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpCatalogList_unstable", response = McpCatalogListResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCatalogListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust_tiers: Vec<McpTrustTier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<McpCompatibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCatalogListResponse {
    pub outcome: McpPlatformOutcome<McpCatalogPage>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpCatalogDetail_unstable", response = McpCatalogDetailResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCatalogDetailRequest {
    pub catalog: McpCatalogLocator,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCatalogDetailResponse {
    pub outcome: McpPlatformOutcome<McpCatalogDetail>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpPlanCreate_unstable", response = McpPlanCreateResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPlanCreateRequest {
    pub intent: McpPlanIntent,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPlanCreateResponse {
    pub outcome: McpPlatformOutcome<McpPlanReview>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpInstallConfirm_unstable", response = McpInstallConfirmResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpInstallConfirmRequest {
    pub plan_id: String,
    pub plan_digest: String,
    pub user_decision: McpUserDecision,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpInstallConfirmResponse {
    pub outcome: McpPlatformOutcome<McpTaskRef>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpTaskGet_unstable", response = McpTaskGetResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTaskGetRequest {
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTaskGetResponse {
    pub outcome: McpPlatformOutcome<McpTaskRef>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpTaskCancel_unstable", response = McpTaskCancelResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTaskCancelRequest {
    pub task_id: String,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTaskCancelResponse {
    pub outcome: McpPlatformOutcome<McpTaskRef>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpTaskRetry_unstable", response = McpTaskRetryResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTaskRetryRequest {
    pub task_id: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTaskRetryResponse {
    pub outcome: McpPlatformOutcome<McpTaskRef>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpEventsResume_unstable", response = McpEventsResumeResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpEventsResumeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_event_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpEventsResumeResponse {
    pub outcome: McpPlatformOutcome<McpEventsPage>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpList_unstable", response = McpListResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListRequest {
    pub cursor: Option<String>,
    pub page_size: Option<u16>,
    pub registration: Option<McpRegistrationState>,
    pub installation: Option<McpInstallationState>,
    pub runtime: Option<McpRuntimeState>,
    pub health: Option<McpHealthState>,
    pub default_enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListResponse {
    pub outcome: McpPlatformOutcome<McpManagedPage>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpGet_unstable", response = McpGetResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetRequest {
    pub managed_mcp_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGetResponse {
    pub outcome: McpPlatformOutcome<McpManagedDetail>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpHealthRun_unstable", response = McpHealthRunResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHealthRunRequest {
    pub managed_mcp_id: String,
    pub mode: McpHealthCheckMode,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHealthRunResponse {
    pub outcome: McpPlatformOutcome<McpTaskRef>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpHealthGet_unstable", response = McpHealthGetResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHealthGetRequest {
    pub managed_mcp_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHealthGetResponse {
    pub outcome: McpPlatformOutcome<McpHealthStatus>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpSetDefaultEnabled_unstable", response = McpSetDefaultEnabledResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSetDefaultEnabledRequest {
    pub managed_mcp_id: String,
    pub enabled: bool,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSetDefaultEnabledResponse {
    pub outcome: McpPlatformOutcome<McpManagedSummary>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManagedPage {
    pub items: Vec<McpManagedSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManagedSummary {
    pub managed_mcp_id: String,
    pub mcp_id: String,
    pub installation_scope: McpInstallationScope,
    pub registration: McpRegistrationState,
    pub installation: McpInstallationState,
    pub runtime: McpRuntimeState,
    pub health: McpHealthState,
    pub default_enabled: bool,
    pub revision: i64,
    pub updated_at_ms: i64,
    pub distribution_adapter: String,
    pub active_version: Option<String>,
    pub available_version: Option<String>,
    pub available_manifest_digest: Option<String>,
    pub current_task: Option<McpTaskRef>,
    pub recovery_required: bool,
    pub eligibility: McpManagedEligibility,
    pub next_action: McpManagedNextAction,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManagedEligibility {
    pub update: bool,
    pub repair: bool,
    pub uninstall: bool,
    pub reason: McpManagedEligibilityReason,
    pub update_reason: McpManagedEligibilityReason,
    pub repair_reason: McpManagedEligibilityReason,
    pub uninstall_reason: McpManagedEligibilityReason,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpManagedEligibilityReason {
    #[default]
    Eligible,
    RegistrationOnly,
    TaskRecoveryRequired,
    TaskInProgress,
    TaskInterrupted,
    NotInstalled,
    NoUpdateAvailable,
    RuntimeUnavailable,
    PolicyDenied,
    Incompatible,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpManagedNextAction {
    #[default]
    None,
    EnableAfterHealth,
    Repair,
    ResolveRecovery,
    WaitForTask,
    ResumeTask,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManagedDetail {
    pub summary: McpManagedSummary,
    pub distribution_adapter: String,
    pub active_manifest_digest: String,
    pub active_version: String,
    pub extension_config_key: String,
    pub projection_digest: String,
    pub latest_health: Option<McpHealthObservation>,
    pub registration_task: Option<McpTaskRef>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHealthStatus {
    pub managed_mcp_id: String,
    pub state: McpHealthState,
    pub latest: Option<McpHealthObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHealthObservation {
    pub task_id: String,
    pub mode: McpHealthCheckMode,
    pub result: McpHealthResult,
    pub latency_ms: i64,
    pub capabilities_digest: Option<String>,
    pub tools_digest: Option<String>,
    pub checked_at_ms: i64,
    pub detail_code: McpHealthDetailCode,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpHealthCheckMode {
    #[default]
    Registration,
    Runtime,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpRegistrationState {
    #[default]
    Absent,
    Registered,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpInstallationState {
    #[default]
    NotApplicable,
    NotInstalled,
    Staged,
    Installed,
    UpdateAvailable,
    RepairRequired,
    UninstallPending,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpRuntimeState {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpHealthState {
    #[default]
    Unknown,
    Checking,
    Healthy,
    Degraded,
    Unhealthy,
    BlockedAuth,
    Incompatible,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpHealthResult {
    Healthy,
    Unhealthy,
    BlockedAuth,
    Incompatible,
    Timeout,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpHealthDetailCode {
    ProjectionConsistent,
    ProjectionDrift,
    CredentialHandleMissing,
    ExpectedStatus,
    UnexpectedStatus,
    McpInitializeSucceeded,
    McpInitializeFailed,
    McpListToolsSucceeded,
    McpListToolsFailed,
    IncompatibleHealthContract,
    CleanupFailed,
    Timeout,
    Cancelled,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTrustTier {
    Official,
    Community,
    #[default]
    Local,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpCompatibility {
    #[default]
    Compatible,
    Incompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpCatalogLocator {
    ManifestDigest {
        manifest_digest: String,
    },
    CatalogRef {
        source_id: String,
        mcp_id: String,
        version: String,
    },
}

impl Default for McpCatalogLocator {
    fn default() -> Self {
        Self::ManifestDigest {
            manifest_digest: String::new(),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCatalogPage {
    pub items: Vec<McpCatalogSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub cache: McpCatalogCacheMetadata,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCatalogCacheMetadata {
    pub offline: bool,
    pub local_persistence_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_verified_at_ms: Option<i64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCatalogSummary {
    pub source_id: String,
    pub mcp_id: String,
    pub version: String,
    pub manifest_digest: String,
    pub name: String,
    pub description: String,
    pub publisher_id: String,
    pub publisher_name: String,
    pub trust_tier: McpTrustTier,
    pub proof: McpSourceProof,
    pub compatibility: McpCompatibility,
    pub distribution: McpDistributionKind,
    pub verified_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCatalogDetail {
    pub source_id: String,
    pub mcp_id: String,
    pub version: String,
    pub manifest_digest: String,
    pub name: String,
    pub description: String,
    pub publisher: McpPublisher,
    pub proof: McpSourceProof,
    pub trust_tier: McpTrustTier,
    pub distribution: McpDistributionContract,
    pub transport: McpTransportContract,
    pub auth: McpAuthContract,
    pub health: McpHealthContract,
    pub capabilities: Vec<McpCapability>,
    pub permissions: Vec<McpPermission>,
    pub compatibility: McpCompatibility,
    pub verified_at_ms: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPublisher {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    pub signing_identities: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpSourceProof {
    #[default]
    LocalBytes,
    Catalog {
        index_digest: String,
        declared_manifest_digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<McpSignatureProof>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSignatureProof {
    pub algorithm: String,
    pub signing_identity: String,
    pub present: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpDistributionKind {
    #[default]
    RemoteHttp,
    ManualStdio,
    Npm,
    PythonWheel,
    BinaryArchive,
    Docker,
    GitDev,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpDistributionContract {
    RemoteHttp,
    ManualStdio {
        platforms: Vec<String>,
    },
    Npm {
        package: String,
        package_version: String,
        artifacts: Vec<McpArtifactContract>,
    },
    PythonWheel {
        package: String,
        package_version: String,
        python: String,
        artifacts: Vec<McpArtifactContract>,
    },
    BinaryArchive {
        archive_format: String,
        artifacts: Vec<McpArtifactContract>,
    },
    Docker {
        image: String,
        digest: String,
    },
    GitDev {
        repository: String,
        commit: String,
        adapter: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpArtifactContract {
    pub platform: String,
    pub architecture: String,
    pub origin: String,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransportContract {
    Stdio {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        startup_timeout_seconds: Option<u64>,
    },
    StreamableHttp {
        endpoint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connect_timeout_seconds: Option<u64>,
        allowed_redirect_origins: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpAuthContract {
    None,
    ApiKeyHeader {
        header_name: String,
        credential_name: String,
        prefix_required: bool,
    },
    Environment {
        environment_key: String,
        credential_name: String,
    },
    Oauth2 {
        authorization_endpoint: String,
        token_endpoint: String,
        client_registration: String,
        scopes: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpHealthContract {
    McpInitialize {
        timeout_seconds: u64,
    },
    McpListTools {
        timeout_seconds: u64,
    },
    Http {
        relative_endpoint: String,
        expected_status: u16,
        timeout_seconds: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpCapability {
    Tools,
    Resources,
    Prompts,
    Sampling,
    Elicitation,
    Logging,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPermission {
    pub id: String,
    pub kind: McpPermissionKind,
    pub reason: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpPermissionKind {
    FilesystemRead,
    FilesystemWrite,
    Network,
    Credentials,
    ProcessSpawn,
    Docker,
    HostApplication,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpPlanIntent {
    Register {
        manifest_digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        installation_scope: Option<McpInstallationScope>,
    },
    Install {
        manifest_digest: String,
    },
    Update {
        managed_mcp_id: String,
        target_version: String,
    },
    Repair {
        managed_mcp_id: String,
    },
    Uninstall {
        managed_mcp_id: String,
        preserve_user_data: bool,
    },
}

impl Default for McpPlanIntent {
    fn default() -> Self {
        Self::Register {
            manifest_digest: String::new(),
            installation_scope: None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpInstallationScope {
    #[default]
    User,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpUserDecision {
    Confirm,
    #[default]
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPlanReview {
    pub plan_id: String,
    pub plan_digest: String,
    pub expires_at_ms: i64,
    pub source_id: String,
    pub proof: McpSourceProof,
    pub trust_tier: McpTrustTier,
    pub publisher: McpPublisher,
    pub mcp_id: String,
    pub name: String,
    pub version: String,
    pub permissions: Vec<McpPermission>,
    pub network_origins: Vec<String>,
    pub file_effects: McpFileEffects,
    pub host_effects: McpHostEffects,
    pub process_effects: McpProcessEffects,
    pub reversibility: McpReversibility,
    pub policy: McpPolicyReview,
    pub warnings: Vec<McpPlanWarning>,
    pub required_confirmations: Vec<McpRequiredConfirmation>,
    pub default_disabled: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpFileEffects {
    pub writes_files: bool,
    pub removes_files: bool,
    pub owned_items: u32,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHostEffects {
    pub registration_ids: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProcessEffects {
    pub process_required_for_connection: bool,
    pub starts_during_confirmation: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpReversibility {
    pub reversible: bool,
    pub rollback_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPolicyReview {
    pub outcome: McpPolicyOutcome,
    pub reasons: Vec<McpPolicyReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpPolicyOutcome {
    Allow,
    Deny,
    NeedsConfirmation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPolicyReason {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpPlanWarning {
    DefaultDisabled,
    RegistrationOnly,
    ManagedArtifactDownload,
    ExistingVersionRetainedUntilCommit,
    RemovesOwnedFilesOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpRequiredConfirmation {
    Policy { reason_code: String },
    Permission { permission_id: String },
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTaskRef {
    pub task_id: String,
    pub operation: McpTaskOperation,
    pub status: McpTaskStatus,
    pub progress: u8,
    pub cancellable: bool,
    pub revision: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTaskOperation {
    #[default]
    Register,
    Install,
    Update,
    Repair,
    Uninstall,
    Health,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTaskStatus {
    #[default]
    Planned,
    AwaitingConfirmation,
    Queued,
    Running,
    Cancelling,
    Verifying,
    Activating,
    RollingBack,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    RecoveryRequired,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpEventsPage {
    pub events: Vec<McpEventEnvelope>,
    pub next_event_id: i64,
    pub tasks: Vec<McpTaskRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpEventEnvelope {
    pub event_id: i64,
    pub task_id: String,
    pub task_local_sequence: i64,
    pub occurred_at_ms: i64,
    pub actor: String,
    pub event_type: McpEventType,
    pub payload: McpEventPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpEventType {
    TaskCreated,
    TaskStatusChanged,
    StepStarted,
    StepCommitted,
    RecoveryInterrupted,
    ConfirmationRecorded,
    CancellationRequested,
    RetryQueued,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpEventPayload {
    TaskCreated {
        status: McpTaskStatus,
    },
    TaskStatusChanged {
        from: McpTaskStatus,
        to: McpTaskStatus,
    },
    ConfirmationRecorded {
        from: McpTaskStatus,
        to: McpTaskStatus,
        plan_id: String,
        plan_digest: String,
    },
    CancellationRequested {
        from: McpTaskStatus,
        to: McpTaskStatus,
    },
    StepStatusChanged {
        ordinal: i64,
        from: McpTaskStepStatus,
        to: McpTaskStepStatus,
    },
    RecoveryDecision {
        decision: McpRecoveryDecision,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTaskStepStatus {
    NotStarted,
    Started,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpRecoveryDecision {
    ResumeFromStep { ordinal: i64 },
    RollbackFromStep { ordinal: i64 },
    RequiresManualRecovery,
}

impl<T> Default for McpPlatformOutcome<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::Success {
            value: T::default(),
        }
    }
}

#[cfg(test)]
#[path = "mcp_platform_tests.rs"]
mod tests;
