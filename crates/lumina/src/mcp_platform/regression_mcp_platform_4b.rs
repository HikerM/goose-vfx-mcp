use crate as lumina;
use crate::mcp_platform::task_runner::TaskRunner;
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lumina::agents::extension::Envs;
use lumina::agents::extension_manager::ExtensionManager;
use lumina::agents::ExtensionConfig;
use lumina::config::extensions::{get_enabled_extensions_with_config, ExtensionEntry};
use lumina::config::Config;
use lumina::mcp_platform::manifest::HealthCheck;
use lumina::mcp_platform::{
    parse_manifest, AuthRequirement, AuthRequirementResolver, Clock, CompensationDescriptor,
    ConfigProjectionSink, ConnectionProjection, CoreManagedRemoteHttpNetworkPolicy,
    CoreTransportProjectionAdapter, CreateTask, EmptyHostIntegrationAdapter, HealthAdapterResult,
    HealthCheckAdapter, HealthCheckMode, HealthDetailCode, HealthExecution, HealthResultCode,
    HealthRunInput, IdGenerator, InMemoryIntegritySigner, InstallConfirmInput, InstallationPlan,
    LifecyclePorts, ManagedListInput, ManagedRemoteHttpClient, ManagedRemoteResolver, Manifest,
    ManifestProof, ManifestRecord, ManualConnectionInput, ManualHttpAuth, ManualPlanCreateInput,
    McpPlatformError, McpPlatformErrorCode, McpPlatformResult, McpPlatformService,
    McpPlatformServiceOptions, PlanCreateInput, PlanIntent, ProductionHealthCheckAdapter,
    ProfileApplicationMarker, ProfileApplyPlanInput, ProfileCreateInput, ProjectionMutationStatus,
    ProjectionSink, ProjectionSnapshot, PutOwnedProjection, RedactedErrorCode, RegistrationEffect,
    RegistrationEffectAdapter, RegistrationEffectEvidence, RemoteHttpNetworkPolicy,
    RemoveOwnedProjection, RequestContext, RollbackEvidence, RollbackStatus,
    SetDefaultEnabledInput, SqliteMcpPlatformRepository, StepTransition, TaskRecord, TaskStatus,
    TaskStepStatus, TaskTransition, TransportProjectionAdapter, TrustTier, UserDecision,
};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio_util::sync::CancellationToken;

const REMOTE: &str =
    include_str!("../../../../documentation/static/schemas/examples/remote-http.json");

fn integrity_signer() -> Arc<dyn lumina::mcp_platform::IntegritySigner> {
    InMemoryIntegritySigner::new_for_testing([0x4b; 32])
}

fn test_context(label: &str) -> RequestContext {
    RequestContext::local_authenticated_client(label.to_string())
}

struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct TestIds(AtomicU64);

impl IdGenerator for TestIds {
    fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}_{:04}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

#[derive(Default)]
struct RecordingGate {
    syntax: AtomicUsize,
    plans: AtomicUsize,
    connections: AtomicUsize,
}

#[async_trait]
impl RemoteHttpNetworkPolicy for RecordingGate {
    fn validate_endpoint(&self, endpoint: &str) -> McpPlatformResult<()> {
        self.syntax.fetch_add(1, Ordering::SeqCst);
        if endpoint.starts_with("https://") {
            Ok(())
        } else {
            Err(McpPlatformError::new(
                McpPlatformErrorCode::UnsafeUrl,
                "test gate rejected endpoint",
            ))
        }
    }

    async fn validate_for_plan(
        &self,
        endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<()> {
        self.plans.fetch_add(1, Ordering::SeqCst);
        self.validate_endpoint(endpoint)
    }

    async fn secure_client(
        &self,
        _endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<ManagedRemoteHttpClient> {
        self.connections.fetch_add(1, Ordering::SeqCst);
        Err(McpPlatformError::new(
            McpPlatformErrorCode::RemoteHttpPolicyUnavailable,
            "TLS hostname failure for mcp.example.com with bearer-canary",
        ))
    }
}

struct StaticResolver(Vec<SocketAddr>);

#[async_trait]
impl ManagedRemoteResolver for StaticResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> McpPlatformResult<Vec<SocketAddr>> {
        Ok(self.0.clone())
    }
}

struct SuccessfulRegistration;

#[async_trait]
impl RegistrationEffectAdapter for SuccessfulRegistration {
    fn adapter_id(&self) -> &'static str {
        "connection_registration"
    }

    fn adapter_version(&self) -> &'static str {
        "1"
    }

    async fn verify(
        &self,
        _effect: &RegistrationEffect,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<RegistrationEffectEvidence> {
        Ok(RegistrationEffectEvidence {
            adapter_id: self.adapter_id().to_string(),
            adapter_version: self.adapter_version().to_string(),
        })
    }
}

#[derive(Default)]
struct ReadyAuthProbe(AtomicUsize);

impl AuthRequirementResolver for ReadyAuthProbe {
    fn requirement(
        &self,
        _auth: &lumina::mcp_platform::manifest::Auth,
    ) -> McpPlatformResult<AuthRequirement> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AuthRequirement::Ready)
    }
}

#[derive(Default)]
struct HealthyAdapterProbe(AtomicUsize);

#[async_trait]
impl HealthCheckAdapter for HealthyAdapterProbe {
    fn adapter_id(&self) -> &'static str {
        "mcp_health"
    }

    fn adapter_version(&self) -> &'static str {
        "1"
    }

    async fn run(
        &self,
        _execution: HealthExecution,
        _cancellation: CancellationToken,
    ) -> McpPlatformResult<HealthAdapterResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(HealthAdapterResult {
            result_code: HealthResultCode::Healthy,
            latency_ms: 1,
            capabilities_digest: None,
            tools_digest: None,
            detail_code: HealthDetailCode::McpInitializeSucceeded,
        })
    }
}

#[derive(Default)]
struct TransportProbe {
    extension_config_calls: AtomicUsize,
}

impl TransportProjectionAdapter for TransportProbe {
    fn adapter_id(&self) -> &'static str {
        "core_transport_projection"
    }

    fn adapter_version(&self) -> &'static str {
        "1"
    }

    fn registration_effect(
        &self,
        manifest: &Manifest,
        plan: &InstallationPlan,
    ) -> McpPlatformResult<RegistrationEffect> {
        CoreTransportProjectionAdapter.registration_effect(manifest, plan)
    }

    fn extension_config(
        &self,
        projection: &ConnectionProjection,
        stable_key: &str,
    ) -> McpPlatformResult<ExtensionConfig> {
        self.extension_config_calls.fetch_add(1, Ordering::SeqCst);
        CoreTransportProjectionAdapter.extension_config(projection, stable_key)
    }
}

#[derive(Default)]
struct ProjectionProbe {
    entries: Mutex<BTreeMap<String, ExtensionEntry>>,
    get_calls: AtomicUsize,
    put_calls: AtomicUsize,
    replace_calls: AtomicUsize,
    set_enabled_calls: AtomicUsize,
}

#[async_trait]
impl ProjectionSink for ProjectionProbe {
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
        self.put_calls.fetch_add(1, Ordering::SeqCst);
        let entry = ExtensionEntry {
            enabled: false,
            config,
        };
        let mut entries = self.entries.lock().unwrap();
        match entries.get(key) {
            Some(existing) if existing == &entry => Ok(ProjectionSnapshot {
                entry,
                created: false,
            }),
            Some(_) => Err(McpPlatformError::new(
                McpPlatformErrorCode::ProjectionConflict,
                "test projection conflict",
            )),
            None => {
                entries.insert(key.to_string(), entry.clone());
                Ok(ProjectionSnapshot {
                    entry,
                    created: true,
                })
            }
        }
    }

    async fn get(&self, key: &str) -> McpPlatformResult<Option<ProjectionSnapshot>> {
        self.get_calls.fetch_add(1, Ordering::SeqCst);
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
        self.set_enabled_calls.fetch_add(1, Ordering::SeqCst);
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.get_mut(key).ok_or_else(|| {
            McpPlatformError::new(McpPlatformErrorCode::NotFound, "test projection missing")
        })?;
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
        let mut entries = self.entries.lock().unwrap();
        match entries.get(key) {
            None => Ok(false),
            Some(existing) if existing == &expected.entry => {
                entries.remove(key);
                Ok(true)
            }
            Some(_) => Err(McpPlatformError::new(
                McpPlatformErrorCode::ProjectionConflict,
                "test projection conflict",
            )),
        }
    }

    async fn replace_owned_disabled(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
        config: ExtensionConfig,
    ) -> McpPlatformResult<ProjectionSnapshot> {
        self.replace_calls.fetch_add(1, Ordering::SeqCst);
        let mut entries = self.entries.lock().unwrap();
        let existing = entries.get_mut(key).ok_or_else(|| {
            McpPlatformError::new(McpPlatformErrorCode::NotFound, "test projection missing")
        })?;
        if existing != &expected.entry {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::ProjectionConflict,
                "test projection conflict",
            ));
        }
        let entry = ExtensionEntry {
            enabled: false,
            config,
        };
        *existing = entry.clone();
        Ok(ProjectionSnapshot {
            entry,
            created: false,
        })
    }
}

async fn service_with_gate(
    gate: Arc<dyn RemoteHttpNetworkPolicy>,
) -> (
    tempfile::TempDir,
    Arc<SqliteMcpPlatformRepository>,
    McpPlatformService,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            directory.path().join("platform.db"),
            integrity_signer(),
        )
        .await
        .unwrap(),
    );
    let service = McpPlatformService::new_with_remote_http_network_policy(
        repository.clone(),
        Arc::new(TestClock(AtomicI64::new(1_000))),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions::default(),
        gate,
    );
    (directory, repository, service)
}

fn managed_config() -> ExtensionConfig {
    ExtensionConfig::ManagedStreamableHttp {
        name: "managed-test".to_string(),
        description: "managed test".to_string(),
        uri: "https://mcp.example.com/".to_string(),
        timeout: Some(1),
        bundled: None,
        available_tools: Vec::new(),
    }
}

fn unauthenticated_remote() -> String {
    let mut manifest: serde_json::Value = serde_json::from_str(REMOTE).unwrap();
    manifest["auth"] = serde_json::json!({"type":"none"});
    manifest["transport"]["allowed_redirect_origins"] = serde_json::json!([]);
    serde_json::to_string(&manifest).unwrap()
}

async fn stage_stale_projection_rollback(
    repository: &SqliteMcpPlatformRepository,
    source_task: &TaskRecord,
    task_id: &str,
    idempotency_key: &str,
    compensation: &CompensationDescriptor,
    now_ms: i64,
) -> TaskRecord {
    let rollback_evidence = RollbackEvidence {
        compensation_available: true,
        remaining_compensations: vec![compensation.clone()],
    };
    let task = repository
        .create_task(CreateTask {
            task_id,
            plan_id: &source_task.plan_id,
            plan_digest: &source_task.plan_digest,
            operation: source_task.operation,
            idempotency_key,
            actor: "crashed-worker",
            now_ms,
            adapter_evidence: source_task.adapter_evidence.as_ref(),
            rollback_evidence: Some(&rollback_evidence),
        })
        .await
        .unwrap();
    let awaiting = repository
        .transition_task(TaskTransition {
            task_id,
            expected_revision: task.revision,
            next_status: TaskStatus::AwaitingConfirmation,
            actor: "crashed-worker",
            now_ms: now_ms + 1,
            heartbeat_at_ms: None,
            progress: 0,
            redacted_error: None,
            rollback_status: RollbackStatus::NotRequired,
            rollback_evidence: Some(&rollback_evidence),
        })
        .await
        .unwrap();
    repository
        .confirm_task(task_id, awaiting.revision, "crashed-worker", now_ms + 2)
        .await
        .unwrap();
    let running = repository
        .claim_next_task("crashed-worker", now_ms + 3, 30_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(running.task_id, task_id);
    repository
        .add_task_step_with_adapter(
            task_id,
            7,
            &format!("{task_id}:projection-writer"),
            &CompensationDescriptor::NoCompensation,
            "connection_projection_repository",
            "1",
            "crashed-worker",
            running.revision,
            now_ms + 4,
        )
        .await
        .unwrap();
    repository
        .transition_task_step(StepTransition {
            task_id,
            ordinal: 7,
            owner_id: "crashed-worker",
            expected_task_revision: running.revision,
            expected_status: TaskStepStatus::NotStarted,
            next_status: TaskStepStatus::Started,
            evidence: None,
            actor: "crashed-worker",
            now_ms: now_ms + 4,
        })
        .await
        .unwrap();
    let running = repository.get_task(task_id).await.unwrap();
    repository
        .put_owned_connection_projection(PutOwnedProjection {
            plan_id: &source_task.plan_id,
            owner_task_id: task_id,
            worker_owner_id: "crashed-worker",
            step_ordinal: 7,
            step_token: &format!("{task_id}:projection-writer"),
            now_ms: now_ms + 5,
        })
        .await
        .unwrap();
    repository
        .transition_task_step(StepTransition {
            task_id,
            ordinal: 7,
            owner_id: "crashed-worker",
            expected_task_revision: running.revision,
            expected_status: TaskStepStatus::Started,
            next_status: TaskStepStatus::Committed,
            evidence: None,
            actor: "crashed-worker",
            now_ms: now_ms + 6,
        })
        .await
        .unwrap();
    let running = repository.get_task(task_id).await.unwrap();
    repository
        .add_task_step_with_adapter(
            task_id,
            8,
            &format!("{task_id}:durable-restore"),
            compensation,
            "extension_config_sink",
            "1",
            "crashed-worker",
            running.revision,
            now_ms + 7,
        )
        .await
        .unwrap();
    repository
        .transition_task_step(StepTransition {
            task_id,
            ordinal: 8,
            owner_id: "crashed-worker",
            expected_task_revision: running.revision,
            expected_status: TaskStepStatus::NotStarted,
            next_status: TaskStepStatus::Started,
            evidence: None,
            actor: "crashed-worker",
            now_ms: now_ms + 7,
        })
        .await
        .unwrap();
    let running = repository.get_task(task_id).await.unwrap();
    repository
        .transition_task_step(StepTransition {
            task_id,
            ordinal: 8,
            owner_id: "crashed-worker",
            expected_task_revision: running.revision,
            expected_status: TaskStepStatus::Started,
            next_status: TaskStepStatus::Committed,
            evidence: None,
            actor: "crashed-worker",
            now_ms: now_ms + 8,
        })
        .await
        .unwrap();
    let running = repository.get_task(task_id).await.unwrap();
    repository
        .transition_task(TaskTransition {
            task_id,
            expected_revision: running.revision,
            next_status: TaskStatus::RollingBack,
            actor: "crashed-worker",
            now_ms: now_ms + 9,
            heartbeat_at_ms: Some(now_ms + 9),
            progress: 80,
            redacted_error: None,
            rollback_status: RollbackStatus::InProgress,
            rollback_evidence: Some(&rollback_evidence),
        })
        .await
        .unwrap()
}

fn canonical_json(value: &serde_json::Value, output: &mut Vec<u8>) {
    match value {
        serde_json::Value::Null => output.extend_from_slice(b"null"),
        serde_json::Value::Bool(value) => {
            output.extend_from_slice(if *value { b"true" } else { b"false" })
        }
        serde_json::Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        serde_json::Value::String(value) => {
            output.extend_from_slice(serde_json::to_string(value).unwrap().as_bytes())
        }
        serde_json::Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                canonical_json(value, output);
            }
            output.push(b']');
        }
        serde_json::Value::Object(values) => {
            output.push(b'{');
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                canonical_json(&serde_json::Value::String(key.clone()), output);
                output.push(b':');
                canonical_json(&values[key], output);
            }
            output.push(b'}');
        }
    }
}

fn refresh_plan_digest(plan: &mut serde_json::Value) {
    let mut content = plan.clone();
    content.as_object_mut().unwrap().remove("plan_digest");
    let mut canonical = Vec::new();
    canonical_json(&content, &mut canonical);
    plan["plan_digest"] =
        serde_json::Value::String(lumina::utils::bytes_to_hex(Sha256::digest(canonical)));
}

#[tokio::test]
async fn plan_activation_health_and_runtime_use_the_managed_gate() {
    let gate = Arc::new(RecordingGate::default());
    let (directory, repository, service) = service_with_gate(gate.clone()).await;
    let context = test_context("mcp-platform-4b");
    let review = service
        .manual_plan_create(
            &context,
            ManualPlanCreateInput {
                connection: ManualConnectionInput::RemoteHttp {
                    endpoint: "https://mcp.example.com/".to_string(),
                    auth: ManualHttpAuth::None,
                },
                idempotency_key: "managed-plan".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(gate.plans.load(Ordering::SeqCst), 2);

    let task = service
        .install_confirm(
            &context,
            InstallConfirmInput {
                plan_id: review.plan_id,
                plan_digest: review.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "managed-confirm".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(service.runner_tick().await.unwrap());
    assert!(gate.connections.load(Ordering::SeqCst) >= 1);
    assert_ne!(
        repository.get_task(&task.task_id).await.unwrap().status,
        lumina::mcp_platform::TaskStatus::Succeeded
    );

    let health = ProductionHealthCheckAdapter::new(gate.clone());
    let result = health
        .run(
            HealthExecution {
                effect: RegistrationEffect::RemoteHttp {
                    endpoint: "https://mcp.example.com/".to_string(),
                    allowed_redirect_origins: Vec::new(),
                    timeout_seconds: Some(1),
                },
                check: HealthCheck::McpInitialize { timeout_seconds: 1 },
                projection_config: managed_config(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.result_code, HealthResultCode::Unhealthy);

    let manager = Arc::new(
        ExtensionManager::new_without_provider_with_managed_remote_http_policy(
            directory.path().to_path_buf(),
            gate.clone(),
        ),
    );
    let error = manager
        .add_extension(managed_config(), None, None, None)
        .await
        .unwrap_err();
    let redacted = error.to_string();
    assert!(!redacted.contains("mcp.example.com"));
    assert!(!redacted.contains("bearer-canary"));
    assert!(gate.connections.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn endpoint_and_dns_policy_fail_closed_without_public_network() {
    let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
    let policy =
        CoreManagedRemoteHttpNetworkPolicy::with_resolver(Arc::new(StaticResolver(vec![public])));
    policy
        .validate_for_plan("https://mcp.example.com/", Duration::from_secs(1))
        .await
        .unwrap();

    for endpoint in [
        "http://mcp.example.com/",
        "https://user@mcp.example.com/",
        "https://mcp.example.com/?token=value",
        "https://mcp.example.com/#fragment",
        "https://mcp.example.com:443/",
        "https://mcp.example.com",
        "https://127.0.0.1/",
        "https://localhost/",
        "https://printer.local/",
        "https://service.internal/",
        "https://router.home.arpa/",
        "https://home.arpa/",
        "https://service.example/",
        "https://hidden.onion/",
        "https://xn--bcher-kva.example/",
        "https://bücher.example/",
        "https://mcp.example.com/%2fadmin",
    ] {
        assert_eq!(
            policy.validate_endpoint(endpoint).unwrap_err().code(),
            McpPlatformErrorCode::UnsafeUrl,
            "accepted {endpoint}"
        );
    }

    for peers in [
        vec!["10.0.0.1:443".parse().unwrap()],
        vec!["169.254.169.254:443".parse().unwrap()],
        vec!["[fc00::1]:443".parse().unwrap()],
        vec!["8.8.8.8:8443".parse().unwrap()],
        vec![public, "127.0.0.1:443".parse().unwrap()],
    ] {
        let policy =
            CoreManagedRemoteHttpNetworkPolicy::with_resolver(Arc::new(StaticResolver(peers)));
        assert_eq!(
            policy
                .validate_for_plan("https://mcp.example.com/", Duration::from_secs(1))
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::UnsafeUrl
        );
    }
}

#[tokio::test]
async fn credentials_fail_before_dns_or_persistence_and_projection_is_redacted() {
    let gate = Arc::new(RecordingGate::default());
    let (_directory, repository, service) = service_with_gate(gate.clone()).await;
    let error = service
        .manual_plan_create(
            &test_context("mcp-platform-4b"),
            ManualPlanCreateInput {
                connection: ManualConnectionInput::RemoteHttp {
                    endpoint: "https://mcp.example.com/".to_string(),
                    auth: ManualHttpAuth::BearerReference {
                        auth_reference: "opaque-canary-handle".to_string(),
                    },
                },
                idempotency_key: "credential-denied".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::CredentialMissing);
    assert_eq!(gate.syntax.load(Ordering::SeqCst), 1);
    assert_eq!(gate.plans.load(Ordering::SeqCst), 0);
    assert_eq!(gate.connections.load(Ordering::SeqCst), 0);
    assert!(repository.list_manifests().await.unwrap().is_empty());

    let catalog_gate = Arc::new(RecordingGate::default());
    let (_catalog_directory, catalog_repository, catalog_service) =
        service_with_gate(catalog_gate.clone()).await;
    let verified = parse_manifest(REMOTE.as_bytes()).unwrap();
    let manifest_digest = verified.digest().to_string();
    catalog_repository
        .save_manifest(&ManifestRecord {
            verified,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: Default::default(),
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let error = catalog_service
        .plan_create(
            &test_context("mcp-platform-4b"),
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest,
                    installation_scope: lumina::mcp_platform::InstallationScope::User,
                },
                idempotency_key: "catalog-credential-denied".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::CredentialMissing);
    assert_eq!(catalog_gate.syntax.load(Ordering::SeqCst), 1);
    assert_eq!(catalog_gate.plans.load(Ordering::SeqCst), 0);
    assert_eq!(catalog_gate.connections.load(Ordering::SeqCst), 0);
    assert!(catalog_repository
        .get_plan_by_idempotency_key("catalog-credential-denied")
        .await
        .unwrap()
        .is_none());

    let projection = ConnectionProjection::RemoteHttp {
        name: "managed".to_string(),
        description: String::new(),
        uri: "https://mcp.example.com/".to_string(),
        timeout_seconds: Some(1),
    };
    let serialized_projection = serde_json::to_string(&projection).unwrap();
    let serialized_config = serde_json::to_string(&projection.to_extension_config()).unwrap();
    for forbidden in [
        "opaque-canary-handle",
        "credential_name",
        "header_name",
        "env_keys",
        "headers",
    ] {
        assert!(!serialized_projection.contains(forbidden));
        assert!(!serialized_config.contains(forbidden));
    }
}

#[tokio::test]
async fn historical_authenticated_remote_fails_closed_for_health_enablement_and_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("platform.db");
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &database_path,
            integrity_signer(),
        )
        .await
        .unwrap(),
    );
    let gate = Arc::new(RecordingGate::default());
    let auth = Arc::new(ReadyAuthProbe::default());
    let health = Arc::new(HealthyAdapterProbe::default());
    let sink = Arc::new(ProjectionProbe::default());
    let service = McpPlatformService::new_with_lifecycle_ports(
        repository.clone(),
        Arc::new(TestClock(AtomicI64::new(1_000))),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions::default(),
        LifecyclePorts {
            registration: Arc::new(SuccessfulRegistration),
            host_integration: Arc::new(EmptyHostIntegrationAdapter),
            transport: Arc::new(CoreTransportProjectionAdapter),
            auth: auth.clone(),
            health: health.clone(),
            projection_sink: sink.clone(),
        },
        gate.clone(),
    );
    let verified = parse_manifest(unauthenticated_remote().as_bytes()).unwrap();
    let manifest_digest = verified.digest().to_string();
    repository
        .save_manifest(&ManifestRecord {
            verified,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: Default::default(),
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let review = service
        .plan_create(
            &test_context("mcp-platform-4b"),
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest,
                    installation_scope: lumina::mcp_platform::InstallationScope::User,
                },
                idempotency_key: "historical-auth-plan".to_string(),
            },
        )
        .await
        .unwrap();
    let install = service
        .install_confirm(
            &test_context("mcp-platform-4b"),
            InstallConfirmInput {
                plan_id: review.plan_id,
                plan_digest: review.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "historical-auth-install".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(service.runner_tick().await.unwrap());
    assert_eq!(
        repository.get_task(&install.task_id).await.unwrap().status,
        TaskStatus::Succeeded
    );
    let managed = service
        .managed_list(
            &test_context("mcp-platform-4b"),
            ManagedListInput::default(),
        )
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();

    let authenticated = parse_manifest(REMOTE.as_bytes()).unwrap();
    let authenticated_digest = authenticated.digest().to_string();
    repository
        .save_manifest(&ManifestRecord {
            verified: authenticated,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: Default::default(),
            created_at_ms: 2,
        })
        .await
        .unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&database_path))
        .await
        .unwrap();
    sqlx::query("UPDATE managed_mcps SET active_manifest_digest = ? WHERE managed_mcp_id = ?")
        .bind(&authenticated_digest)
        .bind(&managed.managed_mcp_id)
        .execute(&pool)
        .await
        .unwrap();

    let auth_calls = auth.0.load(Ordering::SeqCst);
    let health_calls = health.0.load(Ordering::SeqCst);
    let connection_calls = gate.connections.load(Ordering::SeqCst);
    let set_enabled_calls = sink.set_enabled_calls.load(Ordering::SeqCst);
    let health_task = service
        .health_run(
            &test_context("mcp-platform-4b"),
            HealthRunInput {
                managed_mcp_id: managed.managed_mcp_id.clone(),
                mode: HealthCheckMode::Runtime,
                idempotency_key: "historical-auth-health".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(service.runner_tick().await.unwrap());
    let failed_health = repository.get_task(&health_task.task_id).await.unwrap();
    assert_eq!(failed_health.status, TaskStatus::Failed);
    let redacted = failed_health.redacted_error.unwrap();
    assert_eq!(redacted.code(), RedactedErrorCode::AdapterFailed);
    assert_eq!(
        redacted.message(),
        "managed remote HTTP credentials are unavailable"
    );
    assert_eq!(auth.0.load(Ordering::SeqCst), auth_calls);
    assert_eq!(health.0.load(Ordering::SeqCst), health_calls);
    assert_eq!(gate.connections.load(Ordering::SeqCst), connection_calls);

    let inventory = repository
        .get_managed_inventory(&managed.managed_mcp_id)
        .await
        .unwrap();
    let error = service
        .set_default_enabled(
            &test_context("mcp-platform-4b"),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.managed_mcp_id.clone(),
                enabled: true,
                expected_revision: inventory.managed.revision,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::CredentialMissing);
    assert_eq!(
        error.message(),
        "managed remote HTTP credentials are unavailable"
    );
    assert_eq!(auth.0.load(Ordering::SeqCst), auth_calls);
    assert_eq!(
        sink.set_enabled_calls.load(Ordering::SeqCst),
        set_enabled_calls
    );

    sqlx::query(
        "INSERT INTO projection_mutations(managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,created_at_ms,updated_at_ms) VALUES (?, ?, 0, 1, 'started', 3, 3)",
    )
    .bind(&managed.managed_mcp_id)
    .bind(inventory.managed.revision)
    .execute(&pool)
    .await
    .unwrap();
    assert!(!service.runner_tick().await.unwrap());
    let pending = repository
        .list_pending_projection_mutations()
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.managed_mcp_id == managed.managed_mcp_id)
        .unwrap();
    assert_eq!(pending.status, ProjectionMutationStatus::RecoveryRequired);
    assert_eq!(
        sink.set_enabled_calls.load(Ordering::SeqCst),
        set_enabled_calls
    );
    assert!(
        !repository
            .get_managed_inventory(&managed.managed_mcp_id)
            .await
            .unwrap()
            .managed
            .state
            .default_enabled
    );
    assert_eq!(auth.0.load(Ordering::SeqCst), auth_calls);
    assert_eq!(health.0.load(Ordering::SeqCst), health_calls);
    assert_eq!(gate.connections.load(Ordering::SeqCst), connection_calls);
    pool.close().await;
}

#[tokio::test]
async fn projection_json_tampering_stales_plan_confirm_consume_hydrate_and_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("platform.db");
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &database_path,
            integrity_signer(),
        )
        .await
        .unwrap(),
    );
    let service = McpPlatformService::new_with_lifecycle_ports(
        repository.clone(),
        Arc::new(TestClock(AtomicI64::new(1_000))),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions::default(),
        LifecyclePorts {
            registration: Arc::new(SuccessfulRegistration),
            host_integration: Arc::new(EmptyHostIntegrationAdapter),
            transport: Arc::new(CoreTransportProjectionAdapter),
            auth: Arc::new(ReadyAuthProbe::default()),
            health: Arc::new(HealthyAdapterProbe::default()),
            projection_sink: Arc::new(ProjectionProbe::default()),
        },
        Arc::new(RecordingGate::default()),
    );
    let verified = parse_manifest(unauthenticated_remote().as_bytes()).unwrap();
    let manifest_digest = verified.digest().to_string();
    repository
        .save_manifest(&ManifestRecord {
            verified,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: Default::default(),
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let context = test_context("mcp-platform-4b");
    let review = service
        .plan_create(
            &context,
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest,
                    installation_scope: lumina::mcp_platform::InstallationScope::User,
                },
                idempotency_key: "projection-tamper-install-plan".to_string(),
            },
        )
        .await
        .unwrap();
    let install = service
        .install_confirm(
            &context,
            InstallConfirmInput {
                plan_id: review.plan_id,
                plan_digest: review.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "projection-tamper-install".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(service.runner_tick().await.unwrap());
    assert_eq!(
        repository.get_task(&install.task_id).await.unwrap().status,
        TaskStatus::Succeeded
    );
    let managed = service
        .managed_list(&context, ManagedListInput::default())
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    let original_projection = repository
        .get_connection_projection(&managed.managed_mcp_id)
        .await
        .unwrap();
    let source_task = repository
        .get_task(original_projection.owner_task_id.as_deref().unwrap())
        .await
        .unwrap();
    let sibling = repository
        .create_task(CreateTask {
            task_id: "projection-sibling-task",
            plan_id: &source_task.plan_id,
            plan_digest: &source_task.plan_digest,
            operation: source_task.operation,
            idempotency_key: "projection-sibling-task-key",
            actor: "sibling-writer",
            now_ms: 1_002,
            adapter_evidence: source_task.adapter_evidence.as_ref(),
            rollback_evidence: source_task.rollback_evidence.as_ref(),
        })
        .await
        .unwrap();
    let sibling = repository
        .transition_task(TaskTransition {
            task_id: &sibling.task_id,
            expected_revision: sibling.revision,
            next_status: TaskStatus::AwaitingConfirmation,
            actor: "sibling-writer",
            now_ms: 1_003,
            heartbeat_at_ms: None,
            progress: 0,
            redacted_error: None,
            rollback_status: RollbackStatus::NotRequired,
            rollback_evidence: sibling.rollback_evidence.as_ref(),
        })
        .await
        .unwrap();
    repository
        .confirm_task(&sibling.task_id, sibling.revision, "sibling-writer", 1_004)
        .await
        .unwrap();
    let sibling = repository
        .claim_next_task("sibling-writer", 1_005, 30_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sibling.task_id, "projection-sibling-task");
    repository
        .add_task_step_with_adapter(
            &sibling.task_id,
            8,
            "projection-sibling-config-step",
            &CompensationDescriptor::NoCompensation,
            "extension_config_sink",
            "1",
            "sibling-writer",
            sibling.revision,
            1_006,
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .put_owned_connection_projection(PutOwnedProjection {
                plan_id: &sibling.plan_id,
                owner_task_id: &sibling.task_id,
                worker_owner_id: "sibling-writer",
                step_ordinal: 8,
                step_token: "projection-sibling-config-step",
                now_ms: 1_006,
            })
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
    assert_eq!(
        repository
            .remove_owned_projection(RemoveOwnedProjection {
                managed_mcp_id: &managed.managed_mcp_id,
                task_id: &sibling.task_id,
                worker_owner_id: "sibling-writer",
                compensation_ordinal: 8,
                now_ms: 1_006,
            })
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
    let completed_owner_write = repository
        .put_owned_connection_projection(PutOwnedProjection {
            plan_id: original_projection.plan_id.as_deref().unwrap(),
            owner_task_id: original_projection.owner_task_id.as_deref().unwrap(),
            worker_owner_id: "runner",
            step_ordinal: 8,
            step_token: "completed-owner-cannot-reuse",
            now_ms: 1_001,
        })
        .await
        .unwrap_err();
    assert_eq!(
        completed_owner_write.code(),
        McpPlatformErrorCode::IntegrityError
    );
    let replacement = ConnectionProjection::RemoteHttp {
        name: original_projection.link_key.clone(),
        description: "public writer replacement".to_string(),
        uri: "https://mcp.example.com/replacement".to_string(),
        timeout_seconds: Some(30),
    };
    let public_write_error = repository
        .put_connection_projection(
            &managed.managed_mcp_id,
            &original_projection.link_key,
            &replacement,
            Some(original_projection.revision),
            1_001,
        )
        .await
        .unwrap_err();
    assert_eq!(
        public_write_error.code(),
        McpPlatformErrorCode::ProjectionConflict
    );
    assert_eq!(
        repository
            .get_connection_projection(&managed.managed_mcp_id)
            .await
            .unwrap(),
        original_projection
    );
    let profile = service
        .profile_create(
            &context,
            ProfileCreateInput {
                name: "Projection tamper profile".to_string(),
                description: String::new(),
                managed_mcp_ids: vec![managed.managed_mcp_id.clone()],
                idempotency_key: "projection-tamper-profile".to_string(),
            },
        )
        .await
        .unwrap();
    let plan = service
        .profile_apply_plan_create(
            &context,
            ProfileApplyPlanInput {
                profile_id: profile.profile_id.clone(),
                profile_revision: profile.revision,
                idempotency_key: "projection-tamper-application-plan".to_string(),
            },
        )
        .await
        .unwrap();

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&database_path))
        .await
        .unwrap();
    let original_json: String = sqlx::query_scalar(
        "SELECT projection_json FROM connection_projections WHERE managed_mcp_id=?",
    )
    .bind(&managed.managed_mcp_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let original_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM connection_projections WHERE managed_mcp_id=?")
            .bind(&managed.managed_mcp_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let original_plan_snapshot: String =
        sqlx::query_scalar("SELECT snapshot_json FROM mcp_profile_apply_plans WHERE plan_id=?")
            .bind(&plan.plan_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut tampered_plan_snapshot: serde_json::Value =
        serde_json::from_str(&original_plan_snapshot).unwrap();
    tampered_plan_snapshot["entries"][0]["health"] = serde_json::json!("snapshot-secret-canary");
    let tampered_plan_snapshot = serde_json::to_string(&tampered_plan_snapshot).unwrap();
    let set_plan_snapshot = |value: String| {
        let pool = pool.clone();
        let plan_id = plan.plan_id.clone();
        async move {
            sqlx::query("UPDATE mcp_profile_apply_plans SET snapshot_json=? WHERE plan_id=?")
                .bind(value)
                .bind(plan_id)
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    let mut tampered: serde_json::Value = serde_json::from_str(&original_json).unwrap();
    tampered["uri"] = serde_json::json!("https://projection-secret-canary.invalid/");
    let tampered_json = serde_json::to_string(&tampered).unwrap();
    let tampered_projection: ConnectionProjection = serde_json::from_value(tampered).unwrap();
    let tampered_digest =
        lumina::mcp_platform::projection_config_digest(&tampered_projection).unwrap();
    let set_projection_json = |value: String| {
        let pool = pool.clone();
        let managed_mcp_id = managed.managed_mcp_id.clone();
        async move {
            sqlx::query(
                "UPDATE connection_projections SET projection_json=? WHERE managed_mcp_id=?",
            )
            .bind(value)
            .bind(managed_mcp_id)
            .execute(&pool)
            .await
            .unwrap();
        }
    };

    set_plan_snapshot(tampered_plan_snapshot.clone()).await;
    let snapshot_confirm_error = service
        .profile_apply_confirm(
            &context,
            &plan.plan_id,
            &plan.confirmation.confirmation_token,
            true,
        )
        .await
        .unwrap_err();
    assert_eq!(
        snapshot_confirm_error.code(),
        McpPlatformErrorCode::PlanStale
    );
    assert!(!snapshot_confirm_error
        .message()
        .contains("snapshot-secret-canary"));
    set_plan_snapshot(original_plan_snapshot.clone()).await;

    sqlx::query(
        "UPDATE connection_projections SET projection_json=?,projection_digest=? WHERE managed_mcp_id=?",
    )
    .bind(&tampered_json)
    .bind(&tampered_digest)
    .bind(&managed.managed_mcp_id)
    .execute(&pool)
    .await
    .unwrap();
    let authoritative_binding_error = repository
        .get_connection_projection(&managed.managed_mcp_id)
        .await
        .unwrap_err();
    assert_eq!(
        authoritative_binding_error.code(),
        McpPlatformErrorCode::IntegrityError
    );
    sqlx::query(
        "UPDATE connection_projections SET projection_json=?,projection_digest=? WHERE managed_mcp_id=?",
    )
    .bind(&original_json)
    .bind(&original_projection.projection_digest)
    .bind(&managed.managed_mcp_id)
    .execute(&pool)
    .await
    .unwrap();

    set_projection_json(tampered_json.clone()).await;
    let plan_error = service
        .profile_apply_plan_create(
            &context,
            ProfileApplyPlanInput {
                profile_id: profile.profile_id,
                profile_revision: profile.revision,
                idempotency_key: "projection-tamper-second-plan".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(plan_error.code(), McpPlatformErrorCode::IntegrityError);
    assert!(!plan_error.message().contains("projection-secret-canary"));
    let confirm_error = service
        .profile_apply_confirm(
            &context,
            &plan.plan_id,
            &plan.confirmation.confirmation_token,
            true,
        )
        .await
        .unwrap_err();
    assert_eq!(confirm_error.code(), McpPlatformErrorCode::IntegrityError);
    assert!(!confirm_error.message().contains("projection-secret-canary"));

    set_projection_json(original_json.clone()).await;
    let token = service
        .profile_apply_confirm(
            &context,
            &plan.plan_id,
            &plan.confirmation.confirmation_token,
            true,
        )
        .await
        .unwrap();
    set_plan_snapshot(tampered_plan_snapshot.clone()).await;
    let snapshot_consume_error = service
        .consume_profile_application_token(&context, &token.token)
        .await
        .unwrap_err();
    assert_eq!(
        snapshot_consume_error.code(),
        McpPlatformErrorCode::PlanStale
    );
    assert!(!snapshot_consume_error
        .message()
        .contains("snapshot-secret-canary"));
    set_plan_snapshot(original_plan_snapshot.clone()).await;
    set_projection_json(tampered_json.clone()).await;
    assert!(service
        .consume_profile_application_token(&context, &token.token)
        .await
        .is_err());

    set_projection_json(original_json.clone()).await;
    let consumed = service
        .consume_profile_application_token(&context, &token.token)
        .await
        .unwrap();
    service
        .finish_profile_application(
            &consumed.application_id,
            Some("projection-tamper-session"),
            "created",
            "session_created",
        )
        .await
        .unwrap();
    let marker = ProfileApplicationMarker {
        application_id: consumed.application_id.clone(),
        profile_id: consumed.profile_id.clone(),
        profile_revision: consumed.profile_revision,
        merge_policy: "replace_managed_only".to_string(),
        original_session_id: "projection-tamper-session".to_string(),
        managed_extension_names: consumed.managed_extension_names().to_vec(),
    };
    set_plan_snapshot(tampered_plan_snapshot).await;
    let snapshot_runtime_error = service
        .verify_profile_application_runtime(&context, &consumed)
        .await
        .unwrap_err();
    assert_eq!(
        snapshot_runtime_error.code(),
        McpPlatformErrorCode::PlanStale
    );
    assert!(!snapshot_runtime_error
        .message()
        .contains("snapshot-secret-canary"));
    let snapshot_hydrate_error = service
        .hydrate_profile_application(&context, &marker, "projection-tamper-session")
        .await
        .unwrap_err();
    assert_eq!(
        snapshot_hydrate_error.code(),
        McpPlatformErrorCode::PlanStale
    );
    set_plan_snapshot(original_plan_snapshot).await;
    sqlx::query("UPDATE connection_projections SET projection_digest='' WHERE managed_mcp_id=?")
        .bind(&managed.managed_mcp_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(service
        .verify_profile_application_runtime(&context, &consumed)
        .await
        .is_err());
    sqlx::query("UPDATE connection_projections SET projection_digest=? WHERE managed_mcp_id=?")
        .bind(&original_projection.projection_digest)
        .bind(&managed.managed_mcp_id)
        .execute(&pool)
        .await
        .unwrap();
    set_projection_json(tampered_json).await;
    assert!(service
        .hydrate_profile_application(&context, &marker, "projection-tamper-session")
        .await
        .is_err());
    assert!(service
        .verify_profile_application_runtime(&context, &consumed)
        .await
        .is_err());

    set_projection_json(original_json.clone()).await;
    let hydrated = service
        .hydrate_profile_application(&context, &marker, "projection-tamper-session")
        .await
        .unwrap();
    service
        .verify_profile_application_runtime(&context, &hydrated)
        .await
        .unwrap();

    let injected_projection = ConnectionProjection::ManualStdio {
        name: original_projection.link_key.clone(),
        description: "legacy downgrade injection".to_string(),
        executable: "attacker-controlled-command".to_string(),
        args: vec!["--exfiltrate".to_string()],
        environment_keys: Vec::new(),
        cwd: None,
        timeout_seconds: Some(30),
    };
    let injected_json =
        lumina::mcp_platform::encode_projection_config(&injected_projection).unwrap();
    let injected_digest =
        lumina::mcp_platform::projection_config_digest(&injected_projection).unwrap();
    sqlx::query(
        "UPDATE connection_projections SET projection_json=?,projection_digest=?,plan_id=NULL,manifest_digest=NULL,owner_task_id=NULL WHERE managed_mcp_id=?",
    )
    .bind(injected_json)
    .bind(injected_digest)
    .bind(&managed.managed_mcp_id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        repository
            .get_connection_projection(&managed.managed_mcp_id)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
    assert!(service.managed_extension_provenance().await.is_err());

    sqlx::query(
        "UPDATE connection_projections SET projection_json=?,projection_digest=?,plan_id=?,manifest_digest=?,owner_task_id=? WHERE managed_mcp_id=?",
    )
    .bind(&original_json)
    .bind(&original_projection.projection_digest)
    .bind(original_projection.plan_id.as_deref())
    .bind(original_projection.manifest_digest.as_deref())
    .bind(original_projection.owner_task_id.as_deref())
    .bind(&managed.managed_mcp_id)
    .execute(&pool)
    .await
    .unwrap();

    let mixed_manifest_digest = "f".repeat(64);
    sqlx::query("UPDATE managed_mcps SET active_manifest_digest=? WHERE managed_mcp_id=?")
        .bind(&mixed_manifest_digest)
        .bind(&managed.managed_mcp_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        service
            .verify_profile_application_runtime(&context, &hydrated)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanStale
    );
    assert!(service
        .profile_apply_confirm(
            &context,
            &plan.plan_id,
            &plan.confirmation.confirmation_token,
            true,
        )
        .await
        .is_err());
    sqlx::query("UPDATE managed_mcps SET active_manifest_digest=? WHERE managed_mcp_id=?")
        .bind(original_projection.manifest_digest.as_deref())
        .bind(&managed.managed_mcp_id)
        .execute(&pool)
        .await
        .unwrap();
    let final_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM connection_projections WHERE managed_mcp_id=?")
            .bind(&managed.managed_mcp_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(final_revision, original_revision);
    pool.close().await;
}

#[tokio::test]
async fn durable_projection_rollback_validates_plan_and_digest_before_restore_effects() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("platform.db");
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            &database_path,
            integrity_signer(),
        )
        .await
        .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(1_000)));
    let gate = Arc::new(RecordingGate::default());
    let auth = Arc::new(ReadyAuthProbe::default());
    let health = Arc::new(HealthyAdapterProbe::default());
    let transport = Arc::new(TransportProbe::default());
    let sink = Arc::new(ProjectionProbe::default());
    let ports = LifecyclePorts {
        registration: Arc::new(SuccessfulRegistration),
        host_integration: Arc::new(EmptyHostIntegrationAdapter),
        transport: transport.clone(),
        auth: auth.clone(),
        health,
        projection_sink: sink.clone(),
    };
    let service = McpPlatformService::new_with_lifecycle_ports(
        repository.clone(),
        clock.clone(),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions::default(),
        ports.clone(),
        gate,
    );
    let verified = parse_manifest(unauthenticated_remote().as_bytes()).unwrap();
    let manifest_digest = verified.digest().to_string();
    repository
        .save_manifest(&ManifestRecord {
            verified,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: Default::default(),
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let review = service
        .plan_create(
            &test_context("mcp-platform-4b"),
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest: manifest_digest.clone(),
                    installation_scope: lumina::mcp_platform::InstallationScope::User,
                },
                idempotency_key: "rollback-source-plan".to_string(),
            },
        )
        .await
        .unwrap();
    let install = service
        .install_confirm(
            &test_context("mcp-platform-4b"),
            InstallConfirmInput {
                plan_id: review.plan_id,
                plan_digest: review.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "rollback-source-install".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(service.runner_tick().await.unwrap());
    let source_task = repository.get_task(&install.task_id).await.unwrap();
    assert_eq!(source_task.status, TaskStatus::Succeeded);
    let managed = service
        .managed_list(
            &test_context("mcp-platform-4b"),
            ManagedListInput::default(),
        )
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    let original = repository
        .get_connection_projection(&managed.managed_mcp_id)
        .await
        .unwrap();

    let mut authenticated: serde_json::Value = serde_json::from_str(REMOTE).unwrap();
    authenticated["auth"]["authorization_url"] =
        serde_json::json!("https://credential-canary.invalid/authorize");
    authenticated["auth"]["token_url"] =
        serde_json::json!("https://credential-canary.invalid/token");
    let authenticated =
        parse_manifest(serde_json::to_vec(&authenticated).unwrap().as_slice()).unwrap();
    let authenticated_digest = authenticated.digest().to_string();
    repository
        .save_manifest(&ManifestRecord {
            verified: authenticated,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: Default::default(),
            created_at_ms: 2,
        })
        .await
        .unwrap();

    let restored_projection = original.projection.clone();
    let restored_digest =
        lumina::mcp_platform::projection_config_digest(&restored_projection).unwrap();
    let restore = |manifest_digest: String| CompensationDescriptor::RestoreManagedProjection {
        managed_mcp_id: managed.managed_mcp_id.clone(),
        link_key: original.link_key.clone(),
        projection: restored_projection.clone(),
        enabled: true,
        projection_digest: restored_digest.clone(),
        manifest_digest,
        plan_id: original.plan_id.clone().unwrap(),
        owner_task_id: original.owner_task_id.clone(),
    };
    let current_projection = ConnectionProjection::RemoteHttp {
        name: "current".to_string(),
        description: "current projection".to_string(),
        uri: "https://current-projection.invalid/".to_string(),
        timeout_seconds: Some(2),
    };
    let current_config = CoreTransportProjectionAdapter
        .extension_config(&current_projection, &original.link_key)
        .unwrap();
    let current_digest =
        lumina::mcp_platform::projection_config_digest(&current_projection).unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&database_path))
        .await
        .unwrap();

    let authenticated_compensation = restore(authenticated_digest);
    let authenticated_task = stage_stale_projection_rollback(
        repository.as_ref(),
        &source_task,
        "task-authenticated-rollback",
        "authenticated-rollback",
        &authenticated_compensation,
        10_000,
    )
    .await;
    sqlx::query(
        "UPDATE connection_projections SET projection_json = ?, projection_digest = ?, \
         owner_task_id = ? WHERE managed_mcp_id = ?",
    )
    .bind(serde_json::to_string(&current_projection).unwrap())
    .bind(&current_digest)
    .bind(&authenticated_task.task_id)
    .bind(&managed.managed_mcp_id)
    .execute(&pool)
    .await
    .unwrap();
    sink.entries.lock().unwrap().insert(
        original.link_key.clone(),
        ExtensionEntry {
            enabled: false,
            config: current_config.clone(),
        },
    );
    let sink_calls = (
        sink.get_calls.load(Ordering::SeqCst),
        sink.put_calls.load(Ordering::SeqCst),
        sink.replace_calls.load(Ordering::SeqCst),
        sink.set_enabled_calls.load(Ordering::SeqCst),
    );
    let auth_calls = auth.0.load(Ordering::SeqCst);
    let extension_config_calls = transport.extension_config_calls.load(Ordering::SeqCst);
    clock.0.store(100_000, Ordering::SeqCst);
    TaskRunner::new(
        repository.clone(),
        clock.clone(),
        ports.clone(),
        "recovery-worker-authenticated".to_string(),
    )
    .recover_startup()
    .await
    .unwrap();

    let failed = repository
        .get_task(&authenticated_task.task_id)
        .await
        .unwrap();
    assert_eq!(failed.status, TaskStatus::RecoveryRequired);
    assert_eq!(failed.rollback_status, RollbackStatus::Incomplete);
    assert_eq!(
        failed.redacted_error.as_ref().unwrap().code(),
        RedactedErrorCode::RollbackFailed
    );
    let redacted = failed.redacted_error.as_ref().unwrap().message();
    assert_eq!(redacted, "MCP lifecycle compensation requires recovery");
    for canary in [
        "credential-canary",
        "rollback-projection-canary",
        "current-projection",
    ] {
        assert!(!redacted.contains(canary));
    }
    assert_eq!(
        (
            sink.get_calls.load(Ordering::SeqCst),
            sink.put_calls.load(Ordering::SeqCst),
            sink.replace_calls.load(Ordering::SeqCst),
            sink.set_enabled_calls.load(Ordering::SeqCst),
        ),
        sink_calls
    );
    assert_eq!(auth.0.load(Ordering::SeqCst), auth_calls);
    assert_eq!(
        transport.extension_config_calls.load(Ordering::SeqCst),
        extension_config_calls
    );
    let unchanged_error = repository
        .get_connection_projection(&managed.managed_mcp_id)
        .await
        .unwrap_err();
    assert_eq!(unchanged_error.code(), McpPlatformErrorCode::IntegrityError);
    let unchanged_owner: String = sqlx::query_scalar(
        "SELECT owner_task_id FROM connection_projections WHERE managed_mcp_id=?",
    )
    .bind(&managed.managed_mcp_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unchanged_owner, authenticated_task.task_id);
    let authenticated_step = repository
        .list_task_steps(&authenticated_task.task_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        authenticated_step.compensation_status,
        lumina::mcp_platform::CompensationStatus::Started
    );

    let digest_compensation = CompensationDescriptor::RestoreManagedProjection {
        managed_mcp_id: managed.managed_mcp_id.clone(),
        link_key: original.link_key.clone(),
        projection: original.projection.clone(),
        enabled: true,
        projection_digest: "0".repeat(64),
        manifest_digest: manifest_digest.clone(),
        plan_id: original.plan_id.clone().unwrap(),
        owner_task_id: original.owner_task_id.clone(),
    };
    let digest_task = stage_stale_projection_rollback(
        repository.as_ref(),
        &source_task,
        "task-digest-rollback",
        "digest-rollback",
        &digest_compensation,
        15_000,
    )
    .await;
    sqlx::query(
        "UPDATE connection_projections SET projection_json = ?, projection_digest = ?, \
         owner_task_id = ? WHERE managed_mcp_id = ?",
    )
    .bind(serde_json::to_string(&current_projection).unwrap())
    .bind(&current_digest)
    .bind(&digest_task.task_id)
    .bind(&managed.managed_mcp_id)
    .execute(&pool)
    .await
    .unwrap();
    let sink_calls = (
        sink.get_calls.load(Ordering::SeqCst),
        sink.put_calls.load(Ordering::SeqCst),
        sink.replace_calls.load(Ordering::SeqCst),
        sink.set_enabled_calls.load(Ordering::SeqCst),
    );
    let extension_config_calls = transport.extension_config_calls.load(Ordering::SeqCst);
    TaskRunner::new(
        repository.clone(),
        clock.clone(),
        ports.clone(),
        "recovery-worker-digest".to_string(),
    )
    .recover_startup()
    .await
    .unwrap();
    let digest_failed = repository.get_task(&digest_task.task_id).await.unwrap();
    assert_eq!(digest_failed.status, TaskStatus::RecoveryRequired);
    assert_eq!(digest_failed.rollback_status, RollbackStatus::Incomplete);
    assert_eq!(
        (
            sink.get_calls.load(Ordering::SeqCst),
            sink.put_calls.load(Ordering::SeqCst),
            sink.replace_calls.load(Ordering::SeqCst),
            sink.set_enabled_calls.load(Ordering::SeqCst),
        ),
        sink_calls
    );
    assert_eq!(
        transport.extension_config_calls.load(Ordering::SeqCst),
        extension_config_calls
    );

    let metadata_compensation = restore(manifest_digest.clone());
    let metadata_task = stage_stale_projection_rollback(
        repository.as_ref(),
        &source_task,
        "task-same-digest-metadata-rollback",
        "same-digest-metadata-rollback",
        &metadata_compensation,
        18_000,
    )
    .await;
    sqlx::query(
        "UPDATE connection_projections SET projection_json = ?, projection_digest = ?, \
         manifest_digest = ?, owner_task_id = ? WHERE managed_mcp_id = ?",
    )
    .bind(serde_json::to_string(&original.projection).unwrap())
    .bind(&original.projection_digest)
    .bind("0".repeat(64))
    .bind(&metadata_task.task_id)
    .bind(&managed.managed_mcp_id)
    .execute(&pool)
    .await
    .unwrap();
    let sink_calls = (
        sink.get_calls.load(Ordering::SeqCst),
        sink.put_calls.load(Ordering::SeqCst),
        sink.replace_calls.load(Ordering::SeqCst),
        sink.set_enabled_calls.load(Ordering::SeqCst),
    );
    let extension_config_calls = transport.extension_config_calls.load(Ordering::SeqCst);
    TaskRunner::new(
        repository.clone(),
        clock.clone(),
        ports.clone(),
        "recovery-worker-same-digest-metadata".to_string(),
    )
    .recover_startup()
    .await
    .unwrap();
    let metadata_failed = repository.get_task(&metadata_task.task_id).await.unwrap();
    assert_eq!(metadata_failed.status, TaskStatus::RecoveryRequired);
    assert_eq!(metadata_failed.rollback_status, RollbackStatus::Incomplete);
    assert_eq!(
        (
            sink.get_calls.load(Ordering::SeqCst),
            sink.put_calls.load(Ordering::SeqCst),
            sink.replace_calls.load(Ordering::SeqCst),
            sink.set_enabled_calls.load(Ordering::SeqCst),
        ),
        sink_calls
    );
    assert_eq!(
        transport.extension_config_calls.load(Ordering::SeqCst),
        extension_config_calls
    );

    let none_compensation = restore(manifest_digest);
    let none_task = stage_stale_projection_rollback(
        repository.as_ref(),
        &source_task,
        "task-none-auth-rollback",
        "none-auth-rollback",
        &none_compensation,
        20_000,
    )
    .await;
    sqlx::query(
        "UPDATE connection_projections SET projection_json = ?, projection_digest = ?, \
         manifest_digest = ?, owner_task_id = ? WHERE managed_mcp_id = ?",
    )
    .bind(serde_json::to_string(&original.projection).unwrap())
    .bind(&original.projection_digest)
    .bind(original.manifest_digest.as_deref())
    .bind(&none_task.task_id)
    .bind(&managed.managed_mcp_id)
    .execute(&pool)
    .await
    .unwrap();
    sink.entries.lock().unwrap().insert(
        original.link_key.clone(),
        ExtensionEntry {
            enabled: false,
            config: current_config,
        },
    );
    let sink_calls = (
        sink.get_calls.load(Ordering::SeqCst),
        sink.put_calls.load(Ordering::SeqCst),
        sink.replace_calls.load(Ordering::SeqCst),
        sink.set_enabled_calls.load(Ordering::SeqCst),
    );
    let extension_config_calls = transport.extension_config_calls.load(Ordering::SeqCst);
    TaskRunner::new(
        repository.clone(),
        clock.clone(),
        ports.clone(),
        "recovery-worker-none".to_string(),
    )
    .recover_startup()
    .await
    .unwrap();

    let restored_task = repository.get_task(&none_task.task_id).await.unwrap();
    assert_eq!(restored_task.status, TaskStatus::Failed);
    assert_eq!(restored_task.rollback_status, RollbackStatus::Complete);
    assert_eq!(sink.get_calls.load(Ordering::SeqCst), sink_calls.0 + 1);
    assert_eq!(sink.put_calls.load(Ordering::SeqCst), sink_calls.1);
    assert_eq!(sink.replace_calls.load(Ordering::SeqCst), sink_calls.2 + 1);
    assert_eq!(
        sink.set_enabled_calls.load(Ordering::SeqCst),
        sink_calls.3 + 1
    );
    assert_eq!(
        transport.extension_config_calls.load(Ordering::SeqCst),
        extension_config_calls + 1
    );
    let restored = repository
        .get_connection_projection(&managed.managed_mcp_id)
        .await
        .unwrap();
    assert_eq!(restored.projection, restored_projection);
    assert_eq!(restored.projection_digest, restored_digest);
    assert_eq!(restored.owner_task_id, original.owner_task_id);
    let expected_restored_config = CoreTransportProjectionAdapter
        .extension_config(&restored_projection, &original.link_key)
        .unwrap();
    let restored_sink_entry = sink.entries.lock().unwrap()[&original.link_key].clone();
    assert!(restored_sink_entry.enabled);
    assert_eq!(restored_sink_entry.config, expected_restored_config);
    let none_step = repository
        .list_task_steps(&none_task.task_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        none_step.compensation_status,
        lumina::mcp_platform::CompensationStatus::Committed
    );

    let tampered_task = stage_stale_projection_rollback(
        repository.as_ref(),
        &source_task,
        "task-compensation-binding-tamper",
        "compensation-binding-tamper",
        &none_compensation,
        25_000,
    )
    .await;
    sqlx::query("UPDATE task_steps SET compensation_json=? WHERE task_id=? AND ordinal=8")
        .bind(serde_json::to_string(&authenticated_compensation).unwrap())
        .bind(&tampered_task.task_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        repository
            .list_task_steps(&tampered_task.task_id)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
    let sink_calls = (
        sink.get_calls.load(Ordering::SeqCst),
        sink.put_calls.load(Ordering::SeqCst),
        sink.replace_calls.load(Ordering::SeqCst),
        sink.set_enabled_calls.load(Ordering::SeqCst),
    );
    clock.0.store(100_000, Ordering::SeqCst);
    TaskRunner::new(
        repository.clone(),
        clock,
        ports,
        "recovery-worker-binding-tamper".to_string(),
    )
    .recover_startup()
    .await
    .unwrap();
    assert_eq!(
        repository
            .get_task(&tampered_task.task_id)
            .await
            .unwrap()
            .status,
        TaskStatus::RecoveryRequired
    );
    assert_eq!(
        (
            sink.get_calls.load(Ordering::SeqCst),
            sink.put_calls.load(Ordering::SeqCst),
            sink.replace_calls.load(Ordering::SeqCst),
            sink.set_enabled_calls.load(Ordering::SeqCst),
        ),
        sink_calls
    );
    pool.close().await;
}

#[test]
fn legacy_none_auth_is_stripped_and_legacy_credentials_are_rejected() {
    let legacy_none = serde_json::json!({
        "type":"remote_http",
        "name":"legacy",
        "description":"",
        "uri":"https://mcp.example.com/",
        "timeout_seconds":1,
        "auth":{"type":"none"}
    });
    let projection: ConnectionProjection = serde_json::from_value(legacy_none).unwrap();
    let serialized = serde_json::to_string(&projection).unwrap();
    assert!(!serialized.contains("auth"));

    let legacy_credential = serde_json::json!({
        "type":"remote_http",
        "name":"legacy",
        "description":"",
        "uri":"https://mcp.example.com/",
        "timeout_seconds":1,
        "auth":{
            "type":"api_key_header",
            "header_name":"Authorization",
            "prefix":"Bearer",
            "credential_name":"opaque-canary-handle"
        }
    });
    let error = serde_json::from_value::<ConnectionProjection>(legacy_credential).unwrap_err();
    assert!(!error.to_string().contains("opaque-canary-handle"));
}

#[tokio::test]
async fn legacy_plan_digest_accepts_only_none_auth_and_strips_projection_metadata() {
    let gate = Arc::new(RecordingGate::default());
    let (_directory, repository, service) = service_with_gate(gate).await;
    let review = service
        .manual_plan_create(
            &test_context("mcp-platform-4b"),
            ManualPlanCreateInput {
                connection: ManualConnectionInput::RemoteHttp {
                    endpoint: "https://mcp.example.com/".to_string(),
                    auth: ManualHttpAuth::None,
                },
                idempotency_key: "legacy-none-plan".to_string(),
            },
        )
        .await
        .unwrap();
    let record = repository.get_plan(&review.plan_id).await.unwrap();
    let mut legacy = serde_json::to_value(&record.plan).unwrap();
    legacy["connection_projection"]["auth"] = serde_json::json!({"type":"none"});
    refresh_plan_digest(&mut legacy);

    let loaded: InstallationPlan = serde_json::from_value(legacy.clone()).unwrap();
    let reserialized = serde_json::to_value(loaded).unwrap();
    assert!(reserialized["connection_projection"].get("auth").is_none());

    legacy["connection_projection"]["auth"] = serde_json::json!({
        "type":"api_key_header",
        "header_name":"Authorization",
        "prefix":"Bearer",
        "credential_name":"opaque-canary-handle"
    });
    refresh_plan_digest(&mut legacy);
    let error = serde_json::from_value::<InstallationPlan>(legacy).unwrap_err();
    assert!(!error.to_string().contains("opaque-canary-handle"));
}

#[tokio::test]
async fn redirects_are_rejected_and_generic_streamable_http_keeps_its_own_path() {
    let gate = Arc::new(RecordingGate::default());
    let (directory, repository, service) = service_with_gate(gate.clone()).await;
    let mut manifest: serde_json::Value = serde_json::from_str(REMOTE).unwrap();
    manifest["auth"] = serde_json::json!({"type":"none"});
    let verified = parse_manifest(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    let digest = verified.digest().to_string();
    repository
        .save_manifest(&ManifestRecord {
            verified,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: Default::default(),
            created_at_ms: 1,
        })
        .await
        .unwrap();
    let error = service
        .plan_create(
            &test_context("mcp-platform-4b"),
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest: digest,
                    installation_scope: lumina::mcp_platform::InstallationScope::User,
                },
                idempotency_key: "redirect-denied".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::UnsafeUrl);
    assert_eq!(gate.plans.load(Ordering::SeqCst), 0);

    let manager = Arc::new(
        ExtensionManager::new_without_provider_with_managed_remote_http_policy(
            directory.path().to_path_buf(),
            gate.clone(),
        ),
    );
    let legacy_managed = ExtensionConfig::StreamableHttp {
        name: "managed_mcp_legacy_remote".to_string(),
        description: String::new(),
        uri: "https://mcp.example.com/${OPAQUE_CANARY}".to_string(),
        envs: Envs::default(),
        env_keys: vec!["OPAQUE_CANARY".to_string()],
        headers: HashMap::from([(
            "Authorization".to_string(),
            "Bearer ${OPAQUE_CANARY}".to_string(),
        )]),
        timeout: Some(1),
        socket: None,
        bundled: None,
        available_tools: Vec::new(),
    };
    let error = manager
        .add_extension(legacy_managed, None, None, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("requires replanning or repair"));
    assert!(!error.to_string().contains("OPAQUE_CANARY"));
    assert_eq!(gate.connections.load(Ordering::SeqCst), 0);

    let generic = ExtensionConfig::StreamableHttp {
        name: "generic-user-config".to_string(),
        description: String::new(),
        uri: "http://localhost:1".to_string(),
        envs: Envs::default(),
        env_keys: Vec::new(),
        headers: HashMap::from([("bad header name".to_string(), "value".to_string())]),
        timeout: Some(1),
        socket: None,
        bundled: None,
        available_tools: Vec::new(),
    };
    assert!(manager
        .add_extension(generic, None, None, None)
        .await
        .is_err());
    assert_eq!(gate.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn repair_projection_replaces_legacy_generic_managed_http_as_disabled() {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.yaml");
    let secrets_path = directory.path().join("secrets.yaml");
    let key = "managed_mcp_legacy_remote";
    std::fs::write(
        &config_path,
        format!(
            r#"
extensions:
  {key}:
    enabled: true
    type: streamable_http
    name: {key}
    description: legacy managed remote
    uri: https://mcp.example.com/
    env_keys: [OPAQUE_CANARY]
    headers:
      Authorization: Bearer ${{OPAQUE_CANARY}}
"#
        ),
    )
    .unwrap();
    std::fs::write(&secrets_path, "").unwrap();
    let config = Arc::new(Config::new_with_file_secrets(&config_path, &secrets_path).unwrap());
    assert!(get_enabled_extensions_with_config(&config)
        .into_iter()
        .all(|extension| extension.key() != key));
    let sink = ConfigProjectionSink::with_config(config.clone());
    let replacement = ExtensionConfig::ManagedStreamableHttp {
        name: key.to_string(),
        description: "managed remote".to_string(),
        uri: "https://mcp.example.com/".to_string(),
        timeout: Some(1),
        bundled: None,
        available_tools: Vec::new(),
    };

    let legacy = sink.get(key).await.unwrap().unwrap();
    assert!(legacy.entry.enabled);
    assert!(matches!(
        legacy.entry.config,
        ExtensionConfig::StreamableHttp { ref name, .. } if name == key
    ));

    let snapshot = sink.put_disabled(key, replacement.clone()).await.unwrap();
    assert!(!snapshot.created);
    assert!(!snapshot.entry.enabled);
    assert_eq!(snapshot.entry.config, replacement);
    assert!(get_enabled_extensions_with_config(&config)
        .into_iter()
        .all(|extension| extension.key() != key));

    let stored = std::fs::read_to_string(&config_path).unwrap();
    assert!(!stored.contains("OPAQUE_CANARY"));
    assert!(!stored.contains("Authorization"));
    let yaml: serde_yaml::Value = serde_yaml::from_str(&stored).unwrap();
    assert_eq!(
        yaml["extensions"][key]["type"].as_str(),
        Some("managed_streamable_http")
    );
    assert_eq!(yaml["extensions"][key]["enabled"].as_bool(), Some(false));
}

#[test]
fn production_source_pins_dns_and_has_no_bypass_or_generic_projection() {
    let connector = include_str!("managed_remote.rs");
    let health = include_str!("health.rs");
    let runtime = include_str!("../agents/extension_manager.rs");
    let projection = include_str!("plan.rs");
    let application = include_str!("service/application.rs");

    for required in [
        ".resolve_to_addrs(&endpoint.host, peers)",
        ".redirect(reqwest::redirect::Policy::none())",
        ".no_proxy()",
        ".https_only(true)",
    ] {
        assert!(connector.contains(required), "missing {required}");
    }
    for forbidden in [
        "danger_accept_invalid_certs",
        "danger_accept_invalid_hostnames",
        "tls_built_in_root_certs(false)",
    ] {
        assert!(!connector.contains(forbidden), "found {forbidden}");
    }
    assert!(health.contains(".secure_client("));
    assert!(health.contains("managed.client()"));
    assert!(runtime.contains("ExtensionConfig::ManagedStreamableHttp"));
    assert!(runtime.contains(".secure_client(uri, timeout_duration)"));
    assert!(runtime.contains("StreamableHttpClientTransport::with_client("));
    assert!(runtime.contains("managed.client()"));
    let legacy_guard = runtime
        .find("legacy managed remote HTTP configuration requires replanning or repair")
        .unwrap();
    let secret_resolution = runtime.find("let resolved_config =").unwrap();
    assert!(legacy_guard < secret_resolution);
    assert!(projection.contains("ExtensionConfig::ManagedStreamableHttp"));
    assert!(!projection.contains("remote_auth_projection"));
    assert!(!application.contains("pub fn with_remote_http_network_policy"));
    assert!(application.contains("lifecycle_ports(remote_http.clone())"));
}
