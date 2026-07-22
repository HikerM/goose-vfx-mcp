use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use goose::acp::server_factory::{AcpServer, AcpServerFactoryConfig};
use goose::agents::{ExtensionConfig, GoosePlatform};
use goose::config::extensions::{
    get_enabled_extensions_with_config, try_remove_extension, try_set_extension_at_key,
    try_set_extension_enabled, ExtensionEntry,
};
use goose::config::Config;
use goose::custom_requests::{
    McpListResponse, McpPlatformErrorCodeDto, McpPlatformOutcome, MCP_LIST_METHOD,
};
use goose::mcp_platform::manifest::{Architecture, Auth, Platform};
use goose::mcp_platform::service::{Clock, IdGenerator};
use goose::mcp_platform::{
    parse_manifest, run_bounded_health_session, AuthRequirement, AuthRequirementResolver,
    CompensationDescriptor, CompensationStatus, ConfigProjectionSink,
    CoreTransportProjectionAdapter, EmptyHostIntegrationAdapter, HealthAdapterResult,
    HealthCheckAdapter, HealthCheckMode, HealthCheckSession, HealthDetailCode, HealthExecution,
    HealthResultCode, HealthRunInput, HealthState, InstallConfirmInput, InstallationScope,
    LifecyclePorts, ManagedGetInput, ManagedListInput, ManifestProof, ManifestRecord,
    McpPlatformError, McpPlatformErrorCode, McpPlatformResult, McpPlatformService,
    McpPlatformServiceOptions, PlanCreateInput, PlanIntent, ProjectionMutationStatus,
    ProjectionSink, ProjectionSnapshot, RegistrationEffect, RegistrationEffectAdapter,
    RegistrationEffectEvidence, RegistrationState, RequestContext, RollbackStatus, RuntimeState,
    SetDefaultEnabledInput, SqliteMcpPlatformRepository, TaskCancelInput, TaskRetryInput,
    TaskRunner, TaskStatus, TaskStepStatus, TaskTransition, TrustTier, UserDecision,
};
use sha2::{Digest as _, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const REMOTE: &str =
    include_str!("../../../documentation/static/schemas/examples/remote-http.json");
const MANUAL: &str =
    include_str!("../../../documentation/static/schemas/examples/manual-stdio.json");

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
        format!("{prefix}_{:06}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

#[derive(Default)]
struct FakeRegistration {
    effects: Mutex<Vec<RegistrationEffect>>,
    fail: AtomicBool,
}

#[async_trait]
impl RegistrationEffectAdapter for FakeRegistration {
    fn adapter_id(&self) -> &'static str {
        "connection_registration"
    }

    fn adapter_version(&self) -> &'static str {
        "1"
    }

    async fn verify(
        &self,
        effect: &RegistrationEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<RegistrationEffectEvidence> {
        if cancellation.is_cancelled() {
            return Err(platform_error(McpPlatformErrorCode::TaskNotCancellable));
        }
        self.effects.lock().unwrap().push(effect.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(platform_error(McpPlatformErrorCode::RepositoryUnavailable));
        }
        Ok(RegistrationEffectEvidence {
            adapter_id: self.adapter_id().to_string(),
            adapter_version: self.adapter_version().to_string(),
        })
    }
}

#[derive(Default)]
struct FakeAuth {
    ready: AtomicBool,
}

impl FakeAuth {
    fn ready() -> Self {
        Self {
            ready: AtomicBool::new(true),
        }
    }
}

impl AuthRequirementResolver for FakeAuth {
    fn requirement(&self, auth: &Auth) -> McpPlatformResult<AuthRequirement> {
        if matches!(auth, Auth::None) || self.ready.load(Ordering::SeqCst) {
            Ok(AuthRequirement::Ready)
        } else {
            Ok(AuthRequirement::MissingOpaqueHandle {
                credential_name: "opaque_test_handle".to_string(),
            })
        }
    }
}

struct FakeHealth {
    result: Mutex<HealthAdapterResult>,
    calls: AtomicUsize,
    block_until_cancelled: AtomicBool,
    entered: Notify,
}

impl Default for FakeHealth {
    fn default() -> Self {
        Self {
            result: Mutex::new(health_result(
                HealthResultCode::Healthy,
                HealthDetailCode::McpInitializeSucceeded,
            )),
            calls: AtomicUsize::new(0),
            block_until_cancelled: AtomicBool::new(false),
            entered: Notify::new(),
        }
    }
}

#[async_trait]
impl HealthCheckAdapter for FakeHealth {
    fn adapter_id(&self) -> &'static str {
        "mcp_health"
    }

    fn adapter_version(&self) -> &'static str {
        "1"
    }

    async fn run(
        &self,
        _execution: HealthExecution,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<HealthAdapterResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.block_until_cancelled.load(Ordering::SeqCst) {
            self.entered.notify_one();
            cancellation.cancelled().await;
        }
        if cancellation.is_cancelled() {
            return Ok(health_result(
                HealthResultCode::Cancelled,
                HealthDetailCode::Cancelled,
            ));
        }
        Ok(self.result.lock().unwrap().clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockingHealthPhase {
    Open,
    Initialize,
    ListTools,
    None,
}

struct PhaseHealthSession {
    block_phase: BlockingHealthPhase,
    entered: Notify,
    release: Notify,
    resource_created: AtomicBool,
    manager_entry: AtomicBool,
    close_count: AtomicUsize,
    close_fails: bool,
}

impl PhaseHealthSession {
    fn new(block_phase: BlockingHealthPhase, close_fails: bool) -> Self {
        Self {
            block_phase,
            entered: Notify::new(),
            release: Notify::new(),
            resource_created: AtomicBool::new(false),
            manager_entry: AtomicBool::new(false),
            close_count: AtomicUsize::new(0),
            close_fails,
        }
    }

    async fn block(&self, phase: BlockingHealthPhase) {
        if self.block_phase == phase {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait]
impl HealthCheckSession for PhaseHealthSession {
    async fn open(&self) -> McpPlatformResult<()> {
        self.resource_created.store(true, Ordering::SeqCst);
        self.block(BlockingHealthPhase::Open).await;
        self.manager_entry.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn initialize(&self) -> McpPlatformResult<()> {
        self.block(BlockingHealthPhase::Initialize).await;
        Ok(())
    }

    async fn list_tools(&self) -> McpPlatformResult<Option<String>> {
        self.block(BlockingHealthPhase::ListTools).await;
        Ok(Some("c".repeat(64)))
    }

    async fn close(&self) -> McpPlatformResult<()> {
        self.close_count.fetch_add(1, Ordering::SeqCst);
        self.manager_entry.store(false, Ordering::SeqCst);
        self.resource_created.store(false, Ordering::SeqCst);
        if self.close_fails {
            Err(platform_error(McpPlatformErrorCode::HealthFailed))
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct FakeProjectionSink {
    entries: Mutex<BTreeMap<String, ExtensionEntry>>,
    fail_put: AtomicBool,
    fail_set: AtomicBool,
    fail_remove: AtomicBool,
    puts: AtomicUsize,
    removes: AtomicUsize,
    block_after_put: AtomicBool,
    put_entered: Notify,
    put_release: Notify,
}

#[async_trait]
impl ProjectionSink for FakeProjectionSink {
    fn adapter_id(&self) -> &'static str {
        "extension_config_sink"
    }

    fn adapter_version(&self) -> &'static str {
        "1"
    }

    async fn put_disabled(
        &self,
        key: &str,
        config: ExtensionConfig,
    ) -> McpPlatformResult<ProjectionSnapshot> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        if self.fail_put.load(Ordering::SeqCst) {
            return Err(platform_error(McpPlatformErrorCode::RepositoryUnavailable));
        }
        let entry = ExtensionEntry {
            enabled: false,
            config,
        };
        let snapshot = {
            let mut entries = self.entries.lock().unwrap();
            match entries.get(key) {
                Some(existing) if existing == &entry => Ok(ProjectionSnapshot {
                    entry,
                    created: false,
                }),
                Some(_) => Err(platform_error(McpPlatformErrorCode::ProjectionConflict)),
                None => {
                    entries.insert(key.to_string(), entry.clone());
                    Ok(ProjectionSnapshot {
                        entry,
                        created: true,
                    })
                }
            }
        }?;
        if self.block_after_put.load(Ordering::SeqCst) {
            self.put_entered.notify_one();
            self.put_release.notified().await;
        }
        Ok(snapshot)
    }

    async fn get(&self, key: &str) -> McpPlatformResult<Option<ProjectionSnapshot>> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .map(|entry| ProjectionSnapshot {
                entry,
                created: false,
            }))
    }

    async fn set_enabled(&self, key: &str, enabled: bool) -> McpPlatformResult<ProjectionSnapshot> {
        if self.fail_set.load(Ordering::SeqCst) {
            return Err(platform_error(McpPlatformErrorCode::RepositoryUnavailable));
        }
        let mut entries = self.entries.lock().unwrap();
        let entry = entries
            .get_mut(key)
            .ok_or_else(|| platform_error(McpPlatformErrorCode::NotFound))?;
        entry.enabled = enabled;
        Ok(ProjectionSnapshot {
            entry: entry.clone(),
            created: false,
        })
    }

    async fn remove_owned(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
    ) -> McpPlatformResult<bool> {
        self.removes.fetch_add(1, Ordering::SeqCst);
        if self.fail_remove.load(Ordering::SeqCst) {
            return Err(platform_error(McpPlatformErrorCode::RepositoryUnavailable));
        }
        let mut entries = self.entries.lock().unwrap();
        match entries.get(key) {
            None => Ok(false),
            Some(existing) if existing == &expected.entry => {
                entries.remove(key);
                Ok(true)
            }
            Some(_) => Err(platform_error(McpPlatformErrorCode::ProjectionConflict)),
        }
    }
}

struct Harness {
    _directory: TempDir,
    path: PathBuf,
    repository: Arc<SqliteMcpPlatformRepository>,
    service: Arc<McpPlatformService>,
    clock: Arc<TestClock>,
    ports: LifecyclePorts,
    registration: Arc<FakeRegistration>,
    auth: Arc<FakeAuth>,
    health: Arc<FakeHealth>,
    sink: Arc<FakeProjectionSink>,
}

impl Harness {
    async fn new() -> Self {
        Self::new_for(Platform::Windows, Architecture::Aarch64).await
    }

    async fn new_for(platform: Platform, arch: Architecture) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp-platform").join("platform.db");
        let repository = Arc::new(SqliteMcpPlatformRepository::open_path(&path).await.unwrap());
        let clock = Arc::new(TestClock::new(100));
        let registration = Arc::new(FakeRegistration::default());
        let auth = Arc::new(FakeAuth::ready());
        let health = Arc::new(FakeHealth::default());
        let sink = Arc::new(FakeProjectionSink::default());
        let ports = LifecyclePorts {
            registration: registration.clone(),
            host_integration: Arc::new(EmptyHostIntegrationAdapter),
            transport: Arc::new(CoreTransportProjectionAdapter),
            auth: auth.clone(),
            health: health.clone(),
            projection_sink: sink.clone(),
        };
        let service = Arc::new(McpPlatformService::new_with_lifecycle_ports(
            repository.clone(),
            clock.clone(),
            Arc::new(TestIds::default()),
            McpPlatformServiceOptions {
                compatibility_target: goose::mcp_platform::CompatibilityTarget { platform, arch },
                plan_ttl_ms: 1_000_000,
            },
            ports.clone(),
        ));
        Self {
            _directory: directory,
            path,
            repository,
            service,
            clock,
            ports,
            registration,
            auth,
            health,
            sink,
        }
    }

    fn context(&self) -> RequestContext {
        self.service.trusted_local_context()
    }

    async fn save(&self, source: &str, trust_tier: TrustTier) -> String {
        let verified = parse_manifest(source.as_bytes()).unwrap();
        let digest = verified.digest().to_string();
        self.repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier,
                created_at_ms: self.clock.now_ms(),
            })
            .await
            .unwrap();
        digest
    }

    async fn confirm(
        &self,
        source: &str,
        trust_tier: TrustTier,
        key: &str,
    ) -> goose::mcp_platform::TaskRef {
        let digest = self.save(source, trust_tier).await;
        let plan = self
            .service
            .plan_create(
                &self.context(),
                PlanCreateInput {
                    intent: PlanIntent::Register {
                        manifest_digest: digest,
                        installation_scope: InstallationScope::User,
                    },
                    idempotency_key: format!("plan_{key}"),
                },
            )
            .await
            .unwrap();
        self.service
            .install_confirm(
                &self.context(),
                InstallConfirmInput {
                    plan_id: plan.plan_id,
                    plan_digest: plan.plan_digest,
                    decision: UserDecision::Confirm,
                    idempotency_key: format!("task_{key}"),
                },
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn manual_stdio_descriptor_is_direct_and_stable_on_all_supported_platform_targets() {
    for (index, (platform, arch)) in [
        (Platform::Windows, Architecture::X86_64),
        (Platform::Macos, Architecture::Aarch64),
        (Platform::Linux, Architecture::X86_64),
    ]
    .into_iter()
    .enumerate()
    {
        let harness = Harness::new_for(platform, arch).await;
        let task = harness
            .confirm(MANUAL, TrustTier::Local, &format!("descriptor_{index}"))
            .await;
        assert!(harness.service.runner_tick().await.unwrap());
        assert_eq!(
            task_status(&harness, &task.task_id).await,
            TaskStatus::Succeeded
        );
        let effects = harness.registration.effects.lock().unwrap();
        assert!(
            matches!(effects.as_slice(), [RegistrationEffect::ManualStdio { spawn }] if
            spawn.executable == "mcp-filesystem"
            && spawn.argv == ["--root", "${user.workspace}"]
            && spawn.environment_keys.is_empty())
        );
    }
}

fn platform_error(code: McpPlatformErrorCode) -> McpPlatformError {
    McpPlatformError::new(code, "typed test adapter failure")
}

fn health_result(
    result_code: HealthResultCode,
    detail_code: HealthDetailCode,
) -> HealthAdapterResult {
    HealthAdapterResult {
        result_code,
        latency_ms: 7,
        capabilities_digest: Some("a".repeat(64)),
        tools_digest: Some("b".repeat(64)),
        detail_code,
    }
}

async fn raw_pool(path: &Path) -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .unwrap()
}

async fn task_status(harness: &Harness, task_id: &str) -> TaskStatus {
    harness.repository.get_task(task_id).await.unwrap().status
}

fn stable_link_key(mcp_id: &str) -> String {
    let digest = Sha256::digest(format!("{mcp_id}\0user").as_bytes());
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("managed_mcp_{suffix}")
}

#[tokio::test]
async fn remote_and_manual_registration_execute_to_disabled_durable_inventory() {
    let harness = Harness::new().await;
    let remote = harness.confirm(REMOTE, TrustTier::Official, "remote").await;
    let manual = harness.confirm(MANUAL, TrustTier::Local, "manual").await;

    assert!(harness.service.runner_tick().await.unwrap());
    assert!(harness.service.runner_tick().await.unwrap());
    let remote_record = harness.repository.get_task(&remote.task_id).await.unwrap();
    let remote_steps = harness
        .repository
        .list_task_steps(&remote.task_id)
        .await
        .unwrap();
    assert_eq!(
        remote_record.status,
        TaskStatus::Succeeded,
        "remote task: {remote_record:?}; steps: {remote_steps:?}"
    );
    let manual_record = harness.repository.get_task(&manual.task_id).await.unwrap();
    let manual_steps = harness
        .repository
        .list_task_steps(&manual.task_id)
        .await
        .unwrap();
    assert_eq!(
        manual_record.status,
        TaskStatus::Succeeded,
        "manual task: {manual_record:?}; steps: {manual_steps:?}"
    );

    let effects = harness.registration.effects.lock().unwrap().clone();
    assert!(
        matches!(&effects[0], RegistrationEffect::RemoteHttp { endpoint, .. } if endpoint == "https://mcp.example.com/v1")
    );
    assert!(
        matches!(&effects[1], RegistrationEffect::ManualStdio { spawn } if
        spawn.executable == "mcp-filesystem"
        && spawn.argv == ["--root", "${user.workspace}"]
        && spawn.environment_keys.is_empty())
    );

    let page = harness
        .service
        .managed_list(&harness.context(), ManagedListInput::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    for summary in &page.items {
        assert_eq!(summary.registration, RegistrationState::Registered);
        assert_eq!(summary.health, HealthState::Unknown);
        assert!(!summary.default_enabled);
        let detail = harness
            .service
            .managed_get(
                &harness.context(),
                ManagedGetInput {
                    managed_mcp_id: summary.managed_mcp_id.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(detail.active_manifest_digest.len(), 64);
        assert!(!harness.sink.entries.lock().unwrap()[&detail.extension_config_key].enabled);
    }

    let reopened = SqliteMcpPlatformRepository::open_path(&harness.path)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .list_managed_inventory(None, 10, &Default::default())
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn two_runners_claim_a_queued_task_exactly_once() {
    let harness = Harness::new().await;
    let task = harness.confirm(MANUAL, TrustTier::Local, "claim").await;
    let left = Arc::new(TaskRunner::new(
        harness.repository.clone(),
        harness.clock.clone(),
        harness.ports.clone(),
        "worker_left".to_string(),
    ));
    let right = Arc::new(TaskRunner::new(
        harness.repository.clone(),
        harness.clock.clone(),
        harness.ports.clone(),
        "worker_right".to_string(),
    ));
    let (left_result, right_result) = tokio::join!(left.tick(), right.tick());
    let claimed = usize::from(left_result.unwrap()) + usize::from(right_result.unwrap());
    assert_eq!(claimed, 1);
    assert_eq!(
        task_status(&harness, &task.task_id).await,
        TaskStatus::Succeeded
    );
    assert_eq!(harness.registration.effects.lock().unwrap().len(), 1);
    assert_eq!(harness.sink.puts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn runner_shutdown_is_awaitable_before_start_and_during_active_health() {
    let harness = Harness::new().await;
    let stopped_before_start = Arc::new(TaskRunner::new(
        harness.repository.clone(),
        harness.clock.clone(),
        harness.ports.clone(),
        "stopped_before_start".to_string(),
    ));
    stopped_before_start.shutdown();
    tokio::time::timeout(Duration::from_secs(1), stopped_before_start.run())
        .await
        .unwrap();

    harness
        .confirm(MANUAL, TrustTier::Local, "shutdown_health")
        .await;
    harness.service.runner_tick().await.unwrap();
    let managed = harness
        .service
        .managed_list(&harness.context(), ManagedListInput::default())
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    harness
        .health
        .block_until_cancelled
        .store(true, Ordering::SeqCst);
    harness
        .service
        .health_run(
            &harness.context(),
            HealthRunInput {
                managed_mcp_id: managed.managed_mcp_id,
                mode: HealthCheckMode::Runtime,
                idempotency_key: "shutdown_active_health".to_string(),
            },
        )
        .await
        .unwrap();
    let active = Arc::new(TaskRunner::new(
        harness.repository.clone(),
        harness.clock.clone(),
        harness.ports.clone(),
        "active_shutdown_worker".to_string(),
    ));
    let running = tokio::spawn(active.clone().run());
    harness.health.entered.notified().await;
    active.shutdown();
    tokio::time::timeout(Duration::from_secs(1), running)
        .await
        .unwrap()
        .unwrap();
    assert!(harness
        .sink
        .entries
        .lock()
        .unwrap()
        .values()
        .all(|entry| !entry.enabled));
}

#[tokio::test]
async fn cancellation_before_effect_and_after_projection_effect_settles_safely() {
    let no_effect = Harness::new().await;
    let queued = no_effect
        .confirm(MANUAL, TrustTier::Local, "cancel_queued")
        .await;
    let cancelled = no_effect
        .service
        .task_cancel(
            &no_effect.context(),
            TaskCancelInput {
                task_id: queued.task_id.clone(),
                expected_revision: queued.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    assert!(!no_effect.service.runner_tick().await.unwrap());
    assert!(no_effect.sink.entries.lock().unwrap().is_empty());

    let after_effect = Harness::new().await;
    after_effect
        .sink
        .block_after_put
        .store(true, Ordering::SeqCst);
    let task = after_effect
        .confirm(MANUAL, TrustTier::Local, "cancel_after_put")
        .await;
    let service = after_effect.service.clone();
    let execution = tokio::spawn(async move { service.runner_tick().await });
    tokio::time::timeout(
        Duration::from_secs(5),
        after_effect.sink.put_entered.notified(),
    )
    .await
    .expect("runner did not reach projection effect");
    let current = after_effect
        .repository
        .get_task(&task.task_id)
        .await
        .unwrap();
    assert_eq!(current.status, TaskStatus::Activating);
    tokio::time::timeout(
        Duration::from_secs(5),
        after_effect.service.task_cancel(
            &after_effect.context(),
            TaskCancelInput {
                task_id: task.task_id.clone(),
                expected_revision: current.revision,
            },
        ),
    )
    .await
    .expect("cancellation request blocked")
    .unwrap();
    after_effect.sink.put_release.notify_one();
    assert!(tokio::time::timeout(Duration::from_secs(5), execution)
        .await
        .expect("cancelled runner did not stop")
        .unwrap()
        .unwrap());
    assert_eq!(
        task_status(&after_effect, &task.task_id).await,
        TaskStatus::Cancelled
    );
    assert!(after_effect.sink.entries.lock().unwrap().is_empty());
    let pool = raw_pool(&after_effect.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM managed_mcps")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM connection_projections")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn cancellation_with_failed_compensation_requires_recovery() {
    let harness = Harness::new().await;
    harness.sink.block_after_put.store(true, Ordering::SeqCst);
    harness.sink.fail_remove.store(true, Ordering::SeqCst);
    let task = harness
        .confirm(MANUAL, TrustTier::Local, "cancel_recovery")
        .await;
    let service = harness.service.clone();
    let execution = tokio::spawn(async move { service.runner_tick().await });
    tokio::time::timeout(Duration::from_secs(5), harness.sink.put_entered.notified())
        .await
        .expect("runner did not reach projection effect");
    let current = harness.repository.get_task(&task.task_id).await.unwrap();
    harness
        .service
        .task_cancel(
            &harness.context(),
            TaskCancelInput {
                task_id: task.task_id.clone(),
                expected_revision: current.revision,
            },
        )
        .await
        .unwrap();
    harness.sink.put_release.notify_one();
    assert!(tokio::time::timeout(Duration::from_secs(5), execution)
        .await
        .expect("runner did not settle failed compensation")
        .unwrap()
        .unwrap());
    let failed = harness.repository.get_task(&task.task_id).await.unwrap();
    assert_eq!(failed.status, TaskStatus::RecoveryRequired);
    assert_eq!(failed.rollback_status, RollbackStatus::Incomplete);
    assert!(harness
        .sink
        .entries
        .lock()
        .unwrap()
        .contains_key(&stable_link_key("local.example.filesystem")));
}

#[tokio::test]
async fn projection_sink_failure_and_conflict_rollback_without_overwriting() {
    let failed_put = Harness::new().await;
    failed_put.sink.fail_put.store(true, Ordering::SeqCst);
    let task = failed_put
        .confirm(MANUAL, TrustTier::Local, "put_failure")
        .await;
    assert!(failed_put.service.runner_tick().await.unwrap());
    assert_eq!(
        task_status(&failed_put, &task.task_id).await,
        TaskStatus::Failed
    );
    let pool = raw_pool(&failed_put.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM managed_mcps")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM connection_projections")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    let conflict = Harness::new().await;
    let key = stable_link_key("local.example.filesystem");
    let user_entry = ExtensionEntry {
        enabled: true,
        config: ExtensionConfig::Builtin {
            name: "user-owned-entry".to_string(),
            description: String::new(),
            display_name: None,
            timeout: None,
            bundled: None,
            available_tools: Vec::new(),
        },
    };
    conflict
        .sink
        .entries
        .lock()
        .unwrap()
        .insert(key.clone(), user_entry.clone());
    let task = conflict
        .confirm(MANUAL, TrustTier::Local, "projection_conflict")
        .await;
    assert!(conflict.service.runner_tick().await.unwrap());
    assert_eq!(
        task_status(&conflict, &task.task_id).await,
        TaskStatus::Failed
    );
    assert_eq!(conflict.sink.entries.lock().unwrap()[&key], user_entry);
    assert_eq!(conflict.sink.removes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn started_side_effect_commit_failures_rollback_every_owned_resource() {
    for ordinal in [1_i64, 2, 3] {
        let harness = Harness::new().await;
        let task = harness
            .confirm(MANUAL, TrustTier::Local, &format!("crash_{ordinal}"))
            .await;
        let pool = raw_pool(&harness.path).await;
        sqlx::query(&format!(
            "CREATE TRIGGER inject_commit_failure BEFORE UPDATE OF status ON task_steps \
             WHEN NEW.status = 'committed' AND NEW.ordinal = {ordinal} \
             BEGIN SELECT RAISE(ABORT, 'injected commit failure'); END"
        ))
        .execute(&pool)
        .await
        .unwrap();

        assert!(harness.service.runner_tick().await.unwrap());
        assert_eq!(
            task_status(&harness, &task.task_id).await,
            TaskStatus::Failed
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM managed_mcps")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM connection_projections")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert!(harness.sink.entries.lock().unwrap().is_empty());
        let steps = harness
            .repository
            .list_task_steps(&task.task_id)
            .await
            .unwrap();
        assert!(steps
            .iter()
            .filter(|step| step.status != TaskStepStatus::NotStarted)
            .all(|step| step.compensation_status == CompensationStatus::Committed));
    }
}

#[tokio::test]
async fn compensation_effect_before_commit_replays_idempotently_after_restart() {
    let harness = Harness::new().await;
    let task = harness
        .confirm(MANUAL, TrustTier::Local, "comp_crash")
        .await;
    let pool = raw_pool(&harness.path).await;
    sqlx::query(
        "CREATE TRIGGER fail_forward BEFORE UPDATE OF status ON task_steps \
         WHEN NEW.status = 'committed' AND NEW.ordinal = 3 \
         BEGIN SELECT RAISE(ABORT, 'forward failure'); END",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_compensation_commit BEFORE UPDATE OF compensation_status ON task_steps \
         WHEN NEW.compensation_status = 'committed' AND NEW.ordinal = 3 \
         BEGIN SELECT RAISE(ABORT, 'compensation crash'); END",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(harness.service.runner_tick().await.unwrap());
    assert_eq!(
        task_status(&harness, &task.task_id).await,
        TaskStatus::RecoveryRequired
    );
    assert!(harness.sink.entries.lock().unwrap().is_empty());
    sqlx::query("DROP TRIGGER fail_forward")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER fail_compensation_commit")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE tasks SET status = 'rolling_back', heartbeat_at_ms = 0, updated_at_ms = 0 \
         WHERE task_id = ?",
    )
    .bind(&task.task_id)
    .execute(&pool)
    .await
    .unwrap();
    harness.clock.set(100_000);
    let restarted = TaskRunner::new(
        harness.repository.clone(),
        harness.clock.clone(),
        harness.ports.clone(),
        "worker_restarted".to_string(),
    );
    restarted.recover_startup().await.unwrap();
    assert_eq!(
        task_status(&harness, &task.task_id).await,
        TaskStatus::Failed
    );
    assert!(harness.sink.entries.lock().unwrap().is_empty());
    assert!(harness.sink.removes.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn recovery_isolates_corrupt_steps_and_rejects_incompatible_adapters() {
    let harness = Harness::new().await;
    let first = harness.confirm(MANUAL, TrustTier::Local, "corrupt").await;
    let second = harness
        .confirm(REMOTE, TrustTier::Official, "healthy")
        .await;
    let third_source = REMOTE.replace("com.example.knowledge-search", "com.example.versioned");
    let third = harness
        .confirm(&third_source, TrustTier::Official, "versioned")
        .await;
    let claimed_first = harness
        .repository
        .claim_next_task("old_worker", 100, 30_000)
        .await
        .unwrap()
        .unwrap();
    let claimed_second = harness
        .repository
        .claim_next_task("old_worker", 100, 30_000)
        .await
        .unwrap()
        .unwrap();
    let claimed_third = harness
        .repository
        .claim_next_task("old_worker", 100, 30_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed_first.task_id, first.task_id);
    assert_eq!(claimed_second.task_id, second.task_id);
    assert_eq!(claimed_third.task_id, third.task_id);
    harness
        .repository
        .add_task_step_with_adapter(
            &first.task_id,
            0,
            "corrupt_step",
            &CompensationDescriptor::RemoveManagedMcp {
                managed_mcp_id: "managed_corrupt".to_string(),
            },
            "connection_registration",
            "1",
        )
        .await
        .unwrap();
    harness
        .repository
        .add_task_step_with_adapter(
            &third.task_id,
            0,
            "versioned_step",
            &CompensationDescriptor::RemoveManagedMcp {
                managed_mcp_id: "managed_versioned".to_string(),
            },
            "connection_registration",
            "999",
        )
        .await
        .unwrap();
    let pool = raw_pool(&harness.path).await;
    sqlx::query("UPDATE task_steps SET compensation_json = '{' WHERE task_id = ?")
        .bind(&first.task_id)
        .execute(&pool)
        .await
        .unwrap();
    harness.clock.set(100_000);
    let runner = TaskRunner::new(
        harness.repository.clone(),
        harness.clock.clone(),
        harness.ports.clone(),
        "recovery_worker".to_string(),
    );
    runner.recover_startup().await.unwrap();
    assert_eq!(
        task_status(&harness, &first.task_id).await,
        TaskStatus::RecoveryRequired
    );
    assert_eq!(
        task_status(&harness, &second.task_id).await,
        TaskStatus::Queued
    );
    assert_eq!(
        task_status(&harness, &third.task_id).await,
        TaskStatus::RecoveryRequired
    );
}

#[tokio::test]
async fn repeated_retry_keys_replay_and_distinct_keys_create_durable_attempts() {
    let harness = Harness::new().await;
    let task = harness.confirm(MANUAL, TrustTier::Local, "retry").await;
    let running = harness
        .repository
        .transition_task(TaskTransition {
            task_id: &task.task_id,
            expected_revision: task.revision,
            next_status: TaskStatus::Running,
            actor: "test",
            now_ms: 101,
            heartbeat_at_ms: Some(101),
            progress: 0,
            redacted_error: None,
            rollback_status: RollbackStatus::NotRequired,
            rollback_evidence: None,
        })
        .await
        .unwrap();
    let mut failed = harness
        .repository
        .transition_task(TaskTransition {
            task_id: &task.task_id,
            expected_revision: running.revision,
            next_status: TaskStatus::Failed,
            actor: "test",
            now_ms: 102,
            heartbeat_at_ms: Some(102),
            progress: 0,
            redacted_error: None,
            rollback_status: RollbackStatus::NotRequired,
            rollback_evidence: None,
        })
        .await
        .unwrap();
    for attempt in 1..=3 {
        let key = format!("retry_attempt_{attempt}");
        let queued = harness
            .service
            .task_retry(
                &harness.context(),
                TaskRetryInput {
                    task_id: task.task_id.clone(),
                    expected_revision: failed.revision,
                    idempotency_key: key.clone(),
                },
            )
            .await
            .unwrap();
        let running = harness
            .repository
            .transition_task(TaskTransition {
                task_id: &task.task_id,
                expected_revision: queued.revision,
                next_status: TaskStatus::Running,
                actor: "test",
                now_ms: 110 + attempt,
                heartbeat_at_ms: Some(110 + attempt),
                progress: 0,
                redacted_error: None,
                rollback_status: RollbackStatus::NotRequired,
                rollback_evidence: None,
            })
            .await
            .unwrap();
        failed = harness
            .repository
            .transition_task(TaskTransition {
                task_id: &task.task_id,
                expected_revision: running.revision,
                next_status: TaskStatus::Failed,
                actor: "test",
                now_ms: 120 + attempt,
                heartbeat_at_ms: Some(120 + attempt),
                progress: 0,
                redacted_error: None,
                rollback_status: RollbackStatus::NotRequired,
                rollback_evidence: None,
            })
            .await
            .unwrap();
        let replay = harness
            .service
            .task_retry(
                &harness.context(),
                TaskRetryInput {
                    task_id: task.task_id.clone(),
                    expected_revision: 0,
                    idempotency_key: key,
                },
            )
            .await
            .unwrap();
        assert_eq!(replay.status, TaskStatus::Failed);
        assert_eq!(replay.revision, failed.revision);
    }
    let attempts = harness
        .repository
        .list_retry_attempts(&task.task_id)
        .await
        .unwrap();
    assert_eq!(attempts.len(), 3);
    assert_eq!(
        attempts.iter().map(|item| item.attempt).collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[tokio::test]
async fn health_results_are_orthogonal_and_never_auto_enable() {
    let harness = Harness::new().await;
    let task = harness.confirm(MANUAL, TrustTier::Local, "health").await;
    harness.service.runner_tick().await.unwrap();
    let managed = harness
        .service
        .managed_list(&harness.context(), ManagedListInput::default())
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    assert_eq!(
        task_status(&harness, &task.task_id).await,
        TaskStatus::Succeeded
    );

    let cases = [
        (
            HealthResultCode::Healthy,
            HealthDetailCode::McpInitializeSucceeded,
        ),
        (
            HealthResultCode::Unhealthy,
            HealthDetailCode::McpInitializeFailed,
        ),
        (
            HealthResultCode::Incompatible,
            HealthDetailCode::IncompatibleHealthContract,
        ),
        (HealthResultCode::Timeout, HealthDetailCode::Timeout),
        (HealthResultCode::Cancelled, HealthDetailCode::Cancelled),
    ];
    for (index, (result, detail)) in cases.into_iter().enumerate() {
        *harness.health.result.lock().unwrap() = health_result(result, detail);
        harness
            .service
            .health_run(
                &harness.context(),
                HealthRunInput {
                    managed_mcp_id: managed.managed_mcp_id.clone(),
                    mode: HealthCheckMode::Runtime,
                    idempotency_key: format!("health_{index}"),
                },
            )
            .await
            .unwrap();
        harness.service.runner_tick().await.unwrap();
        let inventory = harness
            .repository
            .get_managed_inventory(&managed.managed_mcp_id)
            .await
            .unwrap();
        assert!(!inventory.managed.state.default_enabled);
        assert_eq!(
            harness
                .repository
                .latest_health_observation(&managed.managed_mcp_id)
                .await
                .unwrap()
                .unwrap()
                .result_code,
            result
        );
    }

    harness.auth.ready.store(false, Ordering::SeqCst);
    let remote_source = REMOTE.replace("com.example.knowledge-search", "com.example.auth-health");
    harness
        .confirm(&remote_source, TrustTier::Official, "blocked_auth")
        .await;
    harness.service.runner_tick().await.unwrap();
    let remote = harness
        .service
        .managed_list(&harness.context(), ManagedListInput::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.mcp_id == "com.example.auth-health")
        .unwrap();
    harness
        .service
        .health_run(
            &harness.context(),
            HealthRunInput {
                managed_mcp_id: remote.managed_mcp_id.clone(),
                mode: HealthCheckMode::Runtime,
                idempotency_key: "health_blocked_auth".to_string(),
            },
        )
        .await
        .unwrap();
    harness.service.runner_tick().await.unwrap();
    assert_eq!(
        harness
            .repository
            .latest_health_observation(&remote.managed_mcp_id)
            .await
            .unwrap()
            .unwrap()
            .result_code,
        HealthResultCode::BlockedAuth
    );
}

#[tokio::test]
async fn health_session_cancel_timeout_and_success_close_every_phase_without_residue() {
    for (phase, list_tools) in [
        (BlockingHealthPhase::Open, false),
        (BlockingHealthPhase::Initialize, false),
        (BlockingHealthPhase::ListTools, true),
    ] {
        let session = Arc::new(PhaseHealthSession::new(phase, false));
        let cancellation = CancellationToken::new();
        let running = {
            let session = session.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                run_bounded_health_session(
                    session.as_ref(),
                    list_tools,
                    Duration::from_secs(30),
                    cancellation,
                )
                .await
            })
        };
        session.entered.notified().await;
        cancellation.cancel();
        let result = running.await.unwrap().unwrap();
        assert_eq!(result.result_code, HealthResultCode::Cancelled);
        assert_eq!(result.detail_code, HealthDetailCode::Cancelled);
        assert_eq!(session.close_count.load(Ordering::SeqCst), 1);
        assert!(!session.resource_created.load(Ordering::SeqCst));
        assert!(!session.manager_entry.load(Ordering::SeqCst));
    }

    let timeout = PhaseHealthSession::new(BlockingHealthPhase::Initialize, false);
    let result = run_bounded_health_session(
        &timeout,
        false,
        Duration::from_millis(1),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.result_code, HealthResultCode::Timeout);
    assert_eq!(timeout.close_count.load(Ordering::SeqCst), 1);
    assert!(!timeout.resource_created.load(Ordering::SeqCst));
    assert!(!timeout.manager_entry.load(Ordering::SeqCst));

    let cleanup_failure = PhaseHealthSession::new(BlockingHealthPhase::None, true);
    let result = run_bounded_health_session(
        &cleanup_failure,
        true,
        Duration::from_secs(1),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.result_code, HealthResultCode::Unhealthy);
    assert_eq!(result.detail_code, HealthDetailCode::CleanupFailed);
    assert_eq!(cleanup_failure.close_count.load(Ordering::SeqCst), 1);

    let success = PhaseHealthSession::new(BlockingHealthPhase::None, false);
    let result = run_bounded_health_session(
        &success,
        true,
        Duration::from_secs(1),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.result_code, HealthResultCode::Healthy);
    assert_eq!(result.detail_code, HealthDetailCode::McpListToolsSucceeded);
    assert_eq!(success.close_count.load(Ordering::SeqCst), 1);
    assert!(!success.resource_created.load(Ordering::SeqCst));
    assert!(!success.manager_entry.load(Ordering::SeqCst));
}

#[tokio::test]
async fn enable_is_gated_revisioned_serialized_and_disable_is_always_safe() {
    let harness = Harness::new().await;
    harness.confirm(MANUAL, TrustTier::Local, "enable").await;
    harness.service.runner_tick().await.unwrap();
    let managed = harness
        .service
        .managed_list(&harness.context(), ManagedListInput::default())
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    assert_eq!(
        harness
            .service
            .set_default_enabled(
                &harness.context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed.managed_mcp_id.clone(),
                    enabled: true,
                    expected_revision: managed.revision,
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::HealthFailed
    );
    harness
        .service
        .health_run(
            &harness.context(),
            HealthRunInput {
                managed_mcp_id: managed.managed_mcp_id.clone(),
                mode: HealthCheckMode::Registration,
                idempotency_key: "registration_health".to_string(),
            },
        )
        .await
        .unwrap();
    harness.service.runner_tick().await.unwrap();
    let healthy = harness
        .repository
        .get_managed_inventory(&managed.managed_mcp_id)
        .await
        .unwrap();
    let enabled = harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.managed_mcp_id.clone(),
                enabled: true,
                expected_revision: healthy.managed.revision,
            },
        )
        .await
        .unwrap();
    assert!(enabled.default_enabled);
    assert_eq!(
        harness
            .service
            .set_default_enabled(
                &harness.context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed.managed_mcp_id.clone(),
                    enabled: false,
                    expected_revision: healthy.managed.revision,
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::RevisionConflict
    );

    let key = harness
        .repository
        .get_connection_projection(&managed.managed_mcp_id)
        .await
        .unwrap()
        .link_key;
    harness.sink.entries.lock().unwrap().remove(&key);
    let current = harness
        .repository
        .get_managed_inventory(&managed.managed_mcp_id)
        .await
        .unwrap();
    let disabled = harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.managed_mcp_id.clone(),
                enabled: false,
                expected_revision: current.managed.revision,
            },
        )
        .await
        .unwrap();
    assert!(!disabled.default_enabled);
}

#[tokio::test]
async fn projection_mutation_survives_orthogonal_revision_changes_and_serializes_races() {
    let harness = Harness::new().await;
    harness
        .confirm(MANUAL, TrustTier::Local, "mutation_race")
        .await;
    harness.service.runner_tick().await.unwrap();
    let managed = harness
        .service
        .managed_list(&harness.context(), ManagedListInput::default())
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    harness
        .service
        .health_run(
            &harness.context(),
            HealthRunInput {
                managed_mcp_id: managed.managed_mcp_id.clone(),
                mode: HealthCheckMode::Registration,
                idempotency_key: "mutation_health".to_string(),
            },
        )
        .await
        .unwrap();
    harness.service.runner_tick().await.unwrap();
    let healthy = harness
        .repository
        .get_managed_inventory(&managed.managed_mcp_id)
        .await
        .unwrap();
    let mutation = harness
        .repository
        .begin_projection_mutation(&managed.managed_mcp_id, healthy.managed.revision, true, 200)
        .await
        .unwrap();
    let projection = harness
        .repository
        .get_connection_projection(&managed.managed_mcp_id)
        .await
        .unwrap();
    harness
        .sink
        .set_enabled(&projection.link_key, true)
        .await
        .unwrap();
    harness
        .repository
        .mark_projection_config_committed(mutation.mutation_id, 201)
        .await
        .unwrap();
    let mut orthogonal_state = healthy.managed.state.clone();
    orthogonal_state.runtime = RuntimeState::Running;
    harness
        .repository
        .update_managed_state(
            &managed.managed_mcp_id,
            healthy.managed.revision,
            &orthogonal_state,
            202,
        )
        .await
        .unwrap();
    harness
        .repository
        .complete_projection_mutation(mutation.mutation_id, 203)
        .await
        .unwrap();
    let completed = harness
        .repository
        .get_managed_inventory(&managed.managed_mcp_id)
        .await
        .unwrap();
    assert!(completed.managed.state.default_enabled);
    assert_eq!(completed.managed.state.runtime, RuntimeState::Running);

    let disabled = harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.managed_mcp_id.clone(),
                enabled: false,
                expected_revision: completed.managed.revision,
            },
        )
        .await
        .unwrap();
    harness
        .sink
        .set_enabled(&projection.link_key, true)
        .await
        .unwrap();
    let race_revision = disabled.revision;
    let left_context = harness.context();
    let right_context = harness.context();
    let left = harness.service.set_default_enabled(
        &left_context,
        SetDefaultEnabledInput {
            managed_mcp_id: managed.managed_mcp_id.clone(),
            enabled: true,
            expected_revision: race_revision,
        },
    );
    let right = harness.service.set_default_enabled(
        &right_context,
        SetDefaultEnabledInput {
            managed_mcp_id: managed.managed_mcp_id.clone(),
            enabled: false,
            expected_revision: race_revision,
        },
    );
    let (left, right) = tokio::join!(left, right);
    assert!(left.is_ok() ^ right.is_ok());
    let final_inventory = harness
        .repository
        .get_managed_inventory(&managed.managed_mcp_id)
        .await
        .unwrap();
    let final_projection = harness
        .sink
        .get(&projection.link_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        final_inventory.managed.state.default_enabled,
        final_projection.entry.enabled
    );
}

#[tokio::test]
async fn projection_mutation_sink_failure_is_reconciled_without_restart() {
    let harness = Harness::new().await;
    harness
        .confirm(MANUAL, TrustTier::Local, "mutation_failure")
        .await;
    harness.service.runner_tick().await.unwrap();
    let managed = harness
        .service
        .managed_list(&harness.context(), ManagedListInput::default())
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    harness
        .service
        .health_run(
            &harness.context(),
            HealthRunInput {
                managed_mcp_id: managed.managed_mcp_id.clone(),
                mode: HealthCheckMode::Registration,
                idempotency_key: "failure_health".to_string(),
            },
        )
        .await
        .unwrap();
    harness.service.runner_tick().await.unwrap();
    let healthy = harness
        .repository
        .get_managed_inventory(&managed.managed_mcp_id)
        .await
        .unwrap();
    harness.sink.fail_set.store(true, Ordering::SeqCst);
    assert!(harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.managed_mcp_id.clone(),
                enabled: true,
                expected_revision: healthy.managed.revision,
            },
        )
        .await
        .is_err());
    let pool = raw_pool(&harness.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM projection_mutations ORDER BY mutation_id DESC LIMIT 1"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "recovery_required"
    );
    harness.sink.fail_set.store(false, Ordering::SeqCst);
    let enabled = harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.managed_mcp_id,
                enabled: true,
                expected_revision: healthy.managed.revision,
            },
        )
        .await
        .unwrap();
    assert!(enabled.default_enabled);
}

#[tokio::test]
async fn filtered_keyset_pagination_does_not_skip_late_matches() {
    let harness = Harness::new().await;
    for index in 0..5 {
        let source = REMOTE.replace(
            "com.example.knowledge-search",
            &format!("com.example.pagination-{index}"),
        );
        harness
            .confirm(&source, TrustTier::Official, &format!("page_{index}"))
            .await;
        harness.service.runner_tick().await.unwrap();
    }
    let all = harness
        .service
        .managed_list(
            &harness.context(),
            ManagedListInput {
                page_size: Some(10),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .items;
    for (index, item) in all.iter().rev().take(2).enumerate() {
        harness
            .service
            .health_run(
                &harness.context(),
                HealthRunInput {
                    managed_mcp_id: item.managed_mcp_id.clone(),
                    mode: HealthCheckMode::Registration,
                    idempotency_key: format!("page_health_{index}"),
                },
            )
            .await
            .unwrap();
        harness.service.runner_tick().await.unwrap();
    }
    let first = harness
        .service
        .managed_list(
            &harness.context(),
            ManagedListInput {
                page_size: Some(1),
                registration: Some(RegistrationState::Registered),
                health: Some(HealthState::Healthy),
                default_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    let second = harness
        .service
        .managed_list(
            &harness.context(),
            ManagedListInput {
                cursor: first.next_cursor.clone(),
                page_size: Some(1),
                registration: Some(RegistrationState::Registered),
                health: Some(HealthState::Healthy),
                default_enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_ne!(
        first.items[0].managed_mcp_id,
        second.items[0].managed_mcp_id
    );
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn schema_and_closed_journal_enums_reopen_forward_only() {
    let harness = Harness::new().await;
    let diagnostics = harness.repository.diagnostics().await.unwrap();
    for table in [
        "managed_mcps",
        "managed_versions",
        "connection_projections",
        "health_observations",
        "task_retry_attempts",
        "task_step_history",
        "projection_mutations",
    ] {
        assert!(diagnostics.tables.iter().any(|value| value == table));
    }
    for value in [
        CompensationStatus::Pending,
        CompensationStatus::Started,
        CompensationStatus::Committed,
    ] {
        let encoded = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<CompensationStatus>(&encoded).unwrap(),
            value
        );
    }
    assert!(serde_json::from_str::<CompensationStatus>("\"arbitrary\"").is_err());
    assert!(serde_json::from_str::<ProjectionMutationStatus>("\"arbitrary\"").is_err());
}

#[test]
fn agent_and_acp_extension_loading_only_returns_enabled_managed_projections() {
    let directory = TempDir::new().unwrap();
    let config_path = directory.path().join("config.yaml");
    let secrets_path = directory.path().join("secrets.yaml");
    std::fs::write(
        &config_path,
        r#"
extensions:
  managed_mcp_disabled_projection:
    enabled: false
    type: streamable_http
    name: managed-disabled
    description: disabled managed projection
    uri: https://disabled.example.invalid/mcp
    env_keys: []
    headers: {}
  managed_mcp_enabled_projection:
    enabled: true
    type: stdio
    name: managed-enabled
    description: enabled managed projection
    cmd: mcp-safe-executable
    args: ["--literal", "$(not-a-shell)"]
    env_keys: []
"#,
    )
    .unwrap();
    std::fs::write(&secrets_path, "").unwrap();
    let config = Config::new_with_file_secrets(&config_path, &secrets_path).unwrap();

    let loaded = get_enabled_extensions_with_config(&config);
    let names = loaded.iter().map(ExtensionConfig::name).collect::<Vec<_>>();
    assert!(names.iter().any(|name| name == "managed-enabled"));
    assert!(!names.iter().any(|name| name == "managed-disabled"));
}

#[test]
fn production_runner_health_and_acp_boundary_have_no_shell_or_repository_escape_hatch() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runner = std::fs::read_to_string(root.join("src/mcp_platform/task_runner.rs")).unwrap();
    let health = std::fs::read_to_string(root.join("src/mcp_platform/health.rs")).unwrap();
    let lifecycle = std::fs::read_to_string(root.join("src/mcp_platform/lifecycle.rs")).unwrap();
    let handler = std::fs::read_to_string(root.join("src/acp/server/mcp_platform.rs")).unwrap();
    let wire = std::fs::read_to_string(
        root.join("../goose-sdk-types/src/custom_requests/mcp_platform.rs"),
    )
    .unwrap();
    let wire = wire.split("#[cfg(test)]").next().unwrap();

    for source in [&runner, &health, &lifecycle] {
        for forbidden in [
            "std::process::Command",
            "tokio::process::Command",
            "Command::new(",
            "cmd.exe",
            "powershell.exe",
            "/bin/sh",
            "sh -c",
            "pre_hook",
            "post_hook",
        ] {
            assert!(!source.contains(forbidden), "found {forbidden}");
        }
    }
    for forbidden in [
        "sqlx::",
        "mcp_platform::repository::",
        "Config::",
        "std::process",
        "tokio::process",
    ] {
        assert!(!handler.contains(forbidden), "found {forbidden}");
    }
    for forbidden in [
        "serde_json::Value",
        "#[serde(flatten)]",
        "command:",
        "shell:",
        "path:",
        "credential_value",
    ] {
        assert!(!wire.contains(forbidden), "found {forbidden}");
    }
}

fn test_acp_server(directory: &TempDir) -> AcpServer {
    AcpServer::new(AcpServerFactoryConfig {
        builtins: Vec::new(),
        data_dir: directory.path().join("data"),
        config_dir: directory.path().join("config"),
        goose_platform: GoosePlatform::GooseCli,
        additional_source_roots: Vec::new(),
    })
}

#[tokio::test]
async fn server_shared_platform_service_is_lazy_canonical_and_shutdown_is_awaited() {
    let directory = TempDir::new().unwrap();
    let canonical_database = directory.path().join("data/mcp-platform/platform.db");
    let obsolete_database = directory.path().join("data/platform.db");
    let server = test_acp_server(&directory);
    let first = server.create_agent().await.unwrap();
    let second = server.create_agent().await.unwrap();

    for agent in [&first, &second] {
        let prompts = agent
            .dispatch_custom_request("_goose/unstable/config/prompts/list", serde_json::json!({}))
            .await
            .unwrap();
        assert!(prompts["prompts"].is_array());
    }
    assert!(!canonical_database.exists());
    assert!(!obsolete_database.exists());

    for agent in [&first, &second] {
        let value = agent
            .dispatch_custom_request(MCP_LIST_METHOD, serde_json::json!({"pageSize": 10}))
            .await
            .unwrap();
        let response: McpListResponse = serde_json::from_value(value).unwrap();
        assert!(matches!(
            response.outcome,
            McpPlatformOutcome::Success { .. }
        ));
    }
    assert!(canonical_database.exists());
    assert!(!obsolete_database.exists());
    drop(first);
    let value = second
        .dispatch_custom_request(MCP_LIST_METHOD, serde_json::json!({"pageSize": 10}))
        .await
        .unwrap();
    let response: McpListResponse = serde_json::from_value(value).unwrap();
    assert!(matches!(
        response.outcome,
        McpPlatformOutcome::Success { .. }
    ));

    tokio::time::timeout(Duration::from_secs(5), server.shutdown())
        .await
        .expect("server-owned worker shutdown must be awaitable");
}

#[tokio::test]
async fn platform_database_failure_is_lazy_and_non_mcp_custom_methods_remain_available() {
    let directory = TempDir::new().unwrap();
    let data_dir = directory.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("mcp-platform"), b"not a directory").unwrap();
    let server = test_acp_server(&directory);
    let first = server.create_agent().await.unwrap();
    let second = server.create_agent().await.unwrap();

    let prompts = first
        .dispatch_custom_request("_goose/unstable/config/prompts/list", serde_json::json!({}))
        .await
        .unwrap();
    assert!(prompts["prompts"].is_array());

    let value = first
        .dispatch_custom_request(MCP_LIST_METHOD, serde_json::json!({"pageSize": 10}))
        .await
        .unwrap();
    let response: McpListResponse = serde_json::from_value(value).unwrap();
    let McpPlatformOutcome::Error { error } = response.outcome else {
        panic!("broken platform DB must produce a typed MCP error");
    };
    assert_eq!(error.code, McpPlatformErrorCodeDto::RepositoryUnavailable);
    assert!(!error.correlation_id.is_empty());

    let prompts = second
        .dispatch_custom_request("_goose/unstable/config/prompts/list", serde_json::json!({}))
        .await
        .unwrap();
    assert!(prompts["prompts"].is_array());
    server.shutdown().await;
}

#[tokio::test]
async fn managed_config_namespace_is_user_read_only_and_platform_mutations_preserve_siblings() {
    let directory = TempDir::new().unwrap();
    let config_path = directory.path().join("config.yaml");
    let secrets_path = directory.path().join("secrets.yaml");
    std::fs::write(
        &config_path,
        r#"
extensions:
  broken_user_sibling:
    enabled: true
    type: stdio
    name: Broken User Sibling
    description: deliberately unparseable because cmd is absent
    args: []
"#,
    )
    .unwrap();
    std::fs::write(&secrets_path, "").unwrap();
    let config = Arc::new(Config::new_with_file_secrets(&config_path, &secrets_path).unwrap());
    let sink = ConfigProjectionSink::with_config(config);
    let key = "managed_mcp_3a_namespace";
    let entry = ExtensionEntry {
        enabled: false,
        config: ExtensionConfig::Stdio {
            name: "Managed Namespace Test".to_string(),
            description: "managed projection".to_string(),
            cmd: "mcp-safe-executable".to_string(),
            args: vec!["--literal".to_string(), "$(not-a-shell)".to_string()],
            envs: Default::default(),
            env_keys: vec!["OPAQUE_CREDENTIAL_NAME".to_string()],
            timeout: Some(5),
            cwd: None,
            bundled: None,
            available_tools: Vec::new(),
        },
    };

    assert!(try_set_extension_at_key(key, entry.clone()).is_err());
    assert!(try_set_extension_enabled(key, true).is_err());
    assert!(try_remove_extension(key).is_err());

    let before: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    let broken_before = before["extensions"]["broken_user_sibling"].clone();
    let created = sink.put_disabled(key, entry.config.clone()).await.unwrap();
    assert!(created.created);
    assert!(!created.entry.enabled);
    let replay = sink.put_disabled(key, entry.config.clone()).await.unwrap();
    assert!(!replay.created);

    let enabled = sink.set_enabled(key, true).await.unwrap();
    assert!(enabled.entry.enabled);
    let after_enable: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(
        after_enable["extensions"]["broken_user_sibling"],
        broken_before
    );
    assert_eq!(
        after_enable["extensions"][key]["args"][1].as_str(),
        Some("$(not-a-shell)")
    );

    assert!(sink.remove_owned(key, &enabled).await.unwrap());
    let after_remove: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(after_remove["extensions"].get(key).is_none());
    assert_eq!(
        after_remove["extensions"]["broken_user_sibling"],
        broken_before
    );
}
