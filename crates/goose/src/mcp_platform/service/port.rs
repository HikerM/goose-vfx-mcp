use async_trait::async_trait;

use crate::mcp_platform::error::McpPlatformResult;
use crate::mcp_platform::repository::{
    ActivateManagedInstallation, AuditEventRecord, CompensationTransition,
    ConnectionProjectionRecord, CreateHealthTask, CreateTask, GlobalAuditPage,
    HealthObservationRecord, HealthTaskRequestRecord, ManagedMcpInventoryRecord,
    ManagedUninstallSnapshot, ManifestRecord, NewHealthObservation, PlanRecord, PutOwnedProjection,
    RegisterManagedMcp, RegisterManagedMcpOutcome, RestoreOwnedProjection, RetryAttemptRecord,
    SavePlan, StageManagedInstallation, StageManagedInstallationOutcome, StepTransition,
    TaskRecord, TaskStepRecord, TaskTransition,
};
use crate::mcp_platform::task::CompensationDescriptor;
use crate::mcp_platform::task::TaskOperation;

#[async_trait]
pub trait McpPlatformRepositoryPort: Send + Sync {
    async fn save_manifest(
        &self,
        record: &crate::mcp_platform::repository::ManifestRecord,
    ) -> McpPlatformResult<crate::mcp_platform::repository::InsertOutcome> {
        let _ = record;
        Err(crate::mcp_platform::McpPlatformError::new(
            crate::mcp_platform::McpPlatformErrorCode::OperationNotSupported,
            "repository does not support verified manifest persistence",
        ))
    }
    async fn list_manifests(&self) -> McpPlatformResult<Vec<ManifestRecord>>;
    async fn get_manifest(&self, digest: &str) -> McpPlatformResult<ManifestRecord>;
    async fn get_manifest_by_identity(
        &self,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<ManifestRecord>;
    async fn save_plan(&self, input: SavePlan<'_>) -> McpPlatformResult<PlanRecord>;
    async fn get_plan(&self, plan_id: &str) -> McpPlatformResult<PlanRecord>;
    async fn get_plan_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<PlanRecord>>;
    async fn create_task(&self, input: CreateTask<'_>) -> McpPlatformResult<TaskRecord>;
    async fn get_task(&self, task_id: &str) -> McpPlatformResult<TaskRecord>;
    async fn get_task_by_idempotency_key(
        &self,
        operation: TaskOperation,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<TaskRecord>>;
    async fn latest_managed_lifecycle_task(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<TaskRecord>>;
    async fn transition_task(
        &self,
        transition: TaskTransition<'_>,
    ) -> McpPlatformResult<TaskRecord>;
    async fn confirm_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord>;
    async fn request_cancel(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord>;
    async fn retry_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        retry_idempotency_key: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord>;
    async fn list_audit_events(&self, task_id: &str) -> McpPlatformResult<Vec<AuditEventRecord>>;
    async fn list_global_audit_events(
        &self,
        after_event_id: i64,
        limit: usize,
        task_ids: &[String],
    ) -> McpPlatformResult<GlobalAuditPage>;
    async fn claim_next_task(
        &self,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<Option<TaskRecord>>;
    async fn renew_task_lease(
        &self,
        task_id: &str,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<TaskRecord>;
    async fn add_task_step(
        &self,
        task_id: &str,
        ordinal: i64,
        idempotency_token: &str,
        compensation: &CompensationDescriptor,
        adapter_id: &str,
        adapter_version: &str,
    ) -> McpPlatformResult<TaskStepRecord>;
    async fn transition_task_step(
        &self,
        transition: StepTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord>;
    async fn transition_compensation(
        &self,
        transition: CompensationTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord>;
    async fn list_task_steps(&self, task_id: &str) -> McpPlatformResult<Vec<TaskStepRecord>>;
    async fn recover_stale_tasks(
        &self,
        heartbeat_cutoff_ms: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<Vec<crate::mcp_platform::repository::RecoveryRecord>>;
    async fn register_managed_mcp(
        &self,
        input: RegisterManagedMcp<'_>,
    ) -> McpPlatformResult<RegisterManagedMcpOutcome>;
    async fn stage_managed_installation(
        &self,
        input: StageManagedInstallation<'_>,
    ) -> McpPlatformResult<StageManagedInstallationOutcome>;
    async fn claim_artifact(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
        stale_before_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn mark_artifact_claim_verified(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn release_artifact_claim(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn mark_managed_runtime_activated(
        &self,
        managed_mcp_id: &str,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn finalize_retained_version_cleanup(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn is_managed_finalizing(
        &self,
        task_id: &str,
        operation: TaskOperation,
    ) -> McpPlatformResult<bool>;
    async fn record_managed_finalization_failure(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<i64>;
    async fn activate_managed_installation(
        &self,
        input: ActivateManagedInstallation<'_>,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord>;
    async fn rollback_managed_installation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn begin_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedUninstallSnapshot>;
    async fn mark_managed_uninstall_quarantined(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn cancel_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn finalize_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn get_managed_inventory(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord>;
    async fn list_managed_inventory(
        &self,
        after_managed_mcp_id: Option<&str>,
        limit: usize,
        filter: &crate::mcp_platform::repository::ManagedInventoryFilter,
    ) -> McpPlatformResult<Vec<ManagedMcpInventoryRecord>>;
    async fn put_owned_connection_projection(
        &self,
        input: PutOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord>;
    async fn get_connection_projection(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ConnectionProjectionRecord>;
    async fn restore_owned_connection_projection(
        &self,
        input: RestoreOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord>;
    async fn remove_owned_projection(
        &self,
        managed_mcp_id: &str,
        owner_task_id: &str,
    ) -> McpPlatformResult<bool>;
    async fn remove_owned_managed_mcp(
        &self,
        managed_mcp_id: &str,
        owner_task_id: &str,
    ) -> McpPlatformResult<bool>;
    async fn update_managed_state(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        state: &crate::mcp_platform::ManagedMcpState,
        now_ms: i64,
    ) -> McpPlatformResult<crate::mcp_platform::repository::ManagedMcpRecord>;
    async fn create_health_task(
        &self,
        input: CreateHealthTask<'_>,
    ) -> McpPlatformResult<TaskRecord>;
    async fn get_health_task_request(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<HealthTaskRequestRecord>;
    async fn append_health_observation(
        &self,
        input: NewHealthObservation<'_>,
    ) -> McpPlatformResult<HealthObservationRecord>;
    async fn latest_health_observation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<HealthObservationRecord>>;
    async fn list_retry_attempts(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<Vec<RetryAttemptRecord>>;
    async fn begin_projection_mutation(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        desired_enabled: bool,
        now_ms: i64,
    ) -> McpPlatformResult<crate::mcp_platform::repository::ProjectionMutationRecord>;
    async fn mark_projection_config_committed(
        &self,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn complete_projection_mutation(
        &self,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
    async fn list_pending_projection_mutations(
        &self,
    ) -> McpPlatformResult<Vec<crate::mcp_platform::repository::ProjectionMutationRecord>>;
    async fn projection_recovery_required(&self, managed_mcp_id: &str) -> McpPlatformResult<bool>;
    async fn mark_projection_mutation_recovery_required(
        &self,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()>;
}

#[async_trait]
impl McpPlatformRepositoryPort for crate::mcp_platform::repository::SqliteMcpPlatformRepository {
    async fn save_manifest(
        &self,
        record: &crate::mcp_platform::repository::ManifestRecord,
    ) -> McpPlatformResult<crate::mcp_platform::repository::InsertOutcome> {
        self.save_manifest(record).await
    }

    async fn list_manifests(&self) -> McpPlatformResult<Vec<ManifestRecord>> {
        self.list_manifests().await
    }

    async fn get_manifest(&self, digest: &str) -> McpPlatformResult<ManifestRecord> {
        self.get_manifest(digest).await
    }

    async fn get_manifest_by_identity(
        &self,
        mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<ManifestRecord> {
        self.get_manifest_by_identity(mcp_id, version).await
    }

    async fn save_plan(&self, input: SavePlan<'_>) -> McpPlatformResult<PlanRecord> {
        self.save_plan(input).await
    }

    async fn get_plan(&self, plan_id: &str) -> McpPlatformResult<PlanRecord> {
        self.get_plan(plan_id).await
    }

    async fn get_plan_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<PlanRecord>> {
        self.get_plan_by_idempotency_key(idempotency_key).await
    }

    async fn create_task(&self, input: CreateTask<'_>) -> McpPlatformResult<TaskRecord> {
        self.create_task(input).await
    }

    async fn get_task(&self, task_id: &str) -> McpPlatformResult<TaskRecord> {
        self.get_task(task_id).await
    }

    async fn get_task_by_idempotency_key(
        &self,
        operation: TaskOperation,
        idempotency_key: &str,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        self.get_task_by_idempotency_key(operation, idempotency_key)
            .await
    }

    async fn latest_managed_lifecycle_task(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        self.latest_managed_lifecycle_task(managed_mcp_id).await
    }

    async fn transition_task(
        &self,
        transition: TaskTransition<'_>,
    ) -> McpPlatformResult<TaskRecord> {
        self.transition_task(transition).await
    }

    async fn confirm_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        self.confirm_task(task_id, expected_revision, actor, now_ms)
            .await
    }

    async fn request_cancel(
        &self,
        task_id: &str,
        expected_revision: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        self.request_cancel(task_id, expected_revision, actor, now_ms)
            .await
    }

    async fn retry_task(
        &self,
        task_id: &str,
        expected_revision: i64,
        retry_idempotency_key: &str,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        self.retry_task(
            task_id,
            expected_revision,
            retry_idempotency_key,
            actor,
            now_ms,
        )
        .await
    }

    async fn list_audit_events(&self, task_id: &str) -> McpPlatformResult<Vec<AuditEventRecord>> {
        self.list_audit_events(task_id).await
    }

    async fn list_global_audit_events(
        &self,
        after_event_id: i64,
        limit: usize,
        task_ids: &[String],
    ) -> McpPlatformResult<GlobalAuditPage> {
        self.list_global_audit_events(after_event_id, limit, task_ids)
            .await
    }

    async fn claim_next_task(
        &self,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<Option<TaskRecord>> {
        self.claim_next_task(owner_id, now_ms, lease_duration_ms)
            .await
    }

    async fn renew_task_lease(
        &self,
        task_id: &str,
        owner_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> McpPlatformResult<TaskRecord> {
        self.renew_task_lease(task_id, owner_id, now_ms, lease_duration_ms)
            .await
    }

    async fn add_task_step(
        &self,
        task_id: &str,
        ordinal: i64,
        idempotency_token: &str,
        compensation: &CompensationDescriptor,
        adapter_id: &str,
        adapter_version: &str,
    ) -> McpPlatformResult<TaskStepRecord> {
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

    async fn transition_task_step(
        &self,
        transition: StepTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord> {
        self.transition_task_step(transition).await
    }

    async fn transition_compensation(
        &self,
        transition: CompensationTransition<'_>,
    ) -> McpPlatformResult<TaskStepRecord> {
        self.transition_compensation(transition).await
    }

    async fn list_task_steps(&self, task_id: &str) -> McpPlatformResult<Vec<TaskStepRecord>> {
        self.list_task_steps(task_id).await
    }

    async fn recover_stale_tasks(
        &self,
        heartbeat_cutoff_ms: i64,
        actor: &str,
        now_ms: i64,
    ) -> McpPlatformResult<Vec<crate::mcp_platform::repository::RecoveryRecord>> {
        self.recover_stale_tasks(heartbeat_cutoff_ms, actor, now_ms)
            .await
    }

    async fn register_managed_mcp(
        &self,
        input: RegisterManagedMcp<'_>,
    ) -> McpPlatformResult<RegisterManagedMcpOutcome> {
        self.register_managed_mcp(input).await
    }

    async fn stage_managed_installation(
        &self,
        input: StageManagedInstallation<'_>,
    ) -> McpPlatformResult<StageManagedInstallationOutcome> {
        self.stage_managed_installation(input).await
    }
    async fn claim_artifact(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
        stale_before_ms: i64,
    ) -> McpPlatformResult<()> {
        self.claim_artifact(artifact_digest, task_id, now_ms, stale_before_ms)
            .await
    }
    async fn mark_artifact_claim_verified(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.mark_artifact_claim_verified(artifact_digest, task_id, now_ms)
            .await
    }
    async fn release_artifact_claim(
        &self,
        artifact_digest: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.release_artifact_claim(artifact_digest, task_id, now_ms)
            .await
    }
    async fn mark_managed_runtime_activated(
        &self,
        managed_mcp_id: &str,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.mark_managed_runtime_activated(managed_mcp_id, target_version, task_id, now_ms)
            .await
    }
    async fn finalize_retained_version_cleanup(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.finalize_retained_version_cleanup(managed_mcp_id, version, task_id, now_ms)
            .await
    }
    async fn is_managed_finalizing(
        &self,
        task_id: &str,
        operation: TaskOperation,
    ) -> McpPlatformResult<bool> {
        self.is_managed_finalizing(task_id, operation).await
    }

    async fn record_managed_finalization_failure(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<i64> {
        self.record_managed_finalization_failure(task_id, now_ms)
            .await
    }
    async fn activate_managed_installation(
        &self,
        input: ActivateManagedInstallation<'_>,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        self.activate_managed_installation(input).await
    }
    async fn rollback_managed_installation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        target_version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.rollback_managed_installation(
            managed_mcp_id,
            previous_version,
            target_version,
            task_id,
            now_ms,
        )
        .await
    }
    async fn begin_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<ManagedUninstallSnapshot> {
        self.begin_managed_uninstall(managed_mcp_id, version, task_id, now_ms)
            .await
    }
    async fn mark_managed_uninstall_quarantined(
        &self,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.mark_managed_uninstall_quarantined(task_id, now_ms)
            .await
    }
    async fn cancel_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.cancel_managed_uninstall(managed_mcp_id, task_id, now_ms)
            .await
    }
    async fn finalize_managed_uninstall(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.finalize_managed_uninstall(managed_mcp_id, version, task_id, now_ms)
            .await
    }

    async fn get_managed_inventory(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        self.get_managed_inventory(managed_mcp_id).await
    }

    async fn list_managed_inventory(
        &self,
        after_managed_mcp_id: Option<&str>,
        limit: usize,
        filter: &crate::mcp_platform::repository::ManagedInventoryFilter,
    ) -> McpPlatformResult<Vec<ManagedMcpInventoryRecord>> {
        self.list_managed_inventory(after_managed_mcp_id, limit, filter)
            .await
    }

    async fn put_owned_connection_projection(
        &self,
        input: PutOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        self.put_owned_connection_projection(input).await
    }

    async fn get_connection_projection(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        self.get_connection_projection(managed_mcp_id).await
    }
    async fn restore_owned_connection_projection(
        &self,
        input: RestoreOwnedProjection<'_>,
    ) -> McpPlatformResult<ConnectionProjectionRecord> {
        self.restore_owned_connection_projection(input).await
    }

    async fn remove_owned_projection(
        &self,
        managed_mcp_id: &str,
        owner_task_id: &str,
    ) -> McpPlatformResult<bool> {
        self.remove_owned_projection(managed_mcp_id, owner_task_id)
            .await
    }

    async fn remove_owned_managed_mcp(
        &self,
        managed_mcp_id: &str,
        owner_task_id: &str,
    ) -> McpPlatformResult<bool> {
        self.remove_owned_managed_mcp(managed_mcp_id, owner_task_id)
            .await
    }

    async fn update_managed_state(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        state: &crate::mcp_platform::ManagedMcpState,
        now_ms: i64,
    ) -> McpPlatformResult<crate::mcp_platform::repository::ManagedMcpRecord> {
        self.update_managed_state(managed_mcp_id, expected_revision, state, now_ms)
            .await
    }

    async fn create_health_task(
        &self,
        input: CreateHealthTask<'_>,
    ) -> McpPlatformResult<TaskRecord> {
        self.create_health_task(input).await
    }

    async fn get_health_task_request(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<HealthTaskRequestRecord> {
        self.get_health_task_request(task_id).await
    }

    async fn append_health_observation(
        &self,
        input: NewHealthObservation<'_>,
    ) -> McpPlatformResult<HealthObservationRecord> {
        self.append_health_observation(input).await
    }

    async fn latest_health_observation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<HealthObservationRecord>> {
        self.latest_health_observation(managed_mcp_id).await
    }

    async fn list_retry_attempts(
        &self,
        task_id: &str,
    ) -> McpPlatformResult<Vec<RetryAttemptRecord>> {
        self.list_retry_attempts(task_id).await
    }

    async fn begin_projection_mutation(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        desired_enabled: bool,
        now_ms: i64,
    ) -> McpPlatformResult<crate::mcp_platform::repository::ProjectionMutationRecord> {
        self.begin_projection_mutation(managed_mcp_id, expected_revision, desired_enabled, now_ms)
            .await
    }
    async fn mark_projection_config_committed(
        &self,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.mark_projection_config_committed(mutation_id, now_ms)
            .await
    }
    async fn complete_projection_mutation(
        &self,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.complete_projection_mutation(mutation_id, now_ms).await
    }
    async fn list_pending_projection_mutations(
        &self,
    ) -> McpPlatformResult<Vec<crate::mcp_platform::repository::ProjectionMutationRecord>> {
        self.list_pending_projection_mutations().await
    }

    async fn projection_recovery_required(&self, managed_mcp_id: &str) -> McpPlatformResult<bool> {
        self.projection_recovery_required(managed_mcp_id).await
    }
    async fn mark_projection_mutation_recovery_required(
        &self,
        mutation_id: i64,
        now_ms: i64,
    ) -> McpPlatformResult<()> {
        self.mark_projection_mutation_recovery_required(mutation_id, now_ms)
            .await
    }
}
