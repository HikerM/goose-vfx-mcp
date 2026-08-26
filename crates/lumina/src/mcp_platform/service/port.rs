use async_trait::async_trait;

use crate::mcp_platform::error::McpPlatformResult;

#[async_trait]
pub trait HttpsManifestSourceFetcher: Send + Sync {
    async fn fetch(&self, requested_url: &str) -> McpPlatformResult<HttpsManifestFetchResult>;
}

pub use crate::mcp_platform::https_manifest_fetcher::HttpsManifestFetchResult;

#[derive(Debug, Default)]
pub struct UnavailableHttpsManifestSourceFetcher;

#[async_trait]
impl HttpsManifestSourceFetcher for UnavailableHttpsManifestSourceFetcher {
    async fn fetch(&self, _requested_url: &str) -> McpPlatformResult<HttpsManifestFetchResult> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "HTTPS manifest fetching is not configured",
        ))
    }
}
use crate::mcp_platform::intake::{
    AuthorityCommitOutcome, AuthorityVerifiedOpaqueTuple, IntakeCandidateRecord,
    IntakeLifecycleState, IntakePrivateReferenceKind, IntakeSourceFacet, IntakeTransport,
    RecordIntakeApprovalGrant, RecordIntakeBindingReady, RecordIntakeConsentGrant,
    SaveIntakeCandidate, SaveIntakeConfigurationRef, VerifiedNoLiveProof,
};
use crate::mcp_platform::intake_inspection::{
    InspectionConsentRecord, RecordIntakeInspectionSnapshot, SaveIntakeInspectionConsent,
    StoredInspectionSnapshot,
};
use crate::mcp_platform::intake_remote_inspection::{
    RemoteInspectionAttemptRecord, RemoteInspectionConsumptionRecord, RemoteInspectionPurpose,
    RemoteInspectionReservationRecord, RemoteInspectionSafeSubcode, RemoteInspectionSafeSummary,
};
use crate::mcp_platform::repository::{
    ActivateManagedInstallation, ApplyGovernedSourceRefresh, AuditEventRecord,
    CompensationTransition, ConfirmSourceProvisioning, ConnectionProjectionRecord,
    CreateHealthTask, CreateTask, ExecutionAuthorization, GlobalAuditPage,
    GovernedCatalogEntryRecord, GovernedCatalogSourceRecord, GovernedSourceRefreshAuditRecord,
    GovernedSourceRefreshCommitOutcome, GovernedSourceRefreshRegistrationRecord,
    GovernedSourceRefreshTrustAnchorRecord, GovernedSourceTrustPinRecord, HealthObservationRecord,
    HealthTaskRequestRecord, HttpsManifestProvisionRecord, ManagedCredentialEnrollmentRecord,
    ManagedMcpInventoryRecord, ManagedMcpStateUpdate, ManagedUninstallSnapshot, ManifestRecord,
    ManifestSourceContext, NewHealthObservation, PlanRecord, ProfileManagedSnapshotSource,
    PutOwnedProjection, QueuedTaskCandidate, QueuedTaskPreflight, RecordGovernedSourceRefreshAudit,
    RecoveryEligibility, RegisterManagedMcp, RegisterManagedMcpOutcome, RemoveOwnedProjection,
    RestoreOwnedProjection, RetryAttemptRecord, SaveGovernedCatalogImport,
    SaveGovernedSourceRefreshRegistration, SaveManagedCredentialEnrollment, SavePlan,
    SqliteMcpPlatformRepository, StageManagedInstallation, StageManagedInstallationOutcome,
    StepTransition, TaskRecord, TaskStepRecord, TaskTransition,
};
use crate::mcp_platform::task::CompensationDescriptor;
use crate::mcp_platform::task::TaskOperation;
use crate::mcp_platform::{
    ChangeProfile, ConsumedProfileApplication, McpProfile, ProfileApplicationMarker,
    ProfileRevision, SaveProfile, SaveProfileApplicationToken, SaveProfileApplyConfirmation,
    SaveProfileApplyPlan, StoredProfileApplyPlan,
};

pub(crate) struct ReserveOrLoadRemoteInspection<'a> {
    pub(crate) reservation_id: &'a str,
    pub(crate) candidate_id: &'a str,
    pub(crate) candidate_revision: i64,
    pub(crate) candidate_lifecycle_state: IntakeLifecycleState,
    pub(crate) source_facet: IntakeSourceFacet,
    pub(crate) transport: IntakeTransport,
    pub(crate) purpose: RemoteInspectionPurpose,
    pub(crate) policy_revision: i64,
    pub(crate) intent_fingerprint: &'a str,
    pub(crate) intent_fingerprint_key_id: &'a str,
    pub(crate) now_ms: i64,
}

pub(crate) struct TombstoneRemoteInspectionPrebind<'a> {
    pub(crate) reservation_id: &'a str,
    pub(crate) generation: i64,
    pub(crate) proof: &'a VerifiedNoLiveProof,
    pub(crate) now_ms: i64,
}

pub(crate) struct BindRemoteInspectionPendingCommit<'a> {
    pub(crate) reservation_id: &'a str,
    pub(crate) generation: i64,
    pub(crate) consent_id: &'a str,
    pub(crate) candidate_lifecycle_state: IntakeLifecycleState,
    pub(crate) expires_at_ms: i64,
    pub(crate) verified_tuple: &'a AuthorityVerifiedOpaqueTuple,
    pub(crate) now_ms: i64,
}

pub(crate) struct RecordRemoteInspectionCommitOutcome<'a> {
    pub(crate) reservation_id: &'a str,
    pub(crate) generation: i64,
    pub(crate) consent_id: &'a str,
    pub(crate) outcome: &'a AuthorityCommitOutcome,
    pub(crate) now_ms: i64,
}

pub(crate) struct ClaimCommittedRemoteInspectionAttempt<'a> {
    pub(crate) reservation_id: &'a str,
    pub(crate) generation: i64,
    pub(crate) consent_id: &'a str,
    pub(crate) attempt_id: &'a str,
    pub(crate) claim_nonce: &'a str,
    pub(crate) claimed_at_ms: i64,
    pub(crate) claim_expires_at_ms: i64,
}

pub(crate) struct FinalizeRemoteInspectionConsumption<'a> {
    pub(crate) reservation_id: &'a str,
    pub(crate) generation: i64,
    pub(crate) consent_id: &'a str,
    pub(crate) attempt_id: &'a str,
    pub(crate) snapshot_id: &'a str,
    pub(crate) claim_nonce: &'a str,
    pub(crate) safe_subcode: RemoteInspectionSafeSubcode,
    pub(crate) safe_summary: RemoteInspectionSafeSummary,
    pub(crate) finalized_at_ms: i64,
}

#[async_trait]
pub(crate) trait McpPlatformRepositoryPort: Send + Sync {
    async fn save_https_manifest_provision(
        &self,
        _record: crate::mcp_platform::repository::HttpsManifestProvisionRecord,
        _token_hash: &str,
    ) -> McpPlatformResult<()> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "HTTPS manifest provisioning persistence is not configured",
        ))
    }

    async fn claim_https_manifest_provision(
        &self,
        _provision_id: &str,
        _token_hash: &str,
        _actor: &str,
        _binding: &str,
        _now_ms: i64,
    ) -> McpPlatformResult<crate::mcp_platform::repository::HttpsManifestProvisionRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "HTTPS manifest provisioning persistence is not configured",
        ))
    }
    async fn finalize_https_manifest_provision(
        &self,
        _provision_id: &str,
        _token_hash: &str,
        _actor: &str,
        _binding: &str,
        _now_ms: i64,
        _manifest: &ManifestRecord,
    ) -> McpPlatformResult<crate::mcp_platform::repository::InsertOutcome> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "HTTPS manifest provisioning persistence is not configured",
        ))
    }
    async fn consume_https_manifest_provision(
        &self,
        _provision_id: &str,
        _token_hash: &str,
        _actor: &str,
        _binding: &str,
        _now_ms: i64,
    ) -> McpPlatformResult<()> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "HTTPS manifest provisioning persistence is not configured",
        ))
    }
    async fn reject_https_manifest_provision(
        &self,
        _provision_id: &str,
        _token_hash: &str,
        _actor: &str,
        _binding: &str,
        _now_ms: i64,
    ) -> McpPlatformResult<()> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "HTTPS manifest provisioning persistence is not configured",
        ))
    }
    async fn verify_integrity(&self) -> McpPlatformResult<()>;

    async fn recovery_eligibility(&self) -> McpPlatformResult<RecoveryEligibility> {
        Ok(RecoveryEligibility::Blocked)
    }

    fn mint_intake_private_reference(
        &self,
        _kind: IntakePrivateReferenceKind,
    ) -> McpPlatformResult<String> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake private reference minting",
        ))
    }

    async fn get_profile_managed_snapshot_source(
        &self,
        _managed_mcp_id: &str,
    ) -> McpPlatformResult<ProfileManagedSnapshotSource> {
        Err(profile_repository_unsupported())
    }
    async fn create_profile(&self, _input: SaveProfile<'_>) -> McpPlatformResult<McpProfile> {
        Err(profile_repository_unsupported())
    }
    async fn change_profile(&self, _input: ChangeProfile<'_>) -> McpPlatformResult<McpProfile> {
        Err(profile_repository_unsupported())
    }
    async fn list_profiles(&self, _include_archived: bool) -> McpPlatformResult<Vec<McpProfile>> {
        Err(profile_repository_unsupported())
    }
    async fn get_profile(&self, _profile_id: &str) -> McpPlatformResult<McpProfile> {
        Err(profile_repository_unsupported())
    }
    async fn get_profile_revision(
        &self,
        _profile_id: &str,
        _revision: i64,
    ) -> McpPlatformResult<ProfileRevision> {
        Err(profile_repository_unsupported())
    }
    async fn list_profile_revisions(
        &self,
        _profile_id: &str,
    ) -> McpPlatformResult<Vec<ProfileRevision>> {
        Err(profile_repository_unsupported())
    }
    async fn save_profile_apply_plan(
        &self,
        _input: SaveProfileApplyPlan<'_>,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        Err(profile_repository_unsupported())
    }
    async fn get_profile_apply_plan(
        &self,
        _plan_id: &str,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        Err(profile_repository_unsupported())
    }
    async fn create_profile_apply_confirmation(
        &self,
        _input: SaveProfileApplyConfirmation<'_>,
    ) -> McpPlatformResult<()> {
        Err(profile_repository_unsupported())
    }
    async fn get_profile_apply_confirmation_plan(
        &self,
        _confirmation_hash: &str,
        _plan_id: &str,
        _actor: &str,
        _now_ms: i64,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        Err(profile_repository_unsupported())
    }
    async fn get_profile_application_plan_by_token(
        &self,
        _token_hash: &str,
        _actor: &str,
        _now_ms: i64,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        Err(profile_repository_unsupported())
    }
    async fn get_profile_application_plan_by_application(
        &self,
        _application_id: &str,
        _actor: &str,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        Err(profile_repository_unsupported())
    }
    async fn get_profile_application_cleanup_state(
        &self,
        _session_id: &str,
        _actor: &str,
    ) -> McpPlatformResult<Option<(String, String)>> {
        Err(profile_repository_unsupported())
    }
    async fn create_profile_application_token(
        &self,
        _input: SaveProfileApplicationToken<'_>,
    ) -> McpPlatformResult<()> {
        Err(profile_repository_unsupported())
    }
    async fn consume_profile_application_token(
        &self,
        _token_hash: &str,
        _actor: &str,
        _now_ms: i64,
    ) -> McpPlatformResult<ConsumedProfileApplication> {
        Err(profile_repository_unsupported())
    }
    async fn hydrate_profile_application(
        &self,
        _marker: &ProfileApplicationMarker,
        _session_id: &str,
        _actor: &str,
    ) -> McpPlatformResult<ConsumedProfileApplication> {
        Err(profile_repository_unsupported())
    }
    async fn finish_profile_application(
        &self,
        _application_id: &str,
        _session_id: Option<&str>,
        _status: &str,
        _detail_code: &str,
        _actor: &str,
        _now_ms: i64,
    ) -> McpPlatformResult<()> {
        Err(profile_repository_unsupported())
    }
    async fn save_manifest(
        &self,
        record: &crate::mcp_platform::repository::ManifestRecord,
    ) -> McpPlatformResult<crate::mcp_platform::repository::InsertOutcome> {
        let _ = record;
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support verified manifest persistence",
        ))
    }
    async fn list_manifests(&self) -> McpPlatformResult<Vec<ManifestRecord>>;
    async fn list_manifests_with_source_context(
        &self,
        source_context: &ManifestSourceContext,
    ) -> McpPlatformResult<Vec<ManifestRecord>> {
        let _ = source_context;
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support contextual manifest listings",
        ))
    }
    async fn save_governed_catalog_import(
        &self,
        _input: SaveGovernedCatalogImport,
    ) -> McpPlatformResult<()> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed catalog imports",
        ))
    }
    async fn confirm_source_provisioning(
        &self,
        _input: ConfirmSourceProvisioning,
    ) -> McpPlatformResult<crate::mcp_platform::repository::SourceProvisioningAuditRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support trusted source provisioning",
        ))
    }
    async fn list_governed_catalog_entries(
        &self,
    ) -> McpPlatformResult<Vec<GovernedCatalogEntryRecord>> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed catalog listings",
        ))
    }
    async fn get_governed_catalog_entry(
        &self,
        _source_id: &str,
        _mcp_id: &str,
        _version: &str,
    ) -> McpPlatformResult<GovernedCatalogEntryRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed catalog detail lookups",
        ))
    }
    async fn get_governed_catalog_entry_by_digest(
        &self,
        _manifest_digest: &str,
    ) -> McpPlatformResult<GovernedCatalogEntryRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed catalog digest lookups",
        ))
    }
    async fn list_governed_catalog_entries_by_digest(
        &self,
        _manifest_digest: &str,
    ) -> McpPlatformResult<Vec<GovernedCatalogEntryRecord>> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed catalog digest listings",
        ))
    }
    async fn source_catalog_document_provenance(
        &self,
        _source_id: &str,
    ) -> McpPlatformResult<
        crate::mcp_platform::repository::sqlite::StoredSourceCatalogDocumentProvenance,
    > {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support verified source catalog provenance lookups",
        ))
    }
    async fn source_catalog_release_matches(
        &self,
        _source_id: &str,
        _release_id: &str,
        _mcp_id: &str,
        _version: &str,
    ) -> McpPlatformResult<bool> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support verified source catalog release lookups",
        ))
    }
    async fn list_governed_catalog_sources(
        &self,
    ) -> McpPlatformResult<Vec<GovernedCatalogSourceRecord>> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed catalog source listings",
        ))
    }
    async fn get_governed_catalog_source(
        &self,
        _source_id: &str,
    ) -> McpPlatformResult<GovernedCatalogSourceRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed catalog source lookups",
        ))
    }
    async fn save_governed_source_refresh_registration(
        &self,
        _input: SaveGovernedSourceRefreshRegistration<'_>,
    ) -> McpPlatformResult<GovernedSourceRefreshRegistrationRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed source refresh registrations",
        ))
    }
    async fn get_governed_source_refresh_registration(
        &self,
        _source_id: &str,
    ) -> McpPlatformResult<GovernedSourceRefreshRegistrationRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed source refresh registration lookups",
        ))
    }
    async fn get_governed_source_refresh_trust_anchor(
        &self,
        _source_id: &str,
    ) -> McpPlatformResult<GovernedSourceRefreshTrustAnchorRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed source refresh trust anchors",
        ))
    }
    async fn has_governed_source_refresh_trust_anchor(
        &self,
        _source_id: &str,
    ) -> McpPlatformResult<bool> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed source refresh trust anchor checks",
        ))
    }
    async fn get_governed_source_trust_pin(
        &self,
        _source_id: &str,
    ) -> McpPlatformResult<GovernedSourceTrustPinRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed source trust pins",
        ))
    }
    async fn apply_governed_source_refresh(
        &self,
        _input: ApplyGovernedSourceRefresh,
    ) -> McpPlatformResult<GovernedSourceRefreshCommitOutcome> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed source refresh commits",
        ))
    }
    async fn list_governed_source_refresh_registrations(
        &self,
    ) -> McpPlatformResult<Vec<GovernedSourceRefreshRegistrationRecord>> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed source refresh registration listings",
        ))
    }
    async fn record_governed_source_refresh_audit(
        &self,
        _input: RecordGovernedSourceRefreshAudit<'_>,
    ) -> McpPlatformResult<GovernedSourceRefreshAuditRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed source refresh audits",
        ))
    }
    async fn list_governed_source_refresh_audits(
        &self,
        _source_id: &str,
    ) -> McpPlatformResult<Vec<GovernedSourceRefreshAuditRecord>> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support governed source refresh audit listings",
        ))
    }
    async fn get_manifest(&self, digest: &str) -> McpPlatformResult<ManifestRecord>;
    async fn get_manifest_with_source_context(
        &self,
        digest: &str,
        source_context: &ManifestSourceContext,
    ) -> McpPlatformResult<ManifestRecord> {
        let _ = (digest, source_context);
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support contextual manifest lookups",
        ))
    }
    async fn get_manifest_by_identity(
        &self,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<ManifestRecord>;
    async fn get_manifest_by_identity_with_source_context(
        &self,
        mcp_id: &str,
        version: &str,
        source_context: &ManifestSourceContext,
    ) -> McpPlatformResult<ManifestRecord> {
        let _ = (mcp_id, version, source_context);
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support contextual manifest identity lookups",
        ))
    }
    async fn get_managed_credential_enrollment(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<ManagedCredentialEnrollmentRecord>>;
    async fn save_managed_credential_enrollment(
        &self,
        input: SaveManagedCredentialEnrollment<'_>,
    ) -> McpPlatformResult<ManagedCredentialEnrollmentRecord>;
    async fn save_plan(&self, input: SavePlan<'_>) -> McpPlatformResult<PlanRecord>;
    async fn get_plan(&self, plan_id: &str) -> McpPlatformResult<PlanRecord>;
    async fn get_plan_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<PlanRecord>>;
    async fn save_intake_configuration_ref(
        &self,
        _input: SaveIntakeConfigurationRef<'_>,
    ) -> McpPlatformResult<String> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake configuration persistence",
        ))
    }
    async fn save_intake_candidate(
        &self,
        _input: SaveIntakeCandidate<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake candidate persistence",
        ))
    }
    async fn get_intake_candidate(
        &self,
        _candidate_id: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake candidate persistence",
        ))
    }
    async fn get_intake_candidate_by_submission_binding(
        &self,
        _submission_binding: &str,
    ) -> McpPlatformResult<Option<IntakeCandidateRecord>> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake candidate persistence",
        ))
    }
    async fn record_intake_consent_grant(
        &self,
        _input: RecordIntakeConsentGrant<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake candidate persistence",
        ))
    }
    async fn record_intake_approval_grant(
        &self,
        _input: RecordIntakeApprovalGrant<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake candidate persistence",
        ))
    }
    async fn record_intake_binding_ready(
        &self,
        _input: RecordIntakeBindingReady<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake candidate persistence",
        ))
    }
    async fn save_intake_inspection_consent(
        &self,
        _input: SaveIntakeInspectionConsent,
    ) -> McpPlatformResult<InspectionConsentRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake inspection persistence",
        ))
    }
    async fn get_intake_inspection_consent(
        &self,
        _consent_id: &str,
    ) -> McpPlatformResult<InspectionConsentRecord> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake inspection persistence",
        ))
    }
    async fn record_intake_inspection_snapshot(
        &self,
        _input: RecordIntakeInspectionSnapshot,
    ) -> McpPlatformResult<StoredInspectionSnapshot> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake inspection persistence",
        ))
    }
    async fn get_intake_inspection_snapshot(
        &self,
        _snapshot_id: &str,
    ) -> McpPlatformResult<StoredInspectionSnapshot> {
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support intake inspection persistence",
        ))
    }
    async fn create_task(&self, input: CreateTask<'_>) -> McpPlatformResult<TaskRecord>;
    async fn get_task(&self, task_id: &str) -> McpPlatformResult<TaskRecord>;
    async fn get_task_by_idempotency_key(
        &self,
        operation: TaskOperation,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<TaskRecord>>;
    async fn latest_managed_lifecycle_task(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<TaskRecord>>;
    async fn transition_task(
        &self,
        transition: TaskTransition<'_>,
    ) -> McpPlatformResult<TaskRecord>;
    async fn confirm_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord>;
    async fn request_cancel(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord>;
    async fn retry_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        retry_idempotency_key: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord>;
    async fn list_audit_events(&self, task_id: &str) -> McpPlatformResult<Vec<AuditEventRecord>>;
    async fn list_global_audit_events(
        &self,
        after_event_id: i64,
        limit: usize,
        task_ids: &[String],
    ) -> McpPlatformResult<GlobalAuditPage>;
    async fn next_queued_task_candidate(
        &self,
        now_ms: i64,
    ) -> McpPlatformResult<Option<QueuedTaskCandidate>>;
    async fn load_queued_task_preflight(
        &self,
        candidate: &QueuedTaskCandidate,
    ) -> McpPlatformResult<QueuedTaskPreflight>;
    async fn claim_preflighted_task(
        &self,
        preflight: &QueuedTaskPreflight,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<Option<TaskRecord>>;
    async fn reject_queued_task_candidate(
        &self,
        candidate: &QueuedTaskCandidate,
        actor: &str,
        now_ms: i64,
        redacted_error: &crate::mcp_platform::task::RedactedError,
    ) -> McpPlatformResult<bool>;
    async fn authorize_execution(
        &self,
        task_id: &str,
        owner_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ExecutionAuthorization>;
    async fn validate_execution_authorization(
        &self,
        authorization: &ExecutionAuthorization,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn renew_task_lease(
        &self,
        task_id: &str,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<TaskRecord>;
    async fn add_task_step(
        &self,
        task_id: &str,
        ordinal: i64,
        idempotency_token: &str,
        compensation: &CompensationDescriptor,
        adapter_id: &str,
        adapter_version: &str,
        owner_id: &str,
        expected_task_revision: i64,
        now_ms: i64,
    ) -> McpPlatformResult<TaskStepRecord>;
    async fn transition_task_step(
        &self,
        transition: StepTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord>;
    async fn transition_compensation(
        &self,
        transition: CompensationTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord>;
    async fn list_task_steps(&self, task_id: &str) -> McpPlatformResult<Vec<TaskStepRecord>>;
    async fn recover_stale_tasks(
        &self,
        heartbeat_cutoff_ms: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<Vec<crate::mcp_platform::repository::RecoveryRecord>>;
    async fn register_managed_mcp(
        &self,
        input: RegisterManagedMcp<'_>,
    ) -> McpPlatformResult<RegisterManagedMcpOutcome>;
    async fn stage_managed_installation(
        &self,
        input: StageManagedInstallation<'_>,
    ) -> McpPlatformResult<StageManagedInstallationOutcome>;
    async fn claim_artifact(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
        stale_before_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn mark_artifact_claim_verified(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn release_artifact_claim(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn mark_managed_runtime_activated(
        &self,
        managed_mcp_id: &str,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn finalize_retained_version_cleanup(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn is_managed_finalizing(
        &self,
        task_id: &str,
        operation: TaskOperation,
    ) -> McpPlatformResult<bool>;
    async fn record_managed_finalization_failure(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<i64>;
    async fn activate_managed_installation(
        &self,
        input: ActivateManagedInstallation<'_>,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord>;
    async fn rollback_managed_installation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn begin_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedUninstallSnapshot>;
    async fn mark_managed_uninstall_quarantined(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn cancel_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn finalize_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn get_managed_inventory(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord>;
    async fn list_managed_inventory(
        &self,
        after_managed_mcp_id: Option<&str>,
        limit: usize,
        filter: &crate::mcp_platform::repository::ManagedInventoryFilter,
    ) -> McpPlatformResult<Vec<ManagedMcpInventoryRecord>>;
    async fn put_owned_connection_projection(
        &self,
        input: PutOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord>;
    async fn get_connection_projection(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ConnectionProjectionRecord>;
    async fn restore_owned_connection_projection(
        &self,
        input: RestoreOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord>;
    async fn validate_owned_connection_projection_restore(
        &self,
        input: RestoreOwnedProjection<'_>,
    ) -> McpPlatformResult<()>;
    async fn remove_owned_projection(
        &self,
        input: RemoveOwnedProjection<'_>,
    ) -> McpPlatformResult<bool>;
    async fn remove_owned_managed_mcp(
        &self,
        managed_mcp_id: &str,
        owner_task_id: &str,
    ) -> McpPlatformResult<bool>;
    async fn update_managed_state(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        state: &ManagedMcpStateUpdate,
        now_ms: i64,
    ) -> McpPlatformResult<crate::mcp_platform::repository::ManagedMcpRecord>;
    async fn create_health_task(
        &self,
        input: CreateHealthTask<'_>,
    ) -> McpPlatformResult<TaskRecord>;
    async fn get_health_task_request(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<HealthTaskRequestRecord>;
    async fn append_health_observation(
        &self,
        input: NewHealthObservation<'_>,
    ) -> McpPlatformResult<HealthObservationRecord>;
    async fn latest_health_observation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<HealthObservationRecord>>;
    async fn list_retry_attempts(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<Vec<RetryAttemptRecord>>;
    async fn list_pending_projection_mutations(
        &self,
    ) -> McpPlatformResult<Vec<crate::mcp_platform::repository::ProjectionMutationRecord>>;
    async fn projection_recovery_required(&self, managed_mcp_id: &str) -> McpPlatformResult<bool>;
}

#[async_trait]
pub(crate) trait RemoteInspectionRepositoryPort: McpPlatformRepositoryPort {
    async fn reserve_or_load_active_remote_inspection(
        &self,
        input: ReserveOrLoadRemoteInspection<'_>,
    ) -> McpPlatformResult<RemoteInspectionReservationRecord>;

    async fn tombstone_remote_inspection_prebind(
        &self,
        input: TombstoneRemoteInspectionPrebind<'_>,
    ) -> McpPlatformResult<RemoteInspectionReservationRecord>;

    async fn bind_remote_inspection_pending_commit(
        &self,
        input: BindRemoteInspectionPendingCommit<'_>,
    ) -> McpPlatformResult<RemoteInspectionReservationRecord>;

    async fn record_remote_inspection_commit_outcome(
        &self,
        input: RecordRemoteInspectionCommitOutcome<'_>,
    ) -> McpPlatformResult<RemoteInspectionReservationRecord>;

    async fn claim_committed_remote_inspection_attempt(
        &self,
        input: ClaimCommittedRemoteInspectionAttempt<'_>,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord>;

    async fn record_remote_inspection_owner_started(
        &self,
        attempt_id: &str,
        claim_nonce: &str,
        now_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord>;

    async fn record_remote_inspection_owner_finished(
        &self,
        attempt_id: &str,
        claim_nonce: &str,
        now_ms: i64,
    ) -> McpPlatformResult<RemoteInspectionAttemptRecord>;

    async fn finalize_remote_inspection_consumption(
        &self,
        input: FinalizeRemoteInspectionConsumption<'_>,
    ) -> McpPlatformResult<RemoteInspectionConsumptionRecord>;
}

#[async_trait]
impl McpPlatformRepositoryPort for crate::mcp_platform::repository::SqliteMcpPlatformRepository {
    async fn save_https_manifest_provision(
        &self,
        record: HttpsManifestProvisionRecord,
        token_hash: &str,
    ) -> McpPlatformResult<()> {
        self.save_https_manifest_provision(record, token_hash).await
    }

    async fn claim_https_manifest_provision(
        &self,
        provision_id: &str,
        token_hash: &str,
        actor: &str,
        binding: &str,
        now_ms: i64,
    ) -> McpPlatformResult<HttpsManifestProvisionRecord> {
        self.claim_https_manifest_provision(provision_id, token_hash, actor, binding, now_ms)
            .await
    }
    async fn finalize_https_manifest_provision(
        &self,
        provision_id: &str,
        token_hash: &str,
        actor: &str,
        binding: &str,
        now_ms: i64,
        manifest: &ManifestRecord,
    ) -> McpPlatformResult<crate::mcp_platform::repository::InsertOutcome> {
        self.finalize_https_manifest_provision(
            provision_id,
            token_hash,
            actor,
            binding,
            now_ms,
            manifest,
        )
        .await
    }
    async fn consume_https_manifest_provision(
        &self,
        provision_id: &str,
        token_hash: &str,
        actor: &str,
        binding: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.consume_https_manifest_provision(provision_id, token_hash, actor, binding, now_ms)
            .await
    }
    async fn reject_https_manifest_provision(
        &self,
        provision_id: &str,
        token_hash: &str,
        actor: &str,
        binding: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.reject_https_manifest_provision(provision_id, token_hash, actor, binding, now_ms)
            .await
    }
    async fn verify_integrity(&self) -> McpPlatformResult<()> {
        self.verify_integrity().await
    }

    async fn recovery_eligibility(&self) -> McpPlatformResult<RecoveryEligibility> {
        self.recovery_eligibility().await
    }

    fn mint_intake_private_reference(
        &self,
        kind: IntakePrivateReferenceKind,
    ) -> McpPlatformResult<String> {
        crate::mcp_platform::repository::SqliteMcpPlatformRepository::mint_intake_private_reference(
            self, kind,
        )
    }

    async fn get_profile_managed_snapshot_source(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ProfileManagedSnapshotSource> {
        self.get_profile_managed_snapshot_source(managed_mcp_id)
            .await
    }
    async fn create_profile(&self, input: SaveProfile<'_>) -> McpPlatformResult<McpProfile> {
        self.create_profile(input).await
    }
    async fn change_profile(&self, input: ChangeProfile<'_>) -> McpPlatformResult<McpProfile> {
        self.change_profile(input).await
    }
    async fn list_profiles(&self, include_archived: bool) -> McpPlatformResult<Vec<McpProfile>> {
        self.list_profiles(include_archived).await
    }
    async fn get_profile(&self, profile_id: &str) -> McpPlatformResult<McpProfile> {
        self.get_profile(profile_id).await
    }
    async fn get_profile_revision(
        &self,
        profile_id: &str,
        revision: i64,
    ) -> McpPlatformResult<ProfileRevision> {
        self.get_profile_revision(profile_id, revision).await
    }
    async fn list_profile_revisions(
        &self,
        profile_id: &str,
    ) -> McpPlatformResult<Vec<ProfileRevision>> {
        self.list_profile_revisions(profile_id).await
    }
    async fn save_profile_apply_plan(
        &self,
        input: SaveProfileApplyPlan<'_>,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        self.save_profile_apply_plan(input).await
    }
    async fn get_profile_apply_plan(
        &self,
        plan_id: &str,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        self.get_profile_apply_plan(plan_id).await
    }
    async fn create_profile_apply_confirmation(
        &self,
        input: SaveProfileApplyConfirmation<'_>,
    ) -> McpPlatformResult<()> {
        self.create_profile_apply_confirmation(input).await
    }
    async fn get_profile_apply_confirmation_plan(
        &self,
        confirmation_hash: &str,
        plan_id: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        self.get_profile_apply_confirmation_plan(confirmation_hash, plan_id, actor, now_ms)
            .await
    }
    async fn get_profile_application_plan_by_token(
        &self,
        token_hash: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        self.get_profile_application_plan_by_token(token_hash, actor, now_ms)
            .await
    }
    async fn get_profile_application_plan_by_application(
        &self,
        application_id: &str,
        actor: &str,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        self.get_profile_application_plan_by_application(application_id, actor)
            .await
    }
    async fn get_profile_application_cleanup_state(
        &self,
        session_id: &str,
        actor: &str,
    ) -> McpPlatformResult<Option<(String, String)>> {
        self.get_profile_application_cleanup_state(session_id, actor)
            .await
    }
    async fn create_profile_application_token(
        &self,
        input: SaveProfileApplicationToken<'_>,
    ) -> McpPlatformResult<()> {
        self.create_profile_application_token(input).await
    }
    async fn consume_profile_application_token(
        &self,
        token_hash: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ConsumedProfileApplication> {
        self.consume_profile_application_token(token_hash, actor, now_ms)
            .await
    }
    async fn hydrate_profile_application(
        &self,
        marker: &ProfileApplicationMarker,
        session_id: &str,
        actor: &str,
    ) -> McpPlatformResult<ConsumedProfileApplication> {
        self.hydrate_profile_application(marker, session_id, actor)
            .await
    }
    async fn finish_profile_application(
        &self,
        application_id: &str,
        session_id: Option<&str>,
        status: &str,
        detail_code: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.finish_profile_application(
            application_id,
            session_id,
            status,
            detail_code,
            actor,
            now_ms,
        )
        .await
    }
    async fn save_manifest(
        &self,
        record: &crate::mcp_platform::repository::ManifestRecord,
    ) -> McpPlatformResult<crate::mcp_platform::repository::InsertOutcome> {
        self.save_manifest(record).await
    }

    async fn save_governed_catalog_import(
        &self,
        input: SaveGovernedCatalogImport,
    ) -> McpPlatformResult<()> {
        self.save_governed_catalog_import(input).await
    }

    async fn confirm_source_provisioning(
        &self,
        input: ConfirmSourceProvisioning,
    ) -> McpPlatformResult<crate::mcp_platform::repository::SourceProvisioningAuditRecord> {
        self.confirm_source_provisioning(input).await
    }

    async fn list_manifests(&self) -> McpPlatformResult<Vec<ManifestRecord>> {
        self.list_manifests().await
    }

    async fn list_manifests_with_source_context(
        &self,
        source_context: &ManifestSourceContext,
    ) -> McpPlatformResult<Vec<ManifestRecord>> {
        SqliteMcpPlatformRepository::list_manifests_with_source_context(self, source_context).await
    }

    async fn list_governed_catalog_entries(
        &self,
    ) -> McpPlatformResult<Vec<GovernedCatalogEntryRecord>> {
        self.list_governed_catalog_entries().await
    }

    async fn get_governed_catalog_entry(
        &self,
        source_id: &str,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<GovernedCatalogEntryRecord> {
        self.get_governed_catalog_entry(source_id, mcp_id, version)
            .await
    }

    async fn get_governed_catalog_entry_by_digest(
        &self,
        manifest_digest: &str,
    ) -> McpPlatformResult<GovernedCatalogEntryRecord> {
        self.get_governed_catalog_entry_by_digest(manifest_digest)
            .await
    }

    async fn list_governed_catalog_entries_by_digest(
        &self,
        manifest_digest: &str,
    ) -> McpPlatformResult<Vec<GovernedCatalogEntryRecord>> {
        self.list_governed_catalog_entries_by_digest(manifest_digest)
            .await
    }

    async fn source_catalog_document_provenance(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<
        crate::mcp_platform::repository::sqlite::StoredSourceCatalogDocumentProvenance,
    > {
        self.source_catalog_document_provenance(source_id).await
    }

    async fn source_catalog_release_matches(
        &self,
        source_id: &str,
        release_id: &str,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<bool> {
        self.source_catalog_release_matches(source_id, release_id, mcp_id, version)
            .await
    }
    async fn list_governed_catalog_sources(
        &self,
    ) -> McpPlatformResult<Vec<GovernedCatalogSourceRecord>> {
        self.list_governed_catalog_sources().await
    }
    async fn get_governed_catalog_source(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<GovernedCatalogSourceRecord> {
        self.get_governed_catalog_source(source_id).await
    }
    async fn save_governed_source_refresh_registration(
        &self,
        input: SaveGovernedSourceRefreshRegistration<'_>,
    ) -> McpPlatformResult<GovernedSourceRefreshRegistrationRecord> {
        self.save_governed_source_refresh_registration(input).await
    }
    async fn get_governed_source_refresh_registration(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<GovernedSourceRefreshRegistrationRecord> {
        self.get_governed_source_refresh_registration(source_id)
            .await
    }
    async fn get_governed_source_refresh_trust_anchor(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<GovernedSourceRefreshTrustAnchorRecord> {
        self.get_governed_source_refresh_trust_anchor(source_id)
            .await
    }
    async fn has_governed_source_refresh_trust_anchor(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<bool> {
        self.has_governed_source_refresh_trust_anchor(source_id)
            .await
    }
    async fn get_governed_source_trust_pin(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<GovernedSourceTrustPinRecord> {
        self.get_governed_source_trust_pin(source_id).await
    }
    async fn apply_governed_source_refresh(
        &self,
        input: ApplyGovernedSourceRefresh,
    ) -> McpPlatformResult<GovernedSourceRefreshCommitOutcome> {
        self.apply_governed_source_refresh(input).await
    }
    async fn list_governed_source_refresh_registrations(
        &self,
    ) -> McpPlatformResult<Vec<GovernedSourceRefreshRegistrationRecord>> {
        self.list_governed_source_refresh_registrations().await
    }
    async fn record_governed_source_refresh_audit(
        &self,
        input: RecordGovernedSourceRefreshAudit<'_>,
    ) -> McpPlatformResult<GovernedSourceRefreshAuditRecord> {
        self.record_governed_source_refresh_audit(input).await
    }
    async fn list_governed_source_refresh_audits(
        &self,
        source_id: &str,
    ) -> McpPlatformResult<Vec<GovernedSourceRefreshAuditRecord>> {
        self.list_governed_source_refresh_audits(source_id).await
    }

    async fn get_manifest(&self, digest: &str) -> McpPlatformResult<ManifestRecord> {
        self.get_manifest(digest).await
    }

    async fn get_manifest_with_source_context(
        &self,
        digest: &str,
        source_context: &ManifestSourceContext,
    ) -> McpPlatformResult<ManifestRecord> {
        SqliteMcpPlatformRepository::get_manifest_with_source_context(self, digest, source_context)
            .await
    }

    async fn get_manifest_by_identity(
        &self,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<ManifestRecord> {
        self.get_manifest_by_identity(mcp_id, version).await
    }

    async fn get_manifest_by_identity_with_source_context(
        &self,
        mcp_id: &str,
        version: &str,
        source_context: &ManifestSourceContext,
    ) -> McpPlatformResult<ManifestRecord> {
        SqliteMcpPlatformRepository::get_manifest_by_identity_with_source_context(
            self,
            mcp_id,
            version,
            source_context,
        )
        .await
    }

    async fn get_managed_credential_enrollment(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<ManagedCredentialEnrollmentRecord>> {
        self.get_managed_credential_enrollment(managed_mcp_id).await
    }

    async fn save_managed_credential_enrollment(
        &self,
        input: SaveManagedCredentialEnrollment<'_>,
    ) -> McpPlatformResult<ManagedCredentialEnrollmentRecord> {
        self.save_managed_credential_enrollment(input).await
    }

    async fn save_plan(&self, input: SavePlan<'_>) -> McpPlatformResult<PlanRecord> {
        self.save_plan(input).await
    }

    async fn get_plan(&self, plan_id: &str) -> McpPlatformResult<PlanRecord> {
        self.get_plan(plan_id).await
    }

    async fn get_plan_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<PlanRecord>> {
        self.get_plan_by_idempotency_key(idempotency_key).await
    }

    async fn save_intake_configuration_ref(
        &self,
        input: SaveIntakeConfigurationRef<'_>,
    ) -> McpPlatformResult<String> {
        self.save_intake_configuration_ref(input).await
    }

    async fn save_intake_candidate(
        &self,
        input: SaveIntakeCandidate<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.save_intake_candidate(input).await
    }

    async fn get_intake_candidate(
        &self,
        candidate_id: &str,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.get_intake_candidate(candidate_id).await
    }

    async fn get_intake_candidate_by_submission_binding(
        &self,
        submission_binding: &str,
    ) -> McpPlatformResult<Option<IntakeCandidateRecord>> {
        self.get_intake_candidate_by_submission_binding(submission_binding)
            .await
    }

    async fn record_intake_consent_grant(
        &self,
        input: RecordIntakeConsentGrant<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.record_intake_consent_grant(input).await
    }

    async fn record_intake_approval_grant(
        &self,
        input: RecordIntakeApprovalGrant<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.record_intake_approval_grant(input).await
    }

    async fn record_intake_binding_ready(
        &self,
        input: RecordIntakeBindingReady<'_>,
    ) -> McpPlatformResult<IntakeCandidateRecord> {
        self.record_intake_binding_ready(input).await
    }

    async fn save_intake_inspection_consent(
        &self,
        input: SaveIntakeInspectionConsent,
    ) -> McpPlatformResult<InspectionConsentRecord> {
        self.save_intake_inspection_consent(input).await
    }

    async fn get_intake_inspection_consent(
        &self,
        consent_id: &str,
    ) -> McpPlatformResult<InspectionConsentRecord> {
        self.get_intake_inspection_consent(consent_id).await
    }

    async fn record_intake_inspection_snapshot(
        &self,
        input: RecordIntakeInspectionSnapshot,
    ) -> McpPlatformResult<StoredInspectionSnapshot> {
        self.record_intake_inspection_snapshot(input).await
    }

    async fn get_intake_inspection_snapshot(
        &self,
        snapshot_id: &str,
    ) -> McpPlatformResult<StoredInspectionSnapshot> {
        self.get_intake_inspection_snapshot(snapshot_id).await
    }

    async fn create_task(&self, input: CreateTask<'_>) -> McpPlatformResult<TaskRecord> {
        self.create_task(input).await
    }

    async fn get_task(&self, task_id: &str) -> McpPlatformResult<TaskRecord> {
        self.get_task(task_id).await
    }

    async fn get_task_by_idempotency_key(
        &self,
        operation: TaskOperation,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        self.get_task_by_idempotency_key(operation, idempotency_key)
            .await
    }

    async fn latest_managed_lifecycle_task(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        self.latest_managed_lifecycle_task(managed_mcp_id).await
    }

    async fn transition_task(
        &self,
        transition: TaskTransition<'_>,
    ) -> McpPlatformResult<TaskRecord> {
        self.transition_task(transition).await
    }

    async fn confirm_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        self.confirm_task(task_id, expected_revision, actor, now_ms)
            .await
    }

    async fn request_cancel(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        self.request_cancel(task_id, expected_revision, actor, now_ms)
            .await
    }

    async fn retry_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        retry_idempotency_key: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        self.retry_task(
            task_id,
            expected_revision,
            retry_idempotency_key,
            actor,
            now_ms,
        )
        .await
    }

    async fn list_audit_events(&self, task_id: &str) -> McpPlatformResult<Vec<AuditEventRecord>> {
        self.list_audit_events(task_id).await
    }

    async fn list_global_audit_events(
        &self,
        after_event_id: i64,
        limit: usize,
        task_ids: &[String],
    ) -> McpPlatformResult<GlobalAuditPage> {
        self.list_global_audit_events(after_event_id, limit, task_ids)
            .await
    }

    async fn next_queued_task_candidate(
        &self,
        now_ms: i64,
    ) -> McpPlatformResult<Option<QueuedTaskCandidate>> {
        self.next_queued_task_candidate(now_ms).await
    }

    async fn load_queued_task_preflight(
        &self,
        candidate: &QueuedTaskCandidate,
    ) -> McpPlatformResult<QueuedTaskPreflight> {
        self.load_queued_task_preflight(candidate).await
    }

    async fn claim_preflighted_task(
        &self,
        preflight: &QueuedTaskPreflight,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        self.claim_preflighted_task(preflight, owner_id, now_ms, lease_duration_ms)
            .await
    }

    async fn reject_queued_task_candidate(
        &self,
        candidate: &QueuedTaskCandidate,
        actor: &str,
        now_ms: i64,
        redacted_error: &crate::mcp_platform::task::RedactedError,
    ) -> McpPlatformResult<bool> {
        self.reject_queued_task_candidate(candidate, actor, now_ms, redacted_error)
            .await
    }

    async fn authorize_execution(
        &self,
        task_id: &str,
        owner_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ExecutionAuthorization> {
        self.authorize_execution(task_id, owner_id, now_ms).await
    }

    async fn validate_execution_authorization(
        &self,
        authorization: &ExecutionAuthorization,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.validate_execution_authorization(authorization, now_ms)
            .await
    }

    async fn renew_task_lease(
        &self,
        task_id: &str,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        self.renew_task_lease(task_id, owner_id, now_ms, lease_duration_ms)
            .await
    }

    async fn add_task_step(
        &self,
        task_id: &str,
        ordinal: i64,
        idempotency_token: &str,
        compensation: &CompensationDescriptor,
        adapter_id: &str,
        adapter_version: &str,
        owner_id: &str,
        expected_task_revision: i64,
        now_ms: i64,
    ) -> McpPlatformResult<TaskStepRecord> {
        self.add_task_step_with_adapter(
            task_id,
            ordinal,
            idempotency_token,
            compensation,
            adapter_id,
            adapter_version,
            owner_id,
            expected_task_revision,
            now_ms,
        )
        .await
    }

    async fn transition_task_step(
        &self,
        transition: StepTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord> {
        self.transition_task_step(transition).await
    }

    async fn transition_compensation(
        &self,
        transition: CompensationTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord> {
        self.transition_compensation(transition).await
    }

    async fn list_task_steps(&self, task_id: &str) -> McpPlatformResult<Vec<TaskStepRecord>> {
        self.list_task_steps(task_id).await
    }

    async fn recover_stale_tasks(
        &self,
        heartbeat_cutoff_ms: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<Vec<crate::mcp_platform::repository::RecoveryRecord>> {
        self.recover_stale_tasks(heartbeat_cutoff_ms, actor, now_ms)
            .await
    }

    async fn register_managed_mcp(
        &self,
        input: RegisterManagedMcp<'_>,
    ) -> McpPlatformResult<RegisterManagedMcpOutcome> {
        self.register_managed_mcp(input).await
    }

    async fn stage_managed_installation(
        &self,
        input: StageManagedInstallation<'_>,
    ) -> McpPlatformResult<StageManagedInstallationOutcome> {
        self.stage_managed_installation(input).await
    }
    async fn claim_artifact(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
        stale_before_ms: i64,
    ) -> McpPlatformResult<()> {
        self.claim_artifact(artifact_digest, task_id, now_ms, stale_before_ms)
            .await
    }
    async fn mark_artifact_claim_verified(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.mark_artifact_claim_verified(artifact_digest, task_id, now_ms)
            .await
    }
    async fn release_artifact_claim(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.release_artifact_claim(artifact_digest, task_id, now_ms)
            .await
    }
    async fn mark_managed_runtime_activated(
        &self,
        managed_mcp_id: &str,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.mark_managed_runtime_activated(managed_mcp_id, target_version, task_id, now_ms)
            .await
    }
    async fn finalize_retained_version_cleanup(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.finalize_retained_version_cleanup(managed_mcp_id, version, task_id, now_ms)
            .await
    }
    async fn is_managed_finalizing(
        &self,
        task_id: &str,
        operation: TaskOperation,
    ) -> McpPlatformResult<bool> {
        self.is_managed_finalizing(task_id, operation).await
    }

    async fn record_managed_finalization_failure(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<i64> {
        self.record_managed_finalization_failure(task_id, now_ms)
            .await
    }
    async fn activate_managed_installation(
        &self,
        input: ActivateManagedInstallation<'_>,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        self.activate_managed_installation(input).await
    }
    async fn rollback_managed_installation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.rollback_managed_installation(
            managed_mcp_id,
            previous_version,
            target_version,
            task_id,
            now_ms,
        )
        .await
    }
    async fn begin_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedUninstallSnapshot> {
        self.begin_managed_uninstall(managed_mcp_id, version, task_id, now_ms)
            .await
    }
    async fn mark_managed_uninstall_quarantined(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.mark_managed_uninstall_quarantined(task_id, now_ms)
            .await
    }
    async fn cancel_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.cancel_managed_uninstall(managed_mcp_id, task_id, now_ms)
            .await
    }
    async fn finalize_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.finalize_managed_uninstall(managed_mcp_id, version, task_id, now_ms)
            .await
    }

    async fn get_managed_inventory(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        self.get_managed_inventory(managed_mcp_id).await
    }

    async fn list_managed_inventory(
        &self,
        after_managed_mcp_id: Option<&str>,
        limit: usize,
        filter: &crate::mcp_platform::repository::ManagedInventoryFilter,
    ) -> McpPlatformResult<Vec<ManagedMcpInventoryRecord>> {
        self.list_managed_inventory(after_managed_mcp_id, limit, filter)
            .await
    }

    async fn put_owned_connection_projection(
        &self,
        input: PutOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        self.put_owned_connection_projection(input).await
    }

    async fn get_connection_projection(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        self.get_connection_projection(managed_mcp_id).await
    }
    async fn restore_owned_connection_projection(
        &self,
        input: RestoreOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        self.restore_owned_connection_projection(input).await
    }

    async fn validate_owned_connection_projection_restore(
        &self,
        input: RestoreOwnedProjection<'_>,
    ) -> McpPlatformResult<()> {
        self.validate_owned_connection_projection_restore(input)
            .await
    }

    async fn remove_owned_projection(
        &self,
        input: RemoveOwnedProjection<'_>,
    ) -> McpPlatformResult<bool> {
        self.remove_owned_projection(input).await
    }

    async fn remove_owned_managed_mcp(
        &self,
        managed_mcp_id: &str,
        owner_task_id: &str,
    ) -> McpPlatformResult<bool> {
        self.remove_owned_managed_mcp(managed_mcp_id, owner_task_id)
            .await
    }

    async fn update_managed_state(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        state: &ManagedMcpStateUpdate,
        now_ms: i64,
    ) -> McpPlatformResult<crate::mcp_platform::repository::ManagedMcpRecord> {
        self.apply_managed_state_update(managed_mcp_id, expected_revision, state, now_ms)
            .await
    }

    async fn create_health_task(
        &self,
        input: CreateHealthTask<'_>,
    ) -> McpPlatformResult<TaskRecord> {
        self.create_health_task(input).await
    }

    async fn get_health_task_request(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<HealthTaskRequestRecord> {
        self.get_health_task_request(task_id).await
    }

    async fn append_health_observation(
        &self,
        input: NewHealthObservation<'_>,
    ) -> McpPlatformResult<HealthObservationRecord> {
        self.append_health_observation(input).await
    }

    async fn latest_health_observation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<HealthObservationRecord>> {
        self.latest_health_observation(managed_mcp_id).await
    }

    async fn list_retry_attempts(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<Vec<RetryAttemptRecord>> {
        self.list_retry_attempts(task_id).await
    }

    async fn list_pending_projection_mutations(
        &self,
    ) -> McpPlatformResult<Vec<crate::mcp_platform::repository::ProjectionMutationRecord>> {
        self.list_pending_projection_mutations().await
    }

    async fn projection_recovery_required(&self, managed_mcp_id: &str) -> McpPlatformResult<bool> {
        self.projection_recovery_required(managed_mcp_id).await
    }
}

fn profile_repository_unsupported() -> crate::mcp_platform::McpPlatformError {
    crate::mcp_platform::McpPlatformError::new(
        crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
        "repository does not support MCP profiles",
    )
}
