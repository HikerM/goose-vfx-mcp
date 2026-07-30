use std::fmt;

pub const MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE: &str = "Managed MCP storage on Windows is unavailable. Choose or repair a regular writable folder on D and restart Goose before retrying.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum McpPlatformErrorCode {
    InvalidRequest,
    InvalidJson,
    InvalidManifest,
    UnsupportedSchema,
    VersionNotExact,
    UnsafeUrl,
    InvalidDigest,
    ImmutableReferenceRequired,
    DuplicateSelector,
    TransportMismatch,
    PathTraversal,
    UnknownTemplateVariable,
    PolicyDenied,
    UnknownAdapter,
    NotImplementedForPhase,
    OperationNotSupported,
    ManualStdioProviderUnavailable,
    RemoteHttpPolicyUnavailable,
    CandidateStateConflict,
    ManifestIdentityConflict,
    ManifestConflict,
    SchemaTooNew,
    IntegrityError,
    IntegrityUnavailable,
    PlanConflict,
    PlanStale,
    PlanExpired,
    IdempotencyConflict,
    RevisionConflict,
    InvalidTransition,
    RepositoryUnavailable,
    NotFound,
    SerializationFailed,
    ProjectionConflict,
    ProjectionWitnessExpired,
    ProjectionWitnessConsumed,
    CredentialMissing,
    HealthFailed,
    TaskNotCancellable,
    RollbackIncomplete,
    AdapterIncompatible,
    DockerUnavailable,
    DaemonPolicyDenied,
    ImageDigestMismatch,
    RegistryAuthRequired,
    MountPermissionDenied,
    GitUnavailable,
    GitOriginDenied,
    CommitUnavailable,
    UnsafeRepositoryTree,
    DevelopmentModeRequired,
    RuntimeControlUnavailable,
}

impl McpPlatformErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidJson => "invalid_json",
            Self::InvalidManifest => "invalid_manifest",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::VersionNotExact => "version_not_exact",
            Self::UnsafeUrl => "unsafe_url",
            Self::InvalidDigest => "invalid_digest",
            Self::ImmutableReferenceRequired => "immutable_reference_required",
            Self::DuplicateSelector => "duplicate_selector",
            Self::TransportMismatch => "transport_mismatch",
            Self::PathTraversal => "path_traversal",
            Self::UnknownTemplateVariable => "unknown_template_variable",
            Self::PolicyDenied => "policy_denied",
            Self::UnknownAdapter => "unknown_adapter",
            Self::NotImplementedForPhase => "not_implemented_for_phase",
            Self::OperationNotSupported => "operation_not_supported",
            Self::ManualStdioProviderUnavailable => "manual_stdio_provider_unavailable",
            Self::RemoteHttpPolicyUnavailable => "remote_http_policy_unavailable",
            Self::CandidateStateConflict => "candidate_state_conflict",
            Self::ManifestIdentityConflict => "manifest_identity_conflict",
            Self::ManifestConflict => "manifest_conflict",
            Self::SchemaTooNew => "schema_too_new",
            Self::IntegrityError => "integrity_error",
            Self::IntegrityUnavailable => "integrity_unavailable",
            Self::PlanConflict => "plan_conflict",
            Self::PlanStale => "plan_stale",
            Self::PlanExpired => "plan_expired",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::RevisionConflict => "revision_conflict",
            Self::InvalidTransition => "invalid_transition",
            Self::RepositoryUnavailable => "repository_unavailable",
            Self::NotFound => "not_found",
            Self::SerializationFailed => "serialization_failed",
            Self::ProjectionConflict => "projection_conflict",
            Self::ProjectionWitnessExpired => "projection_witness_expired",
            Self::ProjectionWitnessConsumed => "projection_witness_consumed",
            Self::CredentialMissing => "credential_missing",
            Self::HealthFailed => "health_failed",
            Self::TaskNotCancellable => "task_not_cancellable",
            Self::RollbackIncomplete => "rollback_incomplete",
            Self::AdapterIncompatible => "adapter_incompatible",
            Self::DockerUnavailable => "docker_unavailable",
            Self::DaemonPolicyDenied => "daemon_policy_denied",
            Self::ImageDigestMismatch => "image_digest_mismatch",
            Self::RegistryAuthRequired => "registry_auth_required",
            Self::MountPermissionDenied => "mount_permission_denied",
            Self::GitUnavailable => "git_unavailable",
            Self::GitOriginDenied => "git_origin_denied",
            Self::CommitUnavailable => "commit_unavailable",
            Self::UnsafeRepositoryTree => "unsafe_repository_tree",
            Self::DevelopmentModeRequired => "development_mode_required",
            Self::RuntimeControlUnavailable => "runtime_control_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPlatformError {
    code: McpPlatformErrorCode,
    message: &'static str,
}

impl McpPlatformError {
    pub const fn new(code: McpPlatformErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }

    pub const fn code(&self) -> McpPlatformErrorCode {
        self.code
    }

    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for McpPlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for McpPlatformError {}

pub type McpPlatformResult<T> = Result<T, McpPlatformError>;
