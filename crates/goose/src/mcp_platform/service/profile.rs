use std::collections::{BTreeSet, HashSet};

use crate::utils::bytes_to_hex;
use sha2::{Digest, Sha256};

use crate::agents::ExtensionConfig;
use crate::mcp_platform::credential_runtime_gate::CredentialRuntimeStatus;
use crate::mcp_platform::domain::{HealthState, InstallationState, RegistrationState};
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::lifecycle::TransportProjectionAdapter;
use crate::mcp_platform::repository::{ManagedInventoryFilter, ManagedMcpInventoryRecord};
use crate::mcp_platform::{
    extension_source_fingerprint, profile_apply_plan_snapshot_digest, profile_auth_evidence_digest,
    profile_policy_evidence_digest, ChangeProfile, ConsumedProfileApplication, McpProfile,
    ProfileApplicationMarker, ProfileApplicationTokenView, ProfileApplyConfirmationView,
    ProfileApplyEntryView, ProfileApplyPlanView, ProfileDraft, ProfileDraftCandidate, ProfileEntry,
    ProfileManagedReference, SaveProfile, SaveProfileApplicationToken,
    SaveProfileApplyConfirmation, SaveProfileApplyPlan, StoredProfileApplyConfirmation,
    StoredProfileApplyPlan, StoredProfileManagedSnapshot,
};

use super::dto::CredentialStatus;
use super::{McpPlatformService, RequestContext};

const PROFILE_PLAN_TTL_MS: i64 = 10 * 60 * 1000;
const PROFILE_TOKEN_TTL_MS: i64 = 2 * 60 * 1000;
const COMPLETE_INVENTORY_PAGE_SIZE: usize = 512;
const COMPLETE_INVENTORY_MAX_ITEMS: usize = 100_000;

#[derive(Debug, Clone)]
pub struct ProfileCreateInput {
    pub name: String,
    pub description: String,
    pub managed_mcp_ids: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone)]
pub struct ProfileUpdateInput {
    pub profile_id: String,
    pub expected_revision: i64,
    pub name: String,
    pub description: String,
    pub managed_mcp_ids: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone)]
pub struct ProfileRestoreInput {
    pub profile_id: String,
    pub source_revision: i64,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone)]
pub struct ProfileArchiveInput {
    pub profile_id: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone)]
pub struct ProfileApplyPlanInput {
    pub profile_id: String,
    pub profile_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ProfileConnectionTestTarget {
    pub(crate) managed_mcp_id: String,
    pub(crate) extension_config: ExtensionConfig,
}

impl McpPlatformService {
    pub(crate) async fn profile_connection_test_targets(
        &self,
        context: &RequestContext,
        profile_id: &str,
    ) -> McpPlatformResult<Vec<String>> {
        self.require_mutation_authority()?;
        validate_profile_id(profile_id)?;
        self.repository.verify_integrity().await?;
        let profile = self.repository.get_profile(profile_id).await?;
        if profile.archived || profile.entries.is_empty() {
            return Err(invalid_request(
                "profile is not available for connection testing",
            ));
        }

        let mut targets = Vec::with_capacity(profile.entries.len());
        for entry in profile.entries {
            let detail = self
                .managed_get(
                    context,
                    super::dto::ManagedGetInput {
                        managed_mcp_id: entry.managed_mcp_id.clone(),
                    },
                )
                .await?;
            let summary = &detail.summary;
            if summary.registration != RegistrationState::Registered
                || !matches!(
                    summary.installation,
                    InstallationState::Installed | InstallationState::NotApplicable
                )
                || !summary.default_enabled
                || summary.health != HealthState::Healthy
                || summary.recovery_required
                || summary.credential_status != Some(super::dto::CredentialStatus::Ready)
            {
                return Err(invalid_request(
                    "profile contains an MCP that is not ready for connection testing",
                ));
            }

            targets.push(entry.managed_mcp_id);
        }
        Ok(targets)
    }

    /// Acquires the runtime projection immediately before a connection test starts it.
    /// On Windows only managed local projections require activation before config construction.
    pub(crate) async fn profile_connection_test_target(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ProfileConnectionTestTarget> {
        self.require_mutation_authority()?;
        let projection = self
            .repository
            .get_connection_projection(managed_mcp_id)
            .await?;
        projection.projection.require_runtime_transport()?;
        self.validate_managed_plan_authority(
            &self
                .repository
                .get_managed_inventory(managed_mcp_id)
                .await?,
        )
        .await?;

        #[cfg(windows)]
        let extension_config = match &projection.projection {
            crate::mcp_platform::ConnectionProjection::RemoteHttp { .. }
            | crate::mcp_platform::ConnectionProjection::ManualStdio { .. } => {
                crate::mcp_platform::CoreTransportProjectionAdapter
                    .extension_config(&projection.projection, &projection.link_key)?
            }
            crate::mcp_platform::ConnectionProjection::ManagedStdio { .. }
            | crate::mcp_platform::ConnectionProjection::ManagedDockerStdio { .. } => {
                let activation = self
                    .runner
                    .acquire_verified_runtime_activation(managed_mcp_id)
                    .await?;
                if activation.projection() != &projection.projection {
                    return Err(McpPlatformError::new(
                        McpPlatformErrorCode::IntegrityError,
                        "managed MCP runtime activation no longer matches its anchored configuration",
                    ));
                }
                crate::mcp_platform::CoreTransportProjectionAdapter
                    .extension_config(activation.projection(), &projection.link_key)?
            }
        };
        #[cfg(not(windows))]
        let extension_config = crate::mcp_platform::CoreTransportProjectionAdapter
            .extension_config(&projection.projection, &projection.link_key)?;
        Ok(ProfileConnectionTestTarget {
            managed_mcp_id: managed_mcp_id.to_string(),
            extension_config,
        })
    }

    pub async fn managed_extension_names(&self) -> McpPlatformResult<Vec<String>> {
        let mut names = self
            .managed_extension_provenance()
            .await?
            .into_iter()
            .map(|(_, name, _)| name)
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub(crate) async fn managed_extension_provenance(
        &self,
    ) -> McpPlatformResult<Vec<(String, String, ProfileManagedReference)>> {
        self.repository.verify_integrity().await?;
        let inventory = self.complete_managed_inventory().await?;
        let mut provenance = Vec::with_capacity(inventory.len());
        for item in inventory {
            let managed_mcp_id = item.managed.managed_mcp_id;
            let projection = match self
                .repository
                .get_connection_projection(&managed_mcp_id)
                .await
            {
                Ok(projection) => projection,
                Err(error) if error.code() == McpPlatformErrorCode::NotFound => continue,
                Err(error) => return Err(error),
            };
            let active_manifest_digest = item
                .lifecycle
                .active_manifest_digest
                .as_deref()
                .ok_or_else(plan_stale)?;
            if projection.manifest_digest.as_deref() != Some(active_manifest_digest)
                || projection.plan_id.is_none()
                || projection.owner_task_id.is_none()
            {
                return Err(plan_stale());
            }
            let config = crate::mcp_platform::CoreTransportProjectionAdapter
                .extension_config(&projection.projection, &projection.link_key)?;
            let extension_name = projection.link_key;
            provenance.push((
                managed_mcp_id.clone(),
                extension_name.clone(),
                ProfileManagedReference {
                    managed_mcp_id,
                    extension_name,
                    projection_digest: projection.projection_digest,
                    source_fingerprint: extension_source_fingerprint(&config)?,
                },
            ));
        }
        provenance.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(provenance)
    }

    pub async fn profile_list(
        &self,
        _context: &RequestContext,
        include_archived: bool,
    ) -> McpPlatformResult<Vec<McpProfile>> {
        self.repository.list_profiles(include_archived).await
    }

    pub async fn profile_get(
        &self,
        _context: &RequestContext,
        profile_id: &str,
    ) -> McpPlatformResult<McpProfile> {
        validate_profile_id(profile_id)?;
        self.repository.get_profile(profile_id).await
    }

    pub async fn profile_history(
        &self,
        _context: &RequestContext,
        profile_id: &str,
    ) -> McpPlatformResult<Vec<crate::mcp_platform::ProfileRevision>> {
        validate_profile_id(profile_id)?;
        self.repository.list_profile_revisions(profile_id).await
    }

    pub async fn profile_credential_status(&self, profile: &McpProfile) -> CredentialStatus {
        let mut status = CredentialStatus::Ready;
        for entry in &profile.entries {
            let inventory = match self
                .repository
                .get_managed_inventory(&entry.managed_mcp_id)
                .await
            {
                Ok(inventory) => inventory,
                Err(_) => return CredentialStatus::TemporarilyUnavailable,
            };
            let manifest_digest = match inventory.lifecycle.active_manifest_digest.as_deref() {
                Some(manifest_digest) => manifest_digest,
                None => return CredentialStatus::TemporarilyUnavailable,
            };
            let manifest = match self.repository.get_manifest(manifest_digest).await {
                Ok(manifest) => manifest,
                Err(_) => return CredentialStatus::TemporarilyUnavailable,
            };
            let credential_status = self
                .runner
                .managed_enrollment_runtime_snapshot(
                    &manifest.verified.manifest().auth,
                    &inventory.managed.managed_mcp_id,
                    inventory.managed.revision,
                    manifest_digest,
                )
                .await;
            status =
                merge_credential_status(status, into_credential_status(credential_status.status()));
            if status == CredentialStatus::TemporarilyUnavailable {
                break;
            }
        }
        status
    }

    pub async fn profile_create(
        &self,
        context: &RequestContext,
        input: ProfileCreateInput,
    ) -> McpPlatformResult<McpProfile> {
        self.require_mutation_authority()?;
        validate_profile_text(&input.name, &input.description, &input.idempotency_key)?;
        let entries = self
            .validate_profile_entries(&input.managed_mcp_ids)
            .await?;
        let profile_id = self.ids.next_id("profile");
        let request_digest = digest(&(&input.name, &input.description, &entries))?;
        self.repository
            .create_profile(SaveProfile {
                profile_id: &profile_id,
                name: input.name.trim(),
                description: input.description.trim(),
                entries: &entries,
                idempotency_key: &input.idempotency_key,
                request_digest: &request_digest,
                actor: context.actor(),
                now_ms: self.clock.now_ms(),
            })
            .await
    }

    pub async fn profile_update(
        &self,
        context: &RequestContext,
        input: ProfileUpdateInput,
    ) -> McpPlatformResult<McpProfile> {
        self.require_mutation_authority()?;
        validate_profile_id(&input.profile_id)?;
        validate_profile_text(&input.name, &input.description, &input.idempotency_key)?;
        let entries = self
            .validate_profile_entries(&input.managed_mcp_ids)
            .await?;
        let request_digest = digest(&(
            &input.profile_id,
            input.expected_revision,
            &input.name,
            &input.description,
            &entries,
        ))?;
        self.repository
            .change_profile(ChangeProfile {
                profile_id: &input.profile_id,
                expected_revision: input.expected_revision,
                name: input.name.trim(),
                description: input.description.trim(),
                entries: &entries,
                archived: false,
                operation: "update",
                idempotency_key: &input.idempotency_key,
                request_digest: &request_digest,
                actor: context.actor(),
                now_ms: self.clock.now_ms(),
            })
            .await
    }

    pub async fn profile_restore(
        &self,
        context: &RequestContext,
        input: ProfileRestoreInput,
    ) -> McpPlatformResult<McpProfile> {
        self.require_mutation_authority()?;
        validate_profile_id(&input.profile_id)?;
        validate_key(&input.idempotency_key)?;
        let revision = self
            .repository
            .get_profile_revision(&input.profile_id, input.source_revision)
            .await?;
        let ids = revision
            .entries
            .iter()
            .map(|entry| entry.managed_mcp_id.clone())
            .collect::<Vec<_>>();
        let entries = self.validate_profile_entries(&ids).await?;
        let request_digest = digest(&(
            &input.profile_id,
            input.source_revision,
            input.expected_revision,
        ))?;
        self.repository
            .change_profile(ChangeProfile {
                profile_id: &input.profile_id,
                expected_revision: input.expected_revision,
                name: &revision.name,
                description: &revision.description,
                entries: &entries,
                archived: revision.archived,
                operation: "restore",
                idempotency_key: &input.idempotency_key,
                request_digest: &request_digest,
                actor: context.actor(),
                now_ms: self.clock.now_ms(),
            })
            .await
    }

    pub async fn profile_archive(
        &self,
        context: &RequestContext,
        input: ProfileArchiveInput,
    ) -> McpPlatformResult<McpProfile> {
        self.require_mutation_authority()?;
        validate_profile_id(&input.profile_id)?;
        validate_key(&input.idempotency_key)?;
        let current = self.repository.get_profile(&input.profile_id).await?;
        let request_digest = digest(&(&input.profile_id, input.expected_revision))?;
        self.repository
            .change_profile(ChangeProfile {
                profile_id: &input.profile_id,
                expected_revision: input.expected_revision,
                name: &current.name,
                description: &current.description,
                entries: &current.entries,
                archived: true,
                operation: "archive",
                idempotency_key: &input.idempotency_key,
                request_digest: &request_digest,
                actor: context.actor(),
                now_ms: self.clock.now_ms(),
            })
            .await
    }

    pub async fn profile_draft_create(
        &self,
        _context: &RequestContext,
        text: &str,
        locale: &str,
    ) -> McpPlatformResult<ProfileDraft> {
        if text.trim().is_empty() || text.len() > 4_096 || locale.len() > 32 {
            return Err(invalid_request("invalid profile draft input"));
        }
        let terms = normalized_terms(text);
        let inventory = self.complete_managed_inventory().await?;
        let mut candidates = Vec::new();
        let mut matched_terms = BTreeSet::new();
        for item in inventory {
            if !matches!(
                item.managed.state.installation,
                InstallationState::Installed | InstallationState::NotApplicable
            ) {
                continue;
            }
            let manifest =
                self.repository
                    .get_manifest(&item.lifecycle.active_manifest_digest.clone().ok_or_else(
                        || {
                            McpPlatformError::new(
                                McpPlatformErrorCode::IntegrityError,
                                "installed MCP has no active manifest",
                            )
                        },
                    )?)
                    .await?;
            let manifest = manifest.verified.manifest();
            let haystack = normalized_terms(&format!(
                "{} {} {}",
                manifest.id, manifest.name, manifest.description
            ));
            let overlap = terms
                .intersection(&haystack)
                .cloned()
                .collect::<BTreeSet<_>>();
            if overlap.is_empty() {
                continue;
            }
            matched_terms.extend(overlap.iter().cloned());
            let confidence = (overlap.len() as f32 / terms.len().max(1) as f32).min(1.0);
            candidates.push(ProfileDraftCandidate {
                managed_mcp_id: item.managed.managed_mcp_id,
                mcp_id: manifest.id.clone(),
                name: manifest.name.clone(),
                description: manifest.description.clone(),
                confidence,
                reason_code: "stable_metadata_term_match".to_string(),
            });
        }
        candidates.sort_by(|left, right| {
            right
                .confidence
                .total_cmp(&left.confidence)
                .then_with(|| left.managed_mcp_id.cmp(&right.managed_mcp_id))
        });
        let entries = candidates
            .iter()
            .filter(|candidate| candidate.confidence >= 0.2)
            .enumerate()
            .map(|(ordinal, candidate)| ProfileEntry {
                managed_mcp_id: candidate.managed_mcp_id.clone(),
                ordinal: ordinal as i64,
            })
            .collect::<Vec<_>>();
        let unresolved_terms = terms
            .difference(&matched_terms)
            .cloned()
            .collect::<Vec<_>>();
        Ok(ProfileDraft {
            locale: locale.to_string(),
            name: text.trim().chars().take(80).collect(),
            description: text.trim().chars().take(512).collect(),
            low_confidence: entries.is_empty()
                || candidates
                    .first()
                    .is_none_or(|candidate| candidate.confidence < 0.35),
            entries,
            candidates,
            unresolved_terms,
            persisted: false,
        })
    }

    pub async fn profile_apply_plan_create(
        &self,
        context: &RequestContext,
        input: ProfileApplyPlanInput,
    ) -> McpPlatformResult<ProfileApplyPlanView> {
        self.require_mutation_authority()?;
        validate_profile_id(&input.profile_id)?;
        validate_key(&input.idempotency_key)?;
        let profile = self.repository.get_profile(&input.profile_id).await?;
        if profile.archived || profile.revision != input.profile_revision {
            return Err(plan_stale());
        }
        let mut entries = Vec::with_capacity(profile.entries.len());
        let mut entry_views = Vec::with_capacity(profile.entries.len());
        for entry in &profile.entries {
            let (snapshot, view) = self
                .current_profile_snapshot_with_view(&entry.managed_mcp_id)
                .await?;
            entries.push(snapshot);
            entry_views.push(view);
        }
        if entries
            .iter()
            .any(|entry| !profile_snapshot_is_ready(entry))
        {
            return Err(plan_stale());
        }
        let now_ms = self.clock.now_ms();
        let plan_id = self.ids.next_id("profile_plan");
        let mut plan = StoredProfileApplyPlan {
            plan_id,
            profile_id: input.profile_id,
            profile_revision: input.profile_revision,
            internal_plan_digest: String::new(),
            merge_policy: "replace_managed_only".to_string(),
            entries,
            actor: context.actor().to_string(),
            expires_at_ms: now_ms + PROFILE_PLAN_TTL_MS,
            created_at_ms: now_ms,
        };
        plan.internal_plan_digest = profile_apply_plan_snapshot_digest(&plan)?;
        let request_digest = digest(&(&plan.profile_id, plan.profile_revision))?;
        let plan = self
            .repository
            .save_profile_apply_plan(SaveProfileApplyPlan {
                plan: &plan,
                idempotency_key: &input.idempotency_key,
                request_digest: &request_digest,
            })
            .await?;
        let confirmation = self
            .issue_profile_apply_confirmation(&plan, context.actor())
            .await?;
        Ok(ProfileApplyPlanView {
            plan_id: plan.plan_id,
            profile_id: plan.profile_id,
            profile_revision: plan.profile_revision,
            merge_policy: plan.merge_policy,
            entries: entry_views,
            expires_at_ms: plan.expires_at_ms,
            confirmation: ProfileApplyConfirmationView {
                confirmation_token: confirmation.token,
            },
        })
    }

    pub async fn profile_apply_confirm(
        &self,
        context: &RequestContext,
        plan_id: &str,
        confirmation_token: &str,
        confirm: bool,
    ) -> McpPlatformResult<ProfileApplicationTokenView> {
        self.require_mutation_authority()?;
        validate_profile_id(plan_id)?;
        if !confirm {
            return Err(invalid_request("profile application was not confirmed"));
        }
        if confirmation_token.len() > 512
            || !confirmation_token.starts_with("profile_apply_confirmation_")
        {
            return Err(invalid_request("invalid profile apply confirmation token"));
        }
        let confirmation_hash = bytes_to_hex(Sha256::digest(confirmation_token.as_bytes()));
        let now_ms = self.clock.now_ms();
        let plan = self
            .repository
            .get_profile_apply_confirmation_plan(
                &confirmation_hash,
                plan_id,
                context.actor(),
                now_ms,
            )
            .await?;
        self.validate_current_profile_plan(&plan, true).await?;
        let token = self.ids.next_id("profile_application");
        let application_id = self.ids.next_id("profile_application_record");
        let token_hash = bytes_to_hex(Sha256::digest(token.as_bytes()));
        let expires_at_ms = now_ms + PROFILE_TOKEN_TTL_MS;
        self.repository
            .create_profile_application_token(SaveProfileApplicationToken {
                application_id: &application_id,
                confirmation_hash: &confirmation_hash,
                token_hash: &token_hash,
                plan_id,
                internal_plan_digest: plan.internal_plan_digest(),
                actor: context.actor(),
                expires_at_ms,
                created_at_ms: now_ms,
            })
            .await?;
        Ok(ProfileApplicationTokenView {
            token,
            plan_id: plan_id.to_string(),
            profile_id: plan.profile_id,
            profile_revision: plan.profile_revision,
            expires_at_ms,
        })
    }

    pub(crate) async fn consume_profile_application_token(
        &self,
        context: &RequestContext,
        token: &str,
    ) -> McpPlatformResult<ConsumedProfileApplication> {
        self.require_mutation_authority()?;
        if token.len() > 512 || !token.starts_with("profile_application_") {
            return Err(invalid_request("invalid profile application token"));
        }
        let token_hash = bytes_to_hex(Sha256::digest(token.as_bytes()));
        let plan = self
            .repository
            .get_profile_application_plan_by_token(
                &token_hash,
                context.actor(),
                self.clock.now_ms(),
            )
            .await?;
        self.validate_current_profile_plan(&plan, true).await?;
        self.require_profile_runtime_transport(&plan).await?;
        self.repository
            .consume_profile_application_token(&token_hash, context.actor(), self.clock.now_ms())
            .await
    }

    pub(crate) async fn hydrate_profile_application(
        &self,
        context: &RequestContext,
        marker: &ProfileApplicationMarker,
        session_id: &str,
    ) -> McpPlatformResult<ConsumedProfileApplication> {
        if marker.original_session_id != session_id || marker.merge_policy != "replace_managed_only"
        {
            return Err(plan_stale());
        }
        let plan = self
            .repository
            .get_profile_application_plan_by_application(&marker.application_id, context.actor())
            .await?;
        self.validate_current_profile_plan(&plan, false).await?;
        self.require_profile_runtime_transport(&plan).await?;
        let application = self
            .repository
            .hydrate_profile_application(marker, session_id, context.actor())
            .await?;
        if application.managed_extension_names() != marker.managed_extension_names {
            return Err(plan_stale());
        }
        Ok(application)
    }

    pub(crate) async fn verify_profile_application_runtime(
        &self,
        context: &RequestContext,
        application: &ConsumedProfileApplication,
    ) -> McpPlatformResult<()> {
        application.verify_runtime_integrity()?;
        let plan = self
            .repository
            .get_profile_application_plan_by_application(
                &application.application_id,
                context.actor(),
            )
            .await?;
        if plan.profile_id != application.profile_id
            || plan.profile_revision != application.profile_revision
            || plan.internal_plan_digest() != application.plan_digest
        {
            return Err(plan_stale());
        }
        self.validate_current_profile_plan(&plan, false).await?;
        self.require_profile_runtime_transport(&plan).await?;
        self.verify_profile_application_runtime_gate(&plan, &application.application_id)
            .await?;
        let provenance = self.managed_extension_provenance().await?;
        let mut expected_names = provenance
            .iter()
            .map(|(_, name, _)| name.clone())
            .collect::<Vec<_>>();
        expected_names.sort();
        let expected_references = plan
            .entries
            .iter()
            .map(|entry| {
                provenance
                    .iter()
                    .find(|(managed_mcp_id, _, _)| managed_mcp_id == &entry.managed_mcp_id)
                    .map(|(_, _, reference)| reference.clone())
                    .ok_or_else(plan_stale)
            })
            .collect::<McpPlatformResult<Vec<_>>>()?;
        let extensions_match = application.extensions().len() == expected_references.len()
            && application
                .extensions()
                .iter()
                .zip(&expected_references)
                .all(|(extension, reference)| {
                    extension.name() == reference.extension_name
                        && extension_source_fingerprint(extension)
                            .is_ok_and(|digest| digest == reference.source_fingerprint)
                });
        if application.managed_extension_names() != expected_names
            || application.managed_references() != expected_references
            || !extensions_match
        {
            return Err(plan_stale());
        }
        Ok(())
    }

    pub(crate) async fn finish_profile_application(
        &self,
        application_id: &str,
        session_id: Option<&str>,
        status: &str,
        detail_code: &str,
    ) -> McpPlatformResult<()> {
        self.require_mutation_authority()?;
        self.repository
            .finish_profile_application(
                application_id,
                session_id,
                status,
                detail_code,
                "local_authenticated_client",
                self.clock.now_ms(),
            )
            .await
    }

    pub(crate) async fn profile_application_cleanup_state(
        &self,
        session_id: &str,
    ) -> McpPlatformResult<Option<(String, String)>> {
        self.repository
            .get_profile_application_cleanup_state(session_id, "local_authenticated_client")
            .await
    }

    async fn validate_profile_entries(
        &self,
        managed_mcp_ids: &[String],
    ) -> McpPlatformResult<Vec<ProfileEntry>> {
        if managed_mcp_ids.len() > 128 {
            return Err(invalid_request("too many profile entries"));
        }
        let mut seen = HashSet::new();
        let mut entries = Vec::with_capacity(managed_mcp_ids.len());
        for (ordinal, managed_mcp_id) in managed_mcp_ids.iter().enumerate() {
            validate_profile_id(managed_mcp_id)?;
            if !seen.insert(managed_mcp_id) {
                return Err(invalid_request("duplicate managed MCP profile entry"));
            }
            let managed = self
                .repository
                .get_managed_inventory(managed_mcp_id)
                .await?;
            if !matches!(
                managed.managed.state.installation,
                InstallationState::Installed | InstallationState::NotApplicable
            ) {
                return Err(invalid_request(
                    "profile entries must reference installed MCPs",
                ));
            }
            let projection = self
                .repository
                .get_connection_projection(managed_mcp_id)
                .await?;
            if managed.lifecycle.active_manifest_digest.as_deref()
                != projection.manifest_digest.as_deref()
                || projection.plan_id.is_none()
                || projection.owner_task_id.is_none()
            {
                return Err(plan_stale());
            }
            entries.push(ProfileEntry {
                managed_mcp_id: managed_mcp_id.clone(),
                ordinal: ordinal as i64,
            });
        }
        Ok(entries)
    }

    async fn complete_managed_inventory(
        &self,
    ) -> McpPlatformResult<Vec<ManagedMcpInventoryRecord>> {
        let mut inventory = Vec::new();
        let mut after_managed_mcp_id = None;
        let mut seen_cursors = HashSet::new();
        loop {
            let remaining = COMPLETE_INVENTORY_MAX_ITEMS.saturating_sub(inventory.len());
            let limit = COMPLETE_INVENTORY_PAGE_SIZE.min(remaining.saturating_add(1));
            let page = self
                .repository
                .list_managed_inventory(
                    after_managed_mcp_id.as_deref(),
                    limit,
                    &ManagedInventoryFilter::default(),
                )
                .await?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            let next_cursor = validate_complete_inventory_page(
                after_managed_mcp_id.as_deref(),
                page.iter().map(|item| item.managed.managed_mcp_id.as_str()),
                page_len,
                limit,
                inventory.len(),
            )?
            .ok_or_else(incomplete_inventory)?;
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(incomplete_inventory());
            }
            inventory.extend(page);
            after_managed_mcp_id = Some(next_cursor);
            if page_len < limit {
                break;
            }
        }
        Ok(inventory)
    }

    async fn current_profile_snapshot_with_view(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<(StoredProfileManagedSnapshot, ProfileApplyEntryView)> {
        let source = self
            .repository
            .get_profile_managed_snapshot_source(managed_mcp_id)
            .await?;
        let snapshot = self
            .profile_snapshot_from_source(managed_mcp_id, source.clone())
            .await?;
        let manifest = source.manifest.verified.manifest();
        Ok((
            snapshot.clone(),
            ProfileApplyEntryView {
                managed_mcp_id: managed_mcp_id.to_string(),
                mcp_id: Some(manifest.id.clone()),
                name: Some(manifest.name.clone()),
                version: Some(manifest.version.as_str().to_string()),
                health: snapshot.health.clone(),
                auth_ready: snapshot.auth_ready,
                policy_ready: snapshot.policy_ready,
                readiness: profile_snapshot_readiness(&snapshot).to_string(),
            },
        ))
    }

    async fn current_profile_snapshot(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<StoredProfileManagedSnapshot> {
        let source = self
            .repository
            .get_profile_managed_snapshot_source(managed_mcp_id)
            .await?;
        self.profile_snapshot_from_source(managed_mcp_id, source)
            .await
    }

    async fn profile_snapshot_from_source(
        &self,
        managed_mcp_id: &str,
        source: crate::mcp_platform::repository::ProfileManagedSnapshotSource,
    ) -> McpPlatformResult<StoredProfileManagedSnapshot> {
        if source.manifest_digest.len() != 64 || source.projection_digest.len() != 64 {
            return Err(plan_stale());
        }
        if source.manifest.verified.digest() != source.manifest_digest.as_str() {
            return Err(plan_stale());
        }
        let auth = self
            .runner
            .managed_enrollment_runtime_snapshot(
                &source.manifest.verified.manifest().auth,
                managed_mcp_id,
                source.managed_revision,
                &source.manifest_digest,
            )
            .await;
        let state = source.state;
        let policy_ready = state.registration == RegistrationState::Registered
            && matches!(
                state.installation,
                InstallationState::Installed | InstallationState::NotApplicable
            );
        let auth_evidence_digest = profile_auth_evidence_digest(
            auth.evidence_digest(),
            source.managed_revision,
            &source.manifest_digest,
            source.projection_revision,
            &source.projection_digest,
        )?;
        let policy_evidence_digest = profile_policy_evidence_digest(
            &state,
            source.managed_revision,
            &source.manifest_digest,
            source.projection_revision,
            &source.projection_digest,
        )?;
        Ok(StoredProfileManagedSnapshot {
            managed_mcp_id: managed_mcp_id.to_string(),
            managed_revision: source.managed_revision,
            manifest_digest: source.manifest_digest,
            projection_revision: source.projection_revision,
            projection_digest: source.projection_digest,
            health: health_code(state.health).to_string(),
            auth_ready: auth.is_ready(),
            policy_ready,
            auth_evidence_digest,
            policy_evidence_digest,
        })
    }

    async fn issue_profile_apply_confirmation(
        &self,
        plan: &StoredProfileApplyPlan,
        actor: &str,
    ) -> McpPlatformResult<StoredProfileApplyConfirmation> {
        let token = self.ids.next_id("profile_apply_confirmation");
        let confirmation_hash = bytes_to_hex(Sha256::digest(token.as_bytes()));
        self.repository
            .create_profile_apply_confirmation(SaveProfileApplyConfirmation {
                confirmation_hash: &confirmation_hash,
                plan_id: &plan.plan_id,
                actor,
                expires_at_ms: plan.expires_at_ms,
                created_at_ms: self.clock.now_ms(),
            })
            .await?;
        Ok(StoredProfileApplyConfirmation {
            token,
            plan_id: plan.plan_id.clone(),
            actor: actor.to_string(),
            expires_at_ms: plan.expires_at_ms,
        })
    }

    async fn validate_current_profile_plan(
        &self,
        plan: &StoredProfileApplyPlan,
        enforce_expiry: bool,
    ) -> McpPlatformResult<()> {
        if enforce_expiry && plan.expires_at_ms <= self.clock.now_ms() {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::PlanExpired,
                "MCP profile application plan expired",
            ));
        }
        let plan_entries = plan
            .entries
            .iter()
            .enumerate()
            .map(|(ordinal, entry)| ProfileEntry {
                managed_mcp_id: entry.managed_mcp_id.clone(),
                ordinal: ordinal as i64,
            })
            .collect::<Vec<_>>();
        for expected in &plan.entries {
            if self
                .current_profile_snapshot(&expected.managed_mcp_id)
                .await?
                != *expected
            {
                return Err(plan_stale());
            }
        }
        Ok(())
    }

    async fn verify_profile_application_runtime_gate(
        &self,
        plan: &StoredProfileApplyPlan,
        _application_id: &str,
    ) -> McpPlatformResult<()> {
        for expected in &plan.entries {
            let source = self
                .repository
                .get_profile_managed_snapshot_source(&expected.managed_mcp_id)
                .await?;
            let auth = self
                .runner
                .managed_enrollment_runtime_snapshot(
                    &source.manifest.verified.manifest().auth,
                    &expected.managed_mcp_id,
                    source.managed_revision,
                    &source.manifest_digest,
                )
                .await;
            if !auth.is_ready() {
                return Err(auth.status().readiness_error());
            }
        }
        Ok(())
    }

    async fn require_profile_runtime_transport(
        &self,
        plan: &StoredProfileApplyPlan,
    ) -> McpPlatformResult<()> {
        for expected in &plan.entries {
            self.repository
                .get_connection_projection(&expected.managed_mcp_id)
                .await?
                .projection
                .require_runtime_transport()?;
        }
        Ok(())
    }
}

fn health_code(state: HealthState) -> &'static str {
    match state {
        HealthState::Unknown => "unknown",
        HealthState::Checking => "checking",
        HealthState::Healthy => "healthy",
        HealthState::Degraded => "degraded",
        HealthState::Unhealthy => "unhealthy",
        HealthState::BlockedAuth => "blocked_auth",
        HealthState::Incompatible => "incompatible",
    }
}

fn profile_snapshot_is_ready(snapshot: &StoredProfileManagedSnapshot) -> bool {
    snapshot.health == "healthy" && snapshot.auth_ready && snapshot.policy_ready
}

fn profile_snapshot_readiness(snapshot: &StoredProfileManagedSnapshot) -> &'static str {
    if snapshot.health != "healthy" {
        "blocked_health"
    } else if !snapshot.auth_ready {
        "blocked_auth"
    } else if !snapshot.policy_ready {
        "blocked_policy"
    } else {
        "ready"
    }
}

fn normalized_terms(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|term| term.chars().count() >= 2)
        .collect()
}

fn validate_profile_text(name: &str, description: &str, key: &str) -> McpPlatformResult<()> {
    if name.trim().is_empty() || name.len() > 128 || description.len() > 2_048 {
        return Err(invalid_request("invalid profile text"));
    }
    validate_key(key)
}

fn validate_key(value: &str) -> McpPlatformResult<()> {
    if value.trim().is_empty() || value.len() > 256 {
        return Err(invalid_request("invalid idempotency key"));
    }
    Ok(())
}

fn validate_profile_id(value: &str) -> McpPlatformResult<()> {
    if value.trim().is_empty() || value.len() > 256 || value.contains('\0') {
        return Err(invalid_request("invalid MCP profile identifier"));
    }
    Ok(())
}

fn digest(value: &impl serde::Serialize) -> McpPlatformResult<String> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        McpPlatformError::new(
            McpPlatformErrorCode::SerializationFailed,
            "failed to serialize profile data",
        )
    })?;
    Ok(bytes_to_hex(Sha256::digest(bytes)))
}

fn merge_credential_status(current: CredentialStatus, next: CredentialStatus) -> CredentialStatus {
    use CredentialStatus::{
        ReRegistrationRequired, Ready, TemporarilyUnavailable, TrustedStateConflict, Unconfigured,
    };

    match (current, next) {
        (TemporarilyUnavailable, _) | (_, TemporarilyUnavailable) => TemporarilyUnavailable,
        (TrustedStateConflict, _) | (_, TrustedStateConflict) => TrustedStateConflict,
        (ReRegistrationRequired, _) | (_, ReRegistrationRequired) => ReRegistrationRequired,
        (Unconfigured, _) | (_, Unconfigured) => Unconfigured,
        _ => Ready,
    }
}

fn into_credential_status(status: CredentialRuntimeStatus) -> CredentialStatus {
    match status {
        CredentialRuntimeStatus::Ready => CredentialStatus::Ready,
        CredentialRuntimeStatus::Unconfigured => CredentialStatus::Unconfigured,
        CredentialRuntimeStatus::ReRegistrationRequired => CredentialStatus::ReRegistrationRequired,
        CredentialRuntimeStatus::TrustedStateConflict => CredentialStatus::TrustedStateConflict,
        CredentialRuntimeStatus::TemporarilyUnavailable => CredentialStatus::TemporarilyUnavailable,
    }
}

fn invalid_request(message: &'static str) -> McpPlatformError {
    McpPlatformError::new(McpPlatformErrorCode::InvalidRequest, message)
}

fn plan_stale() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PlanStale,
        "MCP profile application plan is stale",
    )
}

fn incomplete_inventory() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "managed MCP inventory could not be enumerated completely",
    )
}

fn validate_complete_inventory_page<'a>(
    after_managed_mcp_id: Option<&str>,
    managed_mcp_ids: impl IntoIterator<Item = &'a str>,
    page_len: usize,
    limit: usize,
    accumulated_len: usize,
) -> McpPlatformResult<Option<String>> {
    if page_len > limit || accumulated_len.saturating_add(page_len) > COMPLETE_INVENTORY_MAX_ITEMS {
        return Err(incomplete_inventory());
    }
    let mut previous = after_managed_mcp_id;
    let mut last = None;
    let mut counted = 0;
    for managed_mcp_id in managed_mcp_ids {
        if previous.is_some_and(|cursor| managed_mcp_id <= cursor) {
            return Err(incomplete_inventory());
        }
        previous = Some(managed_mcp_id);
        last = Some(managed_mcp_id.to_string());
        counted += 1;
    }
    if counted != page_len {
        return Err(incomplete_inventory());
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::mcp_platform::repository::{InMemoryIntegritySigner, SqliteMcpPlatformRepository};
    use crate::mcp_platform::{ConnectionProjection, McpPlatformErrorCode};

    use super::*;
    use crate::mcp_platform::service::{McpPlatformServiceOptions, SystemClock, UuidGenerator};

    #[tokio::test]
    async fn natural_language_draft_is_local_deterministic_and_not_persisted() {
        let repository = SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            InMemoryIntegritySigner::new_for_testing([0x35; 32]),
        )
        .await
        .unwrap();
        let service = McpPlatformService::new_trusted(
            Arc::new(repository.clone()),
            Arc::new(SystemClock),
            Arc::new(UuidGenerator),
            McpPlatformServiceOptions::default(),
        );
        let context = service.trusted_local_context();
        let first = service
            .profile_draft_create(&context, "coding github tools", "en-US")
            .await
            .unwrap();
        let second = service
            .profile_draft_create(&context, "coding github tools", "en-US")
            .await
            .unwrap();
        assert_eq!(first, second);
        assert!(!first.persisted);
        assert!(repository.list_profiles(true).await.unwrap().is_empty());
        assert!(repository
            .list_managed_inventory(None, 1, &ManagedInventoryFilter::default())
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn connection_test_target_rejects_read_only_service_before_runtime_access() {
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_url_with_integrity_signer(
                "sqlite::memory:",
                InMemoryIntegritySigner::new_for_testing([0x36; 32]),
            )
            .await
            .unwrap(),
        );
        let service = McpPlatformService::new(
            repository,
            Arc::new(SystemClock),
            Arc::new(UuidGenerator),
            McpPlatformServiceOptions::default(),
        );
        let error = service
            .profile_connection_test_target("managed_not_contacted")
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::PolicyDenied);
    }

    #[test]
    fn complete_inventory_page_rejects_repeated_or_nonadvancing_cursors() {
        for ids in [vec!["managed-b"], vec!["managed-a", "managed-c"]] {
            let error = validate_complete_inventory_page(
                Some("managed-b"),
                ids.iter().copied(),
                ids.len(),
                COMPLETE_INVENTORY_PAGE_SIZE,
                10,
            )
            .unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
            assert!(!error.message().contains("managed-b"));
        }
    }

    #[test]
    fn complete_inventory_page_rejects_resource_limit_without_partial_success() {
        let error = validate_complete_inventory_page(
            Some("managed-y"),
            ["managed-z"],
            1,
            1,
            COMPLETE_INVENTORY_MAX_ITEMS,
        )
        .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    }

    #[test]
    fn profile_credential_status_aggregation_preserves_more_specific_non_ready_states() {
        assert_eq!(
            merge_credential_status(CredentialStatus::Ready, CredentialStatus::Unconfigured),
            CredentialStatus::Unconfigured
        );
        assert_eq!(
            merge_credential_status(
                CredentialStatus::Unconfigured,
                CredentialStatus::ReRegistrationRequired,
            ),
            CredentialStatus::ReRegistrationRequired
        );
        assert_eq!(
            merge_credential_status(
                CredentialStatus::ReRegistrationRequired,
                CredentialStatus::TrustedStateConflict,
            ),
            CredentialStatus::TrustedStateConflict
        );
        assert_eq!(
            merge_credential_status(
                CredentialStatus::TrustedStateConflict,
                CredentialStatus::TemporarilyUnavailable,
            ),
            CredentialStatus::TemporarilyUnavailable
        );
    }

    #[test]
    fn connection_test_activation_is_limited_to_managed_runtime_projections() {
        let remote = ConnectionProjection::RemoteHttp {
            name: "remote".to_string(),
            description: "remote".to_string(),
            uri: "https://example.test/mcp".to_string(),
            timeout_seconds: None,
        };
        let manual = ConnectionProjection::ManualStdio {
            name: "manual".to_string(),
            description: "manual".to_string(),
            executable: "manual".to_string(),
            args: Vec::new(),
            environment_keys: Vec::new(),
            cwd: None,
            timeout_seconds: None,
        };
        let managed = ConnectionProjection::ManagedStdio {
            name: "managed".to_string(),
            description: "managed".to_string(),
            executable: "managed".to_string(),
            args: Vec::new(),
            environment_keys: Vec::new(),
            cwd: None,
            timeout_seconds: None,
        };
        let docker = ConnectionProjection::ManagedDockerStdio {
            name: "docker".to_string(),
            description: "docker".to_string(),
            executable: "docker".to_string(),
            args: Vec::new(),
            cwd: None,
            timeout_seconds: None,
        };

        assert!(!connection_test_projection_requires_activation(&remote));
        assert!(!connection_test_projection_requires_activation(&manual));
        assert!(connection_test_projection_requires_activation(&managed));
        assert!(connection_test_projection_requires_activation(&docker));
    }
}

#[cfg(test)]
fn connection_test_projection_requires_activation(
    projection: &crate::mcp_platform::ConnectionProjection,
) -> bool {
    match projection {
        crate::mcp_platform::ConnectionProjection::RemoteHttp { .. }
        | crate::mcp_platform::ConnectionProjection::ManualStdio { .. } => false,
        crate::mcp_platform::ConnectionProjection::ManagedStdio { .. }
        | crate::mcp_platform::ConnectionProjection::ManagedDockerStdio { .. } => true,
    }
}
