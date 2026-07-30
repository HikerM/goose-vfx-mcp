use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::collections::{BTreeMap, VecDeque};
#[cfg(test)]
use std::sync::Mutex;

use base64::Engine as _;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::error::McpPlatformErrorCode;
use super::intake::{
    AuthorityCommitOutcome, AuthorityRecordRef, AuthorityVerifiedOpaqueTuple, VerifiedNoLiveProof,
    VerifierMintedTargetHandle, VerifierMintedTargetIdentityBinding,
};
use super::intake_remote_inspection::{
    RemoteInspectionAuthorityProvider, RemoteInspectionNoLiveProofKind,
};

const KEY_BYTES: usize = 32;
#[cfg(feature = "system-keyring")]
const KEYRING_SERVICE: &str = "goose-mcp-platform-credential-authority-v1";
#[cfg(feature = "system-keyring")]
const ROOT_ACCOUNT: &str = "credential-authority/root";
#[cfg(feature = "system-keyring")]
const ANCHOR_KEYRING_SERVICE: &str = "goose-mcp-platform-credential-authority-anchor-v1";
#[cfg(feature = "system-keyring")]
const ANCHOR_ROOT_ACCOUNT: &str = "credential-authority-anchor/root";
const ROOT_NAMESPACE: &str = "goose.mcp-platform.credential-authority";
const ANCHOR_ROOT_NAMESPACE: &str = "goose.mcp-platform.credential-authority.anchor-root";
const HANDLE_ANCHOR_NAMESPACE: &str = "goose.mcp-platform.credential-authority.handle-anchor";
const ENVELOPE_DOMAIN: &[u8] = b"goose.mcp-platform.credential-authority";
const ROOT_FORMAT_VERSION: u8 = 1;
const ANCHOR_ROOT_FORMAT_VERSION: u8 = 1;
const RECORD_FORMAT_VERSION: u8 = 1;
const HANDLE_ANCHOR_FORMAT_VERSION: u8 = 1;
const PAYLOAD_BINDING_KEY_DOMAIN: &str = "credential_record_payload_binding_key_v1";
const PAYLOAD_BINDING_DOMAIN: &str = "credential_record_payload_binding_v1";
const HANDLE_ANCHOR_DOMAIN: &str = "credential_handle_anchor_v1";
#[cfg(feature = "system-keyring")]
const SYSTEM_KEYRING_PROVIDER_ID: &str = "system-keyring";
#[cfg(test)]
const TEST_IN_MEMORY_PROVIDER_ID: &str = "in-memory-test";
#[cfg(not(feature = "system-keyring"))]
const UNSUPPORTED_PROVIDER_ID: &str = "unsupported";
#[cfg(test)]
const SYSTEM_KEYRING_UNAVAILABLE_REASON: &str =
    "managed MCP credential authority system keyring is unavailable";
#[cfg(any(test, not(feature = "system-keyring")))]
const SYSTEM_KEYRING_UNSUPPORTED_REASON: &str =
    "managed MCP credential authority requires system keyring support";

pub(crate) struct RemoteInspectionAuthorityPermit(());

pub(crate) struct RemoteInspectionTargetAuthority {
    provider: RemoteInspectionAuthorityProvider,
    provider_key_epoch: i64,
    generation: i64,
    permit: RemoteInspectionAuthorityPermit,
}

impl fmt::Debug for RemoteInspectionTargetAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteInspectionTargetAuthority")
            .field("provider", &self.provider)
            .field("provider_key_epoch", &self.provider_key_epoch)
            .field("generation", &self.generation)
            .finish()
    }
}

impl RemoteInspectionTargetAuthority {
    #[cfg(test)]
    pub(crate) fn in_memory_for_testing() -> Self {
        Self {
            provider: RemoteInspectionAuthorityProvider::InMemoryTest,
            provider_key_epoch: 1,
            generation: 1,
            permit: RemoteInspectionAuthorityPermit(()),
        }
    }

    pub(crate) const fn provider(&self) -> RemoteInspectionAuthorityProvider {
        self.provider
    }

    pub(crate) const fn provider_key_epoch(&self) -> i64 {
        self.provider_key_epoch
    }

    pub(crate) const fn generation(&self) -> i64 {
        self.generation
    }

    pub(crate) fn mint_verified_opaque_tuple(&self) -> AuthorityVerifiedOpaqueTuple {
        let record_ref = AuthorityRecordRef::from_authority(
            &self.permit,
            self.mint_opaque("authority_record_ref"),
        );
        AuthorityVerifiedOpaqueTuple::from_authority(
            &self.permit,
            VerifierMintedTargetHandle::from_authority(
                &self.permit,
                self.mint_opaque("target_handle"),
            ),
            VerifierMintedTargetIdentityBinding::from_authority(
                &self.permit,
                self.mint_opaque("target_identity_binding"),
            ),
            self.provider,
            self.provider_key_epoch,
            self.generation,
            record_ref,
        )
    }

    pub(crate) fn verify_no_live_proof(
        &self,
        tuple: &AuthorityVerifiedOpaqueTuple,
        reservation_id: &str,
        reservation_generation: i64,
        intent_fingerprint: &str,
        kind: RemoteInspectionNoLiveProofKind,
        observed_at_ms: i64,
    ) -> VerifiedNoLiveProof {
        let proof_id = self.mint_opaque("no_live_proof");
        let proof_hmac = self.proof_hmac(
            tuple,
            reservation_id,
            reservation_generation,
            intent_fingerprint,
            kind,
            observed_at_ms,
            &proof_id,
        );
        VerifiedNoLiveProof::from_authority(
            &self.permit,
            kind,
            proof_id,
            proof_hmac,
            self.provider_key_epoch,
            observed_at_ms,
            AuthorityRecordRef::from_authority(
                &self.permit,
                tuple.authority_record_ref().as_str().to_string(),
            ),
        )
    }

    pub(crate) fn committed(&self) -> AuthorityCommitOutcome {
        self.permit.committed()
    }

    pub(crate) fn reconcile_required(&self) -> AuthorityCommitOutcome {
        self.permit.reconcile_required()
    }

    pub(crate) fn blocked_no_live(&self, proof: VerifiedNoLiveProof) -> AuthorityCommitOutcome {
        self.permit.blocked_no_live(proof)
    }

    fn mint_opaque(&self, label: &str) -> String {
        format!("{label}_{}", Uuid::new_v4().simple())
    }

    fn proof_hmac(
        &self,
        tuple: &AuthorityVerifiedOpaqueTuple,
        reservation_id: &str,
        reservation_generation: i64,
        intent_fingerprint: &str,
        kind: RemoteInspectionNoLiveProofKind,
        observed_at_ms: i64,
        proof_id: &str,
    ) -> String {
        let mut digest = Sha256::new();
        digest.update(b"goose.mcp-platform.remote-inspection.no-live-proof.v1");
        digest.update(self.provider.as_str().as_bytes());
        digest.update(self.provider_key_epoch.to_string().as_bytes());
        digest.update(self.generation.to_string().as_bytes());
        digest.update(tuple.authority_record_ref().as_str().as_bytes());
        digest.update(reservation_id.as_bytes());
        digest.update(reservation_generation.to_string().as_bytes());
        digest.update(intent_fingerprint.as_bytes());
        digest.update(kind.as_str().as_bytes());
        digest.update(observed_at_ms.to_string().as_bytes());
        digest.update(proof_id.as_bytes());
        crate::utils::bytes_to_hex(digest.finalize())
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CredentialHandle(String);

impl CredentialHandle {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, CredentialAuthorityError> {
        let value = value.into();
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 128
            || !bytes[0].is_ascii_lowercase()
            || bytes.iter().any(|byte| {
                !matches!(
                    byte,
                    b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.'
                )
            })
            || bytes
                .windows(2)
                .any(|window| matches!(window, [b'-', b'-'] | [b'_', b'_'] | [b'.', b'.']))
            || matches!(bytes.last(), Some(b'-' | b'_' | b'.'))
        {
            return Err(CredentialAuthorityError::invalid_handle());
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CredentialHandle")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for CredentialHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CredentialReadinessBinding {
    AuthRequirementProbe {
        auth_type: String,
        reference_label: String,
    },
    ManagedEnable {
        managed_mcp_id: String,
        provider_id: String,
        repository_id: String,
        repository_path: String,
        inventory_revision: i64,
        checkpoint_sequence: u64,
        checkpoint_root: String,
    },
    HealthCheck {
        managed_mcp_id: String,
        provider_id: String,
        repository_id: String,
        repository_path: String,
        health_check_id: String,
        mode: String,
        health_revision: i64,
        checkpoint_sequence: u64,
    },
    ProfileSnapshot {
        profile_id: String,
        repository_id: String,
        repository_path: String,
        profile_revision: i64,
        checkpoint_sequence: u64,
    },
    ProfileSnapshotRuntime {
        profile_id: String,
        profile_revision: i64,
        managed_mcp_id: String,
        managed_revision: i64,
        projection_revision: i64,
    },
    ProfileApplication {
        profile_id: String,
        application_id: String,
        repository_id: String,
        repository_path: String,
        profile_revision: i64,
        checkpoint_sequence: u64,
    },
    ProfileApplicationRuntime {
        profile_id: String,
        application_id: String,
        managed_mcp_id: String,
        profile_revision: i64,
        managed_revision: i64,
        plan_digest: String,
    },
    ManagedEnableRuntime {
        managed_mcp_id: String,
        expected_revision: i64,
        desired_enabled: bool,
        manifest_digest: String,
        projection_digest: String,
    },
    HealthRuntime {
        managed_mcp_id: String,
        task_id: String,
        mode: String,
        managed_revision: i64,
        manifest_digest: String,
    },
    ProjectionMutation {
        managed_mcp_id: String,
        provider_id: String,
        repository_id: String,
        repository_path: String,
        mutation_id: String,
        projection_revision: i64,
        checkpoint_sequence: u64,
        checkpoint_root: String,
    },
}

impl CredentialReadinessBinding {
    pub(crate) fn auth_requirement_probe(
        auth_type: impl Into<String>,
        reference_label: impl Into<String>,
    ) -> Self {
        Self::AuthRequirementProbe {
            auth_type: auth_type.into(),
            reference_label: reference_label.into(),
        }
    }

    pub(crate) fn managed_enable(
        managed_mcp_id: impl Into<String>,
        provider_id: impl Into<String>,
        repository_id: impl Into<String>,
        repository_path: impl Into<String>,
        inventory_revision: i64,
        checkpoint_sequence: u64,
        checkpoint_root: impl Into<String>,
    ) -> Self {
        Self::ManagedEnable {
            managed_mcp_id: managed_mcp_id.into(),
            provider_id: provider_id.into(),
            repository_id: repository_id.into(),
            repository_path: repository_path.into(),
            inventory_revision,
            checkpoint_sequence,
            checkpoint_root: checkpoint_root.into(),
        }
    }

    pub(crate) fn health_check(
        managed_mcp_id: impl Into<String>,
        provider_id: impl Into<String>,
        repository_id: impl Into<String>,
        repository_path: impl Into<String>,
        health_check_id: impl Into<String>,
        mode: impl Into<String>,
        health_revision: i64,
        checkpoint_sequence: u64,
    ) -> Self {
        Self::HealthCheck {
            managed_mcp_id: managed_mcp_id.into(),
            provider_id: provider_id.into(),
            repository_id: repository_id.into(),
            repository_path: repository_path.into(),
            health_check_id: health_check_id.into(),
            mode: mode.into(),
            health_revision,
            checkpoint_sequence,
        }
    }

    pub(crate) fn profile_snapshot(
        profile_id: impl Into<String>,
        repository_id: impl Into<String>,
        repository_path: impl Into<String>,
        profile_revision: i64,
        checkpoint_sequence: u64,
    ) -> Self {
        Self::ProfileSnapshot {
            profile_id: profile_id.into(),
            repository_id: repository_id.into(),
            repository_path: repository_path.into(),
            profile_revision,
            checkpoint_sequence,
        }
    }

    pub(crate) fn profile_snapshot_runtime(
        profile_id: impl Into<String>,
        profile_revision: i64,
        managed_mcp_id: impl Into<String>,
        managed_revision: i64,
        projection_revision: i64,
    ) -> Self {
        Self::ProfileSnapshotRuntime {
            profile_id: profile_id.into(),
            profile_revision,
            managed_mcp_id: managed_mcp_id.into(),
            managed_revision,
            projection_revision,
        }
    }

    pub(crate) fn profile_application(
        profile_id: impl Into<String>,
        application_id: impl Into<String>,
        repository_id: impl Into<String>,
        repository_path: impl Into<String>,
        profile_revision: i64,
        checkpoint_sequence: u64,
    ) -> Self {
        Self::ProfileApplication {
            profile_id: profile_id.into(),
            application_id: application_id.into(),
            repository_id: repository_id.into(),
            repository_path: repository_path.into(),
            profile_revision,
            checkpoint_sequence,
        }
    }

    pub(crate) fn profile_application_runtime(
        profile_id: impl Into<String>,
        application_id: impl Into<String>,
        managed_mcp_id: impl Into<String>,
        profile_revision: i64,
        managed_revision: i64,
        plan_digest: impl Into<String>,
    ) -> Self {
        Self::ProfileApplicationRuntime {
            profile_id: profile_id.into(),
            application_id: application_id.into(),
            managed_mcp_id: managed_mcp_id.into(),
            profile_revision,
            managed_revision,
            plan_digest: plan_digest.into(),
        }
    }

    pub(crate) fn managed_enable_runtime(
        managed_mcp_id: impl Into<String>,
        expected_revision: i64,
        desired_enabled: bool,
        manifest_digest: impl Into<String>,
        projection_digest: impl Into<String>,
    ) -> Self {
        Self::ManagedEnableRuntime {
            managed_mcp_id: managed_mcp_id.into(),
            expected_revision,
            desired_enabled,
            manifest_digest: manifest_digest.into(),
            projection_digest: projection_digest.into(),
        }
    }

    pub(crate) fn health_runtime(
        managed_mcp_id: impl Into<String>,
        task_id: impl Into<String>,
        mode: impl Into<String>,
        managed_revision: i64,
        manifest_digest: impl Into<String>,
    ) -> Self {
        Self::HealthRuntime {
            managed_mcp_id: managed_mcp_id.into(),
            task_id: task_id.into(),
            mode: mode.into(),
            managed_revision,
            manifest_digest: manifest_digest.into(),
        }
    }

    pub(crate) fn projection_mutation(
        managed_mcp_id: impl Into<String>,
        provider_id: impl Into<String>,
        repository_id: impl Into<String>,
        repository_path: impl Into<String>,
        mutation_id: impl Into<String>,
        projection_revision: i64,
        checkpoint_sequence: u64,
        checkpoint_root: impl Into<String>,
    ) -> Self {
        Self::ProjectionMutation {
            managed_mcp_id: managed_mcp_id.into(),
            provider_id: provider_id.into(),
            repository_id: repository_id.into(),
            repository_path: repository_path.into(),
            mutation_id: mutation_id.into(),
            projection_revision,
            checkpoint_sequence,
            checkpoint_root: checkpoint_root.into(),
        }
    }

    fn operation_context(&self) -> &'static str {
        match self {
            Self::AuthRequirementProbe { .. } => "auth_requirement_probe",
            Self::ManagedEnable { .. } => "managed_enable",
            Self::HealthCheck { .. } => "health_check",
            Self::ProfileSnapshot { .. } => "profile_snapshot",
            Self::ProfileSnapshotRuntime { .. } => "profile_snapshot_runtime",
            Self::ProfileApplication { .. } => "profile_application",
            Self::ProfileApplicationRuntime { .. } => "profile_application_runtime",
            Self::ManagedEnableRuntime { .. } => "managed_enable_runtime",
            Self::HealthRuntime { .. } => "health_runtime",
            Self::ProjectionMutation { .. } => "projection_mutation",
        }
    }

    fn canonical_payload(&self) -> Vec<u8> {
        match self {
            Self::AuthRequirementProbe {
                auth_type,
                reference_label,
            } => canonical_fields(&[
                b"auth_requirement_probe",
                auth_type.as_bytes(),
                reference_label.as_bytes(),
            ]),
            Self::ManagedEnable {
                managed_mcp_id,
                provider_id,
                repository_id,
                repository_path,
                inventory_revision,
                checkpoint_sequence,
                checkpoint_root,
            } => canonical_fields(&[
                b"managed_enable",
                managed_mcp_id.as_bytes(),
                provider_id.as_bytes(),
                repository_id.as_bytes(),
                repository_path.as_bytes(),
                inventory_revision.to_string().as_bytes(),
                checkpoint_sequence.to_string().as_bytes(),
                checkpoint_root.as_bytes(),
            ]),
            Self::HealthCheck {
                managed_mcp_id,
                provider_id,
                repository_id,
                repository_path,
                health_check_id,
                mode,
                health_revision,
                checkpoint_sequence,
            } => canonical_fields(&[
                b"health_check",
                managed_mcp_id.as_bytes(),
                provider_id.as_bytes(),
                repository_id.as_bytes(),
                repository_path.as_bytes(),
                health_check_id.as_bytes(),
                mode.as_bytes(),
                health_revision.to_string().as_bytes(),
                checkpoint_sequence.to_string().as_bytes(),
            ]),
            Self::ProfileSnapshot {
                profile_id,
                repository_id,
                repository_path,
                profile_revision,
                checkpoint_sequence,
            } => canonical_fields(&[
                b"profile_snapshot",
                profile_id.as_bytes(),
                repository_id.as_bytes(),
                repository_path.as_bytes(),
                profile_revision.to_string().as_bytes(),
                checkpoint_sequence.to_string().as_bytes(),
            ]),
            Self::ProfileSnapshotRuntime {
                profile_id,
                profile_revision,
                managed_mcp_id,
                managed_revision,
                projection_revision,
            } => canonical_fields(&[
                b"profile_snapshot_runtime",
                profile_id.as_bytes(),
                profile_revision.to_string().as_bytes(),
                managed_mcp_id.as_bytes(),
                managed_revision.to_string().as_bytes(),
                projection_revision.to_string().as_bytes(),
            ]),
            Self::ProfileApplication {
                profile_id,
                application_id,
                repository_id,
                repository_path,
                profile_revision,
                checkpoint_sequence,
            } => canonical_fields(&[
                b"profile_application",
                profile_id.as_bytes(),
                application_id.as_bytes(),
                repository_id.as_bytes(),
                repository_path.as_bytes(),
                profile_revision.to_string().as_bytes(),
                checkpoint_sequence.to_string().as_bytes(),
            ]),
            Self::ProfileApplicationRuntime {
                profile_id,
                application_id,
                managed_mcp_id,
                profile_revision,
                managed_revision,
                plan_digest,
            } => canonical_fields(&[
                b"profile_application_runtime",
                profile_id.as_bytes(),
                application_id.as_bytes(),
                managed_mcp_id.as_bytes(),
                profile_revision.to_string().as_bytes(),
                managed_revision.to_string().as_bytes(),
                plan_digest.as_bytes(),
            ]),
            Self::ManagedEnableRuntime {
                managed_mcp_id,
                expected_revision,
                desired_enabled,
                manifest_digest,
                projection_digest,
            } => canonical_fields(&[
                b"managed_enable_runtime",
                managed_mcp_id.as_bytes(),
                expected_revision.to_string().as_bytes(),
                if *desired_enabled { b"1" } else { b"0" },
                manifest_digest.as_bytes(),
                projection_digest.as_bytes(),
            ]),
            Self::HealthRuntime {
                managed_mcp_id,
                task_id,
                mode,
                managed_revision,
                manifest_digest,
            } => canonical_fields(&[
                b"health_runtime",
                managed_mcp_id.as_bytes(),
                task_id.as_bytes(),
                mode.as_bytes(),
                managed_revision.to_string().as_bytes(),
                manifest_digest.as_bytes(),
            ]),
            Self::ProjectionMutation {
                managed_mcp_id,
                provider_id,
                repository_id,
                repository_path,
                mutation_id,
                projection_revision,
                checkpoint_sequence,
                checkpoint_root,
            } => canonical_fields(&[
                b"projection_mutation",
                managed_mcp_id.as_bytes(),
                provider_id.as_bytes(),
                repository_id.as_bytes(),
                repository_path.as_bytes(),
                mutation_id.as_bytes(),
                projection_revision.to_string().as_bytes(),
                checkpoint_sequence.to_string().as_bytes(),
                checkpoint_root.as_bytes(),
            ]),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialReadinessStatus {
    Ready,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CredentialReadinessSnapshot {
    handle: CredentialHandle,
    status: CredentialReadinessStatus,
    provider_id: String,
    root_instance_id: String,
    key_epoch: u64,
    anchor_root_instance_id: String,
    anchor_key_epoch: u64,
    generation: u64,
    secret_revision: u64,
    binding_digest: String,
    evidence_digest: String,
    operation_context: &'static str,
    remediation: Option<RemediationCode>,
}

impl CredentialReadinessSnapshot {
    pub(crate) fn handle(&self) -> &CredentialHandle {
        &self.handle
    }

    pub(crate) fn status(&self) -> CredentialReadinessStatus {
        self.status
    }

    pub(crate) fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub(crate) fn root_instance_id(&self) -> &str {
        &self.root_instance_id
    }

    pub(crate) fn key_epoch(&self) -> u64 {
        self.key_epoch
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn secret_revision(&self) -> u64 {
        self.secret_revision
    }

    pub(crate) fn binding_digest(&self) -> &str {
        &self.binding_digest
    }

    pub(crate) fn evidence_digest(&self) -> &str {
        &self.evidence_digest
    }

    pub(crate) fn remediation(&self) -> Option<RemediationCode> {
        self.remediation
    }

    fn require_ready(&self) -> Result<(), CredentialAuthorityError> {
        if self.status == CredentialReadinessStatus::Ready {
            Ok(())
        } else {
            Err(CredentialAuthorityError::deleted_credential())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialAuthorityErrorKind {
    InvalidHandle,
    CapabilityRequired,
    Unavailable,
    Unsupported,
    MissingRoot,
    MissingCredential,
    DeletedCredential,
    LegacyConflict,
    WitnessExpired,
    WitnessConsumed,
    WitnessMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemediationCode {
    UseSystemKeyring,
    InitializeAuthorityRoot,
    CreateCredential,
    RecreateCredential,
    TrustedReenrollment,
    ReissueWitness,
    RestartAuthoritySession,
    VerifyBinding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CredentialAuthorityError {
    kind: CredentialAuthorityErrorKind,
    remediation: RemediationCode,
    code: McpPlatformErrorCode,
    message: &'static str,
}

impl CredentialAuthorityError {
    pub(crate) fn kind(&self) -> CredentialAuthorityErrorKind {
        self.kind
    }

    pub(crate) fn remediation(&self) -> RemediationCode {
        self.remediation
    }

    pub(crate) fn code(&self) -> McpPlatformErrorCode {
        self.code
    }

    const fn invalid_handle() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::InvalidHandle,
            remediation: RemediationCode::CreateCredential,
            code: McpPlatformErrorCode::InvalidRequest,
            message: "managed MCP credential authority handle is invalid",
        }
    }

    const fn capability_required() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::CapabilityRequired,
            remediation: RemediationCode::VerifyBinding,
            code: McpPlatformErrorCode::PolicyDenied,
            message: "managed MCP credential authority trusted write capability is required",
        }
    }

    const fn unavailable() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::Unavailable,
            remediation: RemediationCode::UseSystemKeyring,
            code: McpPlatformErrorCode::IntegrityUnavailable,
            message: "managed MCP credential authority is unavailable",
        }
    }

    const fn unsupported() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::Unsupported,
            remediation: RemediationCode::UseSystemKeyring,
            code: McpPlatformErrorCode::IntegrityUnavailable,
            message: "managed MCP credential authority provider is unsupported",
        }
    }

    const fn missing_root() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::MissingRoot,
            remediation: RemediationCode::InitializeAuthorityRoot,
            code: McpPlatformErrorCode::IntegrityUnavailable,
            message: "managed MCP credential authority root is missing",
        }
    }

    const fn missing_root_with_residual_state() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::LegacyConflict,
            remediation: RemediationCode::TrustedReenrollment,
            code: McpPlatformErrorCode::IntegrityError,
            message: "managed MCP credential authority root is missing while residual state requires trusted re-enrollment",
        }
    }

    const fn missing_credential() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::MissingCredential,
            remediation: RemediationCode::CreateCredential,
            code: McpPlatformErrorCode::CredentialMissing,
            message: "managed MCP credential authority credential is missing",
        }
    }

    const fn deleted_credential() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::DeletedCredential,
            remediation: RemediationCode::RecreateCredential,
            code: McpPlatformErrorCode::CredentialMissing,
            message: "managed MCP credential authority credential is deleted",
        }
    }

    const fn legacy_conflict() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::LegacyConflict,
            remediation: RemediationCode::TrustedReenrollment,
            code: McpPlatformErrorCode::IntegrityError,
            message: "managed MCP credential authority record requires trusted re-enrollment",
        }
    }

    const fn witness_expired() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::WitnessExpired,
            remediation: RemediationCode::ReissueWitness,
            code: McpPlatformErrorCode::IntegrityError,
            message: "managed MCP credential authority witness expired",
        }
    }

    const fn witness_consumed() -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::WitnessConsumed,
            remediation: RemediationCode::ReissueWitness,
            code: McpPlatformErrorCode::IntegrityError,
            message: "managed MCP credential authority witness was already consumed",
        }
    }

    const fn witness_mismatch(remediation: RemediationCode) -> Self {
        Self {
            kind: CredentialAuthorityErrorKind::WitnessMismatch,
            remediation,
            code: McpPlatformErrorCode::IntegrityError,
            message: "managed MCP credential authority witness no longer matches current authority",
        }
    }
}

impl fmt::Display for CredentialAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for CredentialAuthorityError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialAuthorityProviderHealth {
    Ready,
    Unavailable { reason: &'static str },
    Unsupported { reason: &'static str },
}

pub(crate) trait CredentialAuthorityProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn health(&self) -> CredentialAuthorityProviderHealth;
    fn load_root(&self) -> Result<Option<StoredAuthorityRootRecord>, CredentialAuthorityError>;
    fn persist_root(
        &self,
        root: &StoredAuthorityRootRecord,
    ) -> Result<(), CredentialAuthorityError>;
    fn load_anchor_root(&self) -> Result<Option<StoredAnchorRootRecord>, CredentialAuthorityError>;
    fn persist_anchor_root(
        &self,
        root: &StoredAnchorRootRecord,
    ) -> Result<(), CredentialAuthorityError>;
    fn load_record(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Option<StoredCredentialRecord>, CredentialAuthorityError>;
    fn persist_record(
        &self,
        handle: &CredentialHandle,
        record: &StoredCredentialRecord,
    ) -> Result<(), CredentialAuthorityError>;
    fn load_anchor(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Option<StoredCredentialAnchorRecord>, CredentialAuthorityError>;
    fn persist_anchor(
        &self,
        handle: &CredentialHandle,
        anchor: &StoredCredentialAnchorRecord,
    ) -> Result<(), CredentialAuthorityError>;

    #[cfg(test)]
    fn stored_record_count_for_testing(&self) -> usize {
        0
    }
}

struct CredentialAuthorityCapabilityIssuer(());

pub(crate) struct CredentialAuthorityWriteCapability {
    issuer: Arc<CredentialAuthorityCapabilityIssuer>,
}

impl CredentialAuthorityWriteCapability {
    fn issued_by(&self, issuer: &Arc<CredentialAuthorityCapabilityIssuer>) -> bool {
        Arc::ptr_eq(&self.issuer, issuer)
    }
}

struct WitnessSession {
    session_id: String,
    mac_key: [u8; KEY_BYTES],
}

trait TimeSource: Send + Sync {
    fn now_ms(&self) -> i64;
}

struct SystemTimeSource;

impl TimeSource for SystemTimeSource {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
}

pub(crate) struct CredentialAuthority {
    provider: Arc<dyn CredentialAuthorityProvider>,
    time_source: Arc<dyn TimeSource>,
    write_issuer: Arc<CredentialAuthorityCapabilityIssuer>,
    write_capability: CredentialAuthorityWriteCapability,
    witness_session: WitnessSession,
}

#[derive(Clone)]
pub(crate) struct EnrollmentAuthorityReader {
    provider: Arc<dyn CredentialAuthorityProvider>,
    time_source: Arc<dyn TimeSource>,
}

impl fmt::Debug for EnrollmentAuthorityReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollmentAuthorityReader")
            .field("provider_id", &self.provider.provider_id())
            .finish_non_exhaustive()
    }
}

impl EnrollmentAuthorityReader {
    pub(crate) fn authority_summary(&self) -> EnrollmentAuthoritySummary {
        EnrollmentAuthoritySummary::best_effort_single_process(self.provider.provider_id())
    }

    pub(crate) fn provider_health(&self) -> CredentialAuthorityProviderHealth {
        self.provider.health()
    }

    pub(crate) fn readiness_snapshot(
        &self,
        handle: &CredentialHandle,
        binding: &CredentialReadinessBinding,
    ) -> Result<CredentialReadinessSnapshot, CredentialAuthorityError> {
        CredentialAuthority::new(self.provider.clone(), self.time_source.clone())
            .readiness_snapshot(handle, binding)
    }
}

impl CredentialAuthority {
    pub(crate) fn production_default() -> Self {
        Self::new(production_provider(), Arc::new(SystemTimeSource))
    }

    fn new(
        provider: Arc<dyn CredentialAuthorityProvider>,
        time_source: Arc<dyn TimeSource>,
    ) -> Self {
        let mut mac_key = [0_u8; KEY_BYTES];
        rand::rng().fill(&mut mac_key);
        let write_issuer = Arc::new(CredentialAuthorityCapabilityIssuer(()));
        Self {
            provider,
            time_source,
            write_issuer: write_issuer.clone(),
            write_capability: CredentialAuthorityWriteCapability {
                issuer: write_issuer,
            },
            witness_session: WitnessSession {
                session_id: Uuid::new_v4().to_string(),
                mac_key,
            },
        }
    }

    #[cfg(test)]
    fn with_test_provider(
        provider: Arc<dyn CredentialAuthorityProvider>,
        time_source: Arc<dyn TimeSource>,
    ) -> Self {
        Self::new(provider, time_source)
    }

    #[cfg(test)]
    pub(crate) fn in_memory_for_testing(time_source: Arc<dyn TimeSource>) -> Self {
        Self::new(
            Arc::new(InMemoryCredentialAuthorityProvider::default()),
            time_source,
        )
    }

    #[cfg(test)]
    pub(crate) fn in_memory_for_testing_default_time() -> Self {
        Self::new(
            Arc::new(InMemoryCredentialAuthorityProvider::default()),
            Arc::new(SystemTimeSource),
        )
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_testing_default_time() -> Self {
        Self::with_test_provider(
            Arc::new(InMemoryCredentialAuthorityProvider::with_mode(
                TestProviderMode::Unavailable,
            )),
            Arc::new(SystemTimeSource),
        )
    }

    #[cfg(test)]
    pub(crate) fn unsupported_for_testing_default_time() -> Self {
        Self::with_test_provider(
            Arc::new(InMemoryCredentialAuthorityProvider::with_mode(
                TestProviderMode::Unsupported,
            )),
            Arc::new(SystemTimeSource),
        )
    }

    pub(crate) fn provider_id(&self) -> &str {
        self.provider.provider_id()
    }

    pub(crate) fn enrollment_reader(&self) -> EnrollmentAuthorityReader {
        EnrollmentAuthorityReader {
            provider: self.provider.clone(),
            time_source: self.time_source.clone(),
        }
    }

    pub(crate) fn write_capability(&self) -> &CredentialAuthorityWriteCapability {
        &self.write_capability
    }

    #[cfg(test)]
    pub(crate) fn stored_record_count_for_testing(&self) -> usize {
        self.provider.stored_record_count_for_testing()
    }

    pub(crate) fn readiness_snapshot(
        &self,
        handle: &CredentialHandle,
        binding: &CredentialReadinessBinding,
    ) -> Result<CredentialReadinessSnapshot, CredentialAuthorityError> {
        self.require_provider_ready()?;
        let AnchoredCredentialState {
            root,
            anchor_root,
            record,
        } = self.load_committed_state(handle)?;
        let binding_digest = binding_digest(binding);
        let status = if record.active {
            CredentialReadinessStatus::Ready
        } else {
            CredentialReadinessStatus::Deleted
        };
        let remediation = if status == CredentialReadinessStatus::Ready {
            None
        } else {
            Some(RemediationCode::RecreateCredential)
        };
        Ok(CredentialReadinessSnapshot {
            handle: handle.clone(),
            status,
            provider_id: root.provider_id.clone(),
            root_instance_id: root.instance_id.clone(),
            key_epoch: root.key_epoch,
            anchor_root_instance_id: anchor_root.instance_id.clone(),
            anchor_key_epoch: anchor_root.key_epoch,
            generation: record.generation,
            secret_revision: record.secret_revision,
            binding_digest,
            evidence_digest: stable_evidence_digest(&root, handle, &record),
            operation_context: binding.operation_context(),
            remediation,
        })
    }

    pub(crate) fn mint_readiness_witness(
        &self,
        snapshot: &CredentialReadinessSnapshot,
        ttl: Duration,
    ) -> Result<CredentialReadinessWitness, CredentialAuthorityError> {
        snapshot.require_ready()?;
        let issued_at_ms = self.time_source.now_ms();
        let expires_at_ms = issued_at_ms.saturating_add(ttl.as_millis() as i64);
        let claims = WitnessClaims {
            operation_context: snapshot.operation_context.to_string(),
            handle: snapshot.handle.as_str().to_string(),
            provider_id: snapshot.provider_id.clone(),
            root_instance_id: snapshot.root_instance_id.clone(),
            key_epoch: snapshot.key_epoch,
            anchor_root_instance_id: snapshot.anchor_root_instance_id.clone(),
            anchor_key_epoch: snapshot.anchor_key_epoch,
            generation: snapshot.generation,
            secret_revision: snapshot.secret_revision,
            binding_digest: snapshot.binding_digest.clone(),
            evidence_digest: snapshot.evidence_digest.clone(),
            issued_at_ms,
            expires_at_ms,
            session_id: self.witness_session.session_id.clone(),
            active: true,
        };
        Ok(CredentialReadinessWitness {
            claims: claims.clone(),
            mac: self.sign_witness_claims(&claims),
            consumed: AtomicBool::new(false),
        })
    }

    pub(crate) fn verify_readiness_witness(
        &self,
        witness: &CredentialReadinessWitness,
        handle: &CredentialHandle,
        binding: &CredentialReadinessBinding,
    ) -> Result<CredentialReadinessSnapshot, CredentialAuthorityError> {
        let snapshot = self.readiness_snapshot(handle, binding)?;
        snapshot.require_ready()?;
        witness.verify(self, &snapshot, handle, binding)?;
        Ok(snapshot)
    }

    fn store_secret_internal(
        &self,
        handle: &CredentialHandle,
        secret: &[u8],
    ) -> Result<(), CredentialAuthorityError> {
        self.require_provider_ready()?;
        if secret.is_empty() {
            return Err(CredentialAuthorityError::missing_credential());
        }
        let (root, anchor_root) = self.ensure_roots()?;
        let current = self.load_current_state_for_write(handle, &root, &anchor_root)?;
        let next = match current {
            Some(record) if record.active => StoredCredentialRecord::active(
                handle.as_str().to_string(),
                &root,
                record.generation,
                record.secret_revision.saturating_add(1),
                secret,
            ),
            Some(record) => StoredCredentialRecord::active(
                handle.as_str().to_string(),
                &root,
                record.generation.saturating_add(1),
                1,
                secret,
            ),
            None => {
                StoredCredentialRecord::active(handle.as_str().to_string(), &root, 1, 1, secret)
            }
        };
        let next_state = next.clone().validate(handle, &root)?;
        let next_anchor = StoredCredentialAnchorRecord::from_record_state(
            handle,
            &root,
            &anchor_root,
            &next_state,
        );
        self.provider.persist_anchor(handle, &next_anchor)?;
        self.provider.persist_record(handle, &next)?;
        let persisted = self.load_committed_state(handle)?;
        if persisted.record != next_state
            || persisted.root != root
            || persisted.anchor_root != anchor_root
        {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn store_secret_for_trusted_write(
        &self,
        capability: &CredentialAuthorityWriteCapability,
        handle: &CredentialHandle,
        secret: &[u8],
    ) -> Result<(), CredentialAuthorityError> {
        if !capability.issued_by(&self.write_issuer) {
            return Err(CredentialAuthorityError::capability_required());
        }
        self.store_secret_internal(handle, secret)
    }

    #[cfg(test)]
    pub(crate) fn delete_secret_for_trusted_write(
        &self,
        capability: &CredentialAuthorityWriteCapability,
        handle: &CredentialHandle,
    ) -> Result<(), CredentialAuthorityError> {
        if !capability.issued_by(&self.write_issuer) {
            return Err(CredentialAuthorityError::capability_required());
        }
        self.require_provider_ready()?;
        let (root, anchor_root) = self.ensure_roots()?;
        let current = self
            .load_current_state_for_write(handle, &root, &anchor_root)?
            .ok_or_else(CredentialAuthorityError::missing_credential)?;
        if !current.active {
            return Err(CredentialAuthorityError::deleted_credential());
        }
        let next = StoredCredentialRecord::deleted(
            handle.as_str().to_string(),
            &root,
            current.generation.saturating_add(1),
            current.secret_revision,
        );
        let next_state = next.clone().validate(handle, &root)?;
        let next_anchor = StoredCredentialAnchorRecord::from_record_state(
            handle,
            &root,
            &anchor_root,
            &next_state,
        );
        self.provider.persist_anchor(handle, &next_anchor)?;
        self.provider.persist_record(handle, &next)?;
        let persisted = self.load_committed_state(handle)?;
        if persisted.record != next_state
            || persisted.root != root
            || persisted.anchor_root != anchor_root
        {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        Ok(())
    }

    fn require_provider_ready(&self) -> Result<(), CredentialAuthorityError> {
        match self.provider.health() {
            CredentialAuthorityProviderHealth::Ready => Ok(()),
            CredentialAuthorityProviderHealth::Unavailable { .. } => {
                Err(CredentialAuthorityError::unavailable())
            }
            CredentialAuthorityProviderHealth::Unsupported { .. } => {
                Err(CredentialAuthorityError::unsupported())
            }
        }
    }

    fn load_root(&self) -> Result<AuthorityRoot, CredentialAuthorityError> {
        let stored = self
            .provider
            .load_root()?
            .ok_or_else(CredentialAuthorityError::missing_root)?;
        stored.validate(self.provider.provider_id())
    }

    fn load_root_for_read(
        &self,
        handle: &CredentialHandle,
    ) -> Result<AuthorityRoot, CredentialAuthorityError> {
        match self.provider.load_root()? {
            Some(stored) => stored.validate(self.provider.provider_id()),
            None => {
                if self.has_missing_root_residual_state(handle)? {
                    Err(CredentialAuthorityError::missing_root_with_residual_state())
                } else {
                    Err(CredentialAuthorityError::missing_root())
                }
            }
        }
    }

    fn has_missing_root_residual_state(
        &self,
        handle: &CredentialHandle,
    ) -> Result<bool, CredentialAuthorityError> {
        if self.provider.load_anchor_root()?.is_some() {
            return Ok(true);
        }
        if self.provider.load_anchor(handle)?.is_some() {
            return Ok(true);
        }
        if self.provider.load_record(handle)?.is_some() {
            return Ok(true);
        }
        Ok(false)
    }

    fn load_anchor_root(&self) -> Result<AnchorRoot, CredentialAuthorityError> {
        let stored = self
            .provider
            .load_anchor_root()?
            .ok_or_else(CredentialAuthorityError::legacy_conflict)?;
        stored.validate(self.provider.provider_id())
    }

    fn ensure_roots(&self) -> Result<(AuthorityRoot, AnchorRoot), CredentialAuthorityError> {
        match (
            self.provider.load_root()?,
            self.provider.load_anchor_root()?,
        ) {
            (Some(root), Some(anchor_root)) => Ok((
                root.validate(self.provider.provider_id())?,
                anchor_root.validate(self.provider.provider_id())?,
            )),
            (None, None) => {
                let mut evidence_key = [0_u8; KEY_BYTES];
                rand::rng().fill(&mut evidence_key);
                let root = StoredAuthorityRootRecord {
                    format_version: ROOT_FORMAT_VERSION,
                    namespace: ROOT_NAMESPACE.to_string(),
                    provider_id: self.provider.provider_id().to_string(),
                    instance_id: Uuid::new_v4().to_string(),
                    key_epoch: 1,
                    evidence_key: base64::engine::general_purpose::STANDARD_NO_PAD
                        .encode(evidence_key),
                };
                let mut anchor_key = [0_u8; KEY_BYTES];
                rand::rng().fill(&mut anchor_key);
                let anchor_root = StoredAnchorRootRecord {
                    format_version: ANCHOR_ROOT_FORMAT_VERSION,
                    namespace: ANCHOR_ROOT_NAMESPACE.to_string(),
                    provider_id: self.provider.provider_id().to_string(),
                    instance_id: Uuid::new_v4().to_string(),
                    key_epoch: 1,
                    anchor_key: base64::engine::general_purpose::STANDARD_NO_PAD.encode(anchor_key),
                };
                self.provider.persist_root(&root)?;
                self.provider.persist_anchor_root(&anchor_root)?;
                Ok((self.load_root()?, self.load_anchor_root()?))
            }
            _ => Err(CredentialAuthorityError::legacy_conflict()),
        }
    }

    fn load_record(
        &self,
        handle: &CredentialHandle,
        root: &AuthorityRoot,
    ) -> Result<CredentialRecordState, CredentialAuthorityError> {
        self.load_record_optional(handle, root)?
            .ok_or_else(CredentialAuthorityError::missing_credential)
    }

    fn load_record_optional(
        &self,
        handle: &CredentialHandle,
        root: &AuthorityRoot,
    ) -> Result<Option<CredentialRecordState>, CredentialAuthorityError> {
        self.provider
            .load_record(handle)?
            .map(|record| record.validate(handle, root))
            .transpose()
    }

    fn load_anchor_optional(
        &self,
        handle: &CredentialHandle,
        root: &AuthorityRoot,
        anchor_root: &AnchorRoot,
    ) -> Result<Option<CredentialAnchorState>, CredentialAuthorityError> {
        self.provider
            .load_anchor(handle)?
            .map(|anchor| anchor.validate(handle, root, anchor_root))
            .transpose()
    }

    fn load_current_state_for_write(
        &self,
        handle: &CredentialHandle,
        root: &AuthorityRoot,
        anchor_root: &AnchorRoot,
    ) -> Result<Option<CredentialRecordState>, CredentialAuthorityError> {
        let anchor = self.load_anchor_optional(handle, root, anchor_root)?;
        let record = self.load_record_optional(handle, root)?;
        match (anchor, record) {
            (None, None) => Ok(None),
            (Some(_), None) | (None, Some(_)) => Err(CredentialAuthorityError::legacy_conflict()),
            (Some(anchor), Some(record)) => {
                if record.matches_anchor(&anchor) {
                    Ok(Some(record))
                } else {
                    Err(CredentialAuthorityError::legacy_conflict())
                }
            }
        }
    }

    fn load_committed_state(
        &self,
        handle: &CredentialHandle,
    ) -> Result<AnchoredCredentialState, CredentialAuthorityError> {
        let root_before = self.load_root_for_read(handle)?;
        let anchor_root_before = self.load_anchor_root()?;
        let anchor_before = self.load_anchor_optional(handle, &root_before, &anchor_root_before)?;
        let record_before = self.load_record_optional(handle, &root_before)?;
        let anchor_after = self.load_anchor_optional(handle, &root_before, &anchor_root_before)?;
        let anchor_root_after = self.load_anchor_root()?;
        let root_after = self.load_root_for_read(handle)?;
        let record_after = self.load_record_optional(handle, &root_before)?;
        if root_before != root_after
            || anchor_root_before != anchor_root_after
            || anchor_before != anchor_after
            || record_before != record_after
        {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        match (anchor_before, record_before) {
            (None, None) => Err(CredentialAuthorityError::missing_credential()),
            (Some(_), None) | (None, Some(_)) => Err(CredentialAuthorityError::legacy_conflict()),
            (Some(anchor), Some(record)) => {
                if !record.matches_anchor(&anchor) {
                    return Err(CredentialAuthorityError::legacy_conflict());
                }
                Ok(AnchoredCredentialState {
                    root: root_before,
                    anchor_root: anchor_root_before,
                    record,
                })
            }
        }
    }

    fn sign_witness_claims(&self, claims: &WitnessClaims) -> [u8; 32] {
        hmac_sha256(
            &self.witness_session.mac_key,
            &integrity_envelope(
                "credential_readiness_witness_v1",
                &claims.canonical_payload(),
            ),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnrollmentAuthoritySummary {
    pub provider_id: String,
    pub writer_mode: String,
}

impl EnrollmentAuthoritySummary {
    pub(crate) fn best_effort_single_process(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            writer_mode: "best_effort_single_process".to_string(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct EnrollmentWriteResult {
    pub handle: String,
    pub status: CredentialReadinessStatus,
    pub evidence_digest: String,
    pub authority: EnrollmentAuthoritySummary,
}

impl fmt::Debug for EnrollmentWriteResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollmentWriteResult")
            .field("handle", &"[REDACTED]")
            .field("status", &self.status)
            .field("evidence_digest", &"[REDACTED]")
            .field("authority", &self.authority)
            .finish()
    }
}

pub(crate) struct EnrollmentWriterOwner {
    authority: CredentialAuthority,
    instance_id: String,
}

impl fmt::Debug for EnrollmentWriterOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollmentWriterOwner")
            .field("provider_id", &self.authority.provider_id())
            .field("instance_id", &self.instance_id)
            .finish_non_exhaustive()
    }
}

impl EnrollmentWriterOwner {
    pub(crate) fn production_default() -> Self {
        Self::new(CredentialAuthority::production_default())
    }

    #[cfg(test)]
    pub(crate) fn in_memory_for_testing(time_source: Arc<dyn TimeSource>) -> Self {
        Self::new(CredentialAuthority::in_memory_for_testing(time_source))
    }

    #[cfg(test)]
    pub(crate) fn in_memory_for_testing_default_time() -> Self {
        Self::new(CredentialAuthority::in_memory_for_testing_default_time())
    }

    #[cfg(test)]
    pub(crate) fn with_test_authority(authority: CredentialAuthority) -> Self {
        Self::new(authority)
    }

    fn new(authority: CredentialAuthority) -> Self {
        Self {
            authority,
            instance_id: Uuid::new_v4().to_string(),
        }
    }

    pub(crate) fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub(crate) fn authority_summary(&self) -> EnrollmentAuthoritySummary {
        EnrollmentAuthoritySummary::best_effort_single_process(self.authority.provider_id())
    }

    pub(crate) fn authority_reader(&self) -> EnrollmentAuthorityReader {
        self.authority.enrollment_reader()
    }

    pub(crate) fn write_validated_secret(
        &self,
        handle: &CredentialHandle,
        secret: &[u8],
        binding: &CredentialReadinessBinding,
    ) -> Result<EnrollmentWriteResult, CredentialAuthorityError> {
        self.authority.store_secret_internal(handle, secret)?;
        let snapshot = self.authority.readiness_snapshot(handle, binding)?;
        Ok(EnrollmentWriteResult {
            handle: handle.as_str().to_string(),
            status: snapshot.status(),
            evidence_digest: snapshot.evidence_digest().to_string(),
            authority: self.authority_summary(),
        })
    }

    #[cfg(test)]
    pub(crate) fn stored_record_count_for_testing(&self) -> usize {
        self.authority.stored_record_count_for_testing()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AuthorityRoot {
    provider_id: String,
    instance_id: String,
    key_epoch: u64,
    evidence_key: [u8; KEY_BYTES],
}

#[derive(Clone, PartialEq, Eq)]
struct AnchorRoot {
    provider_id: String,
    instance_id: String,
    key_epoch: u64,
    anchor_key: [u8; KEY_BYTES],
}

#[derive(Clone, PartialEq, Eq)]
struct CredentialRecordState {
    generation: u64,
    secret_revision: u64,
    active: bool,
    payload_binding: [u8; 32],
}

#[derive(Clone, PartialEq, Eq)]
struct CredentialAnchorState {
    generation: u64,
    secret_revision: u64,
    active: bool,
    payload_binding: [u8; 32],
}

struct AnchoredCredentialState {
    root: AuthorityRoot,
    anchor_root: AnchorRoot,
    record: CredentialRecordState,
}

impl CredentialRecordState {
    fn matches_anchor(&self, anchor: &CredentialAnchorState) -> bool {
        self.generation == anchor.generation
            && self.secret_revision == anchor.secret_revision
            && self.active == anchor.active
            && constant_time_eq(&self.payload_binding, &anchor.payload_binding)
    }
}

#[derive(Clone)]
struct WitnessClaims {
    operation_context: String,
    handle: String,
    provider_id: String,
    root_instance_id: String,
    key_epoch: u64,
    anchor_root_instance_id: String,
    anchor_key_epoch: u64,
    generation: u64,
    secret_revision: u64,
    binding_digest: String,
    evidence_digest: String,
    issued_at_ms: i64,
    expires_at_ms: i64,
    session_id: String,
    active: bool,
}

impl WitnessClaims {
    fn canonical_payload(&self) -> Vec<u8> {
        canonical_fields(&[
            self.operation_context.as_bytes(),
            self.handle.as_bytes(),
            self.provider_id.as_bytes(),
            self.root_instance_id.as_bytes(),
            self.key_epoch.to_string().as_bytes(),
            self.anchor_root_instance_id.as_bytes(),
            self.anchor_key_epoch.to_string().as_bytes(),
            self.generation.to_string().as_bytes(),
            self.secret_revision.to_string().as_bytes(),
            self.binding_digest.as_bytes(),
            self.evidence_digest.as_bytes(),
            self.issued_at_ms.to_string().as_bytes(),
            self.expires_at_ms.to_string().as_bytes(),
            self.session_id.as_bytes(),
            bool_flag(self.active),
        ])
    }
}

pub(crate) struct CredentialReadinessWitness {
    claims: WitnessClaims,
    mac: [u8; 32],
    consumed: AtomicBool,
}

impl CredentialReadinessWitness {
    fn verify(
        &self,
        authority: &CredentialAuthority,
        snapshot: &CredentialReadinessSnapshot,
        handle: &CredentialHandle,
        binding: &CredentialReadinessBinding,
    ) -> Result<(), CredentialAuthorityError> {
        let now_ms = authority.time_source.now_ms();
        if now_ms > self.claims.expires_at_ms || now_ms < self.claims.issued_at_ms {
            return Err(CredentialAuthorityError::witness_expired());
        }
        if self.claims.session_id != authority.witness_session.session_id {
            return Err(CredentialAuthorityError::witness_mismatch(
                RemediationCode::RestartAuthoritySession,
            ));
        }
        if self.claims.operation_context != binding.operation_context()
            || self.claims.handle != handle.as_str()
            || self.claims.provider_id != snapshot.provider_id
            || self.claims.root_instance_id != snapshot.root_instance_id
            || self.claims.key_epoch != snapshot.key_epoch
            || self.claims.anchor_root_instance_id != snapshot.anchor_root_instance_id
            || self.claims.anchor_key_epoch != snapshot.anchor_key_epoch
            || self.claims.generation != snapshot.generation
            || self.claims.secret_revision != snapshot.secret_revision
            || self.claims.binding_digest != snapshot.binding_digest
            || self.claims.evidence_digest != snapshot.evidence_digest
            || !self.claims.active
        {
            return Err(CredentialAuthorityError::witness_mismatch(
                RemediationCode::VerifyBinding,
            ));
        }
        let expected_mac = authority.sign_witness_claims(&self.claims);
        if !constant_time_eq(&expected_mac, &self.mac) {
            return Err(CredentialAuthorityError::witness_mismatch(
                RemediationCode::VerifyBinding,
            ));
        }
        if self.consumed.swap(true, Ordering::AcqRel) {
            return Err(CredentialAuthorityError::witness_consumed());
        }
        Ok(())
    }

    #[cfg(test)]
    fn tampered(
        &self,
        mutate: impl FnOnce(&mut WitnessClaims, &mut [u8; 32]),
    ) -> CredentialReadinessWitness {
        let mut claims = self.claims.clone();
        let mut mac = self.mac;
        mutate(&mut claims, &mut mac);
        CredentialReadinessWitness {
            claims,
            mac,
            consumed: AtomicBool::new(false),
        }
    }
}

impl fmt::Debug for CredentialReadinessWitness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialReadinessWitness([REDACTED])")
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredAuthorityRootRecord {
    format_version: u8,
    namespace: String,
    provider_id: String,
    instance_id: String,
    key_epoch: u64,
    evidence_key: String,
}

impl StoredAuthorityRootRecord {
    fn validate(
        self,
        expected_provider_id: &str,
    ) -> Result<AuthorityRoot, CredentialAuthorityError> {
        if self.format_version != ROOT_FORMAT_VERSION
            || self.namespace != ROOT_NAMESPACE
            || self.provider_id != expected_provider_id
            || self.instance_id.is_empty()
            || self.key_epoch == 0
        {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        let evidence_key = base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(self.evidence_key.as_bytes())
            .map_err(|_| CredentialAuthorityError::legacy_conflict())?;
        if evidence_key.len() != KEY_BYTES {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        let mut key = [0_u8; KEY_BYTES];
        key.copy_from_slice(&evidence_key);
        Ok(AuthorityRoot {
            provider_id: self.provider_id,
            instance_id: self.instance_id,
            key_epoch: self.key_epoch,
            evidence_key: key,
        })
    }
}

impl fmt::Debug for StoredAuthorityRootRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredAuthorityRootRecord")
            .field("format_version", &self.format_version)
            .field("namespace", &self.namespace)
            .field("provider_id", &self.provider_id)
            .field("instance_id", &self.instance_id)
            .field("key_epoch", &self.key_epoch)
            .field("evidence_key", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredAnchorRootRecord {
    format_version: u8,
    namespace: String,
    provider_id: String,
    instance_id: String,
    key_epoch: u64,
    anchor_key: String,
}

impl StoredAnchorRootRecord {
    fn validate(self, expected_provider_id: &str) -> Result<AnchorRoot, CredentialAuthorityError> {
        if self.format_version != ANCHOR_ROOT_FORMAT_VERSION
            || self.namespace != ANCHOR_ROOT_NAMESPACE
            || self.provider_id != expected_provider_id
            || self.instance_id.is_empty()
            || self.key_epoch == 0
        {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        let anchor_key = base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(self.anchor_key.as_bytes())
            .map_err(|_| CredentialAuthorityError::legacy_conflict())?;
        if anchor_key.len() != KEY_BYTES {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        let mut key = [0_u8; KEY_BYTES];
        key.copy_from_slice(&anchor_key);
        Ok(AnchorRoot {
            provider_id: self.provider_id,
            instance_id: self.instance_id,
            key_epoch: self.key_epoch,
            anchor_key: key,
        })
    }
}

impl fmt::Debug for StoredAnchorRootRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredAnchorRootRecord")
            .field("format_version", &self.format_version)
            .field("namespace", &self.namespace)
            .field("provider_id", &self.provider_id)
            .field("instance_id", &self.instance_id)
            .field("key_epoch", &self.key_epoch)
            .field("anchor_key", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StoredCredentialStatus {
    Active,
    Deleted,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum StoredSecretPayload {
    InlineBase64 { payload_b64: String },
}

impl fmt::Debug for StoredSecretPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoredSecretPayload([REDACTED])")
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredCredentialRecord {
    format_version: u8,
    handle: String,
    provider_id: String,
    authority_instance_id: String,
    key_epoch: u64,
    generation: u64,
    secret_revision: u64,
    status: StoredCredentialStatus,
    payload: Option<StoredSecretPayload>,
    payload_binding_b64: Option<String>,
}

impl StoredCredentialRecord {
    fn active(
        handle: String,
        root: &AuthorityRoot,
        generation: u64,
        secret_revision: u64,
        secret: &[u8],
    ) -> Self {
        let payload_binding_b64 =
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(payload_binding_mac(
                root,
                &handle,
                generation,
                secret_revision,
                StoredCredentialStatus::Active.as_storage_label(),
                secret,
            ));
        Self {
            format_version: RECORD_FORMAT_VERSION,
            handle,
            provider_id: root.provider_id.clone(),
            authority_instance_id: root.instance_id.clone(),
            key_epoch: root.key_epoch,
            generation,
            secret_revision,
            status: StoredCredentialStatus::Active,
            payload: Some(StoredSecretPayload::InlineBase64 {
                payload_b64: base64::engine::general_purpose::STANDARD_NO_PAD.encode(secret),
            }),
            payload_binding_b64: Some(payload_binding_b64),
        }
    }

    fn deleted(
        handle: String,
        root: &AuthorityRoot,
        generation: u64,
        secret_revision: u64,
    ) -> Self {
        let payload_binding_b64 =
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(payload_binding_mac(
                root,
                &handle,
                generation,
                secret_revision,
                StoredCredentialStatus::Deleted.as_storage_label(),
                &[],
            ));
        Self {
            format_version: RECORD_FORMAT_VERSION,
            handle,
            provider_id: root.provider_id.clone(),
            authority_instance_id: root.instance_id.clone(),
            key_epoch: root.key_epoch,
            generation,
            secret_revision,
            status: StoredCredentialStatus::Deleted,
            payload: None,
            payload_binding_b64: Some(payload_binding_b64),
        }
    }

    fn validate(
        self,
        handle: &CredentialHandle,
        root: &AuthorityRoot,
    ) -> Result<CredentialRecordState, CredentialAuthorityError> {
        if self.format_version != RECORD_FORMAT_VERSION
            || self.handle != handle.as_str()
            || self.provider_id != root.provider_id
            || self.authority_instance_id != root.instance_id
            || self.key_epoch != root.key_epoch
            || self.generation == 0
            || self.secret_revision == 0
        {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        match (&self.status, &self.payload) {
            (
                StoredCredentialStatus::Active,
                Some(StoredSecretPayload::InlineBase64 { payload_b64 }),
            ) => {
                let payload = decode_canonical_base64(payload_b64)?;
                if payload.is_empty() {
                    return Err(CredentialAuthorityError::legacy_conflict());
                }
                let encoded_binding = self
                    .payload_binding_b64
                    .as_deref()
                    .ok_or_else(CredentialAuthorityError::legacy_conflict)?;
                let binding = decode_fixed_canonical_base64::<32>(encoded_binding)?;
                let expected_binding = payload_binding_mac(
                    root,
                    &self.handle,
                    self.generation,
                    self.secret_revision,
                    self.status.as_storage_label(),
                    &payload,
                );
                if !constant_time_eq(&binding, &expected_binding) {
                    return Err(CredentialAuthorityError::legacy_conflict());
                }
                Ok(CredentialRecordState {
                    generation: self.generation,
                    secret_revision: self.secret_revision,
                    active: true,
                    payload_binding: binding,
                })
            }
            (StoredCredentialStatus::Deleted, None) => {
                let encoded_binding = self
                    .payload_binding_b64
                    .as_deref()
                    .ok_or_else(CredentialAuthorityError::legacy_conflict)?;
                let binding = decode_fixed_canonical_base64::<32>(encoded_binding)?;
                let expected_binding = payload_binding_mac(
                    root,
                    &self.handle,
                    self.generation,
                    self.secret_revision,
                    self.status.as_storage_label(),
                    &[],
                );
                if !constant_time_eq(&binding, &expected_binding) {
                    return Err(CredentialAuthorityError::legacy_conflict());
                }
                Ok(CredentialRecordState {
                    generation: self.generation,
                    secret_revision: self.secret_revision,
                    active: false,
                    payload_binding: binding,
                })
            }
            _ => Err(CredentialAuthorityError::legacy_conflict()),
        }
    }
}

impl fmt::Debug for StoredCredentialRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredCredentialRecord")
            .field("format_version", &self.format_version)
            .field("handle", &self.handle)
            .field("provider_id", &self.provider_id)
            .field("authority_instance_id", &self.authority_instance_id)
            .field("key_epoch", &self.key_epoch)
            .field("generation", &self.generation)
            .field("secret_revision", &self.secret_revision)
            .field("status", &self.status)
            .field("payload", &"[REDACTED]")
            .field("payload_binding_b64", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for StoredCredentialStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Active => "Active",
            Self::Deleted => "Deleted",
        })
    }
}

impl StoredCredentialStatus {
    fn as_storage_label(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredCredentialAnchorRecord {
    format_version: u8,
    namespace: String,
    handle: String,
    authority_provider_id: String,
    authority_instance_id: String,
    authority_key_epoch: u64,
    anchor_provider_id: String,
    anchor_instance_id: String,
    anchor_key_epoch: u64,
    generation: u64,
    secret_revision: u64,
    status: StoredCredentialStatus,
    record_binding_b64: String,
    anchor_mac_b64: String,
}

impl StoredCredentialAnchorRecord {
    fn from_record_state(
        handle: &CredentialHandle,
        root: &AuthorityRoot,
        anchor_root: &AnchorRoot,
        record: &CredentialRecordState,
    ) -> Self {
        Self {
            format_version: HANDLE_ANCHOR_FORMAT_VERSION,
            namespace: HANDLE_ANCHOR_NAMESPACE.to_string(),
            handle: handle.as_str().to_string(),
            authority_provider_id: root.provider_id.clone(),
            authority_instance_id: root.instance_id.clone(),
            authority_key_epoch: root.key_epoch,
            anchor_provider_id: anchor_root.provider_id.clone(),
            anchor_instance_id: anchor_root.instance_id.clone(),
            anchor_key_epoch: anchor_root.key_epoch,
            generation: record.generation,
            secret_revision: record.secret_revision,
            status: if record.active {
                StoredCredentialStatus::Active
            } else {
                StoredCredentialStatus::Deleted
            },
            record_binding_b64: base64::engine::general_purpose::STANDARD_NO_PAD
                .encode(record.payload_binding),
            anchor_mac_b64: base64::engine::general_purpose::STANDARD_NO_PAD.encode(
                handle_anchor_mac(
                    anchor_root,
                    handle.as_str(),
                    &root.provider_id,
                    &root.instance_id,
                    root.key_epoch,
                    record.generation,
                    record.secret_revision,
                    if record.active { "active" } else { "deleted" },
                    &record.payload_binding,
                ),
            ),
        }
    }

    fn validate(
        self,
        handle: &CredentialHandle,
        root: &AuthorityRoot,
        anchor_root: &AnchorRoot,
    ) -> Result<CredentialAnchorState, CredentialAuthorityError> {
        if self.format_version != HANDLE_ANCHOR_FORMAT_VERSION
            || self.namespace != HANDLE_ANCHOR_NAMESPACE
            || self.handle != handle.as_str()
            || self.authority_provider_id != root.provider_id
            || self.authority_instance_id != root.instance_id
            || self.authority_key_epoch != root.key_epoch
            || self.anchor_provider_id != anchor_root.provider_id
            || self.anchor_instance_id != anchor_root.instance_id
            || self.anchor_key_epoch != anchor_root.key_epoch
            || self.generation == 0
            || self.secret_revision == 0
        {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        let record_binding = decode_fixed_canonical_base64::<32>(&self.record_binding_b64)?;
        let anchor_mac = decode_fixed_canonical_base64::<32>(&self.anchor_mac_b64)?;
        let expected_anchor_mac = handle_anchor_mac(
            anchor_root,
            handle.as_str(),
            &self.authority_provider_id,
            &self.authority_instance_id,
            self.authority_key_epoch,
            self.generation,
            self.secret_revision,
            self.status.as_storage_label(),
            &record_binding,
        );
        if !constant_time_eq(&anchor_mac, &expected_anchor_mac) {
            return Err(CredentialAuthorityError::legacy_conflict());
        }
        Ok(CredentialAnchorState {
            generation: self.generation,
            secret_revision: self.secret_revision,
            active: matches!(self.status, StoredCredentialStatus::Active),
            payload_binding: record_binding,
        })
    }
}

impl fmt::Debug for StoredCredentialAnchorRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredCredentialAnchorRecord")
            .field("format_version", &self.format_version)
            .field("namespace", &self.namespace)
            .field("handle", &self.handle)
            .field("authority_provider_id", &self.authority_provider_id)
            .field("authority_instance_id", &self.authority_instance_id)
            .field("authority_key_epoch", &self.authority_key_epoch)
            .field("anchor_provider_id", &self.anchor_provider_id)
            .field("anchor_instance_id", &self.anchor_instance_id)
            .field("anchor_key_epoch", &self.anchor_key_epoch)
            .field("generation", &self.generation)
            .field("secret_revision", &self.secret_revision)
            .field("status", &self.status)
            .field("record_binding_b64", &"[REDACTED]")
            .field("anchor_mac_b64", &"[REDACTED]")
            .finish()
    }
}

fn stable_evidence_digest(
    root: &AuthorityRoot,
    handle: &CredentialHandle,
    record: &CredentialRecordState,
) -> String {
    crate::utils::bytes_to_hex(hmac_sha256(
        &root.evidence_key,
        &integrity_envelope(
            "credential_readiness_evidence_v1",
            &canonical_fields(&[
                handle.as_str().as_bytes(),
                record.generation.to_string().as_bytes(),
                record.secret_revision.to_string().as_bytes(),
                bool_flag(record.active),
                root.provider_id.as_bytes(),
                root.instance_id.as_bytes(),
                root.key_epoch.to_string().as_bytes(),
            ]),
        ),
    ))
}

fn binding_digest(binding: &CredentialReadinessBinding) -> String {
    digest(&[
        binding.operation_context().as_bytes(),
        &binding.canonical_payload(),
    ])
}

fn payload_binding_mac(
    root: &AuthorityRoot,
    handle: &str,
    generation: u64,
    secret_revision: u64,
    status: &str,
    payload: &[u8],
) -> [u8; 32] {
    let binding_key = hmac_sha256(
        &root.evidence_key,
        &integrity_envelope(PAYLOAD_BINDING_KEY_DOMAIN, &[]),
    );
    hmac_sha256(
        &binding_key,
        &integrity_envelope(
            PAYLOAD_BINDING_DOMAIN,
            &canonical_fields(&[
                root.provider_id.as_bytes(),
                root.instance_id.as_bytes(),
                root.key_epoch.to_string().as_bytes(),
                handle.as_bytes(),
                generation.to_string().as_bytes(),
                secret_revision.to_string().as_bytes(),
                status.as_bytes(),
                payload,
            ]),
        ),
    )
}

fn handle_anchor_mac(
    anchor_root: &AnchorRoot,
    handle: &str,
    authority_provider_id: &str,
    authority_instance_id: &str,
    authority_key_epoch: u64,
    generation: u64,
    secret_revision: u64,
    status: &str,
    record_binding: &[u8; 32],
) -> [u8; 32] {
    hmac_sha256(
        &anchor_root.anchor_key,
        &integrity_envelope(
            HANDLE_ANCHOR_DOMAIN,
            &canonical_fields(&[
                handle.as_bytes(),
                authority_provider_id.as_bytes(),
                authority_instance_id.as_bytes(),
                authority_key_epoch.to_string().as_bytes(),
                anchor_root.provider_id.as_bytes(),
                anchor_root.instance_id.as_bytes(),
                anchor_root.key_epoch.to_string().as_bytes(),
                generation.to_string().as_bytes(),
                secret_revision.to_string().as_bytes(),
                status.as_bytes(),
                record_binding,
            ]),
        ),
    )
}

fn canonical_fields(fields: &[&[u8]]) -> Vec<u8> {
    let mut payload = Vec::new();
    for field in fields {
        payload.extend_from_slice(&(field.len() as u64).to_be_bytes());
        payload.extend_from_slice(field);
    }
    payload
}

fn bool_flag(value: bool) -> &'static [u8] {
    if value {
        b"1"
    } else {
        b"0"
    }
}

fn integrity_envelope(domain: &str, canonical_payload: &[u8]) -> Vec<u8> {
    canonical_fields(&[
        ENVELOPE_DOMAIN,
        b"schema-v2",
        domain.as_bytes(),
        canonical_payload,
    ])
}

fn digest(fields: &[&[u8]]) -> String {
    crate::utils::bytes_to_hex(Sha256::digest(canonical_fields(fields)))
}

fn decode_canonical_base64(value: &str) -> Result<Vec<u8>, CredentialAuthorityError> {
    let decoded = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(value.as_bytes())
        .map_err(|_| CredentialAuthorityError::legacy_conflict())?;
    if base64::engine::general_purpose::STANDARD_NO_PAD.encode(&decoded) != value {
        return Err(CredentialAuthorityError::legacy_conflict());
    }
    Ok(decoded)
}

fn decode_fixed_canonical_base64<const N: usize>(
    value: &str,
) -> Result<[u8; N], CredentialAuthorityError> {
    let decoded = decode_canonical_base64(value)?;
    if decoded.len() != N {
        return Err(CredentialAuthorityError::legacy_conflict());
    }
    let mut bytes = [0_u8; N];
    bytes.copy_from_slice(&decoded);
    Ok(bytes)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;
    let mut normalized = [0_u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK_BYTES];
    let mut outer_pad = [0x5c_u8; BLOCK_BYTES];
    for index in 0..BLOCK_BYTES {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(feature = "system-keyring")]
struct SystemKeyringCredentialAuthorityProvider;

#[cfg(feature = "system-keyring")]
impl SystemKeyringCredentialAuthorityProvider {
    fn entry(service: &str, account: &str) -> Result<keyring::Entry, CredentialAuthorityError> {
        keyring::Entry::new(service, account).map_err(|_| CredentialAuthorityError::unavailable())
    }

    fn load_json<T>(service: &str, account: &str) -> Result<Option<T>, CredentialAuthorityError>
    where
        T: for<'de> Deserialize<'de>,
    {
        match Self::entry(service, account)?.get_password() {
            Ok(encoded) => serde_json::from_str(&encoded)
                .map(Some)
                .map_err(|_| CredentialAuthorityError::legacy_conflict()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(CredentialAuthorityError::unavailable()),
        }
    }

    fn persist_json<T>(
        service: &str,
        account: &str,
        value: &T,
    ) -> Result<(), CredentialAuthorityError>
    where
        T: Serialize,
    {
        let encoded = serde_json::to_string(value)
            .map_err(|_| CredentialAuthorityError::legacy_conflict())?;
        Self::entry(service, account)?
            .set_password(&encoded)
            .map_err(|_| CredentialAuthorityError::unavailable())
    }
}

#[cfg(feature = "system-keyring")]
impl CredentialAuthorityProvider for SystemKeyringCredentialAuthorityProvider {
    fn provider_id(&self) -> &str {
        SYSTEM_KEYRING_PROVIDER_ID
    }

    fn health(&self) -> CredentialAuthorityProviderHealth {
        CredentialAuthorityProviderHealth::Ready
    }

    fn load_root(&self) -> Result<Option<StoredAuthorityRootRecord>, CredentialAuthorityError> {
        Self::load_json(KEYRING_SERVICE, ROOT_ACCOUNT)
    }

    fn persist_root(
        &self,
        root: &StoredAuthorityRootRecord,
    ) -> Result<(), CredentialAuthorityError> {
        Self::persist_json(KEYRING_SERVICE, ROOT_ACCOUNT, root)
    }

    fn load_anchor_root(&self) -> Result<Option<StoredAnchorRootRecord>, CredentialAuthorityError> {
        Self::load_json(ANCHOR_KEYRING_SERVICE, ANCHOR_ROOT_ACCOUNT)
    }

    fn persist_anchor_root(
        &self,
        root: &StoredAnchorRootRecord,
    ) -> Result<(), CredentialAuthorityError> {
        Self::persist_json(ANCHOR_KEYRING_SERVICE, ANCHOR_ROOT_ACCOUNT, root)
    }

    fn load_record(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Option<StoredCredentialRecord>, CredentialAuthorityError> {
        Self::load_json(KEYRING_SERVICE, &record_account(handle))
    }

    fn persist_record(
        &self,
        handle: &CredentialHandle,
        record: &StoredCredentialRecord,
    ) -> Result<(), CredentialAuthorityError> {
        Self::persist_json(KEYRING_SERVICE, &record_account(handle), record)
    }

    fn load_anchor(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Option<StoredCredentialAnchorRecord>, CredentialAuthorityError> {
        Self::load_json(ANCHOR_KEYRING_SERVICE, &anchor_account(handle))
    }

    fn persist_anchor(
        &self,
        handle: &CredentialHandle,
        anchor: &StoredCredentialAnchorRecord,
    ) -> Result<(), CredentialAuthorityError> {
        Self::persist_json(ANCHOR_KEYRING_SERVICE, &anchor_account(handle), anchor)
    }
}

#[cfg(not(feature = "system-keyring"))]
struct UnsupportedCredentialAuthorityProvider;

#[cfg(not(feature = "system-keyring"))]
impl CredentialAuthorityProvider for UnsupportedCredentialAuthorityProvider {
    fn provider_id(&self) -> &str {
        UNSUPPORTED_PROVIDER_ID
    }

    fn health(&self) -> CredentialAuthorityProviderHealth {
        CredentialAuthorityProviderHealth::Unsupported {
            reason: SYSTEM_KEYRING_UNSUPPORTED_REASON,
        }
    }

    fn load_root(&self) -> Result<Option<StoredAuthorityRootRecord>, CredentialAuthorityError> {
        Err(CredentialAuthorityError::unsupported())
    }

    fn persist_root(
        &self,
        _root: &StoredAuthorityRootRecord,
    ) -> Result<(), CredentialAuthorityError> {
        Err(CredentialAuthorityError::unsupported())
    }

    fn load_anchor_root(&self) -> Result<Option<StoredAnchorRootRecord>, CredentialAuthorityError> {
        Err(CredentialAuthorityError::unsupported())
    }

    fn persist_anchor_root(
        &self,
        _root: &StoredAnchorRootRecord,
    ) -> Result<(), CredentialAuthorityError> {
        Err(CredentialAuthorityError::unsupported())
    }

    fn load_record(
        &self,
        _handle: &CredentialHandle,
    ) -> Result<Option<StoredCredentialRecord>, CredentialAuthorityError> {
        Err(CredentialAuthorityError::unsupported())
    }

    fn persist_record(
        &self,
        _handle: &CredentialHandle,
        _record: &StoredCredentialRecord,
    ) -> Result<(), CredentialAuthorityError> {
        Err(CredentialAuthorityError::unsupported())
    }

    fn load_anchor(
        &self,
        _handle: &CredentialHandle,
    ) -> Result<Option<StoredCredentialAnchorRecord>, CredentialAuthorityError> {
        Err(CredentialAuthorityError::unsupported())
    }

    fn persist_anchor(
        &self,
        _handle: &CredentialHandle,
        _anchor: &StoredCredentialAnchorRecord,
    ) -> Result<(), CredentialAuthorityError> {
        Err(CredentialAuthorityError::unsupported())
    }
}

fn production_provider() -> Arc<dyn CredentialAuthorityProvider> {
    #[cfg(feature = "system-keyring")]
    {
        Arc::new(SystemKeyringCredentialAuthorityProvider)
    }
    #[cfg(not(feature = "system-keyring"))]
    {
        Arc::new(UnsupportedCredentialAuthorityProvider)
    }
}

#[cfg(any(test, feature = "system-keyring"))]
fn record_account(handle: &CredentialHandle) -> String {
    format!("credential-authority/record/{}", handle.as_str())
}

#[cfg(any(test, feature = "system-keyring"))]
fn anchor_account(handle: &CredentialHandle) -> String {
    format!("credential-authority-anchor/record/{}", handle.as_str())
}

#[cfg(test)]
#[derive(Clone)]
struct ManualTimeSource {
    now_ms: Arc<Mutex<i64>>,
}

#[cfg(test)]
impl ManualTimeSource {
    fn new(now_ms: i64) -> Self {
        Self {
            now_ms: Arc::new(Mutex::new(now_ms)),
        }
    }

    fn advance_ms(&self, delta_ms: i64) {
        *self.now_ms.lock().unwrap() += delta_ms;
    }
}

#[cfg(test)]
impl TimeSource for ManualTimeSource {
    fn now_ms(&self) -> i64 {
        *self.now_ms.lock().unwrap()
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestProviderMode {
    Ready,
    Unavailable,
    Unsupported,
}

#[cfg(test)]
struct InMemoryStore {
    root: Option<String>,
    anchor_root: Option<String>,
    records: BTreeMap<String, String>,
    anchors: BTreeMap<String, String>,
}

#[cfg(test)]
impl Default for InMemoryStore {
    fn default() -> Self {
        Self {
            root: None,
            anchor_root: None,
            records: BTreeMap::new(),
            anchors: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
struct InMemoryCredentialAuthorityProvider {
    store: Mutex<InMemoryStore>,
    scripted_anchor_loads: Mutex<BTreeMap<String, VecDeque<Option<String>>>>,
    scripted_record_loads: Mutex<BTreeMap<String, VecDeque<Option<String>>>>,
    mode: TestProviderMode,
}

#[cfg(test)]
impl InMemoryCredentialAuthorityProvider {
    fn with_mode(mode: TestProviderMode) -> Self {
        Self {
            store: Mutex::new(InMemoryStore::default()),
            scripted_anchor_loads: Mutex::new(BTreeMap::new()),
            scripted_record_loads: Mutex::new(BTreeMap::new()),
            mode,
        }
    }

    fn set_raw_root_for_testing(&self, value: Option<String>) {
        self.store.lock().unwrap().root = value;
    }

    fn raw_root_for_testing(&self) -> Option<String> {
        self.store.lock().unwrap().root.clone()
    }

    fn set_raw_anchor_root_for_testing(&self, value: Option<String>) {
        self.store.lock().unwrap().anchor_root = value;
    }

    fn raw_anchor_root_for_testing(&self) -> Option<String> {
        self.store.lock().unwrap().anchor_root.clone()
    }

    fn set_raw_record_for_testing(&self, handle: &CredentialHandle, value: Option<String>) {
        let mut store = self.store.lock().unwrap();
        match value {
            Some(value) => {
                store.records.insert(record_account(handle), value);
            }
            None => {
                store.records.remove(&record_account(handle));
            }
        }
    }

    fn raw_record_for_testing(&self, handle: &CredentialHandle) -> Option<String> {
        self.store
            .lock()
            .unwrap()
            .records
            .get(&record_account(handle))
            .cloned()
    }

    fn set_raw_anchor_for_testing(&self, handle: &CredentialHandle, value: Option<String>) {
        let mut store = self.store.lock().unwrap();
        match value {
            Some(value) => {
                store.anchors.insert(anchor_account(handle), value);
            }
            None => {
                store.anchors.remove(&anchor_account(handle));
            }
        }
    }

    fn raw_anchor_for_testing(&self, handle: &CredentialHandle) -> Option<String> {
        self.store
            .lock()
            .unwrap()
            .anchors
            .get(&anchor_account(handle))
            .cloned()
    }

    fn script_anchor_loads_for_testing(
        &self,
        handle: &CredentialHandle,
        values: impl IntoIterator<Item = Option<String>>,
    ) {
        self.scripted_anchor_loads.lock().unwrap().insert(
            anchor_account(handle),
            values.into_iter().collect::<VecDeque<_>>(),
        );
    }

    fn script_record_loads_for_testing(
        &self,
        handle: &CredentialHandle,
        values: impl IntoIterator<Item = Option<String>>,
    ) {
        self.scripted_record_loads.lock().unwrap().insert(
            record_account(handle),
            values.into_iter().collect::<VecDeque<_>>(),
        );
    }

    fn ensure_ready(&self) -> Result<(), CredentialAuthorityError> {
        match self.mode {
            TestProviderMode::Ready => Ok(()),
            TestProviderMode::Unavailable => Err(CredentialAuthorityError::unavailable()),
            TestProviderMode::Unsupported => Err(CredentialAuthorityError::unsupported()),
        }
    }
}

#[cfg(test)]
impl Default for InMemoryCredentialAuthorityProvider {
    fn default() -> Self {
        Self::with_mode(TestProviderMode::Ready)
    }
}

#[cfg(test)]
impl CredentialAuthorityProvider for InMemoryCredentialAuthorityProvider {
    fn provider_id(&self) -> &str {
        TEST_IN_MEMORY_PROVIDER_ID
    }

    fn health(&self) -> CredentialAuthorityProviderHealth {
        match self.mode {
            TestProviderMode::Ready => CredentialAuthorityProviderHealth::Ready,
            TestProviderMode::Unavailable => CredentialAuthorityProviderHealth::Unavailable {
                reason: SYSTEM_KEYRING_UNAVAILABLE_REASON,
            },
            TestProviderMode::Unsupported => CredentialAuthorityProviderHealth::Unsupported {
                reason: SYSTEM_KEYRING_UNSUPPORTED_REASON,
            },
        }
    }

    fn load_root(&self) -> Result<Option<StoredAuthorityRootRecord>, CredentialAuthorityError> {
        self.ensure_ready()?;
        self.store
            .lock()
            .unwrap()
            .root
            .as_ref()
            .map(|value| {
                serde_json::from_str(value).map_err(|_| CredentialAuthorityError::legacy_conflict())
            })
            .transpose()
    }

    fn persist_root(
        &self,
        root: &StoredAuthorityRootRecord,
    ) -> Result<(), CredentialAuthorityError> {
        self.ensure_ready()?;
        self.store.lock().unwrap().root = Some(
            serde_json::to_string(root).map_err(|_| CredentialAuthorityError::legacy_conflict())?,
        );
        Ok(())
    }

    fn load_anchor_root(&self) -> Result<Option<StoredAnchorRootRecord>, CredentialAuthorityError> {
        self.ensure_ready()?;
        self.store
            .lock()
            .unwrap()
            .anchor_root
            .as_ref()
            .map(|value| {
                serde_json::from_str(value).map_err(|_| CredentialAuthorityError::legacy_conflict())
            })
            .transpose()
    }

    fn persist_anchor_root(
        &self,
        root: &StoredAnchorRootRecord,
    ) -> Result<(), CredentialAuthorityError> {
        self.ensure_ready()?;
        self.store.lock().unwrap().anchor_root = Some(
            serde_json::to_string(root).map_err(|_| CredentialAuthorityError::legacy_conflict())?,
        );
        Ok(())
    }

    fn load_record(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Option<StoredCredentialRecord>, CredentialAuthorityError> {
        self.ensure_ready()?;
        if let Some(scripted) = self
            .scripted_record_loads
            .lock()
            .unwrap()
            .get_mut(&record_account(handle))
            .and_then(VecDeque::pop_front)
        {
            return scripted
                .as_ref()
                .map(|value| {
                    serde_json::from_str(value)
                        .map_err(|_| CredentialAuthorityError::legacy_conflict())
                })
                .transpose();
        }
        self.store
            .lock()
            .unwrap()
            .records
            .get(&record_account(handle))
            .map(|value| {
                serde_json::from_str(value).map_err(|_| CredentialAuthorityError::legacy_conflict())
            })
            .transpose()
    }

    fn persist_record(
        &self,
        handle: &CredentialHandle,
        record: &StoredCredentialRecord,
    ) -> Result<(), CredentialAuthorityError> {
        self.ensure_ready()?;
        self.store.lock().unwrap().records.insert(
            record_account(handle),
            serde_json::to_string(record)
                .map_err(|_| CredentialAuthorityError::legacy_conflict())?,
        );
        Ok(())
    }

    fn load_anchor(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Option<StoredCredentialAnchorRecord>, CredentialAuthorityError> {
        self.ensure_ready()?;
        if let Some(scripted) = self
            .scripted_anchor_loads
            .lock()
            .unwrap()
            .get_mut(&anchor_account(handle))
            .and_then(VecDeque::pop_front)
        {
            return scripted
                .as_ref()
                .map(|value| {
                    serde_json::from_str(value)
                        .map_err(|_| CredentialAuthorityError::legacy_conflict())
                })
                .transpose();
        }
        self.store
            .lock()
            .unwrap()
            .anchors
            .get(&anchor_account(handle))
            .map(|value| {
                serde_json::from_str(value).map_err(|_| CredentialAuthorityError::legacy_conflict())
            })
            .transpose()
    }

    fn persist_anchor(
        &self,
        handle: &CredentialHandle,
        anchor: &StoredCredentialAnchorRecord,
    ) -> Result<(), CredentialAuthorityError> {
        self.ensure_ready()?;
        self.store.lock().unwrap().anchors.insert(
            anchor_account(handle),
            serde_json::to_string(anchor)
                .map_err(|_| CredentialAuthorityError::legacy_conflict())?,
        );
        Ok(())
    }

    #[cfg(test)]
    fn stored_record_count_for_testing(&self) -> usize {
        self.store.lock().unwrap().records.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> CredentialHandle {
        CredentialHandle::parse("opaque-canary-handle").unwrap()
    }

    fn binding() -> CredentialReadinessBinding {
        CredentialReadinessBinding::managed_enable(
            "managed-1",
            "provider-a",
            "repo-1",
            "D:/repo.db",
            7,
            11,
            "checkpoint-a",
        )
    }

    fn health_binding() -> CredentialReadinessBinding {
        CredentialReadinessBinding::health_check(
            "managed-1",
            "provider-a",
            "repo-1",
            "D:/repo.db",
            "health-1",
            "runtime",
            9,
            12,
        )
    }

    fn seeded_authority(
        now_ms: i64,
        secret: &[u8],
    ) -> (
        CredentialAuthority,
        Arc<InMemoryCredentialAuthorityProvider>,
    ) {
        let provider = Arc::new(InMemoryCredentialAuthorityProvider::default());
        let authority = CredentialAuthority::with_test_provider(
            provider.clone(),
            Arc::new(ManualTimeSource::new(now_ms)),
        );
        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle(), secret)
            .unwrap();
        (authority, provider)
    }

    fn authority_root(authority: &CredentialAuthority) -> AuthorityRoot {
        authority.load_root().unwrap()
    }

    fn anchor_root(authority: &CredentialAuthority) -> AnchorRoot {
        authority.load_anchor_root().unwrap()
    }

    fn active_record(authority: &CredentialAuthority, secret: &[u8]) -> StoredCredentialRecord {
        StoredCredentialRecord::active(
            handle().as_str().to_string(),
            &authority_root(authority),
            1,
            1,
            secret,
        )
    }

    fn active_record_json(authority: &CredentialAuthority, secret: &[u8]) -> serde_json::Value {
        serde_json::to_value(active_record(authority, secret)).unwrap()
    }

    fn anchor_json_for_record(
        authority: &CredentialAuthority,
        record: &StoredCredentialRecord,
    ) -> serde_json::Value {
        let root = authority_root(authority);
        let anchor_root = anchor_root(authority);
        let state = record.clone().validate(&handle(), &root).unwrap();
        serde_json::to_value(StoredCredentialAnchorRecord::from_record_state(
            &handle(),
            &root,
            &anchor_root,
            &state,
        ))
        .unwrap()
    }

    fn active_anchor_json(authority: &CredentialAuthority, secret: &[u8]) -> serde_json::Value {
        anchor_json_for_record(authority, &active_record(authority, secret))
    }

    fn install_record_json(
        provider: &InMemoryCredentialAuthorityProvider,
        value: serde_json::Value,
    ) {
        provider
            .set_raw_record_for_testing(&handle(), Some(serde_json::to_string(&value).unwrap()));
    }

    fn install_anchor_json(
        provider: &InMemoryCredentialAuthorityProvider,
        value: serde_json::Value,
    ) {
        provider
            .set_raw_anchor_for_testing(&handle(), Some(serde_json::to_string(&value).unwrap()));
    }

    #[test]
    fn handle_validation_is_canonical_and_does_not_normalize_env_keys() {
        assert!(CredentialHandle::parse("opaque.test-handle_1").is_ok());
        assert_eq!(
            CredentialHandle::parse("OPENAI_API_KEY")
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::InvalidHandle
        );
        assert!(CredentialHandle::parse("bad--handle").is_err());
    }

    #[test]
    fn ready_snapshot_and_witness_verify() {
        let time = Arc::new(ManualTimeSource::new(10_000));
        let authority = CredentialAuthority::in_memory_for_testing(time);
        authority
            .store_secret_for_trusted_write(
                authority.write_capability(),
                &handle(),
                b"canary-secret",
            )
            .unwrap();

        let snapshot = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        assert_eq!(snapshot.status(), CredentialReadinessStatus::Ready);
        let witness = authority
            .mint_readiness_witness(&snapshot, Duration::from_secs(30))
            .unwrap();
        let verified = authority
            .verify_readiness_witness(&witness, &handle(), &binding())
            .unwrap();
        assert_eq!(verified.evidence_digest(), snapshot.evidence_digest());
        assert_eq!(verified.binding_digest(), snapshot.binding_digest());
    }

    #[test]
    fn missing_unavailable_unsupported_and_missing_credential_fail_closed() {
        let time = Arc::new(ManualTimeSource::new(10_000));

        let missing_root = CredentialAuthority::in_memory_for_testing(time.clone());
        assert_eq!(
            missing_root
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::MissingRoot
        );

        let unavailable_provider = Arc::new(InMemoryCredentialAuthorityProvider::with_mode(
            TestProviderMode::Unavailable,
        ));
        let unavailable =
            CredentialAuthority::with_test_provider(unavailable_provider, time.clone());
        assert_eq!(
            unavailable
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::Unavailable
        );

        let unsupported_provider = Arc::new(InMemoryCredentialAuthorityProvider::with_mode(
            TestProviderMode::Unsupported,
        ));
        let unsupported = CredentialAuthority::with_test_provider(unsupported_provider, time);
        assert_eq!(
            unsupported
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::Unsupported
        );

        let missing_credential =
            CredentialAuthority::in_memory_for_testing(Arc::new(ManualTimeSource::new(10_000)));
        missing_credential
            .ensure_roots()
            .expect("test fixture should initialize roots");
        assert_eq!(
            missing_credential
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::MissingCredential
        );
    }

    #[test]
    fn rotate_and_delete_change_evidence() {
        let time = Arc::new(ManualTimeSource::new(20_000));
        let authority = CredentialAuthority::in_memory_for_testing(time);
        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle(), b"secret-1")
            .unwrap();
        let first = authority.readiness_snapshot(&handle(), &binding()).unwrap();

        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle(), b"secret-2")
            .unwrap();
        let rotated = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        assert_ne!(first.evidence_digest(), rotated.evidence_digest());
        assert_eq!(rotated.generation(), first.generation());
        assert!(rotated.secret_revision() > first.secret_revision());

        authority
            .delete_secret_for_trusted_write(authority.write_capability(), &handle())
            .unwrap();
        let deleted = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        assert_eq!(deleted.status(), CredentialReadinessStatus::Deleted);
        assert_ne!(rotated.evidence_digest(), deleted.evidence_digest());
        assert!(deleted.generation() > rotated.generation());
    }

    #[test]
    fn replayed_old_record_and_mixed_anchor_record_states_fail_closed() {
        let (authority, provider) = seeded_authority(22_000, b"secret-1");
        let original_record = provider.raw_record_for_testing(&handle()).unwrap();
        let original_anchor = provider.raw_anchor_for_testing(&handle()).unwrap();

        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle(), b"secret-2")
            .unwrap();
        let rotated_snapshot = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        let rotated_witness = authority
            .mint_readiness_witness(&rotated_snapshot, Duration::from_secs(30))
            .unwrap();
        let rotated_record = provider.raw_record_for_testing(&handle()).unwrap();
        let rotated_anchor = provider.raw_anchor_for_testing(&handle()).unwrap();

        provider.set_raw_record_for_testing(&handle(), Some(original_record.clone()));
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            authority
                .verify_readiness_witness(&rotated_witness, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        provider.set_raw_record_for_testing(&handle(), Some(rotated_record.clone()));
        provider.set_raw_anchor_for_testing(&handle(), None);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        provider.set_raw_record_for_testing(&handle(), None);
        provider.set_raw_anchor_for_testing(&handle(), Some(rotated_anchor.clone()));
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        provider.set_raw_record_for_testing(&handle(), Some(rotated_record.clone()));
        provider.set_raw_anchor_for_testing(&handle(), Some(original_anchor.clone()));
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        provider.set_raw_record_for_testing(&handle(), Some(original_record));
        provider.set_raw_anchor_for_testing(&handle(), Some(rotated_anchor.clone()));
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        provider.set_raw_record_for_testing(&handle(), Some(rotated_record.clone()));
        provider.set_raw_anchor_for_testing(&handle(), Some(rotated_anchor.clone()));
        authority
            .delete_secret_for_trusted_write(authority.write_capability(), &handle())
            .unwrap();
        let deleted_record = provider.raw_record_for_testing(&handle()).unwrap();
        let deleted_anchor = provider.raw_anchor_for_testing(&handle()).unwrap();

        provider.set_raw_record_for_testing(&handle(), Some(rotated_record));
        provider.set_raw_anchor_for_testing(&handle(), Some(deleted_anchor.clone()));
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        provider.set_raw_record_for_testing(&handle(), Some(deleted_record));
        provider.set_raw_anchor_for_testing(&handle(), Some(rotated_anchor));
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
    }

    #[test]
    fn anchor_first_intermediate_fails_closed_without_read_side_repair() {
        let (authority, provider) = seeded_authority(23_000, b"secret-1");
        let old_record = provider.raw_record_for_testing(&handle()).unwrap();
        let old_anchor = provider.raw_anchor_for_testing(&handle()).unwrap();
        let root = authority_root(&authority);
        let anchor_root = anchor_root(&authority);
        let next_record =
            StoredCredentialRecord::active(handle().as_str().to_string(), &root, 1, 2, b"secret-2");
        let next_state = next_record.clone().validate(&handle(), &root).unwrap();
        let next_anchor = StoredCredentialAnchorRecord::from_record_state(
            &handle(),
            &root,
            &anchor_root,
            &next_state,
        );

        provider.set_raw_anchor_for_testing(
            &handle(),
            Some(serde_json::to_string(&next_anchor).unwrap()),
        );
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            provider.raw_record_for_testing(&handle()).unwrap(),
            old_record
        );
        assert_eq!(
            provider.raw_anchor_for_testing(&handle()).unwrap(),
            serde_json::to_string(&next_anchor).unwrap()
        );

        provider.set_raw_record_for_testing(
            &handle(),
            Some(serde_json::to_string(&next_record).unwrap()),
        );
        let snapshot = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        assert_eq!(snapshot.secret_revision(), 2);
        assert_eq!(
            provider.raw_anchor_for_testing(&handle()).unwrap(),
            serde_json::to_string(&next_anchor).unwrap()
        );
        assert_ne!(
            provider.raw_anchor_for_testing(&handle()).unwrap(),
            old_anchor
        );
    }

    #[test]
    fn anchor_interleaving_between_a1_and_a2_is_detected() {
        let (authority, provider) = seeded_authority(24_000, b"secret-1");
        let anchor_before = provider.raw_anchor_for_testing(&handle()).unwrap();
        let root = authority_root(&authority);
        let anchor_root = anchor_root(&authority);
        let rotated_record =
            StoredCredentialRecord::active(handle().as_str().to_string(), &root, 1, 2, b"secret-2");
        let rotated_state = rotated_record.clone().validate(&handle(), &root).unwrap();
        let anchor_after = serde_json::to_string(&StoredCredentialAnchorRecord::from_record_state(
            &handle(),
            &root,
            &anchor_root,
            &rotated_state,
        ))
        .unwrap();
        provider.script_anchor_loads_for_testing(
            &handle(),
            [Some(anchor_before.clone()), Some(anchor_after)],
        );
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
    }

    #[test]
    fn record_replay_after_c1_before_final_return_fails_closed() {
        let (authority, provider) = seeded_authority(24_500, b"secret-1");
        let original_record = provider.raw_record_for_testing(&handle()).unwrap();

        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle(), b"secret-2")
            .unwrap();
        let rotated_snapshot = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        let rotated_witness = authority
            .mint_readiness_witness(&rotated_snapshot, Duration::from_secs(30))
            .unwrap();
        let rotated_record = provider.raw_record_for_testing(&handle()).unwrap();

        provider.set_raw_record_for_testing(&handle(), Some(original_record.clone()));
        provider.script_record_loads_for_testing(
            &handle(),
            [Some(rotated_record.clone()), Some(original_record.clone())],
        );
        let snapshot_error = authority
            .readiness_snapshot(&handle(), &binding())
            .unwrap_err();
        assert_eq!(
            snapshot_error.kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            snapshot_error.remediation(),
            RemediationCode::TrustedReenrollment
        );

        provider.script_record_loads_for_testing(
            &handle(),
            [Some(rotated_record), Some(original_record)],
        );
        let witness_error = authority
            .verify_readiness_witness(&rotated_witness, &handle(), &binding())
            .unwrap_err();
        assert_eq!(
            witness_error.kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            witness_error.remediation(),
            RemediationCode::TrustedReenrollment
        );
    }

    #[test]
    fn active_payload_base64_must_be_canonical_and_old_witness_cannot_be_reused() {
        let (authority, provider) = seeded_authority(25_000, b"secret-1");
        let snapshot = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        let witness = authority
            .mint_readiness_witness(&snapshot, Duration::from_secs(30))
            .unwrap();

        let mut empty_payload = active_record_json(&authority, b"secret-1");
        empty_payload["payload"]["payload_b64"] = serde_json::Value::String(String::new());
        install_record_json(&provider, empty_payload);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            authority
                .verify_readiness_witness(&witness, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        let mut invalid_payload = active_record_json(&authority, b"secret-1");
        invalid_payload["payload"]["payload_b64"] = serde_json::Value::String("%%%".to_string());
        install_record_json(&provider, invalid_payload);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        let mut truncated_payload = active_record_json(&authority, b"secret-1");
        truncated_payload["payload"]["payload_b64"] = serde_json::Value::String("c2Vj".to_string());
        install_record_json(&provider, truncated_payload);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        let mut replaced_payload = active_record_json(&authority, b"secret-1");
        replaced_payload["payload"]["payload_b64"] =
            serde_json::Value::String("c2VjcmV0LTI".to_string());
        install_record_json(&provider, replaced_payload);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
    }

    #[test]
    fn record_binding_rejects_mac_and_metadata_swaps() {
        let (authority, provider) = seeded_authority(26_000, b"secret-1");
        let original_root = provider.raw_root_for_testing().unwrap();
        let original_anchor_root = provider.raw_anchor_root_for_testing().unwrap();

        let mut tampered_binding = active_record_json(&authority, b"secret-1");
        tampered_binding["payload_binding_b64"] = serde_json::Value::String(
            base64::engine::general_purpose::STANDARD_NO_PAD.encode([0x5a_u8; 32]),
        );
        install_record_json(&provider, tampered_binding);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        let mut wrong_handle = active_record_json(&authority, b"secret-1");
        wrong_handle["handle"] = serde_json::Value::String("other-handle".to_string());
        install_record_json(&provider, wrong_handle);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        let mut wrong_epoch = active_record_json(&authority, b"secret-1");
        wrong_epoch["key_epoch"] = serde_json::Value::Number(2_u64.into());
        install_record_json(&provider, wrong_epoch);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        let mut wrong_revision = active_record_json(&authority, b"secret-1");
        wrong_revision["secret_revision"] = serde_json::Value::Number(2_u64.into());
        install_record_json(&provider, wrong_revision);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        let mut wrong_status = active_record_json(&authority, b"secret-1");
        wrong_status["status"] = serde_json::Value::String("deleted".to_string());
        wrong_status["payload"] = serde_json::Value::Null;
        wrong_status["payload_binding_b64"] = serde_json::Value::Null;
        install_record_json(&provider, wrong_status);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        install_record_json(&provider, active_record_json(&authority, b"secret-1"));

        let mut tampered_anchor_mac = active_anchor_json(&authority, b"secret-1");
        tampered_anchor_mac["anchor_mac_b64"] = serde_json::Value::String(
            base64::engine::general_purpose::STANDARD_NO_PAD.encode([0xa5_u8; 32]),
        );
        install_anchor_json(&provider, tampered_anchor_mac);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        install_anchor_json(&provider, active_anchor_json(&authority, b"secret-1"));

        let mut wrong_anchor_identity = active_anchor_json(&authority, b"secret-1");
        wrong_anchor_identity["authority_instance_id"] =
            serde_json::Value::String("wrong-root".to_string());
        install_anchor_json(&provider, wrong_anchor_identity);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        install_anchor_json(&provider, active_anchor_json(&authority, b"secret-1"));

        let mut wrong_anchor_epoch = active_anchor_json(&authority, b"secret-1");
        wrong_anchor_epoch["anchor_key_epoch"] = serde_json::Value::Number(2_u64.into());
        install_anchor_json(&provider, wrong_anchor_epoch);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        install_anchor_json(&provider, active_anchor_json(&authority, b"secret-1"));

        let mut wrong_root_instance =
            serde_json::from_str::<serde_json::Value>(&provider.raw_root_for_testing().unwrap())
                .unwrap();
        wrong_root_instance["instance_id"] = serde_json::Value::String("wrong-root".to_string());
        provider
            .set_raw_root_for_testing(Some(serde_json::to_string(&wrong_root_instance).unwrap()));
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        provider.set_raw_root_for_testing(Some(original_root));

        let mut wrong_anchor_root =
            serde_json::from_str::<serde_json::Value>(&original_anchor_root).unwrap();
        wrong_anchor_root["key_epoch"] = serde_json::Value::Number(2_u64.into());
        provider.set_raw_anchor_root_for_testing(Some(
            serde_json::to_string(&wrong_anchor_root).unwrap(),
        ));
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
    }

    #[test]
    fn witness_rejects_wrong_binding_handle_provider_root_and_epoch() {
        let time = Arc::new(ManualTimeSource::new(30_000));
        let authority = CredentialAuthority::in_memory_for_testing(time);
        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle(), b"secret-1")
            .unwrap();
        let snapshot = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        let witness = authority
            .mint_readiness_witness(&snapshot, Duration::from_secs(30))
            .unwrap();

        assert_eq!(
            authority
                .verify_readiness_witness(&witness, &handle(), &health_binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::WitnessMismatch
        );

        let wrong_handle = CredentialHandle::parse("other-handle").unwrap();
        assert_eq!(
            authority
                .verify_readiness_witness(&witness, &wrong_handle, &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::MissingCredential
        );

        let wrong_provider = witness.tampered(|claims, _| {
            claims.provider_id = "provider-b".to_string();
        });
        assert_eq!(
            authority
                .verify_readiness_witness(&wrong_provider, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::WitnessMismatch
        );

        let wrong_root = witness.tampered(|claims, _| {
            claims.root_instance_id = "root-b".to_string();
        });
        assert_eq!(
            authority
                .verify_readiness_witness(&wrong_root, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::WitnessMismatch
        );

        let wrong_epoch = witness.tampered(|claims, _| {
            claims.key_epoch += 1;
        });
        assert_eq!(
            authority
                .verify_readiness_witness(&wrong_epoch, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::WitnessMismatch
        );

        let wrong_anchor_root = witness.tampered(|claims, _| {
            claims.anchor_root_instance_id = "anchor-root-b".to_string();
        });
        assert_eq!(
            authority
                .verify_readiness_witness(&wrong_anchor_root, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::WitnessMismatch
        );

        let wrong_anchor_epoch = witness.tampered(|claims, _| {
            claims.anchor_key_epoch += 1;
        });
        assert_eq!(
            authority
                .verify_readiness_witness(&wrong_anchor_epoch, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::WitnessMismatch
        );
    }

    #[test]
    fn witness_rejects_tamper_replay_and_expiry() {
        let time = Arc::new(ManualTimeSource::new(40_000));
        let authority = CredentialAuthority::in_memory_for_testing(time.clone());
        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle(), b"secret-1")
            .unwrap();
        let snapshot = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        let witness = authority
            .mint_readiness_witness(&snapshot, Duration::from_secs(1))
            .unwrap();

        let tampered = witness.tampered(|_, mac| {
            mac[0] ^= 0x55;
        });
        assert_eq!(
            authority
                .verify_readiness_witness(&tampered, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::WitnessMismatch
        );

        authority
            .verify_readiness_witness(&witness, &handle(), &binding())
            .unwrap();
        assert_eq!(
            authority
                .verify_readiness_witness(&witness, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::WitnessConsumed
        );

        let expiring = authority
            .mint_readiness_witness(&snapshot, Duration::from_millis(500))
            .unwrap();
        time.advance_ms(600);
        assert_eq!(
            authority
                .verify_readiness_witness(&expiring, &handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::WitnessExpired
        );
    }

    #[test]
    fn restart_invalidates_old_witness_but_preserves_stable_evidence() {
        let time = Arc::new(ManualTimeSource::new(50_000));
        let provider = Arc::new(InMemoryCredentialAuthorityProvider::default());
        let first = CredentialAuthority::with_test_provider(provider.clone(), time.clone());
        first
            .store_secret_for_trusted_write(first.write_capability(), &handle(), b"secret-1")
            .unwrap();
        let first_snapshot = first.readiness_snapshot(&handle(), &binding()).unwrap();
        let witness = first
            .mint_readiness_witness(&first_snapshot, Duration::from_secs(5))
            .unwrap();

        let restarted = CredentialAuthority::with_test_provider(provider, time);
        let restarted_snapshot = restarted.readiness_snapshot(&handle(), &binding()).unwrap();
        assert_eq!(
            first_snapshot.evidence_digest(),
            restarted_snapshot.evidence_digest()
        );
        assert_eq!(
            restarted
                .verify_readiness_witness(&witness, &handle(), &binding())
                .unwrap_err()
                .remediation(),
            RemediationCode::RestartAuthoritySession
        );
    }

    #[test]
    fn production_default_never_selects_in_memory_provider() {
        let authority = CredentialAuthority::production_default();
        #[cfg(test)]
        assert_ne!(authority.provider_id(), TEST_IN_MEMORY_PROVIDER_ID);
    }

    #[test]
    fn enrollment_writer_owner_only_returns_safe_summary_and_ready_status() {
        let owner = EnrollmentWriterOwner::in_memory_for_testing_default_time();
        let handle = CredentialHandle::parse("enr.safe-owner").unwrap();
        let secret = "owner-secret";
        let write = owner
            .write_validated_secret(
                &handle,
                secret.as_bytes(),
                &CredentialReadinessBinding::auth_requirement_probe(
                    "managed_enrollment",
                    "managed_owner",
                ),
            )
            .unwrap();
        let write_debug = format!("{write:?}");
        assert_eq!(write.status, CredentialReadinessStatus::Ready);
        assert_eq!(write.authority.writer_mode, "best_effort_single_process");
        assert!(!format!("{owner:?}").contains(secret));
        assert!(!write_debug.contains(secret));
        assert!(!write_debug.contains(handle.as_str()));
        assert!(!write_debug.contains(write.evidence_digest.as_str()));
    }

    #[test]
    fn debug_error_and_storage_boundary_do_not_leak_raw_secret() {
        let time = Arc::new(ManualTimeSource::new(60_000));
        let authority = CredentialAuthority::in_memory_for_testing(time);
        authority
            .store_secret_for_trusted_write(
                authority.write_capability(),
                &handle(),
                b"canary-secret",
            )
            .unwrap();
        let snapshot = authority.readiness_snapshot(&handle(), &binding()).unwrap();
        let witness = authority
            .mint_readiness_witness(&snapshot, Duration::from_secs(10))
            .unwrap();

        assert!(!format!("{witness:?}").contains("canary-secret"));
        let record = StoredCredentialRecord::active(
            handle().as_str().to_string(),
            &authority.load_root().unwrap(),
            1,
            1,
            b"canary-secret",
        );
        let record_debug = format!("{record:?}");
        assert!(!record_debug.contains("canary-secret"));
        assert!(!record_debug.contains(record.payload_binding_b64.as_deref().unwrap()));

        let encoded = serde_json::to_string(&record).unwrap();
        assert!(!encoded.contains("canary-secret"));

        let root_record = authority.provider.load_root().unwrap().unwrap();
        let root_debug = format!("{root_record:?}");
        assert!(!root_debug.contains(&root_record.evidence_key));

        let anchor_root_record = authority.provider.load_anchor_root().unwrap().unwrap();
        let anchor_root_debug = format!("{anchor_root_record:?}");
        assert!(!anchor_root_debug.contains(&anchor_root_record.anchor_key));

        let anchor_record = authority.provider.load_anchor(&handle()).unwrap().unwrap();
        let anchor_record_debug = format!("{anchor_record:?}");
        assert!(!anchor_record_debug.contains(anchor_record.record_binding_b64.as_str()));
        assert!(!anchor_record_debug.contains(anchor_record.anchor_mac_b64.as_str()));

        let error = CredentialAuthorityError::legacy_conflict();
        assert!(!format!("{error}").contains("canary-secret"));
        assert!(!format!("{error:?}").contains("canary-secret"));
    }

    #[test]
    fn legacy_conflict_and_public_boundary_are_enforced() {
        let time = Arc::new(ManualTimeSource::new(70_000));
        let provider = Arc::new(InMemoryCredentialAuthorityProvider::default());
        let authority = CredentialAuthority::with_test_provider(provider.clone(), time);
        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle(), b"secret-1")
            .unwrap();

        provider.set_raw_record_for_testing(
            &handle(),
            Some(
                r#"{"format_version":1,"handle":"opaque-canary-handle","provider_id":"in-memory-test","authority_instance_id":"wrong","key_epoch":1,"generation":1,"secret_revision":1,"status":"active","payload":{"type":"inline_base64","payload_b64":"c2VjcmV0"}} "#.trim().to_string(),
            ),
        );
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );

        let mod_source = include_str!("mod.rs");
        assert!(mod_source.contains("pub(crate) mod credential_authority;"));
        assert!(!mod_source.contains("pub use credential_authority"));

        let module_source = include_str!("credential_authority.rs");
        assert!(module_source.contains("#[cfg(test)]"));
        assert!(module_source.contains("pub(crate) fn in_memory_for_testing"));
        assert!(module_source.contains("pub(crate) struct EnrollmentWriterOwner"));
        assert!(module_source.contains("pub(crate) struct CredentialAuthorityWriteCapability"));
        assert!(
            !module_source.contains("#[derive(Clone)]\npub(crate) struct EnrollmentWriterOwner")
        );
        assert!(!module_source
            .contains("#[derive(Clone)]\npub(crate) struct CredentialAuthorityWriteCapability"));
        assert!(!module_source.contains("pub(crate) fn persist_record"));
        assert!(!module_source.contains("pub(crate) fn persist_anchor"));
        assert!(!module_source.contains("pub(crate) fn persist_root"));
        assert!(!module_source.contains("pub(crate) fn persist_anchor_root"));
        assert!(module_source.contains("fn store_secret_internal("));
        assert!(module_source
            .contains("#[cfg(test)]\n    pub(crate) fn store_secret_for_trusted_write"));
        assert!(module_source.contains("self.authority.store_secret_internal(handle, secret)?;"));
    }

    #[test]
    fn root_legacy_conflict_is_detected() {
        let time = Arc::new(ManualTimeSource::new(80_000));
        let provider = Arc::new(InMemoryCredentialAuthorityProvider::default());
        provider.set_raw_root_for_testing(Some("{\"format_version\":0}".to_string()));
        let authority = CredentialAuthority::with_test_provider(provider, time);
        assert_eq!(
            authority
                .readiness_snapshot(&handle(), &binding())
                .unwrap_err()
                .kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
    }

    #[test]
    fn missing_root_or_anchor_root_with_existing_state_fails_closed() {
        let (authority, provider) = seeded_authority(81_000, b"secret-1");

        provider.set_raw_root_for_testing(None);
        let root_missing_with_full_residuals = authority
            .readiness_snapshot(&handle(), &binding())
            .unwrap_err();
        assert_eq!(
            root_missing_with_full_residuals.kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            root_missing_with_full_residuals.remediation(),
            RemediationCode::TrustedReenrollment
        );

        let (authority, provider) = seeded_authority(81_100, b"secret-1");
        provider.set_raw_root_for_testing(None);
        provider.set_raw_record_for_testing(&handle(), None);
        provider.set_raw_anchor_for_testing(&handle(), None);
        let root_missing_with_anchor_root_residual = authority
            .readiness_snapshot(&handle(), &binding())
            .unwrap_err();
        assert_eq!(
            root_missing_with_anchor_root_residual.kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            root_missing_with_anchor_root_residual.remediation(),
            RemediationCode::TrustedReenrollment
        );

        let (authority, provider) = seeded_authority(81_200, b"secret-1");
        provider.set_raw_root_for_testing(None);
        provider.set_raw_anchor_root_for_testing(None);
        provider.set_raw_record_for_testing(&handle(), None);
        let root_missing_with_anchor_residual = authority
            .readiness_snapshot(&handle(), &binding())
            .unwrap_err();
        assert_eq!(
            root_missing_with_anchor_residual.kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            root_missing_with_anchor_residual.remediation(),
            RemediationCode::TrustedReenrollment
        );

        let (authority, provider) = seeded_authority(81_300, b"secret-1");
        provider.set_raw_root_for_testing(None);
        provider.set_raw_anchor_root_for_testing(None);
        provider.set_raw_anchor_for_testing(&handle(), None);
        let root_missing_with_record_residual = authority
            .readiness_snapshot(&handle(), &binding())
            .unwrap_err();
        assert_eq!(
            root_missing_with_record_residual.kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            root_missing_with_record_residual.remediation(),
            RemediationCode::TrustedReenrollment
        );

        let (authority, provider) = seeded_authority(82_000, b"secret-1");
        provider.set_raw_anchor_root_for_testing(None);
        let missing_anchor_root = authority
            .readiness_snapshot(&handle(), &binding())
            .unwrap_err();
        assert_eq!(
            missing_anchor_root.kind(),
            CredentialAuthorityErrorKind::LegacyConflict
        );
        assert_eq!(
            missing_anchor_root.remediation(),
            RemediationCode::TrustedReenrollment
        );
    }
}
