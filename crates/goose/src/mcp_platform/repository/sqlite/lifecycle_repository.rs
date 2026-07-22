use sqlx::Row;

use crate::mcp_platform::domain::{InstallationState, ManagedMcpState, RegistrationState};
use crate::mcp_platform::error::{McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::repository::{
    ActivateManagedInstallation, AuditEventType, AuditPayload, ConnectionProjectionRecord,
    CreateHealthTask, HealthObservationRecord, HealthTaskRequestRecord, ManagedMcpInventoryRecord,
    ManagedMcpLifecycleMetadata, ManagedUninstallSnapshot, NewHealthObservation,
    ProjectionMutationRecord, ProjectionMutationStatus, PutOwnedProjection, RegisterManagedMcp,
    RegisterManagedMcpOutcome, RestoreOwnedProjection, RetryAttemptRecord,
    StageManagedInstallation, StageManagedInstallationOutcome, TaskRecord,
};
use crate::mcp_platform::task::{TaskOperation, TaskStatus};
use crate::mcp_platform::SupplyChainEvidence;

use super::records::{
    append_audit, decode, decode_database_enum, decode_task_row, encode, error, fetch_task,
    fetch_valid_plan, integrity_error, map_sqlx, not_found, NewAuditEvent,
};
use super::SqliteMcpPlatformRepository;

impl SqliteMcpPlatformRepository {
    pub async fn claim_artifact(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
        stale_before_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT owner_task_id,status,updated_at_ms FROM artifact_claims WHERE artifact_digest = ?")
            .bind(artifact_digest).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
        match row {
            None => {
                sqlx::query("INSERT INTO artifact_claims(artifact_digest,owner_task_id,status,updated_at_ms) VALUES (?,?,'fetching',?)")
                    .bind(artifact_digest).bind(task_id).bind(now_ms).execute(&mut *tx).await.map_err(map_sqlx)?;
            }
            Some(row) => {
                let owner: String = row.try_get("owner_task_id").map_err(map_sqlx)?;
                let status: String = row.try_get("status").map_err(map_sqlx)?;
                let updated: i64 = row.try_get("updated_at_ms").map_err(map_sqlx)?;
                if owner != task_id && status != "released" && updated >= stale_before_ms {
                    return Err(crate::mcp_platform::error::McpPlatformError::new(
                        McpPlatformErrorCode::PlanConflict,
                        "managed artifact is claimed by another lifecycle task",
                    ));
                }
                sqlx::query("UPDATE artifact_claims SET owner_task_id = ?,status = 'fetching',updated_at_ms = ? WHERE artifact_digest = ?")
                    .bind(task_id).bind(now_ms).bind(artifact_digest).execute(&mut *tx).await.map_err(map_sqlx)?;
            }
        }
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn mark_artifact_claim_verified(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let result = sqlx::query("UPDATE artifact_claims SET status = 'verified',updated_at_ms = ? WHERE artifact_digest = ? AND owner_task_id = ? AND status IN ('fetching','verified')")
            .bind(now_ms).bind(artifact_digest).bind(task_id).execute(&self.pool).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        Ok(())
    }

    pub async fn release_artifact_claim(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let result = sqlx::query("UPDATE artifact_claims SET status = 'released',updated_at_ms = ? WHERE artifact_digest = ? AND owner_task_id = ? AND status IN ('fetching','verified','released')")
            .bind(now_ms).bind(artifact_digest).bind(task_id).execute(&self.pool).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        Ok(())
    }

    pub async fn mark_managed_runtime_activated(
        &self,
        managed_mcp_id: &str,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let result = sqlx::query("UPDATE activation_journal SET status = 'pointer_committed',updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ? AND target_version = ? AND status IN ('started','pointer_committed')")
            .bind(now_ms).bind(managed_mcp_id).bind(task_id).bind(target_version).execute(&self.pool).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        Ok(())
    }

    pub async fn finalize_retained_version_cleanup(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?",
        )
        .bind(managed_mcp_id)
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        if status == "cleanup_committed" {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        if status != "health_committed" {
            return Err(integrity_error());
        }
        let row = sqlx::query("SELECT artifact_digest,active,activation_state FROM managed_versions WHERE managed_mcp_id = ? AND version = ?")
            .bind(managed_mcp_id).bind(version).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
        let Some(row) = row else {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        };
        if row.try_get::<bool, _>("active").map_err(map_sqlx)?
            || row
                .try_get::<String, _>("activation_state")
                .map_err(map_sqlx)?
                != "retained"
        {
            return Err(integrity_error());
        }
        let owned = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM installation_ownership WHERE managed_mcp_id = ? AND version = ? AND remove_on_uninstall = 1)")
            .bind(managed_mcp_id).bind(version).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
        if !owned {
            return Err(integrity_error());
        }
        let digest: Option<String> = row.try_get("artifact_digest").map_err(map_sqlx)?;
        sqlx::query("DELETE FROM installation_ownership WHERE managed_mcp_id = ? AND version = ?")
            .bind(managed_mcp_id)
            .bind(version)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query(
            "DELETE FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 0",
        )
        .bind(managed_mcp_id)
        .bind(version)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        if let Some(digest) = digest {
            sqlx::query("UPDATE artifact_cache SET reference_count = reference_count - 1 WHERE artifact_digest = ? AND reference_count > 0").bind(digest).execute(&mut *tx).await.map_err(map_sqlx)?;
        }
        sqlx::query("UPDATE activation_journal SET status = 'cleanup_committed',updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ? AND status = 'health_committed'")
            .bind(now_ms).bind(managed_mcp_id).bind(task_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn is_managed_finalizing(
        &self,
        task_id: &str,
        operation: TaskOperation,
    ) -> McpPlatformResult<bool> {
        match operation {
            TaskOperation::Uninstall => Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM uninstall_journal WHERE task_id = ? AND status = 'committed')").bind(task_id).fetch_one(&self.pool).await.map_err(map_sqlx)?),
            TaskOperation::Update => Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM activation_journal WHERE task_id = ? AND status = 'cleanup_committed')").bind(task_id).fetch_one(&self.pool).await.map_err(map_sqlx)?),
            TaskOperation::Repair => Ok(sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM activation_journal a JOIN task_steps s ON s.task_id = a.task_id WHERE a.task_id = ? AND a.status = 'health_committed' AND s.ordinal >= 10 AND s.status = 'started')").bind(task_id).fetch_one(&self.pool).await.map_err(map_sqlx)?),
            _ => Ok(false),
        }
    }

    pub async fn record_managed_finalization_failure(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<i64> {
        let mut tx = self.begin_immediate().await?;
        sqlx::query(
            "UPDATE tasks SET finalization_failures = finalization_failures + 1, updated_at_ms = ? WHERE task_id = ?",
        )
        .bind(now_ms)
        .bind(task_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let failures = sqlx::query_scalar::<_, i64>(
            "SELECT finalization_failures FROM tasks WHERE task_id = ?",
        )
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(failures)
    }

    pub async fn stage_managed_installation(
        &self,
        input: StageManagedInstallation<'_>,
    ) -> McpPlatformResult<StageManagedInstallationOutcome> {
        let mut tx = self.begin_immediate().await?;
        let lease_owner = sqlx::query_scalar::<_, String>(
            "SELECT task_id FROM managed_lifecycle_leases WHERE managed_mcp_id = ?",
        )
        .bind(input.managed_mcp_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        if lease_owner.as_deref() != Some(input.task_id) {
            return Err(crate::mcp_platform::error::McpPlatformError::new(
                McpPlatformErrorCode::PlanConflict,
                "managed MCP lifecycle lease is not owned by this task",
            ));
        }
        let manifest =
            sqlx::query("SELECT mcp_id, version FROM manifest_blobs WHERE manifest_digest = ?")
                .bind(input.manifest_digest)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx)?
                .ok_or_else(not_found)?;
        if manifest.try_get::<String, _>("mcp_id").map_err(map_sqlx)? != input.mcp_id
            || manifest.try_get::<String, _>("version").map_err(map_sqlx)? != input.version
        {
            return Err(integrity_error());
        }
        if let Some(journal) = sqlx::query("SELECT previous_version,target_version FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?")
            .bind(input.managed_mcp_id).bind(input.task_id).fetch_optional(&mut *tx).await.map_err(map_sqlx)?
        {
            let previous_version: Option<String> = journal.try_get("previous_version").map_err(map_sqlx)?;
            if journal.try_get::<String, _>("target_version").map_err(map_sqlx)? != input.version { return Err(integrity_error()); }
            let managed = sqlx::query("SELECT mcp_id,installation_scope,distribution_adapter,owner_task_id FROM managed_mcps WHERE managed_mcp_id = ?")
                .bind(input.managed_mcp_id).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
            if managed.try_get::<String, _>("mcp_id").map_err(map_sqlx)? != input.mcp_id
                || managed.try_get::<String, _>("installation_scope").map_err(map_sqlx)? != input.installation_scope
                || managed.try_get::<String, _>("distribution_adapter").map_err(map_sqlx)? != input.distribution_adapter
            { return Err(integrity_error()); }
            let version = sqlx::query("SELECT manifest_digest,artifact_digest,materialized_tree_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ?")
                .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
            if version.try_get::<String, _>("manifest_digest").map_err(map_sqlx)? != input.manifest_digest
                || version.try_get::<Option<String>, _>("artifact_digest").map_err(map_sqlx)?.as_deref() != Some(&input.verification_evidence.artifact_digest)
                || version.try_get::<Option<String>, _>("materialized_tree_digest").map_err(map_sqlx)?.as_deref() != Some(input.materialized_tree_digest)
            { return Err(integrity_error()); }
            let created_managed_mcp = previous_version.is_none() && managed.try_get::<Option<String>, _>("owner_task_id").map_err(map_sqlx)?.as_deref() == Some(input.task_id);
            let created_version = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM installation_ownership WHERE managed_mcp_id = ? AND version = ? AND owner_task_id = ?)")
                .bind(input.managed_mcp_id).bind(input.version).bind(input.task_id).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(StageManagedInstallationOutcome { record: self.get_managed_inventory(input.managed_mcp_id).await?, created_managed_mcp, created_version, previous_version });
        }
        let existing = sqlx::query("SELECT managed_mcp_id,state_json,active_version,distribution_adapter FROM managed_mcps WHERE mcp_id = ? AND installation_scope = ?")
            .bind(input.mcp_id).bind(input.installation_scope).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
        let created_managed_mcp = existing.is_none();
        let (previous_version, mut state) = if let Some(row) = existing {
            if row
                .try_get::<String, _>("managed_mcp_id")
                .map_err(map_sqlx)?
                != input.managed_mcp_id
                || row
                    .try_get::<String, _>("distribution_adapter")
                    .map_err(map_sqlx)?
                    != input.distribution_adapter
            {
                return Err(integrity_error());
            }
            (
                row.try_get::<Option<String>, _>("active_version")
                    .map_err(map_sqlx)?,
                decode::<ManagedMcpState>(
                    &row.try_get::<String, _>("state_json").map_err(map_sqlx)?,
                )?,
            )
        } else {
            let state = ManagedMcpState {
                registration: RegistrationState::Registered,
                ..ManagedMcpState::default()
            };
            sqlx::query(r#"INSERT INTO managed_mcps (managed_mcp_id,mcp_id,installation_scope,state_json,revision,created_at_ms,updated_at_ms,distribution_adapter,active_manifest_digest,active_version,owner_task_id) VALUES (?,?,?,?,0,?,?,?,NULL,NULL,?)"#)
                .bind(input.managed_mcp_id).bind(input.mcp_id).bind(input.installation_scope).bind(encode(&state)?).bind(input.now_ms).bind(input.now_ms).bind(input.distribution_adapter).bind(input.task_id)
                .execute(&mut *tx).await.map_err(map_sqlx)?;
            (None, state)
        };
        let version_exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM managed_versions WHERE managed_mcp_id = ? AND version = ?)")
            .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
        if !version_exists {
            sqlx::query(r#"INSERT INTO managed_versions (managed_mcp_id,version,manifest_digest,installation_root,verified,active,adapter_evidence_json,created_at_ms,artifact_digest,verification_evidence_json,materialized_tree_digest,activation_state,supply_chain_evidence_json) VALUES (?,?,?,?,1,0,?,?,?,?,?, 'staged',?)"#)
                .bind(input.managed_mcp_id).bind(input.version).bind(input.manifest_digest).bind(input.installation_root).bind(encode(input.adapter_evidence)?).bind(input.now_ms).bind(&input.verification_evidence.artifact_digest).bind(encode(input.verification_evidence)?).bind(input.materialized_tree_digest).bind(input.supply_chain_evidence.map(encode).transpose()?)
                .execute(&mut *tx).await.map_err(map_sqlx)?;
            for path in input.owned_relative_paths {
                let expected_digest = (path == ".").then_some(input.materialized_tree_digest);
                sqlx::query(r#"INSERT INTO installation_ownership (managed_mcp_id,version,relative_path,path_kind,expected_digest,owner_task_id,remove_on_uninstall) VALUES (?,?,?,'directory',?,?,1)"#)
                    .bind(input.managed_mcp_id).bind(input.version).bind(path).bind(expected_digest).bind(input.task_id).execute(&mut *tx).await.map_err(map_sqlx)?;
            }
            sqlx::query(r#"INSERT INTO artifact_cache (artifact_digest,source_origin,size_bytes,adapter_id,adapter_version,platform_selector,verification_evidence_json,reference_count,verified_at_ms) VALUES (?,?,?,?,?,?,?,1,?) ON CONFLICT(artifact_digest) DO UPDATE SET reference_count = reference_count + 1, verification_evidence_json = excluded.verification_evidence_json, verified_at_ms = excluded.verified_at_ms"#)
                .bind(&input.verification_evidence.artifact_digest).bind(&input.verification_evidence.source_origin).bind(i64::try_from(input.verification_evidence.size_bytes).map_err(|_| integrity_error())?).bind(&input.verification_evidence.adapter_id).bind(&input.verification_evidence.adapter_version).bind(&input.verification_evidence.platform_selector).bind(encode(input.verification_evidence)?).bind(input.now_ms)
                .execute(&mut *tx).await.map_err(map_sqlx)?;
        } else if previous_version.as_deref() == Some(input.version) {
            let existing_digest = sqlx::query_scalar::<_, Option<String>>("SELECT artifact_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 1")
                .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
            if existing_digest.as_deref() != Some(&input.verification_evidence.artifact_digest) {
                return Err(integrity_error());
            }
            let persisted_evidence = sqlx::query_scalar::<_, Option<String>>("SELECT supply_chain_evidence_json FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 1")
                .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
            if !same_supply_chain_authority(
                persisted_evidence.as_deref(),
                input.supply_chain_evidence,
            )? {
                return Err(integrity_error());
            }
            sqlx::query("UPDATE managed_versions SET installation_root = ?,verified = 1,adapter_evidence_json = ?,verification_evidence_json = ?,materialized_tree_digest = ?,activation_state = 'staged' WHERE managed_mcp_id = ? AND version = ? AND active = 1")
                .bind(input.installation_root).bind(encode(input.adapter_evidence)?).bind(encode(input.verification_evidence)?).bind(input.materialized_tree_digest).bind(input.managed_mcp_id).bind(input.version).execute(&mut *tx).await.map_err(map_sqlx)?;
            sqlx::query("UPDATE installation_ownership SET expected_digest = ? WHERE managed_mcp_id = ? AND version = ? AND relative_path = '.'")
                .bind(input.materialized_tree_digest).bind(input.managed_mcp_id).bind(input.version).execute(&mut *tx).await.map_err(map_sqlx)?;
        } else {
            let existing = sqlx::query("SELECT manifest_digest,artifact_digest,materialized_tree_digest,supply_chain_evidence_json,verified FROM managed_versions WHERE managed_mcp_id = ? AND version = ?")
                .bind(input.managed_mcp_id).bind(input.version).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
            if existing
                .try_get::<String, _>("manifest_digest")
                .map_err(map_sqlx)?
                != input.manifest_digest
                || existing
                    .try_get::<Option<String>, _>("artifact_digest")
                    .map_err(map_sqlx)?
                    .as_deref()
                    != Some(&input.verification_evidence.artifact_digest)
                || !existing.try_get::<bool, _>("verified").map_err(map_sqlx)?
                || existing
                    .try_get::<Option<String>, _>("materialized_tree_digest")
                    .map_err(map_sqlx)?
                    .as_deref()
                    != Some(input.materialized_tree_digest)
                || !same_supply_chain_authority(
                    existing
                        .try_get::<Option<String>, _>("supply_chain_evidence_json")
                        .map_err(map_sqlx)?
                        .as_deref(),
                    input.supply_chain_evidence,
                )?
            {
                return Err(integrity_error());
            }
            sqlx::query("UPDATE managed_versions SET activation_state = 'staged' WHERE managed_mcp_id = ? AND version = ?")
                .bind(input.managed_mcp_id).bind(input.version).execute(&mut *tx).await.map_err(map_sqlx)?;
        }
        let previous_state_json = encode(&state)?;
        state.installation = InstallationState::Staged;
        state.default_enabled = false;
        sqlx::query("UPDATE managed_mcps SET state_json = ?, updated_at_ms = ?, revision = revision + 1 WHERE managed_mcp_id = ?")
            .bind(encode(&state)?).bind(input.now_ms).bind(input.managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        let existing_activation = sqlx::query("SELECT previous_version,target_version,previous_state_json FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?")
            .bind(input.managed_mcp_id).bind(input.task_id).fetch_optional(&mut *tx).await.map_err(map_sqlx)?;
        if let Some(row) = existing_activation {
            if row
                .try_get::<Option<String>, _>("previous_version")
                .map_err(map_sqlx)?
                != previous_version
                || row
                    .try_get::<String, _>("target_version")
                    .map_err(map_sqlx)?
                    != input.version
                || row
                    .try_get::<String, _>("previous_state_json")
                    .map_err(map_sqlx)?
                    != previous_state_json
            {
                return Err(integrity_error());
            }
        } else {
            sqlx::query("INSERT INTO activation_journal(managed_mcp_id,task_id,previous_version,target_version,previous_state_json,status,created_at_ms,updated_at_ms) VALUES (?,?,?,?,?,'started',?,?)")
                .bind(input.managed_mcp_id).bind(input.task_id).bind(&previous_version).bind(input.version).bind(previous_state_json).bind(input.now_ms).bind(input.now_ms).execute(&mut *tx).await.map_err(map_sqlx)?;
        }
        tx.commit().await.map_err(map_sqlx)?;
        Ok(StageManagedInstallationOutcome {
            record: self.get_managed_inventory(input.managed_mcp_id).await?,
            created_managed_mcp,
            created_version: !version_exists,
            previous_version,
        })
    }

    pub async fn activate_managed_installation(
        &self,
        input: ActivateManagedInstallation<'_>,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        let mut tx = self.begin_immediate().await?;
        let journal = sqlx::query("SELECT previous_version,target_version,status FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?")
            .bind(input.managed_mcp_id).bind(input.task_id).fetch_optional(&mut *tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        if journal
            .try_get::<String, _>("target_version")
            .map_err(map_sqlx)?
            != input.target_version
        {
            return Err(integrity_error());
        }
        let status: String = journal.try_get("status").map_err(map_sqlx)?;
        if status == "health_committed" {
            tx.commit().await.map_err(map_sqlx)?;
            return self.get_managed_inventory(input.managed_mcp_id).await;
        }
        if status != "pointer_committed" {
            return Err(integrity_error());
        }
        let target = sqlx::query("SELECT manifest_digest,activation_state FROM managed_versions WHERE managed_mcp_id = ? AND version = ?")
            .bind(input.managed_mcp_id).bind(input.target_version).fetch_optional(&mut *tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        if target
            .try_get::<String, _>("activation_state")
            .map_err(map_sqlx)?
            != "staged"
        {
            return Err(integrity_error());
        }
        let previous_version = journal
            .try_get::<Option<String>, _>("previous_version")
            .map_err(map_sqlx)?;
        if let Some(previous) = previous_version
            .as_deref()
            .filter(|previous| *previous != input.target_version)
        {
            sqlx::query("UPDATE managed_versions SET active = 0, activation_state = 'retained' WHERE managed_mcp_id = ? AND version = ? AND active = 1")
                .bind(input.managed_mcp_id).bind(previous).execute(&mut *tx).await.map_err(map_sqlx)?;
        }
        sqlx::query("UPDATE managed_versions SET active = 1, activation_state = 'active' WHERE managed_mcp_id = ? AND version = ?")
            .bind(input.managed_mcp_id).bind(input.target_version).execute(&mut *tx).await.map_err(map_sqlx)?;
        let state_json = sqlx::query_scalar::<_, String>(
            "SELECT state_json FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(input.managed_mcp_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let mut state = decode::<ManagedMcpState>(&state_json)?;
        state.installation = InstallationState::Installed;
        state.default_enabled = input.default_enabled;
        let manifest_digest: String = target.try_get("manifest_digest").map_err(map_sqlx)?;
        sqlx::query("UPDATE managed_mcps SET state_json = ?, active_manifest_digest = ?, active_version = ?, updated_at_ms = ?, revision = revision + 1 WHERE managed_mcp_id = ?")
            .bind(encode(&state)?).bind(manifest_digest).bind(input.target_version).bind(input.now_ms).bind(input.managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query("UPDATE activation_journal SET status = 'health_committed', updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ? AND status = 'pointer_committed'")
            .bind(input.now_ms).bind(input.managed_mcp_id).bind(input.task_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_managed_inventory(input.managed_mcp_id).await
    }

    pub async fn rollback_managed_installation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let journal = sqlx::query("SELECT previous_version,target_version,previous_state_json,status FROM activation_journal WHERE managed_mcp_id = ? AND task_id = ?")
            .bind(managed_mcp_id).bind(task_id).fetch_optional(&mut *tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        if journal
            .try_get::<Option<String>, _>("previous_version")
            .map_err(map_sqlx)?
            .as_deref()
            != previous_version
            || journal
                .try_get::<String, _>("target_version")
                .map_err(map_sqlx)?
                != target_version
        {
            return Err(integrity_error());
        }
        if journal.try_get::<String, _>("status").map_err(map_sqlx)? == "rolled_back" {
            if previous_version.is_none() {
                sqlx::query("DELETE FROM managed_mcps WHERE managed_mcp_id = ? AND owner_task_id = ? AND NOT EXISTS(SELECT 1 FROM managed_versions WHERE managed_mcp_id = ?) AND NOT EXISTS(SELECT 1 FROM connection_projections WHERE managed_mcp_id = ?)")
                    .bind(managed_mcp_id).bind(task_id).bind(managed_mcp_id).bind(managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
            }
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        if previous_version == Some(target_version) {
            let state_json: String = journal.try_get("previous_state_json").map_err(map_sqlx)?;
            let state = decode::<ManagedMcpState>(&state_json)?;
            sqlx::query("UPDATE managed_versions SET active = 1,activation_state = 'active' WHERE managed_mcp_id = ? AND version = ?").bind(managed_mcp_id).bind(target_version).execute(&mut *tx).await.map_err(map_sqlx)?;
            sqlx::query("UPDATE managed_mcps SET state_json = ?,updated_at_ms = ?,revision = revision + 1 WHERE managed_mcp_id = ?").bind(encode(&state)?).bind(now_ms).bind(managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
            sqlx::query("UPDATE activation_journal SET status = 'rolled_back',updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ?").bind(now_ms).bind(managed_mcp_id).bind(task_id).execute(&mut *tx).await.map_err(map_sqlx)?;
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        let owned = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM installation_ownership WHERE managed_mcp_id = ? AND version = ? AND owner_task_id = ?)")
            .bind(managed_mcp_id).bind(target_version).bind(task_id).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
        if !owned {
            return Err(integrity_error());
        }
        let artifact_digest = sqlx::query_scalar::<_, Option<String>>(
            "SELECT artifact_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ?",
        )
        .bind(managed_mcp_id)
        .bind(target_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        .flatten();
        sqlx::query("UPDATE managed_versions SET active = 0, activation_state = 'removal_pending' WHERE managed_mcp_id = ? AND version = ?")
            .bind(managed_mcp_id).bind(target_version).execute(&mut *tx).await.map_err(map_sqlx)?;
        if let Some(previous) = previous_version {
            sqlx::query("UPDATE managed_versions SET active = 1, activation_state = 'active' WHERE managed_mcp_id = ? AND version = ?")
                .bind(managed_mcp_id).bind(previous).execute(&mut *tx).await.map_err(map_sqlx)?;
            let digest = sqlx::query_scalar::<_, String>("SELECT manifest_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ?").bind(managed_mcp_id).bind(previous).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
            let state_json: String = journal.try_get("previous_state_json").map_err(map_sqlx)?;
            let state = decode::<ManagedMcpState>(&state_json)?;
            sqlx::query("UPDATE managed_mcps SET state_json = ?,active_manifest_digest = ?,active_version = ?,updated_at_ms = ?,revision = revision + 1 WHERE managed_mcp_id = ?")
                .bind(encode(&state)?).bind(digest).bind(previous).bind(now_ms).bind(managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        }
        sqlx::query("DELETE FROM installation_ownership WHERE managed_mcp_id = ? AND version = ? AND owner_task_id = ?").bind(managed_mcp_id).bind(target_version).bind(task_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query(
            "DELETE FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 0",
        )
        .bind(managed_mcp_id)
        .bind(target_version)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        if let Some(digest) = artifact_digest {
            sqlx::query("UPDATE artifact_cache SET reference_count = reference_count - 1 WHERE artifact_digest = ? AND reference_count > 0").bind(digest).execute(&mut *tx).await.map_err(map_sqlx)?;
        }
        sqlx::query("UPDATE activation_journal SET status = 'rolled_back',updated_at_ms = ? WHERE managed_mcp_id = ? AND task_id = ?").bind(now_ms).bind(managed_mcp_id).bind(task_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        if previous_version.is_none() {
            sqlx::query("DELETE FROM managed_mcps WHERE managed_mcp_id = ? AND owner_task_id = ? AND NOT EXISTS(SELECT 1 FROM managed_versions WHERE managed_mcp_id = ?)").bind(managed_mcp_id).bind(task_id).bind(managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        }
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn begin_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedUninstallSnapshot> {
        let mut tx = self.begin_immediate().await?;
        if let Some(row) = sqlx::query("SELECT managed_mcp_id,version,artifact_digest FROM uninstall_journal WHERE task_id = ?").bind(task_id).fetch_optional(&mut *tx).await.map_err(map_sqlx)? {
            let snapshot = ManagedUninstallSnapshot { managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?, version: row.try_get("version").map_err(map_sqlx)?, artifact_digest: row.try_get("artifact_digest").map_err(map_sqlx)? };
            if snapshot.managed_mcp_id != managed_mcp_id || snapshot.version != version { return Err(integrity_error()); }
            tx.commit().await.map_err(map_sqlx)?; return Ok(snapshot);
        }
        let lease_owner = sqlx::query_scalar::<_, String>(
            "SELECT task_id FROM managed_lifecycle_leases WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        if lease_owner.as_deref() != Some(task_id) {
            return Err(crate::mcp_platform::error::McpPlatformError::new(
                McpPlatformErrorCode::PlanConflict,
                "managed MCP lifecycle lease is not owned by this task",
            ));
        }
        let uninstall_pending = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM uninstall_journal WHERE managed_mcp_id = ? AND status IN ('started','quarantined'))")
            .bind(managed_mcp_id).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
        if uninstall_pending {
            return Err(crate::mcp_platform::error::McpPlatformError::new(
                McpPlatformErrorCode::PlanConflict,
                "managed MCP already has an active lifecycle mutation",
            ));
        }
        let row = sqlx::query(
            "SELECT state_json,active_version FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        if row
            .try_get::<Option<String>, _>("active_version")
            .map_err(map_sqlx)?
            .as_deref()
            != Some(version)
        {
            return Err(integrity_error());
        }
        let previous_state_json: String = row.try_get("state_json").map_err(map_sqlx)?;
        let mut state = decode::<ManagedMcpState>(&previous_state_json)?;
        state.installation = InstallationState::UninstallPending;
        let artifact_digest = sqlx::query_scalar::<_, Option<String>>("SELECT artifact_digest FROM managed_versions WHERE managed_mcp_id = ? AND version = ? AND active = 1").bind(managed_mcp_id).bind(version).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query("INSERT INTO uninstall_journal(task_id,managed_mcp_id,version,previous_state_json,artifact_digest,status,created_at_ms,updated_at_ms) VALUES (?,?,?,?,?,'started',?,?)")
            .bind(task_id).bind(managed_mcp_id).bind(version).bind(previous_state_json).bind(&artifact_digest).bind(now_ms).bind(now_ms).execute(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query("UPDATE managed_mcps SET state_json = ?,updated_at_ms = ?,revision = revision + 1 WHERE managed_mcp_id = ?").bind(encode(&state)?).bind(now_ms).bind(managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(ManagedUninstallSnapshot {
            managed_mcp_id: managed_mcp_id.to_string(),
            version: version.to_string(),
            artifact_digest,
        })
    }

    pub async fn mark_managed_uninstall_quarantined(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        if sqlx::query_scalar::<_, String>("SELECT status FROM uninstall_journal WHERE task_id = ?")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .as_deref()
            == Some("committed")
        {
            return Ok(());
        }
        let result = sqlx::query("UPDATE uninstall_journal SET status = 'quarantined',updated_at_ms = ? WHERE task_id = ? AND status IN ('started','quarantined')").bind(now_ms).bind(task_id).execute(&self.pool).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        Ok(())
    }

    pub async fn cancel_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT previous_state_json,status FROM uninstall_journal WHERE task_id = ? AND managed_mcp_id = ?").bind(task_id).bind(managed_mcp_id).fetch_optional(&mut *tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        let status: String = row.try_get("status").map_err(map_sqlx)?;
        if status == "cancelled" {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        if !matches!(status.as_str(), "started" | "quarantined") {
            return Err(integrity_error());
        }
        let previous: String = row.try_get("previous_state_json").map_err(map_sqlx)?;
        sqlx::query("UPDATE managed_mcps SET state_json = ?,updated_at_ms = ?,revision = revision + 1 WHERE managed_mcp_id = ?").bind(previous).bind(now_ms).bind(managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE uninstall_journal SET status = 'cancelled',updated_at_ms = ? WHERE task_id = ?",
        )
        .bind(now_ms)
        .bind(task_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn finalize_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT status,artifact_digest FROM uninstall_journal WHERE task_id = ? AND managed_mcp_id = ? AND version = ?").bind(task_id).bind(managed_mcp_id).bind(version).fetch_optional(&mut *tx).await.map_err(map_sqlx)?.ok_or_else(not_found)?;
        let status: String = row.try_get("status").map_err(map_sqlx)?;
        if status == "committed" {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        if status != "quarantined" {
            return Err(integrity_error());
        }
        let version_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM managed_versions WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let owned_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(DISTINCT version) FROM installation_ownership WHERE managed_mcp_id = ? AND remove_on_uninstall = 1").bind(managed_mcp_id).fetch_one(&mut *tx).await.map_err(map_sqlx)?;
        if version_count == 0 || owned_count != version_count {
            return Err(integrity_error());
        }
        let digests = sqlx::query_scalar::<_, Option<String>>(
            "SELECT artifact_digest FROM managed_versions WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query("DELETE FROM connection_projections WHERE managed_mcp_id = ?")
            .bind(managed_mcp_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query("DELETE FROM installation_ownership WHERE managed_mcp_id = ?")
            .bind(managed_mcp_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query("DELETE FROM managed_versions WHERE managed_mcp_id = ?")
            .bind(managed_mcp_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query("DELETE FROM managed_mcps WHERE managed_mcp_id = ? AND NOT EXISTS(SELECT 1 FROM managed_versions WHERE managed_mcp_id = ?)").bind(managed_mcp_id).bind(managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        for digest in digests.into_iter().flatten() {
            sqlx::query("UPDATE artifact_cache SET reference_count = reference_count - 1 WHERE artifact_digest = ? AND reference_count > 0").bind(digest).execute(&mut *tx).await.map_err(map_sqlx)?;
        }
        sqlx::query(
            "UPDATE uninstall_journal SET status = 'committed',updated_at_ms = ? WHERE task_id = ?",
        )
        .bind(now_ms)
        .bind(task_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn register_managed_mcp(
        &self,
        input: RegisterManagedMcp<'_>,
    ) -> McpPlatformResult<RegisterManagedMcpOutcome> {
        let mut tx = self.begin_immediate().await?;
        let manifest =
            sqlx::query("SELECT mcp_id, version FROM manifest_blobs WHERE manifest_digest = ?")
                .bind(input.manifest_digest)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx)?
                .ok_or_else(not_found)?;
        if manifest.try_get::<String, _>("mcp_id").map_err(map_sqlx)? != input.mcp_id
            || manifest.try_get::<String, _>("version").map_err(map_sqlx)? != input.version
        {
            return Err(integrity_error());
        }

        if let Some(row) = sqlx::query(
            r#"SELECT managed_mcp_id, distribution_adapter, active_manifest_digest,
                active_version FROM managed_mcps WHERE mcp_id = ? AND installation_scope = ?"#,
        )
        .bind(input.mcp_id)
        .bind(input.installation_scope)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        {
            let managed_mcp_id: String = row.try_get("managed_mcp_id").map_err(map_sqlx)?;
            let matches = managed_mcp_id == input.managed_mcp_id
                && row
                    .try_get::<String, _>("distribution_adapter")
                    .map_err(map_sqlx)?
                    == input.distribution_adapter
                && row
                    .try_get::<Option<String>, _>("active_manifest_digest")
                    .map_err(map_sqlx)?
                    .as_deref()
                    == Some(input.manifest_digest)
                && row
                    .try_get::<Option<String>, _>("active_version")
                    .map_err(map_sqlx)?
                    .as_deref()
                    == Some(input.version);
            tx.commit().await.map_err(map_sqlx)?;
            if !matches {
                return Err(error(
                    McpPlatformErrorCode::IdempotencyConflict,
                    "managed MCP identity already refers to different immutable content",
                ));
            }
            return Ok(RegisterManagedMcpOutcome {
                record: self.get_managed_inventory(input.managed_mcp_id).await?,
                created: false,
            });
        }

        let state = ManagedMcpState {
            registration: RegistrationState::Registered,
            installation: InstallationState::NotApplicable,
            ..ManagedMcpState::default()
        };
        sqlx::query(
            r#"INSERT INTO managed_mcps (
                managed_mcp_id, mcp_id, installation_scope, state_json, revision,
                created_at_ms, updated_at_ms, distribution_adapter, active_manifest_digest,
                active_version, owner_task_id
            ) VALUES (?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(input.managed_mcp_id)
        .bind(input.mcp_id)
        .bind(input.installation_scope)
        .bind(encode(&state)?)
        .bind(input.now_ms)
        .bind(input.now_ms)
        .bind(input.distribution_adapter)
        .bind(input.manifest_digest)
        .bind(input.version)
        .bind(input.task_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"INSERT INTO managed_versions (
                managed_mcp_id, version, manifest_digest, installation_root, verified, active,
                adapter_evidence_json, created_at_ms
            ) VALUES (?, ?, ?, NULL, 1, 1, ?, ?)"#,
        )
        .bind(input.managed_mcp_id)
        .bind(input.version)
        .bind(input.manifest_digest)
        .bind(encode(input.adapter_evidence)?)
        .bind(input.now_ms)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(RegisterManagedMcpOutcome {
            record: self.get_managed_inventory(input.managed_mcp_id).await?,
            created: true,
        })
    }

    pub async fn get_managed_inventory(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        let row = sqlx::query(
            r#"SELECT distribution_adapter, active_manifest_digest, active_version, owner_task_id
                FROM managed_mcps WHERE managed_mcp_id = ?"#,
        )
        .bind(managed_mcp_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let managed = self.get_managed_mcp(managed_mcp_id).await?;
        let lifecycle = ManagedMcpLifecycleMetadata {
            distribution_adapter: row.try_get("distribution_adapter").map_err(map_sqlx)?,
            active_manifest_digest: row.try_get("active_manifest_digest").map_err(map_sqlx)?,
            active_version: row.try_get("active_version").map_err(map_sqlx)?,
            owner_task_id: row.try_get("owner_task_id").map_err(map_sqlx)?,
        };
        match (
            lifecycle.active_manifest_digest.as_deref(),
            lifecycle.active_version.as_deref(),
        ) {
            (Some(digest), Some(version)) => {
                let active = managed
                    .versions
                    .iter()
                    .find(|candidate| candidate.active)
                    .ok_or_else(integrity_error)?;
                if active.manifest_digest != digest || active.version != version {
                    return Err(integrity_error());
                }
                let manifest = self.get_manifest(digest).await?;
                if manifest.verified.manifest().id != managed.mcp_id
                    || manifest.verified.manifest().version.as_str() != version
                {
                    return Err(integrity_error());
                }
            }
            (None, None) => {}
            _ => return Err(integrity_error()),
        }
        Ok(ManagedMcpInventoryRecord { managed, lifecycle })
    }

    pub async fn list_managed_inventory(
        &self,
        after_managed_mcp_id: Option<&str>,
        limit: usize,
        filter: &super::super::ManagedInventoryFilter,
    ) -> McpPlatformResult<Vec<ManagedMcpInventoryRecord>> {
        let ids = sqlx::query_scalar::<_, String>(
            r#"SELECT managed_mcp_id FROM managed_mcps
                WHERE (? IS NULL OR managed_mcp_id > ?)
                  AND (? IS NULL OR json_extract(state_json, '$.registration') = ?)
                  AND (? IS NULL OR json_extract(state_json, '$.installation') = ?)
                  AND (? IS NULL OR json_extract(state_json, '$.runtime') = ?)
                  AND (? IS NULL OR json_extract(state_json, '$.health') = ?)
                  AND (? IS NULL OR json_extract(state_json, '$.default_enabled') = ?)
                ORDER BY managed_mcp_id LIMIT ?"#,
        )
        .bind(after_managed_mcp_id)
        .bind(after_managed_mcp_id)
        .bind(filter.registration.map(enum_json).transpose()?)
        .bind(filter.registration.map(enum_json).transpose()?)
        .bind(filter.installation.map(enum_json).transpose()?)
        .bind(filter.installation.map(enum_json).transpose()?)
        .bind(filter.runtime.map(enum_json).transpose()?)
        .bind(filter.runtime.map(enum_json).transpose()?)
        .bind(filter.health.map(enum_json).transpose()?)
        .bind(filter.health.map(enum_json).transpose()?)
        .bind(filter.default_enabled.map(i64::from))
        .bind(filter.default_enabled.map(i64::from))
        .bind(i64::try_from(limit).map_err(|_| integrity_error())?)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            records.push(self.get_managed_inventory(&id).await?);
        }
        Ok(records)
    }

    pub async fn put_owned_connection_projection(
        &self,
        input: PutOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        let mut tx = self.begin_immediate().await?;
        let managed_digest = sqlx::query_scalar::<_, Option<String>>(
            "SELECT active_manifest_digest FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(input.managed_mcp_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let plan = fetch_valid_plan(&mut tx, input.plan_id).await?;
        if plan.plan.manifest_digest() != input.manifest_digest
            || plan.target.mcp_id != plan.plan.manifest_id()
        {
            return Err(integrity_error());
        }
        let digest_allowed = managed_digest.as_deref() == Some(input.manifest_digest)
            || (plan.target.managed_mcp_id.as_deref() == Some(input.managed_mcp_id)
                && matches!(
                    plan.plan.operation(),
                    crate::mcp_platform::policy::PlanOperation::Update
                        | crate::mcp_platform::policy::PlanOperation::Repair
                ));
        if !digest_allowed {
            return Err(integrity_error());
        }
        if let Some(row) = sqlx::query(
            "SELECT * FROM connection_projections WHERE managed_mcp_id = ? OR link_key = ?",
        )
        .bind(input.managed_mcp_id)
        .bind(input.link_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        {
            let same = row
                .try_get::<String, _>("managed_mcp_id")
                .map_err(map_sqlx)?
                == input.managed_mcp_id
                && row.try_get::<String, _>("link_key").map_err(map_sqlx)? == input.link_key
                && row
                    .try_get::<String, _>("projection_digest")
                    .map_err(map_sqlx)?
                    == input.projection_digest
                && decode::<crate::mcp_platform::plan::ConnectionProjection>(
                    &row.try_get::<String, _>("projection_json")
                        .map_err(map_sqlx)?,
                )? == *input.projection;
            if same {
                tx.commit().await.map_err(map_sqlx)?;
                return self.get_connection_projection(input.managed_mcp_id).await;
            }
            let same_identity = row
                .try_get::<String, _>("managed_mcp_id")
                .map_err(map_sqlx)?
                == input.managed_mcp_id
                && row.try_get::<String, _>("link_key").map_err(map_sqlx)? == input.link_key
                && plan.target.managed_mcp_id.as_deref() == Some(input.managed_mcp_id)
                && matches!(
                    plan.plan.operation(),
                    crate::mcp_platform::policy::PlanOperation::Update
                        | crate::mcp_platform::policy::PlanOperation::Repair
                );
            if !same_identity {
                return Err(error(
                    McpPlatformErrorCode::ProjectionConflict,
                    "extension projection key is already owned by different content",
                ));
            }
            sqlx::query(r#"UPDATE connection_projections SET projection_json = ?, revision = revision + 1, updated_at_ms = ?, plan_id = ?, manifest_digest = ?, owner_task_id = ?, projection_digest = ? WHERE managed_mcp_id = ? AND link_key = ?"#)
                .bind(encode(input.projection)?).bind(input.now_ms).bind(input.plan_id).bind(input.manifest_digest).bind(input.owner_task_id).bind(input.projection_digest).bind(input.managed_mcp_id).bind(input.link_key)
                .execute(&mut *tx).await.map_err(map_sqlx)?;
            tx.commit().await.map_err(map_sqlx)?;
            return self.get_connection_projection(input.managed_mcp_id).await;
        }
        sqlx::query(
            r#"INSERT INTO connection_projections (
                managed_mcp_id, link_key, projection_json, revision, updated_at_ms,
                plan_id, manifest_digest, owner_task_id, projection_digest
            ) VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?)"#,
        )
        .bind(input.managed_mcp_id)
        .bind(input.link_key)
        .bind(encode(input.projection)?)
        .bind(input.now_ms)
        .bind(input.plan_id)
        .bind(input.manifest_digest)
        .bind(input.owner_task_id)
        .bind(input.projection_digest)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        self.get_connection_projection(input.managed_mcp_id).await
    }

    pub async fn remove_owned_projection(
        &self,
        managed_mcp_id: &str,
        owner_task_id: &str,
    ) -> McpPlatformResult<bool> {
        let result = sqlx::query(
            "DELETE FROM connection_projections WHERE managed_mcp_id = ? AND owner_task_id = ?",
        )
        .bind(managed_mcp_id)
        .bind(owner_task_id)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn restore_owned_connection_projection(
        &self,
        input: RestoreOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        let result = sqlx::query(r#"UPDATE connection_projections SET projection_json = ?,revision = revision + 1,updated_at_ms = ?,plan_id = ?,manifest_digest = ?,owner_task_id = ?,projection_digest = ? WHERE managed_mcp_id = ? AND link_key = ? AND owner_task_id = ?"#)
            .bind(encode(input.projection)?).bind(input.now_ms).bind(input.plan_id).bind(input.manifest_digest).bind(input.owner_task_id).bind(input.projection_digest).bind(input.managed_mcp_id).bind(input.link_key).bind(input.replacing_task_id)
            .execute(&self.pool).await.map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(integrity_error());
        }
        self.get_connection_projection(input.managed_mcp_id).await
    }

    pub async fn remove_owned_managed_mcp(
        &self,
        managed_mcp_id: &str,
        owner_task_id: &str,
    ) -> McpPlatformResult<bool> {
        let mut tx = self.begin_immediate().await?;
        let projection_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM connection_projections WHERE managed_mcp_id = ?",
        )
        .bind(managed_mcp_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        if projection_count != 0 {
            return Ok(false);
        }
        sqlx::query(
            "DELETE FROM managed_versions WHERE managed_mcp_id = ? AND EXISTS (SELECT 1 FROM managed_mcps WHERE managed_mcp_id = ? AND owner_task_id = ?)",
        )
        .bind(managed_mcp_id)
        .bind(managed_mcp_id)
        .bind(owner_task_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let result =
            sqlx::query("DELETE FROM managed_mcps WHERE managed_mcp_id = ? AND owner_task_id = ?")
                .bind(managed_mcp_id)
                .bind(owner_task_id)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn create_health_task(
        &self,
        input: CreateHealthTask<'_>,
    ) -> McpPlatformResult<TaskRecord> {
        let mut tx = self.begin_immediate().await?;
        if let Some(row) =
            sqlx::query("SELECT * FROM tasks WHERE operation = 'health' AND idempotency_key = ?")
                .bind(input.idempotency_key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx)?
        {
            let task = decode_task_row(&row)?;
            let request = sqlx::query(
                "SELECT managed_mcp_id, mode FROM health_task_requests WHERE task_id = ?",
            )
            .bind(&task.task_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx)?;
            let same = request
                .try_get::<String, _>("managed_mcp_id")
                .map_err(map_sqlx)?
                == input.managed_mcp_id
                && request.try_get::<String, _>("mode").map_err(map_sqlx)? == input.mode.as_str();
            tx.commit().await.map_err(map_sqlx)?;
            return if same {
                Ok(task)
            } else {
                Err(error(
                    McpPlatformErrorCode::IdempotencyConflict,
                    "health idempotency key already refers to different content",
                ))
            };
        }
        let projection = sqlx::query(
            r#"SELECT cp.plan_id, ip.plan_digest, mv.adapter_evidence_json
                FROM connection_projections cp
                JOIN install_plans ip ON ip.plan_id = cp.plan_id
                JOIN managed_mcps mm ON mm.managed_mcp_id = cp.managed_mcp_id
                JOIN managed_versions mv ON mv.managed_mcp_id = mm.managed_mcp_id AND mv.active = 1
                WHERE cp.managed_mcp_id = ?"#,
        )
        .bind(input.managed_mcp_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let plan_id: String = projection.try_get("plan_id").map_err(map_sqlx)?;
        let plan_digest: String = projection.try_get("plan_digest").map_err(map_sqlx)?;
        sqlx::query(
            r#"INSERT INTO tasks (
                task_id, plan_id, plan_digest, operation, idempotency_key, status, actor,
                created_at_ms, updated_at_ms, progress, step_cursor, adapter_evidence_json,
                rollback_status, revision, event_sequence
            ) VALUES (?, ?, ?, 'health', ?, 'queued', ?, ?, ?, 0, 0, ?, 'not_required', 0, 1)"#,
        )
        .bind(input.task_id)
        .bind(&plan_id)
        .bind(&plan_digest)
        .bind(input.idempotency_key)
        .bind(input.actor)
        .bind(input.now_ms)
        .bind(input.now_ms)
        .bind(
            projection
                .try_get::<Option<String>, _>("adapter_evidence_json")
                .map_err(map_sqlx)?,
        )
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "INSERT INTO health_task_requests(task_id, managed_mcp_id, mode) VALUES (?, ?, ?)",
        )
        .bind(input.task_id)
        .bind(input.managed_mcp_id)
        .bind(input.mode.as_str())
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id: input.task_id,
                sequence: 1,
                event_type: AuditEventType::TaskCreated,
                actor: input.actor,
                occurred_at_ms: input.now_ms,
                payload: &AuditPayload::TaskCreated {
                    status: TaskStatus::Queued,
                },
                redacted_error: None,
            },
        )
        .await?;
        let task = fetch_task(&mut tx, input.task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(task)
    }

    pub async fn get_health_task_request(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<HealthTaskRequestRecord> {
        let row = sqlx::query(
            "SELECT task_id, managed_mcp_id, mode FROM health_task_requests WHERE task_id = ?",
        )
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        Ok(HealthTaskRequestRecord {
            task_id: row.try_get("task_id").map_err(map_sqlx)?,
            managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
            mode: decode_database_enum(&row.try_get::<String, _>("mode").map_err(map_sqlx)?)?,
        })
    }

    pub async fn append_health_observation(
        &self,
        input: NewHealthObservation<'_>,
    ) -> McpPlatformResult<HealthObservationRecord> {
        if input.latency_ms < 0 {
            return Err(integrity_error());
        }
        let mut tx = self.begin_immediate().await?;
        if let Some(observation_id) = sqlx::query_scalar::<_, i64>(
            "SELECT observation_id FROM health_observations WHERE task_id = ?",
        )
        .bind(input.task_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        {
            tx.commit().await.map_err(map_sqlx)?;
            return self.get_health_observation(observation_id).await;
        }
        let state_json = sqlx::query_scalar::<_, String>(
            "SELECT state_json FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(input.managed_mcp_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let mut state: ManagedMcpState = decode(&state_json)?;
        state.health = input.result_code.health_state();
        let result = sqlx::query(
            r#"INSERT INTO health_observations (
                managed_mcp_id, task_id, check_type, result_code, latency_ms,
                capabilities_digest, tools_digest, checked_at_ms, detail_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(input.managed_mcp_id)
        .bind(input.task_id)
        .bind(input.check_type)
        .bind(input.result_code.as_str())
        .bind(input.latency_ms)
        .bind(input.capabilities_digest)
        .bind(input.tools_digest)
        .bind(input.checked_at_ms)
        .bind(encode(&input.detail_code)?)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE managed_mcps SET state_json = ?, revision = revision + 1, updated_at_ms = ? WHERE managed_mcp_id = ?",
        )
        .bind(encode(&state)?)
        .bind(input.checked_at_ms)
        .bind(input.managed_mcp_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let observation_id = result.last_insert_rowid();
        tx.commit().await.map_err(map_sqlx)?;
        self.get_health_observation(observation_id).await
    }

    pub async fn get_health_observation(
        &self,
        observation_id: i64,
    ) -> McpPlatformResult<HealthObservationRecord> {
        let row = sqlx::query("SELECT * FROM health_observations WHERE observation_id = ?")
            .bind(observation_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_health_observation(&row)
    }

    pub async fn latest_health_observation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<HealthObservationRecord>> {
        let row = sqlx::query(
            "SELECT * FROM health_observations WHERE managed_mcp_id = ? ORDER BY checked_at_ms DESC, observation_id DESC LIMIT 1",
        )
        .bind(managed_mcp_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        row.as_ref().map(decode_health_observation).transpose()
    }

    pub async fn list_retry_attempts(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<Vec<RetryAttemptRecord>> {
        let rows =
            sqlx::query("SELECT * FROM task_retry_attempts WHERE task_id = ? ORDER BY attempt")
                .bind(task_id)
                .fetch_all(&self.pool)
                .await
                .map_err(map_sqlx)?;
        rows.iter()
            .map(|row| {
                Ok(RetryAttemptRecord {
                    task_id: row.try_get("task_id").map_err(map_sqlx)?,
                    attempt: row.try_get("attempt").map_err(map_sqlx)?,
                    idempotency_key: row.try_get("idempotency_key").map_err(map_sqlx)?,
                    requested_from_status: decode_database_enum(
                        &row.try_get::<String, _>("requested_from_status")
                            .map_err(map_sqlx)?,
                    )?,
                    actor: row.try_get("actor").map_err(map_sqlx)?,
                    created_at_ms: row.try_get("created_at_ms").map_err(map_sqlx)?,
                })
            })
            .collect()
    }

    pub async fn begin_projection_mutation(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        desired_enabled: bool,
        now_ms: i64,
    ) -> McpPlatformResult<ProjectionMutationRecord> {
        let mut tx = self.begin_immediate().await?;
        let lifecycle_busy = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM managed_lifecycle_leases WHERE managed_mcp_id = ?)",
        )
        .bind(managed_mcp_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        if lifecycle_busy {
            return Err(error(
                McpPlatformErrorCode::PlanConflict,
                "managed MCP lifecycle mutation is active",
            ));
        }
        if let Some(row) = sqlx::query("SELECT * FROM projection_mutations WHERE managed_mcp_id = ? AND status IN ('started','config_committed','recovery_required')")
            .bind(managed_mcp_id).fetch_optional(&mut *tx).await.map_err(map_sqlx)? {
            let record = decode_projection_mutation(&row)?;
            if record.status == ProjectionMutationStatus::RecoveryRequired {
                return Err(error(
                    McpPlatformErrorCode::PlanConflict,
                    "managed MCP projection recovery must be resolved first",
                ));
            }
            tx.commit().await.map_err(map_sqlx)?;
            return if record.expected_revision == expected_revision && record.desired_enabled == desired_enabled { Ok(record) } else { Err(error(McpPlatformErrorCode::RevisionConflict, "managed MCP has an active projection mutation")) };
        }
        let row =
            sqlx::query("SELECT revision, state_json FROM managed_mcps WHERE managed_mcp_id = ?")
                .bind(managed_mcp_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx)?
                .ok_or_else(not_found)?;
        let revision: i64 = row.try_get("revision").map_err(map_sqlx)?;
        if revision != expected_revision {
            return Err(error(
                McpPlatformErrorCode::RevisionConflict,
                "managed MCP revision changed",
            ));
        }
        let state: ManagedMcpState =
            decode(&row.try_get::<String, _>("state_json").map_err(map_sqlx)?)?;
        let result = sqlx::query("INSERT INTO projection_mutations(managed_mcp_id, expected_revision, previous_enabled, desired_enabled, status, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, 'started', ?, ?)")
            .bind(managed_mcp_id).bind(expected_revision).bind(state.default_enabled).bind(desired_enabled).bind(now_ms).bind(now_ms)
            .execute(&mut *tx).await.map_err(map_sqlx)?;
        let record = ProjectionMutationRecord {
            mutation_id: result.last_insert_rowid(),
            managed_mcp_id: managed_mcp_id.to_string(),
            expected_revision,
            previous_enabled: state.default_enabled,
            desired_enabled,
            status: ProjectionMutationStatus::Started,
        };
        tx.commit().await.map_err(map_sqlx)?;
        Ok(record)
    }

    pub async fn mark_projection_config_committed(
        &self,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let result = sqlx::query("UPDATE projection_mutations SET status = 'config_committed', updated_at_ms = ? WHERE mutation_id = ? AND status IN ('started','recovery_required')")
            .bind(now_ms).bind(mutation_id).execute(&self.pool).await.map_err(map_sqlx)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(integrity_error())
        }
    }

    pub async fn complete_projection_mutation(
        &self,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query("SELECT * FROM projection_mutations WHERE mutation_id = ?")
            .bind(mutation_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        let mutation = decode_projection_mutation(&row)?;
        if mutation.status == ProjectionMutationStatus::Committed {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(());
        }
        if mutation.status != ProjectionMutationStatus::ConfigCommitted {
            return Err(integrity_error());
        }
        let state_json = sqlx::query_scalar::<_, String>(
            "SELECT state_json FROM managed_mcps WHERE managed_mcp_id = ?",
        )
        .bind(&mutation.managed_mcp_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
        let mut state: ManagedMcpState = decode(&state_json)?;
        state.default_enabled = mutation.desired_enabled;
        sqlx::query("UPDATE managed_mcps SET state_json = ?, revision = revision + 1, updated_at_ms = ? WHERE managed_mcp_id = ?")
            .bind(encode(&state)?).bind(now_ms).bind(&mutation.managed_mcp_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        sqlx::query("UPDATE projection_mutations SET status = 'committed', updated_at_ms = ? WHERE mutation_id = ?")
            .bind(now_ms).bind(mutation_id).execute(&mut *tx).await.map_err(map_sqlx)?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(())
    }

    pub async fn list_pending_projection_mutations(
        &self,
    ) -> McpPlatformResult<Vec<ProjectionMutationRecord>> {
        let rows = sqlx::query("SELECT * FROM projection_mutations WHERE status IN ('started','config_committed','recovery_required') ORDER BY mutation_id")
            .fetch_all(&self.pool).await.map_err(map_sqlx)?;
        rows.iter().map(decode_projection_mutation).collect()
    }

    pub async fn projection_recovery_required(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<bool> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM projection_mutations WHERE managed_mcp_id = ? AND status = 'recovery_required')",
        )
        .bind(managed_mcp_id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx)
    }

    pub async fn mark_projection_mutation_recovery_required(
        &self,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        sqlx::query("UPDATE projection_mutations SET status = 'recovery_required', updated_at_ms = ? WHERE mutation_id = ? AND status IN ('started','config_committed')")
            .bind(now_ms).bind(mutation_id).execute(&self.pool).await.map_err(map_sqlx)?;
        Ok(())
    }
}

fn same_supply_chain_authority(
    persisted: Option<&str>,
    observed: Option<&SupplyChainEvidence>,
) -> McpPlatformResult<bool> {
    match (persisted, observed) {
        (None, None) => Ok(true),
        (Some(value), Some(observed)) => {
            let persisted: SupplyChainEvidence = decode(value)?;
            Ok(persisted.same_authority(observed))
        }
        _ => Ok(false),
    }
}

fn enum_json<T: serde::Serialize>(value: T) -> McpPlatformResult<String> {
    serde_json::to_string(&value)
        .map(|value| value.trim_matches('"').to_string())
        .map_err(|_| integrity_error())
}

fn decode_projection_mutation(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<ProjectionMutationRecord> {
    Ok(ProjectionMutationRecord {
        mutation_id: row.try_get("mutation_id").map_err(map_sqlx)?,
        managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
        expected_revision: row.try_get("expected_revision").map_err(map_sqlx)?,
        previous_enabled: row.try_get("previous_enabled").map_err(map_sqlx)?,
        desired_enabled: row.try_get("desired_enabled").map_err(map_sqlx)?,
        status: decode_database_enum(&row.try_get::<String, _>("status").map_err(map_sqlx)?)?,
    })
}

fn decode_health_observation(
    row: &sqlx::sqlite::SqliteRow,
) -> McpPlatformResult<HealthObservationRecord> {
    Ok(HealthObservationRecord {
        observation_id: row.try_get("observation_id").map_err(map_sqlx)?,
        managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
        task_id: row.try_get("task_id").map_err(map_sqlx)?,
        check_type: row.try_get("check_type").map_err(map_sqlx)?,
        result_code: decode_database_enum(
            &row.try_get::<String, _>("result_code").map_err(map_sqlx)?,
        )?,
        latency_ms: row.try_get("latency_ms").map_err(map_sqlx)?,
        capabilities_digest: row.try_get("capabilities_digest").map_err(map_sqlx)?,
        tools_digest: row.try_get("tools_digest").map_err(map_sqlx)?,
        checked_at_ms: row.try_get("checked_at_ms").map_err(map_sqlx)?,
        detail_code: decode(&row.try_get::<String, _>("detail_json").map_err(map_sqlx)?)?,
    })
}
