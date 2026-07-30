use std::fmt;

use serde::{Deserialize, Serialize};

mod remote_inspection_authority_types {
    use std::fmt;

    use crate::mcp_platform::credential_authority::RemoteInspectionAuthorityPermit;
    use crate::mcp_platform::intake_remote_inspection::{
        RemoteInspectionAuthorityProvider, RemoteInspectionNoLiveProofKind,
    };

    const MAX_OPAQUE_VALUE_LEN: usize = 256;

    fn validate_opaque_value(value: &str) {
        assert!(
            !value.is_empty(),
            "opaque authority value must not be empty"
        );
        assert!(
            value.len() <= MAX_OPAQUE_VALUE_LEN,
            "opaque authority value exceeds length budget"
        );
        assert!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
            "opaque authority value contains invalid bytes"
        );
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    pub(crate) struct VerifierMintedTargetHandle(String);

    impl VerifierMintedTargetHandle {
        pub(crate) fn as_str(&self) -> &str {
            &self.0
        }

        pub(crate) fn from_authority(
            _permit: &RemoteInspectionAuthorityPermit,
            value: String,
        ) -> Self {
            validate_opaque_value(&value);
            Self(value)
        }
    }

    impl fmt::Debug for VerifierMintedTargetHandle {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_tuple("VerifierMintedTargetHandle")
                .field(&"[REDACTED]")
                .finish()
        }
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    pub(crate) struct VerifierMintedTargetIdentityBinding(String);

    impl VerifierMintedTargetIdentityBinding {
        pub(crate) fn as_str(&self) -> &str {
            &self.0
        }

        pub(crate) fn from_authority(
            _permit: &RemoteInspectionAuthorityPermit,
            value: String,
        ) -> Self {
            validate_opaque_value(&value);
            Self(value)
        }
    }

    impl fmt::Debug for VerifierMintedTargetIdentityBinding {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_tuple("VerifierMintedTargetIdentityBinding")
                .field(&"[REDACTED]")
                .finish()
        }
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    pub(crate) struct AuthorityRecordRef(String);

    impl AuthorityRecordRef {
        pub(crate) fn as_str(&self) -> &str {
            &self.0
        }

        pub(crate) fn from_authority(
            _permit: &RemoteInspectionAuthorityPermit,
            value: String,
        ) -> Self {
            validate_opaque_value(&value);
            Self(value)
        }
    }

    impl fmt::Debug for AuthorityRecordRef {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_tuple("AuthorityRecordRef")
                .field(&"[REDACTED]")
                .finish()
        }
    }

    #[derive(Clone, PartialEq, Eq)]
    pub(crate) struct AuthorityVerifiedOpaqueTuple {
        target_handle: VerifierMintedTargetHandle,
        target_identity_binding: VerifierMintedTargetIdentityBinding,
        authority_provider: RemoteInspectionAuthorityProvider,
        provider_key_epoch: i64,
        generation: i64,
        authority_record_ref: AuthorityRecordRef,
    }

    impl AuthorityVerifiedOpaqueTuple {
        pub(crate) fn from_authority(
            _permit: &RemoteInspectionAuthorityPermit,
            target_handle: VerifierMintedTargetHandle,
            target_identity_binding: VerifierMintedTargetIdentityBinding,
            authority_provider: RemoteInspectionAuthorityProvider,
            provider_key_epoch: i64,
            generation: i64,
            authority_record_ref: AuthorityRecordRef,
        ) -> Self {
            assert!(
                provider_key_epoch > 0,
                "authority provider key epoch must be positive"
            );
            assert!(generation > 0, "authority generation must be positive");
            Self {
                target_handle,
                target_identity_binding,
                authority_provider,
                provider_key_epoch,
                generation,
                authority_record_ref,
            }
        }

        pub(crate) fn target_handle(&self) -> &VerifierMintedTargetHandle {
            &self.target_handle
        }

        pub(crate) fn target_identity_binding(&self) -> &VerifierMintedTargetIdentityBinding {
            &self.target_identity_binding
        }

        pub(crate) const fn authority_provider(&self) -> RemoteInspectionAuthorityProvider {
            self.authority_provider
        }

        pub(crate) const fn provider_key_epoch(&self) -> i64 {
            self.provider_key_epoch
        }

        pub(crate) const fn generation(&self) -> i64 {
            self.generation
        }

        pub(crate) fn authority_record_ref(&self) -> &AuthorityRecordRef {
            &self.authority_record_ref
        }
    }

    impl fmt::Debug for AuthorityVerifiedOpaqueTuple {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("AuthorityVerifiedOpaqueTuple")
                .field("target_handle", &"[REDACTED]")
                .field("target_identity_binding", &"[REDACTED]")
                .field("authority_provider", &self.authority_provider)
                .field("provider_key_epoch", &self.provider_key_epoch)
                .field("generation", &self.generation)
                .field("authority_record_ref", &"[REDACTED]")
                .finish()
        }
    }

    #[derive(Clone, PartialEq, Eq)]
    pub(crate) struct VerifiedNoLiveProof {
        kind: RemoteInspectionNoLiveProofKind,
        proof_id: String,
        proof_hmac: String,
        proof_key_epoch: i64,
        observed_at_ms: i64,
        authority_record_ref: AuthorityRecordRef,
    }

    impl VerifiedNoLiveProof {
        pub(crate) fn from_authority(
            _permit: &RemoteInspectionAuthorityPermit,
            kind: RemoteInspectionNoLiveProofKind,
            proof_id: String,
            proof_hmac: String,
            proof_key_epoch: i64,
            observed_at_ms: i64,
            authority_record_ref: AuthorityRecordRef,
        ) -> Self {
            validate_opaque_value(&proof_id);
            validate_opaque_value(&proof_hmac);
            assert!(proof_key_epoch > 0, "proof key epoch must be positive");
            assert!(
                observed_at_ms >= 0,
                "proof observed_at must be non-negative"
            );
            Self {
                kind,
                proof_id,
                proof_hmac,
                proof_key_epoch,
                observed_at_ms,
                authority_record_ref,
            }
        }

        pub(crate) const fn kind(&self) -> RemoteInspectionNoLiveProofKind {
            self.kind
        }

        pub(crate) fn proof_id(&self) -> &str {
            &self.proof_id
        }

        pub(crate) fn proof_hmac(&self) -> &str {
            &self.proof_hmac
        }

        pub(crate) const fn proof_key_epoch(&self) -> i64 {
            self.proof_key_epoch
        }

        pub(crate) const fn observed_at_ms(&self) -> i64 {
            self.observed_at_ms
        }

        pub(crate) fn authority_record_ref(&self) -> &AuthorityRecordRef {
            &self.authority_record_ref
        }
    }

    impl fmt::Debug for VerifiedNoLiveProof {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("VerifiedNoLiveProof")
                .field("kind", &self.kind)
                .field("proof_id", &"[REDACTED]")
                .field("proof_hmac", &"[REDACTED]")
                .field("proof_key_epoch", &self.proof_key_epoch)
                .field("observed_at_ms", &self.observed_at_ms)
                .field("authority_record_ref", &"[REDACTED]")
                .finish()
        }
    }

    #[derive(Clone, PartialEq, Eq)]
    enum AuthorityCommitOutcomeKind {
        Committed,
        ReconcileRequired,
        BlockedNoLive(VerifiedNoLiveProof),
    }

    #[derive(Clone, PartialEq, Eq)]
    pub(crate) struct AuthorityCommitOutcome {
        kind: AuthorityCommitOutcomeKind,
    }

    impl AuthorityCommitOutcome {
        fn committed() -> Self {
            Self {
                kind: AuthorityCommitOutcomeKind::Committed,
            }
        }

        fn reconcile_required() -> Self {
            Self {
                kind: AuthorityCommitOutcomeKind::ReconcileRequired,
            }
        }

        fn blocked_no_live(proof: VerifiedNoLiveProof) -> Self {
            Self {
                kind: AuthorityCommitOutcomeKind::BlockedNoLive(proof),
            }
        }

        pub(crate) fn is_committed(&self) -> bool {
            matches!(self.kind, AuthorityCommitOutcomeKind::Committed)
        }

        pub(crate) fn is_reconcile_required(&self) -> bool {
            matches!(self.kind, AuthorityCommitOutcomeKind::ReconcileRequired)
        }

        pub(crate) fn is_blocked_no_live(&self) -> bool {
            matches!(self.kind, AuthorityCommitOutcomeKind::BlockedNoLive(_))
        }

        pub(crate) fn blocked_proof(&self) -> Option<&VerifiedNoLiveProof> {
            match &self.kind {
                AuthorityCommitOutcomeKind::BlockedNoLive(proof) => Some(proof),
                AuthorityCommitOutcomeKind::Committed
                | AuthorityCommitOutcomeKind::ReconcileRequired => None,
            }
        }
    }

    impl RemoteInspectionAuthorityPermit {
        pub(crate) fn committed(&self) -> AuthorityCommitOutcome {
            AuthorityCommitOutcome::committed()
        }

        pub(crate) fn reconcile_required(&self) -> AuthorityCommitOutcome {
            AuthorityCommitOutcome::reconcile_required()
        }

        pub(crate) fn blocked_no_live(&self, proof: VerifiedNoLiveProof) -> AuthorityCommitOutcome {
            AuthorityCommitOutcome::blocked_no_live(proof)
        }
    }

    impl fmt::Debug for AuthorityCommitOutcome {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            let kind = if self.is_committed() {
                "Committed"
            } else if self.is_reconcile_required() {
                "ReconcileRequired"
            } else {
                "BlockedNoLive"
            };

            formatter
                .debug_struct("AuthorityCommitOutcome")
                .field("kind", &kind)
                .field(
                    "blocked_proof",
                    &self
                        .blocked_proof()
                        .map(|_| "[REDACTED]")
                        .unwrap_or("[NONE]"),
                )
                .finish()
        }
    }
}

pub(crate) use remote_inspection_authority_types::{
    AuthorityCommitOutcome, AuthorityRecordRef, AuthorityVerifiedOpaqueTuple, VerifiedNoLiveProof,
    VerifierMintedTargetHandle, VerifierMintedTargetIdentityBinding,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IntakeSourceFacet {
    CatalogPlanning,
    ManualHttpsCandidate,
    ApprovedStdioCandidate,
    LegacyQuarantine,
}

impl IntakeSourceFacet {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CatalogPlanning => "catalog_planning",
            Self::ManualHttpsCandidate => "manual_https_candidate",
            Self::ApprovedStdioCandidate => "approved_stdio_candidate",
            Self::LegacyQuarantine => "legacy_quarantine",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "catalog_planning" => Some(Self::CatalogPlanning),
            "manual_https_candidate" => Some(Self::ManualHttpsCandidate),
            "approved_stdio_candidate" => Some(Self::ApprovedStdioCandidate),
            "legacy_quarantine" => Some(Self::LegacyQuarantine),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IntakeTransport {
    StreamableHttp,
    Stdio,
    CatalogReference,
    Legacy,
}

impl IntakeTransport {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::StreamableHttp => "streamable_http",
            Self::Stdio => "stdio",
            Self::CatalogReference => "catalog_reference",
            Self::Legacy => "legacy",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "streamable_http" => Some(Self::StreamableHttp),
            "stdio" => Some(Self::Stdio),
            "catalog_reference" => Some(Self::CatalogReference),
            "legacy" => Some(Self::Legacy),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IntakeLifecycleState {
    LegacyQuarantine,
    Submitted,
    AwaitingConsent,
    ConsentGranted,
    ApprovalGranted,
    BindingReady,
}

impl IntakeLifecycleState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyQuarantine => "legacy_quarantine",
            Self::Submitted => "submitted",
            Self::AwaitingConsent => "awaiting_consent",
            Self::ConsentGranted => "consent_granted",
            Self::ApprovalGranted => "approval_granted",
            Self::BindingReady => "binding_ready",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "legacy_quarantine" => Some(Self::LegacyQuarantine),
            "submitted" => Some(Self::Submitted),
            "awaiting_consent" => Some(Self::AwaitingConsent),
            "consent_granted" => Some(Self::ConsentGranted),
            "approval_granted" => Some(Self::ApprovalGranted),
            "binding_ready" => Some(Self::BindingReady),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IntakeGateReason {
    CandidateStateConflict,
    ManifestIdentityConflict,
    ManualStdioProviderUnavailable,
    RemoteHttpPolicyUnavailable,
    UnsupportedProvider,
    EmptyProvider,
    UnsupportedTransport,
}

impl IntakeGateReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CandidateStateConflict => "candidate_state_conflict",
            Self::ManifestIdentityConflict => "manifest_identity_conflict",
            Self::ManualStdioProviderUnavailable => "manual_stdio_provider_unavailable",
            Self::RemoteHttpPolicyUnavailable => "remote_http_policy_unavailable",
            Self::UnsupportedProvider => "unsupported_provider",
            Self::EmptyProvider => "empty_provider",
            Self::UnsupportedTransport => "unsupported_transport",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "candidate_state_conflict" => Some(Self::CandidateStateConflict),
            "manifest_identity_conflict" => Some(Self::ManifestIdentityConflict),
            "manual_stdio_provider_unavailable" => Some(Self::ManualStdioProviderUnavailable),
            "remote_http_policy_unavailable" => Some(Self::RemoteHttpPolicyUnavailable),
            "unsupported_provider" => Some(Self::UnsupportedProvider),
            "empty_provider" => Some(Self::EmptyProvider),
            "unsupported_transport" => Some(Self::UnsupportedTransport),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum IntakeConfigurationDescriptor {
    CatalogPlanning {
        source_id: String,
        mcp_id: String,
        version: String,
    },
    ManualHttpsCandidate {
        display_origin: String,
    },
    ApprovedStdioCandidate {
        provider_source_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared_mcp_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared_version: Option<String>,
    },
    LegacyQuarantine {
        legacy_flow: String,
    },
}

impl fmt::Debug for IntakeConfigurationDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CatalogPlanning {
                source_id,
                mcp_id,
                version,
            } => formatter
                .debug_struct("CatalogPlanning")
                .field("source_id", source_id)
                .field("mcp_id", mcp_id)
                .field("version", version)
                .finish(),
            Self::ManualHttpsCandidate { display_origin } => formatter
                .debug_struct("ManualHttpsCandidate")
                .field("display_origin", display_origin)
                .finish(),
            Self::ApprovedStdioCandidate {
                provider_source_ref,
                declared_mcp_id,
                declared_version,
            } => formatter
                .debug_struct("ApprovedStdioCandidate")
                .field("provider_source_ref", provider_source_ref)
                .field("declared_mcp_id", declared_mcp_id)
                .field("declared_version", declared_version)
                .finish(),
            Self::LegacyQuarantine { legacy_flow } => formatter
                .debug_struct("LegacyQuarantine")
                .field("legacy_flow", legacy_flow)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntakePrivateReferenceKind {
    CatalogManifest,
    ManualHttpsEndpoint,
    ApprovedStdioPayload,
}

impl IntakePrivateReferenceKind {
    pub(crate) const fn for_source_facet(source_facet: IntakeSourceFacet) -> Option<Self> {
        match source_facet {
            IntakeSourceFacet::CatalogPlanning => Some(Self::CatalogManifest),
            IntakeSourceFacet::ManualHttpsCandidate => Some(Self::ManualHttpsEndpoint),
            IntakeSourceFacet::ApprovedStdioCandidate => Some(Self::ApprovedStdioPayload),
            IntakeSourceFacet::LegacyQuarantine => None,
        }
    }

    pub(crate) fn for_descriptor(descriptor: &IntakeConfigurationDescriptor) -> Option<Self> {
        match descriptor {
            IntakeConfigurationDescriptor::CatalogPlanning { .. } => Some(Self::CatalogManifest),
            IntakeConfigurationDescriptor::ManualHttpsCandidate { .. } => {
                Some(Self::ManualHttpsEndpoint)
            }
            IntakeConfigurationDescriptor::ApprovedStdioCandidate { .. } => {
                Some(Self::ApprovedStdioPayload)
            }
            IntakeConfigurationDescriptor::LegacyQuarantine { .. } => None,
        }
    }

    pub(crate) const fn requires_private_reference(self) -> bool {
        !matches!(self, Self::CatalogManifest)
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CatalogManifest => "catalog_manifest",
            Self::ManualHttpsEndpoint => "manual_https_endpoint",
            Self::ApprovedStdioPayload => "approved_stdio_payload",
        }
    }
}

pub(crate) struct MintedIntakePrivateReference<'a> {
    pub(crate) nonce_hex: &'a str,
    pub(crate) mac_hex: &'a str,
}

const PRIVATE_REFERENCE_NONCE_HEX_LEN: usize = 32;
const PRIVATE_REFERENCE_MAC_HEX_LEN: usize = 64;

pub(crate) fn format_minted_intake_private_reference(
    kind: IntakePrivateReferenceKind,
    nonce_hex: &str,
    mac_hex: &str,
) -> String {
    format!("oprv2_{}_{}_{}", kind.label(), nonce_hex, mac_hex)
}

pub(crate) fn parse_minted_intake_private_reference<'a>(
    kind: IntakePrivateReferenceKind,
    value: &'a str,
) -> Option<MintedIntakePrivateReference<'a>> {
    let prefix = format!("oprv2_{}_", kind.label());
    if !value.starts_with(&prefix) {
        return None;
    }
    let suffix = &value[prefix.len()..];
    let (nonce_hex, mac_hex) = suffix.split_once('_')?;
    if nonce_hex.len() != PRIVATE_REFERENCE_NONCE_HEX_LEN
        || mac_hex.len() != PRIVATE_REFERENCE_MAC_HEX_LEN
        || suffix.matches('_').count() != 1
    {
        return None;
    }
    if !nonce_hex
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    if !mac_hex
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some(MintedIntakePrivateReference { nonce_hex, mac_hex })
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct IntakeCandidateRecord {
    pub(crate) candidate_id: String,
    pub(crate) source_facet: IntakeSourceFacet,
    pub(crate) transport: IntakeTransport,
    pub(crate) lifecycle_state: IntakeLifecycleState,
    pub(crate) gate_reason: Option<IntakeGateReason>,
    pub(crate) submission_binding: String,
    pub(crate) approval_binding: Option<String>,
    pub(crate) manifest_identity_binding: Option<String>,
    pub(crate) redacted_configuration_ref: String,
    pub(crate) descriptor_digest: String,
    pub(crate) registry_owned_payload_digest: Option<String>,
    pub(crate) revision: i64,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
}

impl fmt::Debug for IntakeCandidateRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IntakeCandidateRecord")
            .field("candidate_id", &self.candidate_id)
            .field("source_facet", &self.source_facet)
            .field("transport", &self.transport)
            .field("lifecycle_state", &self.lifecycle_state)
            .field("gate_reason", &self.gate_reason)
            .field(
                "approval_binding",
                &self.approval_binding.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "manifest_identity_binding",
                &self
                    .manifest_identity_binding
                    .as_ref()
                    .map(|_| "[REDACTED]"),
            )
            .field(
                "redacted_configuration_ref",
                &self.redacted_configuration_ref,
            )
            .field("revision", &self.revision)
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

pub(crate) struct SaveIntakeConfigurationRef<'a> {
    pub(crate) configuration_ref: &'a str,
    pub(crate) descriptor: &'a IntakeConfigurationDescriptor,
    pub(crate) private_reference: Option<&'a str>,
    pub(crate) descriptor_digest: &'a str,
    pub(crate) now_ms: i64,
}

pub(crate) struct SaveIntakeCandidate<'a> {
    pub(crate) candidate_id: &'a str,
    pub(crate) submission_binding: &'a str,
    pub(crate) source_facet: IntakeSourceFacet,
    pub(crate) transport: IntakeTransport,
    pub(crate) initial_state: IntakeLifecycleState,
    pub(crate) redacted_configuration_ref: &'a str,
    pub(crate) descriptor_digest: &'a str,
    pub(crate) now_ms: i64,
}

pub(crate) struct RecordIntakeConsentGrant<'a> {
    pub(crate) candidate_id: &'a str,
    pub(crate) consent_binding: &'a str,
    pub(crate) now_ms: i64,
}

pub(crate) struct RecordIntakeApprovalGrant<'a> {
    pub(crate) candidate_id: &'a str,
    pub(crate) approval_binding: &'a str,
    pub(crate) now_ms: i64,
}

pub(crate) struct RecordIntakeBindingReady<'a> {
    pub(crate) candidate_id: &'a str,
    pub(crate) manifest_identity_binding: &'a str,
    pub(crate) registry_owned_payload_digest: Option<&'a str>,
    pub(crate) now_ms: i64,
}

pub(crate) struct LegacyQuarantineCandidateInput {
    pub(crate) legacy_flow: String,
}

pub(crate) struct CatalogPlanningCandidateInput {
    pub(crate) source_id: String,
    pub(crate) mcp_id: String,
    pub(crate) version: String,
    pub(crate) private_manifest_ref: Option<String>,
}

pub(crate) enum ManualHttpsCandidateInput {
    Candidate {
        display_origin: String,
        private_endpoint_ref: String,
    },
    RemoteHttpPolicyUnavailable,
    UnsupportedTransport,
}

pub(crate) enum ApprovedStdioCandidateInput {
    Candidate {
        provider_source_ref: String,
        private_payload_ref: String,
        declared_mcp_id: Option<String>,
        declared_version: Option<String>,
    },
    ManualStdioProviderUnavailable,
    EmptyProvider,
    UnsupportedTransport,
}
