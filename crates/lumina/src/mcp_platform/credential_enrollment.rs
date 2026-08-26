use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Mutex;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngExt;
use sha2::{Digest as _, Sha256};

use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::manifest::{
    CredentialEnrollmentSchema, CredentialFieldInputKind, CredentialFieldSpec,
};
use crate::utils::bytes_to_hex;

pub(crate) const ENROLLMENT_SESSION_TTL_MS: i64 = 120_000;
const CREDENTIAL_REFERENCE_DIGEST_DOMAIN: &str = "managed-credential-runtime-gate-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnrollmentFieldSpecView {
    pub id: String,
    pub label: String,
    pub input_kind: EnrollmentFieldInputKind,
    pub required: bool,
    pub secret: bool,
    pub help: String,
    pub validation: EnrollmentFieldValidationView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnrollmentFieldInputKind {
    SecretText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnrollmentFieldValidationView {
    pub min_length: usize,
    pub max_length: usize,
    pub reject_control_characters: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CredentialEnrollmentSchemaView {
    pub schema_id: String,
    pub fields: Vec<EnrollmentFieldSpecView>,
}

impl CredentialEnrollmentSchemaView {
    pub(crate) fn from_manifest(schema: &CredentialEnrollmentSchema) -> Self {
        Self {
            schema_id: schema.schema_id.to_string(),
            fields: schema
                .fields
                .iter()
                .map(EnrollmentFieldSpecView::from_manifest)
                .collect(),
        }
    }
}

impl EnrollmentFieldSpecView {
    fn from_manifest(spec: &CredentialFieldSpec) -> Self {
        Self {
            id: spec.id.to_string(),
            label: spec.label.to_string(),
            input_kind: match spec.input_kind {
                CredentialFieldInputKind::SecretText => EnrollmentFieldInputKind::SecretText,
            },
            required: spec.required,
            secret: spec.secret,
            help: spec.help.to_string(),
            validation: EnrollmentFieldValidationView {
                min_length: spec.validation.min_length,
                max_length: spec.validation.max_length,
                reject_control_characters: spec.validation.reject_control_characters,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnrollmentSessionView {
    pub session_token: String,
    pub managed_mcp_id: String,
    pub manifest_digest: String,
    pub schema: CredentialEnrollmentSchemaView,
    pub expected_revision: i64,
    pub profile_scope: Option<String>,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BeginEnrollmentSessionRequest {
    pub user_action_binding: UserActionBinding,
    pub managed_mcp_id: String,
    pub manifest_digest: String,
    pub schema: CredentialEnrollmentSchemaView,
    pub expected_revision: i64,
    pub profile_scope: Option<String>,
    pub authority_instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConsumeEnrollmentSessionRequest {
    pub session_token: String,
    pub user_action_binding: UserActionBinding,
    pub managed_mcp_id: String,
    pub manifest_digest: String,
    pub schema_id: String,
    pub expected_revision: i64,
    pub profile_scope: Option<String>,
    pub authority_instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CancelEnrollmentSessionRequest {
    pub session_token: String,
    pub user_action_binding: UserActionBinding,
    pub managed_mcp_id: String,
    pub manifest_digest: String,
    pub schema_id: String,
    pub expected_revision: i64,
    pub profile_scope: Option<String>,
    pub authority_instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConsumedEnrollmentSession {
    pub managed_mcp_id: String,
    pub manifest_digest: String,
    pub schema: CredentialEnrollmentSchemaView,
    pub expected_revision: i64,
    pub profile_scope: Option<String>,
    pub authority_instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnrollmentCancelResult {
    pub cancelled: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ValidatedEnrollmentSubmission {
    pub schema_id: String,
    pub redacted_field_count: usize,
    secret_value: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct UserActionBinding(String);

impl UserActionBinding {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Debug for UserActionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UserActionBinding([OPAQUE])")
    }
}

impl ValidatedEnrollmentSubmission {
    pub(crate) fn secret_bytes(&self) -> &[u8] {
        self.secret_value.as_bytes()
    }
}

impl fmt::Debug for ValidatedEnrollmentSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedEnrollmentSubmission")
            .field("schema_id", &self.schema_id)
            .field("redacted_field_count", &self.redacted_field_count)
            .field("secret_value", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for ValidatedEnrollmentSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ValidatedEnrollmentSubmission(schema_id={}, redacted_field_count={}, secret_value=[REDACTED])",
            self.schema_id, self.redacted_field_count
        )
    }
}

struct EnrollmentSession {
    user_action_binding: UserActionBinding,
    managed_mcp_id: String,
    manifest_digest: String,
    schema: CredentialEnrollmentSchemaView,
    expected_revision: i64,
    profile_scope: Option<String>,
    authority_instance_id: String,
    expires_at_ms: i64,
}

impl fmt::Debug for EnrollmentSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollmentSession")
            .field("managed_mcp_id", &self.managed_mcp_id)
            .field("manifest_digest", &self.manifest_digest)
            .field("schema_id", &self.schema.schema_id)
            .field("expected_revision", &self.expected_revision)
            .field("profile_scope", &self.profile_scope)
            .field("authority_instance_id", &self.authority_instance_id)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Default)]
pub(crate) struct EnrollmentCoordinator {
    sessions: Mutex<HashMap<String, EnrollmentSession>>,
}

impl EnrollmentCoordinator {
    pub(crate) fn begin(
        &self,
        request: BeginEnrollmentSessionRequest,
        now_ms: i64,
    ) -> McpPlatformResult<EnrollmentSessionView> {
        validate_revision(request.expected_revision)?;
        let token = session_token();
        let expires_at_ms = now_ms
            .checked_add(ENROLLMENT_SESSION_TTL_MS)
            .ok_or_else(temporarily_unavailable)?;
        let session = EnrollmentSession {
            user_action_binding: request.user_action_binding.clone(),
            managed_mcp_id: request.managed_mcp_id.clone(),
            manifest_digest: request.manifest_digest.clone(),
            schema: request.schema.clone(),
            expected_revision: request.expected_revision,
            profile_scope: request.profile_scope.clone(),
            authority_instance_id: request.authority_instance_id,
            expires_at_ms,
        };
        self.sessions
            .lock()
            .map_err(|_| temporarily_unavailable())?
            .insert(token.clone(), session);
        Ok(EnrollmentSessionView {
            session_token: token,
            managed_mcp_id: request.managed_mcp_id,
            manifest_digest: request.manifest_digest,
            schema: request.schema,
            expected_revision: request.expected_revision,
            profile_scope: request.profile_scope,
            expires_at_ms,
        })
    }

    pub(crate) fn cancel(
        &self,
        request: CancelEnrollmentSessionRequest,
        now_ms: i64,
    ) -> McpPlatformResult<EnrollmentCancelResult> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| temporarily_unavailable())?;
        let Some(session) = sessions.get(&request.session_token) else {
            return Err(session_stale());
        };
        if !session_matches_cancel_request(session, &request, now_ms) {
            return Err(session_stale());
        }
        sessions.remove(&request.session_token);
        Ok(EnrollmentCancelResult { cancelled: true })
    }

    pub(crate) fn peek(&self, session_token: &str) -> Option<EnrollmentSessionView> {
        self.sessions
            .lock()
            .ok()?
            .get(session_token)
            .map(|session| EnrollmentSessionView {
                session_token: session_token.to_string(),
                managed_mcp_id: session.managed_mcp_id.clone(),
                manifest_digest: session.manifest_digest.clone(),
                schema: session.schema.clone(),
                expected_revision: session.expected_revision,
                profile_scope: session.profile_scope.clone(),
                expires_at_ms: session.expires_at_ms,
            })
    }

    pub(crate) fn consume(
        &self,
        request: ConsumeEnrollmentSessionRequest,
        now_ms: i64,
    ) -> McpPlatformResult<ConsumedEnrollmentSession> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| temporarily_unavailable())?;
        let Some(session) = sessions.get(&request.session_token) else {
            return Err(session_stale());
        };
        if !session_matches_request(session, &request, now_ms) {
            return Err(session_stale());
        }
        let session = sessions
            .remove(&request.session_token)
            .expect("validated enrollment session must still exist");
        Ok(ConsumedEnrollmentSession {
            managed_mcp_id: session.managed_mcp_id,
            manifest_digest: session.manifest_digest,
            schema: session.schema,
            expected_revision: session.expected_revision,
            profile_scope: session.profile_scope,
            authority_instance_id: session.authority_instance_id,
        })
    }

    pub(crate) fn validate_submission(
        &self,
        schema: &CredentialEnrollmentSchemaView,
        submitted_fields: Vec<EnrollmentFieldSubmission>,
    ) -> McpPlatformResult<ValidatedEnrollmentSubmission> {
        let mut fields = BTreeMap::new();
        for field in submitted_fields {
            if fields
                .insert(field.id.clone(), field.value.into_inner())
                .is_some()
            {
                return Err(invalid_submission());
            }
        }
        if schema.fields.len() != 1 {
            return Err(temporarily_unavailable());
        }
        let field = &schema.fields[0];
        let value = fields.remove(&field.id).ok_or_else(invalid_submission)?;
        if !fields.is_empty() {
            return Err(invalid_submission());
        }
        validate_secret_value(&value, field.validation)?;
        Ok(ValidatedEnrollmentSubmission {
            schema_id: schema.schema_id.clone(),
            redacted_field_count: 1,
            secret_value: value,
        })
    }
}

fn session_matches_request(
    session: &EnrollmentSession,
    request: &ConsumeEnrollmentSessionRequest,
    now_ms: i64,
) -> bool {
    session.user_action_binding == request.user_action_binding
        && session.expires_at_ms > now_ms
        && session.managed_mcp_id == request.managed_mcp_id
        && session.manifest_digest == request.manifest_digest
        && session.schema.schema_id == request.schema_id
        && session.expected_revision == request.expected_revision
        && session.profile_scope == request.profile_scope
        && session.authority_instance_id == request.authority_instance_id
}

fn session_matches_cancel_request(
    session: &EnrollmentSession,
    request: &CancelEnrollmentSessionRequest,
    now_ms: i64,
) -> bool {
    session.user_action_binding == request.user_action_binding
        && session.expires_at_ms > now_ms
        && session.managed_mcp_id == request.managed_mcp_id
        && session.manifest_digest == request.manifest_digest
        && session.schema.schema_id == request.schema_id
        && session.expected_revision == request.expected_revision
        && session.profile_scope == request.profile_scope
        && session.authority_instance_id == request.authority_instance_id
}

pub(crate) struct EnrollmentSecret(String);

impl EnrollmentSecret {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Debug for EnrollmentSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentSecret([REDACTED])")
    }
}

pub(crate) struct EnrollmentFieldSubmission {
    pub id: String,
    pub value: EnrollmentSecret,
}

impl fmt::Debug for EnrollmentFieldSubmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollmentFieldSubmission")
            .field("id", &self.id)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

pub(crate) fn credential_reference_digest(handle: &str) -> String {
    bytes_to_hex(Sha256::digest(
        serde_json::to_vec(&(CREDENTIAL_REFERENCE_DIGEST_DOMAIN, "reference", handle))
            .expect("credential reference digest payload must serialize"),
    ))
}

pub(crate) fn next_credential_reference() -> String {
    format!("enr.{}", uuid::Uuid::now_v7().simple())
}

fn session_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn validate_secret_value(
    value: &str,
    validation: EnrollmentFieldValidationView,
) -> McpPlatformResult<()> {
    if value.len() < validation.min_length
        || value.len() > validation.max_length
        || (validation.reject_control_characters
            && value.chars().any(|character| character.is_control()))
    {
        return Err(invalid_submission());
    }
    Ok(())
}

fn validate_revision(revision: i64) -> McpPlatformResult<()> {
    if revision < 0 {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::InvalidRequest,
            "credential enrollment requires a non-negative revision",
        ));
    }
    Ok(())
}

fn invalid_submission() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidRequest,
        "credential enrollment submission is invalid",
    )
}

fn session_stale() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PlanStale,
        "credential enrollment session is stale",
    )
}

fn temporarily_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityUnavailable,
        "credential enrollment is temporarily unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(schema_id: &str) -> CredentialEnrollmentSchemaView {
        CredentialEnrollmentSchemaView {
            schema_id: schema_id.to_string(),
            fields: vec![EnrollmentFieldSpecView {
                id: "secret".to_string(),
                label: "Credential".to_string(),
                input_kind: EnrollmentFieldInputKind::SecretText,
                required: true,
                secret: true,
                help: "Provide the trusted secret.".to_string(),
                validation: EnrollmentFieldValidationView {
                    min_length: 1,
                    max_length: 64,
                    reject_control_characters: true,
                },
            }],
        }
    }

    fn begin_request() -> BeginEnrollmentSessionRequest {
        BeginEnrollmentSessionRequest {
            user_action_binding: UserActionBinding::new("action_1".to_string()),
            managed_mcp_id: "managed_1".to_string(),
            manifest_digest: "a".repeat(64),
            schema: schema("bearer_token"),
            expected_revision: 7,
            profile_scope: Some("profile_1".to_string()),
            authority_instance_id: "authority_1".to_string(),
        }
    }

    fn consume_request(
        session_token: String,
        user_action_binding: UserActionBinding,
    ) -> ConsumeEnrollmentSessionRequest {
        ConsumeEnrollmentSessionRequest {
            session_token,
            user_action_binding,
            managed_mcp_id: "managed_1".to_string(),
            manifest_digest: "a".repeat(64),
            schema_id: "bearer_token".to_string(),
            expected_revision: 7,
            profile_scope: Some("profile_1".to_string()),
            authority_instance_id: "authority_1".to_string(),
        }
    }

    fn cancel_request(
        session_token: String,
        user_action_binding: UserActionBinding,
        profile_scope: Option<&str>,
    ) -> CancelEnrollmentSessionRequest {
        CancelEnrollmentSessionRequest {
            session_token,
            user_action_binding,
            managed_mcp_id: "managed_1".to_string(),
            manifest_digest: "a".repeat(64),
            schema_id: "bearer_token".to_string(),
            expected_revision: 7,
            profile_scope: profile_scope.map(str::to_string),
            authority_instance_id: "authority_1".to_string(),
        }
    }

    #[test]
    fn begin_cancel_expiry_and_replay_are_fail_closed() {
        let coordinator = EnrollmentCoordinator::default();
        let request = begin_request();
        let binding = request.user_action_binding.clone();
        let begun = coordinator.begin(request, 10).unwrap();
        assert_eq!(begun.expires_at_ms, 10 + ENROLLMENT_SESSION_TTL_MS);
        assert_eq!(
            coordinator
                .cancel(
                    cancel_request(
                        begun.session_token.clone(),
                        binding.clone(),
                        Some("profile_1"),
                    ),
                    11
                )
                .unwrap()
                .cancelled,
            true
        );
        assert_eq!(
            coordinator
                .cancel(
                    cancel_request(
                        begun.session_token.clone(),
                        binding.clone(),
                        Some("profile_1")
                    ),
                    12,
                )
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
        assert_eq!(
            coordinator
                .consume(
                    consume_request(begun.session_token.clone(), binding.clone()),
                    13
                )
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );

        let request = begin_request();
        let binding = request.user_action_binding.clone();
        let begun = coordinator.begin(request, 20).unwrap();
        let _consumed = coordinator
            .consume(
                consume_request(begun.session_token.clone(), binding.clone()),
                21,
            )
            .unwrap();
        assert_eq!(
            coordinator
                .cancel(
                    cancel_request(begun.session_token, binding, Some("profile_1")),
                    22
                )
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );

        let request = begin_request();
        let binding = request.user_action_binding.clone();
        let begun = coordinator.begin(request, 30).unwrap();
        assert_eq!(
            coordinator
                .cancel(
                    cancel_request(begun.session_token.clone(), binding, Some("profile_1")),
                    30 + ENROLLMENT_SESSION_TTL_MS,
                )
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
        assert!(coordinator.peek(&begun.session_token).is_some());
    }

    #[test]
    fn cancel_binding_drift_is_rejected_without_consuming_session() {
        let coordinator = EnrollmentCoordinator::default();
        let request = begin_request();
        let binding = request.user_action_binding.clone();
        let begun = coordinator.begin(request, 10).unwrap();

        for request in [
            CancelEnrollmentSessionRequest {
                managed_mcp_id: "managed_other".to_string(),
                ..cancel_request(
                    begun.session_token.clone(),
                    binding.clone(),
                    Some("profile_1"),
                )
            },
            CancelEnrollmentSessionRequest {
                manifest_digest: "b".repeat(64),
                ..cancel_request(
                    begun.session_token.clone(),
                    binding.clone(),
                    Some("profile_1"),
                )
            },
            CancelEnrollmentSessionRequest {
                schema_id: "api_key_header".to_string(),
                ..cancel_request(
                    begun.session_token.clone(),
                    binding.clone(),
                    Some("profile_1"),
                )
            },
            CancelEnrollmentSessionRequest {
                expected_revision: 8,
                ..cancel_request(
                    begun.session_token.clone(),
                    binding.clone(),
                    Some("profile_1"),
                )
            },
            cancel_request(begun.session_token.clone(), binding.clone(), None),
            CancelEnrollmentSessionRequest {
                authority_instance_id: "authority_2".to_string(),
                ..cancel_request(
                    begun.session_token.clone(),
                    binding.clone(),
                    Some("profile_1"),
                )
            },
        ] {
            assert_eq!(
                coordinator.cancel(request, 11).unwrap_err().code(),
                McpPlatformErrorCode::PlanStale
            );
            assert!(coordinator.peek(&begun.session_token).is_some());
        }

        assert_eq!(
            coordinator
                .cancel(
                    cancel_request(begun.session_token.clone(), binding, Some("profile_1")),
                    12
                )
                .unwrap()
                .cancelled,
            true
        );
        assert!(coordinator.peek(&begun.session_token).is_none());
    }

    #[test]
    fn consume_drift_is_rejected_without_consuming_session() {
        let coordinator = EnrollmentCoordinator::default();
        let request = begin_request();
        let binding = request.user_action_binding.clone();
        let begun = coordinator.begin(request, 10).unwrap();

        for request in [
            consume_request(
                begun.session_token.clone(),
                UserActionBinding::new("wrong_action".to_string()),
            ),
            ConsumeEnrollmentSessionRequest {
                managed_mcp_id: "managed_other".to_string(),
                ..consume_request(begun.session_token.clone(), binding.clone())
            },
            ConsumeEnrollmentSessionRequest {
                manifest_digest: "b".repeat(64),
                ..consume_request(begun.session_token.clone(), binding.clone())
            },
            ConsumeEnrollmentSessionRequest {
                schema_id: "api_key_header".to_string(),
                ..consume_request(begun.session_token.clone(), binding.clone())
            },
            ConsumeEnrollmentSessionRequest {
                expected_revision: 8,
                ..consume_request(begun.session_token.clone(), binding.clone())
            },
            ConsumeEnrollmentSessionRequest {
                profile_scope: None,
                ..consume_request(begun.session_token.clone(), binding.clone())
            },
            ConsumeEnrollmentSessionRequest {
                authority_instance_id: "authority_2".to_string(),
                ..consume_request(begun.session_token.clone(), binding.clone())
            },
        ] {
            assert_eq!(
                coordinator.consume(request, 11).unwrap_err().code(),
                McpPlatformErrorCode::PlanStale
            );
            assert!(coordinator.peek(&begun.session_token).is_some());
        }

        let consumed = coordinator
            .consume(consume_request(begun.session_token.clone(), binding), 12)
            .unwrap();
        assert_eq!(consumed.managed_mcp_id, "managed_1");
        assert!(coordinator.peek(&begun.session_token).is_none());
    }

    #[test]
    fn expired_consume_fails_closed_without_removing_session() {
        let coordinator = EnrollmentCoordinator::default();
        let request = begin_request();
        let binding = request.user_action_binding.clone();
        let begun = coordinator.begin(request, 10).unwrap();

        assert_eq!(
            coordinator
                .consume(
                    consume_request(begun.session_token.clone(), binding),
                    10 + ENROLLMENT_SESSION_TTL_MS,
                )
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
        assert!(coordinator.peek(&begun.session_token).is_some());
    }

    #[test]
    fn replayed_consume_is_rejected_after_session_is_already_spent() {
        let coordinator = EnrollmentCoordinator::default();
        let request = begin_request();
        let binding = request.user_action_binding.clone();
        let begun = coordinator.begin(request, 10).unwrap();

        coordinator
            .consume(
                consume_request(begun.session_token.clone(), binding.clone()),
                11,
            )
            .unwrap();
        assert_eq!(
            coordinator
                .consume(consume_request(begun.session_token.clone(), binding), 12)
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
        assert!(coordinator.peek(&begun.session_token).is_none());
    }

    #[test]
    fn field_validation_and_debug_views_never_leak_secret() {
        let coordinator = EnrollmentCoordinator::default();
        let valid = coordinator
            .validate_submission(
                &schema("static_header_secret"),
                vec![EnrollmentFieldSubmission {
                    id: "secret".to_string(),
                    value: EnrollmentSecret::new("top-secret".to_string()),
                }],
            )
            .unwrap();
        assert_eq!(valid.redacted_field_count, 1);
        assert!(!format!("{valid:?}").contains("top-secret"));
        assert!(!format!("{valid}").contains("top-secret"));
        assert!(format!("{valid:?}").contains("schema_id"));
        assert!(format!("{valid}").contains("redacted_field_count=1"));

        let error = coordinator
            .validate_submission(
                &schema("static_header_secret"),
                vec![EnrollmentFieldSubmission {
                    id: "secret".to_string(),
                    value: EnrollmentSecret::new("bad\nsecret".to_string()),
                }],
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);
        assert!(!error.to_string().contains("bad"));
    }

    #[test]
    fn reference_digest_is_stable_and_rotation_changes_reference() {
        let first = next_credential_reference();
        let second = next_credential_reference();
        assert!(first.starts_with("enr."));
        assert!(second.starts_with("enr."));
        assert_ne!(first, second);
        assert_eq!(
            credential_reference_digest(&first),
            credential_reference_digest(&first)
        );
        assert_ne!(
            credential_reference_digest(&first),
            credential_reference_digest(&second)
        );
    }
}
