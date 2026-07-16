use std::fmt;

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
    ManifestConflict,
    SchemaTooNew,
    IntegrityError,
    PlanConflict,
    PlanStale,
    PlanExpired,
    IdempotencyConflict,
    RevisionConflict,
    InvalidTransition,
    RepositoryUnavailable,
    NotFound,
    SerializationFailed,
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
            Self::ManifestConflict => "manifest_conflict",
            Self::SchemaTooNew => "schema_too_new",
            Self::IntegrityError => "integrity_error",
            Self::PlanConflict => "plan_conflict",
            Self::PlanStale => "plan_stale",
            Self::PlanExpired => "plan_expired",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::RevisionConflict => "revision_conflict",
            Self::InvalidTransition => "invalid_transition",
            Self::RepositoryUnavailable => "repository_unavailable",
            Self::NotFound => "not_found",
            Self::SerializationFailed => "serialization_failed",
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
