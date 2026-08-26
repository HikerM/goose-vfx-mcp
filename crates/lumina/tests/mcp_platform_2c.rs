#[path = "acp_common_tests/mod.rs"]
mod common_tests;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use common_tests::fixtures::server::AcpServerConnection;
use common_tests::fixtures::{send_custom, Connection, OpenAiFixture, TestConnectionConfig};
use lumina::acp::server::{AcpProviderFactory, LuminaAcpAgent, LuminaAcpAgentOptions};
use lumina::agents::LuminaPlatform;
use lumina::custom_requests::{
    McpCatalogListResponse, McpPlanCreateResponse, McpPlatformErrorCodeDto, McpPlatformOutcome,
    MCP_CATALOG_LIST_METHOD, MCP_PLAN_CREATE_METHOD,
};
use lumina::mcp_platform::{
    parse_manifest, InstallationScope, McpPlatformErrorCode, McpPlatformService,
    McpPlatformServiceOptions, PlanCreateInput, PlanIntent, RequestContext,
    SqliteMcpPlatformRepository, TrustTier,
};
use lumina::scheduler::{ScheduledJob, SchedulerError};
use lumina::scheduler_trait::SchedulerTrait;
use lumina::session::Session;
use lumina_test_support::IgnoreSessionId;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

const REMOTE: &str =
    include_str!("../../../documentation/static/schemas/examples/remote-http.json");

struct UnusedScheduler;

#[async_trait]
impl SchedulerTrait for UnusedScheduler {
    async fn add_scheduled_job(
        &self,
        _job: ScheduledJob,
        _copy_recipe: bool,
    ) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn add_scheduled_job_from_content(
        &self,
        job: ScheduledJob,
        _recipe_content: &[u8],
    ) -> Result<ScheduledJob, SchedulerError> {
        Ok(job)
    }

    async fn schedule_recipe(
        &self,
        _recipe_path: std::path::PathBuf,
        _cron_schedule: Option<String>,
    ) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn list_scheduled_jobs(&self) -> Vec<ScheduledJob> {
        Vec::new()
    }

    async fn remove_scheduled_job(
        &self,
        _id: &str,
        _remove_recipe: bool,
    ) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn pause_schedule(&self, _id: &str) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn unpause_schedule(&self, _id: &str) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn run_now(&self, _id: &str) -> Result<String, SchedulerError> {
        unreachable!()
    }

    async fn sessions(
        &self,
        _sched_id: &str,
        _limit: usize,
    ) -> Result<Vec<(String, Session)>, SchedulerError> {
        unreachable!()
    }

    async fn update_schedule(
        &self,
        _sched_id: &str,
        _new_cron: String,
    ) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn kill_running_job(&self, _sched_id: &str) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn get_running_job_info(
        &self,
        _sched_id: &str,
    ) -> Result<Option<(String, chrono::DateTime<chrono::Utc>)>, SchedulerError> {
        unreachable!()
    }
}

async fn insert_manifest(path: &Path, trust_tier: TrustTier) -> String {
    let verified = parse_manifest(REMOTE.as_bytes()).unwrap();
    let digest = verified.digest().to_string();
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO manifest_blobs (
            manifest_digest, mcp_id, version, canonical_bytes, proof_json, trust_tier_json,
            created_at_ms
        ) VALUES (?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&digest)
    .bind(&verified.manifest().id)
    .bind(verified.manifest().version.as_str())
    .bind(verified.canonical_json())
    .bind(serde_json::to_string(&lumina::mcp_platform::ManifestProof::LocalBytes).unwrap())
    .bind(serde_json::to_string(&trust_tier).unwrap())
    .bind(1_000_i64)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    digest
}

async fn seed_manifest(
    data_dir: &Path,
    trust_tier: TrustTier,
) -> (Arc<SqliteMcpPlatformRepository>, String) {
    let database_path = data_dir.join("mcp-platform").join("platform.db");
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path(&database_path)
            .await
            .unwrap(),
    );
    let digest = insert_manifest(&database_path, trust_tier).await;
    (repository, digest)
}

async fn agent(data_dir: &Path, config_dir: &Path) -> LuminaAcpAgent {
    let provider_factory: AcpProviderFactory = Arc::new(|_, _, _| {
        Box::pin(async {
            Err(anyhow::anyhow!(
                "provider is unused in public ACP boundary tests"
            ))
        })
    });
    LuminaAcpAgent::new(LuminaAcpAgentOptions {
        provider_factory,
        builtins: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        config_dir: config_dir.to_path_buf(),
        disable_session_naming: true,
        lumina_platform: LuminaPlatform::LuminaCli,
        additional_source_roots: Vec::new(),
        scheduler: Arc::new(UnusedScheduler),
        mcp_platform_service: None,
        mcp_platform_service_cell: None,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn public_raw_acp_dispatch_stays_read_only_for_mcp_platform_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let data_dir = directory.path().join("agent-data");
    let config_dir = directory.path().join("agent-config");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();

    let (repository, digest) = seed_manifest(&data_dir, TrustTier::Local).await;
    let agent = agent(&data_dir, &config_dir).await;

    let catalog = agent
        .dispatch_custom_request(MCP_CATALOG_LIST_METHOD, serde_json::json!({"pageSize": 10}))
        .await
        .unwrap();
    let catalog: McpCatalogListResponse = serde_json::from_value(catalog).unwrap();
    let McpPlatformOutcome::Success { value } = catalog.outcome else {
        panic!("raw ACP dispatch should keep read-only MCP catalog access");
    };
    assert_eq!(value.items.len(), 1);
    assert_eq!(value.items[0].manifest_digest, digest);

    let denied = agent
        .dispatch_custom_request(
            MCP_PLAN_CREATE_METHOD,
            serde_json::json!({
                "intent": {
                    "type": "register",
                    "manifestDigest": digest,
                    "installationScope": InstallationScope::User,
                },
                "idempotencyKey": "public-embed-register",
            }),
        )
        .await
        .unwrap();
    let denied: McpPlanCreateResponse = serde_json::from_value(denied).unwrap();
    let McpPlatformOutcome::Error { error } = denied.outcome else {
        panic!("public raw ACP dispatch must not acquire MCP mutation authority");
    };
    assert_eq!(error.code, McpPlatformErrorCodeDto::PolicyDenied);
    assert!(repository
        .get_plan_by_idempotency_key("public-embed-register")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn public_stdio_serve_stays_read_only_for_mcp_platform_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let data_dir = directory.path().join("serve-agent-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let (_repository, digest) = seed_manifest(&data_dir, TrustTier::Local).await;

    let openai = OpenAiFixture::new(vec![], Arc::new(IgnoreSessionId)).await;
    let config = TestConnectionConfig {
        data_root: data_dir.clone(),
        ..Default::default()
    };
    let connection = AcpServerConnection::new(config, openai).await;

    let catalog = send_custom(
        connection.cx(),
        MCP_CATALOG_LIST_METHOD,
        serde_json::json!({"pageSize": 10}),
    )
    .await
    .unwrap();
    let catalog: McpCatalogListResponse = serde_json::from_value(catalog).unwrap();
    let McpPlatformOutcome::Success { value } = catalog.outcome else {
        panic!("public stdio serve should preserve read-only catalog access");
    };
    assert_eq!(value.items[0].manifest_digest, digest);

    let denied = send_custom(
        connection.cx(),
        MCP_PLAN_CREATE_METHOD,
        serde_json::json!({
            "intent": {
                "type": "register",
                "manifestDigest": digest,
                "installationScope": InstallationScope::User,
            },
            "idempotencyKey": "public-serve-register",
        }),
    )
    .await
    .unwrap();
    let denied: McpPlanCreateResponse = serde_json::from_value(denied).unwrap();
    let McpPlatformOutcome::Error { error } = denied.outcome else {
        panic!("public stdio serve must not acquire MCP mutation authority");
    };
    assert_eq!(error.code, McpPlatformErrorCodeDto::PolicyDenied);
}

#[test]
fn public_mcp_service_inputs_are_still_exposed_but_not_a_write_capability() {
    let _ = McpPlatformServiceOptions::default();
    let _ = PlanCreateInput {
        intent: PlanIntent::Register {
            manifest_digest: "a".repeat(64),
            installation_scope: InstallationScope::User,
        },
        idempotency_key: "public-input".to_string(),
    };
}

#[tokio::test]
async fn public_service_cell_only_observes_read_only_service_after_raw_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let data_dir = directory.path().join("cell-agent-data");
    let config_dir = directory.path().join("cell-agent-config");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();

    let (repository, digest) = seed_manifest(&data_dir, TrustTier::Local).await;
    let cell: Arc<OnceCell<Arc<McpPlatformService>>> = Arc::new(OnceCell::new());
    let provider_factory: AcpProviderFactory = Arc::new(|_, _, _| {
        Box::pin(async {
            Err(anyhow::anyhow!(
                "provider is unused in public ACP boundary tests"
            ))
        })
    });
    let agent = LuminaAcpAgent::new(LuminaAcpAgentOptions {
        provider_factory,
        builtins: Vec::new(),
        data_dir: data_dir.clone(),
        config_dir: config_dir.clone(),
        disable_session_naming: true,
        lumina_platform: LuminaPlatform::LuminaCli,
        additional_source_roots: Vec::new(),
        scheduler: Arc::new(UnusedScheduler),
        mcp_platform_service: None,
        mcp_platform_service_cell: Some(cell.clone()),
    })
    .await
    .unwrap();

    let catalog = agent
        .dispatch_custom_request(MCP_CATALOG_LIST_METHOD, serde_json::json!({"pageSize": 10}))
        .await
        .unwrap();
    let catalog: McpCatalogListResponse = serde_json::from_value(catalog).unwrap();
    let McpPlatformOutcome::Success { value } = catalog.outcome else {
        panic!("raw ACP dispatch should keep read-only MCP catalog access");
    };
    assert_eq!(value.items[0].manifest_digest, digest);

    let leaked = cell
        .get()
        .cloned()
        .expect("public service cell should initialize after read-only dispatch");
    let error = leaked
        .plan_create(
            &RequestContext::local_authenticated_client("public-cell-observer".to_string()),
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest: digest,
                    installation_scope: InstallationScope::User,
                },
                idempotency_key: "public-cell-register".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::PolicyDenied);
    assert!(repository
        .get_plan_by_idempotency_key("public-cell-register")
        .await
        .unwrap()
        .is_none());
}
