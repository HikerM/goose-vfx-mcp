use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use goose::agents::ExtensionConfig;
use goose::config::extensions::ExtensionEntry;
use goose::mcp_platform::adapters::plan_for_manifest;
use goose::mcp_platform::manifest::{Architecture, Platform};
use goose::mcp_platform::{
    parse_manifest, ArtifactSignatureStatus, ArtifactVerificationEvidence, AuthRequirement,
    AuthRequirementResolver, Clock, ConnectionProjection, CoreTransportProjectionAdapter,
    Distribution, DistributionEffectAdapter, EmptyHostIntegrationAdapter,
    ExternalManagedAcquisition, HealthAdapterResult, HealthCheckAdapter, HealthDetailCode,
    HealthExecution, HealthResultCode, IdGenerator, InstallConfirmInput, LifecyclePorts,
    ManagedGetInput, ManagedInstallEffect, ManagedInstallOutcome, ManifestProof, ManifestRecord,
    McpPlatformError, McpPlatformErrorCode, McpPlatformResult, McpPlatformService,
    McpPlatformServiceOptions, PlanCreateInput, PlanIntent, PlanOperation, PolicyContext,
    ProjectionSink, ProjectionSnapshot, RedactedErrorCode, RegistrationEffect,
    RegistrationEffectAdapter, RegistrationEffectEvidence, RuntimeCapabilitySnapshot,
    SqliteMcpPlatformRepository, SupplyChainEvidence, TaskOperation, TaskStatus, TrustTier,
    UserDecision, VerificationResult,
};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio_util::sync::CancellationToken;

const MANUAL: &str =
    include_str!("../../../documentation/static/schemas/examples/manual-stdio.json");
const LEGACY_V1_SCHEMA: &[&str] = &[
    "CREATE TABLE manifest_blobs (manifest_digest TEXT PRIMARY KEY CHECK(length(manifest_digest)=64), mcp_id TEXT NOT NULL, version TEXT NOT NULL, canonical_bytes BLOB NOT NULL, proof_json TEXT NOT NULL, trust_tier_json TEXT NOT NULL, created_at_ms INTEGER NOT NULL, UNIQUE(mcp_id,version))",
    "CREATE TABLE managed_mcps (managed_mcp_id TEXT PRIMARY KEY, mcp_id TEXT NOT NULL, installation_scope TEXT NOT NULL, state_json TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 0 CHECK(revision>=0), created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL, UNIQUE(mcp_id,installation_scope))",
    "CREATE TABLE managed_versions (managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT, version TEXT NOT NULL, manifest_digest TEXT NOT NULL REFERENCES manifest_blobs(manifest_digest) ON DELETE RESTRICT, installation_root TEXT, verified INTEGER NOT NULL CHECK(verified IN (0,1)), active INTEGER NOT NULL CHECK(active IN (0,1)), adapter_evidence_json TEXT, created_at_ms INTEGER NOT NULL, PRIMARY KEY(managed_mcp_id,version))",
    "CREATE UNIQUE INDEX managed_versions_one_active ON managed_versions(managed_mcp_id) WHERE active=1",
    "CREATE TABLE connection_projections (managed_mcp_id TEXT PRIMARY KEY REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT, link_key TEXT NOT NULL UNIQUE, projection_json TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 0 CHECK(revision>=0), updated_at_ms INTEGER NOT NULL)",
    "CREATE TABLE install_plans (plan_id TEXT PRIMARY KEY, plan_digest TEXT NOT NULL CHECK(length(plan_digest)=64), envelope_digest TEXT NOT NULL CHECK(length(envelope_digest)=64), manifest_digest TEXT NOT NULL REFERENCES manifest_blobs(manifest_digest) ON DELETE RESTRICT, operation TEXT NOT NULL CHECK(operation IN ('register','install','update','repair','uninstall','health')), target_json TEXT NOT NULL, plan_json TEXT NOT NULL, policy_evidence_json TEXT NOT NULL, confirmation_evidence_json TEXT NOT NULL, expires_at_ms INTEGER NOT NULL, idempotency_key TEXT NOT NULL UNIQUE, actor TEXT NOT NULL, created_at_ms INTEGER NOT NULL, UNIQUE(plan_id,plan_digest))",
    "CREATE TABLE tasks (task_id TEXT PRIMARY KEY, plan_id TEXT NOT NULL, plan_digest TEXT NOT NULL, operation TEXT NOT NULL CHECK(operation IN ('register','install','update','repair','uninstall','health')), idempotency_key TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('planned','awaiting_confirmation','queued','running','cancelling','verifying','activating','rolling_back','succeeded','failed','cancelled','interrupted','recovery_required')), actor TEXT NOT NULL, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL, heartbeat_at_ms INTEGER, progress INTEGER NOT NULL DEFAULT 0 CHECK(progress BETWEEN 0 AND 100), step_cursor INTEGER NOT NULL DEFAULT 0 CHECK(step_cursor>=0), adapter_evidence_json TEXT, redacted_error_json TEXT, rollback_status TEXT NOT NULL DEFAULT 'not_required' CHECK(rollback_status IN ('not_required','pending','in_progress','complete','incomplete')), rollback_evidence_json TEXT, revision INTEGER NOT NULL DEFAULT 0 CHECK(revision>=0), event_sequence INTEGER NOT NULL DEFAULT 0 CHECK(event_sequence>=0), retry_idempotency_key TEXT, FOREIGN KEY(plan_id,plan_digest) REFERENCES install_plans(plan_id,plan_digest) ON DELETE RESTRICT, UNIQUE(operation,idempotency_key))",
    "CREATE TABLE task_steps (task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT, ordinal INTEGER NOT NULL CHECK(ordinal>=0), status TEXT NOT NULL CHECK(status IN ('not_started','started','committed')), idempotency_token TEXT NOT NULL, compensation_json TEXT NOT NULL, evidence_json TEXT, started_at_ms INTEGER, committed_at_ms INTEGER, PRIMARY KEY(task_id,ordinal), UNIQUE(task_id,idempotency_token))",
    "CREATE TABLE audit_events (event_id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT, sequence INTEGER NOT NULL CHECK(sequence>0), event_type TEXT NOT NULL CHECK(event_type IN ('task_created','task_status_changed','step_started','step_committed','recovery_interrupted','confirmation_recorded','cancellation_requested','retry_queued')), actor TEXT NOT NULL, occurred_at_ms INTEGER NOT NULL, payload_json TEXT NOT NULL, redacted_error_json TEXT, UNIQUE(task_id,sequence))",
];

fn external_manifest(distribution: Value) -> Vec<u8> {
    let mut value: Value = serde_json::from_str(MANUAL).unwrap();
    value["id"] = json!("local.example.external");
    value["host_integrations"] = json!([]);
    value["permissions"] = json!([
        {"id":"spawn-server","kind":"process_spawn","reason":"Start the verified MCP server.","required":true},
        {"id":"docker-runtime","kind":"docker","reason":"Use the server-owned Docker capability.","required":true},
        {"id":"network","kind":"network","reason":"Fetch the immutable external artifact.","required":true},
        {"id":"workspace-read","kind":"filesystem_read","reason":"Read an opaque filesystem grant.","required":true}
    ]);
    value["distribution"] = distribution;
    serde_json::to_vec(&value).unwrap()
}

fn context(trust: TrustTier, development_mode: bool) -> PolicyContext {
    PolicyContext::new(trust, PlanOperation::Install)
        .with_target(Platform::Windows, Architecture::X86_64)
        .with_external_capabilities(true, true, development_mode)
}

fn docker(image: &str, entrypoint: &str) -> Value {
    json!({
        "type":"docker",
        "image":image,
        "digest":{"algorithm":"sha256","value":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
        "entrypoint":{"executable":entrypoint,"args":["--stdio"],"environment_keys":[]},
        "mounts":[]
    })
}

fn git_dev(repository: &str, commit: &str) -> Value {
    json!({
        "type":"git_dev",
        "repository":repository,
        "commit":commit,
        "adapter":"npm",
        "entrypoint":{"executable":"${installation.bin}/server","args":[],"environment_keys":[]}
    })
}

#[test]
fn docker_manifest_only_accepts_registry_port_and_immutable_digest() {
    let verified = parse_manifest(&external_manifest(docker(
        "registry.example:5000/team/server",
        "/server",
    )))
    .unwrap();
    let plan = plan_for_manifest(&verified, &context(TrustTier::Official, false)).unwrap();
    assert_eq!(plan.adapter().id, "docker");
    assert!(format!("{:?}", plan.steps()).contains("AcquireDockerDistribution"));
}

#[test]
fn docker_rejects_tags_local_registry_outside_dev_and_relative_entrypoint() {
    for image in [
        "registry.example/team/server:latest",
        "registry.example/team/server:v1",
        "registry.example/team/server@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        assert_eq!(
            parse_manifest(&external_manifest(docker(image, "/server")))
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::InvalidManifest
        );
    }
    assert_eq!(
        parse_manifest(&external_manifest(docker(
            "registry.example/team/server",
            "server"
        )))
        .unwrap_err()
        .code(),
        McpPlatformErrorCode::InvalidManifest
    );
    let local = parse_manifest(&external_manifest(docker(
        "localhost:5000/team/server",
        "/server",
    )))
    .unwrap();
    assert_eq!(
        plan_for_manifest(&local, &context(TrustTier::Official, false))
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PolicyDenied
    );
    assert!(plan_for_manifest(&local, &context(TrustTier::Local, true)).is_ok());
}

#[test]
fn docker_environment_and_unresolved_mount_grants_fail_before_confirmation() {
    let mut environment = docker("registry.example/team/server", "/server");
    environment["entrypoint"]["environment_keys"] = json!(["DOCKER_HOST"]);
    assert_eq!(
        parse_manifest(&external_manifest(environment))
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::OperationNotSupported
    );

    let mut mounted = docker("registry.example/team/server", "/server");
    mounted["mounts"] = json!([{
        "source_permission":"workspace-read",
        "target":"/workspace",
        "read_only":true
    }]);
    let verified = parse_manifest(&external_manifest(mounted)).unwrap();
    assert_eq!(
        plan_for_manifest(&verified, &context(TrustTier::Official, false))
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::OperationNotSupported
    );
}

#[test]
fn git_dev_requires_local_development_mode_exact_commit_and_public_https_origin() {
    let exact = "0123456789abcdef0123456789abcdef01234567";
    let verified = parse_manifest(&external_manifest(git_dev(
        "https://git.example/repository/project.git",
        exact,
    )))
    .unwrap();
    assert_eq!(
        plan_for_manifest(&verified, &context(TrustTier::Local, false))
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::DevelopmentModeRequired
    );
    assert_eq!(
        plan_for_manifest(&verified, &context(TrustTier::Official, true))
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PolicyDenied
    );
    assert_eq!(
        plan_for_manifest(&verified, &context(TrustTier::Local, true))
            .unwrap()
            .adapter()
            .id,
        "git_dev"
    );

    for commit in [
        "HEAD",
        "01234567",
        "0123456789ABCDEF0123456789ABCDEF01234567",
    ] {
        assert_eq!(
            parse_manifest(&external_manifest(git_dev(
                "https://git.example/repository/project.git",
                commit
            )))
            .unwrap_err()
            .code(),
            McpPlatformErrorCode::CommitUnavailable
        );
    }
    for repository in [
        "https://git.example",
        "https://git.example/a//b",
        "https://git.example/a/../b",
        "https://localhost/repository.git",
        "https://127.0.0.1/repository.git",
        "ssh://git.example/repository.git",
        "https://user@git.example/repository.git",
    ] {
        assert_eq!(
            parse_manifest(&external_manifest(git_dev(repository, exact)))
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::GitOriginDenied,
            "{repository}"
        );
    }
}

#[test]
fn acp_and_production_sources_expose_no_raw_external_execution_entrypoint() {
    let public_module = include_str!("../src/mcp_platform/mod.rs");
    let acp = include_str!("../src/acp/server/mcp_platform.rs");
    let service = include_str!("../src/mcp_platform/service/application.rs");
    assert!(!public_module.contains("pub use external_distribution::*"));
    for source in [acp, service] {
        assert!(!source.contains("DirectProcessRequest"));
        assert!(!source.contains("DirectProcessRunner"));
        assert!(!source.contains("executeDocker"));
        assert!(!source.contains("runGit"));
        assert!(!source.contains("rawArgs"));
        assert!(!source.contains("rawEnv"));
        assert!(!source.contains("rawPath"));
    }
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
        format!("{prefix}_{:06}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

struct TestRegistration;
#[async_trait]
impl RegistrationEffectAdapter for TestRegistration {
    fn adapter_id(&self) -> &'static str {
        "connection_registration"
    }
    fn adapter_version(&self) -> &'static str {
        "1"
    }
    async fn verify(
        &self,
        _effect: &RegistrationEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<RegistrationEffectEvidence> {
        if cancellation.is_cancelled() {
            return Err(test_error(McpPlatformErrorCode::TaskNotCancellable));
        }
        Ok(RegistrationEffectEvidence {
            adapter_id: self.adapter_id().to_string(),
            adapter_version: self.adapter_version().to_string(),
        })
    }
}

struct TestAuth;
impl AuthRequirementResolver for TestAuth {
    fn requirement(
        &self,
        _auth: &goose::mcp_platform::manifest::Auth,
    ) -> McpPlatformResult<AuthRequirement> {
        Ok(AuthRequirement::Ready)
    }
}

struct TestHealth;
#[async_trait]
impl HealthCheckAdapter for TestHealth {
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
struct TestProjectionSink(Mutex<BTreeMap<String, ExtensionEntry>>);

#[async_trait]
impl ProjectionSink for TestProjectionSink {
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
        let entry = ExtensionEntry {
            enabled: false,
            config,
        };
        let mut values = self.0.lock().unwrap();
        match values.get(key) {
            Some(existing) if existing == &entry => Ok(ProjectionSnapshot {
                entry,
                created: false,
            }),
            Some(_) => Err(test_error(McpPlatformErrorCode::ProjectionConflict)),
            None => {
                values.insert(key.to_string(), entry.clone());
                Ok(ProjectionSnapshot {
                    entry,
                    created: true,
                })
            }
        }
    }
    async fn get(&self, key: &str) -> McpPlatformResult<Option<ProjectionSnapshot>> {
        Ok(self
            .0
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
        let mut values = self.0.lock().unwrap();
        let entry = values
            .get_mut(key)
            .ok_or_else(|| test_error(McpPlatformErrorCode::NotFound))?;
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
        let mut values = self.0.lock().unwrap();
        match values.get(key) {
            None => Ok(false),
            Some(value) if value == &expected.entry => {
                values.remove(key);
                Ok(true)
            }
            Some(_) => Err(test_error(McpPlatformErrorCode::ProjectionConflict)),
        }
    }
    async fn replace_owned_disabled(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
        config: ExtensionConfig,
    ) -> McpPlatformResult<ProjectionSnapshot> {
        self.replace_owned(key, expected, config, false).await
    }
    async fn replace_owned(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
        config: ExtensionConfig,
        enabled: bool,
    ) -> McpPlatformResult<ProjectionSnapshot> {
        let mut values = self.0.lock().unwrap();
        if values.get(key) != Some(&expected.entry) {
            return Err(test_error(McpPlatformErrorCode::ProjectionConflict));
        }
        let entry = ExtensionEntry { enabled, config };
        values.insert(key.to_string(), entry.clone());
        Ok(ProjectionSnapshot {
            entry,
            created: false,
        })
    }
}

#[derive(Default)]
struct TestDistribution {
    versions: Mutex<HashSet<(String, String)>>,
    active: Mutex<HashMap<String, String>>,
    quarantine: Mutex<HashMap<String, (String, String)>>,
}

impl TestDistribution {
    fn outcome(effect: &ManagedInstallEffect) -> ManagedInstallOutcome {
        let (projection, adapter_id, size_bytes, supply_chain_evidence) =
            match (&effect.manifest.distribution, &effect.external_acquisition) {
                (
                    Distribution::Docker {
                        image,
                        digest,
                        entrypoint,
                        ..
                    },
                    Some(ExternalManagedAcquisition::Docker {
                        mount_plan_digest, ..
                    }),
                ) => (
                    ConnectionProjection::ManagedDockerStdio {
                        name: effect.manifest.name.clone(),
                        description: effect.manifest.description.clone(),
                        executable: "server-owned-docker".to_string(),
                        args: vec![
                            "run".to_string(),
                            "--rm".to_string(),
                            "--interactive".to_string(),
                            "--read-only".to_string(),
                            "--cap-drop".to_string(),
                            "ALL".to_string(),
                            "--security-opt".to_string(),
                            "no-new-privileges".to_string(),
                            "--pids-limit".to_string(),
                            "256".to_string(),
                            "--label".to_string(),
                            format!("dev.block.goose.managed={}", effect.managed_mcp_id),
                            "--entrypoint".to_string(),
                            entrypoint.executable.clone(),
                            format!("{image}@sha256:{}", digest.value),
                        ],
                        cwd: None,
                        timeout_seconds: Some(10),
                    },
                    "docker",
                    0,
                    Some(SupplyChainEvidence::Docker {
                        image: image.clone(),
                        image_digest: digest.value.clone(),
                        adapter_version: "1".to_string(),
                        daemon_version: "25.0.3".to_string(),
                        rootless: true,
                        mount_plan_digest: mount_plan_digest.clone(),
                        created_at_ms: effect.now_ms,
                    }),
                ),
                (
                    Distribution::GitDev { commit, .. },
                    Some(ExternalManagedAcquisition::GitDev {
                        repository_origin, ..
                    }),
                ) => (
                    ConnectionProjection::ManagedStdio {
                        name: effect.manifest.name.clone(),
                        description: effect.manifest.description.clone(),
                        executable: "server-owned-node".to_string(),
                        args: vec![effect.manifest.version.as_str().to_string()],
                        environment_keys: Vec::new(),
                        cwd: None,
                        timeout_seconds: Some(10),
                    },
                    "git_dev",
                    1,
                    Some(SupplyChainEvidence::GitDev {
                        repository_origin: repository_origin.clone(),
                        commit: commit.clone(),
                        git_tree_id: "d".repeat(40),
                        materialized_tree_digest: "e".repeat(64),
                        adapter_version: "1".to_string(),
                        created_at_ms: effect.now_ms,
                    }),
                ),
                value => panic!("unexpected external effect: {value:?}"),
            };
        ManagedInstallOutcome {
            installation_root: PathBuf::from(format!(
                "managed/{}/{}",
                effect.managed_mcp_id,
                effect.manifest.version.as_str()
            )),
            projection,
            evidence: ArtifactVerificationEvidence {
                source_origin: effect.source_url.clone(),
                artifact_digest: effect.expected_sha256.clone(),
                size_bytes,
                adapter_id: adapter_id.to_string(),
                adapter_version: "1".to_string(),
                platform_selector: effect.platform_selector.clone(),
                verification_result: VerificationResult::Verified,
                installed_at_ms: effect.now_ms,
                artifact_signature: ArtifactSignatureStatus::NotDeclaredByManifestV1,
            },
            materialized_tree_digest: "9".repeat(64),
            owned_relative_paths: vec![".".to_string()],
            replaced_quarantine_token: (effect.operation == TaskOperation::Repair).then(|| {
                format!(
                    "{}-{}-{}-repair",
                    effect.managed_mcp_id,
                    effect.manifest.version.as_str(),
                    effect.task_id
                )
            }),
            supply_chain_evidence,
        }
    }
}

#[async_trait]
impl DistributionEffectAdapter for TestDistribution {
    fn adapter_version(&self) -> &'static str {
        "1"
    }
    async fn install(
        &self,
        effect: &ManagedInstallEffect,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        self.versions.lock().unwrap().insert((
            effect.managed_mcp_id.clone(),
            effect.manifest.version.as_str().to_string(),
        ));
        Ok(Self::outcome(effect))
    }
    async fn inspect_installed(
        &self,
        effect: &ManagedInstallEffect,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        Ok(Self::outcome(effect))
    }
    async fn version_exists(&self, managed_mcp_id: &str, version: &str) -> McpPlatformResult<bool> {
        Ok(self
            .versions
            .lock()
            .unwrap()
            .contains(&(managed_mcp_id.to_string(), version.to_string())))
    }
    async fn activate_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        _task_id: &str,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        self.active
            .lock()
            .unwrap()
            .insert(managed_mcp_id.to_string(), version.to_string());
        Ok(())
    }
    async fn restore_activation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        _task_id: &str,
    ) -> McpPlatformResult<()> {
        let mut active = self.active.lock().unwrap();
        if let Some(version) = previous_version {
            active.insert(managed_mcp_id.to_string(), version.to_string());
        } else {
            active.remove(managed_mcp_id);
        }
        Ok(())
    }
    async fn active_version(&self, managed_mcp_id: &str) -> McpPlatformResult<Option<String>> {
        Ok(self.active.lock().unwrap().get(managed_mcp_id).cloned())
    }
    async fn remove_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        self.versions
            .lock()
            .unwrap()
            .remove(&(managed_mcp_id.to_string(), version.to_string()));
        Ok(())
    }
    async fn quarantine_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<String> {
        let token = format!("{managed_mcp_id}-{version}-{task_id}");
        self.versions
            .lock()
            .unwrap()
            .remove(&(managed_mcp_id.to_string(), version.to_string()));
        self.quarantine.lock().unwrap().insert(
            token.clone(),
            (managed_mcp_id.to_string(), version.to_string()),
        );
        Ok(token)
    }
    async fn restore_quarantined(
        &self,
        _managed_mcp_id: &str,
        _version: &str,
        token: &str,
    ) -> McpPlatformResult<()> {
        if let Some(version) = self.quarantine.lock().unwrap().remove(token) {
            self.versions.lock().unwrap().insert(version);
        }
        Ok(())
    }
    async fn purge_quarantined(
        &self,
        token: &str,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        self.quarantine.lock().unwrap().remove(token);
        Ok(())
    }
}

fn test_error(code: McpPlatformErrorCode) -> McpPlatformError {
    McpPlatformError::new(code, "typed 3C test failure")
}

struct LifecycleHarness {
    _directory: tempfile::TempDir,
    database_path: PathBuf,
    repository: Arc<SqliteMcpPlatformRepository>,
    clock: Arc<TestClock>,
    ids: Arc<TestIds>,
    ports: LifecyclePorts,
    distribution: Arc<TestDistribution>,
}

impl LifecycleHarness {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("platform.db");
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_path(&database_path)
                .await
                .unwrap(),
        );
        Self {
            _directory: directory,
            database_path,
            repository,
            clock: Arc::new(TestClock(AtomicI64::new(1_000))),
            ids: Arc::new(TestIds::default()),
            ports: LifecyclePorts {
                registration: Arc::new(TestRegistration),
                host_integration: Arc::new(EmptyHostIntegrationAdapter),
                transport: Arc::new(CoreTransportProjectionAdapter),
                auth: Arc::new(TestAuth),
                health: Arc::new(TestHealth),
                projection_sink: Arc::new(TestProjectionSink::default()),
            },
            distribution: Arc::new(TestDistribution::default()),
        }
    }

    fn service(&self, development_mode: bool) -> Arc<McpPlatformService> {
        self.service_with_policy(development_mode, true)
    }

    fn service_with_policy(
        &self,
        development_mode: bool,
        docker_daemon_policy_allowed: bool,
    ) -> Arc<McpPlatformService> {
        Arc::new(McpPlatformService::new_with_external_capabilities(
            self.repository.clone(),
            self.clock.clone(),
            self.ids.clone(),
            McpPlatformServiceOptions {
                compatibility_target: goose::mcp_platform::CompatibilityTarget {
                    platform: Platform::Windows,
                    arch: Architecture::X86_64,
                },
                plan_ttl_ms: 1_000_000,
                development_mode,
                docker_daemon_policy_allowed,
            },
            self.ports.clone(),
            self.distribution.clone(),
            RuntimeCapabilitySnapshot {
                node_available: true,
                python_major_minor: Some((3, 11)),
            },
            true,
            true,
        ))
    }

    async fn save(&self, distribution: Value, version: &str, trust_tier: TrustTier) -> String {
        let mut bytes: Value = serde_json::from_slice(&external_manifest(distribution)).unwrap();
        bytes["version"] = json!(version);
        let verified = parse_manifest(&serde_json::to_vec(&bytes).unwrap()).unwrap();
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
        service: &McpPlatformService,
        intent: PlanIntent,
        key: &str,
    ) -> (String, String) {
        let context = service.trusted_local_context();
        let plan = service
            .plan_create(
                &context,
                PlanCreateInput {
                    intent,
                    idempotency_key: format!("plan_{key}"),
                },
            )
            .await
            .unwrap();
        let managed = plan.target.managed_mcp_id.clone().unwrap();
        let task = service
            .install_confirm(
                &context,
                InstallConfirmInput {
                    plan_id: plan.plan_id,
                    plan_digest: plan.plan_digest,
                    decision: UserDecision::Confirm,
                    idempotency_key: format!("task_{key}"),
                },
            )
            .await
            .unwrap();
        (managed, task.task_id)
    }
}

#[tokio::test]
async fn docker_public_service_lifecycle_persists_evidence_and_reopens_v5() {
    let harness = LifecycleHarness::new().await;
    let service = harness.service(false);
    let v1 = harness
        .save(
            docker("registry.example/team/server", "/server"),
            "1.0.0",
            TrustTier::Official,
        )
        .await;
    let (managed, install_task) = harness
        .confirm(
            &service,
            PlanIntent::Install {
                manifest_digest: v1,
            },
            "docker_install",
        )
        .await;
    assert!(service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&install_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );
    let detail = service
        .managed_get(
            &service.trusted_local_context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap();
    assert!(!detail.summary.default_enabled);
    assert!(detail.summary.eligibility.repair);
    assert!(detail.summary.eligibility.uninstall);
    assert_eq!(
        detail.summary.external_capability,
        Some(goose::mcp_platform::service::ManagedExternalCapabilityStatus::DockerDaemonUnverified)
    );
    assert!(matches!(
        detail.supply_chain,
        Some(goose::mcp_platform::service::ManagedSupplyChainSummary::Docker {
            image_digest,
            ..
        }) if image_digest == "a".repeat(64)
    ));
    let policy_denied = harness.service_with_policy(false, false);
    let denied_detail = policy_denied
        .managed_get(
            &policy_denied.trusted_local_context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        denied_detail.summary.external_capability,
        Some(
            goose::mcp_platform::service::ManagedExternalCapabilityStatus::DockerDaemonPolicyDenied
        )
    );
    assert_eq!(
        policy_denied
            .plan_create(
                &policy_denied.trusted_local_context(),
                PlanCreateInput {
                    intent: PlanIntent::Repair {
                        managed_mcp_id: managed.clone(),
                    },
                    idempotency_key: "plan_docker_policy_denied".to_string(),
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::DaemonPolicyDenied
    );

    let (_, repair_task) = harness
        .confirm(
            &service,
            PlanIntent::Repair {
                managed_mcp_id: managed.clone(),
            },
            "docker_repair",
        )
        .await;
    assert!(service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&repair_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );

    let mut next = docker("registry.example/team/server", "/server");
    next["digest"]["value"] = json!("b".repeat(64));
    harness.save(next, "1.1.0", TrustTier::Official).await;
    let (_, update_task) = harness
        .confirm(
            &service,
            PlanIntent::Update {
                managed_mcp_id: managed.clone(),
                target_version: "1.1.0".to_string(),
            },
            "docker_update",
        )
        .await;
    assert!(service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&update_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );

    drop(service);
    let reopened = Arc::new(
        SqliteMcpPlatformRepository::open_path(&harness.database_path)
            .await
            .unwrap(),
    );
    let inventory = reopened.get_managed_inventory(&managed).await.unwrap();
    assert!(matches!(
        inventory.managed.versions[0].supply_chain_evidence,
        Some(SupplyChainEvidence::Docker { ref image_digest, .. }) if image_digest == &"b".repeat(64)
    ));
    let versions = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&harness.database_path))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT MAX(version) FROM schema_version")
            .fetch_one(&versions)
            .await
            .unwrap(),
        5
    );
    versions.close().await;

    let reopened_service = Arc::new(McpPlatformService::new_with_external_capabilities(
        reopened.clone(),
        harness.clock.clone(),
        harness.ids.clone(),
        McpPlatformServiceOptions {
            compatibility_target: goose::mcp_platform::CompatibilityTarget {
                platform: Platform::Windows,
                arch: Architecture::X86_64,
            },
            plan_ttl_ms: 1_000_000,
            development_mode: false,
            docker_daemon_policy_allowed: true,
        },
        harness.ports.clone(),
        harness.distribution.clone(),
        RuntimeCapabilitySnapshot {
            node_available: true,
            python_major_minor: Some((3, 11)),
        },
        true,
        true,
    ));
    let (_, uninstall_task) = harness
        .confirm(
            &reopened_service,
            PlanIntent::Uninstall {
                managed_mcp_id: managed.clone(),
                preserve_user_data: true,
            },
            "docker_uninstall",
        )
        .await;
    assert!(reopened_service.runner_tick().await.unwrap());
    assert_eq!(
        reopened.get_task(&uninstall_task).await.unwrap().status,
        TaskStatus::Succeeded
    );
    assert_eq!(
        reopened
            .get_managed_inventory(&managed)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::NotFound
    );
}

#[tokio::test]
async fn git_dev_mode_off_blocks_mutation_but_allows_owned_uninstall() {
    let harness = LifecycleHarness::new().await;
    let enabled = harness.service(true);
    let digest = harness
        .save(
            git_dev(
                "https://git.example/team/server.git",
                "0123456789abcdef0123456789abcdef01234567",
            ),
            "1.0.0",
            TrustTier::Local,
        )
        .await;
    let initially_disabled = harness.service(false);
    assert_eq!(
        initially_disabled
            .plan_create(
                &initially_disabled.trusted_local_context(),
                PlanCreateInput {
                    intent: PlanIntent::Install {
                        manifest_digest: digest.clone(),
                    },
                    idempotency_key: "plan_git_install_disabled".to_string(),
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::DevelopmentModeRequired
    );
    let (managed, _) = harness
        .confirm(
            &enabled,
            PlanIntent::Install {
                manifest_digest: digest,
            },
            "git_install",
        )
        .await;
    assert!(enabled.runner_tick().await.unwrap());
    let detail = enabled
        .managed_get(
            &enabled.trusted_local_context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        detail.supply_chain,
        Some(goose::mcp_platform::service::ManagedSupplyChainSummary::GitDev { ref commit, .. })
            if commit == "0123456789abcdef0123456789abcdef01234567"
    ));
    harness
        .save(
            git_dev(
                "https://git.example/team/server.git",
                "1123456789abcdef0123456789abcdef01234567",
            ),
            "1.1.0",
            TrustTier::Local,
        )
        .await;
    let disabled = harness.service(false);
    let update = disabled
        .plan_create(
            &disabled.trusted_local_context(),
            PlanCreateInput {
                intent: PlanIntent::Update {
                    managed_mcp_id: managed.clone(),
                    target_version: "1.1.0".to_string(),
                },
                idempotency_key: "plan_git_update_disabled".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(update.code(), McpPlatformErrorCode::DevelopmentModeRequired);
    let repair = disabled
        .plan_create(
            &disabled.trusted_local_context(),
            PlanCreateInput {
                intent: PlanIntent::Repair {
                    managed_mcp_id: managed.clone(),
                },
                idempotency_key: "plan_git_repair_disabled".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(repair.code(), McpPlatformErrorCode::DevelopmentModeRequired);
    let (_, uninstall_task) = harness
        .confirm(
            &disabled,
            PlanIntent::Uninstall {
                managed_mcp_id: managed.clone(),
                preserve_user_data: true,
            },
            "git_uninstall_disabled",
        )
        .await;
    assert!(disabled.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&uninstall_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );
}

#[tokio::test]
async fn persisted_supply_chain_authority_mismatch_rejects_repair() {
    let harness = LifecycleHarness::new().await;
    let service = harness.service(false);
    let digest = harness
        .save(
            docker("registry.example/team/server", "/server"),
            "1.0.0",
            TrustTier::Official,
        )
        .await;
    let (managed, _) = harness
        .confirm(
            &service,
            PlanIntent::Install {
                manifest_digest: digest,
            },
            "authority_install",
        )
        .await;
    assert!(service.runner_tick().await.unwrap());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&harness.database_path))
        .await
        .unwrap();
    let original = sqlx::query_scalar::<_, String>(
        "SELECT supply_chain_evidence_json FROM managed_versions WHERE managed_mcp_id=? AND active=1",
    )
    .bind(&managed)
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut tampered: Value = serde_json::from_str(&original).unwrap();
    tampered["image_digest"] = json!("f".repeat(64));
    sqlx::query("UPDATE managed_versions SET supply_chain_evidence_json=? WHERE managed_mcp_id=? AND active=1")
        .bind(serde_json::to_string(&tampered).unwrap())
        .bind(&managed)
        .execute(&pool)
        .await
        .unwrap();
    let (_, repair_task) = harness
        .confirm(
            &service,
            PlanIntent::Repair {
                managed_mcp_id: managed.clone(),
            },
            "authority_repair",
        )
        .await;
    assert!(service.runner_tick().await.unwrap());
    let failed = harness.repository.get_task(&repair_task).await.unwrap();
    assert!(matches!(
        failed.status,
        TaskStatus::Failed | TaskStatus::RecoveryRequired
    ));
    assert_eq!(
        failed.redacted_error.as_ref().unwrap().code(),
        RedactedErrorCode::VerificationFailed
    );
    assert_eq!(
        harness
            .distribution
            .active_version(&managed)
            .await
            .unwrap()
            .as_deref(),
        Some("1.0.0")
    );
    assert_eq!(harness.distribution.versions.lock().unwrap().len(), 1);
    pool.close().await;
}

#[tokio::test]
async fn repository_rejects_unknown_newer_schema() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("future.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE schema_version(version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO schema_version VALUES (6, 0)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert_eq!(
        SqliteMcpPlatformRepository::open_path(path)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::SchemaTooNew
    );
}

#[tokio::test]
async fn legacy_v1_database_migrates_to_v5_and_reopens_with_evidence_column() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-v1.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE schema_version(version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for statement in LEGACY_V1_SCHEMA {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO schema_version VALUES (1, 0)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let repository = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
    drop(repository);
    let reopened = SqliteMcpPlatformRepository::open_path(&path).await.unwrap();
    drop(reopened);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&path))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT MAX(version) FROM schema_version")
            .fetch_one(&pool)
            .await
            .unwrap(),
        5
    );
    let columns = sqlx::query("PRAGMA table_info(managed_versions)")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| sqlx::Row::get::<String, _>(&row, "name"))
        .collect::<Vec<_>>();
    assert!(columns
        .iter()
        .any(|column| column == "supply_chain_evidence_json"));
}
