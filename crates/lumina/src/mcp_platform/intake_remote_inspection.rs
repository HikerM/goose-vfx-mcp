use std::fmt;

use url::{Host, Url};

use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::intake::{
    AuthorityCommitOutcome, AuthorityVerifiedOpaqueTuple, IntakeLifecycleState, IntakeSourceFacet,
    IntakeTransport, VerifiedNoLiveProof,
};

pub(crate) const REMOTE_INSPECTION_POLICY_REVISION: i64 = 1;
pub(crate) const REMOTE_INSPECTION_TRANSPORT_UNSUPPORTED_SUBCODE: &str =
    "remote_inspection_transport_unsupported";
pub(crate) const REMOTE_INSPECTION_V25_PERSISTENCE_PURPOSE: &str = "private_remote_check_v25";

const MAX_OPAQUE_VALUE_LEN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionGateError {
    TransportUnsupported,
}

impl RemoteInspectionGateError {
    pub(crate) const fn subcode(self) -> &'static str {
        match self {
            Self::TransportUnsupported => REMOTE_INSPECTION_TRANSPORT_UNSUPPORTED_SUBCODE,
        }
    }

    pub(crate) const fn as_mcp_error(self) -> McpPlatformError {
        match self {
            Self::TransportUnsupported => McpPlatformError::new(
                McpPlatformErrorCode::OperationNotSupported,
                REMOTE_INSPECTION_TRANSPORT_UNSUPPORTED_SUBCODE,
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionPurpose {
    PrivateRemoteCheckV26,
}

impl RemoteInspectionPurpose {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PrivateRemoteCheckV26 => "private_remote_check_v26",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "private_remote_check_v26" => Some(Self::PrivateRemoteCheckV26),
            _ => None,
        }
    }

    pub(crate) fn derive(
        source_facet: IntakeSourceFacet,
        transport: IntakeTransport,
    ) -> Result<Self, RemoteInspectionGateError> {
        match (source_facet, transport) {
            (IntakeSourceFacet::ManualHttpsCandidate, IntakeTransport::StreamableHttp) => {
                Ok(Self::PrivateRemoteCheckV26)
            }
            (IntakeSourceFacet::CatalogPlanning, IntakeTransport::CatalogReference) => {
                Err(RemoteInspectionGateError::TransportUnsupported)
            }
            (IntakeSourceFacet::ApprovedStdioCandidate, IntakeTransport::Stdio)
            | (IntakeSourceFacet::LegacyQuarantine, IntakeTransport::Legacy) => {
                Err(RemoteInspectionGateError::TransportUnsupported)
            }
            _ => Err(RemoteInspectionGateError::TransportUnsupported),
        }
    }
}

pub(crate) const fn v25_persistence_purpose_for_remote_inspection(
    purpose: RemoteInspectionPurpose,
) -> &'static str {
    match purpose {
        RemoteInspectionPurpose::PrivateRemoteCheckV26 => REMOTE_INSPECTION_V25_PERSISTENCE_PURPOSE,
    }
}

pub(crate) fn parse_v25_persistence_purpose(value: &str) -> Option<RemoteInspectionPurpose> {
    match value {
        REMOTE_INSPECTION_V25_PERSISTENCE_PURPOSE => {
            Some(RemoteInspectionPurpose::PrivateRemoteCheckV26)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionAuthorityProvider {
    SealedAuthority,
    InMemoryTest,
}

impl RemoteInspectionAuthorityProvider {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SealedAuthority => "sealed_authority",
            Self::InMemoryTest => "in_memory_test",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "sealed_authority" => Some(Self::SealedAuthority),
            "in_memory_test" => Some(Self::InMemoryTest),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionReservationState {
    PrebindReserved,
    PrebindTombstonedTerminal,
    PostbindPendingCommit,
    ReconcileRequired,
    CommittedClaimable,
    PostbindBlockedTerminal,
    Claimed,
    OwnerStarted,
    OwnerFinished,
    ConsumedFinalized,
}

impl RemoteInspectionReservationState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PrebindReserved => "prebind_reserved",
            Self::PrebindTombstonedTerminal => "prebind_tombstoned_terminal",
            Self::PostbindPendingCommit => "postbind_pending_commit",
            Self::ReconcileRequired => "reconcile_required",
            Self::CommittedClaimable => "committed_claimable",
            Self::PostbindBlockedTerminal => "postbind_blocked_terminal",
            Self::Claimed => "claimed",
            Self::OwnerStarted => "owner_started",
            Self::OwnerFinished => "owner_finished",
            Self::ConsumedFinalized => "consumed_finalized",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "prebind_reserved" => Some(Self::PrebindReserved),
            "prebind_tombstoned_terminal" => Some(Self::PrebindTombstonedTerminal),
            "postbind_pending_commit" => Some(Self::PostbindPendingCommit),
            "reconcile_required" => Some(Self::ReconcileRequired),
            "committed_claimable" => Some(Self::CommittedClaimable),
            "postbind_blocked_terminal" => Some(Self::PostbindBlockedTerminal),
            "claimed" => Some(Self::Claimed),
            "owner_started" => Some(Self::OwnerStarted),
            "owner_finished" => Some(Self::OwnerFinished),
            "consumed_finalized" => Some(Self::ConsumedFinalized),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionReservationEventType {
    Reserved,
    PrebindTombstoned,
    PostbindBound,
    AuthorityReconcileRequired,
    AuthorityCommitted,
    PostbindBlockedNoLive,
    ClaimExpired,
    Claimed,
    OwnerStarted,
    OwnerFinished,
    ConsumedFinalized,
}

impl RemoteInspectionReservationEventType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::PrebindTombstoned => "prebind_tombstoned",
            Self::PostbindBound => "postbind_bound",
            Self::AuthorityReconcileRequired => "authority_reconcile_required",
            Self::AuthorityCommitted => "authority_committed",
            Self::PostbindBlockedNoLive => "postbind_blocked_no_live",
            Self::ClaimExpired => "claim_expired",
            Self::Claimed => "claimed",
            Self::OwnerStarted => "owner_started",
            Self::OwnerFinished => "owner_finished",
            Self::ConsumedFinalized => "consumed_finalized",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionCommitState {
    PendingAuthorityCommit,
    Committed,
    BlockedNoLive,
}

impl RemoteInspectionCommitState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PendingAuthorityCommit => "pending_authority_commit",
            Self::Committed => "committed",
            Self::BlockedNoLive => "blocked_no_live",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "pending_authority_commit" => Some(Self::PendingAuthorityCommit),
            "committed" => Some(Self::Committed),
            "blocked_no_live" => Some(Self::BlockedNoLive),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionNoLiveProofKind {
    NeverStaged,
    Tombstoned,
    DefinitiveNoLivePostbind,
    Unknown,
    Missing,
    Unavailable,
    CredentialLost,
}

impl RemoteInspectionNoLiveProofKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NeverStaged => "never_staged",
            Self::Tombstoned => "tombstoned",
            Self::DefinitiveNoLivePostbind => "definitive_no_live_postbind",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
            Self::Unavailable => "unavailable",
            Self::CredentialLost => "credential_lost",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "never_staged" => Some(Self::NeverStaged),
            "tombstoned" => Some(Self::Tombstoned),
            "definitive_no_live_postbind" => Some(Self::DefinitiveNoLivePostbind),
            "unknown" => Some(Self::Unknown),
            "missing" => Some(Self::Missing),
            "unavailable" => Some(Self::Unavailable),
            "credential_lost" => Some(Self::CredentialLost),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionAttemptState {
    Claimed,
    OwnerStarted,
    OwnerFinished,
    Finalized,
}

impl RemoteInspectionAttemptState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::OwnerStarted => "owner_started",
            Self::OwnerFinished => "owner_finished",
            Self::Finalized => "finalized",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "claimed" => Some(Self::Claimed),
            "owner_started" => Some(Self::OwnerStarted),
            "owner_finished" => Some(Self::OwnerFinished),
            "finalized" => Some(Self::Finalized),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionSafeSummary {
    TargetRejected,
    AttemptRejected,
    InspectionBlocked,
    ClassificationUnavailable,
}

impl RemoteInspectionSafeSummary {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::TargetRejected => "target_rejected",
            Self::AttemptRejected => "attempt_rejected",
            Self::InspectionBlocked => "inspection_blocked",
            Self::ClassificationUnavailable => "classification_unavailable",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "target_rejected" => Some(Self::TargetRejected),
            "attempt_rejected" => Some(Self::AttemptRejected),
            "inspection_blocked" => Some(Self::InspectionBlocked),
            "classification_unavailable" => Some(Self::ClassificationUnavailable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionSafeSubcode {
    EmptyTarget,
    ControlCharacterRejected,
    InvalidUrl,
    HttpsRequired,
    HostMissing,
    HostMustContainDot,
    IpLiteralRejected,
    UserInfoRejected,
    FragmentRejected,
    QueryRejected,
    PortNotAllowed,
    ActiveAttemptExists,
    ClaimNonceMismatch,
    InvalidAttemptTransition,
    ConsentExpired,
    ConsentConsumed,
    PolicyRevisionDrift,
    SnapshotIntegrityConflict,
    ClassificationUnavailable,
}

impl RemoteInspectionSafeSubcode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::EmptyTarget => "empty_target",
            Self::ControlCharacterRejected => "control_character_rejected",
            Self::InvalidUrl => "invalid_url",
            Self::HttpsRequired => "https_required",
            Self::HostMissing => "host_missing",
            Self::HostMustContainDot => "host_must_contain_dot",
            Self::IpLiteralRejected => "ip_literal_rejected",
            Self::UserInfoRejected => "userinfo_rejected",
            Self::FragmentRejected => "fragment_rejected",
            Self::QueryRejected => "query_rejected",
            Self::PortNotAllowed => "port_not_allowed",
            Self::ActiveAttemptExists => "active_attempt_exists",
            Self::ClaimNonceMismatch => "claim_nonce_mismatch",
            Self::InvalidAttemptTransition => "invalid_attempt_transition",
            Self::ConsentExpired => "consent_expired",
            Self::ConsentConsumed => "consent_consumed",
            Self::PolicyRevisionDrift => "policy_revision_drift",
            Self::SnapshotIntegrityConflict => "snapshot_integrity_conflict",
            Self::ClassificationUnavailable => "classification_unavailable",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "empty_target" => Some(Self::EmptyTarget),
            "control_character_rejected" => Some(Self::ControlCharacterRejected),
            "invalid_url" => Some(Self::InvalidUrl),
            "https_required" => Some(Self::HttpsRequired),
            "host_missing" => Some(Self::HostMissing),
            "host_must_contain_dot" => Some(Self::HostMustContainDot),
            "ip_literal_rejected" => Some(Self::IpLiteralRejected),
            "userinfo_rejected" => Some(Self::UserInfoRejected),
            "fragment_rejected" => Some(Self::FragmentRejected),
            "query_rejected" => Some(Self::QueryRejected),
            "port_not_allowed" => Some(Self::PortNotAllowed),
            "active_attempt_exists" => Some(Self::ActiveAttemptExists),
            "claim_nonce_mismatch" => Some(Self::ClaimNonceMismatch),
            "invalid_attempt_transition" => Some(Self::InvalidAttemptTransition),
            "consent_expired" => Some(Self::ConsentExpired),
            "consent_consumed" => Some(Self::ConsentConsumed),
            "policy_revision_drift" => Some(Self::PolicyRevisionDrift),
            "snapshot_integrity_conflict" => Some(Self::SnapshotIntegrityConflict),
            "classification_unavailable" => Some(Self::ClassificationUnavailable),
            _ => None,
        }
    }

    pub(crate) const fn summary(self) -> RemoteInspectionSafeSummary {
        match self {
            Self::EmptyTarget
            | Self::ControlCharacterRejected
            | Self::InvalidUrl
            | Self::HttpsRequired
            | Self::HostMissing
            | Self::HostMustContainDot
            | Self::IpLiteralRejected
            | Self::UserInfoRejected
            | Self::FragmentRejected
            | Self::QueryRejected
            | Self::PortNotAllowed => RemoteInspectionSafeSummary::TargetRejected,
            Self::ActiveAttemptExists
            | Self::ClaimNonceMismatch
            | Self::InvalidAttemptTransition
            | Self::ConsentExpired
            | Self::ConsentConsumed => RemoteInspectionSafeSummary::AttemptRejected,
            Self::PolicyRevisionDrift | Self::SnapshotIntegrityConflict => {
                RemoteInspectionSafeSummary::InspectionBlocked
            }
            Self::ClassificationUnavailable => {
                RemoteInspectionSafeSummary::ClassificationUnavailable
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteInspectionFailure {
    code: McpPlatformErrorCode,
    pub(crate) subcode: RemoteInspectionSafeSubcode,
    pub(crate) safe_summary: RemoteInspectionSafeSummary,
}

impl RemoteInspectionFailure {
    pub(crate) const fn code(&self) -> McpPlatformErrorCode {
        self.code
    }

    pub(crate) const fn as_mcp_error(&self) -> McpPlatformError {
        McpPlatformError::new(self.code, "remote inspection failed closed")
    }
}

pub(crate) type RemoteInspectionResult<T> = Result<T, RemoteInspectionFailure>;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RemoteInspectionReservationRecord {
    pub(crate) reservation_id: String,
    pub(crate) generation: i64,
    pub(crate) candidate_id: String,
    pub(crate) candidate_revision: i64,
    pub(crate) candidate_lifecycle_state: IntakeLifecycleState,
    pub(crate) source_facet: IntakeSourceFacet,
    pub(crate) transport: IntakeTransport,
    pub(crate) purpose: RemoteInspectionPurpose,
    pub(crate) policy_revision: i64,
    pub(crate) intent_fingerprint: String,
    pub(crate) intent_fingerprint_key_id: String,
    pub(crate) state: RemoteInspectionReservationState,
    pub(crate) consent_id: Option<String>,
    pub(crate) claim_attempt_id: Option<String>,
    pub(crate) authority_provider: Option<RemoteInspectionAuthorityProvider>,
    pub(crate) authority_key_epoch: Option<i64>,
    pub(crate) authority_generation: Option<i64>,
    pub(crate) has_authority_record_ref: bool,
    pub(crate) no_live_proof_kind: Option<RemoteInspectionNoLiveProofKind>,
    pub(crate) no_live_proof_observed_at_ms: Option<i64>,
    pub(crate) last_event_ordinal: i64,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
}

impl fmt::Debug for RemoteInspectionReservationRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteInspectionReservationRecord")
            .field("reservation_id", &self.reservation_id)
            .field("generation", &self.generation)
            .field("candidate_id", &self.candidate_id)
            .field("candidate_revision", &self.candidate_revision)
            .field("candidate_lifecycle_state", &self.candidate_lifecycle_state)
            .field("source_facet", &self.source_facet)
            .field("transport", &self.transport)
            .field("purpose", &self.purpose)
            .field("policy_revision", &self.policy_revision)
            .field("intent_fingerprint", &"[REDACTED]")
            .field("intent_fingerprint_key_id", &self.intent_fingerprint_key_id)
            .field("state", &self.state)
            .field("consent_id", &self.consent_id)
            .field("claim_attempt_id", &self.claim_attempt_id)
            .field("authority_provider", &self.authority_provider)
            .field("authority_key_epoch", &self.authority_key_epoch)
            .field("authority_generation", &self.authority_generation)
            .field("has_authority_record_ref", &self.has_authority_record_ref)
            .field("no_live_proof_kind", &self.no_live_proof_kind)
            .field(
                "no_live_proof_observed_at_ms",
                &self.no_live_proof_observed_at_ms,
            )
            .field("last_event_ordinal", &self.last_event_ordinal)
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RemoteInspectionConsentRecord {
    pub(crate) consent_id: String,
    pub(crate) candidate_id: String,
    pub(crate) candidate_revision: i64,
    pub(crate) candidate_lifecycle_state: IntakeLifecycleState,
    pub(crate) source_facet: IntakeSourceFacet,
    pub(crate) transport: IntakeTransport,
    pub(crate) purpose: RemoteInspectionPurpose,
    pub(crate) policy_revision: i64,
    pub(crate) expires_at_ms: i64,
    pub(crate) authority_provider: RemoteInspectionAuthorityProvider,
    pub(crate) provider_key_epoch: i64,
    pub(crate) generation: i64,
    pub(crate) has_authority_record_ref: bool,
    pub(crate) created_at_ms: i64,
}

impl fmt::Debug for RemoteInspectionConsentRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteInspectionConsentRecord")
            .field("consent_id", &self.consent_id)
            .field("candidate_id", &self.candidate_id)
            .field("candidate_revision", &self.candidate_revision)
            .field("candidate_lifecycle_state", &self.candidate_lifecycle_state)
            .field("source_facet", &self.source_facet)
            .field("transport", &self.transport)
            .field("purpose", &self.purpose)
            .field("policy_revision", &self.policy_revision)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("authority_provider", &self.authority_provider)
            .field("provider_key_epoch", &self.provider_key_epoch)
            .field("generation", &self.generation)
            .field("has_authority_record_ref", &self.has_authority_record_ref)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RemoteInspectionAttemptRecord {
    pub(crate) attempt_id: String,
    pub(crate) consent_id: String,
    pub(crate) state: RemoteInspectionAttemptState,
    pub(crate) claim_nonce: String,
    pub(crate) claimed_at_ms: i64,
    pub(crate) claim_expires_at_ms: i64,
    pub(crate) owner_started_at_ms: Option<i64>,
    pub(crate) owner_finished_at_ms: Option<i64>,
    pub(crate) finalized_at_ms: Option<i64>,
}

impl fmt::Debug for RemoteInspectionAttemptRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteInspectionAttemptRecord")
            .field("attempt_id", &self.attempt_id)
            .field("consent_id", &self.consent_id)
            .field("state", &self.state)
            .field("claim_nonce", &"[REDACTED]")
            .field("claimed_at_ms", &self.claimed_at_ms)
            .field("claim_expires_at_ms", &self.claim_expires_at_ms)
            .field("owner_started_at_ms", &self.owner_started_at_ms)
            .field("owner_finished_at_ms", &self.owner_finished_at_ms)
            .field("finalized_at_ms", &self.finalized_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RemoteInspectionSnapshotRecord {
    pub(crate) snapshot_id: String,
    pub(crate) consent_id: String,
    pub(crate) attempt_id: String,
    pub(crate) candidate_id: String,
    pub(crate) candidate_revision: i64,
    pub(crate) source_facet: IntakeSourceFacet,
    pub(crate) transport: IntakeTransport,
    pub(crate) purpose: RemoteInspectionPurpose,
    pub(crate) policy_revision: i64,
    pub(crate) authority_provider: RemoteInspectionAuthorityProvider,
    pub(crate) provider_key_epoch: i64,
    pub(crate) generation: i64,
    pub(crate) has_authority_record_ref: bool,
    pub(crate) safe_subcode: RemoteInspectionSafeSubcode,
    pub(crate) safe_summary: RemoteInspectionSafeSummary,
    pub(crate) fail_closed: bool,
    pub(crate) created_at_ms: i64,
}

impl fmt::Debug for RemoteInspectionSnapshotRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteInspectionSnapshotRecord")
            .field("snapshot_id", &self.snapshot_id)
            .field("consent_id", &self.consent_id)
            .field("attempt_id", &self.attempt_id)
            .field("candidate_id", &self.candidate_id)
            .field("candidate_revision", &self.candidate_revision)
            .field("source_facet", &self.source_facet)
            .field("transport", &self.transport)
            .field("purpose", &self.purpose)
            .field("policy_revision", &self.policy_revision)
            .field("authority_provider", &self.authority_provider)
            .field("provider_key_epoch", &self.provider_key_epoch)
            .field("generation", &self.generation)
            .field("has_authority_record_ref", &self.has_authority_record_ref)
            .field("safe_subcode", &self.safe_subcode)
            .field("safe_summary", &self.safe_summary)
            .field("fail_closed", &self.fail_closed)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteInspectionConsumptionRecord {
    pub(crate) consent_id: String,
    pub(crate) attempt_id: String,
    pub(crate) snapshot_id: String,
    pub(crate) candidate_id: String,
    pub(crate) candidate_revision: i64,
    pub(crate) created_at_ms: i64,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CanonicalManualHttpsTarget {
    canonical: String,
    host_ascii: String,
    port: u16,
    path: String,
}

impl CanonicalManualHttpsTarget {
    pub(crate) fn as_str(&self) -> &str {
        &self.canonical
    }

    pub(crate) fn host_ascii(&self) -> &str {
        &self.host_ascii
    }

    pub(crate) const fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Debug for CanonicalManualHttpsTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalManualHttpsTarget")
            .field("canonical", &"[REDACTED]")
            .field("host_ascii", &"[REDACTED]")
            .field("port", &self.port)
            .field("path", &"[REDACTED]")
            .finish()
    }
}

pub(crate) fn trusted_remote_inspection_policy_revision(
    purpose: RemoteInspectionPurpose,
) -> McpPlatformResult<i64> {
    match purpose {
        RemoteInspectionPurpose::PrivateRemoteCheckV26 => Ok(REMOTE_INSPECTION_POLICY_REVISION),
    }
}

pub(crate) fn claim_remote_inspection_attempt(
    attempt_id: &str,
    consent_id: &str,
    claim_nonce: &str,
    claimed_at_ms: i64,
    claim_expires_at_ms: i64,
) -> RemoteInspectionResult<RemoteInspectionAttemptRecord> {
    validate_opaque_value(
        attempt_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    validate_opaque_value(
        consent_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    validate_opaque_value(
        claim_nonce,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    if claimed_at_ms < 0 || claim_expires_at_ms <= claimed_at_ms {
        return Err(integrity_failure(
            RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
        ));
    }
    Ok(RemoteInspectionAttemptRecord {
        attempt_id: attempt_id.to_string(),
        consent_id: consent_id.to_string(),
        state: RemoteInspectionAttemptState::Claimed,
        claim_nonce: claim_nonce.to_string(),
        claimed_at_ms,
        claim_expires_at_ms,
        owner_started_at_ms: None,
        owner_finished_at_ms: None,
        finalized_at_ms: None,
    })
}

pub(crate) fn recover_remote_inspection_attempt_stale_claim(
    current: &RemoteInspectionAttemptRecord,
    now_ms: i64,
) -> RemoteInspectionResult<RemoteInspectionAttemptRecord> {
    if current.state != RemoteInspectionAttemptState::Claimed
        || current.owner_started_at_ms.is_some()
        || current.owner_finished_at_ms.is_some()
        || current.finalized_at_ms.is_some()
        || now_ms < current.claim_expires_at_ms
    {
        return Err(attempt_failure(
            RemoteInspectionSafeSubcode::InvalidAttemptTransition,
        ));
    }
    let mut next = current.clone();
    next.state = RemoteInspectionAttemptState::Finalized;
    next.finalized_at_ms = Some(now_ms);
    Ok(next)
}

pub(crate) fn advance_remote_inspection_attempt(
    current: &RemoteInspectionAttemptRecord,
    claim_nonce: &str,
    next_state: RemoteInspectionAttemptState,
    now_ms: i64,
) -> RemoteInspectionResult<RemoteInspectionAttemptRecord> {
    if current.claim_nonce != claim_nonce {
        return Err(attempt_failure(
            RemoteInspectionSafeSubcode::ClaimNonceMismatch,
        ));
    }
    if now_ms < current.claimed_at_ms {
        return Err(integrity_failure(
            RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
        ));
    }
    match (current.state, next_state) {
        (RemoteInspectionAttemptState::Claimed, RemoteInspectionAttemptState::OwnerStarted) => {
            if now_ms >= current.claim_expires_at_ms {
                return Err(attempt_failure(
                    RemoteInspectionSafeSubcode::InvalidAttemptTransition,
                ));
            }
            let mut next = current.clone();
            next.state = RemoteInspectionAttemptState::OwnerStarted;
            next.owner_started_at_ms = Some(now_ms);
            Ok(next)
        }
        (
            RemoteInspectionAttemptState::OwnerStarted,
            RemoteInspectionAttemptState::OwnerFinished,
        ) if current.owner_started_at_ms.is_some() => {
            if now_ms < current.owner_started_at_ms.expect("guarded by is_some") {
                return Err(integrity_failure(
                    RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
                ));
            }
            let mut next = current.clone();
            next.state = RemoteInspectionAttemptState::OwnerFinished;
            next.owner_finished_at_ms = Some(now_ms);
            Ok(next)
        }
        (RemoteInspectionAttemptState::OwnerFinished, RemoteInspectionAttemptState::Finalized)
            if current.owner_finished_at_ms.is_some() =>
        {
            if now_ms < current.owner_finished_at_ms.expect("guarded by is_some") {
                return Err(integrity_failure(
                    RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
                ));
            }
            let mut next = current.clone();
            next.state = RemoteInspectionAttemptState::Finalized;
            next.finalized_at_ms = Some(now_ms);
            Ok(next)
        }
        _ => Err(attempt_failure(
            RemoteInspectionSafeSubcode::InvalidAttemptTransition,
        )),
    }
}

pub(crate) fn validate_remote_inspection_reservation(
    record: &RemoteInspectionReservationRecord,
) -> McpPlatformResult<()> {
    validate_opaque_value(
        &record.reservation_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )
    .map_err(|failure| failure.as_mcp_error())?;
    validate_opaque_value(
        &record.candidate_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )
    .map_err(|failure| failure.as_mcp_error())?;
    validate_opaque_value(
        &record.intent_fingerprint,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )
    .map_err(|failure| failure.as_mcp_error())?;
    validate_opaque_value(
        &record.intent_fingerprint_key_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )
    .map_err(|failure| failure.as_mcp_error())?;
    validate_supported_remote_shape(record.source_facet, record.transport).map_err(|_| {
        integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict).as_mcp_error()
    })?;
    if record.purpose
        != RemoteInspectionPurpose::parse(record.purpose.as_str()).ok_or_else(|| {
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict).as_mcp_error()
        })?
    {
        return Err(
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                .as_mcp_error(),
        );
    }
    if record.purpose
        != validate_supported_remote_shape(record.source_facet, record.transport).map_err(|_| {
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict).as_mcp_error()
        })?
        || record.policy_revision
            != trusted_remote_inspection_policy_revision(record.purpose).map_err(|_| {
                integrity_failure(RemoteInspectionSafeSubcode::PolicyRevisionDrift).as_mcp_error()
            })?
        || record.candidate_revision <= 0
        || record.generation <= 0
        || record.last_event_ordinal <= 0
        || record.created_at_ms < 0
        || record.updated_at_ms < record.created_at_ms
    {
        return Err(
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                .as_mcp_error(),
        );
    }
    if let Some(provider) = record.authority_provider {
        if record.authority_key_epoch.unwrap_or_default() <= 0
            || record.authority_generation.unwrap_or_default() <= 0
            || !record.has_authority_record_ref
        {
            return Err(
                integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                    .as_mcp_error(),
            );
        }
        let _ = provider;
    } else if record.authority_key_epoch.is_some()
        || record.authority_generation.is_some()
        || record.has_authority_record_ref
    {
        return Err(
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                .as_mcp_error(),
        );
    }
    if record.no_live_proof_kind.is_some() != record.no_live_proof_observed_at_ms.is_some() {
        return Err(
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                .as_mcp_error(),
        );
    }
    if let Some(observed_at_ms) = record.no_live_proof_observed_at_ms {
        if observed_at_ms < 0 {
            return Err(
                integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                    .as_mcp_error(),
            );
        }
    }
    match record.state {
        RemoteInspectionReservationState::PrebindTombstonedTerminal => {
            if record.consent_id.is_some()
                || record.claim_attempt_id.is_some()
                || record.no_live_proof_kind != Some(RemoteInspectionNoLiveProofKind::Tombstoned)
            {
                return Err(integrity_failure(
                    RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
                )
                .as_mcp_error());
            }
        }
        RemoteInspectionReservationState::PostbindBlockedTerminal => {
            if record.no_live_proof_kind
                != Some(RemoteInspectionNoLiveProofKind::DefinitiveNoLivePostbind)
            {
                return Err(integrity_failure(
                    RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
                )
                .as_mcp_error());
            }
        }
        _ if record.no_live_proof_kind.is_some() => {
            return Err(
                integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                    .as_mcp_error(),
            );
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn validate_remote_inspection_consent_record(
    record: &RemoteInspectionConsentRecord,
) -> McpPlatformResult<()> {
    validate_opaque_value(
        &record.consent_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )
    .map_err(|failure| failure.as_mcp_error())?;
    validate_opaque_value(
        &record.candidate_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )
    .map_err(|failure| failure.as_mcp_error())?;
    if record.candidate_revision <= 0
        || record.policy_revision
            != trusted_remote_inspection_policy_revision(record.purpose).map_err(|_| {
                integrity_failure(RemoteInspectionSafeSubcode::PolicyRevisionDrift).as_mcp_error()
            })?
        || record.expires_at_ms <= record.created_at_ms
        || record.created_at_ms < 0
        || record.provider_key_epoch <= 0
        || record.generation <= 0
        || !record.has_authority_record_ref
    {
        return Err(
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                .as_mcp_error(),
        );
    }
    if record.purpose
        != validate_supported_remote_shape(record.source_facet, record.transport).map_err(|_| {
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict).as_mcp_error()
        })?
    {
        return Err(
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                .as_mcp_error(),
        );
    }
    Ok(())
}

pub(crate) fn validate_remote_inspection_attempt_record(
    record: &RemoteInspectionAttemptRecord,
) -> McpPlatformResult<()> {
    let claimed = claim_remote_inspection_attempt(
        &record.attempt_id,
        &record.consent_id,
        &record.claim_nonce,
        record.claimed_at_ms,
        record.claim_expires_at_ms,
    )
    .map_err(|failure| failure.as_mcp_error())?;
    let candidate = match record.state {
        RemoteInspectionAttemptState::Claimed => claimed,
        RemoteInspectionAttemptState::OwnerStarted => advance_remote_inspection_attempt(
            &claimed,
            &record.claim_nonce,
            RemoteInspectionAttemptState::OwnerStarted,
            record.owner_started_at_ms.ok_or_else(|| {
                integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                    .as_mcp_error()
            })?,
        )
        .map_err(|failure| failure.as_mcp_error())?,
        RemoteInspectionAttemptState::OwnerFinished => {
            let started = advance_remote_inspection_attempt(
                &claimed,
                &record.claim_nonce,
                RemoteInspectionAttemptState::OwnerStarted,
                record.owner_started_at_ms.ok_or_else(|| {
                    integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                        .as_mcp_error()
                })?,
            )
            .map_err(|failure| failure.as_mcp_error())?;
            advance_remote_inspection_attempt(
                &started,
                &record.claim_nonce,
                RemoteInspectionAttemptState::OwnerFinished,
                record.owner_finished_at_ms.ok_or_else(|| {
                    integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                        .as_mcp_error()
                })?,
            )
            .map_err(|failure| failure.as_mcp_error())?
        }
        RemoteInspectionAttemptState::Finalized => {
            if record.owner_started_at_ms.is_none() {
                recover_remote_inspection_attempt_stale_claim(
                    &claimed,
                    record.finalized_at_ms.ok_or_else(|| {
                        integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                            .as_mcp_error()
                    })?,
                )
                .map_err(|failure| failure.as_mcp_error())?
            } else {
                let started = advance_remote_inspection_attempt(
                    &claimed,
                    &record.claim_nonce,
                    RemoteInspectionAttemptState::OwnerStarted,
                    record.owner_started_at_ms.ok_or_else(|| {
                        integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                            .as_mcp_error()
                    })?,
                )
                .map_err(|failure| failure.as_mcp_error())?;
                let finished = advance_remote_inspection_attempt(
                    &started,
                    &record.claim_nonce,
                    RemoteInspectionAttemptState::OwnerFinished,
                    record.owner_finished_at_ms.ok_or_else(|| {
                        integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                            .as_mcp_error()
                    })?,
                )
                .map_err(|failure| failure.as_mcp_error())?;
                advance_remote_inspection_attempt(
                    &finished,
                    &record.claim_nonce,
                    RemoteInspectionAttemptState::Finalized,
                    record.finalized_at_ms.ok_or_else(|| {
                        integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                            .as_mcp_error()
                    })?,
                )
                .map_err(|failure| failure.as_mcp_error())?
            }
        }
    };
    if candidate != *record {
        return Err(
            integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
                .as_mcp_error(),
        );
    }
    Ok(())
}

pub(crate) fn validate_remote_inspection_snapshot(
    snapshot: &RemoteInspectionSnapshotRecord,
) -> RemoteInspectionResult<()> {
    validate_opaque_value(
        &snapshot.snapshot_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    validate_opaque_value(
        &snapshot.consent_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    validate_opaque_value(
        &snapshot.attempt_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    validate_opaque_value(
        &snapshot.candidate_id,
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    if snapshot.candidate_revision <= 0
        || snapshot.policy_revision
            != trusted_remote_inspection_policy_revision(snapshot.purpose).map_err(|_| {
                integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict)
            })?
        || snapshot.provider_key_epoch <= 0
        || snapshot.generation <= 0
        || snapshot.created_at_ms < 0
        || !snapshot.fail_closed
        || !snapshot.has_authority_record_ref
        || snapshot.safe_subcode.summary() != snapshot.safe_summary
    {
        return Err(integrity_failure(
            RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
        ));
    }
    if snapshot.purpose
        != validate_supported_remote_shape(snapshot.source_facet, snapshot.transport).map_err(
            |_| integrity_failure(RemoteInspectionSafeSubcode::SnapshotIntegrityConflict),
        )?
    {
        return Err(integrity_failure(
            RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
        ));
    }
    Ok(())
}

pub(crate) fn canonicalize_manual_https_target(
    raw_target: &str,
) -> RemoteInspectionResult<CanonicalManualHttpsTarget> {
    if raw_target.is_empty() {
        return Err(target_failure(RemoteInspectionSafeSubcode::EmptyTarget));
    }
    if raw_target.chars().any(char::is_control) {
        return Err(target_failure(
            RemoteInspectionSafeSubcode::ControlCharacterRejected,
        ));
    }
    if raw_target.contains('\\') {
        return Err(target_failure(RemoteInspectionSafeSubcode::InvalidUrl));
    }
    let parsed = Url::parse(raw_target)
        .map_err(|_| target_failure(RemoteInspectionSafeSubcode::InvalidUrl))?;
    if parsed.scheme() != "https" {
        return Err(target_failure(RemoteInspectionSafeSubcode::HttpsRequired));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(target_failure(
            RemoteInspectionSafeSubcode::UserInfoRejected,
        ));
    }
    if parsed.fragment().is_some() {
        return Err(target_failure(
            RemoteInspectionSafeSubcode::FragmentRejected,
        ));
    }
    if parsed.query().is_some() {
        return Err(target_failure(RemoteInspectionSafeSubcode::QueryRejected));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| target_failure(RemoteInspectionSafeSubcode::HostMissing))?;
    let trimmed_host = host.trim_end_matches('.');
    if trimmed_host.is_empty() {
        return Err(target_failure(RemoteInspectionSafeSubcode::HostMissing));
    }
    let ascii_host = match Host::parse(trimmed_host)
        .map_err(|_| target_failure(RemoteInspectionSafeSubcode::InvalidUrl))?
    {
        Host::Domain(domain) => domain,
        Host::Ipv4(_) | Host::Ipv6(_) => {
            return Err(target_failure(
                RemoteInspectionSafeSubcode::IpLiteralRejected,
            ));
        }
    };
    if !ascii_host.contains('.') {
        return Err(target_failure(
            RemoteInspectionSafeSubcode::HostMustContainDot,
        ));
    }
    let port = match parsed.port() {
        None | Some(443) => 443,
        Some(8443) => 8443,
        Some(_) => return Err(target_failure(RemoteInspectionSafeSubcode::PortNotAllowed)),
    };
    let path = canonicalize_path(raw_path_from_target(raw_target)?)
        .map_err(|_| target_failure(RemoteInspectionSafeSubcode::InvalidUrl))?;
    let canonical = if port == 8443 {
        format!("https://{ascii_host}:8443{path}")
    } else {
        format!("https://{ascii_host}{path}")
    };
    Ok(CanonicalManualHttpsTarget {
        canonical,
        host_ascii: ascii_host,
        port,
        path,
    })
}

fn validate_supported_remote_shape(
    source_facet: IntakeSourceFacet,
    transport: IntakeTransport,
) -> Result<RemoteInspectionPurpose, RemoteInspectionGateError> {
    RemoteInspectionPurpose::derive(source_facet, transport)
}

fn validate_authority_verified_tuple(
    tuple: &AuthorityVerifiedOpaqueTuple,
) -> RemoteInspectionResult<()> {
    validate_opaque_value(
        tuple.target_handle().as_str(),
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    validate_opaque_value(
        tuple.target_identity_binding().as_str(),
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    validate_opaque_value(
        tuple.authority_record_ref().as_str(),
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    if tuple.provider_key_epoch() <= 0 || tuple.generation() <= 0 {
        return Err(integrity_failure(
            RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
        ));
    }
    Ok(())
}

fn validate_verified_no_live_proof(proof: &VerifiedNoLiveProof) -> RemoteInspectionResult<()> {
    validate_opaque_value(
        proof.proof_id(),
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    validate_opaque_value(
        proof.proof_hmac(),
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    validate_opaque_value(
        proof.authority_record_ref().as_str(),
        RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
    )?;
    if proof.proof_key_epoch() <= 0 || proof.observed_at_ms() < 0 {
        return Err(integrity_failure(
            RemoteInspectionSafeSubcode::SnapshotIntegrityConflict,
        ));
    }
    Ok(())
}

pub(crate) fn commit_state_for_outcome(
    outcome: &AuthorityCommitOutcome,
) -> Option<RemoteInspectionCommitState> {
    if outcome.is_committed() {
        Some(RemoteInspectionCommitState::Committed)
    } else if outcome.is_reconcile_required() {
        None
    } else if outcome.is_blocked_no_live() {
        Some(RemoteInspectionCommitState::BlockedNoLive)
    } else {
        None
    }
}

pub(crate) fn proof_for_outcome(outcome: &AuthorityCommitOutcome) -> Option<&VerifiedNoLiveProof> {
    outcome.blocked_proof()
}

fn raw_path_from_target(raw_target: &str) -> RemoteInspectionResult<&str> {
    let authority_start = raw_target
        .find("://")
        .ok_or_else(|| target_failure(RemoteInspectionSafeSubcode::InvalidUrl))?
        + 3;
    let after_scheme = &raw_target[authority_start..];
    let path_start = after_scheme
        .find(&['/', '?', '#'][..])
        .unwrap_or(after_scheme.len());
    Ok(if path_start == after_scheme.len() {
        ""
    } else {
        &after_scheme[path_start..]
    })
}

fn canonicalize_path(path: &str) -> Result<String, ()> {
    if path.is_empty() {
        return Ok("/".to_string());
    }
    if !path.starts_with('/')
        || path.contains("//")
        || path.contains("/./")
        || path.contains("/../")
        || path.ends_with("/.")
        || path.ends_with("/..")
    {
        return Err(());
    }
    Ok(path.to_string())
}

fn validate_opaque_value(
    value: &str,
    subcode: RemoteInspectionSafeSubcode,
) -> RemoteInspectionResult<()> {
    if value.is_empty()
        || value.len() > MAX_OPAQUE_VALUE_LEN
        || value.chars().any(char::is_whitespace)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(integrity_failure(subcode));
    }
    Ok(())
}

fn target_failure(subcode: RemoteInspectionSafeSubcode) -> RemoteInspectionFailure {
    debug_assert_eq!(
        subcode.summary(),
        RemoteInspectionSafeSummary::TargetRejected
    );
    RemoteInspectionFailure {
        code: McpPlatformErrorCode::InvalidRequest,
        subcode,
        safe_summary: subcode.summary(),
    }
}

fn attempt_failure(subcode: RemoteInspectionSafeSubcode) -> RemoteInspectionFailure {
    debug_assert_eq!(
        subcode.summary(),
        RemoteInspectionSafeSummary::AttemptRejected
    );
    RemoteInspectionFailure {
        code: McpPlatformErrorCode::CandidateStateConflict,
        subcode,
        safe_summary: subcode.summary(),
    }
}

fn integrity_failure(subcode: RemoteInspectionSafeSubcode) -> RemoteInspectionFailure {
    RemoteInspectionFailure {
        code: McpPlatformErrorCode::IntegrityError,
        subcode,
        safe_summary: subcode.summary(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_platform::credential_authority::RemoteInspectionTargetAuthority;

    #[test]
    fn remote_transport_derivation_rejects_catalog_fail_closed() {
        let error = RemoteInspectionPurpose::derive(
            IntakeSourceFacet::CatalogPlanning,
            IntakeTransport::CatalogReference,
        )
        .unwrap_err();
        assert_eq!(error, RemoteInspectionGateError::TransportUnsupported);
        assert_eq!(
            error.as_mcp_error().code(),
            McpPlatformErrorCode::OperationNotSupported
        );
        assert_eq!(
            error.subcode(),
            REMOTE_INSPECTION_TRANSPORT_UNSUPPORTED_SUBCODE
        );
    }

    #[test]
    fn canonical_target_preserves_canonical_https_path_bytes() {
        let target = canonicalize_manual_https_target("https://ExAmPle.com.:8443/a/b").unwrap();
        assert_eq!(target.as_str(), "https://example.com:8443/a/b");
        assert_eq!(target.host_ascii(), "example.com");
        assert_eq!(target.port(), 8443);
        assert_eq!(target.path(), "/a/b");
    }

    #[test]
    fn canonical_target_rejects_invalid_inputs_with_stable_subcodes() {
        let cases = [
            ("", RemoteInspectionSafeSubcode::EmptyTarget),
            (
                "http://example.com",
                RemoteInspectionSafeSubcode::HttpsRequired,
            ),
            (
                "https://127.0.0.1",
                RemoteInspectionSafeSubcode::IpLiteralRejected,
            ),
            (
                "https://user@example.com",
                RemoteInspectionSafeSubcode::UserInfoRejected,
            ),
            (
                "https://example.com/path?x=1",
                RemoteInspectionSafeSubcode::QueryRejected,
            ),
            (
                "https://example.com/path#frag",
                RemoteInspectionSafeSubcode::FragmentRejected,
            ),
            (
                "https://example/path",
                RemoteInspectionSafeSubcode::HostMustContainDot,
            ),
            (
                "https://example.com:9443",
                RemoteInspectionSafeSubcode::PortNotAllowed,
            ),
            (
                "https://example.com/a/../b//",
                RemoteInspectionSafeSubcode::InvalidUrl,
            ),
            (
                "https://example.com/./a",
                RemoteInspectionSafeSubcode::InvalidUrl,
            ),
            (
                "https://example.com/../a",
                RemoteInspectionSafeSubcode::InvalidUrl,
            ),
            (
                "https://example.com/a\\b",
                RemoteInspectionSafeSubcode::InvalidUrl,
            ),
        ];
        for (target, subcode) in cases {
            let failure = canonicalize_manual_https_target(target).unwrap_err();
            assert_eq!(failure.code(), McpPlatformErrorCode::InvalidRequest);
            assert_eq!(failure.subcode, subcode);
            assert_eq!(
                failure.safe_summary,
                RemoteInspectionSafeSummary::TargetRejected
            );
        }
    }

    #[test]
    fn attempt_state_machine_only_allows_forward_transitions() {
        let claimed =
            claim_remote_inspection_attempt("attempt_1", "consent_1", "nonce_1", 10, 20).unwrap();
        let started = advance_remote_inspection_attempt(
            &claimed,
            "nonce_1",
            RemoteInspectionAttemptState::OwnerStarted,
            11,
        )
        .unwrap();
        let finished = advance_remote_inspection_attempt(
            &started,
            "nonce_1",
            RemoteInspectionAttemptState::OwnerFinished,
            12,
        )
        .unwrap();
        let finalized = advance_remote_inspection_attempt(
            &finished,
            "nonce_1",
            RemoteInspectionAttemptState::Finalized,
            13,
        )
        .unwrap();
        assert_eq!(finalized.state, RemoteInspectionAttemptState::Finalized);
    }

    #[test]
    fn stale_claim_recovery_only_allows_expired_unstarted_attempts() {
        let claimed =
            claim_remote_inspection_attempt("attempt_2", "consent_2", "nonce_2", 10, 15).unwrap();
        let active = recover_remote_inspection_attempt_stale_claim(&claimed, 14).unwrap_err();
        assert_eq!(
            active.subcode,
            RemoteInspectionSafeSubcode::InvalidAttemptTransition
        );

        let recovered = recover_remote_inspection_attempt_stale_claim(&claimed, 15).unwrap();
        assert_eq!(recovered.state, RemoteInspectionAttemptState::Finalized);
        assert_eq!(recovered.finalized_at_ms, Some(15));
    }

    #[test]
    fn authority_outputs_only_constructible_through_authority() {
        let authority = RemoteInspectionTargetAuthority::in_memory_for_testing();
        let tuple = authority.mint_verified_opaque_tuple();
        let proof = authority.verify_no_live_proof(
            &tuple,
            "reservation_1",
            1,
            "intent_1",
            RemoteInspectionNoLiveProofKind::DefinitiveNoLivePostbind,
            10,
        );
        assert_eq!(
            tuple.authority_provider(),
            RemoteInspectionAuthorityProvider::InMemoryTest
        );
        assert_eq!(
            proof.kind(),
            RemoteInspectionNoLiveProofKind::DefinitiveNoLivePostbind
        );
    }
}
