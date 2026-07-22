use sqlx::Row;

use crate::mcp_platform::domain::{InstallationState, ManagedMcpState, RegistrationState};
use crate::mcp_platform::error::{McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::repository::{
    AuditEventType, AuditPayload, ConnectionProjectionRecord, CreateHealthTask,
    HealthObservationRecord, HealthTaskRequestRecord, ManagedMcpInventoryRecord,
    ManagedMcpLifecycleMetadata, NewHealthObservation, ProjectionMutationRecord,
    ProjectionMutationStatus, PutOwnedProjection, RegisterManagedMcp, RegisterManagedMcpOutcome,
    RetryAttemptRecord, TaskRecord,
};
use crate::mcp_platform::task::TaskStatus;

use super::records::{
    append_audit, decode, decode_database_enum, decode_task_row, encode, error, fetch_task,
    fetch_valid_plan, integrity_error, map_sqlx, not_found, NewAuditEvent,
};
use super::SqliteMcpPlatformRepository;

impl SqliteMcpPlatformRepository {
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
        if managed_digest.as_deref() != Some(input.manifest_digest) {
            return Err(integrity_error());
        }
        let plan = fetch_valid_plan(&mut tx, input.plan_id).await?;
        if plan.plan.manifest_digest() != input.manifest_digest
            || plan.target.mcp_id != plan.plan.manifest_id()
        {
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
            tx.commit().await.map_err(map_sqlx)?;
            if !same {
                return Err(error(
                    McpPlatformErrorCode::ProjectionConflict,
                    "extension projection key is already owned by different content",
                ));
            }
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
        if let Some(row) = sqlx::query("SELECT * FROM projection_mutations WHERE managed_mcp_id = ? AND status IN ('started','config_committed')")
            .bind(managed_mcp_id).fetch_optional(&mut *tx).await.map_err(map_sqlx)? {
            let record = decode_projection_mutation(&row)?;
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
        let result = sqlx::query("UPDATE projection_mutations SET status = 'config_committed', updated_at_ms = ? WHERE mutation_id = ? AND status = 'started'")
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
        let rows = sqlx::query("SELECT * FROM projection_mutations WHERE status IN ('started','config_committed') ORDER BY mutation_id")
            .fetch_all(&self.pool).await.map_err(map_sqlx)?;
        rows.iter().map(decode_projection_mutation).collect()
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
