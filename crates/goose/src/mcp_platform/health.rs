use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agents::extension_manager::ExtensionManager;
use crate::agents::ExtensionConfig;
use crate::config::paths::Paths;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::lifecycle::{
    HealthAdapterResult, HealthCheckAdapter, HealthCheckSession, HealthExecution,
    RegistrationEffect,
};
use super::manifest::HealthCheck;
use super::{HealthDetailCode, HealthResultCode};

#[derive(Debug, Default)]
pub struct ProductionHealthCheckAdapter;

#[async_trait]
impl HealthCheckAdapter for ProductionHealthCheckAdapter {
    fn adapter_id(&self) -> &'static str {
        "mcp_health"
    }
    fn adapter_version(&self) -> &'static str {
        "1"
    }

    async fn run(
        &self,
        execution: HealthExecution,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<HealthAdapterResult> {
        let started = Instant::now();
        let timeout_seconds = match execution.check {
            HealthCheck::McpInitialize { timeout_seconds }
            | HealthCheck::McpListTools { timeout_seconds }
            | HealthCheck::Http {
                timeout_seconds, ..
            } => timeout_seconds,
        };
        let result = match execution.check {
            HealthCheck::McpInitialize { .. } => {
                mcp_health_bounded(
                    execution.projection_config,
                    false,
                    Duration::from_secs(timeout_seconds),
                    cancellation,
                )
                .await?
            }
            HealthCheck::McpListTools { .. } => {
                mcp_health_bounded(
                    execution.projection_config,
                    true,
                    Duration::from_secs(timeout_seconds),
                    cancellation,
                )
                .await?
            }
            HealthCheck::Http { .. } => tokio::select! {
                _ = cancellation.cancelled() => cancelled_result(),
                result = tokio::time::timeout(Duration::from_secs(timeout_seconds), execute_health(execution)) => match result {
                    Ok(result) => result?,
                    Err(_) => timeout_result(),
                },
            },
        };
        Ok(HealthAdapterResult {
            latency_ms: elapsed_ms(started),
            ..result
        })
    }
}

async fn execute_health(execution: HealthExecution) -> McpPlatformResult<HealthAdapterResult> {
    match execution.check {
        HealthCheck::Http {
            path,
            expected_status,
            ..
        } => {
            http_health(
                execution.effect,
                execution.projection_config,
                &path,
                expected_status,
            )
            .await
        }
        HealthCheck::McpInitialize { .. } | HealthCheck::McpListTools { .. } => {
            Err(incompatible_error())
        }
    }
}

async fn http_health(
    effect: RegistrationEffect,
    config: ExtensionConfig,
    path: &str,
    expected_status: u16,
) -> McpPlatformResult<HealthAdapterResult> {
    let RegistrationEffect::RemoteHttp { endpoint, .. } = effect else {
        return Ok(incompatible());
    };
    if path.contains("..") || (!path.is_empty() && !path.starts_with('/')) {
        return Ok(incompatible());
    }
    let resolved = config
        .resolve(crate::config::Config::global())
        .await
        .map_err(|_| health_failed())?;
    let ExtensionConfig::StreamableHttp { headers, .. } = resolved else {
        return Ok(incompatible());
    };
    let base = url::Url::parse(&endpoint).map_err(|_| health_failed())?;
    let target = base.join(path).map_err(|_| health_failed())?;
    if target.scheme() != "https" || target.origin() != base.origin() {
        return Ok(incompatible());
    }
    let mut request = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| health_failed())?
        .get(target);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().await.map_err(|_| health_failed())?;
    let healthy = response.status().as_u16() == expected_status;
    Ok(HealthAdapterResult {
        result_code: if healthy {
            HealthResultCode::Healthy
        } else {
            HealthResultCode::Unhealthy
        },
        latency_ms: 0,
        capabilities_digest: None,
        tools_digest: None,
        detail_code: if healthy {
            HealthDetailCode::ExpectedStatus
        } else {
            HealthDetailCode::UnexpectedStatus
        },
    })
}

struct ExtensionManagerHealthSession {
    manager: Arc<ExtensionManager>,
    config: ExtensionConfig,
    extension_name: String,
    operation: Arc<HealthOpenOperation>,
}

#[derive(Default)]
struct HealthOpenOperation {
    started: AtomicBool,
    succeeded: AtomicBool,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl HealthOpenOperation {
    async fn wait(&self) -> bool {
        let mut task = self.task.lock().await;
        if let Some(running) = task.as_mut() {
            let completed = running.await.is_ok();
            *task = None;
            completed
        } else {
            true
        }
    }

    async fn abort_and_wait(&self) {
        let task = self.task.lock().await.take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }
}

#[async_trait]
impl HealthCheckSession for ExtensionManagerHealthSession {
    async fn open(&self) -> McpPlatformResult<()> {
        if self.operation.started.swap(true, Ordering::SeqCst) {
            return Err(health_failed());
        }
        let manager = self.manager.clone();
        let config = self.config.clone();
        let operation = self.operation.clone();
        let task = tokio::spawn(async move {
            let succeeded = manager
                .add_extension(config, None, None, Some("mcp-platform-health"))
                .await
                .is_ok();
            operation.succeeded.store(succeeded, Ordering::SeqCst);
        });
        *self.operation.task.lock().await = Some(task);
        Ok(())
    }

    async fn initialize(&self) -> McpPlatformResult<()> {
        if self.operation.wait().await && self.operation.succeeded.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(health_failed())
        }
    }

    async fn list_tools(&self) -> McpPlatformResult<Option<String>> {
        let tools = self
            .manager
            .get_prefixed_tools("mcp-platform-health", Some(self.extension_name.clone()))
            .await
            .map_err(|_| health_failed())?;
        serde_json::to_vec(&tools)
            .ok()
            .map(|bytes| hex_digest(&bytes))
            .map(Some)
            .ok_or_else(health_failed)
    }

    async fn close(&self) -> McpPlatformResult<()> {
        self.operation.abort_and_wait().await;
        self.manager
            .remove_extension(&self.extension_name)
            .await
            .map_err(|_| health_failed())
    }
}

async fn mcp_health_bounded(
    config: ExtensionConfig,
    list_tools: bool,
    timeout: Duration,
    cancellation: CancellationToken,
) -> McpPlatformResult<HealthAdapterResult> {
    let extension_name = config.key();
    let session = ExtensionManagerHealthSession {
        manager: Arc::new(ExtensionManager::new_without_provider(Paths::data_dir())),
        config,
        extension_name,
        operation: Arc::new(HealthOpenOperation::default()),
    };
    run_bounded_health_session(&session, list_tools, timeout, cancellation).await
}

pub async fn run_bounded_health_session(
    session: &dyn HealthCheckSession,
    list_tools: bool,
    timeout: Duration,
    cancellation: CancellationToken,
) -> McpPlatformResult<HealthAdapterResult> {
    let deadline = tokio::time::Instant::now() + timeout;
    let result = match bounded_phase(session.open(), deadline, &cancellation).await {
        PhaseResult::Interrupted(result) => result,
        PhaseResult::Finished(Err(_)) => initialize_failed(),
        PhaseResult::Finished(Ok(())) => {
            match bounded_phase(session.initialize(), deadline, &cancellation).await {
                PhaseResult::Interrupted(result) => result,
                PhaseResult::Finished(Err(_)) => initialize_failed(),
                PhaseResult::Finished(Ok(())) if !list_tools => initialized(),
                PhaseResult::Finished(Ok(())) => {
                    match bounded_phase(session.list_tools(), deadline, &cancellation).await {
                        PhaseResult::Interrupted(result) => result,
                        PhaseResult::Finished(Err(_)) => list_tools_failed(),
                        PhaseResult::Finished(Ok(tools_digest)) => {
                            list_tools_succeeded(tools_digest)
                        }
                    }
                }
            }
        }
    };
    if session.close().await.is_err() {
        return Ok(HealthAdapterResult {
            result_code: HealthResultCode::Unhealthy,
            latency_ms: 0,
            capabilities_digest: None,
            tools_digest: None,
            detail_code: HealthDetailCode::CleanupFailed,
        });
    }
    Ok(result)
}

enum PhaseResult<T> {
    Finished(McpPlatformResult<T>),
    Interrupted(HealthAdapterResult),
}

async fn bounded_phase<T>(
    phase: impl std::future::Future<Output = McpPlatformResult<T>>,
    deadline: tokio::time::Instant,
    cancellation: &CancellationToken,
) -> PhaseResult<T> {
    tokio::select! {
        result = phase => PhaseResult::Finished(result),
        _ = cancellation.cancelled() => PhaseResult::Interrupted(cancelled_result()),
        _ = tokio::time::sleep_until(deadline) => PhaseResult::Interrupted(timeout_result()),
    }
}

fn initialized() -> HealthAdapterResult {
    health_result(HealthDetailCode::McpInitializeSucceeded, None)
}

fn initialize_failed() -> HealthAdapterResult {
    unhealthy_result(HealthDetailCode::McpInitializeFailed)
}

fn list_tools_succeeded(tools_digest: Option<String>) -> HealthAdapterResult {
    health_result(HealthDetailCode::McpListToolsSucceeded, tools_digest)
}

fn list_tools_failed() -> HealthAdapterResult {
    unhealthy_result(HealthDetailCode::McpListToolsFailed)
}

fn health_result(
    detail_code: HealthDetailCode,
    tools_digest: Option<String>,
) -> HealthAdapterResult {
    HealthAdapterResult {
        result_code: HealthResultCode::Healthy,
        latency_ms: 0,
        capabilities_digest: None,
        tools_digest,
        detail_code,
    }
}

fn unhealthy_result(detail_code: HealthDetailCode) -> HealthAdapterResult {
    HealthAdapterResult {
        result_code: HealthResultCode::Unhealthy,
        latency_ms: 0,
        capabilities_digest: None,
        tools_digest: None,
        detail_code,
    }
}

fn timeout_result() -> HealthAdapterResult {
    HealthAdapterResult {
        result_code: HealthResultCode::Timeout,
        latency_ms: 0,
        capabilities_digest: None,
        tools_digest: None,
        detail_code: HealthDetailCode::Timeout,
    }
}

fn cancelled_result() -> HealthAdapterResult {
    HealthAdapterResult {
        result_code: HealthResultCode::Cancelled,
        latency_ms: 0,
        capabilities_digest: None,
        tools_digest: None,
        detail_code: HealthDetailCode::Cancelled,
    }
}

fn incompatible() -> HealthAdapterResult {
    HealthAdapterResult {
        result_code: HealthResultCode::Incompatible,
        latency_ms: 0,
        capabilities_digest: None,
        tools_digest: None,
        detail_code: HealthDetailCode::IncompatibleHealthContract,
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn elapsed_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

const fn health_failed() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::HealthFailed,
        "MCP health check failed",
    )
}

const fn incompatible_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidTransition,
        "MCP health check was cancelled",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum BlockPhase {
        Open,
        Initialize,
        ListTools,
        None,
    }

    struct BlockingSession {
        block_phase: BlockPhase,
        entered: Notify,
        release: Notify,
        resource_created: AtomicBool,
        manager_entry: AtomicBool,
        close_count: AtomicUsize,
        close_fails: bool,
    }

    impl BlockingSession {
        async fn block(&self, phase: BlockPhase) {
            if self.block_phase == phase {
                self.entered.notify_one();
                self.release.notified().await;
            }
        }
    }

    #[async_trait]
    impl HealthCheckSession for BlockingSession {
        async fn open(&self) -> McpPlatformResult<()> {
            self.resource_created.store(true, Ordering::SeqCst);
            self.block(BlockPhase::Open).await;
            self.manager_entry.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn initialize(&self) -> McpPlatformResult<()> {
            self.block(BlockPhase::Initialize).await;
            Ok(())
        }

        async fn list_tools(&self) -> McpPlatformResult<Option<String>> {
            self.block(BlockPhase::ListTools).await;
            Ok(Some("a".repeat(64)))
        }

        async fn close(&self) -> McpPlatformResult<()> {
            self.close_count.fetch_add(1, Ordering::SeqCst);
            self.manager_entry.store(false, Ordering::SeqCst);
            self.resource_created.store(false, Ordering::SeqCst);
            if self.close_fails {
                Err(health_failed())
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn cancellation_closes_connect_initialize_and_list_tools_sessions() {
        for (phase, list_tools) in [
            (BlockPhase::Open, false),
            (BlockPhase::Initialize, false),
            (BlockPhase::ListTools, true),
        ] {
            let session = Arc::new(BlockingSession {
                block_phase: phase,
                entered: Notify::new(),
                release: Notify::new(),
                resource_created: AtomicBool::new(false),
                manager_entry: AtomicBool::new(false),
                close_count: AtomicUsize::new(0),
                close_fails: false,
            });
            let cancellation = CancellationToken::new();
            let running = {
                let session = session.clone();
                let cancellation = cancellation.clone();
                tokio::spawn(async move {
                    run_bounded_health_session(
                        session.as_ref(),
                        list_tools,
                        Duration::from_secs(60),
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
    }

    #[tokio::test]
    async fn timeout_closes_session_and_cleanup_failure_cannot_report_healthy() {
        let timed_out = BlockingSession {
            block_phase: BlockPhase::Initialize,
            entered: Notify::new(),
            release: Notify::new(),
            resource_created: AtomicBool::new(false),
            manager_entry: AtomicBool::new(false),
            close_count: AtomicUsize::new(0),
            close_fails: false,
        };
        let result = run_bounded_health_session(
            &timed_out,
            false,
            Duration::from_millis(1),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.result_code, HealthResultCode::Timeout);
        assert_eq!(timed_out.close_count.load(Ordering::SeqCst), 1);
        assert!(!timed_out.resource_created.load(Ordering::SeqCst));
        assert!(!timed_out.manager_entry.load(Ordering::SeqCst));

        let cleanup_failed = BlockingSession {
            block_phase: BlockPhase::None,
            entered: Notify::new(),
            release: Notify::new(),
            resource_created: AtomicBool::new(false),
            manager_entry: AtomicBool::new(false),
            close_count: AtomicUsize::new(0),
            close_fails: true,
        };
        let result = run_bounded_health_session(
            &cleanup_failed,
            true,
            Duration::from_secs(1),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.result_code, HealthResultCode::Unhealthy);
        assert_eq!(result.detail_code, HealthDetailCode::CleanupFailed);
        assert_eq!(cleanup_failed.close_count.load(Ordering::SeqCst), 1);
        assert!(!cleanup_failed.resource_created.load(Ordering::SeqCst));
        assert!(!cleanup_failed.manager_entry.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn successful_health_session_is_also_closed_once() {
        let session = BlockingSession {
            block_phase: BlockPhase::None,
            entered: Notify::new(),
            release: Notify::new(),
            resource_created: AtomicBool::new(false),
            manager_entry: AtomicBool::new(false),
            close_count: AtomicUsize::new(0),
            close_fails: false,
        };
        let result = run_bounded_health_session(
            &session,
            true,
            Duration::from_secs(1),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.result_code, HealthResultCode::Healthy);
        assert_eq!(result.detail_code, HealthDetailCode::McpListToolsSucceeded);
        assert_eq!(session.close_count.load(Ordering::SeqCst), 1);
        assert!(!session.resource_created.load(Ordering::SeqCst));
        assert!(!session.manager_entry.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn immediate_cancel_awaits_an_open_task_that_was_never_polled() {
        let operation = HealthOpenOperation::default();
        let polled = Arc::new(AtomicBool::new(false));
        let task = {
            let polled = polled.clone();
            tokio::spawn(async move {
                polled.store(true, Ordering::SeqCst);
                std::future::pending::<()>().await;
            })
        };
        *operation.task.lock().await = Some(task);
        tokio::time::timeout(Duration::from_secs(1), operation.abort_and_wait())
            .await
            .unwrap();
        assert!(!polled.load(Ordering::SeqCst));
        assert!(operation.task.lock().await.is_none());
        tokio::time::timeout(Duration::from_secs(1), operation.abort_and_wait())
            .await
            .unwrap();
    }
}
