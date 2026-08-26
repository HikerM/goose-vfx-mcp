use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::mcp_platform::credential_authority::{
    CredentialAuthority, CredentialAuthorityError, CredentialAuthorityErrorKind,
    CredentialAuthorityProviderHealth, CredentialHandle, CredentialReadinessBinding,
    CredentialReadinessStatus, EnrollmentAuthorityReader,
};
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::lifecycle::{
    AuthRequirement, AuthRequirementEvidence, AuthRequirementResolver, AuthRequirementSnapshot,
};
use crate::mcp_platform::manifest::{trusted_credential_enrollment_schema, Auth};
use crate::utils::bytes_to_hex;

const RUNTIME_GATE_DIGEST_DOMAIN: &str = "managed-credential-runtime-gate-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CredentialRuntimeStatus {
    Ready,
    Unconfigured,
    ReRegistrationRequired,
    TrustedStateConflict,
    TemporarilyUnavailable,
}

impl CredentialRuntimeStatus {
    pub(crate) const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Unconfigured => "unconfigured",
            Self::ReRegistrationRequired => "re_registration_required",
            Self::TrustedStateConflict => "trusted_state_conflict",
            Self::TemporarilyUnavailable => "temporarily_unavailable",
        }
    }

    pub(crate) const fn readiness_error(self) -> McpPlatformError {
        match self {
            Self::Ready => McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "managed MCP credential runtime gate reached an impossible ready error state",
            ),
            Self::Unconfigured | Self::ReRegistrationRequired => McpPlatformError::new(
                McpPlatformErrorCode::CredentialMissing,
                "managed MCP credential handle is unavailable",
            ),
            Self::TrustedStateConflict => McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "managed MCP credential runtime gate rejected the stored credential state",
            ),
            Self::TemporarilyUnavailable => McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "managed MCP credential runtime gate is temporarily unavailable",
            ),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialReference {
    pub handle: String,
    pub reference_digest: String,
}

impl fmt::Debug for CredentialReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialReference")
            .field("handle", &"[REDACTED]")
            .field("reference_digest", &"[REDACTED]")
            .finish()
    }
}

impl CredentialReference {
    pub(crate) fn from_handle(handle: impl Into<String>) -> Self {
        let handle = handle.into();
        Self {
            reference_digest: credential_reference_digest(&handle),
            handle,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CredentialRuntimeSnapshot {
    requirement: AuthRequirement,
    status: CredentialRuntimeStatus,
    evidence_digest: String,
    reference: Option<CredentialReference>,
}

impl fmt::Debug for CredentialRuntimeSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialRuntimeSnapshot")
            .field(
                "requirement",
                &redacted_requirement_label(&self.requirement),
            )
            .field("status", &self.status)
            .field("evidence_digest", &"[REDACTED]")
            .field("reference", &self.reference.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl CredentialRuntimeSnapshot {
    pub(crate) fn from_legacy_snapshot(
        requirement: AuthRequirement,
        evidence: AuthRequirementEvidence,
        reference: Option<CredentialReference>,
    ) -> Self {
        let status = if evidence.ready {
            CredentialRuntimeStatus::Ready
        } else {
            CredentialRuntimeStatus::Unconfigured
        };
        Self {
            requirement,
            status,
            evidence_digest: evidence.evidence_digest,
            reference,
        }
    }

    pub(crate) fn requirement(&self) -> &AuthRequirement {
        &self.requirement
    }

    pub(crate) const fn status(&self) -> CredentialRuntimeStatus {
        self.status
    }

    pub(crate) fn evidence_digest(&self) -> &str {
        &self.evidence_digest
    }

    pub(crate) fn reference(&self) -> Option<&CredentialReference> {
        self.reference.as_ref()
    }

    pub(crate) const fn is_ready(&self) -> bool {
        self.status.is_ready()
    }

    pub(crate) fn requirement_snapshot(&self) -> AuthRequirementSnapshot {
        AuthRequirementSnapshot {
            requirement: self.requirement.clone(),
            evidence: AuthRequirementEvidence {
                ready: self.is_ready(),
                evidence_digest: self.evidence_digest.clone(),
            },
        }
    }
}

pub(crate) fn auth_reference(auth: &Auth) -> Option<CredentialReference> {
    match auth {
        Auth::ApiKeyHeader {
            credential_name, ..
        }
        | Auth::Environment {
            credential_name, ..
        } => Some(CredentialReference::from_handle(credential_name.clone())),
        Auth::None | Auth::Oauth2 { .. } => None,
    }
}

pub(crate) fn credential_reference_digest(handle: &str) -> String {
    bytes_to_hex(Sha256::digest(
        serde_json::to_vec(&(RUNTIME_GATE_DIGEST_DOMAIN, "reference", handle))
            .expect("credential reference digest payload must serialize"),
    ))
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct EnrollmentFactRuntimeBinding {
    managed_mcp_id: String,
    current_managed_revision: i64,
    current_manifest_digest: String,
    trusted_schema_id: String,
    enrolled_manifest_digest: String,
    enrolled_schema_id: String,
    credential_reference: String,
    reference_digest: String,
    authority_provider_id: String,
    authority_writer_mode: String,
    authority_evidence_digest: String,
    enrollment_revision: i64,
}

impl fmt::Debug for EnrollmentFactRuntimeBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollmentFactRuntimeBinding")
            .field("managed_mcp_id", &self.managed_mcp_id)
            .field("current_managed_revision", &self.current_managed_revision)
            .field("current_manifest_digest", &"[REDACTED]")
            .field("trusted_schema_id", &"[REDACTED]")
            .field("enrolled_manifest_digest", &"[REDACTED]")
            .field("enrolled_schema_id", &"[REDACTED]")
            .field("credential_reference", &"[REDACTED]")
            .field("reference_digest", &"[REDACTED]")
            .field("authority_provider_id", &"[REDACTED]")
            .field("authority_writer_mode", &"[REDACTED]")
            .field("authority_evidence_digest", &"[REDACTED]")
            .field("enrollment_revision", &self.enrollment_revision)
            .finish()
    }
}

impl EnrollmentFactRuntimeBinding {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        managed_mcp_id: impl Into<String>,
        current_managed_revision: i64,
        current_manifest_digest: impl Into<String>,
        trusted_schema_id: impl Into<String>,
        enrolled_manifest_digest: impl Into<String>,
        enrolled_schema_id: impl Into<String>,
        credential_reference: impl Into<String>,
        reference_digest: impl Into<String>,
        authority_provider_id: impl Into<String>,
        authority_writer_mode: impl Into<String>,
        authority_evidence_digest: impl Into<String>,
        enrollment_revision: i64,
    ) -> Self {
        Self {
            managed_mcp_id: managed_mcp_id.into(),
            current_managed_revision,
            current_manifest_digest: current_manifest_digest.into(),
            trusted_schema_id: trusted_schema_id.into(),
            enrolled_manifest_digest: enrolled_manifest_digest.into(),
            enrolled_schema_id: enrolled_schema_id.into(),
            credential_reference: credential_reference.into(),
            reference_digest: reference_digest.into(),
            authority_provider_id: authority_provider_id.into(),
            authority_writer_mode: authority_writer_mode.into(),
            authority_evidence_digest: authority_evidence_digest.into(),
            enrollment_revision,
        }
    }

    fn has_revision_drift(&self) -> bool {
        self.current_managed_revision != self.enrollment_revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnrollmentBindingSource<'a> {
    Missing,
    Bound(&'a EnrollmentFactRuntimeBinding),
    Unavailable,
}

pub(crate) fn enrollment_backed_runtime_snapshot(
    authority: &EnrollmentAuthorityReader,
    auth: &Auth,
    source: EnrollmentBindingSource<'_>,
) -> CredentialRuntimeSnapshot {
    if matches!(auth, Auth::None) {
        return CredentialRuntimeSnapshot {
            requirement: AuthRequirement::Ready,
            status: CredentialRuntimeStatus::Ready,
            evidence_digest: digest_runtime_evidence(
                CredentialRuntimeStatus::Ready,
                None,
                Some("none"),
                Some("enrollment"),
            ),
            reference: None,
        };
    }

    let Ok(schema) = trusted_credential_enrollment_schema(auth) else {
        return enrollment_snapshot(
            CredentialRuntimeStatus::TemporarilyUnavailable,
            None,
            None,
            Some("unsupported_auth"),
        );
    };

    if let EnrollmentBindingSource::Bound(binding) = source {
        if binding.has_revision_drift() {
            return enrollment_snapshot(
                CredentialRuntimeStatus::ReRegistrationRequired,
                Some(&binding.reference_digest),
                Some(schema.schema_id),
                Some("revision_drift"),
            );
        }
    }

    let provider_health = authority.provider_health();
    if matches!(
        provider_health,
        CredentialAuthorityProviderHealth::Unavailable { .. }
    ) || matches!(
        provider_health,
        CredentialAuthorityProviderHealth::Unsupported { .. }
    ) {
        return enrollment_snapshot(
            CredentialRuntimeStatus::TemporarilyUnavailable,
            None,
            None,
            Some("authority_unavailable"),
        );
    }

    let binding = match source {
        EnrollmentBindingSource::Missing => {
            return enrollment_snapshot(
                CredentialRuntimeStatus::Unconfigured,
                None,
                Some(schema.schema_id),
                Some("missing_binding"),
            );
        }
        EnrollmentBindingSource::Unavailable => {
            return enrollment_snapshot(
                CredentialRuntimeStatus::TemporarilyUnavailable,
                None,
                Some(schema.schema_id),
                Some("binding_source_unavailable"),
            );
        }
        EnrollmentBindingSource::Bound(binding) => binding,
    };

    if binding.current_manifest_digest.is_empty()
        || binding.trusted_schema_id.is_empty()
        || binding.authority_evidence_digest.is_empty()
    {
        return enrollment_snapshot(
            CredentialRuntimeStatus::ReRegistrationRequired,
            Some(&binding.reference_digest),
            Some(schema.schema_id),
            Some("missing_enrollment_fact"),
        );
    }
    if binding.current_manifest_digest != binding.enrolled_manifest_digest {
        return enrollment_snapshot(
            CredentialRuntimeStatus::ReRegistrationRequired,
            Some(&binding.reference_digest),
            Some(schema.schema_id),
            Some("manifest_drift"),
        );
    }
    if binding.trusted_schema_id != schema.schema_id
        || binding.enrolled_schema_id != schema.schema_id
    {
        return enrollment_snapshot(
            CredentialRuntimeStatus::ReRegistrationRequired,
            Some(&binding.reference_digest),
            Some(schema.schema_id),
            Some("schema_drift"),
        );
    }

    let runtime_summary = authority.authority_summary();
    if binding.authority_provider_id != runtime_summary.provider_id {
        return enrollment_snapshot(
            CredentialRuntimeStatus::TrustedStateConflict,
            Some(&binding.reference_digest),
            Some(schema.schema_id),
            Some("provider_mismatch"),
        );
    }
    if binding.authority_writer_mode != runtime_summary.writer_mode {
        return enrollment_snapshot(
            CredentialRuntimeStatus::TrustedStateConflict,
            Some(&binding.reference_digest),
            Some(schema.schema_id),
            Some("writer_mode_mismatch"),
        );
    }

    let handle = match CredentialHandle::parse(binding.credential_reference.clone()) {
        Ok(handle) => handle,
        Err(_) => {
            return enrollment_snapshot(
                CredentialRuntimeStatus::TrustedStateConflict,
                Some(&binding.reference_digest),
                Some(schema.schema_id),
                Some("invalid_handle"),
            );
        }
    };
    if credential_reference_digest(handle.as_str()) != binding.reference_digest {
        return enrollment_snapshot(
            CredentialRuntimeStatus::TrustedStateConflict,
            Some(&binding.reference_digest),
            Some(schema.schema_id),
            Some("reference_digest_mismatch"),
        );
    }

    let readiness_binding = CredentialReadinessBinding::auth_requirement_probe(
        "managed_enrollment_runtime",
        &binding.managed_mcp_id,
    );
    match authority.readiness_snapshot(&handle, &readiness_binding) {
        Ok(snapshot) => {
            if snapshot.provider_id() != binding.authority_provider_id {
                return enrollment_snapshot(
                    CredentialRuntimeStatus::TrustedStateConflict,
                    Some(&binding.reference_digest),
                    Some(snapshot.evidence_digest()),
                    Some("provider_snapshot_mismatch"),
                );
            }
            if snapshot.evidence_digest() != binding.authority_evidence_digest {
                return enrollment_snapshot(
                    CredentialRuntimeStatus::TrustedStateConflict,
                    Some(&binding.reference_digest),
                    Some(snapshot.evidence_digest()),
                    Some("authority_evidence_mismatch"),
                );
            }
            let status = match snapshot.status() {
                CredentialReadinessStatus::Ready => CredentialRuntimeStatus::Ready,
                CredentialReadinessStatus::Deleted => {
                    CredentialRuntimeStatus::ReRegistrationRequired
                }
            };
            let requirement = if status.is_ready() {
                AuthRequirement::Ready
            } else {
                AuthRequirement::MissingOpaqueHandle {
                    credential_name: binding.managed_mcp_id.clone(),
                }
            };
            CredentialRuntimeSnapshot {
                requirement,
                status,
                evidence_digest: digest_runtime_evidence(
                    status,
                    Some(&binding.reference_digest),
                    Some(snapshot.evidence_digest()),
                    Some("enrollment"),
                ),
                reference: Some(CredentialReference::from_handle(
                    handle.as_str().to_string(),
                )),
            }
        }
        Err(error) => {
            let status = map_enrollment_authority_status(&error);
            enrollment_snapshot(
                status,
                Some(&binding.reference_digest),
                None,
                Some(authority_error_label(&error)),
            )
        }
    }
}

fn enrollment_snapshot(
    status: CredentialRuntimeStatus,
    reference_digest: Option<&str>,
    authority_evidence: Option<&str>,
    detail: Option<&str>,
) -> CredentialRuntimeSnapshot {
    CredentialRuntimeSnapshot {
        requirement: if status.is_ready() {
            AuthRequirement::Ready
        } else {
            AuthRequirement::MissingOpaqueHandle {
                credential_name: "managed_enrollment".to_string(),
            }
        },
        status,
        evidence_digest: digest_runtime_evidence(
            status,
            reference_digest,
            authority_evidence,
            detail,
        ),
        reference: None,
    }
}

pub(crate) struct CredentialAuthorityRuntimeGateAdapter {
    authority: CredentialAuthority,
}

impl CredentialAuthorityRuntimeGateAdapter {
    pub(crate) fn new(authority: CredentialAuthority) -> Self {
        Self { authority }
    }

    pub(crate) fn production_default() -> Self {
        Self::new(CredentialAuthority::production_default())
    }

    fn generic_probe_binding(auth: &Auth) -> CredentialReadinessBinding {
        match auth {
            Auth::None => CredentialReadinessBinding::auth_requirement_probe("none", "none"),
            Auth::ApiKeyHeader {
                credential_name, ..
            } => CredentialReadinessBinding::auth_requirement_probe(
                "api_key_header",
                credential_name,
            ),
            Auth::Environment {
                credential_name, ..
            } => CredentialReadinessBinding::auth_requirement_probe("environment", credential_name),
            Auth::Oauth2 { .. } => {
                CredentialReadinessBinding::auth_requirement_probe("oauth2", "oauth2")
            }
        }
    }

    fn runtime_snapshot_with_binding(
        &self,
        auth: &Auth,
        binding: &CredentialReadinessBinding,
    ) -> CredentialRuntimeSnapshot {
        let reference = auth_reference(auth);
        let missing_requirement = || match reference.as_ref() {
            Some(reference) => AuthRequirement::MissingOpaqueHandle {
                credential_name: reference.handle.clone(),
            },
            None => AuthRequirement::MissingOpaqueHandle {
                credential_name: "oauth2".to_string(),
            },
        };
        match auth {
            Auth::None => CredentialRuntimeSnapshot {
                requirement: AuthRequirement::Ready,
                status: CredentialRuntimeStatus::Ready,
                evidence_digest: digest_runtime_evidence(
                    CredentialRuntimeStatus::Ready,
                    None,
                    Some("none"),
                    None,
                ),
                reference: None,
            },
            Auth::Oauth2 { .. } => CredentialRuntimeSnapshot {
                requirement: missing_requirement(),
                status: CredentialRuntimeStatus::Unconfigured,
                evidence_digest: digest_runtime_evidence(
                    CredentialRuntimeStatus::Unconfigured,
                    None,
                    Some("oauth2"),
                    None,
                ),
                reference: None,
            },
            Auth::ApiKeyHeader { .. } | Auth::Environment { .. } => {
                let reference = reference
                    .clone()
                    .expect("handled auth variants must carry a reference");
                let handle = match CredentialHandle::parse(reference.handle.clone()) {
                    Ok(handle) => handle,
                    Err(_) => {
                        return CredentialRuntimeSnapshot {
                            requirement: missing_requirement(),
                            status: CredentialRuntimeStatus::TrustedStateConflict,
                            evidence_digest: digest_runtime_evidence(
                                CredentialRuntimeStatus::TrustedStateConflict,
                                Some(&reference.reference_digest),
                                None,
                                Some("invalid_handle"),
                            ),
                            reference: Some(reference),
                        };
                    }
                };
                match self.authority.readiness_snapshot(&handle, binding) {
                    Ok(snapshot) => {
                        let status = match snapshot.status() {
                            CredentialReadinessStatus::Ready => CredentialRuntimeStatus::Ready,
                            CredentialReadinessStatus::Deleted => {
                                CredentialRuntimeStatus::ReRegistrationRequired
                            }
                        };
                        let requirement = if status.is_ready() {
                            AuthRequirement::Ready
                        } else {
                            missing_requirement()
                        };
                        CredentialRuntimeSnapshot {
                            requirement,
                            status,
                            evidence_digest: digest_runtime_evidence(
                                status,
                                Some(&reference.reference_digest),
                                Some(snapshot.evidence_digest()),
                                None,
                            ),
                            reference: Some(reference),
                        }
                    }
                    Err(error) => {
                        let status = map_authority_status(&error);
                        CredentialRuntimeSnapshot {
                            requirement: missing_requirement(),
                            status,
                            evidence_digest: digest_runtime_evidence(
                                status,
                                Some(&reference.reference_digest),
                                None,
                                Some(authority_error_label(&error)),
                            ),
                            reference: Some(reference),
                        }
                    }
                }
            }
        }
    }
}

impl Default for CredentialAuthorityRuntimeGateAdapter {
    fn default() -> Self {
        Self::production_default()
    }
}

impl AuthRequirementResolver for CredentialAuthorityRuntimeGateAdapter {
    fn requirement(&self, auth: &Auth) -> McpPlatformResult<AuthRequirement> {
        Ok(self.snapshot(auth)?.requirement)
    }

    fn snapshot(&self, auth: &Auth) -> McpPlatformResult<AuthRequirementSnapshot> {
        Ok(self
            .runtime_snapshot(auth, &Self::generic_probe_binding(auth))?
            .requirement_snapshot())
    }

    fn runtime_snapshot(
        &self,
        auth: &Auth,
        binding: &CredentialReadinessBinding,
    ) -> McpPlatformResult<CredentialRuntimeSnapshot> {
        Ok(self.runtime_snapshot_with_binding(auth, binding))
    }

    fn verify_runtime_binding(
        &self,
        auth: &Auth,
        binding: &CredentialReadinessBinding,
        ttl: Duration,
    ) -> McpPlatformResult<CredentialRuntimeSnapshot> {
        let snapshot = self.runtime_snapshot_with_binding(auth, binding);
        if !snapshot.is_ready() {
            return Err(snapshot.status().readiness_error());
        }
        let Some(reference) = snapshot.reference() else {
            return Ok(snapshot);
        };
        let handle = CredentialHandle::parse(reference.handle.clone())
            .map_err(|_| CredentialRuntimeStatus::TrustedStateConflict.readiness_error())?;
        let authority_snapshot = self
            .authority
            .readiness_snapshot(&handle, binding)
            .map_err(map_authority_error)?;
        let witness = self
            .authority
            .mint_readiness_witness(&authority_snapshot, ttl)
            .map_err(map_authority_error)?;
        self.authority
            .verify_readiness_witness(&witness, &handle, binding)
            .map_err(map_authority_error)?;
        Ok(self.runtime_snapshot_with_binding(auth, binding))
    }
}

fn map_authority_status(error: &CredentialAuthorityError) -> CredentialRuntimeStatus {
    match error.kind() {
        CredentialAuthorityErrorKind::MissingCredential
        | CredentialAuthorityErrorKind::MissingRoot => CredentialRuntimeStatus::Unconfigured,
        CredentialAuthorityErrorKind::DeletedCredential => {
            CredentialRuntimeStatus::ReRegistrationRequired
        }
        CredentialAuthorityErrorKind::Unavailable | CredentialAuthorityErrorKind::Unsupported => {
            CredentialRuntimeStatus::TemporarilyUnavailable
        }
        CredentialAuthorityErrorKind::InvalidHandle
        | CredentialAuthorityErrorKind::LegacyConflict
        | CredentialAuthorityErrorKind::WitnessExpired
        | CredentialAuthorityErrorKind::WitnessConsumed
        | CredentialAuthorityErrorKind::WitnessMismatch
        | CredentialAuthorityErrorKind::CapabilityRequired => {
            CredentialRuntimeStatus::TrustedStateConflict
        }
    }
}

fn map_enrollment_authority_status(error: &CredentialAuthorityError) -> CredentialRuntimeStatus {
    match error.kind() {
        CredentialAuthorityErrorKind::MissingCredential
        | CredentialAuthorityErrorKind::MissingRoot
        | CredentialAuthorityErrorKind::DeletedCredential => {
            CredentialRuntimeStatus::ReRegistrationRequired
        }
        CredentialAuthorityErrorKind::Unavailable | CredentialAuthorityErrorKind::Unsupported => {
            CredentialRuntimeStatus::TemporarilyUnavailable
        }
        CredentialAuthorityErrorKind::InvalidHandle
        | CredentialAuthorityErrorKind::LegacyConflict
        | CredentialAuthorityErrorKind::WitnessExpired
        | CredentialAuthorityErrorKind::WitnessConsumed
        | CredentialAuthorityErrorKind::WitnessMismatch
        | CredentialAuthorityErrorKind::CapabilityRequired => {
            CredentialRuntimeStatus::TrustedStateConflict
        }
    }
}

fn map_authority_error(error: CredentialAuthorityError) -> McpPlatformError {
    map_authority_status(&error).readiness_error()
}

fn authority_error_label(error: &CredentialAuthorityError) -> &'static str {
    match error.kind() {
        CredentialAuthorityErrorKind::InvalidHandle => "invalid_handle",
        CredentialAuthorityErrorKind::CapabilityRequired => "capability_required",
        CredentialAuthorityErrorKind::Unavailable => "unavailable",
        CredentialAuthorityErrorKind::Unsupported => "unsupported",
        CredentialAuthorityErrorKind::MissingRoot => "missing_root",
        CredentialAuthorityErrorKind::MissingCredential => "missing_credential",
        CredentialAuthorityErrorKind::DeletedCredential => "deleted_credential",
        CredentialAuthorityErrorKind::LegacyConflict => "legacy_conflict",
        CredentialAuthorityErrorKind::WitnessExpired => "witness_expired",
        CredentialAuthorityErrorKind::WitnessConsumed => "witness_consumed",
        CredentialAuthorityErrorKind::WitnessMismatch => "witness_mismatch",
    }
}

fn redacted_requirement_label(requirement: &AuthRequirement) -> &'static str {
    match requirement {
        AuthRequirement::Ready => "ready",
        AuthRequirement::MissingOpaqueHandle { .. } => "missing_opaque_handle",
    }
}

fn digest_runtime_evidence(
    status: CredentialRuntimeStatus,
    reference_digest: Option<&str>,
    authority_evidence: Option<&str>,
    detail: Option<&str>,
) -> String {
    bytes_to_hex(Sha256::digest(
        serde_json::to_vec(&(
            RUNTIME_GATE_DIGEST_DOMAIN,
            status.as_str(),
            reference_digest,
            authority_evidence,
            detail,
        ))
        .expect("credential runtime evidence payload must serialize"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_platform::credential_authority::CredentialReadinessBinding;

    fn auth() -> Auth {
        Auth::Environment {
            environment_key: "TEST_TOKEN".to_string(),
            credential_name: "opaque-canary-handle".to_string(),
        }
    }

    fn binding() -> CredentialReadinessBinding {
        CredentialReadinessBinding::profile_snapshot_runtime("profile_1", 1, "managed_1", 3, 4)
    }

    fn enrollment_probe_binding(managed_mcp_id: &str) -> CredentialReadinessBinding {
        CredentialReadinessBinding::auth_requirement_probe(
            "managed_enrollment_runtime",
            managed_mcp_id,
        )
    }

    fn enrollment_binding(
        authority: &CredentialAuthority,
        handle: &str,
    ) -> EnrollmentFactRuntimeBinding {
        let handle = CredentialHandle::parse(handle).unwrap();
        let snapshot = authority
            .enrollment_reader()
            .readiness_snapshot(&handle, &enrollment_probe_binding("managed_1"))
            .unwrap();
        EnrollmentFactRuntimeBinding::new(
            "managed_1",
            7,
            "manifest-a",
            "static_env_secret",
            "manifest-a",
            "static_env_secret",
            handle.as_str(),
            credential_reference_digest(handle.as_str()),
            snapshot.provider_id(),
            "best_effort_single_process",
            snapshot.evidence_digest(),
            3,
        )
    }

    #[test]
    fn reference_digest_is_stable_and_irreversible() {
        let digest = credential_reference_digest("opaque-canary-handle");
        assert_eq!(digest.len(), 64);
        assert_ne!(digest, "opaque-canary-handle");
        assert_eq!(digest, credential_reference_digest("opaque-canary-handle"));
        assert_ne!(digest, credential_reference_digest("opaque-other-handle"));
    }

    #[test]
    fn provider_backed_runtime_snapshot_tracks_unconfigured_ready_and_deleted() {
        let authority = CredentialAuthority::in_memory_for_testing_default_time();

        let missing = CredentialAuthorityRuntimeGateAdapter::new(authority)
            .runtime_snapshot(&auth(), &binding())
            .unwrap();
        assert_eq!(missing.status(), CredentialRuntimeStatus::Unconfigured);
        assert!(!missing.is_ready());
    }

    #[test]
    fn provider_backed_runtime_snapshot_detects_ready_deleted_and_runtime_verification() {
        let authority = CredentialAuthority::in_memory_for_testing_default_time();
        let handle = CredentialHandle::parse("opaque-canary-handle").unwrap();
        authority
            .store_secret_for_trusted_write(authority.write_capability(), &handle, b"secret-1")
            .unwrap();
        let resolver = CredentialAuthorityRuntimeGateAdapter::new(authority);

        let ready = resolver.runtime_snapshot(&auth(), &binding()).unwrap();
        assert_eq!(ready.status(), CredentialRuntimeStatus::Ready);
        assert!(ready.is_ready());
        assert_eq!(ready.requirement(), &AuthRequirement::Ready);
        assert_eq!(ready.reference().unwrap().handle, "opaque-canary-handle");
        assert!(resolver
            .verify_runtime_binding(&auth(), &binding(), std::time::Duration::from_secs(5))
            .is_ok());

        resolver
            .authority
            .delete_secret_for_trusted_write(resolver.authority.write_capability(), &handle)
            .unwrap();
        let deleted = resolver.runtime_snapshot(&auth(), &binding()).unwrap();
        assert_eq!(
            deleted.status(),
            CredentialRuntimeStatus::ReRegistrationRequired
        );
        assert!(!deleted.is_ready());
    }

    #[test]
    fn runtime_snapshot_never_exposes_stored_secret_material() {
        let authority = CredentialAuthority::in_memory_for_testing_default_time();
        let handle = CredentialHandle::parse("opaque-canary-handle").unwrap();
        let secret = "super-secret-value";
        authority
            .store_secret_for_trusted_write(
                authority.write_capability(),
                &handle,
                secret.as_bytes(),
            )
            .unwrap();
        let resolver = CredentialAuthorityRuntimeGateAdapter::new(authority);

        let snapshot = resolver.runtime_snapshot(&auth(), &binding()).unwrap();
        let debug = format!("{snapshot:?}");
        assert!(snapshot.is_ready());
        assert_eq!(snapshot.reference().unwrap().handle, "opaque-canary-handle");
        assert_ne!(snapshot.evidence_digest(), secret);
        assert!(!debug.contains(secret));
        assert!(!debug.contains("opaque-canary-handle"));
        assert!(!debug.contains(snapshot.evidence_digest()));
        assert!(!snapshot
            .reference()
            .unwrap()
            .reference_digest
            .contains(secret));
    }

    #[test]
    fn enrollment_binding_debug_is_redacted() {
        let binding = EnrollmentFactRuntimeBinding::new(
            "managed_1",
            7,
            "manifest-a",
            "static_env_secret",
            "manifest-a",
            "static_env_secret",
            "enr.secret-handle",
            &"a".repeat(64),
            "in-memory-test",
            "best_effort_single_process",
            &"b".repeat(64),
            3,
        );
        let debug = format!("{binding:?}");
        assert!(!debug.contains("enr.secret-handle"));
        assert!(!debug.contains("manifest-a"));
        assert!(!debug.contains("static_env_secret"));
        assert!(!debug.contains("in-memory-test"));
        assert!(!debug.contains("best_effort_single_process"));
        assert!(!debug.contains(&"a".repeat(64)));
        assert!(!debug.contains(&"b".repeat(64)));
    }

    #[test]
    fn enrollment_backed_runtime_snapshot_covers_status_mapping_and_ignores_legacy_reference() {
        let authority = CredentialAuthority::in_memory_for_testing_default_time();
        let actual_handle = CredentialHandle::parse("enr.actual-handle").unwrap();
        authority
            .store_secret_for_trusted_write(
                authority.write_capability(),
                &actual_handle,
                b"secret-1",
            )
            .unwrap();
        let ready_binding = enrollment_binding(&authority, actual_handle.as_str());
        let auth = Auth::Environment {
            environment_key: "TEST_TOKEN".to_string(),
            credential_name: "legacy-credential-name".to_string(),
        };

        let none_snapshot = enrollment_backed_runtime_snapshot(
            &authority.enrollment_reader(),
            &Auth::None,
            EnrollmentBindingSource::Unavailable,
        );
        assert_eq!(none_snapshot.status(), CredentialRuntimeStatus::Ready);

        let missing = enrollment_backed_runtime_snapshot(
            &authority.enrollment_reader(),
            &auth,
            EnrollmentBindingSource::Missing,
        );
        assert_eq!(missing.status(), CredentialRuntimeStatus::Unconfigured);

        let source_unavailable = enrollment_backed_runtime_snapshot(
            &authority.enrollment_reader(),
            &auth,
            EnrollmentBindingSource::Unavailable,
        );
        assert_eq!(
            source_unavailable.status(),
            CredentialRuntimeStatus::TemporarilyUnavailable
        );

        let ready = enrollment_backed_runtime_snapshot(
            &authority.enrollment_reader(),
            &auth,
            EnrollmentBindingSource::Bound(&ready_binding),
        );
        assert_eq!(ready.status(), CredentialRuntimeStatus::Ready);
        assert_eq!(
            ready
                .reference()
                .expect("ready snapshot must keep enrolled reference")
                .handle,
            actual_handle.as_str()
        );

        let mut manifest_drift = ready_binding.clone();
        manifest_drift.enrolled_manifest_digest = "manifest-b".to_string();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Bound(&manifest_drift),
            )
            .status(),
            CredentialRuntimeStatus::ReRegistrationRequired
        );

        let mut schema_drift = ready_binding.clone();
        schema_drift.enrolled_schema_id = "bearer_token".to_string();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Bound(&schema_drift),
            )
            .status(),
            CredentialRuntimeStatus::ReRegistrationRequired
        );

        let mut missing_evidence = ready_binding.clone();
        missing_evidence.authority_evidence_digest.clear();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Bound(&missing_evidence),
            )
            .status(),
            CredentialRuntimeStatus::ReRegistrationRequired
        );

        let mut revision_drift = ready_binding.clone();
        revision_drift.current_managed_revision += 1;
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Bound(&revision_drift),
            )
            .status(),
            CredentialRuntimeStatus::ReRegistrationRequired
        );

        let mut ref_digest_mismatch = ready_binding.clone();
        ref_digest_mismatch.reference_digest = "f".repeat(64);
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Bound(&ref_digest_mismatch),
            )
            .status(),
            CredentialRuntimeStatus::TrustedStateConflict
        );

        let mut provider_mismatch = ready_binding.clone();
        provider_mismatch.authority_provider_id = "other-provider".to_string();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Bound(&provider_mismatch),
            )
            .status(),
            CredentialRuntimeStatus::TrustedStateConflict
        );

        let mut mode_mismatch = ready_binding.clone();
        mode_mismatch.authority_writer_mode = "compare_and_swap".to_string();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Bound(&mode_mismatch),
            )
            .status(),
            CredentialRuntimeStatus::TrustedStateConflict
        );

        let mut evidence_mismatch = ready_binding.clone();
        evidence_mismatch.authority_evidence_digest = "0".repeat(64);
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Bound(&evidence_mismatch),
            )
            .status(),
            CredentialRuntimeStatus::TrustedStateConflict
        );

        authority
            .delete_secret_for_trusted_write(authority.write_capability(), &actual_handle)
            .unwrap();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &authority.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Bound(&ready_binding),
            )
            .status(),
            CredentialRuntimeStatus::ReRegistrationRequired
        );
    }

    #[test]
    fn enrollment_backed_runtime_snapshot_reports_authority_unavailable() {
        let unavailable = CredentialAuthority::unavailable_for_testing_default_time();
        let auth = auth();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &unavailable.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Missing,
            )
            .status(),
            CredentialRuntimeStatus::TemporarilyUnavailable
        );

        let unsupported = CredentialAuthority::unsupported_for_testing_default_time();
        assert_eq!(
            enrollment_backed_runtime_snapshot(
                &unsupported.enrollment_reader(),
                &auth,
                EnrollmentBindingSource::Missing,
            )
            .status(),
            CredentialRuntimeStatus::TemporarilyUnavailable
        );
    }

    #[test]
    fn enrollment_revision_drift_fails_closed_before_authority_probe() {
        let unsupported = CredentialAuthority::unsupported_for_testing_default_time();
        let binding = EnrollmentFactRuntimeBinding::new(
            "managed_1",
            8,
            "manifest-a",
            "static_env_secret",
            "manifest-a",
            "static_env_secret",
            "enr.revision-drift",
            credential_reference_digest("enr.revision-drift"),
            "unsupported",
            "best_effort_single_process",
            "a".repeat(64),
            7,
        );

        let snapshot = enrollment_backed_runtime_snapshot(
            &unsupported.enrollment_reader(),
            &auth(),
            EnrollmentBindingSource::Bound(&binding),
        );

        assert_eq!(
            snapshot.status(),
            CredentialRuntimeStatus::ReRegistrationRequired
        );
    }
}
