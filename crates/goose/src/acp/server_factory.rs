use crate::acp::server::{AcpProviderFactory, GooseAcpAgent, GooseAcpAgentOptions};
use crate::agents::GoosePlatform;
use crate::scheduler_trait::SchedulerTrait;
use crate::session::SessionManager;
use crate::source_roots::SourceRoot;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::info;

pub struct AcpServerFactoryConfig {
    pub builtins: Vec<String>,
    pub data_dir: std::path::PathBuf,
    pub config_dir: std::path::PathBuf,
    pub goose_platform: GoosePlatform,
    pub additional_source_roots: Vec<SourceRoot>,
}

pub struct AcpServer {
    config: AcpServerFactoryConfig,
    scheduler: OnceCell<Arc<dyn SchedulerTrait>>,
    mcp_platform_service: Arc<OnceCell<Arc<crate::mcp_platform::McpPlatformService>>>,
    trusted_mcp_platform_service: Arc<OnceCell<Arc<crate::mcp_platform::McpPlatformService>>>,
}

impl AcpServer {
    pub fn new(config: AcpServerFactoryConfig) -> Self {
        Self {
            config,
            scheduler: OnceCell::new(),
            mcp_platform_service: Arc::new(OnceCell::new()),
            trusted_mcp_platform_service: Arc::new(OnceCell::new()),
        }
    }

    pub async fn shutdown(&self) {
        if let Some(service) = self.trusted_mcp_platform_service.get() {
            service.shutdown_worker().await;
        } else if let Some(service) = self.mcp_platform_service.get() {
            service.shutdown_worker().await;
        }
    }

    async fn scheduler(&self) -> Result<Arc<dyn SchedulerTrait>> {
        let data_dir = self.config.data_dir.clone();
        self.scheduler
            .get_or_try_init(|| async move {
                let session_manager = Arc::new(SessionManager::new(data_dir.clone()));
                let schedule_file_path = data_dir.join("schedule.json");
                let scheduler =
                    crate::scheduler::Scheduler::new(schedule_file_path, session_manager)
                        .await
                        .map(|scheduler| scheduler as Arc<dyn SchedulerTrait>)?;
                Ok(scheduler)
            })
            .await
            .cloned()
    }

    pub async fn create_agent(&self) -> Result<Arc<GooseAcpAgent>> {
        let config = crate::config::Config::global();
        let disable_session_naming = config.get_goose_disable_session_naming().unwrap_or(false);
        let scheduler = self.scheduler().await?;

        let provider_factory: AcpProviderFactory =
            Arc::new(move |provider_name, extensions, working_dir| {
                Box::pin(async move {
                    match working_dir {
                        Some(working_dir) => {
                            crate::providers::create_with_working_dir(
                                &provider_name,
                                extensions,
                                working_dir,
                            )
                            .await
                        }
                        None => crate::providers::create(&provider_name, extensions).await,
                    }
                })
            });

        let agent = GooseAcpAgent::new_with_trusted_mcp_platform_service_cell(
            GooseAcpAgentOptions {
                provider_factory,
                builtins: self.config.builtins.clone(),
                data_dir: self.config.data_dir.clone(),
                config_dir: self.config.config_dir.clone(),
                disable_session_naming,
                goose_platform: self.config.goose_platform.clone(),
                additional_source_roots: self.config.additional_source_roots.clone(),
                scheduler,
                mcp_platform_service: None,
                mcp_platform_service_cell: Some(self.mcp_platform_service.clone()),
            },
            Some(self.trusted_mcp_platform_service.clone()),
        )
        .await?;
        info!("Created new ACP agent");

        Ok(Arc::new(agent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    fn test_server(directory: &tempfile::TempDir) -> AcpServer {
        AcpServer::new(AcpServerFactoryConfig {
            builtins: Vec::new(),
            data_dir: directory.path().join("data"),
            config_dir: directory.path().join("config"),
            goose_platform: GoosePlatform::GooseCli,
            additional_source_roots: Vec::new(),
        })
    }

    #[tokio::test]
    async fn agents_share_one_lazy_platform_service_owned_by_the_server() {
        let directory = tempfile::tempdir().unwrap();
        let server = test_server(&directory);
        let first = server.create_agent().await.unwrap();
        let second = server.create_agent().await.unwrap();
        assert!(server.mcp_platform_service.get().is_none());
        assert!(!directory
            .path()
            .join("data/mcp-platform/platform.db")
            .exists());
        assert!(Arc::ptr_eq(
            &first.test_mcp_platform_service_cell(),
            &second.test_mcp_platform_service_cell()
        ));
        assert!(Arc::ptr_eq(
            &first.test_trusted_mcp_platform_service_cell(),
            &second.test_trusted_mcp_platform_service_cell()
        ));

        let first_service = first.test_initialize_mcp_platform().await.unwrap();
        let second_service = second.test_initialize_mcp_platform().await.unwrap();
        assert!(Arc::ptr_eq(&first_service, &second_service));
        drop(first);
        assert!(server.trusted_mcp_platform_service.get().is_some());
        server.shutdown().await;
    }

    #[tokio::test]
    async fn platform_database_failure_is_lazy_and_does_not_break_agent_creation() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("mcp-platform"), b"not a directory").unwrap();
        let server = test_server(&directory);
        let first = server.create_agent().await.unwrap();
        let second = server.create_agent().await.unwrap();
        assert!(server.mcp_platform_service.get().is_none());
        assert!(first.test_initialize_mcp_platform().await.is_err());
        assert!(server.mcp_platform_service.get().is_none());
        let _ordinary_non_mcp_dependency = second.permission_manager();
        server.shutdown().await;
    }

    #[tokio::test]
    async fn production_read_only_accessor_leaves_queued_tasks_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let server = test_server(&directory);
        let agent = server.create_agent().await.unwrap();
        let service = agent
            .test_initialize_mcp_platform_read_only()
            .await
            .unwrap();
        assert!(!service.test_worker_started());

        let database_path = directory.path().join("data/mcp-platform/platform.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(database_path)
                    .create_if_missing(false),
            )
            .await
            .unwrap();
        let manifest_digest = "a".repeat(64);
        sqlx::query("INSERT INTO manifest_blobs(manifest_digest,mcp_id,version,canonical_bytes,proof_json,trust_tier_json,created_at_ms) VALUES (?,?,?,?,?,?,?)")
            .bind(&manifest_digest).bind("queued-test").bind("1.0.0").bind(Vec::<u8>::new()).bind("{}").bind("\"official\"").bind(1_i64)
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO install_plans(plan_id,plan_digest,envelope_digest,manifest_digest,operation,target_json,plan_json,policy_evidence_json,confirmation_evidence_json,expires_at_ms,idempotency_key,actor,created_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind("queued-plan").bind("b".repeat(64)).bind("c".repeat(64)).bind(&manifest_digest).bind("install").bind("{}").bind("{}").bind("{}").bind("{}").bind(i64::MAX).bind("queued-plan-key").bind("local_authenticated_client").bind(1_i64)
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO tasks(task_id,plan_id,plan_digest,operation,idempotency_key,status,actor,created_at_ms,updated_at_ms) VALUES (?,?,?,?,?,'queued',?,?,?)")
            .bind("queued-task").bind("queued-plan").bind("b".repeat(64)).bind("install").bind("queued-task-key").bind("local_authenticated_client").bind(1_i64).bind(1_i64)
            .execute(&pool).await.unwrap();

        agent.test_profile_query_handlers_read_only().await;
        let same_service = agent
            .test_initialize_mcp_platform_read_only()
            .await
            .unwrap();
        tokio::task::yield_now().await;
        assert!(Arc::ptr_eq(&service, &same_service));
        assert!(!same_service.test_worker_started());
        let status: String =
            sqlx::query_scalar("SELECT status FROM tasks WHERE task_id='queued-task'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "queued");
        server.shutdown().await;
    }
}
