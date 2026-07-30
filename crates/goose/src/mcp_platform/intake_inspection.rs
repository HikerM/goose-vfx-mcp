use std::fmt;

use serde::{Deserialize, Serialize};

use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::intake::{
    IntakeCandidateRecord, IntakeLifecycleState, IntakeSourceFacet, IntakeTransport,
};

use super::intake_inspection_network::no_network_remote_observation;

pub(crate) const BATCH1_INSPECTION_POLICY_REVISION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InspectionPurpose {
    RemoteCandidateBoundary,
    ApprovedStdioLocalMetadata,
}

impl InspectionPurpose {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::RemoteCandidateBoundary => "remote_candidate_boundary",
            Self::ApprovedStdioLocalMetadata => "approved_stdio_local_metadata",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "remote_candidate_boundary" => Some(Self::RemoteCandidateBoundary),
            "approved_stdio_local_metadata" => Some(Self::ApprovedStdioLocalMetadata),
            _ => None,
        }
    }

    pub(crate) fn derive(
        source_facet: IntakeSourceFacet,
        transport: IntakeTransport,
    ) -> McpPlatformResult<Self> {
        match (source_facet, transport) {
            (IntakeSourceFacet::CatalogPlanning, IntakeTransport::CatalogReference)
            | (IntakeSourceFacet::ManualHttpsCandidate, IntakeTransport::StreamableHttp) => {
                Ok(Self::RemoteCandidateBoundary)
            }
            (IntakeSourceFacet::ApprovedStdioCandidate, IntakeTransport::Stdio) => {
                Ok(Self::ApprovedStdioLocalMetadata)
            }
            (IntakeSourceFacet::LegacyQuarantine, IntakeTransport::Legacy) => {
                Err(McpPlatformError::new(
                    McpPlatformErrorCode::CandidateStateConflict,
                    "legacy quarantine cannot enter batch1 inspection",
                ))
            }
            _ => Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "candidate source facet and transport cannot derive a trusted inspection purpose",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InspectionLifecycle {
    ConsentGranted,
    ConsentConsumed,
}

impl InspectionLifecycle {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ConsentGranted => "consent_granted",
            Self::ConsentConsumed => "consent_consumed",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "consent_granted" => Some(Self::ConsentGranted),
            "consent_consumed" => Some(Self::ConsentConsumed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InspectionState {
    InspectionBlocked,
    InspectionRecorded,
}

impl InspectionState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::InspectionBlocked => "inspection_blocked",
            Self::InspectionRecorded => "inspection_recorded",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "inspection_blocked" => Some(Self::InspectionBlocked),
            "inspection_recorded" => Some(Self::InspectionRecorded),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservationSurface {
    CandidateMetadata,
    ApprovedStdioLocalMetadata,
}

impl ObservationSurface {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CandidateMetadata => "candidate_metadata",
            Self::ApprovedStdioLocalMetadata => "approved_stdio_local_metadata",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "candidate_metadata" => Some(Self::CandidateMetadata),
            "approved_stdio_local_metadata" => Some(Self::ApprovedStdioLocalMetadata),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperationPhase {
    Batch1ZeroEgress,
    Batch1ReadOnlyLocalMetadata,
}

impl OperationPhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Batch1ZeroEgress => "batch1_zero_egress",
            Self::Batch1ReadOnlyLocalMetadata => "batch1_read_only_local_metadata",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "batch1_zero_egress" => Some(Self::Batch1ZeroEgress),
            "batch1_read_only_local_metadata" => Some(Self::Batch1ReadOnlyLocalMetadata),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InspectionFailureFamily {
    PolicyDrift,
    IdentityConflict,
    CandidateStateConflict,
    StdioDisallowed,
    TransportExecution,
    TransportConfiguration,
}

impl InspectionFailureFamily {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PolicyDrift => "policy_drift",
            Self::IdentityConflict => "identity_conflict",
            Self::CandidateStateConflict => "candidate_state_conflict",
            Self::StdioDisallowed => "stdio_disallowed",
            Self::TransportExecution => "transport_execution",
            Self::TransportConfiguration => "transport_configuration",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "policy_drift" => Some(Self::PolicyDrift),
            "identity_conflict" => Some(Self::IdentityConflict),
            "candidate_state_conflict" => Some(Self::CandidateStateConflict),
            "stdio_disallowed" => Some(Self::StdioDisallowed),
            "transport_execution" => Some(Self::TransportExecution),
            "transport_configuration" => Some(Self::TransportConfiguration),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InspectionConflictCode {
    PolicyRevisionDrift,
    CandidateIdentityConflict,
    CandidateRevisionDrift,
    CandidateLifecycleDrift,
    CandidateFacetDrift,
    CandidateTransportDrift,
    CandidatePurposeDrift,
    ConsentBindingDrift,
    ConsentExpired,
    ConsentConsumed,
    StdioBindingUnavailable,
    NetworkExecutionUnavailable,
    TransportPurposeMismatch,
    ClassificationUnavailable,
}

impl InspectionConflictCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PolicyRevisionDrift => "policy_revision_drift",
            Self::CandidateIdentityConflict => "candidate_identity_conflict",
            Self::CandidateRevisionDrift => "candidate_revision_drift",
            Self::CandidateLifecycleDrift => "candidate_lifecycle_drift",
            Self::CandidateFacetDrift => "candidate_facet_drift",
            Self::CandidateTransportDrift => "candidate_transport_drift",
            Self::CandidatePurposeDrift => "candidate_purpose_drift",
            Self::ConsentBindingDrift => "consent_binding_drift",
            Self::ConsentExpired => "consent_expired",
            Self::ConsentConsumed => "consent_consumed",
            Self::StdioBindingUnavailable => "stdio_binding_unavailable",
            Self::NetworkExecutionUnavailable => "network_execution_unavailable",
            Self::TransportPurposeMismatch => "transport_purpose_mismatch",
            Self::ClassificationUnavailable => "classification_unavailable",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "policy_revision_drift" => Some(Self::PolicyRevisionDrift),
            "candidate_identity_conflict" => Some(Self::CandidateIdentityConflict),
            "candidate_revision_drift" => Some(Self::CandidateRevisionDrift),
            "candidate_lifecycle_drift" => Some(Self::CandidateLifecycleDrift),
            "candidate_facet_drift" => Some(Self::CandidateFacetDrift),
            "candidate_transport_drift" => Some(Self::CandidateTransportDrift),
            "candidate_purpose_drift" => Some(Self::CandidatePurposeDrift),
            "consent_binding_drift" => Some(Self::ConsentBindingDrift),
            "consent_expired" => Some(Self::ConsentExpired),
            "consent_consumed" => Some(Self::ConsentConsumed),
            "stdio_binding_unavailable" => Some(Self::StdioBindingUnavailable),
            "network_execution_unavailable" => Some(Self::NetworkExecutionUnavailable),
            "transport_purpose_mismatch" => Some(Self::TransportPurposeMismatch),
            "classification_unavailable" => Some(Self::ClassificationUnavailable),
            _ => None,
        }
    }

    const fn priority(self) -> usize {
        match self {
            Self::PolicyRevisionDrift => 0,
            Self::CandidateIdentityConflict => 1,
            Self::CandidateRevisionDrift
            | Self::CandidateLifecycleDrift
            | Self::CandidateFacetDrift
            | Self::CandidateTransportDrift
            | Self::CandidatePurposeDrift
            | Self::ConsentBindingDrift
            | Self::ConsentExpired
            | Self::ConsentConsumed => 2,
            Self::StdioBindingUnavailable => 3,
            Self::NetworkExecutionUnavailable | Self::TransportPurposeMismatch => 4,
            Self::ClassificationUnavailable => usize::MAX,
        }
    }

    const fn family(self) -> Option<InspectionFailureFamily> {
        match self {
            Self::PolicyRevisionDrift => Some(InspectionFailureFamily::PolicyDrift),
            Self::CandidateIdentityConflict => Some(InspectionFailureFamily::IdentityConflict),
            Self::CandidateRevisionDrift
            | Self::CandidateLifecycleDrift
            | Self::CandidateFacetDrift
            | Self::CandidateTransportDrift
            | Self::CandidatePurposeDrift
            | Self::ConsentBindingDrift
            | Self::ConsentExpired
            | Self::ConsentConsumed => Some(InspectionFailureFamily::CandidateStateConflict),
            Self::StdioBindingUnavailable => Some(InspectionFailureFamily::StdioDisallowed),
            Self::NetworkExecutionUnavailable => Some(InspectionFailureFamily::TransportExecution),
            Self::TransportPurposeMismatch => Some(InspectionFailureFamily::TransportConfiguration),
            Self::ClassificationUnavailable => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InspectionSummaryCode {
    NetworkExecutionUnavailable,
    LocalMetadataOnly,
    PolicyDrift,
    IdentityConflict,
    CandidateStateConflict,
    StdioDisallowed,
    TransportConfiguration,
    ClassificationUnavailable,
}

impl InspectionSummaryCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NetworkExecutionUnavailable => "network_execution_unavailable",
            Self::LocalMetadataOnly => "local_metadata_only",
            Self::PolicyDrift => "policy_drift",
            Self::IdentityConflict => "identity_conflict",
            Self::CandidateStateConflict => "candidate_state_conflict",
            Self::StdioDisallowed => "stdio_disallowed",
            Self::TransportConfiguration => "transport_configuration",
            Self::ClassificationUnavailable => "classification_unavailable",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "network_execution_unavailable" => Some(Self::NetworkExecutionUnavailable),
            "local_metadata_only" => Some(Self::LocalMetadataOnly),
            "policy_drift" => Some(Self::PolicyDrift),
            "identity_conflict" => Some(Self::IdentityConflict),
            "candidate_state_conflict" => Some(Self::CandidateStateConflict),
            "stdio_disallowed" => Some(Self::StdioDisallowed),
            "transport_configuration" => Some(Self::TransportConfiguration),
            "classification_unavailable" => Some(Self::ClassificationUnavailable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InspectionFactCode {
    CandidateMetadataObserved,
    ApprovedStdioLocalMetadataObserved,
    RemoteExecutionUnavailable,
    ProviderMetadataUnavailable,
    PayloadUnavailable,
    BindingUnavailable,
    ManifestUnavailable,
}

impl InspectionFactCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CandidateMetadataObserved => "candidate_metadata_observed",
            Self::ApprovedStdioLocalMetadataObserved => "approved_stdio_local_metadata_observed",
            Self::RemoteExecutionUnavailable => "remote_execution_unavailable",
            Self::ProviderMetadataUnavailable => "provider_metadata_unavailable",
            Self::PayloadUnavailable => "payload_unavailable",
            Self::BindingUnavailable => "binding_unavailable",
            Self::ManifestUnavailable => "manifest_unavailable",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "candidate_metadata_observed" => Some(Self::CandidateMetadataObserved),
            "approved_stdio_local_metadata_observed" => {
                Some(Self::ApprovedStdioLocalMetadataObserved)
            }
            "remote_execution_unavailable" => Some(Self::RemoteExecutionUnavailable),
            "provider_metadata_unavailable" => Some(Self::ProviderMetadataUnavailable),
            "payload_unavailable" => Some(Self::PayloadUnavailable),
            "binding_unavailable" => Some(Self::BindingUnavailable),
            "manifest_unavailable" => Some(Self::ManifestUnavailable),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InspectionBindingTuple {
    pub(crate) candidate_revision: i64,
    pub(crate) source_facet: IntakeSourceFacet,
    pub(crate) transport: IntakeTransport,
    pub(crate) purpose: InspectionPurpose,
    pub(crate) consent_binding: String,
    pub(crate) consent_lifecycle: InspectionLifecycle,
    pub(crate) policy_revision: i64,
}

impl fmt::Debug for InspectionBindingTuple {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InspectionBindingTuple")
            .field("candidate_revision", &self.candidate_revision)
            .field("source_facet", &self.source_facet)
            .field("transport", &self.transport)
            .field("purpose", &self.purpose)
            .field("consent_binding", &"[REDACTED]")
            .field("consent_lifecycle", &self.consent_lifecycle)
            .field("policy_revision", &self.policy_revision)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InspectionConsentRecord {
    pub(crate) consent_id: String,
    pub(crate) candidate_id: String,
    pub(crate) candidate_lifecycle_state: IntakeLifecycleState,
    pub(crate) binding: InspectionBindingTuple,
    pub(crate) expires_at_ms: i64,
    pub(crate) created_at_ms: i64,
    pub(crate) consumed_at_ms: Option<i64>,
}

impl fmt::Debug for InspectionConsentRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InspectionConsentRecord")
            .field("consent_id", &self.consent_id)
            .field("candidate_id", &self.candidate_id)
            .field("candidate_lifecycle_state", &self.candidate_lifecycle_state)
            .field("binding", &self.binding)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("created_at_ms", &self.created_at_ms)
            .field("consumed_at_ms", &self.consumed_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SaveIntakeInspectionConsent {
    pub(crate) consent_id: String,
    pub(crate) candidate_id: String,
    pub(crate) candidate_lifecycle_state: IntakeLifecycleState,
    pub(crate) binding: InspectionBindingTuple,
    pub(crate) expires_at_ms: i64,
    pub(crate) created_at_ms: i64,
}

impl fmt::Debug for SaveIntakeInspectionConsent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SaveIntakeInspectionConsent")
            .field("consent_id", &self.consent_id)
            .field("candidate_id", &self.candidate_id)
            .field("candidate_lifecycle_state", &self.candidate_lifecycle_state)
            .field("binding", &self.binding)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InspectionSnapshot {
    pub(crate) snapshot_id: String,
    pub(crate) candidate_id: String,
    pub(crate) consent_id: String,
    pub(crate) binding: InspectionBindingTuple,
    pub(crate) inspection_state: InspectionState,
    pub(crate) observation_surface: ObservationSurface,
    pub(crate) operation_phase: OperationPhase,
    pub(crate) failure_family: Option<InspectionFailureFamily>,
    pub(crate) summary_code: InspectionSummaryCode,
    pub(crate) conflict_codes: Vec<InspectionConflictCode>,
    pub(crate) fact_codes: Vec<InspectionFactCode>,
    pub(crate) reusable: bool,
    pub(crate) created_at_ms: i64,
}

impl fmt::Debug for InspectionSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InspectionSnapshot")
            .field("snapshot_id", &self.snapshot_id)
            .field("candidate_id", &self.candidate_id)
            .field("consent_id", &self.consent_id)
            .field("binding", &self.binding)
            .field("inspection_state", &self.inspection_state)
            .field("observation_surface", &self.observation_surface)
            .field("operation_phase", &self.operation_phase)
            .field("failure_family", &self.failure_family)
            .field("summary_code", &self.summary_code)
            .field("conflict_codes", &self.conflict_codes)
            .field("fact_codes", &self.fact_codes)
            .field("reusable", &self.reusable)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

pub(crate) type StoredInspectionSnapshot = InspectionSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordIntakeInspectionSnapshot {
    pub(crate) snapshot: InspectionSnapshot,
    pub(crate) candidate_lifecycle_state: IntakeLifecycleState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClassifiedInspectionFailure {
    pub(crate) state: InspectionState,
    pub(crate) failure_family: Option<InspectionFailureFamily>,
    pub(crate) summary_code: InspectionSummaryCode,
    pub(crate) reusable: bool,
}

pub(crate) fn trusted_policy_revision_for(purpose: InspectionPurpose) -> McpPlatformResult<i64> {
    match purpose {
        InspectionPurpose::RemoteCandidateBoundary
        | InspectionPurpose::ApprovedStdioLocalMetadata => Ok(BATCH1_INSPECTION_POLICY_REVISION),
    }
}

pub(crate) fn batch1_consent_tuple(
    candidate: &IntakeCandidateRecord,
    consent_binding: String,
) -> McpPlatformResult<InspectionBindingTuple> {
    let purpose = InspectionPurpose::derive(candidate.source_facet, candidate.transport)?;
    Ok(InspectionBindingTuple {
        candidate_revision: candidate.revision,
        source_facet: candidate.source_facet,
        transport: candidate.transport,
        purpose,
        consent_binding,
        consent_lifecycle: InspectionLifecycle::ConsentGranted,
        policy_revision: trusted_policy_revision_for(purpose)?,
    })
}

pub(crate) fn classify_failure(
    conflict_codes: &[InspectionConflictCode],
) -> ClassifiedInspectionFailure {
    if conflict_codes.is_empty()
        || conflict_codes.contains(&InspectionConflictCode::ClassificationUnavailable)
    {
        return ClassifiedInspectionFailure {
            state: InspectionState::InspectionBlocked,
            failure_family: None,
            summary_code: InspectionSummaryCode::ClassificationUnavailable,
            reusable: false,
        };
    }

    let mut best: Option<(usize, InspectionFailureFamily)> = None;
    let mut ambiguous = false;
    for code in conflict_codes {
        let Some(family) = code.family() else {
            ambiguous = true;
            continue;
        };
        let priority = code.priority();
        match best {
            None => best = Some((priority, family)),
            Some((best_priority, best_family)) if priority < best_priority => {
                best = Some((priority, family));
                ambiguous = false;
            }
            Some((best_priority, best_family))
                if priority == best_priority && family != best_family =>
            {
                ambiguous = true;
            }
            _ => {}
        }
    }

    if ambiguous {
        return ClassifiedInspectionFailure {
            state: InspectionState::InspectionBlocked,
            failure_family: None,
            summary_code: InspectionSummaryCode::ClassificationUnavailable,
            reusable: false,
        };
    }

    let Some((_, family)) = best else {
        return ClassifiedInspectionFailure {
            state: InspectionState::InspectionBlocked,
            failure_family: None,
            summary_code: InspectionSummaryCode::ClassificationUnavailable,
            reusable: false,
        };
    };
    let summary_code = match family {
        InspectionFailureFamily::PolicyDrift => InspectionSummaryCode::PolicyDrift,
        InspectionFailureFamily::IdentityConflict => InspectionSummaryCode::IdentityConflict,
        InspectionFailureFamily::CandidateStateConflict => {
            InspectionSummaryCode::CandidateStateConflict
        }
        InspectionFailureFamily::StdioDisallowed => InspectionSummaryCode::StdioDisallowed,
        InspectionFailureFamily::TransportExecution => {
            InspectionSummaryCode::NetworkExecutionUnavailable
        }
        InspectionFailureFamily::TransportConfiguration => {
            InspectionSummaryCode::TransportConfiguration
        }
    };
    ClassifiedInspectionFailure {
        state: InspectionState::InspectionBlocked,
        failure_family: Some(family),
        summary_code,
        reusable: false,
    }
}

pub(crate) fn build_remote_blocked_snapshot(
    snapshot_id: String,
    candidate_id: String,
    consent_id: String,
    binding: InspectionBindingTuple,
    created_at_ms: i64,
) -> InspectionSnapshot {
    let blocked = no_network_remote_observation(&binding);
    InspectionSnapshot {
        snapshot_id,
        candidate_id,
        consent_id,
        binding,
        inspection_state: blocked.failure.state,
        observation_surface: ObservationSurface::CandidateMetadata,
        operation_phase: OperationPhase::Batch1ZeroEgress,
        failure_family: blocked.failure.failure_family,
        summary_code: blocked.failure.summary_code,
        conflict_codes: blocked.conflict_codes,
        fact_codes: blocked.fact_codes,
        reusable: blocked.failure.reusable,
        created_at_ms,
    }
}

pub(crate) fn build_stdio_local_snapshot(
    snapshot_id: String,
    candidate_id: String,
    consent_id: String,
    binding: InspectionBindingTuple,
    created_at_ms: i64,
) -> InspectionSnapshot {
    InspectionSnapshot {
        snapshot_id,
        candidate_id,
        consent_id,
        binding,
        inspection_state: InspectionState::InspectionRecorded,
        observation_surface: ObservationSurface::ApprovedStdioLocalMetadata,
        operation_phase: OperationPhase::Batch1ReadOnlyLocalMetadata,
        failure_family: None,
        summary_code: InspectionSummaryCode::LocalMetadataOnly,
        conflict_codes: Vec::new(),
        fact_codes: vec![
            InspectionFactCode::ApprovedStdioLocalMetadataObserved,
            InspectionFactCode::ProviderMetadataUnavailable,
            InspectionFactCode::PayloadUnavailable,
            InspectionFactCode::BindingUnavailable,
            InspectionFactCode::ManifestUnavailable,
        ],
        reusable: false,
        created_at_ms,
    }
}

pub(crate) fn validate_candidate_for_batch1_inspection(
    candidate: &IntakeCandidateRecord,
) -> McpPlatformResult<()> {
    let _ = InspectionPurpose::derive(candidate.source_facet, candidate.transport)?;
    if candidate.lifecycle_state == IntakeLifecycleState::LegacyQuarantine {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::CandidateStateConflict,
            "legacy quarantine cannot enter batch1 inspection",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_priority_prefers_policy_drift_before_transport_failures() {
        let failure = classify_failure(&[
            InspectionConflictCode::NetworkExecutionUnavailable,
            InspectionConflictCode::CandidateRevisionDrift,
            InspectionConflictCode::PolicyRevisionDrift,
        ]);
        assert_eq!(failure.state, InspectionState::InspectionBlocked);
        assert_eq!(
            failure.failure_family,
            Some(InspectionFailureFamily::PolicyDrift)
        );
        assert_eq!(failure.summary_code, InspectionSummaryCode::PolicyDrift);
        assert!(!failure.reusable);
    }

    #[test]
    fn classification_unavailable_blocks_reuse() {
        let failure = classify_failure(&[InspectionConflictCode::ClassificationUnavailable]);
        assert_eq!(failure.state, InspectionState::InspectionBlocked);
        assert_eq!(failure.failure_family, None);
        assert_eq!(
            failure.summary_code,
            InspectionSummaryCode::ClassificationUnavailable
        );
        assert!(!failure.reusable);
    }
}
