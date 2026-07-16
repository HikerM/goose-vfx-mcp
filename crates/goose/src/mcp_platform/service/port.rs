use async_trait::async_trait;

use crate::mcp_platform::error::McpPlatformResult;
use crate::mcp_platform::repository::{
    AuditEventRecord, CreateTask, GlobalAuditPage, ManifestRecord, PlanRecord, SavePlan,
    TaskRecord, TaskTransition,
};
use crate::mcp_platform::task::TaskOperation;

#[async_trait]
pub trait McpPlatformRepositoryPort: Send + Sync {
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
}

#[async_trait]
impl McpPlatformRepositoryPort for crate::mcp_platform::repository::SqliteMcpPlatformRepository {
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
}
