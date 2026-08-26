use sqlx::{Row, Sqlite, Transaction};

use crate::mcp_platform::domain::{HealthState, InstallationState, RegistrationState};
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::lifecycle::{CoreTransportProjectionAdapter, TransportProjectionAdapter};
use crate::mcp_platform::{
    extension_source_fingerprint, profile_apply_plan_snapshot_digest,
    profile_policy_evidence_digest, ChangeProfile, ConsumedProfileApplication, McpProfile,
    ProfileApplicationMarker, ProfileCredentialReference, ProfileEntry, ProfileManagedReference,
    ProfileRevision, SaveProfile, SaveProfileApplicationToken, SaveProfileApplyConfirmation,
    SaveProfileApplyPlan, StoredProfileApplyPlan, StoredProfileManagedSnapshot,
};

use super::{decode, encode, map_sqlx, SqliteMcpPlatformRepository};

impl SqliteMcpPlatformRepository {
    pub async fn get_profile_managed_snapshot_source(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<crate::mcp_platform::repository::ProfileManagedSnapshotSource> {
        let mut tx = self.begin_immediate().await?;
        let managed = sqlx::query(
            "SELECT state_json,revision,active_manifest_digest,active_version FROM managed_mcps WHERE managed_mcp_id=?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(super::records::not_found)?;
        let state = decode(
            &managed
                .try_get::<String, _>("state_json")
                .map_err(map_sqlx)?,
        )?;
        let managed_revision = managed.try_get("revision").map_err(map_sqlx)?;
        let manifest_digest = managed
            .try_get::<Option<String>, _>("active_manifest_digest")
            .map_err(map_sqlx)?
            .ok_or_else(super::records::integrity_error)?;
        let active_version = managed
            .try_get::<Option<String>, _>("active_version")
            .map_err(map_sqlx)?
            .ok_or_else(super::records::integrity_error)?;
        let active_digest = sqlx::query_scalar::<_, String>(
            "SELECT manifest_digest FROM managed_versions WHERE managed_mcp_id=? AND version=? AND active=1",
        )
        .bind(managed_mcp_id)
        .bind(&active_version)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(super::records::integrity_error)?;
        if active_digest != manifest_digest {
            return Err(super::records::integrity_error());
        }
        let projection = sqlx::query(
            "SELECT link_key,projection_json,revision,plan_id,manifest_digest,owner_task_id,projection_digest FROM connection_projections WHERE managed_mcp_id=?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(super::records::not_found)?;
        let projection_digest: String =
            projection.try_get("projection_digest").map_err(map_sqlx)?;
        crate::mcp_platform::decode_verified_projection_config(
            &projection
                .try_get::<String, _>("projection_json")
                .map_err(map_sqlx)?,
            &projection_digest,
        )?;
        let projection_revision = projection.try_get("revision").map_err(map_sqlx)?;
        let plan_id = projection
            .try_get::<Option<String>, _>("plan_id")
            .map_err(map_sqlx)?
            .ok_or_else(super::records::integrity_error)?;
        let projection_manifest_digest = projection
            .try_get::<Option<String>, _>("manifest_digest")
            .map_err(map_sqlx)?
            .ok_or_else(super::records::integrity_error)?;
        let owner_task_id = projection
            .try_get::<Option<String>, _>("owner_task_id")
            .map_err(map_sqlx)?
            .ok_or_else(super::records::integrity_error)?;
        let link_key: String = projection.try_get("link_key").map_err(map_sqlx)?;
        if projection_manifest_digest != manifest_digest {
            return Err(super::records::integrity_error());
        }
        let projection_mac = sqlx::query_scalar::<_, String>(
            "SELECT mac FROM managed_projection_integrity WHERE managed_mcp_id=?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(super::records::integrity_error)?;
        self.integrity_signer.verify(
            "managed-projection",
            &super::records::managed_projection_integrity_payload(
                managed_mcp_id,
                &link_key,
                &projection_digest,
                projection_revision,
                Some(&plan_id),
                Some(&projection_manifest_digest),
                Some(&owner_task_id),
            ),
            &projection_mac,
        )?;
        super::lifecycle_repository::verify_projection_writer_binding(
            &mut tx,
            self.integrity_signer.as_ref(),
            managed_mcp_id,
            &owner_task_id,
            &plan_id,
            &link_key,
            &projection_manifest_digest,
            &projection_digest,
        )
        .await?;
        let manifest_row = sqlx::query(
            "SELECT manifest_digest,mcp_id,version,canonical_bytes,proof_json,trust_tier_json,source_ref_json,import_kind,release_id,origin_provenance_json,update_channel_json,created_at_ms FROM manifest_blobs WHERE manifest_digest=?",
        )
        .bind(&manifest_digest)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(super::records::not_found)?;
        let manifest = super::records::decode_manifest_row(&manifest_row)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(
            crate::mcp_platform::repository::ProfileManagedSnapshotSource {
                managed_mcp_id: managed_mcp_id.to_string(),
                state,
                managed_revision,
                manifest,
                manifest_digest,
                projection_revision,
                projection_digest,
            },
        )
    }

    pub async fn get_profile_application_plan_by_token(
        &self,
        token_hash: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        self.verify_integrity().await?;
        let row = sqlx::query(
            "SELECT plan_id,actor,expires_at_ms,consumed_at_ms FROM mcp_profile_application_tokens WHERE token_hash=?",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        if row.try_get::<String, _>("actor").map_err(map_sqlx)? != actor
            || row
                .try_get::<Option<i64>, _>("consumed_at_ms")
                .map_err(map_sqlx)?
                .is_some()
        {
            return Err(plan_stale());
        }
        if row.try_get::<i64, _>("expires_at_ms").map_err(map_sqlx)? <= now_ms {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::PlanExpired,
                "profile application token expired",
            ));
        }
        self.get_profile_apply_plan(&row.try_get::<String, _>("plan_id").map_err(map_sqlx)?)
            .await
    }

    pub async fn get_profile_apply_confirmation_plan(
        &self,
        confirmation_hash: &str,
        plan_id: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        self.verify_integrity().await?;
        let row = sqlx::query(
            "SELECT plan_id,actor,expires_at_ms,consumed_at_ms FROM mcp_profile_apply_confirmations WHERE confirmation_hash=?",
        )
        .bind(confirmation_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(plan_stale)?;
        if row.try_get::<String, _>("plan_id").map_err(map_sqlx)? != plan_id
            || row.try_get::<String, _>("actor").map_err(map_sqlx)? != actor
            || row
                .try_get::<Option<i64>, _>("consumed_at_ms")
                .map_err(map_sqlx)?
                .is_some()
        {
            return Err(plan_stale());
        }
        if row.try_get::<i64, _>("expires_at_ms").map_err(map_sqlx)? <= now_ms {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::PlanExpired,
                "profile apply confirmation expired",
            ));
        }
        self.get_profile_apply_plan(plan_id).await
    }

    pub async fn get_profile_application_plan_by_application(
        &self,
        application_id: &str,
        actor: &str,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        self.verify_integrity().await?;
        let row = sqlx::query(
            "SELECT plan_id,actor,status FROM mcp_profile_applications WHERE application_id=?",
        )
        .bind(application_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let status: String = row.try_get("status").map_err(map_sqlx)?;
        if row.try_get::<String, _>("actor").map_err(map_sqlx)? != actor
            || !matches!(status.as_str(), "consumed" | "created")
        {
            return Err(plan_stale());
        }
        self.get_profile_apply_plan(&row.try_get::<String, _>("plan_id").map_err(map_sqlx)?)
            .await
    }

    pub async fn get_profile_application_cleanup_state(
        &self,
        session_id: &str,
        actor: &str,
    ) -> McpPlatformResult<Option<(String, String)>> {
        self.verify_integrity().await?;
        let row = sqlx::query(
            "SELECT application_id,status FROM mcp_profile_applications WHERE session_id=? AND actor=? AND status IN ('created','recovery_required','deleted') ORDER BY updated_at_ms DESC LIMIT 1",
        )
        .bind(session_id)
        .bind(actor)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        row.map(|row| {
            Ok((
                row.try_get("application_id").map_err(map_sqlx)?,
                row.try_get("status").map_err(map_sqlx)?,
            ))
        })
        .transpose()
    }

    pub(crate) async fn create_profile(
        &self,
        input: SaveProfile<'_>,
    ) -> McpPlatformResult<McpProfile> {
        if let Some(profile) = self
            .profile_idempotency("create", input.idempotency_key, input.request_digest)
            .await?
        {
            return Ok(profile);
        }
        let mut tx = self.begin_immediate().await?;
        sqlx::query(
            "INSERT INTO mcp_profiles(profile_id,name,description,revision,archived,credential_references_json,created_at_ms,updated_at_ms) VALUES (?,?,?,1,0,?,?,?)",
        )
        .bind(input.profile_id)
        .bind(input.name)
        .bind(input.description)
        .bind(encode_empty_profile_credential_references()?)
        .bind(input.now_ms)
        .bind(input.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        replace_entries(&mut tx, input.profile_id, input.entries).await?;
        let profile = McpProfile {
            profile_id: input.profile_id.to_string(),
            name: input.name.to_string(),
            description: input.description.to_string(),
            revision: 1,
            archived: false,
            entries: input.entries.to_vec(),
            created_at_ms: input.now_ms,
            updated_at_ms: input.now_ms,
        };
        insert_revision(&mut tx, &profile, input.actor, "create", input.now_ms).await?;
        insert_idempotency(
            &mut tx,
            "create",
            input.idempotency_key,
            input.request_digest,
            input.profile_id,
            1,
            input.now_ms,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(profile)
    }

    pub(crate) async fn change_profile(
        &self,
        input: ChangeProfile<'_>,
    ) -> McpPlatformResult<McpProfile> {
        if let Some(profile) = self
            .profile_idempotency(input.operation, input.idempotency_key, input.request_digest)
            .await?
        {
            return Ok(profile);
        }
        let mut tx = self.begin_immediate().await?;
        let row =
            sqlx::query("SELECT created_at_ms FROM mcp_profiles WHERE profile_id=? AND revision=?")
                .bind(input.profile_id)
                .bind(input.expected_revision)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        let Some(row) = row else {
            return Err(revision_or_missing(&mut tx, input.profile_id).await?);
        };
        let next_revision = input.expected_revision + 1;
        sqlx::query(
            "UPDATE mcp_profiles SET name=?,description=?,revision=?,archived=?,credential_references_json=?,updated_at_ms=? WHERE profile_id=? AND revision=?",
        )
        .bind(input.name)
        .bind(input.description)
        .bind(next_revision)
        .bind(input.archived)
        .bind(encode_empty_profile_credential_references()?)
        .bind(input.now_ms)
        .bind(input.profile_id)
        .bind(input.expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        replace_entries(&mut tx, input.profile_id, input.entries).await?;
        let profile = McpProfile {
            profile_id: input.profile_id.to_string(),
            name: input.name.to_string(),
            description: input.description.to_string(),
            revision: next_revision,
            archived: input.archived,
            entries: input.entries.to_vec(),
            created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
            updated_at_ms: input.now_ms,
        };
        insert_revision(
            &mut tx,
            &profile,
            input.actor,
            input.operation,
            input.now_ms,
        )
        .await?;
        insert_idempotency(
            &mut tx,
            input.operation,
            input.idempotency_key,
            input.request_digest,
            input.profile_id,
            next_revision,
            input.now_ms,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(profile)
    }

    pub async fn list_profiles(
        &self,
        include_archived: bool,
    ) -> McpPlatformResult<Vec<McpProfile>> {
        self.verify_integrity().await?;
        let rows = sqlx::query(
            "SELECT profile_id,name,description,revision,archived,created_at_ms,updated_at_ms FROM mcp_profiles WHERE archived=0 OR ? ORDER BY updated_at_ms DESC,profile_id",
        )
        .bind(include_archived)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        let mut profiles = Vec::with_capacity(rows.len());
        for row in rows {
            profiles.push(self.decode_profile_row(row).await?);
        }
        Ok(profiles)
    }

    pub async fn get_profile(&self, profile_id: &str) -> McpPlatformResult<McpProfile> {
        self.verify_integrity().await?;
        let row = sqlx::query(
            "SELECT profile_id,name,description,revision,archived,created_at_ms,updated_at_ms FROM mcp_profiles WHERE profile_id=?",
        )
        .bind(profile_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        self.decode_profile_row(row).await
    }

    pub async fn get_profile_revision(
        &self,
        profile_id: &str,
        revision: i64,
    ) -> McpPlatformResult<ProfileRevision> {
        self.verify_integrity().await?;
        let row = sqlx::query(
            "SELECT snapshot_json,actor,operation,created_at_ms FROM mcp_profile_revisions WHERE profile_id=? AND revision=?",
        )
        .bind(profile_id)
        .bind(revision)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let profile: McpProfile = decode(
            &row.try_get::<String, _>("snapshot_json")
                .map_err(map_sqlx)?,
        )?;
        Ok(ProfileRevision {
            profile_id: profile.profile_id,
            revision: profile.revision,
            name: profile.name,
            description: profile.description,
            archived: profile.archived,
            entries: profile.entries,
            actor: row.try_get("actor").map_err(map_sqlx)?,
            operation: row.try_get("operation").map_err(map_sqlx)?,
            created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
        })
    }

    pub(crate) async fn get_profile_credential_references(
        &self,
        profile_id: &str,
    ) -> McpPlatformResult<Vec<ProfileCredentialReference>> {
        self.verify_integrity().await?;
        sqlx::query_scalar::<_, String>("SELECT profile_id FROM mcp_profiles WHERE profile_id=?")
            .bind(profile_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        Ok(Vec::new())
    }

    pub(crate) async fn get_profile_revision_credential_references(
        &self,
        profile_id: &str,
        revision: i64,
    ) -> McpPlatformResult<Vec<ProfileCredentialReference>> {
        self.verify_integrity().await?;
        sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM mcp_profile_revisions WHERE profile_id=? AND revision=?",
        )
        .bind(profile_id)
        .bind(revision)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        Ok(Vec::new())
    }

    pub async fn list_profile_revisions(
        &self,
        profile_id: &str,
    ) -> McpPlatformResult<Vec<ProfileRevision>> {
        self.verify_integrity().await?;
        let rows = sqlx::query(
            "SELECT snapshot_json,actor,operation,created_at_ms FROM mcp_profile_revisions WHERE profile_id=? ORDER BY revision DESC",
        )
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        if rows.is_empty() {
            self.get_profile(profile_id).await?;
        }
        rows.into_iter()
            .map(|row| {
                let profile: McpProfile = decode(
                    &row.try_get::<String, _>("snapshot_json")
                        .map_err(map_sqlx)?,
                )?;
                Ok(ProfileRevision {
                    profile_id: profile.profile_id,
                    revision: profile.revision,
                    name: profile.name,
                    description: profile.description,
                    archived: profile.archived,
                    entries: profile.entries,
                    actor: row.try_get("actor").map_err(map_sqlx)?,
                    operation: row.try_get("operation").map_err(map_sqlx)?,
                    created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
                })
            })
            .collect()
    }

    pub(crate) async fn save_profile_apply_plan(
        &self,
        input: SaveProfileApplyPlan<'_>,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        verify_profile_apply_plan_digest(input.plan, input.plan.internal_plan_digest())?;
        let mut tx = self.begin_immediate().await?;
        if let Some(row) = sqlx::query(
            "SELECT request_digest,plan_digest,snapshot_json FROM mcp_profile_apply_plans WHERE idempotency_key=?",
        )
        .bind(input.idempotency_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        {
            let digest: String = row.try_get("request_digest").map_err(map_sqlx)?;
            if digest != input.request_digest {
                return Err(idempotency_conflict());
            }
            let plan = decode_verified_profile_apply_plan(
                &row.try_get::<String, _>("snapshot_json").map_err(map_sqlx)?,
                &row.try_get::<String, _>("plan_digest").map_err(map_sqlx)?,
            )?;
            tx.commit().await?;
            return Ok(plan);
        }
        let profile = sqlx::query("SELECT revision,archived FROM mcp_profiles WHERE profile_id=?")
            .bind(&input.plan.profile_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        let profile_entries = sqlx::query_scalar::<_, String>(
            "SELECT managed_mcp_id FROM mcp_profile_entries WHERE profile_id=? ORDER BY ordinal",
        )
        .bind(&input.plan.profile_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if profile.try_get::<i64, _>("revision").map_err(map_sqlx)? != input.plan.profile_revision
            || profile.try_get::<bool, _>("archived").map_err(map_sqlx)?
            || profile_entries
                != input
                    .plan
                    .entries
                    .iter()
                    .map(|entry| entry.managed_mcp_id.clone())
                    .collect::<Vec<_>>()
        {
            return Err(plan_stale());
        }
        sqlx::query(
            "INSERT INTO mcp_profile_apply_plans(plan_id,profile_id,profile_revision,plan_digest,snapshot_json,actor,expires_at_ms,idempotency_key,request_digest,created_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&input.plan.plan_id)
        .bind(&input.plan.profile_id)
        .bind(input.plan.profile_revision)
        .bind(input.plan.internal_plan_digest())
        .bind(encode(input.plan)?)
        .bind(&input.plan.actor)
        .bind(input.plan.expires_at_ms)
        .bind(input.idempotency_key)
        .bind(input.request_digest)
        .bind(input.plan.created_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await?;
        Ok(input.plan.clone())
    }

    pub async fn get_profile_apply_plan(
        &self,
        plan_id: &str,
    ) -> McpPlatformResult<StoredProfileApplyPlan> {
        self.verify_integrity().await?;
        let row = sqlx::query(
            "SELECT plan_digest,snapshot_json FROM mcp_profile_apply_plans WHERE plan_id=?",
        )
        .bind(plan_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        decode_verified_profile_apply_plan(
            &row.try_get::<String, _>("snapshot_json")
                .map_err(map_sqlx)?,
            &row.try_get::<String, _>("plan_digest").map_err(map_sqlx)?,
        )
    }

    pub(crate) async fn create_profile_apply_confirmation(
        &self,
        input: SaveProfileApplyConfirmation<'_>,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let plan = load_plan(&mut tx, input.plan_id).await?;
        if plan.actor != input.actor || plan.expires_at_ms != input.expires_at_ms {
            return Err(plan_stale());
        }
        sqlx::query(
            "INSERT INTO mcp_profile_apply_confirmations(confirmation_hash,plan_id,actor,expires_at_ms,created_at_ms) VALUES (?,?,?,?,?)",
        )
        .bind(input.confirmation_hash)
        .bind(input.plan_id)
        .bind(input.actor)
        .bind(input.expires_at_ms)
        .bind(input.created_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn create_profile_application_token(
        &self,
        input: SaveProfileApplicationToken<'_>,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let plan = load_plan(&mut tx, input.plan_id).await?;
        if plan.internal_plan_digest() != input.internal_plan_digest || plan.actor != input.actor {
            return Err(plan_stale());
        }
        let confirmation = sqlx::query(
            "SELECT plan_id,actor,expires_at_ms,consumed_at_ms FROM mcp_profile_apply_confirmations WHERE confirmation_hash=?",
        )
        .bind(input.confirmation_hash)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(plan_stale)?;
        if confirmation
            .try_get::<String, _>("plan_id")
            .map_err(map_sqlx)?
            != input.plan_id
            || confirmation
                .try_get::<String, _>("actor")
                .map_err(map_sqlx)?
                != input.actor
            || confirmation
                .try_get::<i64, _>("expires_at_ms")
                .map_err(map_sqlx)?
                <= input.created_at_ms
            || confirmation
                .try_get::<Option<i64>, _>("consumed_at_ms")
                .map_err(map_sqlx)?
                .is_some()
        {
            return Err(plan_stale());
        }
        validate_plan_snapshot(
            &mut tx,
            self.integrity_signer.as_ref(),
            &plan,
            input.created_at_ms,
        )
        .await?;
        let consumed = sqlx::query(
            "UPDATE mcp_profile_apply_confirmations SET consumed_at_ms=? WHERE confirmation_hash=? AND consumed_at_ms IS NULL",
        )
        .bind(input.created_at_ms)
        .bind(input.confirmation_hash)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if consumed.rows_affected() != 1 {
            return Err(plan_stale());
        }
        sqlx::query(
            "INSERT INTO mcp_profile_application_tokens(token_hash,application_id,plan_id,plan_digest,profile_id,profile_revision,actor,expires_at_ms,created_at_ms) VALUES (?,?,?,?,?,?,?,?,?)",
        )
        .bind(input.token_hash)
        .bind(input.application_id)
        .bind(input.plan_id)
        .bind(input.internal_plan_digest)
        .bind(&plan.profile_id)
        .bind(plan.profile_revision)
        .bind(input.actor)
        .bind(input.expires_at_ms)
        .bind(input.created_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO mcp_profile_applications(application_id,token_hash,plan_id,profile_id,profile_revision,plan_digest,actor,status,created_at_ms,updated_at_ms) VALUES (?,?,?,?,?,?,?,'confirmed',?,?)",
        )
        .bind(input.application_id)
        .bind(input.token_hash)
        .bind(input.plan_id)
        .bind(&plan.profile_id)
        .bind(plan.profile_revision)
        .bind(input.internal_plan_digest)
        .bind(input.actor)
        .bind(input.created_at_ms)
        .bind(input.created_at_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        insert_application_event(
            &mut tx,
            input.application_id,
            "token_created",
            input.actor,
            input.created_at_ms,
            "confirmed",
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub(crate) async fn consume_profile_application_token(
        &self,
        token_hash: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ConsumedProfileApplication> {
        let mut tx = self.begin_immediate().await?;
        let token = sqlx::query(
            "SELECT application_id,plan_id,plan_digest,profile_id,profile_revision,actor,expires_at_ms,consumed_at_ms FROM mcp_profile_application_tokens WHERE token_hash=?",
        )
        .bind(token_hash)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        if token.try_get::<String, _>("actor").map_err(map_sqlx)? != actor
            || token
                .try_get::<Option<i64>, _>("consumed_at_ms")
                .map_err(map_sqlx)?
                .is_some()
        {
            return Err(plan_stale());
        }
        if token.try_get::<i64, _>("expires_at_ms").map_err(map_sqlx)? <= now_ms {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::PlanExpired,
                "profile application token expired",
            ));
        }
        let plan_id: String = token.try_get("plan_id").map_err(map_sqlx)?;
        let application_id: String = token.try_get("application_id").map_err(map_sqlx)?;
        let plan = load_plan(&mut tx, &plan_id).await?;
        if token
            .try_get::<String, _>("plan_digest")
            .map_err(map_sqlx)?
            != plan.internal_plan_digest()
            || token.try_get::<String, _>("profile_id").map_err(map_sqlx)? != plan.profile_id
            || token
                .try_get::<i64, _>("profile_revision")
                .map_err(map_sqlx)?
                != plan.profile_revision
        {
            return Err(plan_stale());
        }
        validate_plan_snapshot(&mut tx, self.integrity_signer.as_ref(), &plan, now_ms).await?;

        let mut extensions = Vec::with_capacity(plan.entries.len());
        let mut managed_references = Vec::with_capacity(plan.entries.len());
        let managed_extension_names = sqlx::query_scalar::<_, String>(
            "SELECT link_key FROM connection_projections ORDER BY link_key",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        for entry in &plan.entries {
            let row = sqlx::query(
                "SELECT link_key,projection_json,projection_digest FROM connection_projections WHERE managed_mcp_id=?",
            )
            .bind(&entry.managed_mcp_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
            let link_key: String = row.try_get("link_key").map_err(map_sqlx)?;
            let projection_digest: String = row.try_get("projection_digest").map_err(map_sqlx)?;
            if projection_digest != entry.projection_digest {
                return Err(plan_stale());
            }
            let projection = crate::mcp_platform::decode_verified_projection_config(
                &row.try_get::<String, _>("projection_json")
                    .map_err(map_sqlx)?,
                &projection_digest,
            )
            .map_err(|_| plan_stale())?;
            let extension =
                CoreTransportProjectionAdapter.extension_config(&projection, &link_key)?;
            managed_references.push(ProfileManagedReference {
                managed_mcp_id: entry.managed_mcp_id.clone(),
                extension_name: link_key.clone(),
                projection_digest,
                source_fingerprint: extension_source_fingerprint(&extension)?,
            });
            extensions.push(extension);
        }
        sqlx::query(
            "UPDATE mcp_profile_application_tokens SET consumed_at_ms=? WHERE token_hash=? AND consumed_at_ms IS NULL",
        )
        .bind(now_ms)
        .bind(token_hash)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE mcp_profile_applications SET status='consumed',updated_at_ms=? WHERE application_id=? AND status='confirmed'",
        )
        .bind(now_ms)
        .bind(&application_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        insert_application_event(
            &mut tx,
            &application_id,
            "token_consumed",
            actor,
            now_ms,
            "accepted",
        )
        .await?;
        let profile_id = plan.profile_id.clone();
        let internal_plan_digest = plan.internal_plan_digest().to_string();
        tx.commit().await.map_err(map_sqlx)?;
        ConsumedProfileApplication::verified(
            application_id,
            profile_id,
            plan.profile_revision,
            internal_plan_digest,
            managed_extension_names,
            managed_references,
            extensions,
        )
    }

    pub(crate) async fn hydrate_profile_application(
        &self,
        marker: &ProfileApplicationMarker,
        session_id: &str,
        actor: &str,
    ) -> McpPlatformResult<ConsumedProfileApplication> {
        let mut tx = self.begin_immediate().await?;
        let application = sqlx::query(
            "SELECT plan_id,profile_id,profile_revision,plan_digest,actor,status,session_id FROM mcp_profile_applications WHERE application_id=?",
        )
        .bind(&marker.application_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        if application
            .try_get::<String, _>("actor")
            .map_err(map_sqlx)?
            != actor
            || application
                .try_get::<String, _>("status")
                .map_err(map_sqlx)?
                != "created"
            || application
                .try_get::<Option<String>, _>("session_id")
                .map_err(map_sqlx)?
                .as_deref()
                != Some(session_id)
            || marker.original_session_id != session_id
            || application
                .try_get::<String, _>("profile_id")
                .map_err(map_sqlx)?
                != marker.profile_id
            || application
                .try_get::<i64, _>("profile_revision")
                .map_err(map_sqlx)?
                != marker.profile_revision
        {
            return Err(plan_stale());
        }
        let plan_id: String = application.try_get("plan_id").map_err(map_sqlx)?;
        let plan = load_plan(&mut tx, &plan_id).await?;
        validate_plan_snapshot(&mut tx, self.integrity_signer.as_ref(), &plan, i64::MIN).await?;
        let (extensions, managed_references) =
            load_profile_extensions(&mut tx, &plan.entries).await?;
        let managed_extension_names = sqlx::query_scalar::<_, String>(
            "SELECT link_key FROM connection_projections ORDER BY link_key",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        ConsumedProfileApplication::verified(
            marker.application_id.clone(),
            marker.profile_id.clone(),
            marker.profile_revision,
            plan.internal_plan_digest().to_string(),
            managed_extension_names,
            managed_references,
            extensions,
        )
    }

    pub(crate) async fn finish_profile_application(
        &self,
        application_id: &str,
        session_id: Option<&str>,
        status: &str,
        detail_code: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let event = match status {
            "created" => "session_created",
            "failed" => "application_failed",
            "rolled_back" => "application_rolled_back",
            "recovery_required" => "recovery_required",
            "deleted" => "session_deleted",
            _ => return Err(invalid_request()),
        };
        let mut tx = self.begin_immediate().await?;
        let current = sqlx::query(
            "SELECT status,session_id,failure_code FROM mcp_profile_applications WHERE application_id=? AND actor=?",
        )
        .bind(application_id)
        .bind(actor)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let current_status: String = current.try_get("status").map_err(map_sqlx)?;
        let current_session_id: Option<String> = current.try_get("session_id").map_err(map_sqlx)?;
        let current_failure_code: Option<String> =
            current.try_get("failure_code").map_err(map_sqlx)?;
        if current_status == status
            && current_session_id.as_deref() == session_id
            && (status == "created"
                || status == "deleted"
                || current_failure_code.as_deref() == Some(detail_code))
        {
            return Ok(());
        }
        let updated = match status {
            "created" => {
                let session_id = session_id.ok_or_else(invalid_request)?;
                sqlx::query("UPDATE mcp_profile_applications SET status='created',session_id=?,failure_code=NULL,updated_at_ms=? WHERE application_id=? AND actor=? AND status='consumed' AND session_id IS NULL")
                    .bind(session_id).bind(now_ms).bind(application_id).bind(actor)
                    .execute(&mut **tx).await.map_err(map_sqlx)?
            }
            "failed" => sqlx::query("UPDATE mcp_profile_applications SET status='failed',session_id=COALESCE(?,session_id),failure_code=?,updated_at_ms=? WHERE application_id=? AND actor=? AND status IN ('consumed','created','failed') AND (session_id IS NULL OR session_id=?)")
                .bind(session_id).bind(detail_code).bind(now_ms).bind(application_id).bind(actor).bind(session_id)
                .execute(&mut **tx).await.map_err(map_sqlx)?,
            "rolled_back" => {
                sqlx::query("UPDATE mcp_profile_applications SET status='rolled_back',session_id=COALESCE(?,session_id),failure_code=?,updated_at_ms=? WHERE application_id=? AND actor=? AND status IN ('consumed','failed') AND (session_id IS NULL OR session_id=?)")
                    .bind(session_id).bind(detail_code).bind(now_ms).bind(application_id).bind(actor).bind(session_id)
                    .execute(&mut **tx).await.map_err(map_sqlx)?
            }
            "recovery_required" => sqlx::query("UPDATE mcp_profile_applications SET status='recovery_required',session_id=COALESCE(?,session_id),failure_code=?,updated_at_ms=? WHERE application_id=? AND actor=? AND status IN ('consumed','created','failed','recovery_required') AND (session_id IS NULL OR session_id=?)")
                .bind(session_id).bind(detail_code).bind(now_ms).bind(application_id).bind(actor).bind(session_id)
                .execute(&mut **tx).await.map_err(map_sqlx)?,
            "deleted" => {
                let session_id = session_id.ok_or_else(invalid_request)?;
                sqlx::query("UPDATE mcp_profile_applications SET status='deleted',failure_code=NULL,updated_at_ms=? WHERE application_id=? AND actor=? AND status IN ('created','recovery_required') AND session_id=?")
                    .bind(now_ms).bind(application_id).bind(actor).bind(session_id)
                    .execute(&mut **tx).await.map_err(map_sqlx)?
            }
            _ => unreachable!(),
        };
        if updated.rows_affected() != 1 {
            return Err(not_found());
        }
        insert_application_event(&mut tx, application_id, event, actor, now_ms, detail_code)
            .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    async fn decode_profile_row(
        &self,
        row: sqlx::sqlite::SqliteRow,
    ) -> McpPlatformResult<McpProfile> {
        let profile_id: String = row.try_get("profile_id").map_err(map_sqlx)?;
        let entries = load_entries(&self.pool, &profile_id).await?;
        Ok(McpProfile {
            profile_id,
            name: row.try_get("name").map_err(map_sqlx)?,
            description: row.try_get("description").map_err(map_sqlx)?,
            revision: row.try_get("revision").map_err(map_sqlx)?,
            archived: row.try_get("archived").map_err(map_sqlx)?,
            entries,
            created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
            updated_at_ms: row.try_get("updated_at_ms").map_err(map_sqlx)?,
        })
    }

    async fn profile_idempotency(
        &self,
        operation: &str,
        key: &str,
        request_digest: &str,
    ) -> McpPlatformResult<Option<McpProfile>> {
        let row = sqlx::query(
            "SELECT request_digest,profile_id,resulting_revision FROM mcp_profile_idempotency WHERE operation=? AND idempotency_key=?",
        )
        .bind(operation)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        let Some(row) = row else { return Ok(None) };
        if row
            .try_get::<String, _>("request_digest")
            .map_err(map_sqlx)?
            != request_digest
        {
            return Err(idempotency_conflict());
        }
        let revision = self
            .get_profile_revision(
                &row.try_get::<String, _>("profile_id").map_err(map_sqlx)?,
                row.try_get("resulting_revision").map_err(map_sqlx)?,
            )
            .await?;
        Ok(Some(McpProfile {
            profile_id: revision.profile_id,
            name: revision.name,
            description: revision.description,
            revision: revision.revision,
            archived: revision.archived,
            entries: revision.entries,
            created_at_ms: revision.created_at_ms,
            updated_at_ms: revision.created_at_ms,
        }))
    }
}

async fn load_entries(
    pool: &sqlx::Pool<Sqlite>,
    profile_id: &str,
) -> McpPlatformResult<Vec<ProfileEntry>> {
    let rows = sqlx::query(
        "SELECT managed_mcp_id,ordinal FROM mcp_profile_entries WHERE profile_id=? ORDER BY ordinal",
    )
    .bind(profile_id)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx)?;
    rows.into_iter()
        .map(|row| {
            Ok(ProfileEntry {
                managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
                ordinal: row.try_get("ordinal").map_err(map_sqlx)?,
            })
        })
        .collect()
}

async fn replace_entries(
    tx: &mut Transaction<'_, Sqlite>,
    profile_id: &str,
    entries: &[ProfileEntry],
) -> McpPlatformResult<()> {
    sqlx::query("DELETE FROM mcp_profile_entries WHERE profile_id=?")
        .bind(profile_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    for entry in entries {
        sqlx::query(
            "INSERT INTO mcp_profile_entries(profile_id,managed_mcp_id,ordinal) VALUES (?,?,?)",
        )
        .bind(profile_id)
        .bind(&entry.managed_mcp_id)
        .bind(entry.ordinal)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    }
    Ok(())
}

async fn insert_revision(
    tx: &mut Transaction<'_, Sqlite>,
    profile: &McpProfile,
    actor: &str,
    operation: &str,
    now_ms: i64,
) -> McpPlatformResult<()> {
    sqlx::query("INSERT INTO mcp_profile_revisions(profile_id,revision,snapshot_json,credential_references_json,actor,operation,created_at_ms) VALUES (?,?,?,?,?,?,?)")
        .bind(&profile.profile_id)
        .bind(profile.revision)
        .bind(encode(profile)?)
        .bind(encode_empty_profile_credential_references()?)
        .bind(actor)
        .bind(operation)
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    Ok(())
}

fn encode_empty_profile_credential_references() -> McpPlatformResult<String> {
    encode(&Vec::<ProfileCredentialReference>::new())
}

#[allow(clippy::too_many_arguments)]
async fn insert_idempotency(
    tx: &mut Transaction<'_, Sqlite>,
    operation: &str,
    key: &str,
    digest: &str,
    profile_id: &str,
    revision: i64,
    now_ms: i64,
) -> McpPlatformResult<()> {
    sqlx::query("INSERT INTO mcp_profile_idempotency(operation,idempotency_key,request_digest,profile_id,resulting_revision,created_at_ms) VALUES (?,?,?,?,?,?)")
        .bind(operation).bind(key).bind(digest).bind(profile_id).bind(revision).bind(now_ms)
        .execute(&mut **tx).await.map_err(map_sqlx)?;
    Ok(())
}

async fn load_plan(
    tx: &mut Transaction<'_, Sqlite>,
    plan_id: &str,
) -> McpPlatformResult<StoredProfileApplyPlan> {
    let row = sqlx::query(
        "SELECT plan_digest,snapshot_json FROM mcp_profile_apply_plans WHERE plan_id=?",
    )
    .bind(plan_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(not_found)?;
    decode_verified_profile_apply_plan(
        &row.try_get::<String, _>("snapshot_json")
            .map_err(map_sqlx)?,
        &row.try_get::<String, _>("plan_digest").map_err(map_sqlx)?,
    )
}

fn decode_verified_profile_apply_plan(
    snapshot_json: &str,
    database_plan_digest: &str,
) -> McpPlatformResult<StoredProfileApplyPlan> {
    let plan: StoredProfileApplyPlan = decode(snapshot_json).map_err(|_| plan_stale())?;
    verify_profile_apply_plan_digest(&plan, database_plan_digest)?;
    Ok(plan)
}

fn verify_profile_apply_plan_digest(
    plan: &StoredProfileApplyPlan,
    database_plan_digest: &str,
) -> McpPlatformResult<()> {
    let recomputed = profile_apply_plan_snapshot_digest(plan).map_err(|_| plan_stale())?;
    if plan.internal_plan_digest() != database_plan_digest
        || recomputed != plan.internal_plan_digest()
    {
        return Err(plan_stale());
    }
    Ok(())
}

async fn load_profile_extensions(
    tx: &mut Transaction<'_, Sqlite>,
    entries: &[StoredProfileManagedSnapshot],
) -> McpPlatformResult<(
    Vec<crate::agents::ExtensionConfig>,
    Vec<ProfileManagedReference>,
)> {
    let mut extensions = Vec::with_capacity(entries.len());
    let mut references = Vec::with_capacity(entries.len());
    for entry in entries {
        let row = sqlx::query(
            "SELECT link_key,projection_json,projection_digest FROM connection_projections WHERE managed_mcp_id=?",
        )
        .bind(&entry.managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(plan_stale)?;
        let link_key: String = row.try_get("link_key").map_err(map_sqlx)?;
        let projection_digest: String = row.try_get("projection_digest").map_err(map_sqlx)?;
        if projection_digest != entry.projection_digest {
            return Err(plan_stale());
        }
        let projection = crate::mcp_platform::decode_verified_projection_config(
            &row.try_get::<String, _>("projection_json")
                .map_err(map_sqlx)?,
            &projection_digest,
        )
        .map_err(|_| plan_stale())?;
        let extension = CoreTransportProjectionAdapter.extension_config(&projection, &link_key)?;
        references.push(ProfileManagedReference {
            managed_mcp_id: entry.managed_mcp_id.clone(),
            extension_name: link_key,
            projection_digest,
            source_fingerprint: extension_source_fingerprint(&extension)?,
        });
        extensions.push(extension);
    }
    Ok((extensions, references))
}

async fn validate_plan_snapshot(
    tx: &mut Transaction<'_, Sqlite>,
    signer: &dyn super::integrity::IntegritySigner,
    plan: &StoredProfileApplyPlan,
    now_ms: i64,
) -> McpPlatformResult<()> {
    if plan.expires_at_ms <= now_ms {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::PlanExpired,
            "MCP profile application plan expired",
        ));
    }
    if plan.merge_policy != "replace_managed_only" {
        return Err(plan_stale());
    }
    let profile = sqlx::query("SELECT revision,archived FROM mcp_profiles WHERE profile_id=?")
        .bind(&plan.profile_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
    if profile.try_get::<i64, _>("revision").map_err(map_sqlx)? != plan.profile_revision
        || profile.try_get::<bool, _>("archived").map_err(map_sqlx)?
    {
        return Err(plan_stale());
    }
    let profile_entries = sqlx::query_scalar::<_, String>(
        "SELECT managed_mcp_id FROM mcp_profile_entries WHERE profile_id=? ORDER BY ordinal",
    )
    .bind(&plan.profile_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if profile_entries
        != plan
            .entries
            .iter()
            .map(|entry| entry.managed_mcp_id.clone())
            .collect::<Vec<_>>()
    {
        return Err(plan_stale());
    }
    for expected in &plan.entries {
        if expected.manifest_digest.len() != 64
            || expected.projection_digest.len() != 64
            || expected.auth_evidence_digest.len() != 64
            || expected.policy_evidence_digest.len() != 64
        {
            return Err(plan_stale());
        }
        let row = sqlx::query(
            r#"SELECT m.revision,m.state_json,m.active_manifest_digest,v.manifest_digest,
                      p.revision AS projection_revision,p.projection_digest,p.projection_json,
                      p.link_key,p.manifest_digest AS projection_manifest_digest,p.plan_id,
                      p.owner_task_id,i.mac AS projection_mac
               FROM managed_mcps m
               JOIN managed_versions v ON v.managed_mcp_id=m.managed_mcp_id AND v.active=1
               JOIN connection_projections p ON p.managed_mcp_id=m.managed_mcp_id
                JOIN projection_writer_bindings w
                 ON w.managed_mcp_id=p.managed_mcp_id AND w.task_id=p.owner_task_id
                AND w.plan_id=p.plan_id AND w.link_key=p.link_key
                AND w.manifest_digest=p.manifest_digest
                 AND w.projection_digest=p.projection_digest
               JOIN managed_projection_integrity i ON i.managed_mcp_id=p.managed_mcp_id
               WHERE m.managed_mcp_id=?"#,
        )
        .bind(&expected.managed_mcp_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(plan_stale)?;
        let state: crate::mcp_platform::ManagedMcpState =
            decode(&row.try_get::<String, _>("state_json").map_err(map_sqlx)?)?;
        let policy_ready = state.registration == RegistrationState::Registered
            && matches!(
                state.installation,
                InstallationState::Installed | InstallationState::NotApplicable
            );
        let managed_revision = row.try_get::<i64, _>("revision").map_err(map_sqlx)?;
        let manifest_digest = row
            .try_get::<String, _>("manifest_digest")
            .map_err(map_sqlx)?;
        let projection_revision = row
            .try_get::<i64, _>("projection_revision")
            .map_err(map_sqlx)?;
        let projection_digest = row
            .try_get::<String, _>("projection_digest")
            .map_err(map_sqlx)?;
        let active_manifest_digest = row
            .try_get::<Option<String>, _>("active_manifest_digest")
            .map_err(map_sqlx)?
            .ok_or_else(plan_stale)?;
        let projection_manifest_digest = row
            .try_get::<Option<String>, _>("projection_manifest_digest")
            .map_err(map_sqlx)?
            .ok_or_else(plan_stale)?;
        let plan_id = row
            .try_get::<Option<String>, _>("plan_id")
            .map_err(map_sqlx)?
            .ok_or_else(plan_stale)?;
        let owner_task_id = row
            .try_get::<Option<String>, _>("owner_task_id")
            .map_err(map_sqlx)?
            .ok_or_else(plan_stale)?;
        let link_key: String = row.try_get("link_key").map_err(map_sqlx)?;
        signer.verify(
            "managed-projection",
            &super::records::managed_projection_integrity_payload(
                &expected.managed_mcp_id,
                &link_key,
                &projection_digest,
                projection_revision,
                Some(&plan_id),
                Some(&projection_manifest_digest),
                Some(&owner_task_id),
            ),
            &row.try_get::<String, _>("projection_mac")
                .map_err(map_sqlx)?,
        )?;
        super::lifecycle_repository::verify_projection_writer_binding(
            tx,
            signer,
            &expected.managed_mcp_id,
            &owner_task_id,
            &plan_id,
            &link_key,
            &projection_manifest_digest,
            &projection_digest,
        )
        .await?;
        crate::mcp_platform::decode_verified_projection_config(
            &row.try_get::<String, _>("projection_json")
                .map_err(map_sqlx)?,
            &projection_digest,
        )
        .map_err(|_| plan_stale())?;
        let policy_evidence_digest = profile_policy_evidence_digest(
            &state,
            managed_revision,
            &manifest_digest,
            projection_revision,
            &projection_digest,
        )?;
        if managed_revision != expected.managed_revision
            || active_manifest_digest != manifest_digest
            || projection_manifest_digest != manifest_digest
            || row
                .try_get::<String, _>("manifest_digest")
                .map_err(map_sqlx)?
                != expected.manifest_digest
            || projection_revision != expected.projection_revision
            || projection_digest != expected.projection_digest
            || health_code(state.health) != expected.health
            || policy_ready != expected.policy_ready
            || policy_evidence_digest != expected.policy_evidence_digest
            || !expected.auth_ready
            || !policy_ready
        {
            return Err(plan_stale());
        }
    }
    Ok(())
}

async fn insert_application_event(
    tx: &mut Transaction<'_, Sqlite>,
    application_id: &str,
    event_type: &str,
    actor: &str,
    now_ms: i64,
    detail_code: &str,
) -> McpPlatformResult<()> {
    sqlx::query("INSERT INTO mcp_profile_application_events(application_id,event_type,actor,occurred_at_ms,detail_code) VALUES (?,?,?,?,?)")
        .bind(application_id).bind(event_type).bind(actor).bind(now_ms).bind(detail_code)
        .execute(&mut **tx).await.map_err(map_sqlx)?;
    Ok(())
}

async fn revision_or_missing(
    tx: &mut Transaction<'_, Sqlite>,
    profile_id: &str,
) -> McpPlatformResult<McpPlatformError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM mcp_profiles WHERE profile_id=?)",
    )
    .bind(profile_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    Ok(if exists {
        McpPlatformError::new(
            McpPlatformErrorCode::RevisionConflict,
            "profile revision conflict",
        )
    } else {
        not_found()
    })
}

fn health_code(value: HealthState) -> String {
    match value {
        HealthState::Unknown => "unknown",
        HealthState::Checking => "checking",
        HealthState::Healthy => "healthy",
        HealthState::Degraded => "degraded",
        HealthState::Unhealthy => "unhealthy",
        HealthState::BlockedAuth => "blocked_auth",
        HealthState::Incompatible => "incompatible",
    }
    .to_string()
}

fn not_found() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::NotFound,
        "MCP profile record not found",
    )
}

fn plan_stale() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PlanStale,
        "MCP profile application plan is stale",
    )
}

fn idempotency_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IdempotencyConflict,
        "profile idempotency key conflict",
    )
}

fn invalid_request() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidRequest,
        "invalid profile application status",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_platform::{
        credential_authority::EnrollmentWriterOwner,
        credential_enrollment::{EnrollmentFieldSubmission, EnrollmentSecret},
        parse_manifest, profile_auth_evidence_digest, profile_policy_evidence_digest,
        ConnectionProjection, HealthState, InstallationState,
        ManagedCredentialEnrollmentBeginInput, ManagedCredentialEnrollmentSubmitInput,
        ManagedMcpState, ManifestProof, ManifestRecord, McpPlatformService,
        McpPlatformServiceOptions, ProfileApplyPlanInput, ProfileCreateInput, RegistrationState,
        RequestContext, RuntimeState, ToolPolicy, ToolPolicyDecision, TrustTier,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use crate::mcp_platform::service::{SystemClock, UuidGenerator};
    use crate::session::{EnabledExtensionsState, ExtensionData, ExtensionState};

    async fn repository() -> SqliteMcpPlatformRepository {
        SqliteMcpPlatformRepository::open_url_with_integrity_signer(
            "sqlite::memory:",
            super::super::InMemoryIntegritySigner::new_for_testing([0x51; 32]),
        )
        .await
        .unwrap()
    }

    async fn sign_projection_fixture(
        repository: &SqliteMcpPlatformRepository,
        managed_mcp_id: &str,
        link_key: &str,
        projection_digest: &str,
        revision: i64,
        plan_id: Option<&str>,
        manifest_digest: Option<&str>,
        owner_task_id: Option<&str>,
    ) {
        let mac = repository
            .integrity_signer
            .sign(
                "managed-projection",
                &super::super::records::managed_projection_integrity_payload(
                    managed_mcp_id,
                    link_key,
                    projection_digest,
                    revision,
                    plan_id,
                    manifest_digest,
                    owner_task_id,
                ),
            )
            .unwrap();
        sqlx::query("INSERT INTO managed_projection_integrity(managed_mcp_id,mac) VALUES (?,?)")
            .bind(managed_mcp_id)
            .bind(mac)
            .execute(&repository.pool)
            .await
            .unwrap();
    }

    async fn own_projection_fixture(
        repository: &SqliteMcpPlatformRepository,
        managed_mcp_id: &str,
        link_key: &str,
        projection_digest: &str,
        revision: i64,
        manifest_digest: &str,
    ) {
        let plan_id = format!("fixture-plan-{managed_mcp_id}");
        let task_id = format!("fixture-task-{managed_mcp_id}");
        let plan_digest = "a".repeat(64);
        sqlx::query(
            r#"INSERT INTO install_plans(
                plan_id,plan_digest,envelope_digest,manifest_digest,operation,target_json,
                plan_json,policy_evidence_json,confirmation_evidence_json,expires_at_ms,
                idempotency_key,actor,created_at_ms
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(&plan_id)
        .bind(&plan_digest)
        .bind("b".repeat(64))
        .bind(manifest_digest)
        .bind("install")
        .bind("{}")
        .bind("{}")
        .bind("{}")
        .bind("{}")
        .bind(10_000_i64)
        .bind(format!("fixture-plan-key-{managed_mcp_id}"))
        .bind("fixture")
        .bind(1_i64)
        .execute(&repository.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO tasks(
                task_id,plan_id,plan_digest,operation,idempotency_key,status,actor,
                created_at_ms,updated_at_ms,progress,step_cursor,rollback_status,
                revision,event_sequence
            ) VALUES (?,?,?,?,?,'succeeded','fixture',1,1,100,9,'not_required',1,1)"#,
        )
        .bind(&task_id)
        .bind(&plan_id)
        .bind(&plan_digest)
        .bind("install")
        .bind(format!("fixture-task-key-{managed_mcp_id}"))
        .execute(&repository.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE connection_projections SET plan_id=?,manifest_digest=?,owner_task_id=? WHERE managed_mcp_id=?",
        )
        .bind(&plan_id)
        .bind(manifest_digest)
        .bind(&task_id)
        .bind(managed_mcp_id)
        .execute(&repository.pool)
        .await
        .unwrap();
        let writer_mac = repository
            .integrity_signer
            .sign(
                "projection-writer-authorization",
                &super::super::records::projection_writer_payload(
                    managed_mcp_id,
                    &task_id,
                    &plan_id,
                    &plan_digest,
                    link_key,
                    manifest_digest,
                    projection_digest,
                    1,
                    "fixture-worker",
                    2,
                    8,
                    "fixture-step",
                ),
            )
            .unwrap();
        sqlx::query(
            r#"INSERT INTO projection_writer_bindings(
                managed_mcp_id,task_id,plan_id,plan_digest,link_key,manifest_digest,
                projection_digest,lifecycle_acquired_at_ms,worker_owner_id,
                worker_lease_expires_at_ms,step_ordinal,step_token,mac
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)"#,
        )
        .bind(managed_mcp_id)
        .bind(&task_id)
        .bind(&plan_id)
        .bind(&plan_digest)
        .bind(link_key)
        .bind(manifest_digest)
        .bind(projection_digest)
        .bind(1_i64)
        .bind("fixture-worker")
        .bind(2_i64)
        .bind(8_i64)
        .bind("fixture-step")
        .bind(writer_mac)
        .execute(&repository.pool)
        .await
        .unwrap();
        sign_projection_fixture(
            repository,
            managed_mcp_id,
            link_key,
            projection_digest,
            revision,
            Some(&plan_id),
            Some(manifest_digest),
            Some(&task_id),
        )
        .await;
    }

    #[tokio::test]
    async fn provenance_paginates_beyond_10000_and_redacts_last_page_alias() {
        let repository = repository().await;
        let state_json = encode(&ManagedMcpState::default()).unwrap();
        let mut tx = repository.pool.begin().await.unwrap();
        for index in 0..=10_000 {
            let managed_mcp_id = format!("managed-{index:05}");
            let mcp_id = format!("mcp-{index:05}");
            let link_key = format!("managed_mcp_{index:05}");
            let projection = ConnectionProjection::RemoteHttp {
                name: link_key.clone(),
                description: "managed provenance".to_string(),
                uri: format!("https://managed-{index:05}.invalid/"),
                timeout_seconds: Some(30),
            };
            sqlx::query(
                "INSERT INTO managed_mcps(managed_mcp_id,mcp_id,installation_scope,state_json,revision,created_at_ms,updated_at_ms) VALUES (?,?,?,?,0,1,1)",
            )
            .bind(&managed_mcp_id)
            .bind(mcp_id)
            .bind("user")
            .bind(&state_json)
            .execute(tx.as_mut())
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO connection_projections(managed_mcp_id,link_key,projection_json,revision,updated_at_ms,projection_digest) VALUES (?,?,?,0,1,?)",
            )
            .bind(&managed_mcp_id)
            .bind(&link_key)
            .bind(crate::mcp_platform::encode_projection_config(&projection).unwrap())
            .bind(crate::mcp_platform::projection_config_digest(&projection).unwrap())
            .execute(tx.as_mut())
            .await
            .unwrap();
            let mac = repository
                .integrity_signer
                .sign(
                    "managed-projection",
                    &super::super::records::managed_projection_integrity_payload(
                        &managed_mcp_id,
                        &link_key,
                        &crate::mcp_platform::projection_config_digest(&projection).unwrap(),
                        0,
                        None,
                        None,
                        None,
                    ),
                )
                .unwrap();
            sqlx::query(
                "INSERT INTO managed_projection_integrity(managed_mcp_id,mac) VALUES (?,?)",
            )
            .bind(&managed_mcp_id)
            .bind(mac)
            .execute(tx.as_mut())
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();

        let service = McpPlatformService::new(
            Arc::new(repository),
            Arc::new(SystemClock),
            Arc::new(UuidGenerator),
            McpPlatformServiceOptions::default(),
        );
        let provenance = service.managed_extension_provenance().await.unwrap();
        assert_eq!(provenance.len(), 10_001);
        assert_eq!(provenance.last().unwrap().0, "managed-10000");

        let ordinary = crate::agents::ExtensionConfig::Builtin {
            name: "developer".to_string(),
            description: String::new(),
            display_name: None,
            timeout: None,
            bundled: Some(true),
            available_tools: Vec::new(),
        };
        let alias = crate::agents::ExtensionConfig::ManagedStreamableHttp {
            name: "last-page-alias".to_string(),
            description: "managed provenance".to_string(),
            uri: "https://managed-10000.invalid/".to_string(),
            timeout: Some(30),
            bundled: None,
            available_tools: Vec::new(),
        };
        let mut extension_data = ExtensionData::new();
        EnabledExtensionsState::new(vec![ordinary.clone(), alias])
            .to_extension_data(&mut extension_data)
            .unwrap();
        let names = provenance.iter().map(|(_, name, _)| name.clone()).collect();
        let source_fingerprints = provenance
            .into_iter()
            .map(|(_, _, reference)| reference.source_fingerprint)
            .collect();
        extension_data
            .redact_managed_configs(&names, &source_fingerprints)
            .unwrap();
        assert_eq!(
            EnabledExtensionsState::from_extension_data(&extension_data)
                .unwrap()
                .extensions,
            vec![ordinary]
        );
    }

    async fn empty_profile(repository: &SqliteMcpPlatformRepository) -> McpProfile {
        repository
            .create_profile(SaveProfile {
                profile_id: "profile_test",
                name: "Test",
                description: "safe",
                entries: &[],
                idempotency_key: "create-key",
                request_digest: &"a".repeat(64),
                actor: "local_authenticated_client",
                now_ms: 10,
            })
            .await
            .unwrap()
    }

    fn plan(profile: &McpProfile, now_ms: i64) -> StoredProfileApplyPlan {
        let mut plan = StoredProfileApplyPlan {
            plan_id: format!("profile_plan_{now_ms}"),
            profile_id: profile.profile_id.clone(),
            profile_revision: profile.revision,
            internal_plan_digest: String::new(),
            merge_policy: "replace_managed_only".to_string(),
            entries: Vec::new(),
            actor: "local_authenticated_client".to_string(),
            expires_at_ms: now_ms + 1_000,
            created_at_ms: now_ms,
        };
        plan.internal_plan_digest = profile_apply_plan_snapshot_digest(&plan).unwrap();
        plan
    }

    async fn create_confirmation(
        repository: &SqliteMcpPlatformRepository,
        plan: &StoredProfileApplyPlan,
        confirmation_hash: &str,
        created_at_ms: i64,
    ) {
        repository
            .create_profile_apply_confirmation(SaveProfileApplyConfirmation {
                confirmation_hash,
                plan_id: &plan.plan_id,
                actor: &plan.actor,
                expires_at_ms: plan.expires_at_ms,
                created_at_ms,
            })
            .await
            .unwrap();
    }

    async fn profile_with_managed_entry(
        repository: &SqliteMcpPlatformRepository,
    ) -> (McpProfile, StoredProfileManagedSnapshot, ManagedMcpState) {
        let manifest_digest = "7".repeat(64);
        let state = ManagedMcpState {
            registration: RegistrationState::Registered,
            installation: InstallationState::Installed,
            runtime: RuntimeState::Stopped,
            health: HealthState::Healthy,
            default_enabled: false,
            session_enabled: BTreeMap::new(),
            tool_policies: BTreeMap::from([(
                "read".to_string(),
                ToolPolicy {
                    decision: ToolPolicyDecision::Allow,
                    scope: None,
                },
            )]),
        };
        sqlx::query("INSERT INTO manifest_blobs(manifest_digest,mcp_id,version,canonical_bytes,proof_json,trust_tier_json,created_at_ms) VALUES (?,?,?,?,?,?,?)")
            .bind(&manifest_digest).bind("actual-entry").bind("1.0.0").bind(Vec::<u8>::new()).bind("{}").bind("\"official\"").bind(1_i64)
            .execute(&repository.pool).await.unwrap();
        sqlx::query("INSERT INTO managed_mcps(managed_mcp_id,mcp_id,installation_scope,state_json,revision,created_at_ms,updated_at_ms,active_manifest_digest,active_version) VALUES (?,?,?,?,?,?,?,?,?)")
            .bind("managed_actual").bind("actual-entry").bind("user").bind(encode(&state).unwrap()).bind(3_i64).bind(1_i64).bind(1_i64).bind(&manifest_digest).bind("1.0.0")
            .execute(&repository.pool).await.unwrap();
        sqlx::query("INSERT INTO managed_versions(managed_mcp_id,version,manifest_digest,verified,active,created_at_ms) VALUES (?,?,?,?,?,?)")
            .bind("managed_actual").bind("1.0.0").bind(&manifest_digest).bind(true).bind(true).bind(1_i64)
            .execute(&repository.pool).await.unwrap();
        let projection = ConnectionProjection::ManagedStdio {
            name: "actual".to_string(),
            description: "actual managed entry".to_string(),
            executable: "managed-binary".to_string(),
            args: vec!["serve".to_string()],
            environment_keys: vec!["ACTUAL_TOKEN".to_string()],
            cwd: None,
            timeout_seconds: Some(30),
        };
        let projection_digest = crate::mcp_platform::projection_config_digest(&projection).unwrap();
        sqlx::query("INSERT INTO connection_projections(managed_mcp_id,link_key,projection_json,revision,updated_at_ms,manifest_digest,projection_digest) VALUES (?,?,?,?,?,?,?)")
            .bind("managed_actual").bind("managed-link").bind(encode(&projection).unwrap()).bind(4_i64).bind(1_i64).bind(&manifest_digest).bind(&projection_digest)
            .execute(&repository.pool).await.unwrap();
        own_projection_fixture(
            repository,
            "managed_actual",
            "managed-link",
            &projection_digest,
            4,
            &manifest_digest,
        )
        .await;
        let entries = vec![ProfileEntry {
            managed_mcp_id: "managed_actual".to_string(),
            ordinal: 0,
        }];
        let profile = repository
            .create_profile(SaveProfile {
                profile_id: "profile_actual",
                name: "Actual",
                description: "actual entry",
                entries: &entries,
                idempotency_key: "actual-profile-key",
                request_digest: &"9".repeat(64),
                actor: "local_authenticated_client",
                now_ms: 2,
            })
            .await
            .unwrap();
        let snapshot = StoredProfileManagedSnapshot {
            managed_mcp_id: "managed_actual".to_string(),
            managed_revision: 3,
            manifest_digest: manifest_digest.clone(),
            projection_revision: 4,
            projection_digest: projection_digest.clone(),
            health: "healthy".to_string(),
            auth_ready: true,
            policy_ready: true,
            auth_evidence_digest: profile_auth_evidence_digest(
                &"a".repeat(64),
                3,
                &manifest_digest,
                4,
                &projection_digest,
            )
            .unwrap(),
            policy_evidence_digest: profile_policy_evidence_digest(
                &state,
                3,
                &manifest_digest,
                4,
                &projection_digest,
            )
            .unwrap(),
        };
        (profile, snapshot, state)
    }

    fn profile_service_with_enrollment(
        repository: &SqliteMcpPlatformRepository,
    ) -> McpPlatformService {
        McpPlatformService::new_trusted(
            Arc::new(repository.clone()),
            Arc::new(SystemClock),
            Arc::new(UuidGenerator),
            McpPlatformServiceOptions::default(),
        )
        .with_enrollment_writer_owner(EnrollmentWriterOwner::in_memory_for_testing_default_time())
    }

    async fn enroll_profile_managed(
        service: &McpPlatformService,
        repository: &SqliteMcpPlatformRepository,
        secret: &str,
    ) {
        let context = service.trusted_local_context();
        let binding = service.new_enrollment_user_action_binding();
        let inventory = repository
            .get_managed_inventory("managed_auth_profile")
            .await
            .unwrap();
        let begin = service
            .credential_enrollment_begin(
                &context,
                ManagedCredentialEnrollmentBeginInput {
                    user_action_binding: binding.clone(),
                    managed_mcp_id: "managed_auth_profile".to_string(),
                    expected_revision: inventory.managed.revision,
                    profile_scope: None,
                },
            )
            .await
            .unwrap();
        service
            .credential_enrollment_submit(
                &context,
                ManagedCredentialEnrollmentSubmitInput {
                    user_action_binding: binding,
                    session_token: begin.session_token,
                    current_profile_scope: None,
                    fields: vec![EnrollmentFieldSubmission {
                        id: "secret".to_string(),
                        value: EnrollmentSecret::new(secret.to_string()),
                    }],
                },
            )
            .await
            .unwrap();
    }

    async fn profile_service_with_enrollment_fixture(
        repository: &SqliteMcpPlatformRepository,
    ) -> McpPlatformService {
        let raw = serde_json::json!({
            "schema_version": 1,
            "id": "auth-profile-test",
            "version": "1.0.0",
            "name": "Auth profile test",
            "description": "Credential evidence regression fixture.",
            "publisher": {"id": "test", "name": "Test"},
            "license": {"spdx": "MIT"},
            "capabilities": ["tools"],
            "permissions": [],
            "distribution": {"type": "remote_http"},
            "transport": {
                "type": "streamable_http",
                "url": "https://example.com/mcp",
                "allowed_redirect_origins": []
            },
            "auth": {
                "type": "environment",
                "environment_key": "TEST_TOKEN",
                "credential_name": "test-token"
            },
            "health_check": {"type": "mcp_initialize", "timeout_seconds": 30},
            "owned_files": [],
            "uninstall": {"mode": "remove_owned_files_only", "preserve_user_data": true}
        });
        let verified = parse_manifest(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let manifest_digest = verified.digest().to_string();
        repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Local,
                source_metadata: Default::default(),
                created_at_ms: 1,
            })
            .await
            .unwrap();
        let state = ManagedMcpState {
            registration: RegistrationState::Registered,
            installation: InstallationState::Installed,
            runtime: RuntimeState::Stopped,
            health: HealthState::Healthy,
            default_enabled: false,
            session_enabled: BTreeMap::new(),
            tool_policies: BTreeMap::new(),
        };
        sqlx::query("INSERT INTO managed_mcps(managed_mcp_id,mcp_id,installation_scope,state_json,revision,created_at_ms,updated_at_ms,active_manifest_digest,active_version) VALUES (?,?,?,?,?,?,?,?,?)")
            .bind("managed_auth_profile").bind("auth-profile-test").bind("user").bind(encode(&state).unwrap()).bind(1_i64).bind(1_i64).bind(1_i64).bind(&manifest_digest).bind("1.0.0")
            .execute(&repository.pool).await.unwrap();
        sqlx::query("INSERT INTO managed_versions(managed_mcp_id,version,manifest_digest,verified,active,created_at_ms) VALUES (?,?,?,?,?,?)")
            .bind("managed_auth_profile").bind("1.0.0").bind(&manifest_digest).bind(true).bind(true).bind(1_i64)
            .execute(&repository.pool).await.unwrap();
        let projection = ConnectionProjection::ManagedStdio {
            name: "auth-profile".to_string(),
            description: "auth profile".to_string(),
            executable: "auth-profile-command".to_string(),
            args: Vec::new(),
            environment_keys: vec!["TEST_TOKEN".to_string()],
            cwd: None,
            timeout_seconds: Some(30),
        };
        let projection_digest = crate::mcp_platform::projection_config_digest(&projection).unwrap();
        sqlx::query("INSERT INTO connection_projections(managed_mcp_id,link_key,projection_json,revision,updated_at_ms,manifest_digest,projection_digest) VALUES (?,?,?,?,?,?,?)")
            .bind("managed_auth_profile").bind("managed_auth_profile_link").bind(encode(&projection).unwrap()).bind(1_i64).bind(1_i64).bind(&manifest_digest).bind(&projection_digest)
            .execute(&repository.pool).await.unwrap();
        own_projection_fixture(
            repository,
            "managed_auth_profile",
            "managed_auth_profile_link",
            &projection_digest,
            1,
            &manifest_digest,
        )
        .await;

        profile_service_with_enrollment(repository)
    }

    #[tokio::test]
    async fn enrollment_drift_stales_every_profile_application_stage_and_redacts_plan_facts() {
        let repository = repository().await;
        let service = profile_service_with_enrollment_fixture(&repository).await;
        enroll_profile_managed(&service, &repository, "secret-1").await;
        let context =
            RequestContext::local_authenticated_client("profile-auth-regression".to_string());
        let profile = service
            .profile_create(
                &context,
                ProfileCreateInput {
                    name: "Auth profile".to_string(),
                    description: String::new(),
                    managed_mcp_ids: vec!["managed_auth_profile".to_string()],
                    idempotency_key: "auth-profile-create".to_string(),
                },
            )
            .await
            .unwrap();
        let plan = service
            .profile_apply_plan_create(
                &context,
                ProfileApplyPlanInput {
                    profile_id: profile.profile_id.clone(),
                    profile_revision: profile.revision,
                    idempotency_key: "auth-profile-plan".to_string(),
                },
            )
            .await
            .unwrap();
        let stored_plan = repository
            .get_profile_apply_plan(&plan.plan_id)
            .await
            .unwrap();
        let original_manifest_digest = stored_plan.entries[0].manifest_digest.clone();
        let enrollment = repository
            .get_managed_credential_enrollment("managed_auth_profile")
            .await
            .unwrap()
            .unwrap();
        let plan_json = serde_json::to_string(&plan).unwrap();
        let plan_debug = format!("{plan:?}");
        for raw in [
            enrollment.credential_reference.as_str(),
            enrollment.reference_digest.as_str(),
            enrollment.authority.provider_id.as_str(),
            enrollment.auth_schema_id.as_str(),
            enrollment
                .authority_evidence_digest
                .as_deref()
                .unwrap_or_default(),
            "profile_scope",
        ] {
            assert!(!plan_json.contains(raw));
            assert!(!plan_debug.contains(raw));
        }

        sqlx::query("DELETE FROM managed_credential_enrollments WHERE managed_mcp_id=?")
            .bind("managed_auth_profile")
            .execute(&repository.pool)
            .await
            .unwrap();
        assert_eq!(
            service
                .profile_apply_plan_create(
                    &context,
                    ProfileApplyPlanInput {
                        profile_id: profile.profile_id.clone(),
                        profile_revision: profile.revision,
                        idempotency_key: "auth-profile-plan-deleted".to_string(),
                    },
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
        assert_eq!(
            service
                .profile_apply_confirm(
                    &context,
                    &plan.plan_id,
                    &plan.confirmation.confirmation_token,
                    true,
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
        enroll_profile_managed(&service, &repository, "secret-2").await;
        let revision_drift_plan = service
            .profile_apply_plan_create(
                &context,
                ProfileApplyPlanInput {
                    profile_id: profile.profile_id.clone(),
                    profile_revision: profile.revision,
                    idempotency_key: "auth-profile-plan-revision".to_string(),
                },
            )
            .await
            .unwrap();
        sqlx::query("UPDATE managed_mcps SET revision=2 WHERE managed_mcp_id=?")
            .bind("managed_auth_profile")
            .execute(&repository.pool)
            .await
            .unwrap();
        assert_eq!(
            service
                .profile_apply_confirm(
                    &context,
                    &revision_drift_plan.plan_id,
                    &revision_drift_plan.confirmation.confirmation_token,
                    true,
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
        sqlx::query("UPDATE managed_mcps SET revision=1 WHERE managed_mcp_id=?")
            .bind("managed_auth_profile")
            .execute(&repository.pool)
            .await
            .unwrap();
        let consume_plan = service
            .profile_apply_plan_create(
                &context,
                ProfileApplyPlanInput {
                    profile_id: profile.profile_id.clone(),
                    profile_revision: profile.revision,
                    idempotency_key: "auth-profile-plan-consume".to_string(),
                },
            )
            .await
            .unwrap();
        let token = service
            .profile_apply_confirm(
                &context,
                &consume_plan.plan_id,
                &consume_plan.confirmation.confirmation_token,
                true,
            )
            .await
            .unwrap();

        sqlx::query("DELETE FROM managed_credential_enrollments WHERE managed_mcp_id=?")
            .bind("managed_auth_profile")
            .execute(&repository.pool)
            .await
            .unwrap();
        assert_eq!(
            service
                .consume_profile_application_token(&context, &token.token)
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
        enroll_profile_managed(&service, &repository, "secret-3").await;
        let stage_plan = service
            .profile_apply_plan_create(
                &context,
                ProfileApplyPlanInput {
                    profile_id: profile.profile_id.clone(),
                    profile_revision: profile.revision,
                    idempotency_key: "auth-profile-plan-stage".to_string(),
                },
            )
            .await
            .unwrap();
        let token = service
            .profile_apply_confirm(
                &context,
                &stage_plan.plan_id,
                &stage_plan.confirmation.confirmation_token,
                true,
            )
            .await
            .unwrap();
        let consumed = service
            .consume_profile_application_token(&context, &token.token)
            .await
            .unwrap();
        service
            .finish_profile_application(
                &consumed.application_id,
                Some("auth-session"),
                "created",
                "session_created",
            )
            .await
            .unwrap();
        let marker = ProfileApplicationMarker {
            application_id: consumed.application_id.clone(),
            profile_id: consumed.profile_id.clone(),
            profile_revision: consumed.profile_revision,
            merge_policy: "replace_managed_only".to_string(),
            original_session_id: "auth-session".to_string(),
            managed_extension_names: consumed.managed_extension_names().to_vec(),
        };

        let drift_manifest = parse_manifest(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "id": "auth-profile-test",
                "version": "1.0.1",
                "name": "Auth profile test",
                "description": "Credential evidence regression fixture.",
                "publisher": {"id": "test", "name": "Test"},
                "license": {"spdx": "MIT"},
                "capabilities": ["tools"],
                "permissions": [],
                "distribution": {"type": "remote_http"},
                "transport": {
                    "type": "streamable_http",
                    "url": "https://example.com/mcp",
                    "allowed_redirect_origins": []
                },
                "auth": {
                    "type": "api_key_header",
                    "header_name": "Authorization",
                    "prefix": "Bearer ",
                    "credential_name": "test-token"
                },
                "health_check": {"type": "mcp_initialize", "timeout_seconds": 30},
                "owned_files": [],
                "uninstall": {"mode": "remove_owned_files_only", "preserve_user_data": true}
            }))
            .unwrap(),
        )
        .unwrap();
        let drift_manifest_digest = drift_manifest.digest().to_string();
        repository
            .save_manifest(&ManifestRecord {
                verified: drift_manifest,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Local,
                source_metadata: Default::default(),
                created_at_ms: 2,
            })
            .await
            .unwrap();
        sqlx::query("UPDATE managed_mcps SET active_manifest_digest=? WHERE managed_mcp_id=?")
            .bind(&drift_manifest_digest)
            .bind("managed_auth_profile")
            .execute(&repository.pool)
            .await
            .unwrap();
        assert_eq!(
            service
                .hydrate_profile_application(&context, &marker, "auth-session")
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
        sqlx::query("UPDATE managed_mcps SET active_manifest_digest=? WHERE managed_mcp_id=?")
            .bind(&original_manifest_digest)
            .bind("managed_auth_profile")
            .execute(&repository.pool)
            .await
            .unwrap();
        let hydrated = service
            .hydrate_profile_application(&context, &marker, "auth-session")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE managed_credential_enrollments SET authority_evidence_digest=? WHERE managed_mcp_id=?",
        )
        .bind("0".repeat(64))
        .bind("managed_auth_profile")
        .execute(&repository.pool)
        .await
        .unwrap();
        assert_eq!(
            service
                .verify_profile_application_runtime(&context, &hydrated)
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PlanStale
        );
    }

    #[tokio::test]
    async fn legacy_profile_credential_references_are_ignored_but_enrollment_gate_still_fails_closed(
    ) {
        let repository = repository().await;
        let service = profile_service_with_enrollment_fixture(&repository).await;
        enroll_profile_managed(&service, &repository, "secret-1").await;
        let context =
            RequestContext::local_authenticated_client("profile-reference-regression".to_string());
        let profile = service
            .profile_create(
                &context,
                ProfileCreateInput {
                    name: "Credential drift profile".to_string(),
                    description: String::new(),
                    managed_mcp_ids: vec!["managed_auth_profile".to_string()],
                    idempotency_key: "credential-drift-profile-create".to_string(),
                },
            )
            .await
            .unwrap();
        let clean_plan = service
            .profile_apply_plan_create(
                &context,
                ProfileApplyPlanInput {
                    profile_id: profile.profile_id.clone(),
                    profile_revision: profile.revision,
                    idempotency_key: "credential-drift-clean-plan".to_string(),
                },
            )
            .await
            .unwrap();
        let tampered_references = vec![ProfileCredentialReference {
            managed_mcp_id: "managed_auth_profile".to_string(),
            credential_reference: "test-token".to_string(),
            reference_digest: "0".repeat(64),
        }];
        sqlx::query("UPDATE mcp_profiles SET credential_references_json=? WHERE profile_id=?")
            .bind(encode(&tampered_references).unwrap())
            .bind(&profile.profile_id)
            .execute(&repository.pool)
            .await
            .unwrap();
        let ignored_plan = service
            .profile_apply_plan_create(
                &context,
                ProfileApplyPlanInput {
                    profile_id: profile.profile_id.clone(),
                    profile_revision: profile.revision,
                    idempotency_key: "credential-drift-plan-create".to_string(),
                },
            )
            .await
            .unwrap();
        assert_eq!(ignored_plan.profile_id, profile.profile_id);
        let token = service
            .profile_apply_confirm(
                &context,
                &clean_plan.plan_id,
                &clean_plan.confirmation.confirmation_token,
                true,
            )
            .await
            .unwrap();
        assert_eq!(token.profile_id, profile.profile_id);

        sqlx::query("DELETE FROM managed_credential_enrollments WHERE managed_mcp_id=?")
            .bind("managed_auth_profile")
            .execute(&repository.pool)
            .await
            .unwrap();
        let create_error = service
            .profile_apply_plan_create(
                &context,
                ProfileApplyPlanInput {
                    profile_id: profile.profile_id,
                    profile_revision: profile.revision,
                    idempotency_key: "credential-drift-plan-create-second".to_string(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(create_error.code(), McpPlatformErrorCode::PlanStale);
    }

    #[tokio::test]
    async fn actual_entry_auth_and_policy_evidence_drift_invalidates_confirmation() {
        let repository = repository().await;
        let (profile, snapshot, original_state) = profile_with_managed_entry(&repository).await;
        let make_plan = |id: &str, snapshot: StoredProfileManagedSnapshot| {
            let mut plan = StoredProfileApplyPlan {
                plan_id: id.to_string(),
                profile_id: profile.profile_id.clone(),
                profile_revision: profile.revision,
                internal_plan_digest: String::new(),
                merge_policy: "replace_managed_only".to_string(),
                entries: vec![snapshot],
                actor: "local_authenticated_client".to_string(),
                expires_at_ms: 1_000,
                created_at_ms: 10,
            };
            plan.internal_plan_digest = profile_apply_plan_snapshot_digest(&plan).unwrap();
            plan
        };
        let policy_plan = make_plan("policy-plan", snapshot.clone());
        repository
            .save_profile_apply_plan(SaveProfileApplyPlan {
                plan: &policy_plan,
                idempotency_key: "policy-plan-key",
                request_digest: &"b".repeat(64),
            })
            .await
            .unwrap();
        let mut policy_drift = original_state.clone();
        policy_drift.tool_policies.get_mut("read").unwrap().decision = ToolPolicyDecision::Deny;
        sqlx::query("UPDATE managed_mcps SET state_json=? WHERE managed_mcp_id='managed_actual'")
            .bind(encode(&policy_drift).unwrap())
            .execute(&repository.pool)
            .await
            .unwrap();
        create_confirmation(&repository, &policy_plan, &"9".repeat(64), 19).await;
        let policy_error = repository
            .create_profile_application_token(SaveProfileApplicationToken {
                application_id: "policy-app",
                confirmation_hash: &"9".repeat(64),
                token_hash: &"c".repeat(64),
                plan_id: &policy_plan.plan_id,
                internal_plan_digest: policy_plan.internal_plan_digest(),
                actor: &policy_plan.actor,
                expires_at_ms: 500,
                created_at_ms: 20,
            })
            .await
            .unwrap_err();
        assert_eq!(policy_error.code(), McpPlatformErrorCode::PlanStale);

        sqlx::query("UPDATE managed_mcps SET state_json=? WHERE managed_mcp_id='managed_actual'")
            .bind(encode(&original_state).unwrap())
            .execute(&repository.pool)
            .await
            .unwrap();
        let consume_plan = make_plan("consume-plan", snapshot.clone());
        repository
            .save_profile_apply_plan(SaveProfileApplyPlan {
                plan: &consume_plan,
                idempotency_key: "consume-plan-key",
                request_digest: &"f".repeat(64),
            })
            .await
            .unwrap();
        let consume_hash = "1".repeat(64);
        create_confirmation(&repository, &consume_plan, &"8".repeat(64), 24).await;
        repository
            .create_profile_application_token(SaveProfileApplicationToken {
                application_id: "consume-app",
                confirmation_hash: &"8".repeat(64),
                token_hash: &consume_hash,
                plan_id: &consume_plan.plan_id,
                internal_plan_digest: consume_plan.internal_plan_digest(),
                actor: &consume_plan.actor,
                expires_at_ms: 500,
                created_at_ms: 25,
            })
            .await
            .unwrap();
        let mut consume_drift = original_state.clone();
        consume_drift.default_enabled = true;
        sqlx::query("UPDATE managed_mcps SET state_json=? WHERE managed_mcp_id='managed_actual'")
            .bind(encode(&consume_drift).unwrap())
            .execute(&repository.pool)
            .await
            .unwrap();
        let consume_error = repository
            .consume_profile_application_token(&consume_hash, "local_authenticated_client", 26)
            .await
            .unwrap_err();
        assert_eq!(consume_error.code(), McpPlatformErrorCode::PlanStale);
        sqlx::query("UPDATE managed_mcps SET state_json=? WHERE managed_mcp_id='managed_actual'")
            .bind(encode(&original_state).unwrap())
            .execute(&repository.pool)
            .await
            .unwrap();
        let auth_plan = make_plan("auth-plan", snapshot);
        repository
            .save_profile_apply_plan(SaveProfileApplyPlan {
                plan: &auth_plan,
                idempotency_key: "auth-plan-key",
                request_digest: &"d".repeat(64),
            })
            .await
            .unwrap();
        let mut auth_drift = original_state;
        auth_drift.health = HealthState::BlockedAuth;
        sqlx::query("UPDATE managed_mcps SET state_json=? WHERE managed_mcp_id='managed_actual'")
            .bind(encode(&auth_drift).unwrap())
            .execute(&repository.pool)
            .await
            .unwrap();
        create_confirmation(&repository, &auth_plan, &"7".repeat(64), 29).await;
        let auth_error = repository
            .create_profile_application_token(SaveProfileApplicationToken {
                application_id: "auth-app",
                confirmation_hash: &"7".repeat(64),
                token_hash: &"e".repeat(64),
                plan_id: &auth_plan.plan_id,
                internal_plan_digest: auth_plan.internal_plan_digest(),
                actor: &auth_plan.actor,
                expires_at_ms: 500,
                created_at_ms: 30,
            })
            .await
            .unwrap_err();
        assert_eq!(auth_error.code(), McpPlatformErrorCode::PlanStale);
    }

    #[tokio::test]
    async fn profile_crud_history_cas_and_idempotency_are_independent() {
        let repository = repository().await;
        let created = empty_profile(&repository).await;
        let replay = empty_profile(&repository).await;
        assert_eq!(created, replay);

        let conflict = repository
            .create_profile(SaveProfile {
                profile_id: "profile_other",
                name: "Other",
                description: "safe",
                entries: &[],
                idempotency_key: "create-key",
                request_digest: &"c".repeat(64),
                actor: "local_authenticated_client",
                now_ms: 11,
            })
            .await
            .unwrap_err();
        assert_eq!(conflict.code(), McpPlatformErrorCode::IdempotencyConflict);

        let updated = repository
            .change_profile(ChangeProfile {
                profile_id: &created.profile_id,
                expected_revision: 1,
                name: "Updated",
                description: "safe",
                entries: &[],
                archived: false,
                operation: "update",
                idempotency_key: "update-key",
                request_digest: &"d".repeat(64),
                actor: "local_authenticated_client",
                now_ms: 12,
            })
            .await
            .unwrap();
        assert_eq!(updated.revision, 2);
        let stale = repository
            .change_profile(ChangeProfile {
                profile_id: &created.profile_id,
                expected_revision: 1,
                name: "Stale",
                description: "safe",
                entries: &[],
                archived: false,
                operation: "update",
                idempotency_key: "stale-key",
                request_digest: &"e".repeat(64),
                actor: "local_authenticated_client",
                now_ms: 13,
            })
            .await
            .unwrap_err();
        assert_eq!(stale.code(), McpPlatformErrorCode::RevisionConflict);
        let history = repository
            .list_profile_revisions(&created.profile_id)
            .await
            .unwrap();
        assert_eq!(
            history.iter().map(|item| item.revision).collect::<Vec<_>>(),
            [2, 1]
        );
        assert!(!repository
            .diagnostics()
            .await
            .unwrap()
            .tables
            .iter()
            .any(|table| table == "sessions"));
    }

    #[tokio::test]
    async fn application_token_is_hash_only_bound_expiring_single_use_and_drift_safe() {
        let repository = repository().await;
        let profile = empty_profile(&repository).await;
        let plan = plan(&profile, 100);
        repository
            .save_profile_apply_plan(SaveProfileApplyPlan {
                plan: &plan,
                idempotency_key: "plan-key",
                request_digest: &"f".repeat(64),
            })
            .await
            .unwrap();
        let token_hash = "1".repeat(64);
        create_confirmation(&repository, &plan, &"2".repeat(64), 109).await;
        repository
            .create_profile_application_token(SaveProfileApplicationToken {
                application_id: "app_ok",
                confirmation_hash: &"2".repeat(64),
                token_hash: &token_hash,
                plan_id: &plan.plan_id,
                internal_plan_digest: plan.internal_plan_digest(),
                actor: &plan.actor,
                expires_at_ms: 500,
                created_at_ms: 110,
            })
            .await
            .unwrap();
        let stored = sqlx::query("SELECT token_hash FROM mcp_profile_application_tokens")
            .fetch_one(&repository.pool)
            .await
            .unwrap();
        assert_eq!(
            stored.try_get::<String, _>("token_hash").unwrap(),
            token_hash
        );
        assert!(!repository
            .diagnostics()
            .await
            .unwrap()
            .tables
            .iter()
            .any(|table| table.contains("audit_events") && table.contains("profile")));

        let actor_mismatch = repository
            .consume_profile_application_token(&token_hash, "other_actor", 120)
            .await
            .unwrap_err();
        assert_eq!(actor_mismatch.code(), McpPlatformErrorCode::PlanStale);
        let consumed = repository
            .consume_profile_application_token(&token_hash, "local_authenticated_client", 120)
            .await
            .unwrap();
        assert_eq!(consumed.profile_revision, 1);
        repository
            .finish_profile_application(
                "app_ok",
                Some("session_failed"),
                "failed",
                "session_setup_failed",
                "local_authenticated_client",
                121,
            )
            .await
            .unwrap();

        let create_failure_hash = "6".repeat(64);
        create_confirmation(&repository, &plan, &"3".repeat(64), 124).await;
        repository
            .create_profile_application_token(SaveProfileApplicationToken {
                application_id: "app_create_failure",
                confirmation_hash: &"3".repeat(64),
                token_hash: &create_failure_hash,
                plan_id: &plan.plan_id,
                internal_plan_digest: plan.internal_plan_digest(),
                actor: &plan.actor,
                expires_at_ms: 600,
                created_at_ms: 125,
            })
            .await
            .unwrap();
        repository
            .consume_profile_application_token(
                &create_failure_hash,
                "local_authenticated_client",
                126,
            )
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_application_failed BEFORE INSERT ON mcp_profile_application_events WHEN NEW.application_id='app_create_failure' AND NEW.event_type='application_failed' BEGIN SELECT RAISE(ABORT, 'injected application state failure'); END",
        )
        .execute(&repository.pool)
        .await
        .unwrap();
        assert!(repository
            .finish_profile_application(
                "app_create_failure",
                None,
                "failed",
                "session_create_failed",
                "local_authenticated_client",
                127,
            )
            .await
            .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM mcp_profile_applications WHERE application_id='app_create_failure'",
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            "consumed"
        );
        sqlx::query("DROP TRIGGER fail_application_failed")
            .execute(&repository.pool)
            .await
            .unwrap();
        repository
            .finish_profile_application(
                "app_create_failure",
                None,
                "failed",
                "session_create_failed",
                "local_authenticated_client",
                127,
            )
            .await
            .unwrap();
        let create_failure = sqlx::query("SELECT status,session_id,failure_code FROM mcp_profile_applications WHERE application_id='app_create_failure'")
            .fetch_one(&repository.pool)
            .await
            .unwrap();
        assert_eq!(
            create_failure.try_get::<String, _>("status").unwrap(),
            "failed"
        );
        assert!(create_failure
            .try_get::<Option<String>, _>("session_id")
            .unwrap()
            .is_none());
        assert_eq!(
            create_failure.try_get::<String, _>("failure_code").unwrap(),
            "session_create_failed"
        );
        repository
            .finish_profile_application(
                "app_create_failure",
                None,
                "rolled_back",
                "session_cleanup_complete",
                "local_authenticated_client",
                128,
            )
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM mcp_profile_applications WHERE application_id='app_create_failure'")
                .fetch_one(&repository.pool)
                .await
                .unwrap(),
            "rolled_back"
        );
        repository
            .finish_profile_application(
                "app_create_failure",
                None,
                "rolled_back",
                "session_cleanup_complete",
                "local_authenticated_client",
                129,
            )
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM mcp_profile_application_events WHERE application_id='app_create_failure'",
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            4
        );
        repository
            .finish_profile_application(
                "app_ok",
                Some("session_failed"),
                "rolled_back",
                "session_cleanup_complete",
                "local_authenticated_client",
                122,
            )
            .await
            .unwrap();
        let application = sqlx::query(
            "SELECT status,failure_code FROM mcp_profile_applications WHERE application_id='app_ok'",
        )
        .fetch_one(&repository.pool)
        .await
        .unwrap();
        assert_eq!(
            application.try_get::<String, _>("status").unwrap(),
            "rolled_back"
        );
        assert_eq!(
            application.try_get::<String, _>("failure_code").unwrap(),
            "session_cleanup_complete"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM mcp_profile_application_events WHERE application_id='app_ok'",
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            4
        );
        let replay = repository
            .consume_profile_application_token(&token_hash, "local_authenticated_client", 121)
            .await
            .unwrap_err();
        assert_eq!(replay.code(), McpPlatformErrorCode::PlanStale);
        assert!(!replay.to_string().contains(&token_hash));

        let expired_hash = "2".repeat(64);
        create_confirmation(&repository, &plan, &"4".repeat(64), 119).await;
        repository
            .create_profile_application_token(SaveProfileApplicationToken {
                application_id: "app_expired",
                confirmation_hash: &"4".repeat(64),
                token_hash: &expired_hash,
                plan_id: &plan.plan_id,
                internal_plan_digest: plan.internal_plan_digest(),
                actor: &plan.actor,
                expires_at_ms: 130,
                created_at_ms: 120,
            })
            .await
            .unwrap();
        let expired = repository
            .consume_profile_application_token(&expired_hash, "local_authenticated_client", 130)
            .await
            .unwrap_err();
        assert_eq!(expired.code(), McpPlatformErrorCode::PlanExpired);

        let delete_hash = "5".repeat(64);
        create_confirmation(&repository, &plan, &"5".repeat(64), 124).await;
        repository
            .create_profile_application_token(SaveProfileApplicationToken {
                application_id: "app_delete",
                confirmation_hash: &"5".repeat(64),
                token_hash: &delete_hash,
                plan_id: &plan.plan_id,
                internal_plan_digest: plan.internal_plan_digest(),
                actor: &plan.actor,
                expires_at_ms: 600,
                created_at_ms: 125,
            })
            .await
            .unwrap();
        repository
            .consume_profile_application_token(&delete_hash, "local_authenticated_client", 126)
            .await
            .unwrap();
        repository
            .finish_profile_application(
                "app_delete",
                Some("original_session"),
                "created",
                "session_created",
                "local_authenticated_client",
                127,
            )
            .await
            .unwrap();
        assert!(repository
            .finish_profile_application(
                "app_delete",
                Some("copied_session"),
                "deleted",
                "session_deleted",
                "local_authenticated_client",
                128,
            )
            .await
            .is_err());
        assert!(repository
            .finish_profile_application(
                "app_delete",
                Some("original_session"),
                "deleted",
                "session_deleted",
                "other_actor",
                128,
            )
            .await
            .is_err());
        repository
            .finish_profile_application(
                "app_delete",
                Some("original_session"),
                "recovery_required",
                "session_delete_started",
                "local_authenticated_client",
                129,
            )
            .await
            .unwrap();
        assert_eq!(
            repository
                .get_profile_application_cleanup_state(
                    "original_session",
                    "local_authenticated_client",
                )
                .await
                .unwrap(),
            Some(("app_delete".to_string(), "recovery_required".to_string()))
        );
        repository
            .finish_profile_application(
                "app_delete",
                Some("original_session"),
                "recovery_required",
                "agent_cleanup_failed",
                "local_authenticated_client",
                130,
            )
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT failure_code FROM mcp_profile_applications WHERE application_id='app_delete'",
            )
            .fetch_one(&repository.pool)
            .await
            .unwrap(),
            "agent_cleanup_failed"
        );
        repository
            .finish_profile_application(
                "app_delete",
                Some("original_session"),
                "recovery_required",
                "session_delete_started",
                "local_authenticated_client",
                131,
            )
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_session_deleted BEFORE INSERT ON mcp_profile_application_events WHEN NEW.application_id='app_delete' AND NEW.event_type='session_deleted' BEGIN SELECT RAISE(ABORT, 'injected delete state failure'); END",
        )
        .execute(&repository.pool)
        .await
        .unwrap();
        assert!(repository
            .finish_profile_application(
                "app_delete",
                Some("original_session"),
                "deleted",
                "session_deleted",
                "local_authenticated_client",
                132,
            )
            .await
            .is_err());
        assert_eq!(
            repository
                .get_profile_application_cleanup_state(
                    "original_session",
                    "local_authenticated_client",
                )
                .await
                .unwrap(),
            Some(("app_delete".to_string(), "recovery_required".to_string()))
        );
        sqlx::query("DROP TRIGGER fail_session_deleted")
            .execute(&repository.pool)
            .await
            .unwrap();
        repository
            .finish_profile_application(
                "app_delete",
                Some("original_session"),
                "deleted",
                "session_deleted",
                "local_authenticated_client",
                133,
            )
            .await
            .unwrap();
        repository
            .finish_profile_application(
                "app_delete",
                Some("original_session"),
                "deleted",
                "session_deleted",
                "local_authenticated_client",
                134,
            )
            .await
            .unwrap();

        repository
            .change_profile(ChangeProfile {
                profile_id: &profile.profile_id,
                expected_revision: 1,
                name: "Drifted",
                description: "safe",
                entries: &[],
                archived: false,
                operation: "update",
                idempotency_key: "drift-key",
                request_digest: &"3".repeat(64),
                actor: "local_authenticated_client",
                now_ms: 140,
            })
            .await
            .unwrap();
        let drift_hash = "4".repeat(64);
        create_confirmation(&repository, &plan, &"6".repeat(64), 149).await;
        let drift = repository
            .create_profile_application_token(SaveProfileApplicationToken {
                application_id: "app_drift",
                confirmation_hash: &"6".repeat(64),
                token_hash: &drift_hash,
                plan_id: &plan.plan_id,
                internal_plan_digest: plan.internal_plan_digest(),
                actor: &plan.actor,
                expires_at_ms: 600,
                created_at_ms: 150,
            })
            .await
            .unwrap_err();
        assert_eq!(drift.code(), McpPlatformErrorCode::PlanStale);
    }
}
