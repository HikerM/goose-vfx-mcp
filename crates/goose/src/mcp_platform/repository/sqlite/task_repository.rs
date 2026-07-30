use crate::mcp_platform::error::{McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::repository::GlobalAuditPage;
use crate::mcp_platform::repository::{
    AuditEventRecord, AuditEventType, AuditPayload, CompensationTransition, CreateTask,
    ExecutionAuthorization, HealthTaskRequestRecord, QueuedTaskCandidate, QueuedTaskPreflight,
    RecoveryRecord, StepTransition, TaskRecord, TaskStepRecord, TaskTransition,
};
use crate::mcp_platform::task::{
    CompensationDescriptor, CompensationStatus, RedactedError, RedactedErrorCode, TaskStatus,
    TaskStepStatus,
};
use crate::mcp_platform::{domain::HealthCheckMode, policy::PlanOperation};
use sqlx::{QueryBuilder, Row, Sqlite, Transaction};

use super::records::{
    append_audit, decode_audit_row, decode_database_enum, decode_task_row, decode_task_step_row,
    encode, encode_optional, error, fetch_task, fetch_task_step, fetch_valid_plan, integrity_error,
    map_sqlx, not_found, recovery_decision, revision_conflict, task_step_compensation_payload,
    task_step_history_compensation_payload, verify_task_step_compensation_binding, NewAuditEvent,
};
use super::SqliteMcpPlatformRepository;

async fn database_now_ms(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<i64> {
    sqlx::query_scalar("SELECT CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)")
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)
}

async fn cancel_expired_queued_task(
    tx: &mut Transaction<'_, Sqlite>,
    task: &TaskRecord,
    actor: &str,
    now_ms: i64,
) -> McpPlatformResult<()> {
    task.status.ensure_transition(TaskStatus::Cancelled)?;
    let sequence = task.event_sequence + 1;
    let expired = RedactedError::new(
        RedactedErrorCode::VerificationFailed,
        "MCP task plan expired before execution authorization",
        std::iter::empty::<&str>(),
    );
    let result = sqlx::query(
        r#"UPDATE tasks SET status = 'cancelled', updated_at_ms = ?, heartbeat_at_ms = NULL,
            owner_id = NULL, lease_expires_at_ms = NULL, redacted_error_json = ?,
            revision = revision + 1, event_sequence = ?
            WHERE task_id = ? AND status = 'queued' AND revision = ?"#,
    )
    .bind(now_ms)
    .bind(encode(&expired)?)
    .bind(sequence)
    .bind(&task.task_id)
    .bind(task.revision)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if result.rows_affected() != 1 {
        return Ok(());
    }
    sqlx::query("DELETE FROM managed_lifecycle_leases WHERE task_id = ?")
        .bind(&task.task_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    append_audit(
        tx,
        NewAuditEvent {
            task_id: &task.task_id,
            sequence,
            event_type: AuditEventType::TaskStatusChanged,
            actor,
            occurred_at_ms: now_ms,
            payload: &AuditPayload::TaskStatusChanged {
                from: TaskStatus::Queued,
                to: TaskStatus::Cancelled,
            },
            redacted_error: Some(&expired),
        },
    )
    .await
}

impl SqliteMcpPlatformRepository {
    pub(crate) async fn next_queued_task_candidate(
        &self,
        now_ms: i64,
    ) -> McpPlatformResult<Option<QueuedTaskCandidate>> {
        let mut tx = self.begin_immediate().await?;
        let mut cancelled_expired_task = false;
        loop {
            let row = sqlx::query(
                r#"SELECT * FROM tasks WHERE status = 'queued'
                    AND (owner_id IS NULL OR lease_expires_at_ms IS NULL OR lease_expires_at_ms <= ?)
                    ORDER BY created_at_ms, task_id LIMIT 1"#,
            )
            .bind(now_ms)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?;
            let Some(row) = row else {
                if cancelled_expired_task {
                    tx.commit().await.map_err(map_sqlx)?;
                } else {
                    tx.rollback().await?;
                }
                return Ok(None);
            };
            let task = decode_task_row(&row)?;
            let plan = fetch_valid_plan(&mut tx, &task.plan_id).await?;
            if plan.expires_at_ms > database_now_ms(&mut tx).await? {
                if cancelled_expired_task {
                    tx.commit().await.map_err(map_sqlx)?;
                } else {
                    tx.rollback().await?;
                }
                return Ok(Some(QueuedTaskCandidate { task }));
            }
            cancel_expired_queued_task(&mut tx, &task, "task-preflight", now_ms).await?;
            cancelled_expired_task = true;
        }
    }

    pub(crate) async fn load_queued_task_preflight(
        &self,
        candidate: &QueuedTaskCandidate,
    ) -> McpPlatformResult<QueuedTaskPreflight> {
        let mut tx = self.begin_immediate().await?;
        let task = fetch_task(&mut tx, &candidate.task().task_id).await?;
        if task.status != TaskStatus::Queued
            || task.revision != candidate.task().revision
            || task.plan_id != candidate.task().plan_id
            || task.plan_digest != candidate.task().plan_digest
            || task.operation != candidate.task().operation
            || task.actor != candidate.task().actor
            || task.owner_id != candidate.task().owner_id
            || task.lease_expires_at_ms != candidate.task().lease_expires_at_ms
        {
            return Err(revision_conflict());
        }
        let plan = fetch_valid_plan(&mut tx, &task.plan_id).await?;
        let db_now_ms = database_now_ms(&mut tx).await?;
        if plan.expires_at_ms <= db_now_ms {
            cancel_expired_queued_task(&mut tx, &task, "task-preflight", db_now_ms).await?;
            tx.commit().await.map_err(map_sqlx)?;
            return Err(revision_conflict());
        }
        let health_request = fetch_health_task_request(&mut tx, &task.task_id).await?;
        if task.plan_digest != plan.plan.plan_digest()
            || !task_plan_operations_compatible(
                task.operation,
                plan.plan.operation(),
                health_request.as_ref(),
            )
            || task.actor != plan.actor
        {
            return Err(integrity_error());
        }
        validate_registration_health_binding(self, &mut tx, &task, &plan, health_request.as_ref())
            .await?;
        let checkpoint_root = tx.checkpoint.root.clone();
        let checkpoint_sequence = tx.checkpoint.sequence;
        tx.rollback().await?;
        let (manifest, _) = self.resolve_manifest_for_plan(&plan).await?;
        if manifest.verified.digest() != plan.plan.manifest_digest()
            || manifest.verified.manifest().id != plan.plan.manifest_id()
            || manifest.verified.manifest().version.as_str() != plan.plan.manifest_version()
        {
            return Err(integrity_error());
        }
        Ok(QueuedTaskPreflight {
            candidate: candidate.clone(),
            plan,
            manifest,
            checkpoint_root,
            checkpoint_sequence,
            issuer: self.authorization_issuer.clone(),
        })
    }

    pub(crate) async fn claim_preflighted_task(
        &self,
        preflight: &QueuedTaskPreflight,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        let lease_expires_at_ms = now_ms
            .checked_add(lease_duration_ms)
            .ok_or_else(integrity_error)?;
        let (manifest, _) = self.resolve_manifest_for_plan(preflight.plan()).await?;
        let mut tx = self.begin_immediate().await?;
        if !preflight.issued_by(&self.authorization_issuer)
            || tx.checkpoint.root != preflight.checkpoint_root()
            || tx.checkpoint.sequence != preflight.checkpoint_sequence()
        {
            tx.rollback().await?;
            return Ok(None);
        }
        let current = fetch_task(&mut tx, &preflight.candidate().task().task_id).await?;
        if current.status != TaskStatus::Queued
            || current.revision != preflight.candidate().task().revision
            || current.plan_id != preflight.candidate().task().plan_id
            || current.plan_digest != preflight.candidate().task().plan_digest
            || current.operation != preflight.candidate().task().operation
            || current.actor != preflight.candidate().task().actor
            || current.owner_id != preflight.candidate().task().owner_id
            || current.lease_expires_at_ms != preflight.candidate().task().lease_expires_at_ms
            || (current.owner_id.is_some()
                && current
                    .lease_expires_at_ms
                    .is_some_and(|expires_at_ms| expires_at_ms > now_ms))
        {
            tx.rollback().await?;
            return Ok(None);
        }
        let plan = fetch_valid_plan(&mut tx, &current.plan_id).await?;
        if plan.expires_at_ms <= database_now_ms(&mut tx).await? {
            cancel_expired_queued_task(&mut tx, &current, owner_id, now_ms).await?;
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(None);
        }
        if plan.envelope_digest != preflight.plan().envelope_digest
            || plan.plan.plan_digest() != preflight.plan().plan.plan_digest()
            || plan.plan.manifest_digest() != preflight.plan().plan.manifest_digest()
            || plan.actor != preflight.plan().actor
        {
            tx.rollback().await?;
            return Ok(None);
        }
        if manifest.verified.digest() != preflight.manifest().verified.digest()
            || manifest.verified.digest() != plan.plan.manifest_digest()
        {
            tx.rollback().await?;
            return Ok(None);
        }
        if managed_lifecycle_operation(current.operation) {
            let managed_mcp_id =
                lifecycle_target_for_plan(&plan, current.operation)?.ok_or_else(integrity_error)?;
            record_lifecycle_target(&mut tx, &current.task_id, &managed_mcp_id).await?;
            claim_lifecycle_lease(
                &mut tx,
                &managed_mcp_id,
                &current.task_id,
                current.operation,
                now_ms,
            )
            .await?;
        }
        current.status.ensure_transition(TaskStatus::Running)?;
        let sequence = current.event_sequence + 1;
        let result = sqlx::query(
            r#"UPDATE tasks SET status = 'running', owner_id = ?, lease_expires_at_ms = ?,
                heartbeat_at_ms = ?, updated_at_ms = ?, revision = revision + 1,
                event_sequence = ? WHERE task_id = ? AND status = 'queued' AND revision = ?
                AND EXISTS (
                    SELECT 1 FROM install_plans
                    WHERE plan_id = ?
                      AND expires_at_ms > CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
                )"#,
        )
        .bind(owner_id)
        .bind(lease_expires_at_ms)
        .bind(now_ms)
        .bind(now_ms)
        .bind(sequence)
        .bind(&current.task_id)
        .bind(current.revision)
        .bind(&current.plan_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(None);
        }
        sqlx::query("UPDATE managed_lifecycle_leases SET expires_at_ms = ? WHERE task_id = ?")
            .bind(lease_expires_at_ms)
            .bind(&current.task_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id: &current.task_id,
                sequence,
                event_type: AuditEventType::TaskStatusChanged,
                actor: owner_id,
                occurred_at_ms: now_ms,
                payload: &AuditPayload::TaskStatusChanged {
                    from: TaskStatus::Queued,
                    to: TaskStatus::Running,
                },
                redacted_error: None,
            },
        )
        .await?;
        let claimed = fetch_task(&mut tx, &current.task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(Some(claimed))
    }

    pub(crate) async fn reject_queued_task_candidate(
        &self,
        candidate: &QueuedTaskCandidate,
        actor: &str,
        now_ms: i64,
        redacted_error: &RedactedError,
    ) -> McpPlatformResult<bool> {
        let mut tx = self.begin_immediate().await?;
        let current = fetch_task(&mut tx, &candidate.task().task_id).await?;
        if current.status != TaskStatus::Queued
            || current.revision != candidate.task().revision
            || current.plan_id != candidate.task().plan_id
            || current.plan_digest != candidate.task().plan_digest
            || current.operation != candidate.task().operation
            || current.actor != candidate.task().actor
            || current.owner_id != candidate.task().owner_id
            || current.lease_expires_at_ms != candidate.task().lease_expires_at_ms
        {
            tx.rollback().await?;
            return Ok(false);
        }
        current.status.ensure_transition(TaskStatus::Cancelled)?;
        let sequence = current.event_sequence + 1;
        let rejected = sqlx::query(
            r#"UPDATE tasks SET status = 'cancelled', updated_at_ms = ?, heartbeat_at_ms = NULL,
                owner_id = NULL, lease_expires_at_ms = NULL, redacted_error_json = ?,
                revision = revision + 1, event_sequence = ?
                WHERE task_id = ? AND status = 'queued' AND revision = ?"#,
        )
        .bind(now_ms)
        .bind(encode(redacted_error)?)
        .bind(sequence)
        .bind(&current.task_id)
        .bind(current.revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if rejected.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM managed_lifecycle_leases WHERE task_id = ?")
            .bind(&current.task_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id: &current.task_id,
                sequence,
                event_type: AuditEventType::TaskStatusChanged,
                actor,
                occurred_at_ms: now_ms,
                payload: &AuditPayload::TaskStatusChanged {
                    from: TaskStatus::Queued,
                    to: TaskStatus::Cancelled,
                },
                redacted_error: Some(redacted_error),
            },
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(true)
    }

    pub(crate) async fn authorize_execution(
        &self,
        task_id: &str,
        owner_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ExecutionAuthorization> {
        let mut tx = self.begin_immediate().await?;
        let task = fetch_task(&mut tx, task_id).await?;
        if task.owner_id.as_deref() != Some(owner_id)
            || task.actor.is_empty()
            || task
                .lease_expires_at_ms
                .is_none_or(|expires_at_ms| expires_at_ms <= now_ms)
            || !matches!(
                task.status,
                TaskStatus::Running
                    | TaskStatus::Verifying
                    | TaskStatus::Activating
                    | TaskStatus::Cancelling
                    | TaskStatus::RollingBack
                    | TaskStatus::Interrupted
            )
        {
            return Err(revision_conflict());
        }
        let plan = fetch_valid_plan(&mut tx, &task.plan_id).await?;
        let health_request = fetch_health_task_request(&mut tx, task_id).await?;
        if task.plan_digest != plan.plan.plan_digest()
            || !task_plan_operations_compatible(
                task.operation,
                plan.plan.operation(),
                health_request.as_ref(),
            )
            || task.actor != plan.actor
        {
            return Err(integrity_error());
        }
        validate_registration_health_binding(self, &mut tx, &task, &plan, health_request.as_ref())
            .await?;
        let (manifest, _) = self.resolve_manifest_for_plan(&plan).await?;
        if manifest.verified.digest() != plan.plan.manifest_digest()
            || manifest.verified.manifest().id != plan.plan.manifest_id()
            || manifest.verified.manifest().version.as_str() != plan.plan.manifest_version()
        {
            return Err(integrity_error());
        }
        let step_rows = sqlx::query("SELECT * FROM task_steps WHERE task_id=? ORDER BY ordinal")
            .bind(task_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        let mut steps = Vec::with_capacity(step_rows.len());
        for row in &step_rows {
            let step = decode_task_step_row(row)?;
            verify_task_step_compensation_binding(
                &mut tx,
                self.integrity_signer.as_ref(),
                &step,
                row,
            )
            .await?;
            steps.push(step);
        }
        let managed_mcp_id = if task.operation == crate::mcp_platform::task::TaskOperation::Register
        {
            lifecycle_target_for_plan(&plan, task.operation)?
        } else {
            health_request
                .as_ref()
                .map(|request| request.managed_mcp_id.clone())
                .or_else(|| plan.target.managed_mcp_id.clone())
                .or_else(|| {
                    plan.target.installation_scope.as_deref().map(|scope| {
                        crate::mcp_platform::task_runner::stable_managed_mcp_id(
                            plan.plan.manifest_id(),
                            scope,
                        )
                    })
                })
        };
        let inventory = if let Some(managed_mcp_id) = managed_mcp_id.as_deref() {
            match Self::fetch_managed_inventory_snapshot(&mut tx, managed_mcp_id).await {
                Ok(inventory) => Some(inventory),
                Err(error) if error.code() == McpPlatformErrorCode::NotFound => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let projection = if let Some(managed_mcp_id) = managed_mcp_id.as_deref() {
            self.fetch_connection_projection_snapshot(&mut tx, managed_mcp_id)
                .await?
        } else {
            None
        };
        let checkpoint_root = tx.checkpoint.root.clone();
        let checkpoint_sequence = tx.checkpoint.sequence;
        tx.rollback().await?;
        Ok(ExecutionAuthorization::new(
            checkpoint_root,
            checkpoint_sequence,
            task,
            plan,
            manifest,
            steps,
            health_request,
            inventory,
            projection,
            self.authorization_issuer.clone(),
        ))
    }

    pub(crate) async fn validate_execution_authorization(
        &self,
        authorization: &ExecutionAuthorization,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        if !authorization.issued_by(&self.authorization_issuer) {
            return Err(integrity_error());
        }
        let mut tx = self.begin_immediate().await?;
        if tx.checkpoint.root != authorization.checkpoint_root
            || tx.checkpoint.sequence != authorization.checkpoint_sequence
        {
            return Err(revision_conflict());
        }
        let current = fetch_task(&mut tx, &authorization.task.task_id).await?;
        if current.revision != authorization.task.revision
            || current.owner_id != authorization.task.owner_id
            || current.owner_id.is_none()
            || current
                .lease_expires_at_ms
                .is_none_or(|expires_at_ms| expires_at_ms <= now_ms)
            || current.plan_digest != authorization.task.plan_digest
            || current.operation != authorization.task.operation
        {
            return Err(revision_conflict());
        }
        tx.rollback().await
    }

    pub async fn latest_managed_lifecycle_task(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        let row = sqlx::query(
            r#"SELECT t.* FROM lifecycle_task_targets ltt
               JOIN tasks t ON t.task_id = ltt.task_id
               WHERE ltt.managed_mcp_id = ?
               ORDER BY t.updated_at_ms DESC, t.created_at_ms DESC, t.task_id DESC
               LIMIT 1"#,
        )
        .bind(managed_mcp_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        row.as_ref().map(decode_task_row).transpose()
    }

    pub(crate) async fn create_task(&self, input: CreateTask<'_>) -> McpPlatformResult<TaskRecord> {
        let mut tx = self.begin_immediate().await?;
        let plan = fetch_valid_plan(&mut tx, input.plan_id).await?;
        if plan.plan.plan_digest() != input.plan_digest
            || plan.plan.operation().as_str() != input.operation.as_str()
        {
            return Err(integrity_error());
        }
        if let Some(row) =
            sqlx::query("SELECT * FROM tasks WHERE operation = ? AND idempotency_key = ?")
                .bind(input.operation.as_str())
                .bind(input.idempotency_key)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?
        {
            let existing = decode_task_row(&row)?;
            if existing.plan_id != input.plan_id
                || existing.plan_digest != input.plan_digest
                || existing.actor != input.actor
                || existing.adapter_evidence.as_ref() != input.adapter_evidence
            {
                return Err(error(
                    McpPlatformErrorCode::IdempotencyConflict,
                    "task idempotency key already refers to different content",
                ));
            }
            if managed_lifecycle_operation(existing.operation) {
                let managed_mcp_id = lifecycle_target_for_plan(&plan, existing.operation)?
                    .ok_or_else(integrity_error)?;
                record_lifecycle_target(&mut tx, &existing.task_id, &managed_mcp_id).await?;
                if !matches!(
                    existing.status,
                    TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled
                ) {
                    claim_lifecycle_lease(
                        &mut tx,
                        &managed_mcp_id,
                        &existing.task_id,
                        existing.operation,
                        input.now_ms,
                    )
                    .await?;
                }
            }
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(existing);
        }
        if input.now_ms >= plan.expires_at_ms {
            return Err(error(
                McpPlatformErrorCode::PlanExpired,
                "installation plan has expired",
            ));
        }
        if sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id = ?)")
            .bind(input.task_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?
        {
            return Err(error(
                McpPlatformErrorCode::IdempotencyConflict,
                "task identifier already exists",
            ));
        }
        sqlx::query(
            r#"INSERT INTO tasks (
                task_id, plan_id, plan_digest, operation, idempotency_key, status, actor,
                created_at_ms, updated_at_ms, heartbeat_at_ms, progress, step_cursor,
                adapter_evidence_json, redacted_error_json, rollback_status,
                rollback_evidence_json, revision, event_sequence
            ) VALUES (?, ?, ?, ?, ?, 'planned', ?, ?, ?, NULL, 0, 0, ?, NULL,
                'not_required', ?, 0, 1)"#,
        )
        .bind(input.task_id)
        .bind(input.plan_id)
        .bind(input.plan_digest)
        .bind(input.operation.as_str())
        .bind(input.idempotency_key)
        .bind(input.actor)
        .bind(input.now_ms)
        .bind(input.now_ms)
        .bind(encode_optional(input.adapter_evidence)?)
        .bind(encode_optional(input.rollback_evidence)?)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if managed_lifecycle_operation(input.operation) {
            let managed_mcp_id =
                lifecycle_target_for_plan(&plan, input.operation)?.ok_or_else(integrity_error)?;
            record_lifecycle_target(&mut tx, input.task_id, &managed_mcp_id).await?;
            claim_lifecycle_lease(
                &mut tx,
                &managed_mcp_id,
                input.task_id,
                input.operation,
                input.now_ms,
            )
            .await?;
        }
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id: input.task_id,
                sequence: 1,
                event_type: AuditEventType::TaskCreated,
                actor: input.actor,
                occurred_at_ms: input.now_ms,
                payload: &AuditPayload::TaskCreated {
                    status: TaskStatus::Planned,
                },
                redacted_error: None,
            },
        )
        .await?;
        let task = fetch_task(&mut tx, input.task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(task)
    }

    pub async fn get_task(&self, task_id: &str) -> McpPlatformResult<TaskRecord> {
        let row = sqlx::query("SELECT * FROM tasks WHERE task_id = ?")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .ok_or_else(not_found)?;
        decode_task_row(&row)
    }

    pub async fn get_task_by_idempotency_key(
        &self,
        operation: crate::mcp_platform::task::TaskOperation,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        let task_id = sqlx::query_scalar::<_, String>(
            "SELECT task_id FROM tasks WHERE operation = ? AND idempotency_key = ?",
        )
        .bind(operation.as_str())
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        match task_id {
            Some(task_id) => self.get_task(&task_id).await.map(Some),
            None => Ok(None),
        }
    }

    pub(crate) async fn transition_task(
        &self,
        transition: TaskTransition<'_>,
    ) -> McpPlatformResult<TaskRecord> {
        if transition.progress > 100 {
            return Err(integrity_error());
        }
        let mut tx = self.begin_immediate().await?;
        let current = fetch_task(&mut tx, transition.task_id).await?;
        if current.revision != transition.expected_revision {
            return Err(revision_conflict());
        }
        if current.owner_id.is_some()
            && (current.owner_id.as_deref() != Some(transition.actor)
                || current
                    .lease_expires_at_ms
                    .is_none_or(|expires_at_ms| expires_at_ms <= transition.now_ms))
        {
            return Err(revision_conflict());
        }
        current.status.ensure_transition(transition.next_status)?;
        let sequence = current.event_sequence + 1;
        let release_lease = transition.next_status == TaskStatus::Queued;
        sqlx::query(
            r#"UPDATE tasks SET status = ?, updated_at_ms = ?,
                heartbeat_at_ms = CASE WHEN ? THEN NULL ELSE ? END,
                owner_id = CASE WHEN ? THEN NULL ELSE owner_id END,
                lease_expires_at_ms = CASE WHEN ? THEN NULL ELSE lease_expires_at_ms END,
                progress = ?, redacted_error_json = ?, rollback_status = ?,
                rollback_evidence_json = ?, revision = revision + 1, event_sequence = ?
                WHERE task_id = ? AND revision = ?"#,
        )
        .bind(transition.next_status.as_str())
        .bind(transition.now_ms)
        .bind(release_lease)
        .bind(transition.heartbeat_at_ms)
        .bind(release_lease)
        .bind(release_lease)
        .bind(i64::from(transition.progress))
        .bind(encode_optional(transition.redacted_error)?)
        .bind(transition.rollback_status.as_str())
        .bind(encode_optional(transition.rollback_evidence)?)
        .bind(sequence)
        .bind(transition.task_id)
        .bind(transition.expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if transition.next_status == TaskStatus::Succeeded
            || (matches!(
                transition.next_status,
                TaskStatus::Failed | TaskStatus::Cancelled
            ) && transition.rollback_status
                != crate::mcp_platform::task::RollbackStatus::Incomplete)
        {
            sqlx::query("DELETE FROM managed_lifecycle_leases WHERE task_id = ?")
                .bind(transition.task_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        }
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id: transition.task_id,
                sequence,
                event_type: AuditEventType::TaskStatusChanged,
                actor: transition.actor,
                occurred_at_ms: transition.now_ms,
                payload: &AuditPayload::TaskStatusChanged {
                    from: current.status,
                    to: transition.next_status,
                },
                redacted_error: transition.redacted_error,
            },
        )
        .await?;
        let updated = fetch_task(&mut tx, transition.task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub(crate) async fn heartbeat_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        now_ms: i64,
        progress: u8,
    ) -> McpPlatformResult<TaskRecord> {
        if progress > 100 {
            return Err(integrity_error());
        }
        let mut tx = self.begin_immediate().await?;
        let current = fetch_task(&mut tx, task_id).await?;
        if current.revision != expected_revision {
            return Err(revision_conflict());
        }
        if !matches!(
            current.status,
            TaskStatus::Running
                | TaskStatus::Cancelling
                | TaskStatus::Verifying
                | TaskStatus::Activating
                | TaskStatus::RollingBack
        ) {
            return Err(error(
                McpPlatformErrorCode::InvalidTransition,
                "only active tasks accept heartbeat updates",
            ));
        }
        if progress < current.progress {
            return Err(integrity_error());
        }
        sqlx::query(
            r#"UPDATE tasks SET heartbeat_at_ms = ?, progress = ?, updated_at_ms = ?,
                revision = revision + 1 WHERE task_id = ? AND revision = ?"#,
        )
        .bind(now_ms)
        .bind(i64::from(progress))
        .bind(now_ms)
        .bind(task_id)
        .bind(expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let updated = fetch_task(&mut tx, task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub(crate) async fn add_task_step(
        &self,
        task_id: &str,
        ordinal: i64,
        idempotency_token: &str,
        compensation: &CompensationDescriptor,
        owner_id: &str,
        expected_task_revision: i64,
        now_ms: i64,
    ) -> McpPlatformResult<TaskStepRecord> {
        let authorization = self.authorize_execution(task_id, owner_id, now_ms).await?;
        if authorization.task.revision != expected_task_revision {
            return Err(revision_conflict());
        }
        let task = authorization.task;
        let adapter_id = task
            .adapter_evidence
            .as_ref()
            .map_or("", |value| value.adapter_id.as_str());
        let adapter_version = task
            .adapter_evidence
            .as_ref()
            .map_or("", |value| value.adapter_version.as_str());
        self.add_task_step_with_adapter(
            task_id,
            ordinal,
            idempotency_token,
            compensation,
            adapter_id,
            adapter_version,
            owner_id,
            expected_task_revision,
            now_ms,
        )
        .await
    }

    pub(crate) async fn add_task_step_with_adapter(
        &self,
        task_id: &str,
        ordinal: i64,
        idempotency_token: &str,
        compensation: &CompensationDescriptor,
        adapter_id: &str,
        adapter_version: &str,
        owner_id: &str,
        expected_task_revision: i64,
        now_ms: i64,
    ) -> McpPlatformResult<TaskStepRecord> {
        if ordinal < 0 {
            return Err(integrity_error());
        }
        let compensation_json = encode(compensation)?;
        let mut tx = self.begin_immediate().await?;
        let task = fetch_task(&mut tx, task_id).await?;
        if task.owner_id.as_deref() != Some(owner_id)
            || task.revision != expected_task_revision
            || task
                .lease_expires_at_ms
                .is_none_or(|expires_at_ms| expires_at_ms <= now_ms)
        {
            return Err(revision_conflict());
        }
        if let Some(row) = sqlx::query("SELECT * FROM task_steps WHERE task_id = ? AND ordinal = ?")
            .bind(task_id)
            .bind(ordinal)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_sqlx)?
        {
            let existing = decode_task_step_row(&row)?;
            verify_task_step_compensation_binding(
                &mut tx,
                self.integrity_signer.as_ref(),
                &existing,
                &row,
            )
            .await?;
            tx.commit().await.map_err(map_sqlx)?;
            return if existing.idempotency_token == idempotency_token
                && existing.compensation == *compensation
            {
                Ok(existing)
            } else {
                Err(error(
                    McpPlatformErrorCode::IdempotencyConflict,
                    "task step identity already has different content",
                ))
            };
        }
        if sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM task_steps WHERE task_id = ? AND idempotency_token = ?)",
        )
        .bind(task_id)
        .bind(idempotency_token)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?
        {
            return Err(error(
                McpPlatformErrorCode::IdempotencyConflict,
                "task step token is already in use",
            ));
        }
        sqlx::query(
            r#"INSERT INTO task_steps (
                task_id, ordinal, status, idempotency_token, compensation_json,
                evidence_json, started_at_ms, committed_at_ms
                , adapter_id, adapter_version
            ) VALUES (?, ?, 'not_started', ?, ?, NULL, NULL, NULL, ?, ?)"#,
        )
        .bind(task_id)
        .bind(ordinal)
        .bind(idempotency_token)
        .bind(&compensation_json)
        .bind(adapter_id)
        .bind(adapter_version)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let unsigned_step = TaskStepRecord {
            task_id: task_id.to_string(),
            ordinal,
            status: TaskStepStatus::NotStarted,
            idempotency_token: idempotency_token.to_string(),
            compensation: compensation.clone(),
            evidence: None,
            started_at_ms: None,
            committed_at_ms: None,
            adapter_id: adapter_id.to_string(),
            adapter_version: adapter_version.to_string(),
            compensation_status: CompensationStatus::Pending,
            compensation_started_at_ms: None,
            compensation_committed_at_ms: None,
        };
        sqlx::query(
            "INSERT INTO task_step_compensation_bindings(task_id,ordinal,mac) VALUES (?,?,?)",
        )
        .bind(task_id)
        .bind(ordinal)
        .bind(self.integrity_signer.sign(
            "task-step-compensation",
            &task_step_compensation_payload(&task, &unsigned_step, &compensation_json, None),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let step =
            fetch_task_step(&mut tx, self.integrity_signer.as_ref(), task_id, ordinal).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(step)
    }

    pub(crate) async fn transition_task_step(
        &self,
        transition: StepTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord> {
        if !transition
            .expected_status
            .can_transition_to(transition.next_status)
        {
            return Err(error(
                McpPlatformErrorCode::InvalidTransition,
                "task step status transition is not allowed",
            ));
        }
        let mut tx = self.begin_immediate().await?;
        let task = fetch_task(&mut tx, transition.task_id).await?;
        if task.owner_id.as_deref() != Some(transition.owner_id)
            || transition.actor != transition.owner_id
            || task.revision != transition.expected_task_revision
            || task
                .lease_expires_at_ms
                .is_none_or(|expires_at_ms| expires_at_ms <= transition.now_ms)
        {
            return Err(revision_conflict());
        }
        let step = fetch_task_step(
            &mut tx,
            self.integrity_signer.as_ref(),
            transition.task_id,
            transition.ordinal,
        )
        .await?;
        if step.status != transition.expected_status {
            return Err(error(
                McpPlatformErrorCode::InvalidTransition,
                "task step no longer has the expected status",
            ));
        }
        let (started_at_ms, committed_at_ms) = match transition.next_status {
            TaskStepStatus::Started => (Some(transition.now_ms), step.committed_at_ms),
            TaskStepStatus::Committed => (step.started_at_ms, Some(transition.now_ms)),
            TaskStepStatus::NotStarted => unreachable!(),
        };
        sqlx::query(
            r#"UPDATE task_steps SET status = ?, evidence_json = ?, started_at_ms = ?,
                committed_at_ms = ? WHERE task_id = ? AND ordinal = ?"#,
        )
        .bind(transition.next_status.as_str())
        .bind(encode_optional(transition.evidence)?)
        .bind(started_at_ms)
        .bind(committed_at_ms)
        .bind(transition.task_id)
        .bind(transition.ordinal)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let sequence = task.event_sequence + 1;
        sqlx::query(
            r#"UPDATE tasks SET step_cursor = MAX(step_cursor, ?), revision = revision + 1,
                event_sequence = ?, updated_at_ms = ? WHERE task_id = ? AND revision = ?
                AND owner_id = ? AND lease_expires_at_ms > ?"#,
        )
        .bind(transition.ordinal)
        .bind(sequence)
        .bind(transition.now_ms)
        .bind(transition.task_id)
        .bind(transition.expected_task_revision)
        .bind(transition.owner_id)
        .bind(transition.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        refresh_task_step_binding(
            &mut tx,
            self.integrity_signer.as_ref(),
            &task,
            transition.task_id,
            transition.ordinal,
        )
        .await?;
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id: transition.task_id,
                sequence,
                event_type: if transition.next_status == TaskStepStatus::Started {
                    AuditEventType::StepStarted
                } else {
                    AuditEventType::StepCommitted
                },
                actor: transition.actor,
                occurred_at_ms: transition.now_ms,
                payload: &AuditPayload::StepStatusChanged {
                    ordinal: transition.ordinal,
                    from: transition.expected_status,
                    to: transition.next_status,
                },
                redacted_error: None,
            },
        )
        .await?;
        let updated = fetch_task_step(
            &mut tx,
            self.integrity_signer.as_ref(),
            transition.task_id,
            transition.ordinal,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub async fn list_task_steps(&self, task_id: &str) -> McpPlatformResult<Vec<TaskStepRecord>> {
        let mut tx = self.begin_immediate().await?;
        let rows = sqlx::query("SELECT * FROM task_steps WHERE task_id = ? ORDER BY ordinal")
            .bind(task_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        let mut steps = Vec::with_capacity(rows.len());
        for row in &rows {
            let step = decode_task_step_row(row)?;
            verify_task_step_compensation_binding(
                &mut tx,
                self.integrity_signer.as_ref(),
                &step,
                row,
            )
            .await?;
            steps.push(step);
        }
        tx.rollback().await?;
        Ok(steps)
    }

    pub(crate) async fn transition_compensation(
        &self,
        transition: CompensationTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord> {
        if !transition
            .expected_status
            .can_transition_to(transition.next_status)
        {
            return Err(error(
                McpPlatformErrorCode::InvalidTransition,
                "task compensation status transition is not allowed",
            ));
        }
        let mut tx = self.begin_immediate().await?;
        let task = fetch_task(&mut tx, transition.task_id).await?;
        if task.owner_id.as_deref() != Some(transition.owner_id)
            || transition.actor != transition.owner_id
            || task.revision != transition.expected_task_revision
            || task
                .lease_expires_at_ms
                .is_none_or(|expires_at_ms| expires_at_ms <= transition.now_ms)
        {
            return Err(revision_conflict());
        }
        let step = fetch_task_step(
            &mut tx,
            self.integrity_signer.as_ref(),
            transition.task_id,
            transition.ordinal,
        )
        .await?;
        if step.compensation_status != transition.expected_status {
            return Err(error(
                McpPlatformErrorCode::InvalidTransition,
                "task compensation no longer has the expected status",
            ));
        }
        let (started_at_ms, committed_at_ms) = match transition.next_status {
            CompensationStatus::Started => {
                (Some(transition.now_ms), step.compensation_committed_at_ms)
            }
            CompensationStatus::Committed => {
                (step.compensation_started_at_ms, Some(transition.now_ms))
            }
            CompensationStatus::Pending => unreachable!(),
        };
        sqlx::query(
            r#"UPDATE task_steps SET compensation_status = ?, compensation_started_at_ms = ?,
                compensation_committed_at_ms = ? WHERE task_id = ? AND ordinal = ?"#,
        )
        .bind(transition.next_status.as_str())
        .bind(started_at_ms)
        .bind(committed_at_ms)
        .bind(transition.task_id)
        .bind(transition.ordinal)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"UPDATE tasks SET updated_at_ms = ?, revision = revision + 1
               WHERE task_id = ? AND revision = ? AND owner_id = ?
                 AND lease_expires_at_ms > ?"#,
        )
        .bind(transition.now_ms)
        .bind(transition.task_id)
        .bind(transition.expected_task_revision)
        .bind(transition.owner_id)
        .bind(transition.now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        refresh_task_step_binding(
            &mut tx,
            self.integrity_signer.as_ref(),
            &task,
            transition.task_id,
            transition.ordinal,
        )
        .await?;
        let updated = fetch_task_step(
            &mut tx,
            self.integrity_signer.as_ref(),
            transition.task_id,
            transition.ordinal,
        )
        .await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub(crate) async fn confirm_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        let mut tx = self.begin_immediate().await?;
        let current = fetch_task(&mut tx, task_id).await?;
        if current.status == TaskStatus::Queued {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(current);
        }
        if current.revision != expected_revision {
            return Err(revision_conflict());
        }
        if current.status != TaskStatus::AwaitingConfirmation {
            return Err(error(
                McpPlatformErrorCode::InvalidTransition,
                "task is not awaiting confirmation",
            ));
        }
        current.status.ensure_transition(TaskStatus::Queued)?;
        let plan = fetch_valid_plan(&mut tx, &current.plan_id).await?;
        if now_ms >= plan.expires_at_ms {
            return Err(error(
                McpPlatformErrorCode::PlanExpired,
                "installation plan has expired",
            ));
        }
        let sequence = current.event_sequence + 1;
        sqlx::query(
            r#"UPDATE tasks SET status = 'queued', updated_at_ms = ?, heartbeat_at_ms = NULL,
                revision = revision + 1, event_sequence = ?
                WHERE task_id = ? AND revision = ?"#,
        )
        .bind(now_ms)
        .bind(sequence)
        .bind(task_id)
        .bind(expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id,
                sequence,
                event_type: AuditEventType::ConfirmationRecorded,
                actor,
                occurred_at_ms: now_ms,
                payload: &AuditPayload::ConfirmationRecorded {
                    from: current.status,
                    to: TaskStatus::Queued,
                    plan_id: current.plan_id,
                    plan_digest: current.plan_digest,
                },
                redacted_error: None,
            },
        )
        .await?;
        let updated = fetch_task(&mut tx, task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub(crate) async fn request_cancel(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        let mut tx = self.begin_immediate().await?;
        let current = fetch_task(&mut tx, task_id).await?;
        let next_status = match current.status {
            TaskStatus::AwaitingConfirmation | TaskStatus::Queued => TaskStatus::Cancelled,
            TaskStatus::Running | TaskStatus::Verifying | TaskStatus::Activating => {
                TaskStatus::Cancelling
            }
            TaskStatus::Cancelling => {
                tx.commit().await.map_err(map_sqlx)?;
                return Ok(current);
            }
            TaskStatus::Cancelled => {
                sqlx::query("DELETE FROM managed_lifecycle_leases WHERE task_id = ?")
                    .bind(task_id)
                    .execute(&mut **tx)
                    .await
                    .map_err(map_sqlx)?;
                tx.commit().await.map_err(map_sqlx)?;
                return Ok(current);
            }
            _ => {
                return Err(error(
                    McpPlatformErrorCode::InvalidTransition,
                    "task is not cancellable in its current state",
                ));
            }
        };
        if current.revision != expected_revision {
            return Err(revision_conflict());
        }
        current.status.ensure_transition(next_status)?;
        let sequence = current.event_sequence + 1;
        sqlx::query(
            r#"UPDATE tasks SET status = ?, updated_at_ms = ?, revision = revision + 1,
                event_sequence = ? WHERE task_id = ? AND revision = ?"#,
        )
        .bind(next_status.as_str())
        .bind(now_ms)
        .bind(sequence)
        .bind(task_id)
        .bind(expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if next_status == TaskStatus::Cancelled {
            sqlx::query("DELETE FROM managed_lifecycle_leases WHERE task_id = ?")
                .bind(task_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        }
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id,
                sequence,
                event_type: AuditEventType::CancellationRequested,
                actor,
                occurred_at_ms: now_ms,
                payload: &AuditPayload::CancellationRequested {
                    from: current.status,
                    to: next_status,
                },
                redacted_error: current.redacted_error.as_ref(),
            },
        )
        .await?;
        let updated = fetch_task(&mut tx, task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub(crate) async fn retry_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        retry_idempotency_key: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        let mut tx = self.begin_immediate().await?;
        let current = fetch_task(&mut tx, task_id).await?;
        let replay_attempt = sqlx::query_scalar::<_, i64>(
            "SELECT attempt FROM task_retry_attempts WHERE task_id = ? AND idempotency_key = ?",
        )
        .bind(task_id)
        .bind(retry_idempotency_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if replay_attempt.is_some() {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(current);
        }
        if current.revision != expected_revision {
            return Err(revision_conflict());
        }
        current.status.ensure_transition(TaskStatus::Queued)?;
        let sequence = current.event_sequence + 1;
        let attempt = current.attempt_count + 1;
        let preserve_finalization = current.status == TaskStatus::RecoveryRequired
            && sqlx::query_scalar::<_, bool>(
                r#"SELECT EXISTS(
                    SELECT 1 FROM uninstall_journal WHERE task_id = ? AND status = 'committed'
                    UNION ALL
                    SELECT 1 FROM activation_journal a WHERE a.task_id = ? AND (
                        a.status = 'cleanup_committed' OR (
                            a.status = 'health_committed' AND EXISTS(
                                SELECT 1 FROM task_steps s WHERE s.task_id = a.task_id
                                AND s.ordinal >= 10 AND s.status = 'started'
                            )
                        )
                    )
                )"#,
            )
            .bind(task_id)
            .bind(task_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        let plan = fetch_valid_plan(&mut tx, &current.plan_id).await?;
        let lifecycle_target = if managed_lifecycle_operation(current.operation) {
            let managed_mcp_id =
                lifecycle_target_for_plan(&plan, current.operation)?.ok_or_else(integrity_error)?;
            record_lifecycle_target(&mut tx, task_id, &managed_mcp_id).await?;
            Some(managed_mcp_id)
        } else {
            None
        };
        if let Some(managed_mcp_id) = lifecycle_target.as_deref() {
            if preserve_finalization {
                let owner = sqlx::query_scalar::<_, String>(
                    "SELECT task_id FROM managed_lifecycle_leases WHERE managed_mcp_id = ?",
                )
                .bind(managed_mcp_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(map_sqlx)?;
                if owner.as_deref() != Some(task_id) {
                    return Err(integrity_error());
                }
            } else {
                claim_lifecycle_lease(&mut tx, managed_mcp_id, task_id, current.operation, now_ms)
                    .await?;
            }
        }
        if !preserve_finalization {
            sqlx::query(
                r#"INSERT INTO task_step_history (
                task_id, attempt, ordinal, status, idempotency_token, compensation_json,
                evidence_json, started_at_ms, committed_at_ms, adapter_id, adapter_version,
                compensation_status, compensation_started_at_ms, compensation_committed_at_ms
            ) SELECT task_id, ?, ordinal, status, idempotency_token, compensation_json,
                evidence_json, started_at_ms, committed_at_ms, adapter_id, adapter_version,
                compensation_status, compensation_started_at_ms, compensation_committed_at_ms
                FROM task_steps WHERE task_id = ?"#,
            )
            .bind(current.attempt_count)
            .bind(task_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
            let step_rows =
                sqlx::query("SELECT * FROM task_steps WHERE task_id = ? ORDER BY ordinal")
                    .bind(task_id)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(map_sqlx)?;
            for row in step_rows {
                let step = decode_task_step_row(&row)?;
                verify_task_step_compensation_binding(
                    &mut tx,
                    self.integrity_signer.as_ref(),
                    &step,
                    &row,
                )
                .await?;
                let compensation_json = row
                    .try_get::<String, _>("compensation_json")
                    .map_err(map_sqlx)?;
                let mac = self.integrity_signer.sign(
                    "task-step-compensation-history",
                    &task_step_history_compensation_payload(
                        &current,
                        current.attempt_count,
                        &step,
                        &compensation_json,
                    ),
                )?;
                sqlx::query(
                    "INSERT INTO task_step_history_compensation_bindings(task_id,attempt,ordinal,mac) VALUES (?,?,?,?)",
                )
                .bind(task_id)
                .bind(current.attempt_count)
                .bind(step.ordinal)
                .bind(mac)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
            }
            sqlx::query("DELETE FROM task_step_compensation_bindings WHERE task_id = ?")
                .bind(task_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
            sqlx::query("DELETE FROM task_steps WHERE task_id = ?")
                .bind(task_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
            sqlx::query("DELETE FROM health_observations WHERE task_id = ?")
                .bind(task_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
            sqlx::query(
                "DELETE FROM activation_journal WHERE task_id = ? AND status = 'rolled_back'",
            )
            .bind(task_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
            sqlx::query("DELETE FROM uninstall_journal WHERE task_id = ? AND status = 'cancelled'")
                .bind(task_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        }
        sqlx::query(
            r#"INSERT INTO task_retry_attempts (
                task_id, attempt, idempotency_key, requested_from_status, actor, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?)"#,
        )
        .bind(task_id)
        .bind(attempt)
        .bind(retry_idempotency_key)
        .bind(current.status.as_str())
        .bind(actor)
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"UPDATE tasks SET status = 'queued', retry_idempotency_key = ?, attempt_count = ?,
                updated_at_ms = ?, heartbeat_at_ms = NULL, revision = revision + 1,
                event_sequence = ?, owner_id = NULL, lease_expires_at_ms = NULL,
                redacted_error_json = NULL, finalization_failures = 0
                WHERE task_id = ? AND revision = ?"#,
        )
        .bind(retry_idempotency_key)
        .bind(attempt)
        .bind(now_ms)
        .bind(sequence)
        .bind(task_id)
        .bind(expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        append_audit(
            &mut tx,
            NewAuditEvent {
                task_id,
                sequence,
                event_type: AuditEventType::RetryQueued,
                actor,
                occurred_at_ms: now_ms,
                payload: &AuditPayload::TaskStatusChanged {
                    from: current.status,
                    to: TaskStatus::Queued,
                },
                redacted_error: current.redacted_error.as_ref(),
            },
        )
        .await?;
        let updated = fetch_task(&mut tx, task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub(crate) async fn claim_next_task(
        &self,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        loop {
            let Some(candidate) = self.next_queued_task_candidate(now_ms).await? else {
                return Ok(None);
            };
            let preflight = match self.load_queued_task_preflight(&candidate).await {
                Ok(preflight) => preflight,
                Err(error) if error.code() == McpPlatformErrorCode::RevisionConflict => {
                    self.reject_queued_task_candidate(
                        &candidate,
                        owner_id,
                        now_ms,
                        &RedactedError::new(
                            RedactedErrorCode::VerificationFailed,
                            "MCP task candidate changed during preflight",
                            std::iter::empty::<&str>(),
                        ),
                    )
                    .await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Some(task) = self
                .claim_preflighted_task(&preflight, owner_id, now_ms, lease_duration_ms)
                .await?
            {
                return Ok(Some(task));
            }
        }
    }

    pub(crate) async fn renew_task_lease(
        &self,
        task_id: &str,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        let lease_expires_at_ms = now_ms
            .checked_add(lease_duration_ms)
            .ok_or_else(integrity_error)?;
        let mut tx = self.begin_immediate().await?;
        let result = sqlx::query(
            r#"UPDATE tasks
                SET heartbeat_at_ms = ?, lease_expires_at_ms = ?, updated_at_ms = ?,
                    revision = revision + 1
                WHERE task_id = ? AND owner_id = ? AND status IN (
                    'running','cancelling','verifying','activating','rolling_back'
                ) AND lease_expires_at_ms > ?"#,
        )
        .bind(now_ms)
        .bind(lease_expires_at_ms)
        .bind(now_ms)
        .bind(task_id)
        .bind(owner_id)
        .bind(now_ms)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(error(
                McpPlatformErrorCode::RevisionConflict,
                "task lease is no longer owned by this worker",
            ));
        }
        sqlx::query(
            "UPDATE managed_lifecycle_leases SET expires_at_ms = ? WHERE task_id = ? AND expires_at_ms > ?",
        )
            .bind(lease_expires_at_ms)
            .bind(task_id)
            .bind(now_ms)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        let task = fetch_task(&mut tx, task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(task)
    }

    pub(crate) async fn recover_stale_tasks(
        &self,
        heartbeat_cutoff_ms: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<Vec<RecoveryRecord>> {
        let mut tx = self.begin_immediate().await?;
        let rows = sqlx::query(
            r#"SELECT * FROM tasks WHERE status IN (
                'running','verifying','activating','cancelling','rolling_back'
            ) AND COALESCE(heartbeat_at_ms, updated_at_ms) < ? ORDER BY created_at_ms, task_id"#,
        )
        .bind(heartbeat_cutoff_ms)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        let mut recovered = Vec::with_capacity(rows.len());
        let recovery_lease_expires_at_ms =
            now_ms.checked_add(30_000).ok_or_else(integrity_error)?;
        for row in rows {
            let task = decode_task_row(&row)?;
            let step_rows =
                sqlx::query("SELECT * FROM task_steps WHERE task_id = ? ORDER BY ordinal")
                    .bind(&task.task_id)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(map_sqlx)?;
            let mut decoded_steps = Vec::with_capacity(step_rows.len());
            for step_row in &step_rows {
                let step = decode_task_step_row(step_row)?;
                verify_task_step_compensation_binding(
                    &mut tx,
                    self.integrity_signer.as_ref(),
                    &step,
                    step_row,
                )
                .await?;
                decoded_steps.push(step);
            }
            let decision = recovery_decision(&task, &decoded_steps);
            let sequence = task.event_sequence + 1;
            let claimed = sqlx::query(
                r#"UPDATE tasks SET status = 'interrupted', updated_at_ms = ?,
                    heartbeat_at_ms=?,owner_id=?,lease_expires_at_ms=?,
                    revision = revision + 1, event_sequence = ? WHERE task_id = ?
                    AND revision=? AND lease_expires_at_ms<=?"#,
            )
            .bind(now_ms)
            .bind(now_ms)
            .bind(actor)
            .bind(recovery_lease_expires_at_ms)
            .bind(sequence)
            .bind(&task.task_id)
            .bind(task.revision)
            .bind(now_ms)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
            if claimed.rows_affected() != 1 {
                continue;
            }
            sqlx::query("UPDATE managed_lifecycle_leases SET expires_at_ms=? WHERE task_id=?")
                .bind(recovery_lease_expires_at_ms)
                .bind(&task.task_id)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
            append_audit(
                &mut tx,
                NewAuditEvent {
                    task_id: &task.task_id,
                    sequence,
                    event_type: AuditEventType::RecoveryInterrupted,
                    actor,
                    occurred_at_ms: now_ms,
                    payload: &AuditPayload::RecoveryDecision {
                        decision: decision.clone(),
                    },
                    redacted_error: task.redacted_error.as_ref(),
                },
            )
            .await?;
            let interrupted = fetch_task(&mut tx, &task.task_id).await?;
            recovered.push(RecoveryRecord {
                task: interrupted,
                decision,
            });
        }
        tx.commit().await.map_err(map_sqlx)?;
        Ok(recovered)
    }

    pub async fn list_audit_events(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<Vec<AuditEventRecord>> {
        let rows = sqlx::query("SELECT * FROM audit_events WHERE task_id = ? ORDER BY sequence")
            .bind(task_id)
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;
        rows.iter().map(decode_audit_row).collect()
    }

    pub async fn list_global_audit_events(
        &self,
        after_event_id: i64,
        limit: usize,
        task_ids: &[String],
    ) -> McpPlatformResult<GlobalAuditPage> {
        let high_water_event_id =
            sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(event_id), ?) FROM audit_events")
                .bind(after_event_id)
                .fetch_one(&self.pool)
                .await
                .map_err(map_sqlx)?;

        if high_water_event_id <= after_event_id {
            return Ok(GlobalAuditPage {
                events: Vec::new(),
                scanned_through_event_id: after_event_id,
            });
        }

        let mut query = QueryBuilder::<Sqlite>::new("SELECT * FROM audit_events WHERE event_id > ");
        query
            .push_bind(after_event_id)
            .push(" AND event_id <= ")
            .push_bind(high_water_event_id);
        if !task_ids.is_empty() {
            query.push(" AND task_id IN (");
            let mut task_id_bindings = query.separated(", ");
            for task_id in task_ids {
                task_id_bindings.push_bind(task_id);
            }
            task_id_bindings.push_unseparated(")");
        }
        query
            .push(" ORDER BY event_id LIMIT ")
            .push_bind(i64::try_from(limit).map_err(|_| {
                error(
                    McpPlatformErrorCode::InvalidRequest,
                    "event page limit exceeds the supported range",
                )
            })?);

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;
        let events = rows
            .iter()
            .map(decode_audit_row)
            .collect::<McpPlatformResult<Vec<_>>>()?;
        let scanned_through_event_id = if events.len() == limit {
            events.last().map_or(after_event_id, |event| event.event_id)
        } else {
            high_water_event_id
        };

        Ok(GlobalAuditPage {
            events,
            scanned_through_event_id,
        })
    }
}

async fn refresh_task_step_binding(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    signer: &dyn super::integrity::IntegritySigner,
    task: &TaskRecord,
    task_id: &str,
    ordinal: i64,
) -> McpPlatformResult<()> {
    let row = sqlx::query("SELECT * FROM task_steps WHERE task_id=? AND ordinal=?")
        .bind(task_id)
        .bind(ordinal)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .ok_or_else(not_found)?;
    let step = decode_task_step_row(&row)?;
    let compensation_json: String = row.try_get("compensation_json").map_err(map_sqlx)?;
    let evidence_json: Option<String> = row.try_get("evidence_json").map_err(map_sqlx)?;
    let mac = signer.sign(
        "task-step-compensation",
        &task_step_compensation_payload(task, &step, &compensation_json, evidence_json.as_deref()),
    )?;
    let updated = sqlx::query(
        "UPDATE task_step_compensation_bindings SET mac=? WHERE task_id=? AND ordinal=?",
    )
    .bind(mac)
    .bind(task_id)
    .bind(ordinal)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if updated.rows_affected() != 1 {
        return Err(integrity_error());
    }
    Ok(())
}

async fn record_lifecycle_target(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    task_id: &str,
    managed_mcp_id: &str,
) -> McpPlatformResult<()> {
    sqlx::query(
        "INSERT INTO lifecycle_task_targets(task_id, managed_mcp_id) VALUES (?, ?) ON CONFLICT(task_id) DO NOTHING",
    )
    .bind(task_id)
    .bind(managed_mcp_id)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    let stored = sqlx::query_scalar::<_, String>(
        "SELECT managed_mcp_id FROM lifecycle_task_targets WHERE task_id = ?",
    )
    .bind(task_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if stored != managed_mcp_id {
        return Err(integrity_error());
    }
    Ok(())
}

async fn fetch_health_task_request(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    task_id: &str,
) -> McpPlatformResult<Option<HealthTaskRequestRecord>> {
    sqlx::query("SELECT task_id,managed_mcp_id,mode FROM health_task_requests WHERE task_id=?")
        .bind(task_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .map(|row| {
            Ok(HealthTaskRequestRecord {
                task_id: row.try_get("task_id").map_err(map_sqlx)?,
                managed_mcp_id: row.try_get("managed_mcp_id").map_err(map_sqlx)?,
                mode: decode_database_enum(&row.try_get::<String, _>("mode").map_err(map_sqlx)?)?,
            })
        })
        .transpose()
}

fn task_plan_operations_compatible(
    task_operation: crate::mcp_platform::task::TaskOperation,
    plan_operation: PlanOperation,
    health_request: Option<&HealthTaskRequestRecord>,
) -> bool {
    if task_operation == crate::mcp_platform::task::TaskOperation::Health
        && health_request.is_some_and(|request| request.mode == HealthCheckMode::Registration)
    {
        return plan_operation == PlanOperation::Register;
    }
    task_operation == plan_operation.into()
}

async fn validate_registration_health_binding(
    repository: &SqliteMcpPlatformRepository,
    tx: &mut Transaction<'_, Sqlite>,
    task: &TaskRecord,
    plan: &crate::mcp_platform::repository::PlanRecord,
    health_request: Option<&HealthTaskRequestRecord>,
) -> McpPlatformResult<()> {
    let Some(request) = health_request.filter(|request| {
        task.operation == crate::mcp_platform::task::TaskOperation::Health
            && request.mode == HealthCheckMode::Registration
    }) else {
        return Ok(());
    };
    if task.plan_digest != plan.plan.plan_digest() {
        return Err(integrity_error());
    }
    let projection = repository
        .fetch_connection_projection_snapshot(tx, &request.managed_mcp_id)
        .await?
        .ok_or_else(integrity_error)?;
    let inventory =
        SqliteMcpPlatformRepository::fetch_managed_inventory_snapshot(tx, &request.managed_mcp_id)
            .await
            .map_err(|error| {
                if error.code() == McpPlatformErrorCode::NotFound {
                    integrity_error()
                } else {
                    error
                }
            })?;
    let projection_owner_task_id = projection
        .owner_task_id
        .as_deref()
        .ok_or_else(integrity_error)?;
    let owner_task = fetch_task(tx, projection_owner_task_id).await?;
    if request.task_id != task.task_id
        || request.managed_mcp_id != projection.managed_mcp_id
        || projection.plan_id.as_deref() != Some(task.plan_id.as_str())
        || projection.manifest_digest.as_deref() != Some(plan.plan.manifest_digest())
        || projection_owner_task_id != owner_task.task_id
        || owner_task.operation != crate::mcp_platform::task::TaskOperation::Register
        || owner_task.plan_id != plan.plan_id
        || owner_task.plan_digest != plan.plan.plan_digest()
        || owner_task.actor != plan.actor
        || inventory.managed.managed_mcp_id != projection.managed_mcp_id
        || inventory.lifecycle.active_manifest_digest.as_deref()
            != Some(plan.plan.manifest_digest())
        || inventory.lifecycle.owner_task_id.as_deref() != Some(projection_owner_task_id)
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn lifecycle_target_for_plan(
    plan: &crate::mcp_platform::repository::PlanRecord,
    operation: crate::mcp_platform::task::TaskOperation,
) -> McpPlatformResult<Option<String>> {
    if operation == crate::mcp_platform::task::TaskOperation::Register {
        let scope = plan.target.installation_scope.as_deref().unwrap_or("user");
        let derived =
            crate::mcp_platform::task_runner::stable_managed_mcp_id(plan.plan.manifest_id(), scope);
        if plan
            .target
            .managed_mcp_id
            .as_ref()
            .is_some_and(|managed_mcp_id| managed_mcp_id != &derived)
        {
            return Err(integrity_error());
        }
        return Ok(Some(derived));
    }
    Ok(plan.target.managed_mcp_id.clone())
}

async fn claim_lifecycle_lease(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    managed_mcp_id: &str,
    task_id: &str,
    operation: crate::mcp_platform::task::TaskOperation,
    now_ms: i64,
) -> McpPlatformResult<()> {
    let lease_operation = match operation {
        crate::mcp_platform::task::TaskOperation::Register => {
            crate::mcp_platform::task::TaskOperation::Install
        }
        operation => operation,
    };
    let projection_busy = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM projection_mutations WHERE managed_mcp_id = ? AND status IN ('started','config_committed','recovery_required'))",
    )
    .bind(managed_mcp_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if projection_busy {
        return Err(error(
            McpPlatformErrorCode::PlanConflict,
            "managed MCP projection mutation must be resolved first",
        ));
    }
    sqlx::query(
        "INSERT INTO managed_lifecycle_leases(managed_mcp_id,task_id,operation,acquired_at_ms,expires_at_ms) VALUES (?,?,?,?,NULL) ON CONFLICT(managed_mcp_id) DO NOTHING",
    )
    .bind(managed_mcp_id)
    .bind(task_id)
    .bind(lease_operation.as_str())
    .bind(now_ms)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    let owner = sqlx::query_scalar::<_, String>(
        "SELECT task_id FROM managed_lifecycle_leases WHERE managed_mcp_id = ?",
    )
    .bind(managed_mcp_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if owner != task_id {
        return Err(error(
            McpPlatformErrorCode::PlanConflict,
            "managed MCP already has an active lifecycle mutation",
        ));
    }
    Ok(())
}

fn managed_lifecycle_operation(operation: crate::mcp_platform::task::TaskOperation) -> bool {
    matches!(
        operation,
        crate::mcp_platform::task::TaskOperation::Register
            | crate::mcp_platform::task::TaskOperation::Install
            | crate::mcp_platform::task::TaskOperation::Update
            | crate::mcp_platform::task::TaskOperation::Repair
            | crate::mcp_platform::task::TaskOperation::Uninstall
    )
}

#[cfg(test)]
mod tests {
    use super::task_plan_operations_compatible;
    use crate::mcp_platform::domain::HealthCheckMode;
    use crate::mcp_platform::policy::PlanOperation;
    use crate::mcp_platform::repository::HealthTaskRequestRecord;
    use crate::mcp_platform::task::TaskOperation;

    fn health_request(mode: HealthCheckMode) -> HealthTaskRequestRecord {
        HealthTaskRequestRecord {
            task_id: "health-task".to_string(),
            managed_mcp_id: "managed-mcp".to_string(),
            mode,
        }
    }

    #[test]
    fn registration_health_can_reference_register_plan() {
        let request = health_request(HealthCheckMode::Registration);

        assert!(task_plan_operations_compatible(
            TaskOperation::Health,
            PlanOperation::Register,
            Some(&request),
        ));
        assert!(!task_plan_operations_compatible(
            TaskOperation::Health,
            PlanOperation::Install,
            Some(&request),
        ));
    }

    #[test]
    fn health_operation_requires_matching_request_mode() {
        let runtime_request = health_request(HealthCheckMode::Runtime);
        let registration_request = health_request(HealthCheckMode::Registration);

        assert!(task_plan_operations_compatible(
            TaskOperation::Health,
            PlanOperation::Health,
            Some(&runtime_request),
        ));
        assert!(!task_plan_operations_compatible(
            TaskOperation::Health,
            PlanOperation::Register,
            Some(&runtime_request),
        ));
        assert!(!task_plan_operations_compatible(
            TaskOperation::Health,
            PlanOperation::Health,
            Some(&registration_request),
        ));
        assert!(task_plan_operations_compatible(
            TaskOperation::Health,
            PlanOperation::Health,
            Some(&runtime_request),
        ));
        assert!(task_plan_operations_compatible(
            TaskOperation::Health,
            PlanOperation::Health,
            None,
        ));
    }

    #[test]
    fn non_health_operations_remain_strictly_equal() {
        assert!(task_plan_operations_compatible(
            TaskOperation::Install,
            PlanOperation::Install,
            None,
        ));
        assert!(!task_plan_operations_compatible(
            TaskOperation::Install,
            PlanOperation::Register,
            Some(&health_request(HealthCheckMode::Registration)),
        ));
    }
}
