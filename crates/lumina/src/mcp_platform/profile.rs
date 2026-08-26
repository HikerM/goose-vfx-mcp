//! Public profile views intentionally exclude internal digest/provenance state.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::agents::ExtensionConfig;
use crate::mcp_platform::domain::ManagedMcpState;
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::utils::bytes_to_hex;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileEntry {
    pub managed_mcp_id: String,
    pub ordinal: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileCredentialReference {
    pub managed_mcp_id: String,
    pub credential_reference: String,
    pub reference_digest: String,
}

impl fmt::Debug for ProfileCredentialReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileCredentialReference")
            .field("managed_mcp_id", &self.managed_mcp_id)
            .field("credential_reference", &"[REDACTED]")
            .field("reference_digest", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpProfile {
    pub profile_id: String,
    pub name: String,
    pub description: String,
    pub revision: i64,
    pub archived: bool,
    pub entries: Vec<ProfileEntry>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRevision {
    pub profile_id: String,
    pub revision: i64,
    pub name: String,
    pub description: String,
    pub archived: bool,
    pub entries: Vec<ProfileEntry>,
    pub actor: String,
    pub operation: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileDraftCandidate {
    pub managed_mcp_id: String,
    pub mcp_id: String,
    pub name: String,
    pub description: String,
    pub confidence: f32,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileDraft {
    pub locale: String,
    pub name: String,
    pub description: String,
    pub entries: Vec<ProfileEntry>,
    pub candidates: Vec<ProfileDraftCandidate>,
    pub unresolved_terms: Vec<String>,
    pub low_confidence: bool,
    pub persisted: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredProfileManagedSnapshot {
    pub managed_mcp_id: String,
    pub managed_revision: i64,
    pub manifest_digest: String,
    pub projection_revision: i64,
    pub projection_digest: String,
    pub health: String,
    pub auth_ready: bool,
    pub policy_ready: bool,
    #[serde(default)]
    pub auth_evidence_digest: String,
    #[serde(default)]
    pub policy_evidence_digest: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredProfileApplyPlan {
    pub plan_id: String,
    pub profile_id: String,
    pub profile_revision: i64,
    #[serde(alias = "plan_digest")]
    pub internal_plan_digest: String,
    pub merge_policy: String,
    pub entries: Vec<StoredProfileManagedSnapshot>,
    pub actor: String,
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
}

pub(crate) fn profile_apply_plan_snapshot_digest(
    plan: &StoredProfileApplyPlan,
) -> McpPlatformResult<String> {
    #[derive(Serialize)]
    struct DigestSnapshot<'a> {
        schema: &'static str,
        plan_id: &'a str,
        profile_id: &'a str,
        profile_revision: i64,
        merge_policy: &'a str,
        entries: &'a [StoredProfileManagedSnapshot],
        actor: &'a str,
        expires_at_ms: i64,
        created_at_ms: i64,
    }

    let value = serde_json::to_value(DigestSnapshot {
        schema: "profile-apply-plan-snapshot-v2",
        plan_id: &plan.plan_id,
        profile_id: &plan.profile_id,
        profile_revision: plan.profile_revision,
        merge_policy: &plan.merge_policy,
        entries: &plan.entries,
        actor: &plan.actor,
        expires_at_ms: plan.expires_at_ms,
        created_at_ms: plan.created_at_ms,
    })
    .map_err(|_| fingerprint_error())?;
    let bytes = canonical_json_bytes(value)?;
    Ok(bytes_to_hex(Sha256::digest(bytes)))
}

impl fmt::Debug for StoredProfileManagedSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredProfileManagedSnapshot")
            .field("managed_mcp_id", &self.managed_mcp_id)
            .field("managed_revision", &self.managed_revision)
            .field("manifest_digest", &"[REDACTED]")
            .field("projection_revision", &self.projection_revision)
            .field("projection_digest", &"[REDACTED]")
            .field("health", &self.health)
            .field("auth_ready", &self.auth_ready)
            .field("policy_ready", &self.policy_ready)
            .field("auth_evidence_digest", &"[REDACTED]")
            .field("policy_evidence_digest", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for StoredProfileApplyPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredProfileApplyPlan")
            .field("plan_id", &self.plan_id)
            .field("profile_id", &self.profile_id)
            .field("profile_revision", &self.profile_revision)
            .field("internal_plan_digest", &"[REDACTED]")
            .field("merge_policy", &self.merge_policy)
            .field("entries", &self.entries)
            .field("actor", &"[REDACTED]")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

impl StoredProfileApplyPlan {
    pub(crate) fn internal_plan_digest(&self) -> &str {
        &self.internal_plan_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileApplyPlanView {
    pub plan_id: String,
    pub profile_id: String,
    pub profile_revision: i64,
    pub merge_policy: String,
    pub entries: Vec<ProfileApplyEntryView>,
    pub expires_at_ms: i64,
    pub confirmation: ProfileApplyConfirmationView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileApplyEntryView {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileApplyConfirmationView {
    pub confirmation_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileApplicationTokenView {
    pub token: String,
    pub plan_id: String,
    pub profile_id: String,
    pub profile_revision: i64,
    pub expires_at_ms: i64,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StoredProfileApplyConfirmation {
    pub token: String,
    pub plan_id: String,
    pub actor: String,
    pub expires_at_ms: i64,
}

impl fmt::Debug for StoredProfileApplyConfirmation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredProfileApplyConfirmation")
            .field("token", &"[REDACTED]")
            .field("plan_id", &self.plan_id)
            .field("actor", &"[REDACTED]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct ConsumedProfileApplication {
    pub(crate) application_id: String,
    pub(crate) profile_id: String,
    pub(crate) profile_revision: i64,
    pub(crate) plan_digest: String,
    managed_extension_names: Vec<String>,
    managed_references: Vec<ProfileManagedReference>,
    extensions: Vec<ExtensionConfig>,
    runtime_digest: String,
}

impl fmt::Debug for ConsumedProfileApplication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConsumedProfileApplication")
            .field("application_id", &self.application_id)
            .field("profile_id", &self.profile_id)
            .field("profile_revision", &self.profile_revision)
            .field("plan_digest", &"[REDACTED]")
            .field("managed_extension_names", &self.managed_extension_names)
            .field("managed_references", &self.managed_references)
            .field("extensions", &self.extensions)
            .field("runtime_digest", &"[REDACTED]")
            .finish()
    }
}

impl ConsumedProfileApplication {
    pub(crate) fn verified(
        application_id: String,
        profile_id: String,
        profile_revision: i64,
        plan_digest: String,
        managed_extension_names: Vec<String>,
        managed_references: Vec<ProfileManagedReference>,
        extensions: Vec<ExtensionConfig>,
    ) -> McpPlatformResult<Self> {
        let runtime_digest = consumed_profile_runtime_digest(
            &application_id,
            &profile_id,
            profile_revision,
            &plan_digest,
            &managed_extension_names,
            &managed_references,
            &extensions,
        )?;
        Ok(Self {
            application_id,
            profile_id,
            profile_revision,
            plan_digest,
            managed_extension_names,
            managed_references,
            extensions,
            runtime_digest,
        })
    }

    pub(crate) fn managed_extension_names(&self) -> &[String] {
        &self.managed_extension_names
    }

    pub(crate) fn managed_references(&self) -> &[ProfileManagedReference] {
        &self.managed_references
    }

    pub(crate) fn extensions(&self) -> &[ExtensionConfig] {
        &self.extensions
    }

    pub(crate) fn verify_runtime_integrity(&self) -> McpPlatformResult<()> {
        let actual = consumed_profile_runtime_digest(
            &self.application_id,
            &self.profile_id,
            self.profile_revision,
            &self.plan_digest,
            &self.managed_extension_names,
            &self.managed_references,
            &self.extensions,
        )?;
        if actual == self.runtime_digest {
            Ok(())
        } else {
            Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "consumed profile application integrity validation failed",
            ))
        }
    }
}

fn consumed_profile_runtime_digest(
    application_id: &str,
    profile_id: &str,
    profile_revision: i64,
    plan_digest: &str,
    managed_extension_names: &[String],
    managed_references: &[ProfileManagedReference],
    extensions: &[ExtensionConfig],
) -> McpPlatformResult<String> {
    digest_evidence(&(
        "consumed-profile-runtime-v1",
        application_id,
        profile_id,
        profile_revision,
        plan_digest,
        managed_extension_names,
        managed_references,
        extensions,
    ))
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileManagedReference {
    pub(crate) managed_mcp_id: String,
    #[serde(default)]
    pub(crate) extension_name: String,
    pub(crate) projection_digest: String,
    pub(crate) source_fingerprint: String,
}

impl fmt::Debug for ProfileManagedReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileManagedReference")
            .field("managed_mcp_id", &self.managed_mcp_id)
            .field("extension_name", &self.extension_name)
            .field("projection_digest", &"[REDACTED]")
            .field("source_fingerprint", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileApplicationMarker {
    pub(crate) application_id: String,
    pub(crate) profile_id: String,
    pub(crate) profile_revision: i64,
    pub(crate) merge_policy: String,
    pub(crate) original_session_id: String,
    pub(crate) managed_extension_names: Vec<String>,
}

impl fmt::Debug for ProfileApplicationMarker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileApplicationMarker")
            .field("application_id", &self.application_id)
            .field("profile_id", &self.profile_id)
            .field("profile_revision", &self.profile_revision)
            .field("merge_policy", &self.merge_policy)
            .field("original_session_id", &self.original_session_id)
            .field("managed_extension_names", &self.managed_extension_names)
            .finish()
    }
}

impl ProfileApplicationMarker {
    pub(crate) fn strip_managed_extensions(&self, extensions: &mut Vec<ExtensionConfig>) {
        extensions.retain(|extension| {
            !self
                .managed_extension_names
                .iter()
                .any(|name| *name == extension.name())
        });
    }
}

pub(crate) fn extension_source_fingerprint(config: &ExtensionConfig) -> McpPlatformResult<String> {
    let mut value = serde_json::to_value(config).map_err(|_| fingerprint_error())?;
    let object = value.as_object_mut().ok_or_else(fingerprint_error)?;
    for key in [
        "name",
        "description",
        "display_name",
        "available_tools",
        "timeout",
        "bundled",
    ] {
        object.remove(key);
    }
    let bytes = serde_json::to_vec(&value).map_err(|_| fingerprint_error())?;
    Ok(bytes_to_hex(Sha256::digest(bytes)))
}

pub(crate) fn profile_auth_evidence_digest(
    resolver_evidence_digest: &str,
    managed_revision: i64,
    manifest_digest: &str,
    projection_revision: i64,
    projection_digest: &str,
) -> McpPlatformResult<String> {
    digest_evidence(&(
        "profile-auth-evidence-v1",
        resolver_evidence_digest,
        managed_revision,
        manifest_digest,
        projection_revision,
        projection_digest,
    ))
}

pub(crate) fn profile_policy_evidence_digest(
    state: &ManagedMcpState,
    managed_revision: i64,
    manifest_digest: &str,
    projection_revision: i64,
    projection_digest: &str,
) -> McpPlatformResult<String> {
    digest_evidence(&(
        "profile-policy-evidence-v1",
        state.registration,
        state.installation,
        state.default_enabled,
        &state.tool_policies,
        managed_revision,
        manifest_digest,
        projection_revision,
        projection_digest,
    ))
}

fn digest_evidence(value: &impl Serialize) -> McpPlatformResult<String> {
    let bytes = serde_json::to_vec(value).map_err(|_| fingerprint_error())?;
    Ok(bytes_to_hex(Sha256::digest(bytes)))
}

fn canonical_json_bytes(value: serde_json::Value) -> McpPlatformResult<Vec<u8>> {
    fn canonicalize(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => {
                let mut entries = object.into_iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                serde_json::Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, canonicalize(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonicalize).collect())
            }
            value => value,
        }
    }

    serde_json::to_vec(&canonicalize(value)).map_err(|_| fingerprint_error())
}

fn fingerprint_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::SerializationFailed,
        "failed to derive managed MCP provenance",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRecommendationCandidate {
    pub provider_id: String,
    pub model_id: String,
    pub confidence_millis: u16,
    pub reason_codes: Vec<String>,
    pub caveat_codes: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SaveProfile<'a> {
    pub(crate) profile_id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) description: &'a str,
    pub(crate) entries: &'a [ProfileEntry],
    pub(crate) idempotency_key: &'a str,
    pub(crate) request_digest: &'a str,
    pub(crate) actor: &'a str,
    pub(crate) now_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct ChangeProfile<'a> {
    pub(crate) profile_id: &'a str,
    pub(crate) expected_revision: i64,
    pub(crate) name: &'a str,
    pub(crate) description: &'a str,
    pub(crate) entries: &'a [ProfileEntry],
    pub(crate) archived: bool,
    pub(crate) operation: &'a str,
    pub(crate) idempotency_key: &'a str,
    pub(crate) request_digest: &'a str,
    pub(crate) actor: &'a str,
    pub(crate) now_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct SaveProfileApplyPlan<'a> {
    pub(crate) plan: &'a StoredProfileApplyPlan,
    pub(crate) idempotency_key: &'a str,
    pub(crate) request_digest: &'a str,
}

#[derive(Debug, Clone)]
pub(crate) struct SaveProfileApplicationToken<'a> {
    pub(crate) application_id: &'a str,
    pub(crate) confirmation_hash: &'a str,
    pub(crate) token_hash: &'a str,
    pub(crate) plan_id: &'a str,
    pub(crate) internal_plan_digest: &'a str,
    pub(crate) actor: &'a str,
    pub(crate) expires_at_ms: i64,
    pub(crate) created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct SaveProfileApplyConfirmation<'a> {
    pub(crate) confirmation_hash: &'a str,
    pub(crate) plan_id: &'a str,
    pub(crate) actor: &'a str,
    pub(crate) expires_at_ms: i64,
    pub(crate) created_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::{ProfileApplicationMarker, ProfileCredentialReference};
    use crate::session::{ExtensionData, ExtensionState};

    #[test]
    fn profile_credential_reference_debug_is_redacted() {
        let reference = ProfileCredentialReference {
            managed_mcp_id: "managed_profile".to_string(),
            credential_reference: "opaque-profile-handle".to_string(),
            reference_digest: "a".repeat(64),
        };
        let debug = format!("{reference:?}");
        assert!(debug.contains("managed_profile"));
        assert!(!debug.contains("opaque-profile-handle"));
        assert!(!debug.contains(&"a".repeat(64)));
    }

    #[test]
    fn profile_application_marker_serialization_excludes_digest_and_provenance() {
        let mut extension_data = ExtensionData::new();
        ProfileApplicationMarker {
            application_id: "application-id".to_string(),
            profile_id: "profile-id".to_string(),
            profile_revision: 7,
            merge_policy: "replace_managed_only".to_string(),
            original_session_id: "session-id".to_string(),
            managed_extension_names: vec!["managed-owned".to_string()],
        }
        .to_extension_data(&mut extension_data)
        .unwrap();

        let json = serde_json::to_string(&extension_data).unwrap();
        assert!(json.contains("managed-owned"));
        assert!(!json.contains("plan_digest"));
        assert!(!json.contains("managed_references"));
        assert!(!json.contains("projection_digest"));
        assert!(!json.contains("source_fingerprint"));
    }
}
