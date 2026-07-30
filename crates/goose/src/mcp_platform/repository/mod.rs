//! Public repository APIs do not expose profile credential references.
//!
//! ```compile_fail
//! # async fn demo(repo: &goose::mcp_platform::SqliteMcpPlatformRepository) {
//! let _ = repo.get_profile_credential_references("profile").await;
//! # }
//! ```
//!
//! ```compile_fail
//! # async fn demo(repo: &goose::mcp_platform::SqliteMcpPlatformRepository) {
//! let _ = repo
//!     .get_profile_revision_credential_references("profile", 1)
//!     .await;
//! # }
//! ```

pub mod sqlite;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use super::domain::{
    HealthCheckMode, HealthDetailCode, HealthResultCode, ManagedMcpState, ToolPolicy, TrustTier,
};
use super::error::{McpPlatformErrorCode, McpPlatformResult};
use super::managed_distribution::ArtifactVerificationEvidence;
use super::manifest::{ManifestProof, ManifestSourceMetadata, VerifiedManifest};
use super::plan::{ConnectionProjection, InstallationPlan};
use super::policy::PolicyDecision;
use super::task::{
    AdapterEvidence, CompensationDescriptor, CompensationStatus, RecoveryDecision, RedactedError,
    RollbackEvidence, RollbackStatus, StepEvidence, TaskOperation, TaskStatus, TaskStepStatus,
};
use super::SupplyChainEvidence;

pub(crate) use super::lifecycle::ProjectionSinkAtomicProof;
pub(crate) use sqlite::system_integrity_signer;
pub(crate) use sqlite::{AnchorCheckpoint, InMemoryIntegritySigner, IntegritySigner};
pub use sqlite::{DatabaseDiagnostics, SqliteMcpPlatformRepository};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryEligibility {
    Eligible,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAssurance {
    DbExternalTamperEvidence,
}

#[derive(Debug)]
pub(crate) struct AuthorizationIssuer(());

impl AuthorizationIssuer {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionAuthorization {
    pub checkpoint_root: String,
    pub checkpoint_sequence: u64,
    pub task: TaskRecord,
    pub plan: PlanRecord,
    pub manifest: ManifestRecord,
    pub steps: Vec<TaskStepRecord>,
    pub health_request: Option<HealthTaskRequestRecord>,
    pub inventory: Option<ManagedMcpInventoryRecord>,
    pub projection: Option<ConnectionProjectionRecord>,
    pub provider_assurance: ProviderAssurance,
    issuer: Arc<AuthorizationIssuer>,
}

/// A queued task identity captured before a worker performs external preflight.
///
/// It intentionally does not authorize execution. The repository compares every
/// bound field again when the worker later attempts to claim the task.
#[derive(Debug, Clone)]
pub(crate) struct QueuedTaskCandidate {
    task: TaskRecord,
}

impl QueuedTaskCandidate {
    pub(crate) fn task(&self) -> &TaskRecord {
        &self.task
    }
}

/// Trusted persisted facts a worker may inspect before changing a task to running.
#[derive(Debug, Clone)]
pub(crate) struct QueuedTaskPreflight {
    candidate: QueuedTaskCandidate,
    plan: PlanRecord,
    manifest: ManifestRecord,
    checkpoint_root: String,
    checkpoint_sequence: u64,
    issuer: Arc<AuthorizationIssuer>,
}

impl QueuedTaskPreflight {
    pub(crate) fn candidate(&self) -> &QueuedTaskCandidate {
        &self.candidate
    }

    pub(crate) fn plan(&self) -> &PlanRecord {
        &self.plan
    }

    pub(crate) fn manifest(&self) -> &ManifestRecord {
        &self.manifest
    }

    pub(crate) fn checkpoint_root(&self) -> &str {
        &self.checkpoint_root
    }

    pub(crate) const fn checkpoint_sequence(&self) -> u64 {
        self.checkpoint_sequence
    }

    pub(crate) fn issued_by(&self, issuer: &Arc<AuthorizationIssuer>) -> bool {
        Arc::ptr_eq(&self.issuer, issuer)
    }
}

impl ExecutionAuthorization {
    pub(crate) fn new(
        checkpoint_root: String,
        checkpoint_sequence: u64,
        task: TaskRecord,
        plan: PlanRecord,
        manifest: ManifestRecord,
        steps: Vec<TaskStepRecord>,
        health_request: Option<HealthTaskRequestRecord>,
        inventory: Option<ManagedMcpInventoryRecord>,
        projection: Option<ConnectionProjectionRecord>,
        issuer: Arc<AuthorizationIssuer>,
    ) -> Self {
        Self {
            checkpoint_root,
            checkpoint_sequence,
            task,
            plan,
            manifest,
            steps,
            health_request,
            inventory,
            projection,
            provider_assurance: ProviderAssurance::DbExternalTamperEvidence,
            issuer,
        }
    }

    pub(crate) fn issued_by(&self, issuer: &Arc<AuthorizationIssuer>) -> bool {
        Arc::ptr_eq(&self.issuer, issuer)
    }
}

#[derive(Debug, Clone)]
pub struct ManifestRecord {
    pub verified: VerifiedManifest,
    pub proof: ManifestProof,
    pub trust_tier: TrustTier,
    pub source_metadata: ManifestSourceMetadata,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    IdempotentReplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernedCatalogDocumentKind {
    Manifest,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernedCatalogSourceRecord {
    pub source_id: String,
    pub import_kind: super::manifest::SourceImportKind,
    pub source_ref: super::manifest::SourceRef,
    pub display_name: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernedSourceRefreshTransportKind {
    VerifiedSourceBundleV1,
}

impl GovernedSourceRefreshTransportKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedSourceBundleV1 => "verified_source_bundle_v1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GovernedSourceTrustBasis {
    UserPin,
}

impl GovernedSourceTrustBasis {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::UserPin => "user_pin",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GovernedSourceTrustPinRecord {
    pub(crate) source_id: String,
    pub(crate) verified_source_id: String,
    pub(crate) trust_basis: GovernedSourceTrustBasis,
    pub(crate) root_digest: String,
    pub(crate) endpoint: String,
    pub(crate) source_document_digest: String,
    pub(crate) descriptor_digest: String,
    pub(crate) created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct ConfirmSourceProvisioning {
    pub(crate) provision_id: String,
    pub(crate) snapshot_digest: String,
    pub(crate) frozen: crate::mcp_platform::source_provisioning::FrozenSourceProvisioningSnapshot,
    pub(crate) import: SaveGovernedCatalogImport,
    pub(crate) actor: String,
    pub(crate) correlation_id: String,
    pub(crate) occurred_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceProvisioningAuditRecord {
    pub(crate) provision_id: String,
    pub(crate) source_id: String,
    pub(crate) document_digest: String,
    pub(crate) root_digest: String,
    pub(crate) descriptor_digest: String,
    pub(crate) trust_basis: GovernedSourceTrustBasis,
    pub(crate) actor: String,
    pub(crate) correlation_id: String,
    pub(crate) occurred_at_ms: i64,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HttpsManifestProvisionRecord {
    pub(crate) provision_id: String,
    pub(crate) actor: String,
    pub(crate) transport_session_binding: String,
    pub(crate) requested_url: Option<String>,
    pub(crate) requested_url_id: String,
    pub(crate) final_url_id: String,
    pub(crate) raw_digest: String,
    pub(crate) parsed_digest: String,
    pub(crate) redirect_chain_digest: String,
    pub(crate) dns_evidence_digest: Option<String>,
    pub(crate) frozen_bytes: Vec<u8>,
    pub(crate) expires_at_ms: i64,
    pub(crate) status: String,
    pub(crate) created_at_ms: i64,
}

impl std::fmt::Debug for HttpsManifestProvisionRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsManifestProvisionRecord")
            .field("provision_id", &self.provision_id)
            .field("actor", &self.actor)
            .field(
                "transport_session_binding_present",
                &!self.transport_session_binding.is_empty(),
            )
            .field("requested_url_id", &self.requested_url_id)
            .field("final_url_id", &self.final_url_id)
            .field("raw_digest", &self.raw_digest)
            .field("parsed_digest", &self.parsed_digest)
            .field("redirect_chain_digest", &self.redirect_chain_digest)
            .field("dns_evidence_digest", &self.dns_evidence_digest)
            .field("frozen_bytes_length", &self.frozen_bytes.len())
            .field("expires_at_ms", &self.expires_at_ms)
            .field("status", &self.status)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GovernedSourceRefreshResultState {
    Idle,
    Succeeded,
    Failed,
}

impl GovernedSourceRefreshResultState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernedSourceRefreshRegistrationRecord {
    pub source_id: String,
    pub transport_kind: GovernedSourceRefreshTransportKind,
    pub endpoint: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_attempted_at_ms: Option<i64>,
    pub last_refreshed_at_ms: Option<i64>,
    pub last_result: GovernedSourceRefreshResultState,
    pub last_document_digest: Option<String>,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SaveGovernedSourceRefreshRegistration<'a> {
    pub source_id: &'a str,
    pub transport_kind: GovernedSourceRefreshTransportKind,
    pub endpoint: &'a str,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedSourceRefreshTrustAnchorRecord {
    pub source_id: String,
    pub endpoint: String,
    pub anchor: crate::verified_source_catalog::SourceTrustAnchorV1,
    pub anchor_document_digest: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct ApplyGovernedSourceRefresh {
    pub source_id: String,
    pub expected_endpoint: String,
    pub expected_anchor_root_digest: String,
    pub import: SaveGovernedCatalogImport,
    pub actor: String,
    pub correlation_id: String,
    pub occurred_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernedSourceRefreshCommitOutcome {
    Applied,
    IdempotentReplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernedSourceRefreshAuditRecord {
    pub audit_id: i64,
    pub source_id: String,
    pub document_digest: Option<String>,
    pub result: GovernedSourceRefreshResultState,
    pub error_code: Option<String>,
    pub actor: String,
    pub correlation_id: String,
    pub occurred_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct RecordGovernedSourceRefreshAudit<'a> {
    pub source_id: &'a str,
    pub document_digest: Option<&'a str>,
    pub result: GovernedSourceRefreshResultState,
    pub error_code: Option<&'a str>,
    pub actor: &'a str,
    pub correlation_id: &'a str,
    pub occurred_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernedCatalogDocumentRecord {
    pub source_id: String,
    pub document_id: String,
    pub document_digest: String,
    pub document_kind: GovernedCatalogDocumentKind,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct SaveGovernedCatalogEntry {
    pub entry_id: String,
    pub manifest: ManifestRecord,
}

#[derive(Debug, Clone)]
pub struct SaveGovernedCatalogImport {
    pub source: GovernedCatalogSourceRecord,
    pub document: GovernedCatalogDocumentRecord,
    pub verified_source_document: Option<crate::verified_source_catalog::VerifiedSourceDocument>,
    pub entries: Vec<SaveGovernedCatalogEntry>,
}

#[derive(Debug, Clone)]
pub struct GovernedCatalogEntryRecord {
    pub source: GovernedCatalogSourceRecord,
    pub document: GovernedCatalogDocumentRecord,
    pub entry_id: String,
    pub manifest: ManifestRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedVersionRecord {
    pub version: String,
    pub manifest_digest: String,
    pub installation_root: Option<String>,
    pub verified: bool,
    pub active: bool,
    pub adapter_evidence: Option<AdapterEvidence>,
    pub materialized_tree_digest: Option<String>,
    pub supply_chain_evidence: Option<SupplyChainEvidence>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedMcpRecord {
    pub managed_mcp_id: String,
    pub mcp_id: String,
    pub installation_scope: String,
    pub state: ManagedMcpState,
    pub revision: i64,
    pub versions: Vec<ManagedVersionRecord>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewManagedMcp {
    pub managed_mcp_id: String,
    pub mcp_id: String,
    pub installation_scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionProjectionRecord {
    pub managed_mcp_id: String,
    pub link_key: String,
    pub projection: ConnectionProjection,
    pub revision: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_task_id: Option<String>,
    #[serde(default)]
    pub projection_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedMcpLifecycleMetadata {
    pub distribution_adapter: String,
    pub active_manifest_digest: Option<String>,
    pub active_version: Option<String>,
    pub owner_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedMcpInventoryRecord {
    pub managed: ManagedMcpRecord,
    pub lifecycle: ManagedMcpLifecycleMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManifestSourceContext {
    GovernedCatalog { source_id: String },
    PersistedManifest { source_id: String },
    HttpsProvision { binding: HttpsManifestSourceBinding },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpsManifestSourceBinding {
    pub provision_id: String,
    pub requested_url_id: String,
    pub final_url_id: String,
    pub redirect_chain_digest: String,
    pub dns_evidence_digest: String,
    pub raw_digest: String,
    pub parsed_digest: String,
}

impl ManifestSourceContext {
    pub fn source_id(&self) -> &str {
        match self {
            Self::GovernedCatalog { source_id } | Self::PersistedManifest { source_id } => {
                source_id
            }
            Self::HttpsProvision { binding } => &binding.provision_id,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManagedCredentialEnrollmentAuthoritySummary {
    pub provider_id: String,
    pub writer_mode: String,
}

impl fmt::Debug for ManagedCredentialEnrollmentAuthoritySummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedCredentialEnrollmentAuthoritySummary")
            .field("provider_id", &"[REDACTED]")
            .field("writer_mode", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ManagedCredentialEnrollmentSummary {
    pub managed_mcp_id: String,
    pub enrollment_revision: i64,
    pub updated_at_ms: i64,
}

impl fmt::Debug for ManagedCredentialEnrollmentSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedCredentialEnrollmentSummary")
            .field("managed_mcp_id", &self.managed_mcp_id)
            .field("enrollment_revision", &self.enrollment_revision)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ManagedCredentialEnrollmentRecord {
    pub(crate) managed_mcp_id: String,
    pub(crate) manifest_digest: String,
    pub(crate) auth_schema_id: String,
    pub(crate) credential_reference: String,
    pub(crate) reference_digest: String,
    pub(crate) authority: ManagedCredentialEnrollmentAuthoritySummary,
    pub(crate) authority_evidence_digest: Option<String>,
    pub(crate) revision: i64,
    pub(crate) updated_at_ms: i64,
}

impl fmt::Debug for ManagedCredentialEnrollmentRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedCredentialEnrollmentRecord")
            .field("managed_mcp_id", &self.managed_mcp_id)
            .field("manifest_digest", &"[REDACTED]")
            .field("auth_schema_id", &"[REDACTED]")
            .field("credential_reference", &"[REDACTED]")
            .field("reference_digest", &"[REDACTED]")
            .field("authority", &"[REDACTED]")
            .field(
                "authority_evidence_digest",
                &self
                    .authority_evidence_digest
                    .as_ref()
                    .map(|_| "[REDACTED]"),
            )
            .field("revision", &self.revision)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

impl ManagedCredentialEnrollmentRecord {
    pub(crate) fn redacted_summary(&self) -> ManagedCredentialEnrollmentSummary {
        ManagedCredentialEnrollmentSummary {
            managed_mcp_id: self.managed_mcp_id.clone(),
            enrollment_revision: self.revision,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

pub(crate) struct SaveManagedCredentialEnrollment<'a> {
    pub managed_mcp_id: &'a str,
    pub manifest_digest: &'a str,
    pub auth_schema_id: &'a str,
    pub credential_reference: &'a str,
    pub reference_digest: &'a str,
    pub authority: &'a ManagedCredentialEnrollmentAuthoritySummary,
    pub authority_evidence_digest: &'a str,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedMcpStateUpdate {
    pub registration: super::RegistrationState,
    pub installation: super::InstallationState,
    pub runtime: super::RuntimeState,
    pub health: super::HealthState,
    pub session_enabled: BTreeMap<String, bool>,
    pub tool_policies: BTreeMap<String, ToolPolicy>,
}

impl ManagedMcpStateUpdate {
    pub fn apply_to(&self, default_enabled: bool) -> ManagedMcpState {
        ManagedMcpState {
            registration: self.registration,
            installation: self.installation,
            runtime: self.runtime,
            health: self.health,
            default_enabled,
            session_enabled: self.session_enabled.clone(),
            tool_policies: self.tool_policies.clone(),
        }
    }

    pub fn preserving_default_enabled(
        state: &ManagedMcpState,
        authorized_default_enabled: bool,
    ) -> McpPlatformResult<Self> {
        if state.default_enabled != authorized_default_enabled {
            return Err(super::error::McpPlatformError::new(
                McpPlatformErrorCode::PolicyDenied,
                "managed default_enabled requires projection authorization",
            ));
        }
        Ok(Self {
            registration: state.registration,
            installation: state.installation,
            runtime: state.runtime,
            health: state.health,
            session_enabled: state.session_enabled.clone(),
            tool_policies: state.tool_policies.clone(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ManagedInventoryFilter {
    pub registration: Option<super::RegistrationState>,
    pub installation: Option<super::InstallationState>,
    pub runtime: Option<super::RuntimeState>,
    pub health: Option<super::HealthState>,
    pub default_enabled: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct RegisterManagedMcp<'a> {
    pub managed_mcp_id: &'a str,
    pub mcp_id: &'a str,
    pub installation_scope: &'a str,
    pub distribution_adapter: &'a str,
    pub manifest_digest: &'a str,
    pub version: &'a str,
    pub task_id: &'a str,
    pub adapter_evidence: &'a AdapterEvidence,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterManagedMcpOutcome {
    pub record: ManagedMcpInventoryRecord,
    pub created: bool,
}

#[derive(Debug, Clone)]
pub struct StageManagedInstallation<'a> {
    pub managed_mcp_id: &'a str,
    pub mcp_id: &'a str,
    pub installation_scope: &'a str,
    pub distribution_adapter: &'a str,
    pub manifest_digest: &'a str,
    pub version: &'a str,
    pub installation_root: &'a str,
    pub task_id: &'a str,
    pub adapter_evidence: &'a AdapterEvidence,
    pub verification_evidence: &'a ArtifactVerificationEvidence,
    pub materialized_tree_digest: &'a str,
    pub supply_chain_evidence: Option<&'a SupplyChainEvidence>,
    pub owned_relative_paths: &'a [String],
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageManagedInstallationOutcome {
    pub record: ManagedMcpInventoryRecord,
    pub created_managed_mcp: bool,
    pub created_version: bool,
    pub previous_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActivateManagedInstallation<'a> {
    pub managed_mcp_id: &'a str,
    pub target_version: &'a str,
    pub task_id: &'a str,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedUninstallSnapshot {
    pub managed_mcp_id: String,
    pub version: String,
    pub artifact_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProfileManagedSnapshotSource {
    pub managed_mcp_id: String,
    pub state: ManagedMcpState,
    pub managed_revision: i64,
    pub manifest: ManifestRecord,
    pub manifest_digest: String,
    pub projection_revision: i64,
    pub projection_digest: String,
}

#[derive(Debug, Clone)]
pub struct PutOwnedProjection<'a> {
    pub plan_id: &'a str,
    pub owner_task_id: &'a str,
    pub worker_owner_id: &'a str,
    pub step_ordinal: i64,
    pub step_token: &'a str,
    pub now_ms: i64,
}

#[derive(Debug, Clone)]
pub struct RemoveOwnedProjection<'a> {
    pub managed_mcp_id: &'a str,
    pub task_id: &'a str,
    pub worker_owner_id: &'a str,
    pub compensation_ordinal: i64,
    pub now_ms: i64,
}

#[derive(Debug, Clone)]
pub struct RestoreOwnedProjection<'a> {
    pub managed_mcp_id: &'a str,
    pub link_key: &'a str,
    pub projection: &'a ConnectionProjection,
    pub plan_id: &'a str,
    pub manifest_digest: &'a str,
    pub owner_task_id: Option<&'a str>,
    pub projection_digest: &'a str,
    pub replacing_task_id: &'a str,
    pub worker_owner_id: &'a str,
    pub compensation_ordinal: i64,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanTarget {
    pub managed_mcp_id: Option<String>,
    pub mcp_id: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_context: Option<ManifestSourceContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfirmationEvidence {
    NotRequired,
    Pending,
    Confirmed { actor: String, confirmed_at_ms: i64 },
}

#[derive(Debug, Clone)]
pub struct SavePlan<'a> {
    pub plan_id: &'a str,
    pub idempotency_key: &'a str,
    pub plan: &'a InstallationPlan,
    pub target: &'a PlanTarget,
    pub policy_evidence: &'a PolicyDecision,
    pub confirmation_evidence: &'a ConfirmationEvidence,
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
    pub actor: &'a str,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRecord {
    pub plan_id: String,
    pub idempotency_key: String,
    pub envelope_digest: String,
    pub plan: InstallationPlan,
    pub target: PlanTarget,
    pub policy_evidence: PolicyDecision,
    pub confirmation_evidence: ConfirmationEvidence,
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
    pub actor: String,
}

#[derive(Debug, Clone)]
pub struct CreateTask<'a> {
    pub task_id: &'a str,
    pub plan_id: &'a str,
    pub plan_digest: &'a str,
    pub operation: TaskOperation,
    pub idempotency_key: &'a str,
    pub actor: &'a str,
    pub now_ms: i64,
    pub adapter_evidence: Option<&'a AdapterEvidence>,
    pub rollback_evidence: Option<&'a RollbackEvidence>,
}

#[derive(Debug, Clone)]
pub struct TaskTransition<'a> {
    pub task_id: &'a str,
    pub expected_revision: i64,
    pub next_status: TaskStatus,
    pub actor: &'a str,
    pub now_ms: i64,
    pub heartbeat_at_ms: Option<i64>,
    pub progress: u8,
    pub redacted_error: Option<&'a RedactedError>,
    pub rollback_status: RollbackStatus,
    pub rollback_evidence: Option<&'a RollbackEvidence>,
}

#[derive(Debug, Clone)]
pub struct StepTransition<'a> {
    pub task_id: &'a str,
    pub ordinal: i64,
    pub owner_id: &'a str,
    pub expected_task_revision: i64,
    pub expected_status: TaskStepStatus,
    pub next_status: TaskStepStatus,
    pub evidence: Option<&'a StepEvidence>,
    pub actor: &'a str,
    pub now_ms: i64,
}

#[derive(Debug, Clone)]
pub struct CompensationTransition<'a> {
    pub task_id: &'a str,
    pub ordinal: i64,
    pub owner_id: &'a str,
    pub expected_task_revision: i64,
    pub expected_status: CompensationStatus,
    pub next_status: CompensationStatus,
    pub actor: &'a str,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRecord {
    pub task_id: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub operation: TaskOperation,
    pub idempotency_key: String,
    pub status: TaskStatus,
    pub actor: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub heartbeat_at_ms: Option<i64>,
    pub progress: u8,
    pub step_cursor: i64,
    pub adapter_evidence: Option<AdapterEvidence>,
    pub redacted_error: Option<RedactedError>,
    pub rollback_status: RollbackStatus,
    pub rollback_evidence: Option<RollbackEvidence>,
    pub revision: i64,
    pub event_sequence: i64,
    pub owner_id: Option<String>,
    pub lease_expires_at_ms: Option<i64>,
    pub attempt_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStepRecord {
    pub task_id: String,
    pub ordinal: i64,
    pub status: TaskStepStatus,
    pub idempotency_token: String,
    pub compensation: CompensationDescriptor,
    pub evidence: Option<StepEvidence>,
    pub started_at_ms: Option<i64>,
    pub committed_at_ms: Option<i64>,
    pub adapter_id: String,
    pub adapter_version: String,
    pub compensation_status: CompensationStatus,
    pub compensation_started_at_ms: Option<i64>,
    pub compensation_committed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryAttemptRecord {
    pub task_id: String,
    pub attempt: i64,
    pub idempotency_key: String,
    pub requested_from_status: TaskStatus,
    pub actor: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthTaskRequestRecord {
    pub task_id: String,
    pub managed_mcp_id: String,
    pub mode: HealthCheckMode,
}

#[derive(Debug, Clone)]
pub struct CreateHealthTask<'a> {
    pub task_id: &'a str,
    pub managed_mcp_id: &'a str,
    pub mode: HealthCheckMode,
    pub idempotency_key: &'a str,
    pub actor: &'a str,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthObservationRecord {
    pub observation_id: i64,
    pub managed_mcp_id: String,
    pub task_id: String,
    pub check_type: String,
    pub result_code: HealthResultCode,
    pub latency_ms: i64,
    pub capabilities_digest: Option<String>,
    pub tools_digest: Option<String>,
    pub checked_at_ms: i64,
    pub detail_code: HealthDetailCode,
}

#[derive(Debug, Clone)]
pub struct NewHealthObservation<'a> {
    pub managed_mcp_id: &'a str,
    pub task_id: &'a str,
    pub check_type: &'a str,
    pub result_code: HealthResultCode,
    pub latency_ms: i64,
    pub capabilities_digest: Option<&'a str>,
    pub tools_digest: Option<&'a str>,
    pub checked_at_ms: i64,
    pub detail_code: HealthDetailCode,
}

pub(crate) const PROJECTION_WITNESS_V2_VERSION: i64 = 2;
pub(crate) const PROJECTION_WITNESS_V2_DOMAIN: &str = "goose.mcp-platform.projection-witness";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionRepositoryIdentity {
    pub provider_id: String,
    pub instance_id: String,
    pub path_binding: String,
    pub key_epoch: u64,
}

impl ProjectionRepositoryIdentity {
    pub(crate) fn projection_anchor(&self, root: String) -> ProjectionAuthorityAnchor {
        ProjectionAuthorityAnchor {
            instance_id: self.instance_id.clone(),
            path_binding: self.path_binding.clone(),
            key_epoch: self.key_epoch,
            sequence: 1,
            root,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionWitnessV2 {
    pub domain: String,
    pub version: i64,
    pub repository: ProjectionRepositoryIdentity,
    pub sink_identity: String,
    pub runtime_id: String,
    pub projection_digest: String,
    pub mutation_id: i64,
    pub managed_mcp_id: String,
    pub expected_revision: i64,
    pub previous_enabled: bool,
    pub desired_enabled: bool,
    pub observed_state_digest: String,
}

impl ProjectionWitnessV2 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        repository: ProjectionRepositoryIdentity,
        sink_identity: String,
        runtime_id: String,
        projection_digest: String,
        mutation_id: i64,
        managed_mcp_id: String,
        expected_revision: i64,
        previous_enabled: bool,
        desired_enabled: bool,
        observed_state_digest: String,
    ) -> Self {
        Self {
            domain: PROJECTION_WITNESS_V2_DOMAIN.to_string(),
            version: PROJECTION_WITNESS_V2_VERSION,
            repository,
            sink_identity,
            runtime_id,
            projection_digest,
            mutation_id,
            managed_mcp_id,
            expected_revision,
            previous_enabled,
            desired_enabled,
            observed_state_digest,
        }
    }

    pub(crate) fn projection_anchor(&self) -> ProjectionAuthorityAnchor {
        self.repository
            .projection_anchor(self.projection_digest.clone())
    }

    pub(crate) fn observed_anchor(&self) -> ProjectionAuthorityAnchor {
        self.repository
            .projection_anchor(self.observed_state_digest.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionMutationRecord {
    pub mutation_id: i64,
    pub managed_mcp_id: String,
    pub expected_revision: i64,
    pub previous_enabled: bool,
    pub desired_enabled: bool,
    pub status: ProjectionMutationStatus,
    pub writer_runtime_id: Option<String>,
    pub writer_sink_id: Option<String>,
    pub writer_anchor: Option<ProjectionAuthorityAnchor>,
    pub writer_witness_version: Option<i64>,
    pub writer_provider_id: Option<String>,
    pub writer_binding_domain: Option<String>,
    pub writer_observed_state_digest: Option<String>,
    pub writer_commitment: Option<String>,
}

impl ProjectionMutationRecord {
    pub(crate) fn witness_v2(&self) -> Option<ProjectionWitnessV2> {
        let version = self.writer_witness_version?;
        if version != PROJECTION_WITNESS_V2_VERSION
            || self.writer_binding_domain.as_deref()? != PROJECTION_WITNESS_V2_DOMAIN
        {
            return None;
        }
        let anchor = self.writer_anchor.as_ref()?;
        if anchor.sequence != 1 {
            return None;
        }
        Some(ProjectionWitnessV2 {
            domain: self.writer_binding_domain.clone()?,
            version,
            repository: ProjectionRepositoryIdentity {
                provider_id: self.writer_provider_id.clone()?,
                instance_id: anchor.instance_id.clone(),
                path_binding: anchor.path_binding.clone(),
                key_epoch: anchor.key_epoch,
            },
            sink_identity: self.writer_sink_id.clone()?,
            runtime_id: self.writer_runtime_id.clone()?,
            projection_digest: anchor.root.clone(),
            mutation_id: self.mutation_id,
            managed_mcp_id: self.managed_mcp_id.clone(),
            expected_revision: self.expected_revision,
            previous_enabled: self.previous_enabled,
            desired_enabled: self.desired_enabled,
            observed_state_digest: self.writer_observed_state_digest.clone()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectionEffectGrantStatus {
    Issued,
    Consuming,
    Applied,
    Unknown,
    Abandoned,
}

impl ProjectionEffectGrantStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Issued => "issued",
            Self::Consuming => "consuming",
            Self::Applied => "applied",
            Self::Unknown => "unknown",
            Self::Abandoned => "abandoned",
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct IssueProjectionEffectGrant<'a> {
    pub(crate) effect_id: &'a str,
    pub(crate) target_digest: &'a str,
    pub(crate) now_ms: i64,
}

pub(crate) struct IssuedProjectionEffectGrant {
    grant_id: String,
    effect_id: String,
    fence_epoch: i64,
    target_digest: String,
    canonical_digest: String,
    pub(crate) sink_identity: String,
    pub(crate) sink_version: String,
    pub(crate) sink_authority: String,
    pub(crate) authority_key_epoch: i64,
    pub(crate) authority_key_fingerprint: String,
    pub(crate) command_binding: String,
    pub(crate) repository_instance_id: String,
    pub(crate) repository_path_binding: String,
    pub(crate) repository_key_epoch: i64,
    nonce: [u8; 32],
}

impl IssuedProjectionEffectGrant {
    pub(crate) fn grant_id(&self) -> &str {
        &self.grant_id
    }

    pub(crate) fn effect_id(&self) -> &str {
        &self.effect_id
    }

    pub(crate) const fn fence_epoch(&self) -> i64 {
        self.fence_epoch
    }

    pub(crate) fn target_digest(&self) -> &str {
        &self.target_digest
    }

    pub(crate) fn canonical_digest(&self) -> &str {
        &self.canonical_digest
    }
}

/// Persisted, non-secret grant material that may be used only to reconcile a
/// previously consumed fenced effect. It deliberately excludes the grant nonce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConsumingProjectionEffectGrant {
    pub(crate) grant_id: String,
    pub(crate) effect_id: String,
    pub(crate) fence_scope: String,
    pub(crate) fence_epoch: i64,
    pub(crate) target_digest: String,
    pub(crate) canonical_digest: String,
    pub(crate) sink_identity: String,
    pub(crate) sink_version: String,
    pub(crate) sink_authority: String,
    pub(crate) authority_key_epoch: i64,
    pub(crate) authority_key_fingerprint: String,
    pub(crate) command_binding: String,
    pub(crate) repository_instance_id: String,
    pub(crate) repository_path_binding: String,
    pub(crate) repository_key_epoch: i64,
}

#[derive(Clone)]
pub(crate) struct FinishProjectionEffectGrant<'a> {
    pub(crate) grant_id: &'a str,
    pub(crate) now_ms: i64,
}

#[derive(Clone)]
pub(crate) struct AbandonProjectionEffectGrant<'a> {
    pub(crate) grant_id: &'a str,
    pub(crate) reason: &'a str,
    pub(crate) now_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionAuthorization {
    checkpoint: ProjectionAuthorityAnchor,
    mutation: ProjectionMutationRecord,
    inventory: ManagedMcpInventoryRecord,
    projection: Option<ConnectionProjectionRecord>,
    plan: Option<PlanRecord>,
    manifest: Option<ManifestRecord>,
    provider_assurance: ProviderAssurance,
}

impl ProjectionAuthorization {
    fn new(
        checkpoint: ProjectionAuthorityAnchor,
        mutation: ProjectionMutationRecord,
        inventory: ManagedMcpInventoryRecord,
        projection: Option<ConnectionProjectionRecord>,
        plan: Option<PlanRecord>,
        manifest: Option<ManifestRecord>,
    ) -> Self {
        Self {
            checkpoint,
            mutation,
            inventory,
            projection,
            plan,
            manifest,
            provider_assurance: ProviderAssurance::DbExternalTamperEvidence,
        }
    }

    pub(crate) fn checkpoint(&self) -> &ProjectionAuthorityAnchor {
        &self.checkpoint
    }

    pub(crate) fn mutation(&self) -> &ProjectionMutationRecord {
        &self.mutation
    }

    pub(crate) fn inventory(&self) -> &ManagedMcpInventoryRecord {
        &self.inventory
    }

    pub(crate) fn projection(&self) -> Option<&ConnectionProjectionRecord> {
        self.projection.as_ref()
    }

    pub(crate) fn plan(&self) -> Option<&PlanRecord> {
        self.plan.as_ref()
    }

    pub(crate) fn manifest(&self) -> Option<&ManifestRecord> {
        self.manifest.as_ref()
    }

    pub(crate) fn witness_v2(&self) -> McpPlatformResult<ProjectionWitnessV2> {
        self.mutation.witness_v2().ok_or_else(|| {
            super::error::McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored projection witness is not an explicit v2 binding",
            )
        })
    }

    pub(crate) const fn provider_assurance(&self) -> ProviderAssurance {
        self.provider_assurance
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectionSinkAtomicProofKind {
    NoopCompareAndSwap,
    CompareAndSwapWrite,
}

impl ProjectionSinkAtomicProofKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoopCompareAndSwap => "noop_compare_and_swap",
            Self::CompareAndSwapWrite => "compare_and_swap_write",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionSinkCommitReceipt {
    witness: ProjectionWitnessV2,
    checkpoint: ProjectionAuthorityAnchor,
    proof: ProjectionSinkAtomicProof,
    repository_binding: String,
}

impl ProjectionSinkCommitReceipt {
    pub(crate) fn new(
        witness: ProjectionWitnessV2,
        checkpoint: ProjectionAuthorityAnchor,
        proof: ProjectionSinkAtomicProof,
        repository_binding: String,
    ) -> McpPlatformResult<Self> {
        if proof.runtime_id() != witness.runtime_id
            || proof.observed_state_digest() != witness.observed_state_digest
            || proof.target_state_digest() != witness.observed_state_digest
            || checkpoint.instance_id != witness.repository.instance_id
            || checkpoint.path_binding != witness.repository.path_binding
            || checkpoint.key_epoch != witness.repository.key_epoch
            || repository_binding.len() != 64
        {
            return Err(super::error::McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "projection sink commit receipt does not bind the authorized witness",
            ));
        }
        Ok(Self {
            witness,
            checkpoint,
            proof,
            repository_binding,
        })
    }

    pub(crate) fn runtime_id(&self) -> &str {
        &self.witness.runtime_id
    }

    pub(crate) fn sink_instance_id(&self) -> &str {
        &self.witness.sink_identity
    }

    pub(crate) fn checkpoint(&self) -> &ProjectionAuthorityAnchor {
        &self.checkpoint
    }

    pub(crate) fn proof(&self) -> &ProjectionSinkAtomicProof {
        &self.proof
    }

    pub(crate) const fn mutation_id(&self) -> i64 {
        self.witness.mutation_id
    }

    pub(crate) const fn expected_revision(&self) -> i64 {
        self.witness.expected_revision
    }

    pub(crate) const fn desired_enabled(&self) -> bool {
        self.witness.desired_enabled
    }

    pub(crate) fn commitment(&self) -> &str {
        &self.witness.projection_digest
    }

    pub(crate) fn witness(&self) -> &ProjectionWitnessV2 {
        &self.witness
    }

    pub(crate) fn repository_binding(&self) -> &str {
        &self.repository_binding
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionRecoveryConfirmationReceipt {
    witness: ProjectionWitnessV2,
    checkpoint: ProjectionAuthorityAnchor,
    proof: ProjectionSinkAtomicProof,
    repository_binding: String,
}

impl ProjectionRecoveryConfirmationReceipt {
    pub(crate) fn new(
        witness: ProjectionWitnessV2,
        checkpoint: ProjectionAuthorityAnchor,
        proof: ProjectionSinkAtomicProof,
        repository_binding: String,
    ) -> McpPlatformResult<Self> {
        if proof.kind() != ProjectionSinkAtomicProofKind::NoopCompareAndSwap
            || proof.runtime_id() != witness.runtime_id
            || proof.observed_state_digest() != proof.target_state_digest()
            || checkpoint.instance_id != witness.repository.instance_id
            || checkpoint.path_binding != witness.repository.path_binding
            || checkpoint.key_epoch != witness.repository.key_epoch
            || repository_binding.len() != 64
        {
            return Err(super::error::McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "projection recovery confirmation receipt does not bind the authorized witness",
            ));
        }
        Ok(Self {
            witness,
            checkpoint,
            proof,
            repository_binding,
        })
    }

    pub(crate) fn checkpoint(&self) -> &ProjectionAuthorityAnchor {
        &self.checkpoint
    }

    pub(crate) fn proof(&self) -> &ProjectionSinkAtomicProof {
        &self.proof
    }

    pub(crate) fn witness(&self) -> &ProjectionWitnessV2 {
        &self.witness
    }

    pub(crate) fn repository_binding(&self) -> &str {
        &self.repository_binding
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionAuthorityAnchor {
    pub instance_id: String,
    pub path_binding: String,
    pub key_epoch: u64,
    pub sequence: u64,
    pub root: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionMutationStatus {
    Started,
    ConfigCommitted,
    Committed,
    RecoveryRequired,
    Resolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    TaskCreated,
    TaskStatusChanged,
    StepStarted,
    StepCommitted,
    RecoveryInterrupted,
    ConfirmationRecorded,
    CancellationRequested,
    RetryQueued,
}

impl AuditEventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskCreated => "task_created",
            Self::TaskStatusChanged => "task_status_changed",
            Self::StepStarted => "step_started",
            Self::StepCommitted => "step_committed",
            Self::RecoveryInterrupted => "recovery_interrupted",
            Self::ConfirmationRecorded => "confirmation_recorded",
            Self::CancellationRequested => "cancellation_requested",
            Self::RetryQueued => "retry_queued",
        }
    }
}

pub(crate) const PROJECTION_SINK_COMMIT_RECEIPT_DOMAIN: &str = "projection-sink-commit-receipt-v4";
pub(crate) const PROJECTION_SINK_RECOVERY_RECEIPT_DOMAIN: &str =
    "projection-sink-recovery-confirmation-receipt-v2";

pub(crate) fn projection_sink_receipt_binding_payload(
    domain: &str,
    witness: &ProjectionWitnessV2,
    checkpoint: &ProjectionAuthorityAnchor,
    proof: &ProjectionSinkAtomicProof,
) -> Vec<u8> {
    canonical_fields(&[
        domain.as_bytes(),
        witness.domain.as_bytes(),
        witness.version.to_string().as_bytes(),
        witness.repository.provider_id.as_bytes(),
        witness.repository.instance_id.as_bytes(),
        witness.repository.path_binding.as_bytes(),
        witness.repository.key_epoch.to_string().as_bytes(),
        witness.sink_identity.as_bytes(),
        witness.runtime_id.as_bytes(),
        witness.projection_digest.as_bytes(),
        witness.mutation_id.to_string().as_bytes(),
        witness.managed_mcp_id.as_bytes(),
        witness.expected_revision.to_string().as_bytes(),
        if witness.previous_enabled { b"1" } else { b"0" },
        if witness.desired_enabled { b"1" } else { b"0" },
        witness.observed_state_digest.as_bytes(),
        checkpoint.instance_id.as_bytes(),
        checkpoint.path_binding.as_bytes(),
        checkpoint.key_epoch.to_string().as_bytes(),
        checkpoint.sequence.to_string().as_bytes(),
        checkpoint.root.as_bytes(),
        proof.adapter_id().as_bytes(),
        proof.adapter_version().as_bytes(),
        proof.runtime_id().as_bytes(),
        proof.observed_state_digest().as_bytes(),
        proof.target_state_digest().as_bytes(),
        proof.kind().as_str().as_bytes(),
        proof.attestation().as_bytes(),
    ])
}

fn canonical_fields(fields: &[&[u8]]) -> Vec<u8> {
    let mut payload = Vec::new();
    for field in fields {
        payload.extend_from_slice(&(field.len() as u64).to_be_bytes());
        payload.extend_from_slice(field);
    }
    payload
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuditPayload {
    TaskCreated {
        status: TaskStatus,
    },
    TaskStatusChanged {
        from: TaskStatus,
        to: TaskStatus,
    },
    ConfirmationRecorded {
        from: TaskStatus,
        to: TaskStatus,
        plan_id: String,
        plan_digest: String,
    },
    CancellationRequested {
        from: TaskStatus,
        to: TaskStatus,
    },
    StepStatusChanged {
        ordinal: i64,
        from: TaskStepStatus,
        to: TaskStepStatus,
    },
    RecoveryDecision {
        decision: RecoveryDecision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEventRecord {
    pub event_id: i64,
    pub task_id: String,
    pub sequence: i64,
    pub event_type: AuditEventType,
    pub actor: String,
    pub occurred_at_ms: i64,
    pub payload: AuditPayload,
    pub redacted_error: Option<RedactedError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalAuditPage {
    pub events: Vec<AuditEventRecord>,
    pub scanned_through_event_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRecord {
    pub task: TaskRecord,
    pub decision: RecoveryDecision,
}
