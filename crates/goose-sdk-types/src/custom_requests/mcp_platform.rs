use agent_client_protocol::{JsonRpcRequest, JsonRpcResponse};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

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
pub const MCP_PROJECTION_RECOVERY_RESOLVE_METHOD: &str =
    "goose.mcpProjectionRecoveryResolve_unstable";
pub const MCP_HEALTH_RUN_METHOD: &str = "goose.mcpHealthRun_unstable";
pub const MCP_HEALTH_GET_METHOD: &str = "goose.mcpHealthGet_unstable";
pub const MCP_SET_DEFAULT_ENABLED_METHOD: &str = "goose.mcpSetDefaultEnabled_unstable";
pub const MCP_RUNTIME_CONTROL_METHOD: &str = "goose.mcpRuntimeControl_unstable";
pub const MCP_SOURCES_POLICY_GET_METHOD: &str = "goose.mcpSourcesPolicyGet_unstable";
pub const MCP_SOURCE_REFRESH_METHOD: &str = "goose.mcpSourceRefresh_unstable";
pub const MCP_SOURCE_PROVISION_PREPARE_METHOD: &str = "goose.mcpSourceProvisionPrepare_unstable";
pub const MCP_SOURCE_PROVISION_CONFIRM_METHOD: &str = "goose.mcpSourceProvisionConfirm_unstable";
pub const MCP_GOVERNED_IMPORT_METHOD: &str = "goose.mcpGovernedImport_unstable";
pub const MCP_MANUAL_STDIO_SOURCES_LIST_METHOD: &str = "goose.mcpManualStdioSourcesList_unstable";
pub const MCP_MANUAL_PLAN_CREATE_METHOD: &str = "goose.mcpManualPlanCreate_unstable";
pub const MCP_PROFILE_LIST_METHOD: &str = "goose.mcpProfileList_unstable";
pub const MCP_PROFILE_GET_METHOD: &str = "goose.mcpProfileGet_unstable";
pub const MCP_PROFILE_CREATE_METHOD: &str = "goose.mcpProfileCreate_unstable";
pub const MCP_PROFILE_UPDATE_METHOD: &str = "goose.mcpProfileUpdate_unstable";
pub const MCP_PROFILE_RESTORE_METHOD: &str = "goose.mcpProfileRestore_unstable";
pub const MCP_PROFILE_ARCHIVE_METHOD: &str = "goose.mcpProfileArchive_unstable";
pub const MCP_PROFILE_DRAFT_CREATE_METHOD: &str = "goose.mcpProfileDraftCreate_unstable";
pub const MCP_PROFILE_MODEL_RECOMMEND_METHOD: &str = "goose.mcpProfileModelRecommend_unstable";
pub const MCP_PROFILE_APPLY_PLAN_CREATE_METHOD: &str = "goose.mcpProfileApplyPlanCreate_unstable";
pub const MCP_PROFILE_APPLY_CONFIRM_METHOD: &str = "goose.mcpProfileApplyConfirm_unstable";
pub const MCP_PROFILE_CONNECTION_TEST_METHOD: &str = "goose.mcpProfileConnectionTest_unstable";
pub const MCP_SOURCE_ADAPTERS_LIST_METHOD: &str = "goose.mcpSourceAdaptersList_unstable";
pub const MCP_HTTPS_MANIFEST_PREPARE_METHOD: &str = "goose.mcpHttpsManifestPrepare_unstable";
pub const MCP_HTTPS_MANIFEST_CONFIRM_METHOD: &str = "goose.mcpHttpsManifestConfirm_unstable";
pub const MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD: &str =
    "goose.mcpHttpsProvisionPlanCreate_unstable";

#[derive(Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "goose.mcpHttpsManifestPrepare_unstable",
    response = McpHttpsManifestPrepareResponse
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsManifestPrepareRequest {
    #[serde(deserialize_with = "deserialize_bounded_https_manifest_url")]
    pub url: String,
}

impl std::fmt::Debug for McpHttpsManifestPrepareRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsManifestPrepareRequest")
            .field("url", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsManifestPrepareResponse {
    pub outcome: McpPlatformOutcome<McpHttpsManifestPrepareResult>,
}

impl std::fmt::Debug for McpHttpsManifestPrepareResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsManifestPrepareResponse")
            .field("outcome", &"[REDACTED]")
            .finish()
    }
}

#[derive(Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "goose.mcpHttpsManifestConfirm_unstable",
    response = McpHttpsManifestConfirmResponse
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsManifestConfirmRequest {
    #[serde(deserialize_with = "deserialize_bounded_provision_id")]
    pub provision_id: String,
    #[serde(deserialize_with = "deserialize_bounded_confirmation_token")]
    pub confirmation_token: String,
    pub confirm: bool,
}

impl std::fmt::Debug for McpHttpsManifestConfirmRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsManifestConfirmRequest")
            .field("provision_id", &self.provision_id)
            .field("confirmation_token", &"[REDACTED]")
            .field("confirm", &self.confirm)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsManifestConfirmResponse {
    pub outcome: McpPlatformOutcome<McpHttpsManifestConfirmResult>,
}

impl std::fmt::Debug for McpHttpsManifestConfirmResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsManifestConfirmResponse")
            .field("outcome", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsManifestPrepareResult {
    pub provision_id: String,
    pub confirmation_token: String,
    pub expires_at_ms: i64,
    pub preview: McpHttpsManifestSecurityPreview,
}

impl std::fmt::Debug for McpHttpsManifestPrepareResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsManifestPrepareResult")
            .field("provision_id", &self.provision_id)
            .field("confirmation_token", &"[REDACTED]")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("preview", &self.preview)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsManifestConfirmResult {
    pub provision_id: String,
    pub manifest_digest: String,
    pub confirmed_at_ms: i64,
    pub preview: McpHttpsManifestSecurityPreview,
}

impl std::fmt::Debug for McpHttpsManifestConfirmResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsManifestConfirmResult")
            .field("provision_id", &"[REDACTED]")
            .field("manifest_digest", &"[REDACTED]")
            .field("confirmed_at_ms", &self.confirmed_at_ms)
            .field("preview", &self.preview)
            .finish()
    }
}

#[derive(Default, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsManifestSecurityPreview {
    #[serde(deserialize_with = "deserialize_safe_preview_text")]
    pub manifest_id: String,
    #[serde(deserialize_with = "deserialize_safe_preview_text")]
    pub version: String,
    #[serde(deserialize_with = "deserialize_safe_preview_origin")]
    pub redacted_origin: String,
    #[serde(deserialize_with = "deserialize_safe_preview_text")]
    pub raw_digest: String,
    #[serde(deserialize_with = "deserialize_safe_preview_text")]
    pub parsed_digest: String,
    #[serde(deserialize_with = "deserialize_safe_preview_text")]
    pub redirect_chain_digest: String,
    #[serde(deserialize_with = "deserialize_safe_preview_text")]
    pub dns_evidence_digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<McpHttpsManifestWarning>,
}

impl std::fmt::Debug for McpHttpsManifestSecurityPreview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsManifestSecurityPreview")
            .field("manifest_id", &"[REDACTED]")
            .field("version", &"[REDACTED]")
            .field("redacted_origin", &"[REDACTED]")
            .field("raw_digest", &"[REDACTED]")
            .field("parsed_digest", &"[REDACTED]")
            .field("redirect_chain_digest", &"[REDACTED]")
            .field("dns_evidence_digest", &"[REDACTED]")
            .field("warnings", &self.warnings.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpHttpsManifestWarning {
    Redirected,
    DnsChanged,
    DigestMismatch,
    PolicyDenied,
}

impl Serialize for McpHttpsManifestSecurityPreview {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if !is_strict_https_origin(&self.redacted_origin)
            || [
                &self.manifest_id,
                &self.version,
                &self.raw_digest,
                &self.parsed_digest,
                &self.redirect_chain_digest,
                &self.dns_evidence_digest,
            ]
            .iter()
            .any(|value| {
                let value = value.as_str();
                value.trim() != value
                    || value.chars().any(char::is_control)
                    || value.contains(['?', '#', '@'])
            })
        {
            return Err(serde::ser::Error::custom(
                "security preview contains unsafe content",
            ));
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            manifest_id: &'a str,
            version: &'a str,
            redacted_origin: &'a str,
            raw_digest: &'a str,
            parsed_digest: &'a str,
            redirect_chain_digest: &'a str,
            dns_evidence_digest: &'a str,
            #[serde(skip_serializing_if = "<[McpHttpsManifestWarning]>::is_empty")]
            warnings: &'a [McpHttpsManifestWarning],
        }
        Wire {
            manifest_id: &self.manifest_id,
            version: &self.version,
            redacted_origin: &self.redacted_origin,
            raw_digest: &self.raw_digest,
            parsed_digest: &self.parsed_digest,
            redirect_chain_digest: &self.redirect_chain_digest,
            dns_evidence_digest: &self.dns_evidence_digest,
            warnings: &self.warnings,
        }
        .serialize(serializer)
    }
}

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

/// Predeclared discovery-only SDK contract. No server handler currently exists, and consumers
/// must not call this method until a separately registered server capability is available.
#[derive(Debug, Default, Clone, Serialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "goose.mcpSourceAdaptersList_unstable",
    response = McpSourceAdaptersListResponse
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceAdaptersListRequest {}

impl<'de> Deserialize<'de> for McpSourceAdaptersListRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct EmptyRequestVisitor;

        impl<'de> serde::de::Visitor<'de> for EmptyRequestVisitor {
            type Value = McpSourceAdaptersListRequest;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an empty source adapters list request")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                if map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {
                    return Err(A::Error::custom(
                        "source adapters list request has unknown fields",
                    ));
                }
                Ok(McpSourceAdaptersListRequest {})
            }
        }

        deserializer.deserialize_map(EmptyRequestVisitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceAdaptersListResponse {
    pub outcome: McpSourceAdaptersListOutcome,
}

/// Closed outcome for source discovery. It deliberately does not reuse the generic platform
/// error envelope, which can carry diagnostic text unsuitable for this discovery boundary.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpSourceAdaptersListOutcome {
    Success { value: McpSourceAdaptersListResult },
    Error { error: McpSourceAdaptersListError },
}

impl McpSourceAdaptersListOutcome {
    pub fn success(value: McpSourceAdaptersListResult) -> Self {
        Self::Success { value }
    }

    pub fn error(error: McpSourceAdaptersListError) -> Self {
        Self::Error { error }
    }
}

/// Discovery failures are intentionally non-diagnostic so a response cannot reflect secrets,
/// URLs, paths, raw configuration, or server correlation data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceAdaptersListError {
    Unavailable,
    PolicyDenied,
}

/// The unstable wire form uses adapter IDs as object keys so JSON Schema and serde share the
/// same identity rule. Object-key duplication is rejected while streaming rather than relying on
/// `serde_json::Value`, which would otherwise silently retain only the final key.
#[derive(Debug, Default, Clone, JsonSchema)]
#[schemars(schema_with = "source_adapters_list_result_schema")]
pub struct McpSourceAdaptersListResult {
    pub adapters: BTreeMap<String, McpSourceAdapterDescriptor>,
}

impl Serialize for McpSourceAdaptersListResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_source_adapter_map(&self.adapters).map_err(|_| {
            S::Error::custom("source adapter list result contains an invalid adapter")
        })?;
        McpSourceAdaptersListResultWireRef {
            adapters: &self.adapters,
        }
        .serialize(serializer)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpSourceAdaptersListResultWireRef<'a> {
    adapters: &'a BTreeMap<String, McpSourceAdapterDescriptor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpSourceAdaptersListResultWire {
    #[serde(deserialize_with = "deserialize_source_adapter_map")]
    adapters: BTreeMap<String, McpSourceAdapterDescriptor>,
}

impl<'de> Deserialize<'de> for McpSourceAdaptersListResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = McpSourceAdaptersListResultWire::deserialize(deserializer)?;
        Ok(Self {
            adapters: wire.adapters,
        })
    }
}

/// A validated, descriptive source adapter contract.
///
/// This unstable type is `non_exhaustive` so external callers must use [`Self::new`] rather than
/// a struct literal. Its fields remain public for read compatibility; serialization revalidates
/// the descriptor to reject a value made invalid through later field mutation.
#[derive(Debug, Clone, JsonSchema)]
#[schemars(schema_with = "source_adapter_descriptor_schema")]
#[non_exhaustive]
pub struct McpSourceAdapterDescriptor {
    pub display_name: String,
    pub source_kind: McpSourceAdapterSourceKind,
    pub trust_mode: McpSourceAdapterTrustMode,
    pub availability: McpSourceAdapterAvailability,
    pub required_approvals: Vec<McpSourceAdapterApproval>,
    pub risks: Vec<McpSourceAdapterRisk>,
    pub required_host_dependencies: Vec<String>,
    pub allowed_transports: Vec<McpSourceAdapterTransport>,
    #[schemars(schema_with = "source_adapter_input_field_map_schema")]
    pub input_fields: BTreeMap<String, McpSourceAdapterInputField>,
}

impl McpSourceAdapterDescriptor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        display_name: String,
        source_kind: McpSourceAdapterSourceKind,
        trust_mode: McpSourceAdapterTrustMode,
        availability: McpSourceAdapterAvailability,
        required_approvals: Vec<McpSourceAdapterApproval>,
        risks: Vec<McpSourceAdapterRisk>,
        required_host_dependencies: Vec<String>,
        allowed_transports: Vec<McpSourceAdapterTransport>,
        input_fields: BTreeMap<String, McpSourceAdapterInputField>,
    ) -> Result<Self, McpSourceAdapterDescriptorValidationError> {
        let mut descriptor = Self {
            display_name,
            source_kind,
            trust_mode,
            availability,
            required_approvals,
            risks,
            required_host_dependencies,
            allowed_transports,
            input_fields,
        };
        descriptor.validate()?;
        descriptor.risks.sort_unstable();
        descriptor.required_approvals.sort_unstable();
        descriptor.required_host_dependencies.sort_unstable();
        descriptor.allowed_transports.sort_unstable();
        Ok(descriptor)
    }

    fn validate(&self) -> Result<(), McpSourceAdapterDescriptorValidationError> {
        validate_source_adapter_descriptor(
            &self.display_name,
            &self.source_kind,
            self.trust_mode,
            &self.availability,
            &self.required_approvals,
            &self.risks,
            &self.required_host_dependencies,
            &self.allowed_transports,
            &self.input_fields,
        )
        .map_err(|_| McpSourceAdapterDescriptorValidationError(()))
    }
}

impl Serialize for McpSourceAdapterDescriptor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate()
            .map_err(|_| S::Error::custom("source adapter descriptor is invalid"))?;
        McpSourceAdapterDescriptorWireRef {
            display_name: &self.display_name,
            source_kind: &self.source_kind,
            trust_mode: self.trust_mode,
            availability: &self.availability,
            required_approvals: &self.required_approvals,
            risks: &self.risks,
            required_host_dependencies: &self.required_host_dependencies,
            allowed_transports: &self.allowed_transports,
            input_fields: &self.input_fields,
        }
        .serialize(serializer)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpSourceAdapterDescriptorWireRef<'a> {
    display_name: &'a str,
    source_kind: &'a McpSourceAdapterSourceKind,
    trust_mode: McpSourceAdapterTrustMode,
    availability: &'a McpSourceAdapterAvailability,
    required_approvals: &'a [McpSourceAdapterApproval],
    risks: &'a [McpSourceAdapterRisk],
    required_host_dependencies: &'a [String],
    allowed_transports: &'a [McpSourceAdapterTransport],
    input_fields: &'a BTreeMap<String, McpSourceAdapterInputField>,
}

/// Validation failure for a source adapter descriptor. Details are deliberately not exposed at
/// this discovery boundary, because callers can surface the error while serializing a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpSourceAdapterDescriptorValidationError(());

impl std::fmt::Display for McpSourceAdapterDescriptorValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("source adapter descriptor is invalid")
    }
}

impl std::error::Error for McpSourceAdapterDescriptorValidationError {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpSourceAdapterDescriptorWire {
    #[serde(deserialize_with = "deserialize_source_adapter_display_name")]
    display_name: String,
    source_kind: McpSourceAdapterSourceKind,
    trust_mode: McpSourceAdapterTrustMode,
    availability: McpSourceAdapterAvailability,
    #[serde(deserialize_with = "deserialize_source_adapter_required_approvals")]
    required_approvals: Vec<McpSourceAdapterApproval>,
    risks: Vec<McpSourceAdapterRisk>,
    #[serde(deserialize_with = "deserialize_source_adapter_host_dependencies")]
    required_host_dependencies: Vec<String>,
    allowed_transports: Vec<McpSourceAdapterTransport>,
    #[serde(deserialize_with = "deserialize_source_adapter_input_field_map")]
    input_fields: BTreeMap<String, McpSourceAdapterInputField>,
}

impl<'de> Deserialize<'de> for McpSourceAdapterDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = McpSourceAdapterDescriptorWire::deserialize(deserializer)?;
        Self::new(
            wire.display_name,
            wire.source_kind,
            wire.trust_mode,
            wire.availability,
            wire.required_approvals,
            wire.risks,
            wire.required_host_dependencies,
            wire.allowed_transports,
            wire.input_fields,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpSourceAdapterSourceKind {
    Catalog {},
    RemoteHttp {},
    ManualStdio {},
    Npm {},
    Uvx {},
    Docker {},
    Git {},
    Opaque {
        #[serde(rename = "kindId")]
        #[serde(deserialize_with = "deserialize_opaque_source_adapter_kind_id")]
        #[schemars(rename = "kindId")]
        #[schemars(schema_with = "source_adapter_opaque_kind_id_schema")]
        kind_id: String,
    },
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceAdapterTrustMode {
    Verified,
    UserManaged,
    Unverified,
}

/// A closed, non-diagnostic availability state for discovery. Only a ready adapter with no
/// required approvals may proceed to a later intake phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpSourceAdapterAvailability {
    ReadyForIntake {},
    Blocked {
        reason: McpSourceAdapterBlockedReason,
    },
}

/// Reasons are deliberately categorical so discovery cannot carry policy prose, host paths,
/// runtime diagnostics, or other potentially sensitive detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceAdapterBlockedReason {
    Policy,
    ApprovalRequired,
    RuntimeDependency,
    Unsupported,
    Unverified,
}

/// Approval categories are a fixed cross-platform vocabulary. They describe the capability to
/// approve, never whether an approval has already been granted.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceAdapterApproval {
    NetworkAccess,
    ProcessExecution,
    FilesystemRead,
    FilesystemWrite,
    HostDependency,
    CredentialReference,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceAdapterRisk {
    NetworkAccess,
    ProcessExecution,
    FilesystemRead,
    FilesystemWrite,
    HostDependency,
    CredentialReference,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceAdapterTransport {
    Catalog,
    RemoteHttp,
    StdioProvider,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(schema_with = "source_adapter_input_field_schema")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceAdapterInputField {
    #[schemars(schema_with = "source_adapter_display_text_schema")]
    pub label: String,
    pub kind: McpSourceAdapterInputFieldKind,
    pub required: bool,
    pub multiline: bool,
    pub secret_reference_only: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpSourceAdapterInputFieldWire {
    #[serde(deserialize_with = "deserialize_source_adapter_display_name")]
    label: String,
    kind: McpSourceAdapterInputFieldKind,
    required: bool,
    multiline: bool,
    secret_reference_only: bool,
}

impl<'de> Deserialize<'de> for McpSourceAdapterInputField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = McpSourceAdapterInputFieldWire::deserialize(deserializer)?;
        if wire.multiline && wire.kind != McpSourceAdapterInputFieldKind::Text {
            return Err(D::Error::custom("multiline is only valid for text fields"));
        }
        if wire.secret_reference_only
            && !matches!(
                wire.kind,
                McpSourceAdapterInputFieldKind::Reference
                    | McpSourceAdapterInputFieldKind::ProviderReference
            )
        {
            return Err(D::Error::custom(
                "secretReferenceOnly requires a reference field",
            ));
        }
        Ok(Self {
            label: wire.label,
            kind: wire.kind,
            required: wire.required,
            multiline: wire.multiline,
            secret_reference_only: wire.secret_reference_only,
        })
    }
}

fn deserialize_source_adapter_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, McpSourceAdapterDescriptor>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_identifier_map(deserializer, "adapter ID")
}

fn validate_source_adapter_map(
    adapters: &BTreeMap<String, McpSourceAdapterDescriptor>,
) -> Result<(), &'static str> {
    if adapters.len() > 32 || adapters.keys().any(|id| !is_valid_source_adapter_id(id)) {
        return Err("source adapter map is invalid");
    }
    Ok(())
}

fn deserialize_source_adapter_input_field_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, McpSourceAdapterInputField>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_identifier_map(deserializer, "input field ID")
}

fn deserialize_identifier_map<'de, D, T>(
    deserializer: D,
    identifier_name: &'static str,
) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct IdentifierMapVisitor<T> {
        identifier_name: &'static str,
        marker: std::marker::PhantomData<T>,
    }

    impl<'de, T> serde::de::Visitor<'de> for IdentifierMapVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = BTreeMap<String, T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "an object keyed by valid {}s",
                self.identifier_name
            )
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, T>()? {
                if !is_valid_source_adapter_id(&key) {
                    return Err(A::Error::custom(format!(
                        "{} is invalid",
                        self.identifier_name
                    )));
                }
                if values.insert(key, value).is_some() {
                    return Err(A::Error::custom(format!(
                        "{} is duplicated",
                        self.identifier_name
                    )));
                }
                if values.len() > 32 {
                    return Err(A::Error::custom(format!(
                        "too many {}s",
                        self.identifier_name
                    )));
                }
            }
            Ok(values)
        }
    }

    deserializer.deserialize_map(IdentifierMapVisitor {
        identifier_name,
        marker: std::marker::PhantomData,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceAdapterInputFieldKind {
    Text,
    Reference,
    Directory,
    ProviderReference,
}

fn deserialize_source_adapter_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if !is_valid_source_adapter_id(&value) {
        return Err(D::Error::custom("identifier is invalid"));
    }
    Ok(value)
}

fn deserialize_opaque_source_adapter_kind_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = deserialize_source_adapter_id(deserializer)?;
    if is_known_source_adapter_kind_id(&value) {
        return Err(D::Error::custom("opaque kind identifier is reserved"));
    }
    Ok(value)
}

fn deserialize_source_adapter_display_name<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if !is_valid_source_adapter_display_text(&value) {
        return Err(D::Error::custom("display text is invalid"));
    }
    Ok(value)
}

fn deserialize_source_adapter_host_dependencies<'de, D>(
    deserializer: D,
) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut dependencies = Vec::<String>::deserialize(deserializer)?;
    for dependency in &dependencies {
        if !is_valid_source_adapter_id(dependency) {
            return Err(D::Error::custom("host dependency identifier is invalid"));
        }
    }
    reject_duplicate_ids(
        dependencies.iter().map(String::as_str),
        "requiredHostDependencies",
    )
    .map_err(D::Error::custom)?;
    if dependencies.len() > 32 {
        return Err(D::Error::custom("too many host dependency identifiers"));
    }
    dependencies.sort_unstable();
    Ok(dependencies)
}

fn deserialize_source_adapter_required_approvals<'de, D>(
    deserializer: D,
) -> Result<Vec<McpSourceAdapterApproval>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut approvals = Vec::<McpSourceAdapterApproval>::deserialize(deserializer)?;
    if approvals.len() > 6 || !has_unique_values(&approvals) {
        return Err(D::Error::custom(
            "required approvals must be a unique bounded set",
        ));
    }
    approvals.sort_unstable();
    Ok(approvals)
}

/// Discovery display text is ASCII-only and already normalized (trimmed, single-spaced).
/// This is a conservative SDK transport boundary, not a substitute for consumer-side UI escaping.
fn is_valid_source_adapter_display_text(value: &str) -> bool {
    if !(1..=160).contains(&value.len())
        || !value.is_ascii()
        || value != value.trim()
        || value.contains("  ")
        || value.contains("..")
    {
        return false;
    }

    let mut characters = value.bytes();
    let Some(first) = characters.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    if !characters.all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, b' ' | b'.' | b',' | b'-' | b'_')
    }) {
        return false;
    }

    !is_secret_like_display_text(value)
}

fn is_secret_like_display_text(value: &str) -> bool {
    let bytes = value.as_bytes();
    matches_ascii_case_insensitive_prefix(bytes, b"sk-") && bytes.len() >= 11
        || matches_ascii_case_insensitive_prefix(bytes, b"ghp_") && bytes.len() >= 12
        || matches_ascii_case_insensitive_prefix(bytes, b"github_pat_") && bytes.len() >= 20
        || matches_ascii_case_insensitive_prefix(bytes, b"akia") && bytes.len() >= 12
        || matches_ascii_case_insensitive_prefix(bytes, b"bearer ") && bytes.len() >= 15
}

fn matches_ascii_case_insensitive_prefix(value: &[u8], prefix: &[u8]) -> bool {
    value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn is_valid_source_adapter_id(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && value.len() <= 128
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_' | '.')
        })
}

fn is_known_source_adapter_kind_id(value: &str) -> bool {
    matches!(
        value,
        "catalog" | "remote_http" | "manual_stdio" | "npm" | "uvx" | "docker" | "git"
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_source_adapter_descriptor(
    display_name: &str,
    source_kind: &McpSourceAdapterSourceKind,
    trust_mode: McpSourceAdapterTrustMode,
    availability: &McpSourceAdapterAvailability,
    required_approvals: &[McpSourceAdapterApproval],
    risks: &[McpSourceAdapterRisk],
    required_host_dependencies: &[String],
    allowed_transports: &[McpSourceAdapterTransport],
    input_fields: &BTreeMap<String, McpSourceAdapterInputField>,
) -> Result<(), &'static str> {
    if !is_valid_source_adapter_display_text(display_name) {
        return Err("display text is invalid");
    }

    if matches!(source_kind, McpSourceAdapterSourceKind::Opaque { kind_id } if !is_valid_source_adapter_id(kind_id) || is_known_source_adapter_kind_id(kind_id))
    {
        return Err("opaque kind identifier is invalid");
    }

    if required_approvals.len() > 6
        || !has_unique_values(required_approvals)
        || required_host_dependencies.len() > 32
        || !has_unique_values(required_host_dependencies)
        || required_host_dependencies
            .iter()
            .any(|dependency| !is_valid_source_adapter_id(dependency))
        || input_fields.len() > 32
        || input_fields.iter().any(|(id, field)| {
            !is_valid_source_adapter_id(id) || !is_valid_source_adapter_input_field(field)
        })
    {
        return Err("descriptor fields are invalid");
    }

    if risks.is_empty()
        || risks.len() > 6
        || !has_unique_values(risks)
        || allowed_transports.len() > 3
        || !has_unique_values(allowed_transports)
    {
        return Err("risk and transport lists must be unique");
    }

    if required_approvals
        .iter()
        .any(|approval| !risks.contains(&approval.risk()))
    {
        return Err("required approvals must correspond to declared risks");
    }

    match availability {
        McpSourceAdapterAvailability::ReadyForIntake {} if required_approvals.is_empty() => {}
        McpSourceAdapterAvailability::Blocked {
            reason: McpSourceAdapterBlockedReason::ApprovalRequired,
        } if !required_approvals.is_empty() => {}
        McpSourceAdapterAvailability::Blocked { reason }
            if !matches!(reason, McpSourceAdapterBlockedReason::ApprovalRequired) => {}
        _ => return Err("availability and required approvals are inconsistent"),
    }

    if matches!(trust_mode, McpSourceAdapterTrustMode::Unverified)
        && !matches!(availability, McpSourceAdapterAvailability::Blocked { .. })
    {
        return Err("unverified adapters must be blocked");
    }
    if matches!(
        availability,
        McpSourceAdapterAvailability::Blocked {
            reason: McpSourceAdapterBlockedReason::Unverified,
        }
    ) && !matches!(trust_mode, McpSourceAdapterTrustMode::Unverified)
    {
        return Err("unverified blocks require unverified trust");
    }

    let has_host_dependency_risk = risks.contains(&McpSourceAdapterRisk::HostDependency);
    if has_host_dependency_risk == required_host_dependencies.is_empty() {
        return Err("host dependencies must match the host dependency risk");
    }

    if input_fields
        .values()
        .any(|field| field.secret_reference_only)
        && !risks.contains(&McpSourceAdapterRisk::CredentialReference)
    {
        return Err("secret reference fields require the credential reference risk");
    }

    match source_kind {
        McpSourceAdapterSourceKind::Catalog {} => require_exact_discovery_matrix(
            trust_mode,
            risks,
            allowed_transports,
            McpSourceAdapterTrustMode::Verified,
            &[McpSourceAdapterRisk::NetworkAccess],
            &[McpSourceAdapterTransport::Catalog],
        ),
        McpSourceAdapterSourceKind::RemoteHttp {} => require_discovery_matrix(
            trust_mode,
            risks,
            allowed_transports,
            &[
                McpSourceAdapterTrustMode::Verified,
                McpSourceAdapterTrustMode::UserManaged,
            ],
            &[
                McpSourceAdapterRisk::NetworkAccess,
                McpSourceAdapterRisk::CredentialReference,
            ],
            &[McpSourceAdapterRisk::NetworkAccess],
            &[McpSourceAdapterTransport::RemoteHttp],
            &[McpSourceAdapterTransport::RemoteHttp],
        ),
        McpSourceAdapterSourceKind::ManualStdio {} => require_discovery_matrix(
            trust_mode,
            risks,
            allowed_transports,
            &[McpSourceAdapterTrustMode::UserManaged],
            &[
                McpSourceAdapterRisk::ProcessExecution,
                McpSourceAdapterRisk::FilesystemRead,
                McpSourceAdapterRisk::FilesystemWrite,
                McpSourceAdapterRisk::HostDependency,
                McpSourceAdapterRisk::CredentialReference,
            ],
            &[McpSourceAdapterRisk::ProcessExecution],
            &[McpSourceAdapterTransport::StdioProvider],
            &[McpSourceAdapterTransport::StdioProvider],
        ),
        McpSourceAdapterSourceKind::Npm {} | McpSourceAdapterSourceKind::Uvx {} => {
            require_discovery_matrix(
                trust_mode,
                risks,
                allowed_transports,
                &[
                    McpSourceAdapterTrustMode::Verified,
                    McpSourceAdapterTrustMode::UserManaged,
                ],
                &[
                    McpSourceAdapterRisk::NetworkAccess,
                    McpSourceAdapterRisk::ProcessExecution,
                    McpSourceAdapterRisk::HostDependency,
                    McpSourceAdapterRisk::CredentialReference,
                ],
                &[
                    McpSourceAdapterRisk::ProcessExecution,
                    McpSourceAdapterRisk::HostDependency,
                ],
                &[
                    McpSourceAdapterTransport::Catalog,
                    McpSourceAdapterTransport::StdioProvider,
                ],
                &[McpSourceAdapterTransport::StdioProvider],
            )
        }
        McpSourceAdapterSourceKind::Docker {} => require_discovery_matrix(
            trust_mode,
            risks,
            allowed_transports,
            &[McpSourceAdapterTrustMode::UserManaged],
            &[
                McpSourceAdapterRisk::ProcessExecution,
                McpSourceAdapterRisk::FilesystemRead,
                McpSourceAdapterRisk::FilesystemWrite,
                McpSourceAdapterRisk::HostDependency,
                McpSourceAdapterRisk::CredentialReference,
            ],
            &[
                McpSourceAdapterRisk::ProcessExecution,
                McpSourceAdapterRisk::HostDependency,
            ],
            &[McpSourceAdapterTransport::StdioProvider],
            &[McpSourceAdapterTransport::StdioProvider],
        ),
        McpSourceAdapterSourceKind::Git {} => require_discovery_matrix(
            trust_mode,
            risks,
            allowed_transports,
            &[McpSourceAdapterTrustMode::UserManaged],
            &[
                McpSourceAdapterRisk::NetworkAccess,
                McpSourceAdapterRisk::ProcessExecution,
                McpSourceAdapterRisk::FilesystemRead,
                McpSourceAdapterRisk::FilesystemWrite,
                McpSourceAdapterRisk::HostDependency,
                McpSourceAdapterRisk::CredentialReference,
            ],
            &[
                McpSourceAdapterRisk::NetworkAccess,
                McpSourceAdapterRisk::ProcessExecution,
            ],
            &[McpSourceAdapterTransport::StdioProvider],
            &[McpSourceAdapterTransport::StdioProvider],
        ),
        McpSourceAdapterSourceKind::Opaque { .. } => {
            if !matches!(trust_mode, McpSourceAdapterTrustMode::Unverified)
                || !matches!(
                    availability,
                    McpSourceAdapterAvailability::Blocked {
                        reason: McpSourceAdapterBlockedReason::Unverified,
                    }
                )
                || !required_host_dependencies.is_empty()
                || !allowed_transports.is_empty()
                || !input_fields.is_empty()
                || !required_approvals.is_empty()
            {
                return Err("opaque source kinds must remain quarantined and non-executable");
            }
            Ok(())
        }
    }
}

fn is_valid_source_adapter_input_field(field: &McpSourceAdapterInputField) -> bool {
    is_valid_source_adapter_display_text(&field.label)
        && (!field.multiline || field.kind == McpSourceAdapterInputFieldKind::Text)
        && (!field.secret_reference_only
            || matches!(
                field.kind,
                McpSourceAdapterInputFieldKind::Reference
                    | McpSourceAdapterInputFieldKind::ProviderReference
            ))
}

impl McpSourceAdapterApproval {
    fn risk(self) -> McpSourceAdapterRisk {
        match self {
            Self::NetworkAccess => McpSourceAdapterRisk::NetworkAccess,
            Self::ProcessExecution => McpSourceAdapterRisk::ProcessExecution,
            Self::FilesystemRead => McpSourceAdapterRisk::FilesystemRead,
            Self::FilesystemWrite => McpSourceAdapterRisk::FilesystemWrite,
            Self::HostDependency => McpSourceAdapterRisk::HostDependency,
            Self::CredentialReference => McpSourceAdapterRisk::CredentialReference,
        }
    }
}

fn require_exact_discovery_matrix(
    trust_mode: McpSourceAdapterTrustMode,
    risks: &[McpSourceAdapterRisk],
    allowed_transports: &[McpSourceAdapterTransport],
    required_trust_mode: McpSourceAdapterTrustMode,
    required_risks: &[McpSourceAdapterRisk],
    required_transports: &[McpSourceAdapterTransport],
) -> Result<(), &'static str> {
    if trust_mode == required_trust_mode
        && risks == required_risks
        && allowed_transports == required_transports
    {
        Ok(())
    } else {
        Err("source kind has an invalid discovery matrix")
    }
}

#[allow(clippy::too_many_arguments)]
fn require_discovery_matrix(
    trust_mode: McpSourceAdapterTrustMode,
    risks: &[McpSourceAdapterRisk],
    allowed_transports: &[McpSourceAdapterTransport],
    allowed_trust_modes: &[McpSourceAdapterTrustMode],
    allowed_risks: &[McpSourceAdapterRisk],
    required_risks: &[McpSourceAdapterRisk],
    allowed_transports_for_kind: &[McpSourceAdapterTransport],
    required_transports: &[McpSourceAdapterTransport],
) -> Result<(), &'static str> {
    if allowed_trust_modes.contains(&trust_mode)
        && risks.iter().all(|risk| allowed_risks.contains(risk))
        && required_risks.iter().all(|risk| risks.contains(risk))
        && allowed_transports
            .iter()
            .all(|transport| allowed_transports_for_kind.contains(transport))
        && required_transports
            .iter()
            .all(|transport| allowed_transports.contains(transport))
    {
        Ok(())
    } else {
        Err("source kind has an invalid discovery matrix")
    }
}

fn has_unique_values<T: Eq + std::hash::Hash>(values: &[T]) -> bool {
    let mut seen = HashSet::new();
    values.iter().all(|value| seen.insert(value))
}

fn source_adapter_id_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "string",
        "minLength": 1,
        "maxLength": 128,
        "pattern": "^[a-z][a-z0-9_.-]{0,127}$",
        "allOf": [{"not": {"pattern": "[\\r\\n]"}}]
    })
}

fn source_adapter_opaque_kind_id_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "string",
        "minLength": 1,
        "maxLength": 128,
        "pattern": "^[a-z][a-z0-9_.-]{0,127}$",
        "allOf": [{"not": {"pattern": "[\\r\\n]"}}],
        "not": {"enum": ["catalog", "remote_http", "manual_stdio", "npm", "uvx", "docker", "git"]}
    })
}

fn source_adapter_display_text_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "string",
        "minLength": 1,
        "maxLength": 160,
        "pattern": "^[A-Za-z0-9](?:[A-Za-z0-9.,_-]| [A-Za-z0-9.,_-])*$",
        "allOf": [
            {"not": {"pattern": "[\\r\\n]"}},
            {"not": {"pattern": "  "}},
            {"not": {"pattern": "\\.\\."}},
            {"not": {"pattern": "^[sS][kK]-.{8,}$"}},
            {"not": {"pattern": "^[gG][hH][pP]_.{8,}$"}},
            {"not": {"pattern": "^[gG][iI][tT][hH][uU][bB]_[pP][aA][tT]_.{9,}$"}},
            {"not": {"pattern": "^[aA][kK][iI][aA].{8,}$"}},
            {"not": {"pattern": "^[bB][eE][aA][rR][eE][rR] .{8,}$"}}
        ]
    })
}

fn source_adapter_map_schema(generator: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "object",
        "maxProperties": 32,
        "propertyNames": source_adapter_id_schema(generator),
        "additionalProperties": generator.subschema_for::<McpSourceAdapterDescriptor>()
    })
}

fn source_adapters_list_result_schema(generator: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "object",
        "additionalProperties": false,
        "required": ["adapters"],
        "properties": {"adapters": source_adapter_map_schema(generator)}
    })
}

fn source_adapter_input_field_schema(generator: &mut SchemaGenerator) -> Schema {
    let text = source_adapter_input_field_variant(
        generator,
        serde_json::json!("text"),
        serde_json::json!({"type": "boolean"}),
        serde_json::json!({"const": false}),
    );
    let reference = source_adapter_input_field_variant(
        generator,
        serde_json::json!("reference"),
        serde_json::json!({"const": false}),
        serde_json::json!({"type": "boolean"}),
    );
    let directory = source_adapter_input_field_variant(
        generator,
        serde_json::json!("directory"),
        serde_json::json!({"const": false}),
        serde_json::json!({"const": false}),
    );
    let provider_reference = source_adapter_input_field_variant(
        generator,
        serde_json::json!("provider_reference"),
        serde_json::json!({"const": false}),
        serde_json::json!({"type": "boolean"}),
    );
    schemars::json_schema!({"oneOf": [text, reference, directory, provider_reference]})
}

fn source_adapter_input_field_variant(
    generator: &mut SchemaGenerator,
    kind: serde_json::Value,
    multiline: serde_json::Value,
    secret_reference_only: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["label", "kind", "required", "multiline", "secretReferenceOnly"],
        "properties": {
            "label": source_adapter_display_text_schema(generator),
            "kind": {"const": kind},
            "required": {"type": "boolean"},
            "multiline": multiline,
            "secretReferenceOnly": secret_reference_only
        }
    })
}

fn source_adapter_descriptor_schema(generator: &mut SchemaGenerator) -> Schema {
    let mut schema = schemars::json_schema!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "displayName", "sourceKind", "trustMode", "availability", "requiredApprovals", "risks",
            "requiredHostDependencies", "allowedTransports", "inputFields"
        ],
        "properties": {
            "displayName": source_adapter_display_text_schema(generator),
            "sourceKind": generator.subschema_for::<McpSourceAdapterSourceKind>(),
            "trustMode": generator.subschema_for::<McpSourceAdapterTrustMode>(),
            "availability": generator.subschema_for::<McpSourceAdapterAvailability>(),
            "requiredApprovals": {
                "type": "array", "maxItems": 6, "uniqueItems": true,
                "items": generator.subschema_for::<McpSourceAdapterApproval>()
            },
            "risks": {
                "type": "array", "minItems": 1, "maxItems": 6, "uniqueItems": true,
                "items": generator.subschema_for::<McpSourceAdapterRisk>()
            },
            "requiredHostDependencies": {
                "type": "array", "maxItems": 32, "uniqueItems": true,
                "items": source_adapter_id_schema(generator)
            },
            "allowedTransports": {
                "type": "array", "maxItems": 3, "uniqueItems": true,
                "items": generator.subschema_for::<McpSourceAdapterTransport>()
            },
            "inputFields": {
                "type": "object", "maxProperties": 32,
                "propertyNames": source_adapter_id_schema(generator),
                "additionalProperties": generator.subschema_for::<McpSourceAdapterInputField>()
            }
        }
    });
    schema.insert(
        "allOf".to_string(),
        serde_json::json!([
            source_adapter_kind_constraint(
                "catalog", ["verified"], ["network_access"], ["network_access"],
                ["catalog"], ["catalog"]
            ),
            source_adapter_kind_constraint(
                "remote_http", ["verified", "user_managed"],
                ["network_access", "credential_reference"], ["network_access"],
                ["remote_http"], ["remote_http"]
            ),
            source_adapter_kind_constraint(
                "manual_stdio", ["user_managed"],
                ["process_execution", "filesystem_read", "filesystem_write", "host_dependency", "credential_reference"],
                ["process_execution"], ["stdio_provider"], ["stdio_provider"]
            ),
            source_adapter_kind_constraint(
                "npm", ["verified", "user_managed"],
                ["network_access", "process_execution", "host_dependency", "credential_reference"],
                ["process_execution", "host_dependency"], ["catalog", "stdio_provider"], ["stdio_provider"]
            ),
            source_adapter_kind_constraint(
                "uvx", ["verified", "user_managed"],
                ["network_access", "process_execution", "host_dependency", "credential_reference"],
                ["process_execution", "host_dependency"], ["catalog", "stdio_provider"], ["stdio_provider"]
            ),
            source_adapter_kind_constraint(
                "docker", ["user_managed"],
                ["process_execution", "filesystem_read", "filesystem_write", "host_dependency", "credential_reference"],
                ["process_execution", "host_dependency"], ["stdio_provider"], ["stdio_provider"]
            ),
            source_adapter_kind_constraint(
                "git", ["user_managed"],
                ["network_access", "process_execution", "filesystem_read", "filesystem_write", "host_dependency", "credential_reference"],
                ["network_access", "process_execution"], ["stdio_provider"], ["stdio_provider"]
            ),
            source_adapter_opaque_kind_constraint(),
            serde_json::json!({
                "if": {
                    "properties": {"availability": {"properties": {"status": {"const": "ready_for_intake"}}, "required": ["status"]}},
                    "required": ["availability"]
                },
                "then": {"properties": {"requiredApprovals": {"maxItems": 0}}}
            }),
            serde_json::json!({
                "if": {"properties": {"requiredApprovals": {"minItems": 1}}, "required": ["requiredApprovals"]},
                "then": {
                    "properties": {"availability": {"properties": {"status": {"const": "blocked"}}, "required": ["status"]}}
                },
                "else": {
                    "properties": {"availability": {"not": {"properties": {"status": {"const": "blocked"}, "reason": {"const": "approval_required"}}, "required": ["status", "reason"]}}}
                }
            }),
            serde_json::json!({
                "if": {"properties": {"trustMode": {"const": "unverified"}}, "required": ["trustMode"]},
                "then": {"properties": {"availability": {"properties": {"status": {"const": "blocked"}}, "required": ["status"]}}}
            }),
            serde_json::json!({
                "if": {"properties": {"availability": {"properties": {"status": {"const": "blocked"}, "reason": {"const": "unverified"}}, "required": ["status", "reason"]}}, "required": ["availability"]},
                "then": {"properties": {"trustMode": {"const": "unverified"}}}
            }),
            source_adapter_required_approval_risk_constraint("network_access"),
            source_adapter_required_approval_risk_constraint("process_execution"),
            source_adapter_required_approval_risk_constraint("filesystem_read"),
            source_adapter_required_approval_risk_constraint("filesystem_write"),
            source_adapter_required_approval_risk_constraint("host_dependency"),
            source_adapter_required_approval_risk_constraint("credential_reference"),
            serde_json::json!({
                "if": {"properties": {"risks": {"contains": {"const": "host_dependency"}}}, "required": ["risks"]},
                "then": {"properties": {"requiredHostDependencies": {"minItems": 1}}},
                "else": {"properties": {"requiredHostDependencies": {"maxItems": 0}}}
            }),
            serde_json::json!({
                "if": {
                    "properties": {"risks": {"not": {"contains": {"const": "credential_reference"}}}},
                    "required": ["risks"]
                },
                "then": {
                    "properties": {
                        "inputFields": {
                            "additionalProperties": {
                                "properties": {"secretReferenceOnly": {"const": false}},
                                "required": ["secretReferenceOnly"]
                            }
                        }
                    }
                }
            })
        ]),
    );
    schema
}

fn source_adapter_required_approval_risk_constraint(approval: &str) -> serde_json::Value {
    serde_json::json!({
        "if": {
            "properties": {"requiredApprovals": {"contains": {"const": approval}}},
            "required": ["requiredApprovals"]
        },
        "then": {"properties": {"risks": {"contains": {"const": approval}}}}
    })
}

fn source_adapter_kind_constraint<
    const TRUST: usize,
    const RISKS: usize,
    const REQUIRED_RISKS: usize,
    const TRANSPORTS: usize,
    const REQUIRED_TRANSPORTS: usize,
>(
    kind: &str,
    trust_modes: [&str; TRUST],
    allowed_risks: [&str; RISKS],
    required_risks: [&str; REQUIRED_RISKS],
    allowed_transports: [&str; TRANSPORTS],
    required_transports: [&str; REQUIRED_TRANSPORTS],
) -> serde_json::Value {
    // serde supports concrete arrays only up to its Rust-version-specific trait
    // implementations. The schema wire format is an array, so normalize each
    // const-generic input to a Vec before passing it to `json!`.
    let trust_modes = trust_modes.to_vec();
    let allowed_risks = allowed_risks.to_vec();
    let required_risks = required_risks
        .iter()
        .map(|risk| serde_json::json!({"contains": {"const": risk}}))
        .collect::<Vec<_>>();
    let allowed_transports = allowed_transports.to_vec();
    let required_transports = required_transports
        .iter()
        .map(|transport| serde_json::json!({"contains": {"const": transport}}))
        .collect::<Vec<_>>();

    serde_json::json!({
        "if": {"properties": {"sourceKind": {"properties": {"type": {"const": kind}}, "required": ["type"]}}, "required": ["sourceKind"]},
        "then": {"properties": {
            "trustMode": {"enum": trust_modes},
            "risks": {
                "items": {"enum": allowed_risks},
                "allOf": required_risks
            },
            "allowedTransports": {
                "items": {"enum": allowed_transports},
                "allOf": required_transports
            }
        }}
    })
}

fn source_adapter_opaque_kind_constraint() -> serde_json::Value {
    serde_json::json!({
        "if": {"properties": {"sourceKind": {"properties": {"type": {"const": "opaque"}}, "required": ["type"]}}, "required": ["sourceKind"]},
        "then": {"properties": {
            "trustMode": {"const": "unverified"},
            "availability": {"properties": {"status": {"const": "blocked"}, "reason": {"const": "unverified"}}, "required": ["status", "reason"]},
            "requiredApprovals": {"maxItems": 0},
            "requiredHostDependencies": {"maxItems": 0},
            "allowedTransports": {"maxItems": 0},
            "inputFields": {"maxProperties": 0}
        }}
    })
}

fn reject_duplicate_ids<'a>(
    values: impl Iterator<Item = &'a str>,
    field: &str,
) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(format!("{field} contains duplicate identifiers"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpPlatformErrorCodeDto {
    InvalidRequest,
    UnsafeUrl,
    NotFound,
    IntegrityError,
    PolicyDenied,
    NotImplementedForPhase,
    OperationNotSupported,
    ManualStdioProviderUnavailable,
    RemoteHttpPolicyUnavailable,
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
    WitnessExpired {},
    WitnessConsumed {},
    AdapterVersionIncompatible {},
    ExternalCapabilityUnavailable { capability: String },
    ManualStdioProviderUnavailable { recovery: McpRecoverySuggestion },
    RemoteHttpPolicyUnavailable { recovery: McpRecoverySuggestion },
    ExternalPolicyDenied { capability: String },
    SupplyChainMismatch { authority: String },
    AuthenticationRequired { provider: String },
    PermissionGrantRequired { permission: String },
    OriginRejected { origin_type: String },
    ImmutableCommitUnavailable {},
    RepositoryTreeRejected {},
    DevelopmentModeRequired {},
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

#[derive(Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "goose.mcpHttpsProvisionPlanCreate_unstable",
    response = McpHttpsProvisionPlanCreateResponse
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsProvisionPlanCreateRequest {
    #[serde(deserialize_with = "deserialize_bounded_provision_id")]
    pub provision_id: String,
    #[serde(deserialize_with = "deserialize_manifest_digest")]
    pub expected_manifest_digest: String,
    #[serde(deserialize_with = "deserialize_bounded_idempotency_key")]
    pub idempotency_key: String,
}

impl std::fmt::Debug for McpHttpsProvisionPlanCreateRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsProvisionPlanCreateRequest")
            .field("provision_id", &"[REDACTED]")
            .field("expected_manifest_digest", &"[REDACTED]")
            .field("idempotency_key", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsProvisionPlanCreateResponse {
    pub outcome: McpPlatformOutcome<McpHttpsProvisionPlanReview>,
}

impl std::fmt::Debug for McpHttpsProvisionPlanCreateResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsProvisionPlanCreateResponse")
            .field("outcome", &McpHttpsProvisionPlanOutcomeDebug(&self.outcome))
            .finish()
    }
}

struct McpHttpsProvisionPlanOutcomeDebug<'a>(&'a McpPlatformOutcome<McpHttpsProvisionPlanReview>);

impl std::fmt::Debug for McpHttpsProvisionPlanOutcomeDebug<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            McpPlatformOutcome::Success { value } => formatter
                .debug_struct("Success")
                .field("value", value)
                .finish(),
            McpPlatformOutcome::Error { .. } => formatter
                .debug_struct("Error")
                .field("error", &"[REDACTED]")
                .finish(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsProvisionPlanReview {
    pub plan_id: String,
    pub plan_digest: String,
    pub expires_at_ms: i64,
    pub trust_tier: McpTrustTier,
    pub mcp_id: String,
    pub name: String,
    pub version: String,
    pub selected_manifest_digest: String,
    pub permissions: Vec<McpHttpsProvisionPermission>,
    pub file_effects: McpHttpsProvisionFileEffects,
    pub process_effects: McpHttpsProvisionProcessEffects,
    pub reversibility: McpHttpsProvisionReversibility,
    pub policy: McpHttpsProvisionPolicy,
    pub warnings: Vec<McpPlanWarning>,
    pub required_confirmations: Vec<McpHttpsProvisionConfirmation>,
    pub default_disabled: bool,
    pub recovery: McpRecoverySuggestion,
}

impl std::fmt::Debug for McpHttpsProvisionPlanReview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsProvisionPlanReview")
            .field("plan_id", &"[REDACTED]")
            .field("plan_digest", &"[REDACTED]")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("trust_tier", &self.trust_tier)
            .field("mcp_id", &"[REDACTED]")
            .field("name", &"[REDACTED]")
            .field("version", &"[REDACTED]")
            .field("selected_manifest_digest", &"[REDACTED]")
            .field("permissions", &self.permissions)
            .field("file_effects", &self.file_effects)
            .field("process_effects", &self.process_effects)
            .field("reversibility", &self.reversibility)
            .field("policy", &self.policy)
            .field("warnings", &self.warnings.len())
            .field("required_confirmations", &self.required_confirmations)
            .field("recovery", &self.recovery)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsProvisionPermission {
    pub kind: McpPermissionKind,
    pub required: bool,
}

impl std::fmt::Debug for McpHttpsProvisionPermission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsProvisionPermission")
            .field("kind", &self.kind)
            .field("required", &self.required)
            .finish()
    }
}

#[derive(Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsProvisionFileEffects {
    pub writes_files: bool,
    pub removes_files: bool,
    pub owned_items: u32,
}

impl std::fmt::Debug for McpHttpsProvisionFileEffects {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsProvisionFileEffects")
            .field("writes_files", &self.writes_files)
            .field("removes_files", &self.removes_files)
            .field("owned_items", &self.owned_items)
            .finish()
    }
}

#[derive(Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsProvisionProcessEffects {
    pub process_required_for_connection: bool,
    pub starts_during_confirmation: bool,
}

impl std::fmt::Debug for McpHttpsProvisionProcessEffects {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsProvisionProcessEffects")
            .field(
                "process_required_for_connection",
                &self.process_required_for_connection,
            )
            .field(
                "starts_during_confirmation",
                &self.starts_during_confirmation,
            )
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsProvisionReversibility {
    pub reversible: bool,
    pub strategy: McpRollbackStrategyName,
}

impl std::fmt::Debug for McpHttpsProvisionReversibility {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsProvisionReversibility")
            .field("reversible", &self.reversible)
            .field("strategy", &self.strategy)
            .finish()
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpRollbackStrategyName {
    #[default]
    Available,
    Unavailable,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHttpsProvisionPolicy {
    pub outcome: McpPolicyOutcome,
    pub reason_count: u32,
}

impl std::fmt::Debug for McpHttpsProvisionPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpHttpsProvisionPolicy")
            .field("outcome", &self.outcome)
            .field("reason_count", &self.reason_count)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpHttpsProvisionConfirmation {
    Policy,
    Permission,
}

impl From<McpPlanReview> for McpHttpsProvisionPlanReview {
    fn from(review: McpPlanReview) -> Self {
        Self {
            plan_id: review.plan_id,
            plan_digest: review.plan_digest,
            expires_at_ms: review.expires_at_ms,
            trust_tier: review.trust_tier,
            mcp_id: review.mcp_id,
            name: review.name,
            version: review.version,
            selected_manifest_digest: review.selected_manifest_digest,
            permissions: review
                .permissions
                .into_iter()
                .map(|permission| McpHttpsProvisionPermission {
                    kind: permission.kind,
                    required: permission.required,
                })
                .collect(),
            file_effects: McpHttpsProvisionFileEffects {
                writes_files: review.file_effects.writes_files,
                removes_files: review.file_effects.removes_files,
                owned_items: review.file_effects.owned_items,
            },
            process_effects: McpHttpsProvisionProcessEffects {
                process_required_for_connection: review
                    .process_effects
                    .process_required_for_connection,
                starts_during_confirmation: review.process_effects.starts_during_confirmation,
            },
            reversibility: McpHttpsProvisionReversibility {
                reversible: review.reversibility.reversible,
                strategy: if review.reversibility.reversible {
                    McpRollbackStrategyName::Available
                } else {
                    McpRollbackStrategyName::Unavailable
                },
            },
            policy: McpHttpsProvisionPolicy {
                outcome: review.policy.outcome,
                reason_count: review.policy.reasons.len() as u32,
            },
            warnings: review.warnings,
            required_confirmations: review
                .required_confirmations
                .into_iter()
                .map(|confirmation| match confirmation {
                    McpRequiredConfirmation::Policy { .. } => McpHttpsProvisionConfirmation::Policy,
                    McpRequiredConfirmation::Permission { .. } => {
                        McpHttpsProvisionConfirmation::Permission
                    }
                })
                .collect(),
            default_disabled: review.default_disabled,
            recovery: review.recovery,
        }
    }
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
#[request(
    method = "goose.mcpProjectionRecoveryResolve_unstable",
    response = McpProjectionRecoveryResolveResponse
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProjectionRecoveryResolveRequest {
    pub managed_mcp_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProjectionRecoveryResolveResponse {
    pub outcome: McpPlatformOutcome<McpProjectionRecoveryResolveResult>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProjectionRecoveryResolveResult {
    pub managed_mcp_id: String,
    pub result: McpProjectionRecoveryResolveStatus,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpProjectionRecoveryResolveStatus {
    #[default]
    Resolved,
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

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpRuntimeControl_unstable", response = McpRuntimeControlResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpRuntimeControlRequest {
    pub managed_mcp_id: String,
    pub expected_revision: i64,
    pub action: McpRuntimeAction,
    pub runtime_binding: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpRuntimeControlResponse {
    pub outcome: McpPlatformOutcome<McpManagedSummary>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpRuntimeAction {
    #[default]
    Start,
    Stop,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpSourcesPolicyGet_unstable", response = McpSourcesPolicyGetResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourcesPolicyGetRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourcesPolicyGetResponse {
    pub outcome: McpPlatformOutcome<McpSourcesPolicyState>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpSourceRefresh_unstable", response = McpSourceRefreshResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceRefreshRequest {
    pub source_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceRefreshResponse {
    pub outcome: McpPlatformOutcome<McpSourceRefreshResult>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "goose.mcpSourceProvisionPrepare_unstable",
    response = McpSourceProvisionPrepareResponse
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceProvisionPrepareRequest {
    #[serde(deserialize_with = "deserialize_bounded_local_directory")]
    pub local_directory: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceProvisionPrepareResponse {
    pub outcome: McpPlatformOutcome<McpSourceProvisionPrepareResult>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "goose.mcpSourceProvisionConfirm_unstable",
    response = McpSourceProvisionConfirmResponse
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceProvisionConfirmRequest {
    #[serde(deserialize_with = "deserialize_bounded_provision_id")]
    pub provision_id: String,
    #[serde(deserialize_with = "deserialize_bounded_confirmation_token")]
    pub confirmation_token: String,
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceProvisionConfirmResponse {
    pub outcome: McpPlatformOutcome<McpSourceProvisionConfirmResult>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceProvisionPrepareResult {
    pub provision_id: String,
    pub confirmation_token: String,
    pub expires_at_ms: i64,
    pub preview: McpSourceProvisionPreview,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceProvisionConfirmResult {
    pub provision_id: String,
    pub confirmed_at_ms: i64,
    pub preview: McpSourceProvisionPreview,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceProvisionPreview {
    pub source_id: String,
    pub display_name: String,
    pub root_digest: String,
    pub document_digest: String,
    pub endpoint_host: String,
    pub refresh_transport: McpSourceProvisionRefreshTransport,
    pub manifest_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub trust_basis: McpSourceProvisionTrustBasis,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceProvisionRefreshTransport {
    #[default]
    VerifiedSourceBundleV1,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceProvisionTrustBasis {
    #[default]
    UserPin,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceRefreshResult {
    pub source_id: String,
    pub display_name: String,
    pub document_digest: String,
    pub manifest_count: u32,
    pub refreshed_at_ms: i64,
}

fn deserialize_bounded_local_directory<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_string(deserializer, 4096, "localDirectory")
}

fn deserialize_bounded_provision_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_string(deserializer, 256, "provisionId")
}

fn deserialize_bounded_confirmation_token<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_string(deserializer, 512, "confirmationToken")
}

fn deserialize_bounded_https_manifest_url<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = deserialize_bounded_string(deserializer, 4096, "url")?;
    if value.trim() != value || !is_strict_https_url(&value) {
        return Err(D::Error::custom("url is invalid"));
    }
    Ok(value)
}

fn deserialize_bounded_idempotency_key<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_string(deserializer, 256, "idempotencyKey")
}

fn deserialize_manifest_digest<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(D::Error::custom("expected_manifest_digest is invalid"));
    }
    Ok(value)
}

fn deserialize_bounded_string<'de, D>(
    deserializer: D,
    maximum_bytes: usize,
    field: &'static str,
) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > maximum_bytes
        || value.chars().any(char::is_control)
    {
        return Err(D::Error::custom(format!("{field} is invalid")));
    }
    Ok(value)
}

fn is_strict_https_url(value: &str) -> bool {
    let Some(authority_and_path) = value.strip_prefix("https://") else {
        return false;
    };
    if authority_and_path.is_empty()
        || authority_and_path.contains(['@', '#'])
        || authority_and_path.chars().any(char::is_whitespace)
    {
        return false;
    }
    let authority_end = authority_and_path
        .find(['/', '?'])
        .unwrap_or(authority_and_path.len());
    let authority = &authority_and_path[..authority_end];
    !authority.is_empty() && !authority.starts_with(':')
}

fn is_strict_https_origin(value: &str) -> bool {
    let Some(authority) = value.strip_prefix("https://") else {
        return false;
    };
    !authority.is_empty()
        && !authority.contains(['/', '?', '#', '@'])
        && !authority.starts_with(':')
        && !authority.chars().any(char::is_whitespace)
}

fn deserialize_safe_preview_text<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = deserialize_bounded_string(deserializer, 1024, "preview")?;
    if value.contains(['?', '#', '@']) {
        return Err(D::Error::custom("preview contains unsafe content"));
    }
    Ok(value)
}

fn deserialize_safe_preview_origin<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = deserialize_bounded_string(deserializer, 512, "redactedOrigin")?;
    if !is_strict_https_origin(&value) {
        return Err(D::Error::custom("redactedOrigin is invalid"));
    }
    Ok(value)
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpGovernedImport_unstable", response = McpGovernedImportResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGovernedImportRequest {
    pub source: McpGovernedImportSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpGovernedImportSource {
    LocalManifest {
        #[serde(rename = "filePath")]
        file_path: String,
    },
    LocalDirectory {
        #[serde(rename = "directoryPath")]
        directory_path: String,
    },
}

impl Default for McpGovernedImportSource {
    fn default() -> Self {
        Self::LocalManifest {
            file_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGovernedImportResponse {
    pub outcome: McpPlatformOutcome<McpGovernedImportResult>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGovernedImportResult {
    pub source: McpGovernedCatalogSourceSummary,
    pub document: McpGovernedCatalogDocumentSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<McpCatalogSummary>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGovernedCatalogSourceSummary {
    pub source_id: String,
    pub import_kind: McpGovernedImportKind,
    pub display_name: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpGovernedImportKind {
    #[default]
    LocalPersistence,
    VerifiedSourceCatalog,
    HttpsManifestUrl,
    EnterpriseDirectory,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGovernedCatalogDocumentSummary {
    pub source_id: String,
    pub document_id: String,
    pub document_digest: String,
    pub document_kind: McpGovernedCatalogDocumentKind,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpGovernedCatalogDocumentKind {
    #[default]
    Manifest,
    Directory,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "goose.mcpManualStdioSourcesList_unstable",
    response = McpManualStdioSourcesListResponse
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManualStdioSourcesListRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManualStdioSourcesListResponse {
    pub outcome: McpPlatformOutcome<McpManualStdioSourcesPage>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpManualPlanCreate_unstable", response = McpManualPlanCreateResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManualPlanCreateRequest {
    pub connection: McpManualConnectionInput,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManualPlanCreateResponse {
    pub outcome: McpPlatformOutcome<McpPlanReview>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileList_unstable", response = McpProfileListResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileListRequest {
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileListResponse {
    pub outcome: McpPlatformOutcome<McpProfilePage>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileGet_unstable", response = McpProfileGetResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileGetRequest {
    pub profile_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileGetResponse {
    pub outcome: McpPlatformOutcome<McpProfileDetail>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileCreate_unstable", response = McpProfileCreateResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileCreateRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub managed_mcp_ids: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileCreateResponse {
    pub outcome: McpPlatformOutcome<McpProfileSummary>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileUpdate_unstable", response = McpProfileUpdateResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileUpdateRequest {
    pub profile_id: String,
    pub expected_revision: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub managed_mcp_ids: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileUpdateResponse {
    pub outcome: McpPlatformOutcome<McpProfileSummary>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileRestore_unstable", response = McpProfileRestoreResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileRestoreRequest {
    pub profile_id: String,
    pub source_revision: i64,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileRestoreResponse {
    pub outcome: McpPlatformOutcome<McpProfileSummary>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileArchive_unstable", response = McpProfileArchiveResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileArchiveRequest {
    pub profile_id: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileArchiveResponse {
    pub outcome: McpPlatformOutcome<McpProfileSummary>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileDraftCreate_unstable", response = McpProfileDraftCreateResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileDraftCreateRequest {
    pub text: String,
    #[serde(default)]
    pub locale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileDraftCreateResponse {
    pub outcome: McpPlatformOutcome<McpProfileDraft>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileModelRecommend_unstable", response = McpProfileModelRecommendResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileModelRecommendRequest {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub provider_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileModelRecommendResponse {
    pub outcome: McpPlatformOutcome<McpModelRecommendation>,
}

/// Explicit, bounded connectivity test for an already governed MCP profile and an existing
/// provider/model configuration. This request intentionally accepts no URLs, commands,
/// credentials, prompts, or tool arguments.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(
    method = "goose.mcpProfileConnectionTest_unstable",
    response = McpProfileConnectionTestResponse
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileConnectionTestRequest {
    pub profile_id: String,
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileConnectionTestResponse {
    pub outcome: McpPlatformOutcome<McpProfileConnectionTestResult>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileConnectionTestResult {
    pub passed: bool,
    pub stages: Vec<McpConnectionTestStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpConnectionTestStage {
    pub phase: McpConnectionTestPhase,
    pub status: McpConnectionTestStatus,
    pub code: McpConnectionTestCode,
    pub diagnostic: String,
    pub repair_suggestion: String,
    pub duration_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_mcp_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionTestPhase {
    Eligibility,
    McpConnectInitialize,
    ToolDiscovery,
    Cleanup,
    ModelRequest,
    ToolVisibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionTestStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionTestCode {
    Eligible,
    ProfileNotReady,
    PolicyDenied,
    McpConnectFailed,
    McpTimeout,
    ToolDiscoveryFailed,
    NoToolsExposed,
    ProviderNotConfigured,
    ModelNotConfigured,
    ProviderInitializationFailed,
    ModelRequestFailed,
    RuntimeActivationFailed,
    ResourceLimitExceeded,
    PrerequisitesFailed,
    CleanupFailed,
    ToolVisibilityValidated,
    ToolVisibilitySkipped,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileApplyPlanCreate_unstable", response = McpProfileApplyPlanCreateResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileApplyPlanCreateRequest {
    pub profile_id: String,
    pub profile_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileApplyPlanCreateResponse {
    pub outcome: McpPlatformOutcome<McpProfileApplyPlan>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "goose.mcpProfileApplyConfirm_unstable", response = McpProfileApplyConfirmResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileApplyConfirmRequest {
    pub plan_id: String,
    pub confirmation_token: String,
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileApplyConfirmResponse {
    pub outcome: McpPlatformOutcome<McpProfileApplicationToken>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfilePage {
    pub items: Vec<McpProfileSummary>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileSummary {
    pub profile_id: String,
    pub name: String,
    pub description: String,
    pub revision: i64,
    pub archived: bool,
    pub entries: Vec<McpProfileEntry>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_status: Option<McpCredentialStatus>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileEntry {
    pub managed_mcp_id: String,
    pub ordinal: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileDetail {
    pub profile: McpProfileSummary,
    pub history: Vec<McpProfileRevisionSummary>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileRevisionSummary {
    pub revision: i64,
    pub operation: String,
    pub actor: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileDraft {
    pub locale: String,
    pub name: String,
    pub description: String,
    pub entries: Vec<McpProfileEntry>,
    pub candidates: Vec<McpProfileDraftCandidate>,
    pub unresolved_terms: Vec<String>,
    pub low_confidence: bool,
    pub persisted: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileDraftCandidate {
    pub managed_mcp_id: String,
    pub mcp_id: String,
    pub name: String,
    pub description: String,
    pub confidence: f32,
    pub reason_code: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpModelRecommendation {
    pub candidates: Vec<McpModelRecommendationCandidate>,
    pub inventory_only: bool,
    pub mutated: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpModelRecommendationCandidate {
    pub provider_id: String,
    pub model_id: String,
    pub confidence: f32,
    pub reason_codes: Vec<String>,
    pub caveat_codes: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileApplyPlan {
    pub plan_id: String,
    pub profile_id: String,
    pub profile_revision: i64,
    pub merge_policy: String,
    pub entries: Vec<McpProfileApplyEntry>,
    pub expires_at_ms: i64,
    pub confirmation: McpProfileApplyConfirmation,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileApplyEntry {
    pub managed_mcp_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub health: String,
    pub auth_ready: bool,
    pub policy_ready: bool,
    pub readiness: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileApplyConfirmation {
    pub confirmation_token: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpProfileApplicationToken {
    pub token: String,
    pub plan_id: String,
    pub profile_id: String,
    pub profile_revision: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum McpManualConnectionInput {
    RemoteHttp {
        endpoint: String,
        #[serde(default)]
        auth: McpManualHttpAuth,
    },
    StdioProvider {
        source_id: String,
    },
}

impl Default for McpManualConnectionInput {
    fn default() -> Self {
        Self::RemoteHttp {
            endpoint: String::new(),
            auth: McpManualHttpAuth::None,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum McpManualHttpAuth {
    #[default]
    None,
    BearerReference {
        auth_reference: String,
    },
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_status: Option<McpCredentialStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_capability: Option<McpExternalCapabilityStatus>,
    pub eligibility: McpManagedEligibility,
    pub next_action: McpManagedNextAction,
    pub phase_capabilities: McpPhaseCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_control: Option<McpRuntimeControlCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpRuntimeControlCapability {
    pub binding: String,
    pub can_start: bool,
    pub can_stop: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPhaseCapabilities {
    pub session_enablement: McpPhaseCapability,
    pub tool_policy: McpPhaseCapability,
    pub profiles: McpPhaseCapability,
    pub model_suggestions: McpPhaseCapability,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpPhaseCapability {
    #[default]
    NotAvailableInThisPhase,
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpExternalCapabilityStatus {
    DockerCliMissing,
    DockerDaemonUnverified,
    DockerDaemonVerified,
    DockerDaemonPolicyDenied,
    GitMissing,
    GitAvailable,
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
    /// Signals that an explicit projection-recovery resolver action is appropriate.
    /// ACP dispatch determines whether that action is available to a caller.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supply_chain: Option<McpSupplyChainSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpSupplyChainSummary {
    Docker {
        image: String,
        image_digest: String,
        adapter_version: String,
        daemon_version: String,
        rootless: bool,
        mount_plan_digest: String,
        created_at_ms: i64,
    },
    GitDev {
        repository_origin: String,
        commit: String,
        git_tree_id: String,
        materialized_tree_digest: String,
        adapter_version: String,
        created_at_ms: i64,
    },
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpHealthStatus {
    pub managed_mcp_id: String,
    pub state: McpHealthState,
    pub latest: Option<McpHealthObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_status: Option<McpCredentialStatus>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpCredentialStatus {
    Unconfigured,
    ReRegistrationRequired,
    TrustedStateConflict,
    TemporarilyUnavailable,
    Ready,
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
pub struct McpCatalogPlanTarget {
    pub source_id: String,
    pub mcp_id: String,
    pub version: String,
    pub manifest_digest: String,
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
    pub freshness: McpCacheFreshness,
    pub refresh_state: McpRefreshState,
    pub recovery: McpRecoverySuggestion,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpCacheFreshness {
    Fresh,
    Stale,
    #[default]
    OfflineVerified,
    Empty,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpRefreshState {
    Idle,
    Refreshing,
    Failed,
    #[default]
    LocalOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceProvenance {
    pub source_ref: McpSourceRef,
    pub import_kind: McpSourceImportKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_source_document: Option<McpVerifiedSourceDocumentProvenance>,
}

impl Default for McpSourceProvenance {
    fn default() -> Self {
        Self {
            source_ref: McpSourceRef::LocalPersistence,
            import_kind: McpSourceImportKind::LocalPersistence,
            release_id: None,
            verified_source_document: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpSourceRef {
    LocalPersistence,
    VerifiedSourceCatalog {
        source_id: String,
    },
    HttpsManifestUrl {
        manifest_url: String,
    },
    EnterpriseDirectory {
        directory_id: String,
        entry_id: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpSourceImportKind {
    #[default]
    LocalPersistence,
    VerifiedSourceCatalog,
    HttpsManifestUrl,
    EnterpriseDirectory,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpVerifiedSourceDocumentProvenance {
    pub source_id: String,
    pub document_digest: String,
    pub canonical_digest: String,
    pub signed_digest: String,
    pub binding_digest: String,
    pub signature_kids: Vec<String>,
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
    pub source_provenance: McpSourceProvenance,
    pub proof: McpSourceProof,
    pub compatibility: McpCompatibility,
    pub distribution: McpDistributionKind,
    pub verified_at_ms: i64,
    pub eligibility: McpEligibility,
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
    pub source_provenance: McpSourceProvenance,
    pub distribution: McpDistributionContract,
    pub transport: McpTransportContract,
    pub auth: McpAuthContract,
    pub health: McpHealthContract,
    pub capabilities: Vec<McpCapability>,
    pub permissions: Vec<McpPermission>,
    pub compatibility: McpCompatibility,
    pub verified_at_ms: i64,
    pub eligibility: McpEligibility,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpEligibility {
    pub outcome: McpEligibilityOutcome,
    pub reason: McpEligibilityReason,
    pub recovery: McpRecoverySuggestion,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpEligibilityOutcome {
    #[default]
    Allowed,
    Restricted,
    Denied,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpEligibilityReason {
    #[default]
    Eligible,
    ConfirmationRequired,
    PlatformUnsupported,
    RuntimeUnavailable,
    PolicyDenied,
    DevelopmentModeRequired,
    ExternalCapabilityUnavailable,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpRecoverySuggestion {
    #[default]
    None,
    ReviewPermissions,
    ChooseCompatibleRelease,
    InstallRequiredRuntime,
    EnableDevelopmentMode,
    RestoreVerifiedCache,
    Retry,
    RecreatePlan,
    ResolveRecovery,
    ContactPolicyAdministrator,
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
        credential_required: bool,
        prefix_required: bool,
    },
    Environment {
        credential_required: bool,
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
    RegisterCatalog {
        catalog: McpCatalogPlanTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        installation_scope: Option<McpInstallationScope>,
    },
    Install {
        manifest_digest: String,
    },
    InstallCatalog {
        catalog: McpCatalogPlanTarget,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_target: Option<McpCatalogPlanTarget>,
    pub proof: McpSourceProof,
    pub trust_tier: McpTrustTier,
    pub source_provenance: McpSourceProvenance,
    pub publisher: McpPublisher,
    pub mcp_id: String,
    pub name: String,
    pub version: String,
    pub selected_manifest_digest: String,
    pub immutable_evidence: McpImmutableEvidence,
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
    pub recovery: McpRecoverySuggestion,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpImmutableEvidence {
    Artifact {
        sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_bytes: Option<u64>,
    },
    Docker {
        image: String,
        image_digest: String,
    },
    GitDev {
        repository_origin: String,
        commit: String,
        tree: McpEvidenceValue,
        materialized_digest: McpEvidenceValue,
    },
    Unavailable {
        reason: McpEvidenceUnavailableReason,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpEvidenceValue {
    Verified {
        value: String,
    },
    Unavailable {
        reason: McpEvidenceUnavailableReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpEvidenceUnavailableReason {
    NotApplicable,
    AvailableAfterMaterialization,
    NoArtifactForRegistration,
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
    pub strategy: McpRollbackStrategy,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpRollbackStrategy {
    RemoveConnectionRegistration,
    StagedActivationRestoresPreviousVersion,
    RepairRestoresVerifiedOwnedContent,
    UninstallRemovesOwnedFiles {
        preserve_user_data: bool,
    },
    #[default]
    Unavailable,
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
    ImmutableContainerImage,
    WritableContainerMount,
    DevelopmentSourcePinnedCommitNoBuild,
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
    pub outcome: McpTaskOutcomeSummary,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTaskOutcomeSummary {
    pub state: McpTaskOutcomeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<McpTaskClosedError>,
    pub rollback: McpTaskRollbackSummary,
    pub finalization: McpTaskFinalizationState,
    pub remaining_effects: Vec<McpRemainingEffect>,
    pub next_action: McpTaskNextAction,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTaskFinalizationState {
    #[default]
    Pending,
    Complete,
    RecoveryRequired,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTaskOutcomeState {
    #[default]
    Pending,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    RecoveryRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTaskClosedError {
    pub code: McpTaskErrorCode,
    pub retryable: bool,
    pub correlation_id: String,
    pub message: String,
    pub suggestion: McpRecoverySuggestion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTaskErrorCode {
    AdapterFailed,
    VerificationFailed,
    ActivationFailed,
    RollbackFailed,
    Cancelled,
    Interrupted,
    Unknown,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTaskRollbackSummary {
    #[default]
    NotRequired,
    Pending,
    InProgress,
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpRemainingEffect {
    ConnectionProjection,
    ManagedInstallation,
    ManagedUninstall,
    ExternalResource,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpTaskNextAction {
    #[default]
    None,
    Wait,
    CancelWhenSafe,
    Retry,
    Resume,
    ResolveRecovery,
    RecreatePlan,
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

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourcesPolicyState {
    pub sources: Vec<McpSourceState>,
    pub policy: McpMachinePolicyState,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSourceState {
    pub source_id: String,
    pub display_name: String,
    pub import_kind: McpGovernedImportKind,
    pub trust_tiers: Vec<McpTrustTier>,
    pub manifest_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_imported_at_ms: Option<i64>,
    pub cache: McpCatalogCacheMetadata,
    pub compatibility: McpCompatibilitySummary,
    pub recovery: McpRecoverySuggestion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh_document_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refreshed_at_ms: Option<i64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCompatibilitySummary {
    pub compatible: u32,
    pub restricted: u32,
    pub denied: u32,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpMachinePolicyState {
    pub target_platform: String,
    pub target_architecture: String,
    pub development_mode: bool,
    pub docker_allowed: bool,
    pub recovery: McpRecoverySuggestion,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManualStdioSourcesPage {
    pub items: Vec<McpManualStdioSource>,
    pub provider: McpManualStdioProviderState,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpManualStdioSource {
    pub source_id: String,
    pub display_name: String,
    pub publisher_name: String,
    pub trust_tier: McpTrustTier,
    pub compatibility: McpCompatibility,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpManualStdioProviderState {
    Available,
    #[default]
    OperationNotSupported,
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
mod https_manifest_wire_tests {
    use super::*;

    #[test]
    fn sensitive_debug_output_is_redacted() {
        let request = McpHttpsManifestPrepareRequest {
            url: "https://user:password@example.test/m?secret=1#fragment".into(),
        };
        assert!(!format!("{request:?}").contains("password"));
        assert!(!format!("{request:?}").contains("secret=1"));

        let request = McpHttpsManifestConfirmRequest {
            provision_id: "provision-secret".into(),
            confirmation_token: "token-secret".into(),
            confirm: true,
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("token-secret"));

        let result = McpHttpsManifestPrepareResult {
            provision_id: "p".into(),
            confirmation_token: "token-secret".into(),
            expires_at_ms: 1,
            preview: McpHttpsManifestSecurityPreview::default(),
        };
        assert!(!format!("{result:?}").contains("token-secret"));
    }

    #[test]
    fn wire_contract_rejects_unknown_or_forbidden_confirm_fields() {
        let json = r#"{"provisionId":"p","confirmationToken":"t","confirm":true,"idempotencyKey":"i","url":"https://x"}"#;
        assert!(serde_json::from_str::<McpHttpsManifestConfirmRequest>(json).is_err());
        assert!(serde_json::from_str::<McpHttpsManifestPrepareRequest>(
            r#"{"url":"http://x","idempotencyKey":"i"}"#
        )
        .is_err());
    }

    #[test]
    fn prepare_url_and_inputs_are_strictly_bounded() {
        for url in [
            "https://user:pass@example.test/m",
            "https://example.test/m#fragment",
            "https://example.test/m?secret=1 ",
            " https://example.test/m",
            "https://",
            "http://example.test/m",
        ] {
            let json = format!(r#"{{"url":"{url}"}}"#);
            assert!(serde_json::from_str::<McpHttpsManifestPrepareRequest>(&json).is_err());
        }
    }

    #[test]
    fn preview_rejects_sensitive_wire_content_and_uses_camel_case() {
        let sensitive = r#"{"manifestId":"m","version":"1","redactedOrigin":"https://example.test?token=1","rawDigest":"x","parsedDigest":"x","redirectChainDigest":"x","dnsEvidenceDigest":"x"}"#;
        assert!(serde_json::from_str::<McpHttpsManifestSecurityPreview>(sensitive).is_err());
        let unknown = r#"{"manifestId":"m","version":"1","redactedOrigin":"https://example.test","rawDigest":"x","parsedDigest":"x","redirectChainDigest":"x","dnsEvidenceDigest":"x","secret":"x"}"#;
        assert!(serde_json::from_str::<McpHttpsManifestSecurityPreview>(unknown).is_err());
        let preview = McpHttpsManifestSecurityPreview {
            manifest_id: "m".into(),
            version: "1".into(),
            redacted_origin: "https://example.test".into(),
            raw_digest: "x".into(),
            parsed_digest: "x".into(),
            redirect_chain_digest: "x".into(),
            dns_evidence_digest: "x".into(),
            warnings: vec![McpHttpsManifestWarning::Redirected],
        };
        let json = serde_json::to_string(&preview).unwrap();
        assert!(json.contains("redactedOrigin"));
    }

    #[test]
    fn confirm_debug_never_contains_token() {
        let response = McpHttpsManifestConfirmResponse {
            outcome: McpPlatformOutcome::success(McpHttpsManifestConfirmResult {
                provision_id: "provision-secret".into(),
                manifest_digest: "digest-secret".into(),
                confirmed_at_ms: 1,
                preview: McpHttpsManifestSecurityPreview::default(),
            }),
        };
        let debug = format!("{response:?}");
        assert!(!debug.contains("provision-secret"));
        assert!(!debug.contains("digest-secret"));
        assert!(!debug.contains("token-secret"));
    }
}

#[cfg(test)]
#[path = "mcp_platform_tests.rs"]
mod tests;
