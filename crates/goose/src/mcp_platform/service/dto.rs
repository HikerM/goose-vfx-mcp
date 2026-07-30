use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::mcp_platform::catalog::CatalogCompatibility;
use crate::mcp_platform::domain::TrustTier;
use crate::mcp_platform::domain::{
    HealthCheckMode, HealthState, InstallationState, RegistrationState, RuntimeState,
};
use crate::mcp_platform::manifest::SourceImportKind;
use crate::mcp_platform::manifest::{Manifest, ManifestProof, ManifestSourceMetadata};
use crate::mcp_platform::plan::InstallationPlan;
use crate::mcp_platform::policy::PolicyDecision;
use crate::mcp_platform::repository::GovernedCatalogDocumentKind;
use crate::mcp_platform::repository::GovernedSourceRefreshResultState;
use crate::mcp_platform::repository::{
    AuditEventRecord, HealthObservationRecord, PlanTarget, TaskRecord,
};

pub const LOCAL_PERSISTED_SOURCE_ID: &str = "local_persistence";

#[derive(Clone, PartialEq, Eq)]
struct TransportSessionBinding(String);

impl std::fmt::Debug for TransportSessionBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TransportSessionBinding([redacted])")
    }
}

impl TransportSessionBinding {
    fn from_authenticated_transport(value: String) -> Option<Self> {
        (!value.is_empty() && value.len() <= 256).then_some(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    actor: &'static str,
    correlation_id: String,
    transport_session_binding: Option<TransportSessionBinding>,
}

impl RequestContext {
    pub fn local_authenticated_client(correlation_id: String) -> Self {
        Self {
            actor: "local_authenticated_client",
            correlation_id,
            transport_session_binding: None,
        }
    }

    pub(crate) fn authenticated_transport_client(
        correlation_id: String,
        transport_session_binding: String,
    ) -> Self {
        Self {
            actor: "local_authenticated_client",
            correlation_id,
            transport_session_binding: TransportSessionBinding::from_authenticated_transport(
                transport_session_binding,
            ),
        }
    }

    pub const fn actor(&self) -> &'static str {
        self.actor
    }

    pub fn correlation_id(&self) -> &str {
        &self.correlation_id
    }

    pub(crate) fn transport_session_binding(&self) -> Option<&str> {
        self.transport_session_binding
            .as_ref()
            .map(TransportSessionBinding::as_str)
    }
}

#[derive(Debug, Clone, Default)]
pub struct CatalogListInput {
    pub query: Option<String>,
    pub trust_tiers: Vec<TrustTier>,
    pub source_ids: Vec<String>,
    pub compatibility: Option<CatalogCompatibility>,
    pub cursor: Option<String>,
    pub page_size: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSummary {
    pub source_id: String,
    pub manifest_digest: String,
    pub mcp_id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub publisher_id: String,
    pub publisher_name: String,
    pub trust_tier: TrustTier,
    pub proof: ManifestProof,
    pub source_metadata: ManifestSourceMetadata,
    pub compatibility: CatalogCompatibility,
    pub distribution_adapter: String,
    pub verified_at_ms: i64,
    pub eligibility: Eligibility,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPage {
    pub items: Vec<CatalogSummary>,
    pub next_cursor: Option<String>,
    pub offline: bool,
    pub local_persistence_only: bool,
    pub newest_verified_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EligibilityOutcome {
    Allowed,
    Restricted,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EligibilityReason {
    Eligible,
    ConfirmationRequired,
    PlatformUnsupported,
    RuntimeUnavailable,
    PolicyDenied,
    DevelopmentModeRequired,
    ExternalCapabilityUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverySuggestion {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eligibility {
    pub outcome: EligibilityOutcome,
    pub reason: EligibilityReason,
    pub recovery: RecoverySuggestion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogLocator {
    ManifestDigest(String),
    CatalogRef {
        source_id: String,
        mcp_id: String,
        version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPlanTarget {
    pub source_id: String,
    pub mcp_id: String,
    pub version: String,
    pub manifest_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogDetail {
    pub source_id: String,
    pub manifest_digest: String,
    pub proof: ManifestProof,
    pub trust_tier: TrustTier,
    pub source_metadata: ManifestSourceMetadata,
    pub compatibility: CatalogCompatibility,
    pub manifest: Manifest,
    pub verified_at_ms: i64,
    pub eligibility: Eligibility,
}

#[derive(Clone, PartialEq, Eq)]
pub struct GovernedManifestImportInput {
    pub document_bytes: Vec<u8>,
    pub source_metadata: ManifestSourceMetadata,
}

impl std::fmt::Debug for GovernedManifestImportInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GovernedManifestImportInput")
            .field("document_bytes_length", &self.document_bytes.len())
            .field("source_metadata_present", &true)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GovernedDirectoryManifestImportInput {
    pub release_id: String,
    pub document_bytes: Vec<u8>,
}

impl std::fmt::Debug for GovernedDirectoryManifestImportInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GovernedDirectoryManifestImportInput")
            .field("release_id", &self.release_id)
            .field("document_bytes_length", &self.document_bytes.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GovernedDirectoryImportInput {
    pub directory_document_bytes: Vec<u8>,
    pub source_metadata: ManifestSourceMetadata,
    pub manifests: Vec<GovernedDirectoryManifestImportInput>,
}

impl std::fmt::Debug for GovernedDirectoryImportInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GovernedDirectoryImportInput")
            .field(
                "directory_document_bytes_length",
                &self.directory_document_bytes.len(),
            )
            .field("source_metadata_present", &true)
            .field("manifest_count", &self.manifests.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourceProvisionPrepareInput {
    pub(crate) frozen: crate::mcp_platform::source_provisioning::FrozenSourceProvisioningSnapshot,
}

impl std::fmt::Debug for SourceProvisionPrepareInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let frozen = &self.frozen;
        let manifest_document_bytes_length = frozen
            .manifests
            .iter()
            .map(|manifest| manifest.document_bytes.len())
            .fold(0usize, usize::saturating_add);
        formatter
            .debug_struct("SourceProvisionPrepareInput")
            .field(
                "snapshot_digest",
                &crate::mcp_platform::source_provisioning::source_provisioning_snapshot_digest(
                    frozen,
                ),
            )
            .field("descriptor_bytes_length", &frozen.descriptor_bytes.len())
            .field(
                "source_document_bytes_length",
                &frozen.source_document_bytes.len(),
            )
            .field("manifest_count", &frozen.manifests.len())
            .field(
                "manifest_document_bytes_length",
                &manifest_document_bytes_length,
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourceProvisionConfirmInput {
    pub(crate) provision_id: String,
    pub(crate) confirmation_token: String,
    pub(crate) confirm: bool,
}

impl std::fmt::Debug for SourceProvisionConfirmInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceProvisionConfirmInput")
            .field("provision_id", &self.provision_id)
            .field(
                "confirmation_token_present",
                &!self.confirmation_token.is_empty(),
            )
            .field("confirmation_token_length", &self.confirmation_token.len())
            .field("confirm", &self.confirm)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceProvisionRefreshTransport {
    VerifiedSourceBundleV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceProvisionTrustBasis {
    UserPin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceProvisionPreview {
    pub(crate) source_id: String,
    pub(crate) display_name: String,
    pub(crate) root_digest: String,
    pub(crate) document_digest: String,
    pub(crate) endpoint_host: String,
    pub(crate) refresh_transport: SourceProvisionRefreshTransport,
    pub(crate) manifest_count: u32,
    pub(crate) warnings: Vec<String>,
    pub(crate) trust_basis: SourceProvisionTrustBasis,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourceProvisionPrepareResult {
    pub(crate) provision_id: String,
    pub(crate) confirmation_token: String,
    pub(crate) expires_at_ms: i64,
    pub(crate) preview: SourceProvisionPreview,
}

impl std::fmt::Debug for SourceProvisionPrepareResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceProvisionPrepareResult")
            .field("provision_id", &self.provision_id)
            .field(
                "confirmation_token_present",
                &!self.confirmation_token.is_empty(),
            )
            .field("confirmation_token_length", &self.confirmation_token.len())
            .field("expires_at_ms", &self.expires_at_ms)
            .field("preview", &self.preview)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceProvisionConfirmResult {
    pub(crate) provision_id: String,
    pub(crate) confirmed_at_ms: i64,
    pub(crate) preview: SourceProvisionPreview,
}

/// A remote HTTPS manifest is identified by its evidence, never by exposing
/// the URL or response bytes to the caller.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HttpsManifestPrepareInput {
    pub(crate) url: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HttpsManifestConfirmInput {
    pub(crate) provision_id: String,
    pub(crate) token: String,
    pub(crate) confirm: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HttpsManifestPlanReviewInput {
    pub(crate) provision_id: String,
    pub(crate) expected_manifest_digest: String,
    pub(crate) idempotency_key: String,
}

impl std::fmt::Debug for HttpsManifestPlanReviewInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpsManifestPlanReviewInput")
            .field("provision_id", &self.provision_id)
            .field("expected_manifest_digest", &self.expected_manifest_digest)
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HttpsManifestPreview {
    pub(crate) manifest_id: String,
    pub(crate) version: String,
    pub(crate) redacted_origin: String,
    pub(crate) raw_digest: String,
    pub(crate) parsed_digest: String,
    pub(crate) redirect_chain_digest: String,
    pub(crate) dns_evidence_digest: String,
}

impl std::fmt::Debug for HttpsManifestPreview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpsManifestPreview")
            .field("manifest_id", &self.manifest_id)
            .field("version", &self.version)
            .field("redacted_origin_present", &!self.redacted_origin.is_empty())
            .field("raw_digest", &self.raw_digest)
            .field("parsed_digest", &self.parsed_digest)
            .field("redirect_chain_digest", &self.redirect_chain_digest)
            .field("dns_evidence_digest", &self.dns_evidence_digest)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HttpsManifestPrepareResult {
    pub(crate) provision_id: String,
    pub(crate) server_token: String,
    pub(crate) expires_at_ms: i64,
    pub(crate) preview: HttpsManifestPreview,
}

impl std::fmt::Debug for HttpsManifestPrepareResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpsManifestPrepareResult")
            .field("provision_id", &self.provision_id)
            .field("server_token_present", &!self.server_token.is_empty())
            .field("expires_at_ms", &self.expires_at_ms)
            .field("preview", &self.preview)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpsManifestConfirmResult {
    pub(crate) provision_id: String,
    pub(crate) confirmed_at_ms: i64,
    pub(crate) manifest_digest: String,
    pub(crate) preview: HttpsManifestPreview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedCatalogSourceSummary {
    pub source_id: String,
    pub import_kind: crate::mcp_platform::SourceImportKind,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedCatalogDocumentSummary {
    pub source_id: String,
    pub document_id: String,
    pub document_digest: String,
    pub document_kind: GovernedCatalogDocumentKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedImportResult {
    pub source: GovernedCatalogSourceSummary,
    pub document: GovernedCatalogDocumentSummary,
    pub entries: Vec<CatalogSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationScope {
    User,
}

impl InstallationScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanIntent {
    Register {
        manifest_digest: String,
        installation_scope: InstallationScope,
    },
    RegisterCatalog {
        catalog: CatalogPlanTarget,
        installation_scope: InstallationScope,
    },
    Install {
        manifest_digest: String,
    },
    InstallCatalog {
        catalog: CatalogPlanTarget,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualConnectionInput {
    RemoteHttp {
        endpoint: String,
        auth: ManualHttpAuth,
    },
    StdioProvider {
        source_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualHttpAuth {
    None,
    BearerReference { auth_reference: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualPlanCreateInput {
    pub connection: ManualConnectionInput,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCreateInput {
    pub intent: PlanIntent,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanReview {
    pub plan_id: String,
    pub plan_digest: String,
    pub expires_at_ms: i64,
    pub source_id: String,
    pub catalog_target: Option<CatalogPlanTarget>,
    pub proof: ManifestProof,
    pub trust_tier: TrustTier,
    pub source_metadata: ManifestSourceMetadata,
    pub manifest: Manifest,
    pub plan: InstallationPlan,
    pub target: PlanTarget,
    pub policy: PolicyDecision,
    pub immutable_evidence: ImmutableEvidence,
    pub reversibility: PlanReversibility,
    pub recovery: RecoverySuggestion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImmutableEvidence {
    Artifact {
        sha256: String,
        size_bytes: Option<u64>,
    },
    Docker {
        image: String,
        image_digest: String,
    },
    GitDev {
        repository_origin: String,
        commit: String,
        tree: EvidenceValue,
        materialized_digest: EvidenceValue,
    },
    Unavailable {
        reason: EvidenceUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceValue {
    Verified(String),
    Unavailable(EvidenceUnavailableReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceUnavailableReason {
    NotApplicable,
    AvailableAfterMaterialization,
    NoArtifactForRegistration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReversibility {
    pub reversible: bool,
    pub strategy: RollbackStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackStrategy {
    RemoveConnectionRegistration,
    StagedActivationRestoresPreviousVersion,
    RepairRestoresVerifiedOwnedContent,
    UninstallRemovesOwnedFiles { preserve_user_data: bool },
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserDecision {
    Confirm,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallConfirmInput {
    pub plan_id: String,
    pub plan_digest: String,
    pub decision: UserDecision,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGetInput {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCancelInput {
    pub task_id: String,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRetryInput {
    pub task_id: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedListInput {
    pub cursor: Option<String>,
    pub page_size: Option<u16>,
    pub registration: Option<RegistrationState>,
    pub installation: Option<InstallationState>,
    pub runtime: Option<RuntimeState>,
    pub health: Option<HealthState>,
    pub default_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedMcpSummary {
    pub managed_mcp_id: String,
    pub mcp_id: String,
    pub installation_scope: String,
    pub registration: RegistrationState,
    pub installation: InstallationState,
    pub runtime: RuntimeState,
    pub health: HealthState,
    pub default_enabled: bool,
    pub revision: i64,
    pub updated_at_ms: i64,
    pub distribution_adapter: String,
    pub active_version: Option<String>,
    pub available_version: Option<String>,
    pub available_manifest_digest: Option<String>,
    pub current_task: Option<TaskRef>,
    pub recovery_required: bool,
    pub credential_status: Option<CredentialStatus>,
    pub external_capability: Option<ManagedExternalCapabilityStatus>,
    pub eligibility: ManagedEligibility,
    pub next_action: ManagedNextAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStatus {
    Unconfigured,
    ReRegistrationRequired,
    TrustedStateConflict,
    TemporarilyUnavailable,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedCredentialEnrollmentBeginInput {
    pub user_action_binding: crate::mcp_platform::credential_enrollment::UserActionBinding,
    pub managed_mcp_id: String,
    pub expected_revision: i64,
    pub profile_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedCredentialEnrollmentBeginView {
    pub session_token: String,
    pub managed_mcp_id: String,
    pub manifest_digest: String,
    pub schema: crate::mcp_platform::credential_enrollment::CredentialEnrollmentSchemaView,
    pub expected_revision: i64,
    pub profile_scope: Option<String>,
    pub expires_at_ms: i64,
}

pub(crate) struct ManagedCredentialEnrollmentSubmitInput {
    pub user_action_binding: crate::mcp_platform::credential_enrollment::UserActionBinding,
    pub session_token: String,
    pub current_profile_scope: Option<String>,
    pub fields: Vec<crate::mcp_platform::credential_enrollment::EnrollmentFieldSubmission>,
}

impl std::fmt::Debug for ManagedCredentialEnrollmentSubmitInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedCredentialEnrollmentSubmitInput")
            .field("session_token", &self.session_token)
            .field("fields", &self.fields)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedCredentialEnrollmentSubmitView {
    pub managed_mcp_id: String,
    pub manifest_digest: String,
    pub credential_status: CredentialStatus,
    pub enrollment_revision: i64,
    pub updated_at_ms: i64,
    pub redacted_field_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedCredentialEnrollmentCancelInput {
    pub user_action_binding: crate::mcp_platform::credential_enrollment::UserActionBinding,
    pub session_token: String,
    pub profile_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedCredentialEnrollmentCancelView {
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedCredentialEnrollmentStatusInput {
    pub managed_mcp_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedCredentialEnrollmentStatusView {
    pub managed_mcp_id: String,
    pub credential_status: CredentialStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePolicyState {
    pub sources: Vec<SourceState>,
    pub policy: MachinePolicyState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceState {
    pub source_id: String,
    pub display_name: String,
    pub import_kind: SourceImportKind,
    pub trust_tiers: Vec<TrustTier>,
    pub manifest_count: u32,
    pub last_imported_at_ms: Option<i64>,
    pub newest_verified_at_ms: Option<i64>,
    pub refreshable: bool,
    pub last_refreshed_at_ms: Option<i64>,
    pub last_refresh_document_digest: Option<String>,
    pub last_refresh_result: Option<GovernedSourceRefreshResultState>,
    pub compatibility: CompatibilitySummary,
    pub recovery: RecoverySuggestion,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompatibilitySummary {
    pub compatible: u32,
    pub restricted: u32,
    pub denied: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachinePolicyState {
    pub target_platform: String,
    pub target_architecture: String,
    pub development_mode: bool,
    pub docker_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedSourceRefreshInput {
    pub source_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedSourceRefreshResult {
    pub source_id: String,
    pub display_name: String,
    pub document_digest: String,
    pub manifest_count: u32,
    pub refreshed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualStdioSourcesPage {
    pub items: Vec<ManualStdioSource>,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualStdioSource {
    pub source_id: String,
    pub display_name: String,
    pub publisher_name: String,
    pub trust_tier: TrustTier,
    pub compatible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedExternalCapabilityStatus {
    DockerCliMissing,
    DockerDaemonUnverified,
    DockerDaemonVerified,
    DockerDaemonPolicyDenied,
    GitMissing,
    GitAvailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedEligibility {
    pub update: bool,
    pub repair: bool,
    pub uninstall: bool,
    pub reason: ManagedEligibilityReason,
    pub update_reason: ManagedEligibilityReason,
    pub repair_reason: ManagedEligibilityReason,
    pub uninstall_reason: ManagedEligibilityReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedEligibilityReason {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedNextAction {
    None,
    EnableAfterHealth,
    Repair,
    ResolveRecovery,
    WaitForTask,
    ResumeTask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedMcpPage {
    pub items: Vec<ManagedMcpSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedGetInput {
    pub managed_mcp_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedMcpDetail {
    pub summary: ManagedMcpSummary,
    pub distribution_adapter: String,
    pub active_manifest_digest: String,
    pub active_version: String,
    pub source_metadata: ManifestSourceMetadata,
    pub extension_config_key: String,
    pub projection_digest: String,
    pub latest_health: Option<HealthObservationRecord>,
    pub registration_task: Option<TaskRef>,
    pub supply_chain: Option<ManagedSupplyChainSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedSupplyChainSummary {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthRunInput {
    pub managed_mcp_id: String,
    pub mode: HealthCheckMode,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthGetInput {
    pub managed_mcp_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthStatus {
    pub managed_mcp_id: String,
    pub state: HealthState,
    pub latest: Option<HealthObservationRecord>,
    pub credential_status: Option<CredentialStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetDefaultEnabledInput {
    pub managed_mcp_id: String,
    pub enabled: bool,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedRuntimeAction {
    Start,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRuntimeControlInput {
    pub managed_mcp_id: String,
    pub expected_revision: i64,
    pub action: ManagedRuntimeAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRef {
    pub task_id: String,
    pub operation: crate::mcp_platform::task::TaskOperation,
    pub status: crate::mcp_platform::task::TaskStatus,
    pub progress: u8,
    pub cancellable: bool,
    pub revision: i64,
    pub updated_at_ms: i64,
    pub redacted_error: Option<crate::mcp_platform::task::RedactedError>,
    pub rollback_status: crate::mcp_platform::task::RollbackStatus,
    pub rollback_evidence: Option<crate::mcp_platform::task::RollbackEvidence>,
}

impl From<TaskRecord> for TaskRef {
    fn from(task: TaskRecord) -> Self {
        Self {
            cancellable: matches!(
                task.status,
                crate::mcp_platform::task::TaskStatus::AwaitingConfirmation
                    | crate::mcp_platform::task::TaskStatus::Queued
                    | crate::mcp_platform::task::TaskStatus::Running
            ),
            task_id: task.task_id,
            operation: task.operation,
            status: task.status,
            progress: task.progress,
            revision: task.revision,
            updated_at_ms: task.updated_at_ms,
            redacted_error: task.redacted_error,
            rollback_status: task.rollback_status,
            rollback_evidence: task.rollback_evidence,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventsResumeInput {
    pub after_event_id: Option<i64>,
    pub limit: Option<u16>,
    pub task_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventsResumePage {
    pub events: Vec<AuditEventRecord>,
    pub next_event_id: i64,
    pub tasks: Vec<TaskRef>,
}

pub(crate) fn unique_task_ids(events: &[AuditEventRecord], requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .cloned()
        .chain(events.iter().map(|event| event.task_id.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
