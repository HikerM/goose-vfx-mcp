use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use goose::acp::server::{AcpProviderFactory, GooseAcpAgent, GooseAcpAgentOptions};
use goose::agents::GoosePlatform;
use goose::custom_requests::{
    McpCatalogDetailRequest, McpCatalogListRequest, McpCatalogListResponse, McpEventsResumeRequest,
    McpInstallConfirmRequest, McpPlanCreateRequest, McpPlatformErrorCodeDto,
    McpPlatformErrorDetails, McpPlatformErrorEnvelope, McpPlatformOutcome, McpTaskCancelRequest,
    McpTaskGetRequest, McpTaskGetResponse, McpTaskRetryRequest, MCP_CATALOG_DETAIL_METHOD,
    MCP_CATALOG_LIST_METHOD, MCP_EVENTS_RESUME_METHOD, MCP_INSTALL_CONFIRM_METHOD,
    MCP_PLAN_CREATE_METHOD, MCP_TASK_CANCEL_METHOD, MCP_TASK_GET_METHOD, MCP_TASK_RETRY_METHOD,
};
use goose::mcp_platform::manifest::{Architecture, Platform};
use goose::mcp_platform::service::{Clock, IdGenerator};
use goose::mcp_platform::{
    parse_manifest, CatalogCompatibility, CatalogListInput, CatalogLocator, EventsResumeInput,
    InstallConfirmInput, InstallationScope, ManifestProof, ManifestRecord, McpPlatformErrorCode,
    McpPlatformService, McpPlatformServiceOptions, PlanCreateInput, PlanIntent, RequestContext,
    RollbackStatus, SqliteMcpPlatformRepository, TaskCancelInput, TaskGetInput, TaskRetryInput,
    TaskStatus, TaskTransition, TrustTier, UserDecision,
};
use goose::scheduler::{ScheduledJob, SchedulerError};
use goose::scheduler_trait::SchedulerTrait;
use goose::session::Session;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;

const REMOTE: &str =
    include_str!("../../../documentation/static/schemas/examples/remote-http.json");
const MANUAL: &str =
    include_str!("../../../documentation/static/schemas/examples/manual-stdio.json");
const NPM: &str = include_str!("../../../documentation/static/schemas/examples/npm-package.json");
const BINARY: &str =
    include_str!("../../../documentation/static/schemas/examples/binary-archive-houdini.json");

#[derive(Default)]
struct TestClock(AtomicI64);

impl TestClock {
    fn new(now_ms: i64) -> Self {
        Self(AtomicI64::new(now_ms))
    }

    fn set(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct TestIds(AtomicU64);

impl IdGenerator for TestIds {
    fn next_id(&self, prefix: &str) -> String {
        let value = self.0.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}_{value:04}")
    }
}

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

    async fn schedule_recipe(
        &self,
        _recipe_path: PathBuf,
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
    ) -> Result<Option<(String, DateTime<Utc>)>, SchedulerError> {
        unreachable!()
    }
}

struct Harness {
    _directory: TempDir,
    path: PathBuf,
    repository: Arc<SqliteMcpPlatformRepository>,
    service: Arc<McpPlatformService>,
    clock: Arc<TestClock>,
}

impl Harness {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp-platform/platform.db");
        let repository = Arc::new(SqliteMcpPlatformRepository::open_path(&path).await.unwrap());
        let clock = Arc::new(TestClock::new(100));
        let service = Arc::new(McpPlatformService::new(
            repository.clone(),
            clock.clone(),
            Arc::new(TestIds::default()),
            McpPlatformServiceOptions {
                compatibility_target: goose::mcp_platform::CompatibilityTarget {
                    platform: Platform::Windows,
                    arch: Architecture::Aarch64,
                },
                plan_ttl_ms: 1_000,
                development_mode: false,
                docker_daemon_policy_allowed: true,
            },
        ));
        Self {
            _directory: directory,
            path,
            repository,
            service,
            clock,
        }
    }

    async fn seed_catalog(&self) -> BTreeMap<&'static str, String> {
        let fixtures = [
            ("remote", REMOTE, TrustTier::Official),
            ("manual", MANUAL, TrustTier::Local),
            ("npm", NPM, TrustTier::Community),
            ("binary", BINARY, TrustTier::Local),
        ];
        let mut digests = BTreeMap::new();
        for (index, (name, fixture, trust_tier)) in fixtures.into_iter().enumerate() {
            let verified = parse_manifest(fixture.as_bytes()).unwrap();
            digests.insert(name, verified.digest().to_string());
            self.repository
                .save_manifest(&ManifestRecord {
                    verified,
                    proof: ManifestProof::LocalBytes,
                    trust_tier,
                    created_at_ms: 10 + index as i64,
                })
                .await
                .unwrap();
        }
        digests
    }

    fn context(&self) -> RequestContext {
        self.service.trusted_local_context()
    }

    async fn create_plan(&self, digest: &str, key: &str) -> goose::mcp_platform::PlanReview {
        self.service
            .plan_create(
                &self.context(),
                PlanCreateInput {
                    intent: PlanIntent::Register {
                        manifest_digest: digest.to_string(),
                        installation_scope: InstallationScope::User,
                    },
                    idempotency_key: key.to_string(),
                },
            )
            .await
            .unwrap()
    }
}

async fn raw_pool(path: &Path) -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn catalog_is_stably_paged_filtered_and_integrity_checked() {
    let harness = Harness::new().await;
    let digests = harness.seed_catalog().await;
    let context = harness.context();

    let first = harness
        .service
        .catalog_list(
            &context,
            CatalogListInput {
                page_size: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.offline);
    assert!(first.local_persistence_only);
    assert_eq!(first.newest_verified_at_ms, Some(13));
    let second = harness
        .service
        .catalog_list(
            &context,
            CatalogListInput {
                cursor: first.next_cursor.clone(),
                page_size: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let identities = first
        .items
        .iter()
        .chain(&second.items)
        .map(|item| item.mcp_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(identities.len(), 4);
    assert!(identities.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(second.newest_verified_at_ms, first.newest_verified_at_ms);

    let last = second.items.last().unwrap();
    let after_last = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({
            "mcp_id": last.mcp_id,
            "version": last.version,
            "digest": last.manifest_digest,
        }))
        .unwrap(),
    );
    let empty_page = harness
        .service
        .catalog_list(
            &context,
            CatalogListInput {
                cursor: Some(after_last),
                page_size: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(empty_page.items.is_empty());
    assert_eq!(
        empty_page.newest_verified_at_ms,
        first.newest_verified_at_ms
    );

    let query = harness
        .service
        .catalog_list(
            &context,
            CatalogListInput {
                query: Some("knowledge".to_string()),
                trust_tiers: vec![TrustTier::Official],
                source_ids: vec!["local_persistence".to_string()],
                compatibility: Some(CatalogCompatibility::Compatible),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(query.items.len(), 1);
    assert_eq!(query.items[0].manifest_digest, digests["remote"]);

    let incompatible = harness
        .service
        .catalog_list(
            &context,
            CatalogListInput {
                compatibility: Some(CatalogCompatibility::Incompatible),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(incompatible.items.len(), 1);
    assert_eq!(incompatible.items[0].manifest_digest, digests["binary"]);

    for invalid in [Some(0), Some(101)] {
        assert_eq!(
            harness
                .service
                .catalog_list(
                    &context,
                    CatalogListInput {
                        page_size: invalid,
                        ..Default::default()
                    },
                )
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::InvalidRequest
        );
    }
    assert_eq!(
        harness
            .service
            .catalog_list(
                &context,
                CatalogListInput {
                    cursor: Some("not-an-opaque-cursor".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::InvalidRequest
    );

    let detail = harness
        .service
        .catalog_detail(
            &context,
            CatalogLocator::CatalogRef {
                source_id: "local_persistence".to_string(),
                mcp_id: "com.example.knowledge-search".to_string(),
                version: "1.4.2".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(detail.manifest_digest, digests["remote"]);
    assert_eq!(detail.manifest.publisher.id, "com.example");

    let pool = raw_pool(&harness.path).await;
    sqlx::query("UPDATE manifest_blobs SET canonical_bytes = '{}' WHERE manifest_digest = ?")
        .bind(&digests["remote"])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        harness
            .service
            .catalog_detail(
                &context,
                CatalogLocator::ManifestDigest(digests["remote"].clone()),
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
}

#[tokio::test]
async fn planning_supports_only_remote_and_manual_with_server_owned_envelopes() {
    let harness = Harness::new().await;
    let digests = harness.seed_catalog().await;

    for (name, key) in [("remote", "plan-remote-key"), ("manual", "plan-manual-key")] {
        let review = harness.create_plan(&digests[name], key).await;
        assert!(review.plan_id.starts_with("plan_"));
        assert_eq!(review.expires_at_ms, 1_100);
        assert!(!review.plan.default_enabled());
        assert!(!review.plan.effects().downloads_artifacts);
        assert!(!review.plan.effects().writes_files);
        assert!(!review.plan.effects().removes_files);
        assert_eq!(review.target.installation_scope.as_deref(), Some("user"));
        let replay = harness.create_plan(&digests[name], key).await;
        assert_eq!(replay.plan_id, review.plan_id);
        assert_eq!(replay.plan_digest, review.plan_digest);
    }

    let conflict = harness
        .service
        .plan_create(
            &harness.context(),
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest: digests["manual"].clone(),
                    installation_scope: InstallationScope::User,
                },
                idempotency_key: "plan-remote-key".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code(), McpPlatformErrorCode::IdempotencyConflict);

    let unsupported_distribution = harness
        .service
        .plan_create(
            &harness.context(),
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest: digests["npm"].clone(),
                    installation_scope: InstallationScope::User,
                },
                idempotency_key: "npm-plan-key".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        unsupported_distribution.code(),
        McpPlatformErrorCode::NotImplementedForPhase
    );

    let unsupported_operation = harness
        .service
        .plan_create(
            &harness.context(),
            PlanCreateInput {
                intent: PlanIntent::Install {
                    manifest_digest: digests["remote"].clone(),
                },
                idempotency_key: "install-plan-key".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        unsupported_operation.code(),
        McpPlatformErrorCode::OperationNotSupported
    );
}

#[tokio::test]
async fn confirmation_and_rejection_are_durable_concurrent_and_trusted() {
    let harness = Harness::new().await;
    let digests = harness.seed_catalog().await;
    let plan = harness
        .create_plan(&digests["remote"], "confirm-plan-key")
        .await;
    let confirm = InstallConfirmInput {
        plan_id: plan.plan_id.clone(),
        plan_digest: plan.plan_digest.clone(),
        decision: UserDecision::Confirm,
        idempotency_key: "confirm-task-key".to_string(),
    };

    let mut wrong = confirm.clone();
    wrong.plan_digest = "f".repeat(64);
    assert_eq!(
        harness
            .service
            .install_confirm(&harness.context(), wrong)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanStale
    );

    let confirmed = harness
        .service
        .install_confirm(&harness.context(), confirm.clone())
        .await
        .unwrap();
    assert_eq!(confirmed.status, TaskStatus::Queued);
    assert_eq!(
        harness
            .service
            .install_confirm(&harness.context(), confirm.clone())
            .await
            .unwrap(),
        confirmed
    );
    let mut opposite = confirm.clone();
    opposite.decision = UserDecision::Reject;
    assert_eq!(
        harness
            .service
            .install_confirm(&harness.context(), opposite)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IdempotencyConflict
    );

    let rejection_plan = harness
        .create_plan(&digests["manual"], "reject-plan-key")
        .await;
    let rejected = harness
        .service
        .install_confirm(
            &harness.context(),
            InstallConfirmInput {
                plan_id: rejection_plan.plan_id,
                plan_digest: rejection_plan.plan_digest,
                decision: UserDecision::Reject,
                idempotency_key: "reject-task-key".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, TaskStatus::Cancelled);
    let rejection_events = harness
        .repository
        .list_audit_events(&rejected.task_id)
        .await
        .unwrap();
    assert!(rejection_events.iter().all(|event| {
        event.actor == "local_authenticated_client" && !event.actor.contains("renderer")
    }));
    assert!(rejection_events.iter().any(|event| {
        event.event_type == goose::mcp_platform::AuditEventType::CancellationRequested
    }));

    let race_plan = harness
        .create_plan(&digests["remote"], "race-plan-key")
        .await;
    let race_input = InstallConfirmInput {
        plan_id: race_plan.plan_id,
        plan_digest: race_plan.plan_digest,
        decision: UserDecision::Confirm,
        idempotency_key: "race-task-key".to_string(),
    };
    let left_service = harness.service.clone();
    let right_service = harness.service.clone();
    let left_context = harness.context();
    let right_context = harness.context();
    let right_input = race_input.clone();
    let (left, right) = tokio::join!(
        left_service.install_confirm(&left_context, race_input),
        right_service.install_confirm(&right_context, right_input)
    );
    assert_eq!(left.unwrap().task_id, right.unwrap().task_id);

    let expired_plan = harness
        .create_plan(&digests["remote"], "expired-plan-key")
        .await;
    harness.clock.set(expired_plan.expires_at_ms);
    assert_eq!(
        harness
            .service
            .install_confirm(
                &harness.context(),
                InstallConfirmInput {
                    plan_id: expired_plan.plan_id,
                    plan_digest: expired_plan.plan_digest,
                    decision: UserDecision::Confirm,
                    idempotency_key: "expired-task-key".to_string(),
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanExpired
    );

    let pool = raw_pool(&harness.path).await;
    let projections: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM connection_projections")
        .fetch_one(&pool)
        .await
        .unwrap();
    let managed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM managed_mcps")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((projections, managed), (0, 0));
}

#[tokio::test]
async fn task_controls_and_global_event_resume_preserve_revision_and_cursor_semantics() {
    let harness = Harness::new().await;
    let digests = harness.seed_catalog().await;
    let plan = harness
        .create_plan(&digests["remote"], "task-plan-key")
        .await;
    let task = harness
        .service
        .install_confirm(
            &harness.context(),
            InstallConfirmInput {
                plan_id: plan.plan_id,
                plan_digest: plan.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "task-control-key".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        harness
            .service
            .task_get(
                &harness.context(),
                TaskGetInput {
                    task_id: task.task_id.clone(),
                },
            )
            .await
            .unwrap(),
        task
    );
    assert_eq!(
        harness
            .service
            .task_cancel(
                &harness.context(),
                TaskCancelInput {
                    task_id: task.task_id.clone(),
                    expected_revision: task.revision + 1,
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::RevisionConflict
    );
    let cancelled = harness
        .service
        .task_cancel(
            &harness.context(),
            TaskCancelInput {
                task_id: task.task_id.clone(),
                expected_revision: task.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    assert!(!cancelled.cancellable);
    assert_eq!(
        harness
            .service
            .task_cancel(
                &harness.context(),
                TaskCancelInput {
                    task_id: task.task_id,
                    expected_revision: task.revision,
                },
            )
            .await
            .unwrap(),
        cancelled
    );

    let retry_plan = harness
        .create_plan(&digests["remote"], "retry-plan-key")
        .await;
    let retry_task = harness
        .service
        .install_confirm(
            &harness.context(),
            InstallConfirmInput {
                plan_id: retry_plan.plan_id,
                plan_digest: retry_plan.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "retry-task-key".to_string(),
            },
        )
        .await
        .unwrap();
    let running = harness
        .repository
        .transition_task(TaskTransition {
            task_id: &retry_task.task_id,
            expected_revision: retry_task.revision,
            next_status: TaskStatus::Running,
            actor: "system-local",
            now_ms: 200,
            heartbeat_at_ms: Some(200),
            progress: 0,
            redacted_error: None,
            rollback_status: RollbackStatus::NotRequired,
            rollback_evidence: None,
        })
        .await
        .unwrap();
    let failed = harness
        .repository
        .transition_task(TaskTransition {
            task_id: &running.task_id,
            expected_revision: running.revision,
            next_status: TaskStatus::Failed,
            actor: "system-local",
            now_ms: 201,
            heartbeat_at_ms: Some(201),
            progress: 0,
            redacted_error: None,
            rollback_status: RollbackStatus::NotRequired,
            rollback_evidence: None,
        })
        .await
        .unwrap();
    assert_eq!(
        harness
            .service
            .task_cancel(
                &harness.context(),
                TaskCancelInput {
                    task_id: failed.task_id.clone(),
                    expected_revision: failed.revision,
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::InvalidTransition
    );
    assert_eq!(
        harness
            .service
            .task_retry(
                &harness.context(),
                TaskRetryInput {
                    task_id: failed.task_id.clone(),
                    expected_revision: failed.revision + 1,
                    idempotency_key: "retry-request-key".to_string(),
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::RevisionConflict
    );
    let retried = harness
        .service
        .task_retry(
            &harness.context(),
            TaskRetryInput {
                task_id: failed.task_id.clone(),
                expected_revision: failed.revision,
                idempotency_key: "retry-request-key".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(retried.status, TaskStatus::Queued);
    assert_eq!(
        harness
            .service
            .task_retry(
                &harness.context(),
                TaskRetryInput {
                    task_id: failed.task_id,
                    expected_revision: retried.revision,
                    idempotency_key: "retry-request-key".to_string(),
                },
            )
            .await
            .unwrap(),
        retried
    );

    let first = harness
        .service
        .events_resume(
            &harness.context(),
            EventsResumeInput {
                after_event_id: None,
                limit: Some(2),
                task_ids: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.events.len(), 2);
    assert!(first.events[0].event_id < first.events[1].event_id);
    assert!(first.events.iter().all(|event| event.sequence > 0));
    let second = harness
        .service
        .events_resume(
            &harness.context(),
            EventsResumeInput {
                after_event_id: Some(first.next_event_id),
                limit: Some(200),
                task_ids: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(second
        .events
        .first()
        .is_none_or(|event| event.event_id > first.next_event_id));
    assert!(second
        .tasks
        .iter()
        .any(|task| task.task_id == retried.task_id));

    let filtered = harness
        .service
        .events_resume(
            &harness.context(),
            EventsResumeInput {
                after_event_id: None,
                limit: Some(200),
                task_ids: vec![retried.task_id.clone()],
            },
        )
        .await
        .unwrap();
    assert!(filtered
        .events
        .iter()
        .all(|event| event.task_id == retried.task_id));
    assert_eq!(filtered.tasks.len(), 1);
    assert_eq!(filtered.tasks[0].status, TaskStatus::Queued);
}

#[tokio::test]
async fn sparse_filtered_event_resume_is_bounded_and_does_not_repeat_or_skip() {
    let harness = Harness::new().await;
    let digests = harness.seed_catalog().await;

    for index in 0..24 {
        let plan = harness
            .create_plan(&digests["remote"], &format!("noise-before-plan-{index}"))
            .await;
        harness
            .service
            .install_confirm(
                &harness.context(),
                InstallConfirmInput {
                    plan_id: plan.plan_id,
                    plan_digest: plan.plan_digest,
                    decision: UserDecision::Confirm,
                    idempotency_key: format!("noise-before-task-{index}"),
                },
            )
            .await
            .unwrap();
    }

    let target_plan = harness
        .create_plan(&digests["remote"], "sparse-target-plan")
        .await;
    let target = harness
        .service
        .install_confirm(
            &harness.context(),
            InstallConfirmInput {
                plan_id: target_plan.plan_id,
                plan_digest: target_plan.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "sparse-target-task".to_string(),
            },
        )
        .await
        .unwrap();

    for index in 0..40 {
        let plan = harness
            .create_plan(&digests["remote"], &format!("noise-after-plan-{index}"))
            .await;
        harness
            .service
            .install_confirm(
                &harness.context(),
                InstallConfirmInput {
                    plan_id: plan.plan_id,
                    plan_digest: plan.plan_digest,
                    decision: UserDecision::Confirm,
                    idempotency_key: format!("noise-after-task-{index}"),
                },
            )
            .await
            .unwrap();
    }

    let expected = harness
        .repository
        .list_audit_events(&target.task_id)
        .await
        .unwrap()
        .into_iter()
        .map(|event| event.event_id)
        .collect::<Vec<_>>();
    let mut cursor = 0;
    let mut observed = Vec::new();
    for _ in 0..expected.len() {
        let page = harness
            .service
            .events_resume(
                &harness.context(),
                EventsResumeInput {
                    after_event_id: Some(cursor),
                    limit: Some(1),
                    task_ids: vec![target.task_id.clone()],
                },
            )
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);
        assert!(page.next_event_id > cursor);
        observed.push(page.events[0].event_id);
        cursor = page.next_event_id;
    }
    assert_eq!(observed, expected);

    let exhausted = harness
        .service
        .events_resume(
            &harness.context(),
            EventsResumeInput {
                after_event_id: Some(cursor),
                limit: Some(1),
                task_ids: vec![target.task_id.clone()],
            },
        )
        .await
        .unwrap();
    assert!(exhausted.events.is_empty());
    assert!(exhausted.next_event_id > cursor);
    let high_water = exhausted.next_event_id;

    let resumed = harness
        .service
        .events_resume(
            &harness.context(),
            EventsResumeInput {
                after_event_id: Some(high_water),
                limit: Some(1),
                task_ids: vec![target.task_id],
            },
        )
        .await
        .unwrap();
    assert!(resumed.events.is_empty());
    assert_eq!(resumed.next_event_id, high_water);
}

#[tokio::test]
async fn service_records_survive_reopen_and_dispatch_schema_registers_all_methods() {
    let harness = Harness::new().await;
    let digests = harness.seed_catalog().await;
    let plan = harness
        .create_plan(&digests["remote"], "reopen-plan-key")
        .await;
    let task = harness
        .service
        .install_confirm(
            &harness.context(),
            InstallConfirmInput {
                plan_id: plan.plan_id.clone(),
                plan_digest: plan.plan_digest.clone(),
                decision: UserDecision::Confirm,
                idempotency_key: "reopen-task-key".to_string(),
            },
        )
        .await
        .unwrap();
    harness.repository.close().await;

    let reopened = Arc::new(
        SqliteMcpPlatformRepository::open_path(&harness.path)
            .await
            .unwrap(),
    );
    let service = McpPlatformService::new(
        reopened,
        Arc::new(TestClock::new(500)),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions::default(),
    );
    let context = service.trusted_local_context();
    assert_eq!(
        service
            .task_get(
                &context,
                TaskGetInput {
                    task_id: task.task_id.clone(),
                },
            )
            .await
            .unwrap()
            .status,
        TaskStatus::Queued
    );
    assert_eq!(
        service
            .catalog_detail(
                &context,
                CatalogLocator::ManifestDigest(digests["remote"].clone()),
            )
            .await
            .unwrap()
            .manifest_digest,
        digests["remote"]
    );

    let methods = GooseAcpAgent::custom_method_schemas(&mut schemars::SchemaGenerator::default())
        .into_iter()
        .map(|schema| schema.method)
        .collect::<Vec<_>>();
    for expected in [
        MCP_CATALOG_LIST_METHOD,
        MCP_CATALOG_DETAIL_METHOD,
        MCP_PLAN_CREATE_METHOD,
        MCP_INSTALL_CONFIRM_METHOD,
        MCP_TASK_GET_METHOD,
        MCP_TASK_CANCEL_METHOD,
        MCP_TASK_RETRY_METHOD,
        MCP_EVENTS_RESUME_METHOD,
    ] {
        assert!(
            methods.iter().any(|method| method == expected),
            "{expected}"
        );
    }
}

#[tokio::test]
async fn acp_handler_maps_repository_failure_without_breaking_existing_custom_methods() {
    let harness = Harness::new().await;
    let provider_factory: AcpProviderFactory = Arc::new(|_, _, _| {
        Box::pin(async { Err(anyhow::anyhow!("provider is unused in MCP handler test")) })
    });
    let agent = GooseAcpAgent::new(GooseAcpAgentOptions {
        provider_factory,
        builtins: Vec::new(),
        data_dir: harness._directory.path().join("agent-data"),
        config_dir: harness._directory.path().join("agent-config"),
        disable_session_naming: true,
        goose_platform: GoosePlatform::GooseCli,
        additional_source_roots: Vec::new(),
        scheduler: Arc::new(UnusedScheduler),
        mcp_platform_service: Some(harness.service.clone()),
        mcp_platform_service_cell: None,
    })
    .await
    .unwrap();
    harness.repository.close().await;

    let value = agent
        .dispatch_custom_request(MCP_CATALOG_LIST_METHOD, serde_json::json!({"pageSize": 10}))
        .await
        .unwrap();
    let response: McpCatalogListResponse = serde_json::from_value(value).unwrap();
    let McpPlatformOutcome::Error { error } = response.outcome else {
        panic!("closed repository must return a typed MCP error");
    };
    assert_eq!(error.code, McpPlatformErrorCodeDto::RepositoryUnavailable);
    assert!(error.retryable);
    assert!(error.correlation_id.starts_with("correlation_"));
    let serialized = serde_json::to_string(&error).unwrap();
    assert!(!serialized.contains("platform.db"));
    assert!(!serialized.contains("SELECT"));

    let prompts = agent
        .dispatch_custom_request("_goose/unstable/config/prompts/list", serde_json::json!({}))
        .await
        .unwrap();
    assert!(prompts["prompts"].is_array());
}

#[test]
fn wire_requests_and_error_responses_are_closed_and_round_trip() {
    let valid = serde_json::json!({
        "planId": "plan_1",
        "planDigest": "a".repeat(64),
        "userDecision": "confirm",
        "idempotencyKey": "confirm-key"
    });
    let request: McpInstallConfirmRequest = serde_json::from_value(valid.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap()["userDecision"],
        "confirm"
    );
    let mut unknown = valid.clone();
    unknown["actor"] = serde_json::json!("renderer-forged");
    assert!(serde_json::from_value::<McpInstallConfirmRequest>(unknown).is_err());
    let mut missing = valid;
    missing.as_object_mut().unwrap().remove("userDecision");
    assert!(serde_json::from_value::<McpInstallConfirmRequest>(missing).is_err());

    let schemas = [
        schemars::schema_for!(McpCatalogListRequest),
        schemars::schema_for!(McpCatalogDetailRequest),
        schemars::schema_for!(McpPlanCreateRequest),
        schemars::schema_for!(McpInstallConfirmRequest),
        schemars::schema_for!(McpTaskGetRequest),
        schemars::schema_for!(McpTaskCancelRequest),
        schemars::schema_for!(McpTaskRetryRequest),
        schemars::schema_for!(McpEventsResumeRequest),
    ];
    for schema in schemas {
        let value = serde_json::to_value(schema).unwrap();
        let text = serde_json::to_string(&value).unwrap();
        assert_eq!(value["additionalProperties"], false);
        for forbidden in ["\"actor\"", "\"command\"", "\"shell\""] {
            assert!(!text.contains(forbidden));
        }
    }

    let response = McpTaskGetResponse {
        outcome: McpPlatformOutcome::Error {
            error: McpPlatformErrorEnvelope {
                code: McpPlatformErrorCodeDto::RepositoryUnavailable,
                message: "Repository unavailable.".to_string(),
                retryable: true,
                correlation_id: "correlation_test".to_string(),
                details: Some(McpPlatformErrorDetails::RepositoryTemporarilyUnavailable {}),
            },
        },
    };
    let value = serde_json::to_value(&response).unwrap();
    let decoded: McpTaskGetResponse = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);

    let without_details = serde_json::json!({
        "outcome": {
            "status": "error",
            "error": {
                "code": "invalid_request",
                "message": "Invalid request.",
                "retryable": false,
                "correlationId": "correlation_without_details"
            }
        }
    });
    let decoded: McpTaskGetResponse = serde_json::from_value(without_details).unwrap();
    let McpPlatformOutcome::Error { error } = decoded.outcome else {
        panic!("expected error outcome");
    };
    assert!(error.details.is_none());
}

#[test]
fn acp_boundary_source_has_no_repository_or_execution_escape_hatch() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let wire = std::fs::read_to_string(
        root.join("../goose-sdk-types/src/custom_requests/mcp_platform.rs"),
    )
    .unwrap();
    let wire = wire.split("#[cfg(test)]").next().unwrap();
    let handler = std::fs::read_to_string(root.join("src/acp/server/mcp_platform.rs")).unwrap();
    for forbidden in ["serde_json::Value", "generic execute", "shell_command"] {
        assert!(!wire.contains(forbidden));
        assert!(!handler.contains(forbidden));
    }
    for forbidden_import in ["sqlx::", "mcp_platform::repository::"] {
        assert!(!handler.contains(forbidden_import));
    }

    let repository =
        std::fs::read_to_string(root.join("src/mcp_platform/repository/sqlite/task_repository.rs"))
            .unwrap();
    let global_query = repository
        .split("pub async fn list_global_audit_events")
        .nth(1)
        .unwrap();
    assert!(global_query.contains("COALESCE(MAX(event_id)"));
    assert!(global_query.contains("QueryBuilder::<Sqlite>"));
    assert!(global_query.contains("push_bind(task_id)"));
    assert!(global_query.contains("ORDER BY event_id LIMIT"));
    assert!(
        !global_query.contains("SELECT * FROM audit_events WHERE event_id > ? ORDER BY event_id")
    );
}
