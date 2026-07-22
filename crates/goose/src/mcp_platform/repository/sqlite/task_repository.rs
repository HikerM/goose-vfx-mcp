use crate::mcp_platform::error::{McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::repository::GlobalAuditPage;
use crate::mcp_platform::repository::{
    AuditEventRecord, AuditEventType, AuditPayload, CompensationTransition, CreateTask,
    RecoveryRecord, StepTransition, TaskRecord, TaskStepRecord, TaskTransition,
};
use crate::mcp_platform::task::{
    CompensationDescriptor, CompensationStatus, TaskStatus, TaskStepStatus,
};
use sqlx::{QueryBuilder, Sqlite};

use super::records::{
    append_audit, decode_audit_row, decode_task_row, decode_task_step_row, encode, encode_optional,
    error, fetch_task, fetch_task_step, fetch_valid_plan, integrity_error, map_sqlx, not_found,
    recovery_decision, revision_conflict, NewAuditEvent,
};
use super::SqliteMcpPlatformRepository;

impl SqliteMcpPlatformRepository {
    pub async fn create_task(&self, input: CreateTask<'_>) -> McpPlatformResult<TaskRecord> {
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
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx)?
        {
            let existing = decode_task_row(&row)?;
            tx.commit().await.map_err(map_sqlx)?;
            return if existing.plan_id == input.plan_id
                && existing.plan_digest == input.plan_digest
                && existing.actor == input.actor
                && existing.adapter_evidence.as_ref() == input.adapter_evidence
                && existing.rollback_evidence.as_ref() == input.rollback_evidence
            {
                Ok(existing)
            } else {
                Err(error(
                    McpPlatformErrorCode::IdempotencyConflict,
                    "task idempotency key already refers to different content",
                ))
            };
        }
        if input.now_ms >= plan.expires_at_ms {
            return Err(error(
                McpPlatformErrorCode::PlanExpired,
                "installation plan has expired",
            ));
        }
        if sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM tasks WHERE task_id = ?)")
            .bind(input.task_id)
            .fetch_one(&mut *tx)
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

    pub async fn transition_task(
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
        current.status.ensure_transition(transition.next_status)?;
        let sequence = current.event_sequence + 1;
        sqlx::query(
            r#"UPDATE tasks SET status = ?, updated_at_ms = ?, heartbeat_at_ms = ?,
                progress = ?, redacted_error_json = ?, rollback_status = ?,
                rollback_evidence_json = ?, revision = revision + 1, event_sequence = ?
                WHERE task_id = ? AND revision = ?"#,
        )
        .bind(transition.next_status.as_str())
        .bind(transition.now_ms)
        .bind(transition.heartbeat_at_ms)
        .bind(i64::from(transition.progress))
        .bind(encode_optional(transition.redacted_error)?)
        .bind(transition.rollback_status.as_str())
        .bind(encode_optional(transition.rollback_evidence)?)
        .bind(sequence)
        .bind(transition.task_id)
        .bind(transition.expected_revision)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
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

    pub async fn heartbeat_task(
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
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let updated = fetch_task(&mut tx, task_id).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub async fn add_task_step(
        &self,
        task_id: &str,
        ordinal: i64,
        idempotency_token: &str,
        compensation: &CompensationDescriptor,
    ) -> McpPlatformResult<TaskStepRecord> {
        let task = self.get_task(task_id).await?;
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
        )
        .await
    }

    pub async fn add_task_step_with_adapter(
        &self,
        task_id: &str,
        ordinal: i64,
        idempotency_token: &str,
        compensation: &CompensationDescriptor,
        adapter_id: &str,
        adapter_version: &str,
    ) -> McpPlatformResult<TaskStepRecord> {
        if ordinal < 0 {
            return Err(integrity_error());
        }
        let mut tx = self.begin_immediate().await?;
        fetch_task(&mut tx, task_id).await?;
        if let Some(row) = sqlx::query("SELECT * FROM task_steps WHERE task_id = ? AND ordinal = ?")
            .bind(task_id)
            .bind(ordinal)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx)?
        {
            let existing = decode_task_step_row(&row)?;
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
        .fetch_one(&mut *tx)
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
        .bind(encode(compensation)?)
        .bind(adapter_id)
        .bind(adapter_version)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let step = fetch_task_step(&mut tx, task_id, ordinal).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(step)
    }

    pub async fn transition_task_step(
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
        let step = fetch_task_step(&mut tx, transition.task_id, transition.ordinal).await?;
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
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let sequence = task.event_sequence + 1;
        sqlx::query(
            r#"UPDATE tasks SET step_cursor = MAX(step_cursor, ?), revision = revision + 1,
                event_sequence = ?, updated_at_ms = ? WHERE task_id = ?"#,
        )
        .bind(transition.ordinal)
        .bind(sequence)
        .bind(transition.now_ms)
        .bind(transition.task_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
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
        let updated = fetch_task_step(&mut tx, transition.task_id, transition.ordinal).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub async fn list_task_steps(&self, task_id: &str) -> McpPlatformResult<Vec<TaskStepRecord>> {
        let rows = sqlx::query("SELECT * FROM task_steps WHERE task_id = ? ORDER BY ordinal")
            .bind(task_id)
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;
        rows.iter().map(decode_task_step_row).collect()
    }

    pub async fn transition_compensation(
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
        let step = fetch_task_step(&mut tx, transition.task_id, transition.ordinal).await?;
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
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE tasks SET updated_at_ms = ?, revision = revision + 1 WHERE task_id = ?",
        )
        .bind(transition.now_ms)
        .bind(transition.task_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let updated = fetch_task_step(&mut tx, transition.task_id, transition.ordinal).await?;
        tx.commit().await.map_err(map_sqlx)?;
        Ok(updated)
    }

    pub async fn confirm_task(
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
        .execute(&mut *tx)
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

    pub async fn request_cancel(
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
            TaskStatus::Cancelling | TaskStatus::Cancelled => {
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
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
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

    pub async fn retry_task(
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
        .fetch_optional(&mut *tx)
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
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query("DELETE FROM task_steps WHERE task_id = ?")
            .bind(task_id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx)?;
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
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            r#"UPDATE tasks SET status = 'queued', retry_idempotency_key = ?, attempt_count = ?,
                updated_at_ms = ?, heartbeat_at_ms = NULL, revision = revision + 1,
                event_sequence = ?, owner_id = NULL, lease_expires_at_ms = NULL,
                redacted_error_json = NULL WHERE task_id = ? AND revision = ?"#,
        )
        .bind(retry_idempotency_key)
        .bind(attempt)
        .bind(now_ms)
        .bind(sequence)
        .bind(task_id)
        .bind(expected_revision)
        .execute(&mut *tx)
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

    pub async fn claim_next_task(
        &self,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        let lease_expires_at_ms = now_ms
            .checked_add(lease_duration_ms)
            .ok_or_else(integrity_error)?;
        let mut tx = self.begin_immediate().await?;
        let row = sqlx::query(
            r#"SELECT * FROM tasks WHERE status = 'queued'
                AND (owner_id IS NULL OR lease_expires_at_ms IS NULL OR lease_expires_at_ms <= ?)
                ORDER BY created_at_ms, task_id LIMIT 1"#,
        )
        .bind(now_ms)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let Some(row) = row else {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(None);
        };
        let current = decode_task_row(&row)?;
        current.status.ensure_transition(TaskStatus::Running)?;
        let sequence = current.event_sequence + 1;
        let result = sqlx::query(
            r#"UPDATE tasks SET status = 'running', owner_id = ?, lease_expires_at_ms = ?,
                heartbeat_at_ms = ?, updated_at_ms = ?, revision = revision + 1,
                event_sequence = ? WHERE task_id = ? AND status = 'queued' AND revision = ?"#,
        )
        .bind(owner_id)
        .bind(lease_expires_at_ms)
        .bind(now_ms)
        .bind(now_ms)
        .bind(sequence)
        .bind(&current.task_id)
        .bind(current.revision)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            tx.commit().await.map_err(map_sqlx)?;
            return Ok(None);
        }
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

    pub async fn renew_task_lease(
        &self,
        task_id: &str,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        let lease_expires_at_ms = now_ms
            .checked_add(lease_duration_ms)
            .ok_or_else(integrity_error)?;
        let result = sqlx::query(
            r#"UPDATE tasks SET heartbeat_at_ms = ?, lease_expires_at_ms = ?, updated_at_ms = ?
                WHERE task_id = ? AND owner_id = ? AND status IN (
                    'running','cancelling','verifying','activating','rolling_back'
                )"#,
        )
        .bind(now_ms)
        .bind(lease_expires_at_ms)
        .bind(now_ms)
        .bind(task_id)
        .bind(owner_id)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx)?;
        if result.rows_affected() != 1 {
            return Err(error(
                McpPlatformErrorCode::RevisionConflict,
                "task lease is no longer owned by this worker",
            ));
        }
        self.get_task(task_id).await
    }

    pub async fn recover_stale_tasks(
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
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx)?;
        let mut recovered = Vec::with_capacity(rows.len());
        for row in rows {
            let task = decode_task_row(&row)?;
            let step_rows =
                sqlx::query("SELECT * FROM task_steps WHERE task_id = ? ORDER BY ordinal")
                    .bind(&task.task_id)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(map_sqlx)?;
            let decoded_steps = step_rows
                .iter()
                .map(decode_task_step_row)
                .collect::<McpPlatformResult<Vec<_>>>();
            let decision = decoded_steps.as_deref().map_or(
                crate::mcp_platform::task::RecoveryDecision::RequiresManualRecovery,
                |steps| recovery_decision(&task, steps),
            );
            let sequence = task.event_sequence + 1;
            sqlx::query(
                r#"UPDATE tasks SET status = 'interrupted', updated_at_ms = ?,
                    revision = revision + 1, event_sequence = ? WHERE task_id = ?"#,
            )
            .bind(now_ms)
            .bind(sequence)
            .bind(&task.task_id)
            .execute(&mut *tx)
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
