//! Trusted local runtime registration for V43 fenced projection sinks.
//!
//! V43a intentionally does not wire TaskRunner recovery, ACP, managed
//! distribution, or MCP execution. It only gives a reviewed local runtime a
//! way to register one durable-receipt sink and gives a future V43b executor a
//! capability lookup. Missing registration is always unavailable. A remote
//! sink is rejected because receipt signature verification is not implemented.
//!
//! ```compile_fail
//! use lumina::mcp_platform::runtime_fenced_sink_registry::RuntimeFencedSinkBootstrapAuthority;
//!
//! let _forged = RuntimeFencedSinkBootstrapAuthority { _sealed: () };
//! ```

use std::sync::{Arc, RwLock};

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::fenced_projection_sink::{
    trusted_runtime_fenced_sink, FencedProjectionSink, FencedProjectionSinkCapability,
    FencedReceiptIssuer, FencedSinkIssueBinding, RuntimeCallGate, TrustedFencedProjectionSink,
};

/// Opaque bootstrap authority for the process-local runtime composition root.
/// It has no public constructor, and this crate-private module is not exposed
/// through the SDK, plugin, UI, ACP, or request surfaces.
pub(super) struct RuntimeFencedSinkBootstrapAuthority {
    _sealed: (),
}

/// The only local composition-root minting site. V43a has no wired executor,
/// so this remains private to the registry and is used only by its local tests.
struct RuntimeFencedSinkCompositionRoot;

impl RuntimeFencedSinkCompositionRoot {
    fn bootstrap_authority() -> RuntimeFencedSinkBootstrapAuthority {
        RuntimeFencedSinkBootstrapAuthority { _sealed: () }
    }
}

struct RegisteredFencedSink {
    binding: FencedSinkIssueBinding,
    capability: TrustedFencedProjectionSink<'static>,
}

/// Process-local registry for exactly one trusted local fenced sink.
///
/// Registration is intentionally write-once. V43 persists the full authority
/// key identity, so a rotated key cannot match an earlier durable receipt.
pub(crate) struct RuntimeFencedSinkRegistry {
    runtime_gate: Arc<RuntimeCallGate>,
    registered: RwLock<Option<RegisteredFencedSink>>,
}

impl Default for RuntimeFencedSinkRegistry {
    fn default() -> Self {
        Self {
            runtime_gate: Arc::new(RuntimeCallGate::new()),
            registered: RwLock::new(None),
        }
    }
}

impl RuntimeFencedSinkRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Registers one local sink. The identity and version are read from the
    /// concrete adapter; callers cannot supply display names as authority.
    pub(crate) fn register_local(
        &self,
        _bootstrap: &RuntimeFencedSinkBootstrapAuthority,
        sink: Arc<dyn FencedProjectionSink>,
        authority_id: String,
        authority_key_epoch: u64,
        authority_key_fingerprint: String,
    ) -> McpPlatformResult<FencedReceiptIssuer> {
        if !self.runtime_gate.is_open() {
            return Err(registration_error());
        }

        let (capability, issuer) = trusted_runtime_fenced_sink(
            _bootstrap,
            sink,
            authority_id,
            authority_key_epoch,
            authority_key_fingerprint,
            self.runtime_gate.clone(),
        )?;
        let binding = FencedSinkIssueBinding::from_trusted(&capability)?;
        let mut registered = self.registered.write().map_err(|_| registration_error())?;
        if registered.is_some() {
            return Err(registration_error());
        }
        *registered = Some(RegisteredFencedSink {
            binding,
            capability,
        });
        Ok(issuer)
    }

    /// Remote receipt verification is deliberately unavailable in V43a.
    /// No remote sink can become trusted based on a trait implementation,
    /// display name, or asserted authority string.
    pub(crate) fn register_remote(
        &self,
        _bootstrap: &RuntimeFencedSinkBootstrapAuthority,
        _sink: Arc<dyn FencedProjectionSink>,
        _authority_id: String,
        _authority_key_epoch: u64,
        _authority_key_fingerprint: String,
    ) -> McpPlatformResult<FencedReceiptIssuer> {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::NotImplementedForPhase,
            "remote fenced projection sinks require receipt signature verification",
        ))
    }

    /// Looks up only an exact V42 persisted sink binding. An identity, version,
    /// authority, liveness, or registry-lock mismatch returns `Unavailable`;
    /// callers get no fallback trait object and must remain fail-closed.
    pub(crate) fn lookup_persisted_binding(
        &self,
        sink_identity: &str,
        sink_version: &str,
        authority_id: &str,
        authority_key_epoch: u64,
        authority_key_fingerprint: &str,
    ) -> FencedProjectionSinkCapability<'static> {
        if !self.runtime_gate.is_open() {
            return FencedProjectionSinkCapability::Unavailable;
        }
        let Ok(registered) = self.registered.read() else {
            return FencedProjectionSinkCapability::Unavailable;
        };
        let Some(registered) = registered.as_ref() else {
            return FencedProjectionSinkCapability::Unavailable;
        };
        if !registered.binding.exactly_matches(
            sink_identity,
            sink_version,
            authority_id,
            authority_key_epoch,
            authority_key_fingerprint,
        ) {
            return FencedProjectionSinkCapability::Unavailable;
        }
        FencedProjectionSinkCapability::Available(registered.capability.clone())
    }

    /// New calls are rejected immediately. Calls that already acquired a
    /// lease may finish, and this method waits for those leases to drain.
    pub(crate) fn close_and_drain(&self) {
        self.runtime_gate.close();
        self.runtime_gate.wait_for_drain();
    }
}

impl Drop for RuntimeFencedSinkRegistry {
    fn drop(&mut self) {
        self.runtime_gate.close();
        if let Ok(registered) = self.registered.get_mut() {
            *registered = None;
        }
    }
}

fn registration_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "fenced projection sink runtime registration failed",
    )
}

#[cfg(all(test, windows))]
mod tests {
    use std::{
        sync::{mpsc, Arc, Mutex},
        time::Duration,
    };

    use async_trait::async_trait;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
    use sqlx::{Connection, Executor, SqliteConnection};

    use super::*;
    use crate::mcp_platform::fenced_projection_sink::{
        FencedEffectCommand, FencedEffectExecution, FencedEffectReceipt,
        FencedProjectionEffectExecutor, FencedProjectionEffectRequest,
    };
    use crate::mcp_platform::repository::{InMemoryIntegritySigner, SqliteMcpPlatformRepository};

    struct LocalSink {
        identity: &'static str,
        version: &'static str,
        calls: Mutex<u64>,
    }

    impl LocalSink {
        fn new(identity: &'static str, version: &'static str) -> Self {
            Self {
                identity,
                version,
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> u64 {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl FencedProjectionSink for LocalSink {
        fn adapter_id(&self) -> &'static str {
            self.identity
        }

        fn adapter_version(&self) -> &'static str {
            self.version
        }

        async fn apply_conditional(
            &self,
            _: &FencedEffectCommand,
            _: &FencedEffectExecution,
        ) -> McpPlatformResult<FencedEffectReceipt> {
            *self.calls.lock().unwrap() += 1;
            Err(registration_error())
        }

        async fn lookup_receipt(
            &self,
            _: &FencedEffectCommand,
            _: &FencedEffectExecution,
        ) -> McpPlatformResult<Option<FencedEffectReceipt>> {
            *self.calls.lock().unwrap() += 1;
            Err(registration_error())
        }
    }

    struct BlockingReceiptSink {
        issuer: Mutex<Option<FencedReceiptIssuer>>,
        apply_started: mpsc::SyncSender<()>,
        release_apply: Mutex<mpsc::Receiver<()>>,
        calls: Mutex<u64>,
    }

    impl BlockingReceiptSink {
        fn new(apply_started: mpsc::SyncSender<()>, release_apply: mpsc::Receiver<()>) -> Self {
            Self {
                issuer: Mutex::new(None),
                apply_started,
                release_apply: Mutex::new(release_apply),
                calls: Mutex::new(0),
            }
        }

        fn install_issuer(&self, issuer: FencedReceiptIssuer) {
            *self.issuer.lock().unwrap() = Some(issuer);
        }

        fn calls(&self) -> u64 {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl FencedProjectionSink for BlockingReceiptSink {
        fn adapter_id(&self) -> &'static str {
            "local.blocking.sink"
        }

        fn adapter_version(&self) -> &'static str {
            "1"
        }

        async fn apply_conditional(
            &self,
            command: &FencedEffectCommand,
            execution: &FencedEffectExecution,
        ) -> McpPlatformResult<FencedEffectReceipt> {
            *self.calls.lock().unwrap() += 1;
            self.apply_started.send(()).unwrap();
            self.release_apply.lock().unwrap().recv().unwrap();
            self.issuer.lock().unwrap().as_ref().unwrap().issue(
                command,
                execution,
                uuid::Uuid::new_v4().to_string(),
            )
        }

        async fn lookup_receipt(
            &self,
            _: &FencedEffectCommand,
            _: &FencedEffectExecution,
        ) -> McpPlatformResult<Option<FencedEffectReceipt>> {
            *self.calls.lock().unwrap() += 1;
            Ok(None)
        }
    }

    async fn repository() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        SqliteMcpPlatformRepository,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime-fenced-sink-registry.db");
        let repository = SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &path,
            InMemoryIntegritySigner::new_for_testing([0x43; 32]),
        )
        .await
        .unwrap();
        (directory, path, repository)
    }

    fn request() -> FencedProjectionEffectRequest {
        FencedProjectionEffectRequest::new(uuid::Uuid::new_v4().to_string(), "a".repeat(64))
            .unwrap()
    }

    fn available(
        registry: &RuntimeFencedSinkRegistry,
        identity: &str,
        version: &str,
        authority: &str,
    ) -> bool {
        matches!(
            registry.lookup_persisted_binding(identity, version, authority, 1, &"a".repeat(64)),
            FencedProjectionSinkCapability::Available(_)
        )
    }

    #[test]
    fn no_registration_is_unavailable() {
        let registry = RuntimeFencedSinkRegistry::new();

        assert!(!available(
            &registry,
            "local.test.sink",
            "1",
            "local-authority"
        ));
    }

    #[test]
    fn valid_local_registration_requires_exact_persisted_binding() {
        let registry = RuntimeFencedSinkRegistry::new();
        let sink = Arc::new(LocalSink::new("local.test.sink", "1"));

        registry
            .register_local(
                &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                sink.clone(),
                "local-authority".to_string(),
                1,
                "a".repeat(64),
            )
            .unwrap();

        assert!(available(
            &registry,
            "local.test.sink",
            "1",
            "local-authority"
        ));
        assert!(!available(
            &registry,
            "other.test.sink",
            "1",
            "local-authority"
        ));
        assert!(!available(
            &registry,
            "local.test.sink",
            "2",
            "local-authority"
        ));
        assert!(!available(
            &registry,
            "local.test.sink",
            "1",
            "other-authority"
        ));
        assert_eq!(sink.calls(), 0);
    }

    #[test]
    fn duplicate_replacement_and_authority_rotation_are_rejected() {
        let registry = RuntimeFencedSinkRegistry::new();
        let first = Arc::new(LocalSink::new("local.test.sink", "1"));
        let replacement = Arc::new(LocalSink::new("local.test.sink", "2"));

        registry
            .register_local(
                &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                first.clone(),
                "local-authority".to_string(),
                1,
                "a".repeat(64),
            )
            .unwrap();
        assert!(registry
            .register_local(
                &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                replacement.clone(),
                "rotated-authority".to_string(),
                1,
                "b".repeat(64),
            )
            .is_err());
        assert!(registry
            .register_local(
                &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                replacement.clone(),
                "local-authority".to_string(),
                2,
                "c".repeat(64),
            )
            .is_err());
        assert!(available(
            &registry,
            "local.test.sink",
            "1",
            "local-authority"
        ));
        assert_eq!(first.calls(), 0);
        assert_eq!(replacement.calls(), 0);
    }

    #[test]
    fn close_rejects_lookup_and_later_registration() {
        let registry = RuntimeFencedSinkRegistry::new();
        let sink = Arc::new(LocalSink::new("local.test.sink", "1"));
        registry
            .register_local(
                &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                sink,
                "local-authority".to_string(),
                1,
                "a".repeat(64),
            )
            .unwrap();

        registry.close_and_drain();

        assert!(!available(
            &registry,
            "local.test.sink",
            "1",
            "local-authority"
        ));
        assert!(registry
            .register_local(
                &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                Arc::new(LocalSink::new("other.test.sink", "1")),
                "other-authority".to_string(),
                1,
                "b".repeat(64),
            )
            .is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_and_drain_rejects_a_second_production_execute_while_an_external_sink_lease_is_held(
    ) {
        let (_directory, _path, repository) = repository().await;
        let guard = Arc::new(repository.acquire_projection_effect_fence().await.unwrap());
        let registry = Arc::new(RuntimeFencedSinkRegistry::new());
        let (apply_started, apply_started_receiver) = mpsc::sync_channel(0);
        let (release_apply, release_apply_receiver) = mpsc::sync_channel(0);
        let sink = Arc::new(BlockingReceiptSink::new(
            apply_started,
            release_apply_receiver,
        ));
        let issuer = registry
            .register_local(
                &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                sink.clone(),
                "local-authority".to_string(),
                1,
                "a".repeat(64),
            )
            .unwrap();
        sink.install_issuer(issuer);

        let FencedProjectionSinkCapability::Available(capability) = registry
            .lookup_persisted_binding(
                "local.blocking.sink",
                "1",
                "local-authority",
                1,
                &"a".repeat(64),
            )
        else {
            panic!("registered runtime sink must be available");
        };
        let first_capability = capability.clone();
        let executor = FencedProjectionEffectExecutor::new(repository.clone());
        let first_execute = tokio::spawn({
            let executor = executor.clone();
            let guard = guard.clone();
            async move {
                executor
                    .execute(&guard, first_capability, request(), 1)
                    .await
            }
        });
        tokio::task::spawn_blocking(move || {
            apply_started_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("first production execute must acquire the external sink lease")
        })
        .await
        .unwrap();
        assert_eq!(sink.calls(), 1);

        let close = tokio::task::spawn_blocking({
            let registry = registry.clone();
            move || registry.close_and_drain()
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.runtime_gate.is_open() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("registry close_and_drain must close its production gate before draining");

        let error = executor
            .execute(&guard, capability, request(), 1)
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::NotImplementedForPhase);
        assert_eq!(
            error.message(),
            "fenced projection sink runtime registration is unavailable"
        );
        assert_eq!(sink.calls(), 1, "rejected execute must not invoke the sink");
        assert!(
            !close.is_finished(),
            "registry close_and_drain must wait for the active external sink lease"
        );

        release_apply.send(()).unwrap();
        first_execute.await.unwrap().unwrap();
        close.await.unwrap();
        assert_eq!(sink.calls(), 1);
        repository.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_and_drain_returns_while_a_production_execute_is_blocked_before_its_sink_lease() {
        let (_directory, path, repository) = repository().await;
        let guard = Arc::new(repository.acquire_projection_effect_fence().await.unwrap());
        let registry = Arc::new(RuntimeFencedSinkRegistry::new());
        let sink = Arc::new(LocalSink::new("local.database.sink", "1"));
        registry
            .register_local(
                &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                sink.clone(),
                "local-authority".to_string(),
                1,
                "a".repeat(64),
            )
            .unwrap();
        let capability = registry.lookup_persisted_binding(
            "local.database.sink",
            "1",
            "local-authority",
            1,
            &"a".repeat(64),
        );
        let (issue_entered, issue_entered_receiver) = mpsc::sync_channel(1);
        let executor = FencedProjectionEffectExecutor::new(repository.clone())
            .with_before_issue_bound_grant(Arc::new(move || {
                issue_entered.send(()).unwrap();
            }));
        let write_lock_options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        let mut write_lock = SqliteConnection::connect_with(&write_lock_options)
            .await
            .unwrap();
        write_lock.execute("BEGIN IMMEDIATE").await.unwrap();

        let execute = tokio::spawn({
            let guard = guard.clone();
            async move {
                executor
                    .execute_with_capability(&guard, capability, request(), 1)
                    .await
            }
        });
        tokio::task::spawn_blocking(move || {
            issue_entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("execute must reach its production database await")
        })
        .await
        .unwrap();
        tokio::task::yield_now().await;

        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::task::spawn_blocking({
                let registry = registry.clone();
                move || registry.close_and_drain()
            }),
        )
        .await
        .expect("registry close_and_drain must not wait for pre-lease database work")
        .unwrap();
        assert_eq!(sink.calls(), 0);
        assert!(
            !execute.is_finished(),
            "the production execute must still be blocked on the SQLite write lock"
        );

        write_lock.execute("ROLLBACK").await.unwrap();
        assert_eq!(
            execute.await.unwrap().unwrap_err().code(),
            McpPlatformErrorCode::NotImplementedForPhase
        );
        assert_eq!(sink.calls(), 0);
        repository.close().await;
    }

    #[test]
    fn remote_and_old_runtime_capabilities_fail_closed_without_side_effects() {
        let (capability, sink) = {
            let registry = RuntimeFencedSinkRegistry::new();
            let sink = Arc::new(LocalSink::new("local.test.sink", "1"));
            assert!(registry
                .register_remote(
                    &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                    sink.clone(),
                    "remote-authority".to_string(),
                    1,
                    "a".repeat(64),
                )
                .is_err());
            registry
                .register_local(
                    &RuntimeFencedSinkCompositionRoot::bootstrap_authority(),
                    sink.clone(),
                    "local-authority".to_string(),
                    1,
                    "a".repeat(64),
                )
                .unwrap();
            assert_eq!(sink.calls(), 0);
            (
                registry.lookup_persisted_binding(
                    "local.test.sink",
                    "1",
                    "local-authority",
                    1,
                    &"a".repeat(64),
                ),
                sink,
            )
        };

        let FencedProjectionSinkCapability::Available(capability) = capability else {
            panic!("registered sink capability must be present before registry drop");
        };
        assert!(capability.ensure_live().is_err());
        assert_eq!(sink.calls(), 0);

        let restarted = RuntimeFencedSinkRegistry::new();
        assert!(!available(
            &restarted,
            "local.test.sink",
            "1",
            "local-authority"
        ));
    }
}
