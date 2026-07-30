use crate as goose;
use crate::mcp_platform::task_runner::{StaticEnrollmentRuntimeBindingResolver, TaskRunner};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use goose::agents::ExtensionConfig;
use goose::config::extensions::ExtensionEntry;
use goose::mcp_platform::adapters::plan_for_manifest;
use goose::mcp_platform::manifest::{Architecture, ArchiveFormat, Platform};
use goose::mcp_platform::repository::AuditPayload;
use goose::mcp_platform::service::{Clock, IdGenerator};
use goose::mcp_platform::{
    observed_projection_digest, parse_manifest, validate_archive_entries, validate_archive_path,
    ArchiveEntryKind, ArchiveEntryMetadata, ArchiveInstaller, ArchiveLimits, ArtifactFetchEffect,
    ArtifactFetcher, ArtifactNetworkClient, ArtifactNetworkResponse, ArtifactSignatureStatus,
    ArtifactVerificationEvidence, AuthRequirement, AuthRequirementResolver, ConnectionProjection,
    CoreTransportProjectionAdapter, DistributionEffectAdapter, EmptyHostIntegrationAdapter,
    FetchedArtifact, HealthAdapterResult, HealthCheckAdapter, HealthDetailCode, HealthExecution,
    HealthResultCode, InstallConfirmInput, Installer, LifecyclePorts, ManagedGetInput,
    ManagedInstallEffect, ManagedInstallOutcome, ManagedStorageCapacity, ManifestProof,
    ManifestRecord, McpPlatformError, McpPlatformErrorCode, McpPlatformResult, McpPlatformService,
    McpPlatformServiceOptions, PlanCreateInput, PlanIntent, PlanOperation, PolicyContext,
    ProductionArtifactFetcher, ProductionDistributionEffectAdapter, ProjectionCommitPlan,
    ProjectionSink, ProjectionSinkAtomicProof, ProjectionSinkAtomicProofKind, ProjectionSnapshot,
    RegistrationEffect, RegistrationEffectAdapter, RegistrationEffectEvidence, RequestContext,
    RuntimeCapabilities, RuntimeCapabilitySnapshot, SetDefaultEnabledInput, Sha256Verifier,
    SqliteMcpPlatformRepository, TaskCancelInput, TaskOperation, TaskRetryInput, TaskStatus,
    TrustTier, UserDecision, VerificationResult,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::Notify;
use tokio_util::bytes::Bytes;
use tokio_util::sync::CancellationToken;

const MANUAL: &str =
    include_str!("../../../../documentation/static/schemas/examples/manual-stdio.json");
const NPM: &str =
    include_str!("../../../../documentation/static/schemas/examples/npm-package.json");
const HOUDINI: &str =
    include_str!("../../../../documentation/static/schemas/examples/binary-archive-houdini.json");

fn managed_manifest(distribution: Value, version: &str) -> Vec<u8> {
    let mut value: Value = serde_json::from_str(MANUAL).unwrap();
    value["id"] = json!("org.example.managed-fixture");
    value["version"] = json!(version);
    value["host_integrations"] = json!([]);
    value["distribution"] = distribution;
    serde_json::to_vec(&value).unwrap()
}

fn install_context() -> PolicyContext {
    PolicyContext::new(TrustTier::Official, PlanOperation::Install)
        .with_target(Platform::Windows, Architecture::X86_64)
        .with_runtime_capabilities(true, Some((3, 11)))
}

#[test]
fn manifest_only_npm_wheel_and_archive_select_generic_adapters() {
    let npm = parse_manifest(NPM.as_bytes()).unwrap();
    let npm_plan = plan_for_manifest(&npm, &install_context()).unwrap();
    assert_eq!(npm_plan.adapter().id, "npm");

    let wheel = managed_manifest(
        json!({
            "type": "python_wheel", "package": "managed-fixture", "package_version": "1.2.3",
            "python": ">=3.11", "artifacts": [{"platform":"any","arch":"any",
            "url":"https://packages.example.test/managed_fixture-1.2.3-py3-none-any.whl",
            "digest":{"algorithm":"sha256","value":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}],
            "entrypoint":{"executable":"${installation.bin}/managed-fixture"}
        }),
        "1.2.3",
    );
    assert_eq!(
        plan_for_manifest(&parse_manifest(&wheel).unwrap(), &install_context())
            .unwrap()
            .adapter()
            .id,
        "python_wheel"
    );

    let archive = managed_manifest(
        json!({
            "type":"binary_archive", "archive_format":"zip", "artifacts":[{"platform":"windows","arch":"x86_64",
            "url":"https://downloads.example.test/managed-fixture-1.2.3.zip",
            "digest":{"algorithm":"sha256","value":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}],
            "entrypoint":{"executable":"${installation.root}/bin/server.exe"}
        }),
        "1.2.3",
    );
    assert_eq!(
        plan_for_manifest(&parse_manifest(&archive).unwrap(), &install_context())
            .unwrap()
            .adapter()
            .id,
        "binary_archive"
    );
}

#[test]
fn phase_3b_rejects_host_integrations_before_managed_execution() {
    let verified = parse_manifest(HOUDINI.as_bytes()).unwrap();
    assert_eq!(
        plan_for_manifest(&verified, &install_context())
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::OperationNotSupported
    );
}

#[test]
fn npm_requires_exact_registry_origin_and_server_node_capability() {
    let verified = parse_manifest(NPM.as_bytes()).unwrap();
    let missing_node = PolicyContext::new(TrustTier::Official, PlanOperation::Install)
        .with_target(Platform::Windows, Architecture::X86_64);
    assert_eq!(
        plan_for_manifest(&verified, &missing_node)
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::AdapterIncompatible
    );

    let mut cross_origin: Value = serde_json::from_str(NPM).unwrap();
    cross_origin["distribution"]["registry"] = json!("https://registry.example.test");
    let error = plan_for_manifest(
        &parse_manifest(&serde_json::to_vec(&cross_origin).unwrap()).unwrap(),
        &install_context(),
    )
    .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::PolicyDenied);
}

#[test]
fn wheel_tag_gate_allows_universal_and_rejects_python_abi_and_platform_mismatch() {
    let wheel = |filename: &str| {
        managed_manifest(
            json!({
                "type":"python_wheel","package":"managed-fixture","package_version":"1.2.3","python":">=3.11",
                "artifacts":[{"platform":"any","arch":"any","url":format!("https://packages.example.test/{filename}"),
                "digest":{"algorithm":"sha256","value":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}}],
                "entrypoint":{"executable":"${installation.bin}/managed-fixture"}
            }),
            "1.2.3",
        )
    };
    assert!(plan_for_manifest(
        &parse_manifest(&wheel("managed_fixture-1.2.3-py3-none-any.whl")).unwrap(),
        &install_context()
    )
    .is_ok());
    for filename in [
        "managed_fixture-1.2.3-cp310-cp310-win_amd64.whl",
        "managed_fixture-1.2.3-cp311-cp310-win_amd64.whl",
        "managed_fixture-1.2.3-cp311-cp311-manylinux_2_28_x86_64.whl",
    ] {
        assert_eq!(
            plan_for_manifest(
                &parse_manifest(&wheel(filename)).unwrap(),
                &install_context()
            )
            .unwrap_err()
            .code(),
            McpPlatformErrorCode::AdapterIncompatible
        );
    }
    assert!(plan_for_manifest(
        &parse_manifest(&wheel("managed_fixture-1.2.3-cp311-cp311-win_amd64.whl")).unwrap(),
        &install_context()
    )
    .is_ok());
}

#[test]
fn artifact_urls_cannot_persist_query_or_fragment_secrets() {
    let mut value: Value = serde_json::from_str(NPM).unwrap();
    value["distribution"]["artifacts"][0]["url"] =
        json!("https://registry.npmjs.org/pkg.tgz?token=canary");
    assert_eq!(
        parse_manifest(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::UnsafeUrl
    );
    value["distribution"]["artifacts"][0]["url"] =
        json!("https://registry.npmjs.org/pkg.tgz#canary");
    assert_eq!(
        parse_manifest(&serde_json::to_vec(&value).unwrap())
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::UnsafeUrl
    );
}

#[test]
fn archive_raw_names_are_rejected_before_strip_components() {
    for value in [
        "/abs/path",
        "C:/drive/path",
        "\\\\server\\share\\file",
        "../evil",
        "prefix/../evil",
        "a//b",
        "a\\..\\evil",
        "./file",
        "prefix//file",
        "CON",
        "con.txt",
        "NUL.json",
        "COM1",
        "lpt9.log",
        "dir/name.",
        "dir/name ",
    ] {
        assert_eq!(
            validate_archive_path(value).unwrap_err().code(),
            McpPlatformErrorCode::PathTraversal,
            "{value}"
        );
    }
    assert!(validate_archive_path("prefix/bin/server").is_ok());
}

#[test]
fn archive_rejects_links_duplicates_case_collisions_and_bombs() {
    let limits = ArchiveLimits {
        maximum_files: 2,
        maximum_single_file_bytes: 100,
        maximum_total_bytes: 150,
        maximum_compression_ratio: 10,
    };
    let entry = |path: &str, kind, compressed, expanded| ArchiveEntryMetadata {
        path: path.to_string(),
        kind,
        compressed_size: compressed,
        expanded_size: expanded,
    };
    for entries in [
        vec![entry("a", ArchiveEntryKind::Symlink, 1, 1)],
        vec![entry("a", ArchiveEntryKind::Hardlink, 1, 1)],
        vec![entry("a", ArchiveEntryKind::ReparsePoint, 1, 1)],
        vec![
            entry("A", ArchiveEntryKind::File, 1, 1),
            entry("a", ArchiveEntryKind::File, 1, 1),
        ],
        vec![entry("a", ArchiveEntryKind::File, 1, 101)],
        vec![entry("a", ArchiveEntryKind::File, 1, 11)],
    ] {
        assert!(validate_archive_entries(&entries, limits).is_err());
    }
    let ratio_limits = ArchiveLimits {
        maximum_files: usize::MAX,
        maximum_single_file_bytes: u64::MAX,
        maximum_total_bytes: u64::MAX,
        maximum_compression_ratio: 200,
    };
    assert!(validate_archive_entries(
        &[entry("threshold", ArchiveEntryKind::File, 1, 200)],
        ratio_limits
    )
    .is_ok());
    assert!(validate_archive_entries(
        &[entry("over", ArchiveEntryKind::File, 1, 201)],
        ratio_limits
    )
    .is_err());
    assert!(validate_archive_entries(
        &[entry(
            "overflow",
            ArchiveEntryKind::File,
            u64::MAX,
            u64::MAX
        )],
        ratio_limits
    )
    .is_err());
}

#[tokio::test]
async fn database_migrates_forward_to_v4_inventory_objects() {
    let directory = tempfile::tempdir().unwrap();
    let repository = SqliteMcpPlatformRepository::open_path(directory.path().join("platform.db"))
        .await
        .unwrap();
    let diagnostics = repository.diagnostics().await.unwrap();
    for table in [
        "artifact_cache",
        "artifact_claims",
        "installation_ownership",
        "activation_journal",
        "uninstall_journal",
    ] {
        assert!(
            diagnostics
                .tables
                .iter()
                .any(|candidate| candidate == table),
            "{table}"
        );
    }
}

#[test]
fn production_boundaries_have_no_shell_or_generic_execution_escape_hatch() {
    for source in [
        include_str!("managed_distribution.rs"),
        include_str!("task_runner.rs"),
        include_str!("../acp/server/mcp_platform.rs"),
    ] {
        assert!(!source.contains("Command::new(\"sh\")"));
        assert!(!source.contains("Command::new(\"cmd\")"));
        assert!(!source.contains("execute_command"));
        assert!(!source.contains("arbitrary_command"));
    }
    let handler = include_str!("../acp/server/mcp_platform.rs");
    for forbidden in [
        "sqlx::",
        "McpPlatformRepository",
        "std::process",
        "tokio::process",
    ] {
        assert!(!handler.contains(forbidden), "{forbidden}");
    }
}

struct FixtureNetwork {
    responses: Mutex<std::collections::VecDeque<ArtifactNetworkResponse>>,
    opens: AtomicU64,
}

#[derive(Default)]
struct BlockingNetwork {
    opens: AtomicU64,
    entered: Notify,
}

#[async_trait]
impl ArtifactNetworkClient for BlockingNetwork {
    async fn open(
        &self,
        _effect: &ArtifactFetchEffect,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<ArtifactNetworkResponse> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        Ok(ArtifactNetworkResponse::new(
            200,
            None,
            Box::pin(stream::pending()),
        ))
    }
}

impl FixtureNetwork {
    fn new(responses: Vec<ArtifactNetworkResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            opens: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl ArtifactNetworkClient for FixtureNetwork {
    async fn open(
        &self,
        _effect: &ArtifactFetchEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ArtifactNetworkResponse> {
        if cancellation.is_cancelled() {
            return Err(test_error(McpPlatformErrorCode::TaskNotCancellable));
        }
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| test_error(McpPlatformErrorCode::RepositoryUnavailable))
    }
}

fn network_response(
    status: u16,
    content_length: Option<u64>,
    chunks: Vec<McpPlatformResult<Vec<u8>>>,
) -> ArtifactNetworkResponse {
    ArtifactNetworkResponse::new(
        status,
        content_length,
        Box::pin(stream::iter(
            chunks.into_iter().map(|chunk| chunk.map(Bytes::from)),
        )),
    )
}

fn fetch_effect(operation_id: &str, bytes: &[u8], maximum_size_bytes: u64) -> ArtifactFetchEffect {
    ArtifactFetchEffect {
        operation_id: operation_id.to_string(),
        source_url: "https://artifacts.example.test/package.bin".to_string(),
        redirect_policy: goose::mcp_platform::RedirectPolicy::DenyAll,
        expected_sha256: Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        expected_size_bytes: Some(bytes.len() as u64),
        maximum_size_bytes,
        timeout_seconds: 5,
    }
}

fn partial_files(path: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(path)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".partial"))
        .collect()
}

#[tokio::test]
async fn production_fetcher_streams_revalidates_cache_serializes_digest_and_cleans_failures() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("cache");
    let payload = b"verified managed artifact".to_vec();
    let network = Arc::new(FixtureNetwork::new(vec![
        network_response(
            200,
            Some(payload.len() as u64),
            vec![Ok(payload[..8].to_vec()), Ok(payload[8..].to_vec())],
        ),
        network_response(200, Some(payload.len() as u64), vec![Ok(payload.clone())]),
        network_response(
            200,
            None,
            vec![
                Ok(b"partial".to_vec()),
                Err(test_error(McpPlatformErrorCode::RepositoryUnavailable)),
            ],
        ),
        network_response(302, None, Vec::new()),
        network_response(200, Some(2048), Vec::new()),
        network_response(
            200,
            Some(payload.len() as u64),
            vec![Ok(vec![b'z'; payload.len()])],
        ),
    ]));
    let fetcher = Arc::new(
        ProductionArtifactFetcher::new_with_network(cache.clone(), network.clone()).unwrap(),
    );
    let effect = fetch_effect("fetch_one", &payload, 1024);
    let first_cancellation = CancellationToken::new();
    let second_cancellation = CancellationToken::new();
    let (first, second) = tokio::join!(
        fetcher.fetch(&effect, &first_cancellation),
        fetcher.fetch(&effect, &second_cancellation)
    );
    assert_eq!(first.unwrap().digest, effect.expected_sha256);
    assert_eq!(second.unwrap().digest, effect.expected_sha256);
    assert_eq!(network.opens.load(Ordering::SeqCst), 1);

    std::fs::write(cache.join(&effect.expected_sha256), b"corrupt cache").unwrap();
    let repaired = fetcher
        .fetch(
            &ArtifactFetchEffect {
                operation_id: "fetch_repair".to_string(),
                ..effect.clone()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(repaired.size_bytes, payload.len() as u64);
    assert_eq!(network.opens.load(Ordering::SeqCst), 2);

    std::fs::remove_file(cache.join(&effect.expected_sha256)).unwrap();
    let stream_error = fetcher
        .fetch(
            &ArtifactFetchEffect {
                operation_id: "fetch_stream_error".to_string(),
                ..effect.clone()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        stream_error.code(),
        McpPlatformErrorCode::RepositoryUnavailable
    );
    assert!(partial_files(&cache).is_empty());

    let redirect = fetcher
        .fetch(
            &ArtifactFetchEffect {
                operation_id: "fetch_redirect".to_string(),
                ..effect
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(redirect.code(), McpPlatformErrorCode::RepositoryUnavailable);
    assert!(partial_files(&cache).is_empty());

    let oversized = fetcher
        .fetch(
            &ArtifactFetchEffect {
                operation_id: "fetch_oversized".to_string(),
                ..fetch_effect("unused", &payload, 1024)
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(oversized.code(), McpPlatformErrorCode::PolicyDenied);
    assert!(partial_files(&cache).is_empty());

    let mismatch = fetcher
        .fetch(
            &ArtifactFetchEffect {
                operation_id: "fetch_mismatch".to_string(),
                ..fetch_effect("unused", &payload, 1024)
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(mismatch.code(), McpPlatformErrorCode::IntegrityError);
    assert!(partial_files(&cache).is_empty());
}

#[tokio::test]
async fn production_fetcher_denies_non_public_destinations_and_cache_links() {
    for host in [
        "127.0.0.1",
        "10.0.0.1",
        "169.254.169.254",
        "[::1]",
        "[fc00::1]",
        "[::ffff:127.0.0.1]",
        "[::ffff:169.254.169.254]",
        "[::ffff:10.0.0.1]",
        "[2001:db8::1]",
        "[3fff::1]",
        "[2001::1]",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let fetcher = ProductionArtifactFetcher::new(directory.path().to_path_buf()).unwrap();
        let mut effect = fetch_effect("private_host", b"x", 16);
        effect.source_url = format!("https://{host}/artifact");
        assert_eq!(
            fetcher
                .fetch(&effect, &CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PolicyDenied,
            "{host}"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let effect = fetch_effect("linked_cache", b"outside", 64);
        let outside = directory.path().join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, cache.join(&effect.expected_sha256)).unwrap();
        let fetcher = ProductionArtifactFetcher::new(cache).unwrap();
        assert_eq!(
            fetcher
                .fetch(&effect, &CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PolicyDenied
        );
    }
}

#[tokio::test]
async fn production_fetcher_cancels_digest_lock_waiters_without_opening_network() {
    let directory = tempfile::tempdir().unwrap();
    let network = Arc::new(BlockingNetwork::default());
    let fetcher = Arc::new(
        ProductionArtifactFetcher::new_with_network(
            directory.path().join("cache"),
            network.clone(),
        )
        .unwrap(),
    );
    let effect = fetch_effect("holder", b"payload", 1024);
    let holder_cancel = CancellationToken::new();
    let holder = {
        let fetcher = fetcher.clone();
        let effect = effect.clone();
        let cancellation = holder_cancel.clone();
        tokio::spawn(async move { fetcher.fetch(&effect, &cancellation).await })
    };
    network.entered.notified().await;
    let waiter_cancel = CancellationToken::new();
    let waiter = {
        let fetcher = fetcher.clone();
        let mut effect = effect.clone();
        effect.operation_id = "waiter".to_string();
        let cancellation = waiter_cancel.clone();
        tokio::spawn(async move { fetcher.fetch(&effect, &cancellation).await })
    };
    tokio::task::yield_now().await;
    waiter_cancel.cancel();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::TaskNotCancellable
    );
    assert_eq!(network.opens.load(Ordering::SeqCst), 1);
    holder_cancel.cancel();
    assert_eq!(
        holder.await.unwrap().unwrap_err().code(),
        McpPlatformErrorCode::TaskNotCancellable
    );
    assert!(partial_files(&directory.path().join("cache")).is_empty());
}

#[derive(Clone)]
struct StaticArtifactFetcher(FetchedArtifact);

#[async_trait]
impl ArtifactFetcher for StaticArtifactFetcher {
    async fn fetch(
        &self,
        effect: &ArtifactFetchEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<FetchedArtifact> {
        if cancellation.is_cancelled() {
            return Err(test_error(McpPlatformErrorCode::TaskNotCancellable));
        }
        if effect.expected_sha256 != self.0.digest
            || effect
                .expected_size_bytes
                .is_some_and(|size| size != self.0.size_bytes)
        {
            return Err(test_error(McpPlatformErrorCode::IntegrityError));
        }
        Ok(self.0.clone())
    }
}

struct BlockingProductionDistribution {
    inner: ProductionDistributionEffectAdapter,
    block_after_install_once: AtomicBool,
    install_calls: AtomicU64,
    installed: Notify,
    block_before_activate_once: AtomicBool,
    activate_entered: Notify,
}

#[async_trait]
impl DistributionEffectAdapter for BlockingProductionDistribution {
    fn adapter_version(&self) -> &'static str {
        self.inner.adapter_version()
    }

    async fn install(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        self.install_calls.fetch_add(1, Ordering::SeqCst);
        let outcome = self.inner.install(effect, cancellation).await?;
        if self.block_after_install_once.swap(false, Ordering::SeqCst) {
            self.installed.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(outcome)
    }

    async fn inspect_installed(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        self.inner.inspect_installed(effect, cancellation).await
    }

    async fn version_exists(&self, managed_mcp_id: &str, version: &str) -> McpPlatformResult<bool> {
        self.inner.version_exists(managed_mcp_id, version).await
    }

    async fn activate_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        if self
            .block_before_activate_once
            .swap(false, Ordering::SeqCst)
        {
            self.activate_entered.notify_one();
            std::future::pending::<()>().await;
        }
        self.inner
            .activate_version(managed_mcp_id, version, task_id, cancellation)
            .await
    }

    async fn restore_activation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        task_id: &str,
    ) -> McpPlatformResult<()> {
        self.inner
            .restore_activation(managed_mcp_id, previous_version, task_id)
            .await
    }

    async fn active_version(&self, managed_mcp_id: &str) -> McpPlatformResult<Option<String>> {
        self.inner.active_version(managed_mcp_id).await
    }

    async fn remove_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        self.inner
            .remove_version(managed_mcp_id, version, cancellation)
            .await
    }

    async fn quarantine_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<String> {
        self.inner
            .quarantine_version(managed_mcp_id, version, task_id, cancellation)
            .await
    }

    async fn restore_quarantined(
        &self,
        managed_mcp_id: &str,
        version: &str,
        token: &str,
    ) -> McpPlatformResult<()> {
        self.inner
            .restore_quarantined(managed_mcp_id, version, token)
            .await
    }

    async fn purge_quarantined(
        &self,
        token: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        self.inner.purge_quarantined(token, cancellation).await
    }
}

fn fixture_artifact(path: PathBuf) -> FetchedArtifact {
    let bytes = std::fs::read(&path).unwrap();
    FetchedArtifact::from_verified_cache_entry(
        path,
        Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        bytes.len() as u64,
    )
    .unwrap()
}

fn write_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for (name, contents) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(contents).unwrap();
    }
    writer.finish().unwrap();
}

fn write_targz(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, contents) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, *name, *contents).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

fn effect_for_manifest(
    manifest: goose::mcp_platform::Manifest,
    artifact: &FetchedArtifact,
    task_id: &str,
    managed_mcp_id: &str,
    selector: &str,
) -> ManagedInstallEffect {
    let source_url = match &manifest.distribution {
        goose::mcp_platform::Distribution::Npm { artifacts, .. }
        | goose::mcp_platform::Distribution::PythonWheel { artifacts, .. }
        | goose::mcp_platform::Distribution::BinaryArchive { artifacts, .. } => {
            artifacts[0].url.clone()
        }
        _ => unreachable!(),
    };
    ManagedInstallEffect {
        task_id: task_id.to_string(),
        managed_mcp_id: managed_mcp_id.to_string(),
        manifest,
        source_url,
        expected_sha256: artifact.digest.clone(),
        expected_size_bytes: Some(artifact.size_bytes),
        platform_selector: selector.to_string(),
        now_ms: 123,
        operation: TaskOperation::Install,
        expected_tree_digest: None,
        rebuild_uncommitted_version: true,
        external_acquisition: None,
    }
}

#[tokio::test]
async fn production_archive_installer_extracts_zip_and_targz_and_rejects_raw_unsafe_entries() {
    let directory = tempfile::tempdir().unwrap();
    let zip_path = directory.path().join("valid.zip");
    write_zip(&zip_path, &[("prefix/bin/server.exe", b"binary")]);
    let zip_artifact = fixture_artifact(zip_path);
    ArchiveInstaller {
        format: ArchiveFormat::Zip,
        strip_components: 1,
        limits: ArchiveLimits::default(),
    }
    .materialize(
        &zip_artifact,
        &directory.path().join("zip-stage"),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(directory.path().join("zip-stage/bin/server.exe")).unwrap(),
        b"binary"
    );

    let targz_path = directory.path().join("valid.tar.gz");
    write_targz(&targz_path, &[("prefix/bin/server", b"binary")]);
    let targz_artifact = fixture_artifact(targz_path);
    ArchiveInstaller {
        format: ArchiveFormat::TarGz,
        strip_components: 1,
        limits: ArchiveLimits::default(),
    }
    .materialize(
        &targz_artifact,
        &directory.path().join("targz-stage"),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    for (index, raw_name) in [
        "/absolute/file",
        "C:/drive/file",
        "../escape",
        "prefix/../escape",
        "prefix//double",
        "prefix\\..\\escape",
        "prefix/CON.txt",
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory.path().join(format!("unsafe-{index}.zip"));
        write_zip(&path, &[(raw_name, b"x")]);
        let artifact = fixture_artifact(path);
        let error = ArchiveInstaller {
            format: ArchiveFormat::Zip,
            strip_components: 1,
            limits: ArchiveLimits::default(),
        }
        .materialize(
            &artifact,
            &directory.path().join(format!("unsafe-stage-{index}")),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.code(),
            McpPlatformErrorCode::PathTraversal,
            "{raw_name}"
        );
    }

    let collision_path = directory.path().join("collision.zip");
    write_zip(
        &collision_path,
        &[("prefix/Bin/server", b"a"), ("prefix/bin/server", b"b")],
    );
    assert_eq!(
        ArchiveInstaller {
            format: ArchiveFormat::Zip,
            strip_components: 1,
            limits: ArchiveLimits::default(),
        }
        .materialize(
            &fixture_artifact(collision_path),
            &directory.path().join("collision-stage"),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .code(),
        McpPlatformErrorCode::PathTraversal
    );
}

#[tokio::test]
async fn production_npm_and_wheel_validate_metadata_dependencies_and_runtime_descriptors() {
    let directory = tempfile::tempdir().unwrap();
    let npm_path = directory.path().join("package.tgz");
    write_targz(
        &npm_path,
        &[
            (
                "package/package.json",
                br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"dependencies":{},"optionalDependencies":{},"peerDependencies":{},"scripts":{}}"#,
            ),
            ("package/dist/server.js", b"console.log('mcp')"),
        ],
    );
    let npm_artifact = fixture_artifact(npm_path);
    let mut npm_value: Value = serde_json::from_str(NPM).unwrap();
    npm_value["distribution"]["artifacts"][0]["digest"]["value"] = json!(npm_artifact.digest);
    npm_value["distribution"]["artifacts"][0]["size_bytes"] = json!(npm_artifact.size_bytes);
    let npm_manifest = parse_manifest(&serde_json::to_vec(&npm_value).unwrap())
        .unwrap()
        .manifest()
        .clone();
    let npm_adapter = ProductionDistributionEffectAdapter::new_with_ports(
        directory.path().join("npm-root"),
        RuntimeCapabilities::for_test(Some(PathBuf::from("server-node")), None, None),
        Arc::new(StaticArtifactFetcher(npm_artifact.clone())),
        Arc::new(Sha256Verifier),
    )
    .unwrap();
    let mut npm_effect = effect_for_manifest(
        npm_manifest,
        &npm_artifact,
        "npm_task",
        "managed_npm",
        "any/any",
    );
    let npm_outcome = npm_adapter
        .install(&npm_effect, &CancellationToken::new())
        .await
        .unwrap();
    match &npm_outcome.projection {
        ConnectionProjection::ManagedStdio {
            executable, args, ..
        } => {
            assert_eq!(executable, "server-node");
            assert!(args[0].replace('\\', "/").ends_with("dist/server.js"));
        }
        _ => panic!("expected managed stdio"),
    }
    npm_effect.expected_tree_digest = Some(npm_outcome.materialized_tree_digest.clone());
    std::fs::write(
        npm_outcome.installation_root.join("dist/server.js"),
        b"tampered",
    )
    .unwrap();
    assert_eq!(
        npm_adapter
            .inspect_installed(&npm_effect, &CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
    npm_effect.expected_tree_digest = None;
    let rebuilt = npm_adapter
        .install(&npm_effect, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(rebuilt.installation_root.join("dist/server.js")).unwrap(),
        b"console.log('mcp')"
    );

    let wheel_path = directory
        .path()
        .join("managed_fixture-1.2.3-py3-none-any.whl");
    write_zip(
        &wheel_path,
        &[
            (
                "managed_fixture-1.2.3.dist-info/METADATA",
                b"Metadata-Version: 2.1\nName: managed-fixture\nVersion: 1.2.3\n",
            ),
            (
                "managed_fixture-1.2.3.dist-info/WHEEL",
                b"Wheel-Version: 1.0\nTag: py3-none-any\n",
            ),
            (
                "managed_fixture-1.2.3.dist-info/entry_points.txt",
                b"[console_scripts]\nmanaged-fixture = managed_fixture.cli:main\n",
            ),
            ("managed_fixture/__init__.py", b""),
            ("managed_fixture/cli.py", b"def main(): pass\n"),
        ],
    );
    let wheel_artifact = fixture_artifact(wheel_path);
    let wheel_bytes = managed_manifest(
        json!({
            "type":"python_wheel","package":"managed-fixture","package_version":"1.2.3","python":">=3.11",
            "artifacts":[{"platform":"any","arch":"any","url":"https://packages.example.test/managed_fixture-1.2.3-py3-none-any.whl",
            "digest":{"algorithm":"sha256","value":wheel_artifact.digest},"size_bytes":wheel_artifact.size_bytes}],
            "entrypoint":{"executable":"${installation.bin}/managed-fixture"}
        }),
        "1.2.3",
    );
    let wheel_manifest = parse_manifest(&wheel_bytes).unwrap().manifest().clone();
    let wheel_adapter = ProductionDistributionEffectAdapter::new_with_ports(
        directory.path().join("wheel-root"),
        RuntimeCapabilities::for_test(None, Some(PathBuf::from("server-python")), Some((3, 11))),
        Arc::new(StaticArtifactFetcher(wheel_artifact.clone())),
        Arc::new(Sha256Verifier),
    )
    .unwrap();
    let mut wheel_effect = effect_for_manifest(
        wheel_manifest,
        &wheel_artifact,
        "wheel_task",
        "managed_wheel",
        "any/any",
    );
    let wheel_outcome = wheel_adapter
        .install(&wheel_effect, &CancellationToken::new())
        .await
        .unwrap();
    match &wheel_outcome.projection {
        ConnectionProjection::ManagedStdio {
            executable, args, ..
        } => {
            assert_eq!(executable, "server-python");
            assert_eq!(args[0], "-I");
            assert!(args[1].ends_with(".goose-wheel-launcher.py"));
        }
        _ => panic!("expected managed stdio"),
    }
    wheel_effect.expected_tree_digest = Some(wheel_outcome.materialized_tree_digest.clone());
    std::fs::write(
        wheel_outcome
            .installation_root
            .join(".goose-wheel-launcher.py"),
        b"tampered",
    )
    .unwrap();
    assert_eq!(
        wheel_adapter
            .inspect_installed(&wheel_effect, &CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
}

async fn install_npm_metadata_fixture(
    root: &std::path::Path,
    case: &str,
    package_json: &[u8],
) -> McpPlatformResult<ManagedInstallOutcome> {
    let artifact_path = root.join(format!("{case}.tgz"));
    write_targz(
        &artifact_path,
        &[
            ("package/package.json", package_json),
            ("package/dist/server.js", b"console.log('mcp')"),
        ],
    );
    let artifact = fixture_artifact(artifact_path);
    let mut value: Value = serde_json::from_str(NPM).unwrap();
    value["distribution"]["artifacts"][0]["digest"]["value"] = json!(artifact.digest);
    value["distribution"]["artifacts"][0]["size_bytes"] = json!(artifact.size_bytes);
    let manifest = parse_manifest(&serde_json::to_vec(&value).unwrap())
        .unwrap()
        .manifest()
        .clone();
    let adapter = ProductionDistributionEffectAdapter::new_with_ports(
        root.join(format!("{case}-root")),
        RuntimeCapabilities::for_test(Some(PathBuf::from("server-node")), None, None),
        Arc::new(StaticArtifactFetcher(artifact.clone())),
        Arc::new(Sha256Verifier),
    )?;
    adapter
        .install(
            &effect_for_manifest(
                manifest,
                &artifact,
                &format!("{case}_task"),
                &format!("managed_{case}"),
                "any/any",
            ),
            &CancellationToken::new(),
        )
        .await
}

async fn install_wheel_metadata_fixture(
    root: &std::path::Path,
    case: &str,
    metadata: &[u8],
) -> McpPlatformResult<ManagedInstallOutcome> {
    let artifact_path = root.join(format!("managed_fixture-1.2.3-{case}-py3-none-any.whl"));
    write_zip(
        &artifact_path,
        &[
            ("managed_fixture-1.2.3.dist-info/METADATA", metadata),
            (
                "managed_fixture-1.2.3.dist-info/WHEEL",
                b"Wheel-Version: 1.0\ntAg: py3-none-any\n",
            ),
            (
                "managed_fixture-1.2.3.dist-info/entry_points.txt",
                b"[console_scripts]\nmanaged-fixture = managed_fixture.cli:main\n",
            ),
            ("managed_fixture/__init__.py", b""),
            ("managed_fixture/cli.py", b"def main(): pass\n"),
        ],
    );
    let artifact = fixture_artifact(artifact_path);
    let manifest = parse_manifest(&managed_manifest(
        json!({
            "type":"python_wheel","package":"managed-fixture","package_version":"1.2.3","python":">=3.11",
            "artifacts":[{"platform":"any","arch":"any","url":format!("https://packages.example.test/{case}.whl"),
            "digest":{"algorithm":"sha256","value":artifact.digest},"size_bytes":artifact.size_bytes}],
            "entrypoint":{"executable":"${installation.bin}/managed-fixture"}
        }),
        "1.2.3",
    ))
    .unwrap()
    .manifest()
    .clone();
    let adapter = ProductionDistributionEffectAdapter::new_with_ports(
        root.join(format!("{case}-root")),
        RuntimeCapabilities::for_test(None, Some(PathBuf::from("server-python")), Some((3, 11))),
        Arc::new(StaticArtifactFetcher(artifact.clone())),
        Arc::new(Sha256Verifier),
    )?;
    adapter
        .install(
            &effect_for_manifest(
                manifest,
                &artifact,
                &format!("{case}_task"),
                &format!("managed_{case}"),
                "any/any",
            ),
            &CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn production_package_metadata_parsers_reject_malformed_or_implicit_code() {
    let directory = tempfile::tempdir().unwrap();
    let npm_cases: [(&str, &[u8]); 8] = [
        ("dependencies_string", br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"dependencies":"x"}"#),
        ("dependencies_array", br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"dependencies":[]}"#),
        ("dependencies_null", br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"dependencies":null}"#),
        ("scripts_nonempty", br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"scripts":{"postinstall":"evil"}}"#),
        ("scripts_string", br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"scripts":"evil"}"#),
        ("optional_nonempty", br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"optionalDependencies":{"x":"1.0.0"}}"#),
        ("bundle_dependencies", br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"bundleDependencies":[]}"#),
        ("bundled_dependencies", br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"bundledDependencies":["x"]}"#),
    ];
    for (case, package_json) in npm_cases {
        assert_eq!(
            install_npm_metadata_fixture(directory.path(), case, package_json)
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::PolicyDenied,
            "{case}"
        );
    }

    let wheel_cases: [(&str, &[u8]); 4] = [
        ("lower_requires", b"Metadata-Version: 2.1\nName: managed-fixture\nVersion: 1.2.3\nrequires-dist: evil==1.0.0\n"),
        ("mixed_requires", b"Metadata-Version: 2.1\nName: managed-fixture\nVersion: 1.2.3\nReQuIrEs-DiSt: evil==1.0.0\n  ; python_version > '3'\n"),
        ("duplicate_name", b"Metadata-Version: 2.1\nName: managed-fixture\nname: managed-fixture\nVersion: 1.2.3\n"),
        ("duplicate_version", b"Metadata-Version: 2.1\nName: managed-fixture\nVersion: 1.2.3\nversion: 1.2.3\n"),
    ];
    for (case, metadata) in wheel_cases {
        let error = install_wheel_metadata_fixture(directory.path(), case, metadata)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.code(),
                McpPlatformErrorCode::PolicyDenied | McpPlatformErrorCode::IntegrityError
            ),
            "{case}: {error:?}"
        );
    }
}

#[tokio::test]
async fn materialized_tree_authority_rejects_binary_replacement_extra_and_missing_files() {
    let directory = tempfile::tempdir().unwrap();
    let artifact_path = directory.path().join("binary.zip");
    write_zip(&artifact_path, &[("bin/server.exe", b"verified-binary")]);
    let artifact = fixture_artifact(artifact_path);
    let manifest = parse_manifest(&managed_manifest(
        json!({
            "type":"binary_archive","archive_format":"zip",
            "artifacts":[{"platform":"windows","arch":"x86_64","url":"https://downloads.example.test/binary.zip",
            "digest":{"algorithm":"sha256","value":artifact.digest},"size_bytes":artifact.size_bytes}],
            "entrypoint":{"executable":"${installation.root}/bin/server.exe"}
        }),
        "1.2.3",
    ))
    .unwrap()
    .manifest()
    .clone();
    let adapter = ProductionDistributionEffectAdapter::new_with_ports(
        directory.path().join("binary-root"),
        RuntimeCapabilities::for_test(None, None, None),
        Arc::new(StaticArtifactFetcher(artifact.clone())),
        Arc::new(Sha256Verifier),
    )
    .unwrap();
    let mut effect = effect_for_manifest(
        manifest,
        &artifact,
        "binary_task",
        "managed_binary",
        "windows/x86_64",
    );
    let installed = adapter
        .install(&effect, &CancellationToken::new())
        .await
        .unwrap();
    effect.expected_tree_digest = Some(installed.materialized_tree_digest.clone());
    let entrypoint = installed.installation_root.join("bin/server.exe");
    std::fs::write(&entrypoint, b"replacement").unwrap();
    assert_eq!(
        adapter
            .inspect_installed(&effect, &CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );

    effect.expected_tree_digest = None;
    let rebuilt = adapter
        .install(&effect, &CancellationToken::new())
        .await
        .unwrap();
    effect.expected_tree_digest = Some(rebuilt.materialized_tree_digest.clone());
    std::fs::write(rebuilt.installation_root.join("extra.txt"), b"extra").unwrap();
    assert_eq!(
        adapter
            .inspect_installed(&effect, &CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
    std::fs::remove_file(rebuilt.installation_root.join("extra.txt")).unwrap();
    std::fs::remove_file(rebuilt.installation_root.join("bin/server.exe")).unwrap();
    assert_eq!(
        adapter
            .inspect_installed(&effect, &CancellationToken::new())
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::IntegrityError
    );
    std::fs::remove_dir_all(&rebuilt.installation_root).unwrap();
    effect.task_id = "binary_repair_task".to_string();
    effect.operation = TaskOperation::Repair;
    effect.expected_tree_digest = None;
    effect.rebuild_uncommitted_version = true;
    let repaired = adapter
        .install(&effect, &CancellationToken::new())
        .await
        .unwrap();
    assert!(repaired.replaced_quarantine_token.is_none());
    assert_eq!(
        std::fs::read(repaired.installation_root.join("bin/server.exe")).unwrap(),
        b"verified-binary"
    );
}

#[tokio::test]
async fn production_distribution_runner_rebuilds_rename_before_commit_and_persists_tree_authority()
{
    let directory = tempfile::tempdir().unwrap();
    let artifact_path = directory.path().join("package.tgz");
    write_targz(
        &artifact_path,
        &[
            (
                "package/package.json",
                br#"{"name":"@example/issue-tracker-mcp","version":"2.3.1","bin":{"issue-tracker-mcp":"dist/server.js"},"dependencies":{},"optionalDependencies":{},"peerDependencies":{},"scripts":{}}"#,
            ),
            ("package/dist/server.js", b"console.log('verified')"),
        ],
    );
    let artifact = fixture_artifact(artifact_path);
    let mut manifest_value: Value = serde_json::from_str(NPM).unwrap();
    manifest_value["distribution"]["artifacts"][0]["digest"]["value"] = json!(artifact.digest);
    manifest_value["distribution"]["artifacts"][0]["size_bytes"] = json!(artifact.size_bytes);
    let verified = parse_manifest(&serde_json::to_vec(&manifest_value).unwrap()).unwrap();
    let manifest_digest = verified.digest().to_string();
    let database_path = directory.path().join("platform.db");
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path(&database_path)
            .await
            .unwrap(),
    );
    repository
        .save_manifest(&ManifestRecord {
            verified,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Official,
            source_metadata: Default::default(),
            created_at_ms: 100,
        })
        .await
        .unwrap();
    let sink = Arc::new(FakeProjectionSink::default());
    let ports = LifecyclePorts {
        registration: Arc::new(FakeRegistration),
        host_integration: Arc::new(EmptyHostIntegrationAdapter),
        transport: Arc::new(CoreTransportProjectionAdapter),
        auth: Arc::new(FakeAuth),
        health: Arc::new(FakeHealth::default()),
        projection_sink: sink,
    };
    let production = Arc::new(BlockingProductionDistribution {
        inner: ProductionDistributionEffectAdapter::new_with_ports(
            directory.path().join("managed-root"),
            RuntimeCapabilities::for_test(Some(PathBuf::from("server-node")), None, None),
            Arc::new(StaticArtifactFetcher(artifact)),
            Arc::new(Sha256Verifier),
        )
        .unwrap(),
        block_after_install_once: AtomicBool::new(true),
        install_calls: AtomicU64::new(0),
        installed: Notify::new(),
        block_before_activate_once: AtomicBool::new(false),
        activate_entered: Notify::new(),
    });
    let clock = Arc::new(TestClock(AtomicI64::new(100)));
    let service = Arc::new(McpPlatformService::new_with_distribution_ports(
        repository.clone(),
        clock.clone(),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions {
            compatibility_target: goose::mcp_platform::CompatibilityTarget {
                platform: Platform::Windows,
                arch: Architecture::X86_64,
            },
            plan_ttl_ms: 1_000_000,
            development_mode: false,
            docker_daemon_policy_allowed: true,
        },
        ports.clone(),
        production.clone(),
        RuntimeCapabilitySnapshot {
            node_available: true,
            python_major_minor: None,
        },
    ));
    let context = service.trusted_local_context();
    let plan = service
        .plan_create(
            &context,
            PlanCreateInput {
                intent: PlanIntent::Install { manifest_digest },
                idempotency_key: "production_plan".to_string(),
            },
        )
        .await
        .unwrap();
    let managed_mcp_id = plan.target.managed_mcp_id.clone().unwrap();
    let task = service
        .install_confirm(
            &context,
            InstallConfirmInput {
                plan_id: plan.plan_id,
                plan_digest: plan.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "production_task".to_string(),
            },
        )
        .await
        .unwrap();
    let running_service = service.clone();
    let execution = tokio::spawn(async move { running_service.runner_tick().await });
    production.installed.notified().await;
    let started = repository.list_task_steps(&task.task_id).await.unwrap();
    assert_eq!(
        started[1].status,
        goose::mcp_platform::TaskStepStatus::Started
    );
    assert!(started[1].evidence.is_none());
    execution.abort();
    let _ = execution.await;
    let version_root = directory
        .path()
        .join("managed-root/installations")
        .join(&managed_mcp_id)
        .join("versions/2.3.1");
    std::fs::write(
        version_root.join("dist/server.js"),
        b"untrusted-replacement",
    )
    .unwrap();
    std::fs::write(version_root.join("extra.txt"), b"untrusted-extra").unwrap();

    clock.set(100_000);
    production
        .block_before_activate_once
        .store(true, Ordering::SeqCst);
    let restarted = Arc::new(
        TaskRunner::new(
            repository.clone(),
            clock.clone(),
            ports.clone(),
            "production-restart".to_string(),
        )
        .with_distribution_adapter(production.clone()),
    );
    restarted.recover_startup().await.unwrap();
    let resumed_runner = restarted.clone();
    let resumed = tokio::spawn(async move { resumed_runner.tick().await });
    production.activate_entered.notified().await;
    assert!(production.install_calls.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        std::fs::read(version_root.join("dist/server.js")).unwrap(),
        b"console.log('verified')"
    );
    assert!(!version_root.join("extra.txt").exists());
    let committed_before_activation = repository
        .list_task_steps(&task.task_id)
        .await
        .unwrap()
        .into_iter()
        .find(|step| step.ordinal == 1)
        .unwrap();
    assert_eq!(
        committed_before_activation.status,
        goose::mcp_platform::TaskStepStatus::Committed
    );
    let tree_digest = match committed_before_activation.evidence.unwrap() {
        goose::mcp_platform::StepEvidence::ManagedDistributionMaterialized {
            tree_digest, ..
        } => tree_digest,
        evidence => panic!("unexpected evidence: {evidence:?}"),
    };
    assert_ne!(tree_digest, "ab".repeat(32));
    let inventory_tree_digest = repository
        .get_managed_inventory(&managed_mcp_id)
        .await
        .unwrap()
        .managed
        .versions
        .iter()
        .find(|version| version.version == "2.3.1")
        .unwrap()
        .materialized_tree_digest
        .clone()
        .unwrap();
    assert_eq!(inventory_tree_digest, tree_digest);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&database_path))
        .await
        .unwrap();
    let ownership_tree_digest = sqlx::query_scalar::<_, String>(
        "SELECT expected_digest FROM installation_ownership WHERE managed_mcp_id = ? AND version = '2.3.1' AND relative_path = '.'",
    )
    .bind(&managed_mcp_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ownership_tree_digest, tree_digest);
    pool.close().await;
    let install_calls_after_commit = production.install_calls.load(Ordering::SeqCst);
    resumed.abort();
    let _ = resumed.await;
    std::fs::write(version_root.join("dist/server.js"), b"committed-tamper").unwrap();
    std::fs::write(version_root.join("committed-extra.txt"), b"extra").unwrap();
    clock.set(200_000);
    let committed_restart = TaskRunner::new(
        repository.clone(),
        clock,
        ports,
        "production-committed-restart".to_string(),
    )
    .with_distribution_adapter(production.clone());
    committed_restart.recover_startup().await.unwrap();
    assert!(committed_restart.tick().await.unwrap());
    assert_eq!(
        production.install_calls.load(Ordering::SeqCst),
        install_calls_after_commit
    );
    assert!(matches!(
        repository.get_task(&task.task_id).await.unwrap().status,
        TaskStatus::Failed | TaskStatus::RecoveryRequired
    ));
}

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
impl TestClock {
    fn set(&self, value: i64) {
        self.0.store(value, Ordering::SeqCst);
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
struct FakeRegistration;
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

#[derive(Default)]
struct FakeAuth;
impl AuthRequirementResolver for FakeAuth {
    fn requirement(
        &self,
        _auth: &goose::mcp_platform::manifest::Auth,
    ) -> McpPlatformResult<AuthRequirement> {
        Ok(AuthRequirement::Ready)
    }
}

struct FakeHealth {
    healthy: AtomicBool,
}
impl Default for FakeHealth {
    fn default() -> Self {
        Self {
            healthy: AtomicBool::new(true),
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
        _cancellation: CancellationToken,
    ) -> McpPlatformResult<HealthAdapterResult> {
        let healthy = self.healthy.load(Ordering::SeqCst);
        Ok(HealthAdapterResult {
            result_code: if healthy {
                HealthResultCode::Healthy
            } else {
                HealthResultCode::Unhealthy
            },
            latency_ms: 1,
            capabilities_digest: None,
            tools_digest: None,
            detail_code: if healthy {
                HealthDetailCode::McpInitializeSucceeded
            } else {
                HealthDetailCode::McpInitializeFailed
            },
        })
    }
}

#[derive(Default)]
struct FakeProjectionSink {
    entries: Mutex<BTreeMap<String, ExtensionEntry>>,
    cleanup_events: Arc<Mutex<Vec<&'static str>>>,
    block_once: AtomicBool,
    entered: Notify,
    release: Notify,
    block_commit_once: AtomicBool,
    commit_entered: Notify,
    commit_release: Notify,
    fail_commit_after_block_once: AtomicBool,
    fail_disable_once: AtomicBool,
    disable_attempts: AtomicU64,
    commits: AtomicU64,
    confirmations: AtomicU64,
}

impl FakeProjectionSink {
    fn with_cleanup_events(cleanup_events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            cleanup_events,
            ..Self::default()
        }
    }

    async fn after_effect(&self) {
        if self.block_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    async fn after_commit(&self) -> McpPlatformResult<()> {
        if self.block_commit_once.swap(false, Ordering::SeqCst) {
            self.commit_entered.notify_one();
            self.commit_release.notified().await;
            if self
                .fail_commit_after_block_once
                .swap(false, Ordering::SeqCst)
            {
                return Err(test_error(McpPlatformErrorCode::RepositoryUnavailable));
            }
        }
        Ok(())
    }
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
        let entry = ExtensionEntry {
            enabled: false,
            config,
        };
        let created = {
            let mut entries = self.entries.lock().unwrap();
            match entries.get(key) {
                Some(existing) if existing == &entry => false,
                Some(_) => return Err(test_error(McpPlatformErrorCode::ProjectionConflict)),
                None => {
                    entries.insert(key.to_string(), entry.clone());
                    true
                }
            }
        };
        self.after_effect().await;
        Ok(ProjectionSnapshot { entry, created })
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
        if !enabled {
            self.disable_attempts.fetch_add(1, Ordering::SeqCst);
            self.cleanup_events.lock().unwrap().push("sink_disabled");
        }
        if !enabled && self.fail_disable_once.swap(false, Ordering::SeqCst) {
            return Err(test_error(McpPlatformErrorCode::RepositoryUnavailable));
        }
        let snapshot = {
            let mut entries = self.entries.lock().unwrap();
            let entry = entries
                .get_mut(key)
                .ok_or_else(|| test_error(McpPlatformErrorCode::NotFound))?;
            entry.enabled = enabled;
            ProjectionSnapshot {
                entry: entry.clone(),
                created: false,
            }
        };
        self.after_effect().await;
        Ok(snapshot)
    }
    async fn commit_enabled_projection(
        &self,
        plan: &ProjectionCommitPlan,
    ) -> McpPlatformResult<ProjectionSinkAtomicProof> {
        let proof = match plan.expected() {
            Some(target) => {
                let expected = plan
                    .expected_current()
                    .ok_or_else(|| test_error(McpPlatformErrorCode::IntegrityError))?;
                let mut entries = self.entries.lock().unwrap();
                match entries.get(plan.key()) {
                    Some(current) if current == &expected => {
                        entries.insert(plan.key().to_string(), target.clone());
                        let digest = observed_projection_digest(
                            plan.sink_identity(),
                            plan.key(),
                            Some(target),
                        )?;
                        plan.test_only_issue_atomic_proof(
                            self.adapter_id(),
                            self.adapter_version(),
                            ProjectionSinkAtomicProofKind::CompareAndSwapWrite,
                            digest,
                        )
                    }
                    _ => Err(test_error(McpPlatformErrorCode::ProjectionConflict)),
                }
            }
            None => {
                if plan.desired_enabled() || self.entries.lock().unwrap().contains_key(plan.key()) {
                    return Err(test_error(McpPlatformErrorCode::ProjectionConflict));
                }
                let digest = observed_projection_digest(plan.sink_identity(), plan.key(), None)?;
                plan.test_only_issue_atomic_proof(
                    self.adapter_id(),
                    self.adapter_version(),
                    ProjectionSinkAtomicProofKind::NoopCompareAndSwap,
                    digest,
                )
            }
        }?;
        self.commits.fetch_add(1, Ordering::SeqCst);
        self.after_commit().await?;
        Ok(proof)
    }
    async fn confirm_target_state(
        &self,
        plan: &ProjectionCommitPlan,
    ) -> McpPlatformResult<Option<ProjectionSinkAtomicProof>> {
        let entries = self.entries.lock().unwrap();
        let digest = match plan.expected() {
            Some(target) if entries.get(plan.key()) == Some(target) => {
                observed_projection_digest(plan.sink_identity(), plan.key(), Some(target))?
            }
            None if !plan.desired_enabled() && !entries.contains_key(plan.key()) => {
                observed_projection_digest(plan.sink_identity(), plan.key(), None)?
            }
            _ => return Err(test_error(McpPlatformErrorCode::ProjectionConflict)),
        };
        drop(entries);
        self.confirmations.fetch_add(1, Ordering::SeqCst);
        Ok(Some(plan.test_only_issue_atomic_proof(
            self.adapter_id(),
            self.adapter_version(),
            ProjectionSinkAtomicProofKind::NoopCompareAndSwap,
            digest,
        )?))
    }
    async fn remove_owned(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
    ) -> McpPlatformResult<bool> {
        let mut entries = self.entries.lock().unwrap();
        match entries.get(key) {
            None => Ok(false),
            Some(value) if value == &expected.entry => {
                entries.remove(key);
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
        let entry = ExtensionEntry { enabled, config };
        {
            let mut entries = self.entries.lock().unwrap();
            match entries.get(key) {
                Some(value) if value == &expected.entry => {
                    entries.insert(key.to_string(), entry.clone());
                }
                _ => return Err(test_error(McpPlatformErrorCode::ProjectionConflict)),
            }
        }
        self.after_effect().await;
        Ok(ProjectionSnapshot {
            entry,
            created: false,
        })
    }
}

#[derive(Default)]
struct FakeDistribution {
    versions: Mutex<HashSet<(String, String)>>,
    active: Mutex<HashMap<String, String>>,
    quarantined: Mutex<HashMap<String, (String, String)>>,
    block_install_once: AtomicBool,
    install_entered: Notify,
    install_release: Notify,
    block_activate_once: AtomicBool,
    activate_entered: Notify,
    activate_release: Notify,
    cleanup_events: Arc<Mutex<Vec<&'static str>>>,
    fail_restore_once: AtomicBool,
    fail_purge_once: AtomicBool,
    always_fail_purge: AtomicBool,
    capacity_mode: AtomicU64,
    capacity_calls: AtomicU64,
    install_calls: AtomicU64,
    fail_install_once: AtomicBool,
}

impl FakeDistribution {
    fn with_cleanup_events(cleanup_events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            cleanup_events,
            ..Self::default()
        }
    }

    fn outcome(effect: &ManagedInstallEffect) -> ManagedInstallOutcome {
        ManagedInstallOutcome {
            installation_root: PathBuf::from(format!(
                "managed/{}/{}",
                effect.managed_mcp_id,
                effect.manifest.version.as_str()
            )),
            projection: ConnectionProjection::ManagedStdio {
                name: effect.manifest.name.clone(),
                description: effect.manifest.description.clone(),
                executable: "server-owned-node".to_string(),
                args: vec![effect.manifest.version.as_str().to_string()],
                environment_keys: Vec::new(),
                cwd: None,
                timeout_seconds: Some(10),
            },
            evidence: ArtifactVerificationEvidence {
                source_origin: "https://registry.npmjs.org".to_string(),
                artifact_digest: effect.expected_sha256.clone(),
                size_bytes: effect.expected_size_bytes.unwrap_or(1),
                adapter_id: "npm".to_string(),
                adapter_version: "1".to_string(),
                platform_selector: effect.platform_selector.clone(),
                verification_result: VerificationResult::Verified,
                installed_at_ms: effect.now_ms,
                artifact_signature: ArtifactSignatureStatus::NotDeclaredByManifestV1,
            },
            materialized_tree_digest: "ab".repeat(32),
            owned_relative_paths: vec![".".to_string()],
            replaced_quarantine_token: (effect.operation
                == goose::mcp_platform::TaskOperation::Repair)
                .then(|| {
                    format!(
                        "{}-{}-{}-repair",
                        effect.managed_mcp_id,
                        effect.manifest.version.as_str(),
                        effect.task_id
                    )
                }),
            supply_chain_evidence: None,
        }
    }
}

#[async_trait]
impl DistributionEffectAdapter for FakeDistribution {
    fn adapter_version(&self) -> &'static str {
        "1"
    }
    async fn check_managed_storage_capacity(
        &self,
        required_peak_bytes: u64,
    ) -> McpPlatformResult<ManagedStorageCapacity> {
        self.capacity_calls.fetch_add(1, Ordering::SeqCst);
        match self.capacity_mode.load(Ordering::SeqCst) {
            0 => Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "managed storage capacity preflight is unavailable",
            )),
            1 => Ok(ManagedStorageCapacity {
                required_peak_bytes,
                available_bytes: required_peak_bytes,
            }),
            2 => Ok(ManagedStorageCapacity {
                required_peak_bytes,
                available_bytes: required_peak_bytes.saturating_sub(1),
            }),
            3 => Ok(ManagedStorageCapacity {
                required_peak_bytes: required_peak_bytes.saturating_add(1),
                available_bytes: required_peak_bytes.saturating_add(1),
            }),
            _ => Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "capacity adapter rejected https://capacity.invalid/path?token=secret at C:\\sensitive\\cache",
            )),
        }
    }
    async fn install(
        &self,
        effect: &ManagedInstallEffect,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        self.install_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_install_once.swap(false, Ordering::SeqCst) {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "managed writer rejected the materialization attempt",
            ));
        }
        let version = effect.manifest.version.as_str().to_string();
        if effect.operation == goose::mcp_platform::TaskOperation::Repair {
            let token = format!(
                "{}-{version}-{}-repair",
                effect.managed_mcp_id, effect.task_id
            );
            self.quarantined
                .lock()
                .unwrap()
                .insert(token, (effect.managed_mcp_id.clone(), version.clone()));
        }
        self.versions
            .lock()
            .unwrap()
            .insert((effect.managed_mcp_id.clone(), version));
        if self.block_install_once.swap(false, Ordering::SeqCst) {
            self.install_entered.notify_one();
            self.install_release.notified().await;
        }
        Ok(Self::outcome(effect))
    }
    async fn inspect_installed(
        &self,
        effect: &ManagedInstallEffect,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        if !self.versions.lock().unwrap().contains(&(
            effect.managed_mcp_id.clone(),
            effect.manifest.version.as_str().to_string(),
        )) {
            return Err(test_error(McpPlatformErrorCode::IntegrityError));
        }
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
        if self.block_activate_once.swap(false, Ordering::SeqCst) {
            self.activate_entered.notify_one();
            self.activate_release.notified().await;
        }
        Ok(())
    }
    async fn restore_activation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        _task_id: &str,
    ) -> McpPlatformResult<()> {
        if previous_version.is_none() {
            self.cleanup_events.lock().unwrap().push("runtime_revoked");
        }
        if previous_version.is_none() && self.fail_restore_once.swap(false, Ordering::SeqCst) {
            return Err(test_error(McpPlatformErrorCode::RepositoryUnavailable));
        }
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
        if !self.quarantined.lock().unwrap().contains_key(&token) {
            if !self
                .versions
                .lock()
                .unwrap()
                .remove(&(managed_mcp_id.to_string(), version.to_string()))
            {
                return Err(test_error(McpPlatformErrorCode::IntegrityError));
            }
            self.quarantined.lock().unwrap().insert(
                token.clone(),
                (managed_mcp_id.to_string(), version.to_string()),
            );
        }
        Ok(token)
    }
    async fn restore_quarantined(
        &self,
        _managed_mcp_id: &str,
        _version: &str,
        token: &str,
    ) -> McpPlatformResult<()> {
        if let Some(value) = self.quarantined.lock().unwrap().remove(token) {
            self.versions.lock().unwrap().insert(value);
        }
        Ok(())
    }
    async fn purge_quarantined(
        &self,
        token: &str,
        _cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        if self.fail_purge_once.swap(false, Ordering::SeqCst) {
            return Err(test_error(McpPlatformErrorCode::RepositoryUnavailable));
        }
        if self.always_fail_purge.load(Ordering::SeqCst) {
            return Err(test_error(McpPlatformErrorCode::RepositoryUnavailable));
        }
        self.quarantined.lock().unwrap().remove(token);
        Ok(())
    }
}

struct ManagedHarness {
    _directory: tempfile::TempDir,
    database_path: PathBuf,
    repository: Arc<SqliteMcpPlatformRepository>,
    service: Arc<McpPlatformService>,
    clock: Arc<TestClock>,
    ports: LifecyclePorts,
    distribution: Arc<FakeDistribution>,
    sink: Arc<FakeProjectionSink>,
    cleanup_events: Arc<Mutex<Vec<&'static str>>>,
    health: Arc<FakeHealth>,
}

impl ManagedHarness {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("platform.db");
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_path(&database_path)
                .await
                .unwrap(),
        );
        let clock = Arc::new(TestClock(AtomicI64::new(100)));
        let cleanup_events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::new(FakeProjectionSink::with_cleanup_events(
            cleanup_events.clone(),
        ));
        let health = Arc::new(FakeHealth::default());
        let ports = LifecyclePorts {
            registration: Arc::new(FakeRegistration),
            host_integration: Arc::new(EmptyHostIntegrationAdapter),
            transport: Arc::new(CoreTransportProjectionAdapter),
            auth: Arc::new(FakeAuth),
            health: health.clone(),
            projection_sink: sink.clone(),
        };
        let distribution = Arc::new(FakeDistribution::with_cleanup_events(
            cleanup_events.clone(),
        ));
        distribution.capacity_mode.store(1, Ordering::SeqCst);
        let service = Arc::new(McpPlatformService::new_with_distribution_ports(
            repository.clone(),
            clock.clone(),
            Arc::new(TestIds::default()),
            McpPlatformServiceOptions {
                compatibility_target: goose::mcp_platform::CompatibilityTarget {
                    platform: Platform::Windows,
                    arch: Architecture::X86_64,
                },
                plan_ttl_ms: 1_000_000,
                development_mode: false,
                docker_daemon_policy_allowed: true,
            },
            ports.clone(),
            distribution.clone(),
            RuntimeCapabilitySnapshot {
                node_available: true,
                python_major_minor: Some((3, 11)),
            },
        ));
        Self {
            _directory: directory,
            database_path,
            repository,
            service,
            clock,
            ports,
            distribution,
            sink,
            cleanup_events,
            health,
        }
    }
    fn context(&self) -> RequestContext {
        self.service.trusted_local_context()
    }
    async fn save_version(&self, version: &str, digest_char: char) -> String {
        let mut value: Value = serde_json::from_str(NPM).unwrap();
        value["version"] = json!(version);
        value["distribution"]["package_version"] = json!(version);
        value["distribution"]["artifacts"][0]["url"] = json!(format!(
            "https://registry.npmjs.org/@example/issue-tracker-mcp/-/issue-tracker-mcp-{version}.tgz"
        ));
        value["distribution"]["artifacts"][0]["digest"]["value"] =
            json!(digest_char.to_string().repeat(64));
        let verified = parse_manifest(&serde_json::to_vec(&value).unwrap()).unwrap();
        let digest = verified.digest().to_string();
        self.repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Official,
                source_metadata: Default::default(),
                created_at_ms: self.clock.now_ms(),
            })
            .await
            .unwrap();
        digest
    }

    async fn save_version_with_host_integration(&self, version: &str, digest_char: char) -> String {
        let mut value: Value = serde_json::from_str(NPM).unwrap();
        let host_manifest: Value = serde_json::from_str(HOUDINI).unwrap();
        value["version"] = json!(version);
        value["distribution"]["package_version"] = json!(version);
        value["distribution"]["artifacts"][0]["url"] = json!(format!(
            "https://registry.npmjs.org/@example/issue-tracker-mcp/-/issue-tracker-mcp-{version}.tgz"
        ));
        value["distribution"]["artifacts"][0]["digest"]["value"] =
            json!(digest_char.to_string().repeat(64));
        value["host_integrations"] = host_manifest["host_integrations"].clone();
        let verified = parse_manifest(&serde_json::to_vec(&value).unwrap()).unwrap();
        let digest = verified.digest().to_string();
        self.repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Official,
                source_metadata: Default::default(),
                created_at_ms: self.clock.now_ms(),
            })
            .await
            .unwrap();
        digest
    }
    async fn confirm(&self, intent: PlanIntent, key: &str) -> (String, String) {
        let plan = self
            .service
            .plan_create(
                &self.context(),
                PlanCreateInput {
                    intent,
                    idempotency_key: format!("plan_{key}"),
                },
            )
            .await
            .unwrap();
        let managed = plan.target.managed_mcp_id.clone().unwrap();
        let task = self
            .service
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
            .unwrap();
        (managed, task.task_id)
    }
    fn restarted_runner(&self) -> TaskRunner {
        TaskRunner::new(
            self.repository.clone(),
            self.clock.clone(),
            self.ports.clone(),
            "restart-worker".to_string(),
        )
        .with_distribution_adapter(self.distribution.clone())
    }
}

fn test_error(code: McpPlatformErrorCode) -> McpPlatformError {
    McpPlatformError::new(code, "typed managed test failure")
}

#[cfg(target_os = "windows")]
#[tokio::test]
async fn managed_capacity_preflight_rejects_before_steps_claims_or_installs() {
    for capacity_mode in [0, 2, 3, 4] {
        for operation in [
            TaskOperation::Install,
            TaskOperation::Update,
            TaskOperation::Repair,
        ] {
            let harness = ManagedHarness::new().await;
            let v1 = harness.save_version("2.3.1", 'a').await;
            let v2 = harness.save_version("2.3.2", 'b').await;
            let (managed, task_id) = match operation {
                TaskOperation::Install => {
                    harness
                        .confirm(
                            PlanIntent::Install {
                                manifest_digest: v1,
                            },
                            &format!("capacity_{capacity_mode}_install"),
                        )
                        .await
                }
                TaskOperation::Update | TaskOperation::Repair => {
                    let (managed, seed_task) = harness
                        .confirm(
                            PlanIntent::Install {
                                manifest_digest: v1,
                            },
                            &format!("capacity_{capacity_mode}_{operation:?}_seed"),
                        )
                        .await;
                    assert!(harness.service.runner_tick().await.unwrap());
                    assert_eq!(
                        harness
                            .repository
                            .get_task(&seed_task)
                            .await
                            .unwrap()
                            .status,
                        TaskStatus::Succeeded
                    );
                    let intent = match operation {
                        TaskOperation::Update => PlanIntent::Update {
                            managed_mcp_id: managed.clone(),
                            target_version: "2.3.2".to_string(),
                        },
                        TaskOperation::Repair => PlanIntent::Repair {
                            managed_mcp_id: managed.clone(),
                        },
                        _ => unreachable!(),
                    };
                    harness
                        .confirm(intent, &format!("capacity_{capacity_mode}_{operation:?}"))
                        .await
                }
                _ => unreachable!(),
            };
            harness
                .distribution
                .capacity_mode
                .store(capacity_mode, Ordering::SeqCst);
            harness
                .distribution
                .capacity_calls
                .store(0, Ordering::SeqCst);
            let versions_before = harness.distribution.versions.lock().unwrap().clone();
            let installs_before = harness.distribution.install_calls.load(Ordering::SeqCst);

            assert!(harness.service.runner_tick().await.unwrap());

            let task = harness.repository.get_task(&task_id).await.unwrap();
            assert_eq!(task.status, TaskStatus::Cancelled);
            let message = task.redacted_error.as_ref().unwrap().message();
            assert!(!message.contains("https://"));
            assert!(!message.contains("token"));
            assert!(!message.contains("sensitive"));
            assert_eq!(message, "MCP task preflight rejected before execution");
            assert!(task.owner_id.is_none());
            assert!(task.lease_expires_at_ms.is_none());
            assert!(
                !harness
                    .repository
                    .list_audit_events(&task_id)
                    .await
                    .unwrap()
                    .iter()
                    .any(|event| matches!(
                        event.payload,
                        AuditPayload::TaskStatusChanged {
                            to: TaskStatus::Running,
                            ..
                        }
                    )),
                "{operation:?} must not enter running before capacity rejection"
            );
            assert_eq!(
                harness.distribution.capacity_calls.load(Ordering::SeqCst),
                1
            );
            assert_eq!(
                harness.distribution.install_calls.load(Ordering::SeqCst),
                installs_before
            );
            assert_eq!(
                *harness.distribution.versions.lock().unwrap(),
                versions_before
            );
            assert!(
                harness
                    .repository
                    .list_task_steps(&task_id)
                    .await
                    .unwrap()
                    .is_empty(),
                "{operation:?} must not create task-step, claim, cache, or install effects after capacity failure"
            );
        }
    }
}

#[cfg(target_os = "windows")]
#[tokio::test]
async fn successful_capacity_observation_does_not_authorize_writer_effects() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", 'c').await;
    let (_managed, task_id) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1,
            },
            "capacity_writer_boundary",
        )
        .await;
    harness
        .distribution
        .fail_install_once
        .store(true, Ordering::SeqCst);

    assert!(harness.service.runner_tick().await.unwrap());

    assert_eq!(
        harness.distribution.capacity_calls.load(Ordering::SeqCst),
        1
    );
    assert_eq!(harness.distribution.install_calls.load(Ordering::SeqCst), 1);
    assert!(harness.distribution.versions.lock().unwrap().is_empty());
    assert_eq!(
        harness.repository.get_task(&task_id).await.unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn service_runner_install_update_repair_uninstall_is_atomic_and_preserves_enabled_intent() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", '1').await;
    let _v2 = harness.save_version("2.3.2", '2').await;
    let (managed, install_task) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1,
            },
            "install",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&install_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );
    let materialize_step = harness
        .repository
        .list_task_steps(&install_task)
        .await
        .unwrap()
        .into_iter()
        .find(|step| step.ordinal == 1)
        .unwrap();
    let step_tree_digest = match materialize_step.evidence.unwrap() {
        goose::mcp_platform::StepEvidence::ManagedDistributionMaterialized {
            tree_digest, ..
        } => tree_digest,
        evidence => panic!("unexpected materialize evidence: {evidence:?}"),
    };
    let inventory_tree_digest = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap()
        .managed
        .versions[0]
        .materialized_tree_digest
        .clone()
        .unwrap();
    assert_eq!(inventory_tree_digest, step_tree_digest);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&harness.database_path))
        .await
        .unwrap();
    let ownership_tree_digest = sqlx::query_scalar::<_, String>(
        "SELECT expected_digest FROM installation_ownership WHERE managed_mcp_id = ? AND version = '2.3.1' AND relative_path = '.'",
    )
    .bind(&managed)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ownership_tree_digest, step_tree_digest);
    let installed = harness
        .service
        .managed_get(
            &harness.context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap();
    assert!(!installed.summary.default_enabled);
    assert!(installed.summary.current_task.is_none());
    assert_eq!(installed.summary.active_version.as_deref(), Some("2.3.1"));
    assert_eq!(
        installed.summary.available_version.as_deref(),
        Some("2.3.2")
    );
    assert!(installed.summary.eligibility.update);
    let runtime_unavailable_service = McpPlatformService::new_with_distribution_ports(
        harness.repository.clone(),
        harness.clock.clone(),
        Arc::new(TestIds::default()),
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
            node_available: false,
            python_major_minor: Some((3, 11)),
        },
    );
    let unavailable = runtime_unavailable_service
        .managed_get(
            &runtime_unavailable_service.trusted_local_context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap();
    assert!(!unavailable.summary.eligibility.update);
    assert_eq!(
        unavailable.summary.eligibility.update_reason,
        goose::mcp_platform::service::ManagedEligibilityReason::RuntimeUnavailable
    );
    harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.clone(),
                enabled: true,
                expected_revision: installed.summary.revision,
            },
        )
        .await
        .unwrap();
    let (_, update_task) = harness
        .confirm(
            PlanIntent::Update {
                managed_mcp_id: managed.clone(),
                target_version: "2.3.2".to_string(),
            },
            "update",
        )
        .await;
    let queued = harness
        .service
        .managed_get(
            &harness.context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        queued.summary.current_task.as_ref().unwrap().task_id,
        update_task
    );
    assert!(!queued.summary.eligibility.update);
    assert!(harness.service.runner_tick().await.unwrap());
    let updated = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    assert_eq!(updated.lifecycle.active_version.as_deref(), Some("2.3.2"));
    assert!(updated.managed.state.default_enabled);
    assert_eq!(updated.managed.versions.len(), 1);
    let updated_summary = harness
        .service
        .managed_get(
            &harness.context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap()
        .summary;
    assert!(!updated_summary.eligibility.update);
    assert_eq!(
        updated_summary.eligibility.update_reason,
        goose::mcp_platform::service::ManagedEligibilityReason::NoUpdateAvailable
    );
    assert!(updated_summary.eligibility.repair);
    assert!(updated_summary.eligibility.uninstall);
    harness
        .save_version_with_host_integration("2.3.3", 'd')
        .await;
    let host_blocked_summary = harness
        .service
        .managed_get(
            &harness.context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap()
        .summary;
    assert_eq!(
        host_blocked_summary.available_version.as_deref(),
        Some("2.3.3")
    );
    assert!(!host_blocked_summary.eligibility.update);
    assert_eq!(
        host_blocked_summary.eligibility.update_reason,
        goose::mcp_platform::service::ManagedEligibilityReason::Incompatible
    );
    assert!(host_blocked_summary.eligibility.repair);
    assert!(host_blocked_summary.eligibility.uninstall);
    let (_, repair_task) = harness
        .confirm(
            PlanIntent::Repair {
                managed_mcp_id: managed.clone(),
            },
            "repair",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&repair_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );
    let (_, uninstall_task) = harness
        .confirm(
            PlanIntent::Uninstall {
                managed_mcp_id: managed.clone(),
                preserve_user_data: true,
            },
            "uninstall",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    let uninstall_record = harness.repository.get_task(&uninstall_task).await.unwrap();
    let uninstall_steps = harness
        .repository
        .list_task_steps(&uninstall_task)
        .await
        .unwrap();
    assert_eq!(
        uninstall_record.status,
        TaskStatus::Succeeded,
        "task={uninstall_record:?} steps={uninstall_steps:?}"
    );
    assert_eq!(
        harness
            .repository
            .get_managed_inventory(&managed)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::NotFound
    );
    assert!(harness
        .distribution
        .active_version(&managed)
        .await
        .unwrap()
        .is_none());
    assert!(harness.distribution.versions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn credential_drift_disables_enabled_update_and_repair_before_new_activation() {
    for operation in [TaskOperation::Update, TaskOperation::Repair] {
        let harness = ManagedHarness::new().await;
        let v1 = harness.save_version("2.3.1", 'm').await;
        let _v2 = harness.save_version("2.3.2", 'n').await;
        let (managed, _) = harness
            .confirm(
                PlanIntent::Install {
                    manifest_digest: v1.clone(),
                },
                "credential_drift_seed",
            )
            .await;
        assert!(harness.service.runner_tick().await.unwrap());

        let seeded = harness
            .repository
            .get_managed_inventory(&managed)
            .await
            .unwrap();
        let manifest = harness.repository.get_manifest(&v1).await.unwrap();
        harness.service.set_enrollment_runtime_binding_resolver(
            StaticEnrollmentRuntimeBindingResolver::ready(
                &managed,
                seeded.managed.revision,
                &v1,
                &manifest.verified.manifest().auth,
            ),
        );
        let enabled = harness
            .service
            .set_default_enabled(
                &harness.context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed.clone(),
                    enabled: true,
                    expected_revision: seeded.managed.revision,
                },
            )
            .await
            .unwrap();
        assert!(enabled.default_enabled);

        let intent = match operation {
            TaskOperation::Update => PlanIntent::Update {
                managed_mcp_id: managed.clone(),
                target_version: "2.3.2".to_string(),
            },
            TaskOperation::Repair => PlanIntent::Repair {
                managed_mcp_id: managed.clone(),
            },
            _ => unreachable!(),
        };
        let (_, task_id) = harness
            .confirm(intent, &format!("credential_drift_{operation:?}"))
            .await;
        harness.service.set_enrollment_runtime_binding_resolver(
            StaticEnrollmentRuntimeBindingResolver::missing(),
        );

        assert!(harness.service.runner_tick().await.unwrap());
        assert_eq!(
            harness.repository.get_task(&task_id).await.unwrap().status,
            TaskStatus::Failed,
        );
        let projection = harness
            .repository
            .get_connection_projection(&managed)
            .await
            .unwrap();
        assert!(
            !harness
                .sink
                .get(&projection.link_key)
                .await
                .unwrap()
                .unwrap()
                .entry
                .enabled,
            "{operation:?} must leave its live projection disabled after credential drift"
        );
        assert!(
            harness
                .distribution
                .active_version(&managed)
                .await
                .unwrap()
                .is_none(),
            "{operation:?} must not leave a managed runtime active after credential drift"
        );
    }
}

#[tokio::test]
async fn credential_drift_after_new_runtime_activation_does_not_restore_old_runtime() {
    for operation in [TaskOperation::Update, TaskOperation::Repair] {
        let harness = ManagedHarness::new().await;
        let v1 = harness.save_version("2.3.1", 'b').await;
        let _v2 = harness.save_version("2.3.2", 'c').await;
        let (managed, _) = harness
            .confirm(
                PlanIntent::Install {
                    manifest_digest: v1.clone(),
                },
                "credential_drift_after_runtime_seed",
            )
            .await;
        assert!(harness.service.runner_tick().await.unwrap());

        let seeded = harness
            .repository
            .get_managed_inventory(&managed)
            .await
            .unwrap();
        let manifest = harness.repository.get_manifest(&v1).await.unwrap();
        harness.service.set_enrollment_runtime_binding_resolver(
            StaticEnrollmentRuntimeBindingResolver::ready(
                &managed,
                seeded.managed.revision,
                &v1,
                &manifest.verified.manifest().auth,
            ),
        );
        harness
            .service
            .set_default_enabled(
                &harness.context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed.clone(),
                    enabled: true,
                    expected_revision: seeded.managed.revision,
                },
            )
            .await
            .unwrap();

        harness
            .distribution
            .block_activate_once
            .store(true, Ordering::SeqCst);
        let intent = match operation {
            TaskOperation::Update => PlanIntent::Update {
                managed_mcp_id: managed.clone(),
                target_version: "2.3.2".to_string(),
            },
            TaskOperation::Repair => PlanIntent::Repair {
                managed_mcp_id: managed.clone(),
            },
            _ => unreachable!(),
        };
        let (_, task_id) = harness
            .confirm(
                intent,
                &format!("credential_drift_after_runtime_{operation:?}"),
            )
            .await;
        let service = harness.service.clone();
        let execution = tokio::spawn(async move { service.runner_tick().await });
        harness.distribution.activate_entered.notified().await;
        assert!(
            harness
                .distribution
                .active_version(&managed)
                .await
                .unwrap()
                .is_some(),
            "{operation:?} must have activated the new runtime before the credential drift"
        );
        harness.service.set_enrollment_runtime_binding_resolver(
            StaticEnrollmentRuntimeBindingResolver::missing(),
        );
        harness.distribution.activate_release.notify_one();
        execution.await.unwrap().unwrap();

        assert_eq!(
            harness.repository.get_task(&task_id).await.unwrap().status,
            TaskStatus::RecoveryRequired,
            "{operation:?} must not report a compensated credential-drift rollback as successful"
        );
        let projection = harness
            .repository
            .get_connection_projection(&managed)
            .await
            .unwrap();
        assert!(
            !harness
                .sink
                .get(&projection.link_key)
                .await
                .unwrap()
                .unwrap()
                .entry
                .enabled,
            "{operation:?} must not restore an enabled projection after credential drift"
        );
        assert!(
            harness
                .distribution
                .active_version(&managed)
                .await
                .unwrap()
                .is_none(),
            "{operation:?} must leave both old and new runtimes inactive after credential drift"
        );
    }
}

#[tokio::test]
async fn projection_recovery_persisted_authority_disables_runtime_when_credentials_drift() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", 'd').await;
    let (managed, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1.clone(),
            },
            "projection_recovery_credential_drift_seed",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());

    let seeded = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    let manifest = harness.repository.get_manifest(&v1).await.unwrap();
    harness.service.set_enrollment_runtime_binding_resolver(
        StaticEnrollmentRuntimeBindingResolver::ready(
            &managed,
            seeded.managed.revision,
            &v1,
            &manifest.verified.manifest().auth,
        ),
    );
    harness.sink.block_commit_once.store(true, Ordering::SeqCst);
    let service = harness.service.clone();
    let managed_for_enable = managed.clone();
    let enablement = tokio::spawn(async move {
        service
            .set_default_enabled(
                &service.trusted_local_context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed_for_enable,
                    enabled: true,
                    expected_revision: seeded.managed.revision,
                },
            )
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        harness.sink.commit_entered.notified(),
    )
    .await
    .unwrap();
    let projection = harness
        .repository
        .get_connection_projection(&managed)
        .await
        .unwrap();
    assert!(
        harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled
    );
    enablement.abort();
    let _ = enablement.await;
    harness
        .sink
        .entries
        .lock()
        .unwrap()
        .get_mut(&projection.link_key)
        .unwrap()
        .enabled = false;

    harness
        .service
        .set_enrollment_runtime_binding_resolver(StaticEnrollmentRuntimeBindingResolver::missing());
    let restarted = harness.restarted_runner();
    restarted.recover_startup().await.unwrap();

    assert!(
        harness
            .repository
            .projection_recovery_required(&managed)
            .await
            .unwrap(),
        "credential drift after a sink commit must remain a recovery-required mutation"
    );
    assert_eq!(
        harness.sink.commits.load(Ordering::SeqCst),
        1,
        "enablement must use the projection sink atomic commit API"
    );
    assert_eq!(
        harness.sink.confirmations.load(Ordering::SeqCst),
        1,
        "persisted-authority recovery must use the projection sink confirmation API"
    );
    assert!(
        !harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled,
        "recovery must explicitly disable a committed enabled sink"
    );
    assert!(
        harness
            .distribution
            .active_version(&managed)
            .await
            .unwrap()
            .is_none(),
        "recovery must revoke the managed runtime when credential readiness fails"
    );
}

#[tokio::test]
async fn projection_recovery_mismatch_disables_enabled_sink_when_credentials_drift() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", 'g').await;
    let (managed, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1.clone(),
            },
            "projection_recovery_mismatch_credential_drift_seed",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());

    let seeded = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    let manifest = harness.repository.get_manifest(&v1).await.unwrap();
    harness.service.set_enrollment_runtime_binding_resolver(
        StaticEnrollmentRuntimeBindingResolver::ready(
            &managed,
            seeded.managed.revision,
            &v1,
            &manifest.verified.manifest().auth,
        ),
    );
    harness.sink.block_commit_once.store(true, Ordering::SeqCst);
    let service = harness.service.clone();
    let managed_for_enable = managed.clone();
    let enablement = tokio::spawn(async move {
        service
            .set_default_enabled(
                &service.trusted_local_context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed_for_enable,
                    enabled: true,
                    expected_revision: seeded.managed.revision,
                },
            )
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        harness.sink.commit_entered.notified(),
    )
    .await
    .unwrap();
    let projection = harness
        .repository
        .get_connection_projection(&managed)
        .await
        .unwrap();
    enablement.abort();
    let _ = enablement.await;
    let mut drifted = harness
        .sink
        .entries
        .lock()
        .unwrap()
        .get(&projection.link_key)
        .cloned()
        .unwrap();
    match &mut drifted.config {
        ExtensionConfig::Stdio { description, .. } => description.push_str(" drift"),
        _ => panic!("managed transport projection must be stdio"),
    }
    harness
        .sink
        .entries
        .lock()
        .unwrap()
        .insert(projection.link_key.clone(), drifted);

    harness
        .service
        .set_enrollment_runtime_binding_resolver(StaticEnrollmentRuntimeBindingResolver::missing());
    harness.restarted_runner().recover_startup().await.unwrap();

    assert!(harness
        .repository
        .projection_recovery_required(&managed)
        .await
        .unwrap());
    assert!(
        !harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled,
        "mismatch recovery must fail closed when credentials drift"
    );
    assert!(
        harness
            .distribution
            .active_version(&managed)
            .await
            .unwrap()
            .is_none(),
        "mismatch recovery must revoke the active runtime"
    );
}

#[tokio::test]
async fn projection_recovery_desired_disabled_fails_closed_for_persisted_enabled_sink() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", 'k').await;
    let (managed, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1.clone(),
            },
            "projection_recovery_desired_disabled_seed",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());

    let seeded = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    let manifest = harness.repository.get_manifest(&v1).await.unwrap();
    harness.service.set_enrollment_runtime_binding_resolver(
        StaticEnrollmentRuntimeBindingResolver::ready(
            &managed,
            seeded.managed.revision,
            &v1,
            &manifest.verified.manifest().auth,
        ),
    );
    harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.clone(),
                enabled: true,
                expected_revision: seeded.managed.revision,
            },
        )
        .await
        .unwrap();
    let enabled = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();

    harness.sink.block_commit_once.store(true, Ordering::SeqCst);
    let service = harness.service.clone();
    let managed_for_disable = managed.clone();
    let disablement = tokio::spawn(async move {
        service
            .set_default_enabled(
                &service.trusted_local_context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed_for_disable,
                    enabled: false,
                    expected_revision: enabled.managed.revision,
                },
            )
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        harness.sink.commit_entered.notified(),
    )
    .await
    .unwrap();
    disablement.abort();
    let _ = disablement.await;

    let projection = harness
        .repository
        .get_connection_projection(&managed)
        .await
        .unwrap();
    harness
        .sink
        .entries
        .lock()
        .unwrap()
        .get_mut(&projection.link_key)
        .unwrap()
        .enabled = true;
    harness
        .service
        .set_enrollment_runtime_binding_resolver(StaticEnrollmentRuntimeBindingResolver::missing());
    harness.cleanup_events.lock().unwrap().clear();

    harness.restarted_runner().recover_startup().await.unwrap();

    assert!(
        harness
            .repository
            .projection_recovery_required(&managed)
            .await
            .unwrap(),
        "credential-gated cleanup remains retryable rather than resolving the mutation"
    );
    assert!(
        !harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled
    );
    assert!(harness
        .distribution
        .active_version(&managed)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        harness.cleanup_events.lock().unwrap().as_slice(),
        ["runtime_revoked", "sink_disabled"],
        "fail-close revokes and verifies the runtime before modifying the sink"
    );
}

#[tokio::test]
async fn projection_restore_credential_drift_disables_new_live_projection_and_runtime() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", 'h').await;
    let _v2 = harness.save_version("2.3.2", 'i').await;
    let (managed, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1.clone(),
            },
            "projection_restore_credential_drift_seed",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());

    let seeded = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    let manifest = harness.repository.get_manifest(&v1).await.unwrap();
    harness.service.set_enrollment_runtime_binding_resolver(
        StaticEnrollmentRuntimeBindingResolver::ready(
            &managed,
            seeded.managed.revision,
            &v1,
            &manifest.verified.manifest().auth,
        ),
    );
    harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.clone(),
                enabled: true,
                expected_revision: seeded.managed.revision,
            },
        )
        .await
        .unwrap();

    harness.sink.block_once.store(true, Ordering::SeqCst);
    harness
        .distribution
        .fail_purge_once
        .store(true, Ordering::SeqCst);
    let (_, task_id) = harness
        .confirm(
            PlanIntent::Update {
                managed_mcp_id: managed.clone(),
                target_version: "2.3.2".to_string(),
            },
            "projection_restore_credential_drift_update",
        )
        .await;
    let service = harness.service.clone();
    let execution = tokio::spawn(async move { service.runner_tick().await });
    tokio::time::timeout(Duration::from_secs(5), harness.sink.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        harness
            .distribution
            .active_version(&managed)
            .await
            .unwrap()
            .as_deref(),
        Some("2.3.2"),
        "the failing commit must occur after new runtime activation"
    );
    harness
        .service
        .set_enrollment_runtime_binding_resolver(StaticEnrollmentRuntimeBindingResolver::missing());
    harness.sink.release.notify_one();
    execution.await.unwrap().unwrap();

    assert_eq!(
        harness.repository.get_task(&task_id).await.unwrap().status,
        TaskStatus::RecoveryRequired,
        "credential drift during projection restoration cannot report a completed rollback"
    );
    let projection = harness
        .repository
        .get_connection_projection(&managed)
        .await
        .unwrap();
    assert!(
        !harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled
    );
    assert!(harness
        .distribution
        .active_version(&managed)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn failed_projection_disable_keeps_recovery_required_and_revokes_runtime() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", 'j').await;
    let (managed, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1.clone(),
            },
            "failed_projection_disable_seed",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());

    let seeded = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    let manifest = harness.repository.get_manifest(&v1).await.unwrap();
    harness.service.set_enrollment_runtime_binding_resolver(
        StaticEnrollmentRuntimeBindingResolver::ready(
            &managed,
            seeded.managed.revision,
            &v1,
            &manifest.verified.manifest().auth,
        ),
    );
    harness.sink.block_commit_once.store(true, Ordering::SeqCst);
    let service = harness.service.clone();
    let managed_for_enable = managed.clone();
    let enablement = tokio::spawn(async move {
        service
            .set_default_enabled(
                &service.trusted_local_context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed_for_enable,
                    enabled: true,
                    expected_revision: seeded.managed.revision,
                },
            )
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        harness.sink.commit_entered.notified(),
    )
    .await
    .unwrap();
    harness
        .service
        .set_enrollment_runtime_binding_resolver(StaticEnrollmentRuntimeBindingResolver::missing());
    harness.sink.fail_disable_once.store(true, Ordering::SeqCst);
    harness.cleanup_events.lock().unwrap().clear();
    harness.sink.commit_release.notify_one();
    assert!(enablement.await.unwrap().is_err());

    let projection = harness
        .repository
        .get_connection_projection(&managed)
        .await
        .unwrap();
    assert!(
        harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled,
        "the test sink intentionally retains the unsafe enabled projection after write failure"
    );
    assert!(
        harness
            .distribution
            .active_version(&managed)
            .await
            .unwrap()
            .is_none(),
        "runtime revocation must still be attempted after projection disable failure"
    );
    assert!(harness
        .repository
        .projection_recovery_required(&managed)
        .await
        .unwrap());
    assert_eq!(harness.sink.disable_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        harness.cleanup_events.lock().unwrap().as_slice(),
        ["runtime_revoked", "sink_disabled"],
        "sink cleanup starts only after the managed runtime is inactive"
    );

    harness.cleanup_events.lock().unwrap().clear();
    harness.restarted_runner().recover_startup().await.unwrap();

    assert!(harness
        .repository
        .projection_recovery_required(&managed)
        .await
        .unwrap());
    assert!(
        !harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled,
        "startup recovery must retry and finish disabling the unsafe sink"
    );
    assert!(harness
        .distribution
        .active_version(&managed)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        harness.cleanup_events.lock().unwrap().as_slice(),
        ["runtime_revoked", "sink_disabled"],
        "automatic retry preserves runtime-first cleanup ordering"
    );

    let mut drifted = harness
        .sink
        .entries
        .lock()
        .unwrap()
        .get(&projection.link_key)
        .cloned()
        .unwrap();
    drifted.enabled = true;
    match &mut drifted.config {
        ExtensionConfig::Stdio { description, .. } => description.push_str(" drift"),
        _ => panic!("managed transport projection must be stdio"),
    }
    harness
        .sink
        .entries
        .lock()
        .unwrap()
        .insert(projection.link_key.clone(), drifted);
    harness
        .distribution
        .active
        .lock()
        .unwrap()
        .insert(managed.clone(), "2.3.1".to_string());
    harness
        .distribution
        .fail_restore_once
        .store(true, Ordering::SeqCst);

    harness.cleanup_events.lock().unwrap().clear();
    assert_eq!(
        harness
            .service
            .resolve_projection_recovery(&managed)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::RepositoryUnavailable
    );
    assert!(harness
        .repository
        .projection_recovery_required(&managed)
        .await
        .unwrap());
    assert!(
        harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled,
        "a failed runtime revocation must leave the sink untouched"
    );
    assert_eq!(
        harness
            .distribution
            .active_version(&managed)
            .await
            .unwrap()
            .as_deref(),
        Some("2.3.1")
    );
    assert_eq!(
        harness.cleanup_events.lock().unwrap().as_slice(),
        ["runtime_revoked"],
        "runtime failure prevents any sink mutation"
    );

    harness.cleanup_events.lock().unwrap().clear();
    assert_eq!(
        harness
            .service
            .resolve_projection_recovery(&managed)
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::RollbackIncomplete
    );
    assert!(
        !harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled,
        "explicit recovery retries the cleanup after runtime revocation succeeds"
    );
    assert_eq!(
        harness.cleanup_events.lock().unwrap().as_slice(),
        ["runtime_revoked", "sink_disabled"],
        "the production service recovery entry delegates to the runtime-first task runner"
    );
}

#[tokio::test]
async fn disabled_projection_runtime_restore_does_not_require_credentials() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", 'l').await;
    let _v2 = harness.save_version("2.3.2", 'm').await;
    let (managed, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1,
            },
            "disabled_projection_runtime_restore_seed",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());

    harness.sink.block_once.store(true, Ordering::SeqCst);
    harness
        .distribution
        .fail_purge_once
        .store(true, Ordering::SeqCst);
    let (_, task_id) = harness
        .confirm(
            PlanIntent::Update {
                managed_mcp_id: managed.clone(),
                target_version: "2.3.2".to_string(),
            },
            "disabled_projection_runtime_restore_update",
        )
        .await;
    let service = harness.service.clone();
    let execution = tokio::spawn(async move { service.runner_tick().await });
    tokio::time::timeout(Duration::from_secs(5), harness.sink.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        harness
            .distribution
            .active_version(&managed)
            .await
            .unwrap()
            .as_deref(),
        Some("2.3.2")
    );
    harness
        .service
        .set_enrollment_runtime_binding_resolver(StaticEnrollmentRuntimeBindingResolver::missing());
    harness.sink.release.notify_one();
    execution.await.unwrap().unwrap();

    assert_eq!(
        harness.repository.get_task(&task_id).await.unwrap().status,
        TaskStatus::Failed,
        "a disabled projection restores its old runtime without credential readiness"
    );
    assert_eq!(
        harness
            .distribution
            .active_version(&managed)
            .await
            .unwrap()
            .as_deref(),
        Some("2.3.1")
    );
}

#[tokio::test]
async fn ready_credentials_keep_enabled_update_and_repair_enabled() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", 'e').await;
    let v2 = harness.save_version("2.3.2", 'f').await;
    let (managed, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1.clone(),
            },
            "ready_enabled_update_repair_seed",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());

    let seeded = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    let manifest = harness.repository.get_manifest(&v1).await.unwrap();
    harness.service.set_enrollment_runtime_binding_resolver(
        StaticEnrollmentRuntimeBindingResolver::ready(
            &managed,
            seeded.managed.revision,
            &v1,
            &manifest.verified.manifest().auth,
        ),
    );
    harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.clone(),
                enabled: true,
                expected_revision: seeded.managed.revision,
            },
        )
        .await
        .unwrap();

    let (_, update_task) = harness
        .confirm(
            PlanIntent::Update {
                managed_mcp_id: managed.clone(),
                target_version: "2.3.2".to_string(),
            },
            "ready_enabled_update",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&update_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );

    let updated = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    let updated_manifest = harness.repository.get_manifest(&v2).await.unwrap();
    harness.service.set_enrollment_runtime_binding_resolver(
        StaticEnrollmentRuntimeBindingResolver::ready(
            &managed,
            updated.managed.revision,
            &v2,
            &updated_manifest.verified.manifest().auth,
        ),
    );
    let (_, repair_task) = harness
        .confirm(
            PlanIntent::Repair {
                managed_mcp_id: managed.clone(),
            },
            "ready_enabled_repair",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&repair_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );
    let projection = harness
        .repository
        .get_connection_projection(&managed)
        .await
        .unwrap();
    assert!(
        harness
            .repository
            .get_managed_inventory(&managed)
            .await
            .unwrap()
            .managed
            .state
            .default_enabled
    );
    assert!(
        harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled
    );
    assert!(harness
        .distribution
        .active_version(&managed)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn managed_default_enabled_survives_update_cancel_and_repair_rollback() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", 'e').await;
    let _v2 = harness.save_version("2.3.2", 'f').await;
    let (managed, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1,
            },
            "default_enabled_seed",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());

    let installed = harness
        .service
        .managed_get(
            &harness.context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap()
        .summary;
    assert!(!installed.default_enabled);
    let projection = harness
        .repository
        .get_connection_projection(&managed)
        .await
        .unwrap();
    assert!(
        !harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled
    );

    let enabled = harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.clone(),
                enabled: true,
                expected_revision: installed.revision,
            },
        )
        .await
        .unwrap();
    assert!(enabled.default_enabled);

    harness.sink.block_commit_once.store(true, Ordering::SeqCst);
    let (_, update_task) = harness
        .confirm(
            PlanIntent::Update {
                managed_mcp_id: managed.clone(),
                target_version: "2.3.2".to_string(),
            },
            "default_enabled_cancel_update",
        )
        .await;
    let service = harness.service.clone();
    let execution = tokio::spawn(async move { service.runner_tick().await });
    tokio::time::timeout(
        Duration::from_secs(5),
        harness.sink.commit_entered.notified(),
    )
    .await
    .unwrap();
    let running = harness.repository.get_task(&update_task).await.unwrap();
    harness
        .service
        .task_cancel(
            &harness.context(),
            TaskCancelInput {
                task_id: update_task.clone(),
                expected_revision: running.revision,
            },
        )
        .await
        .unwrap();
    harness.sink.commit_release.notify_one();
    execution.await.unwrap().unwrap();
    assert_eq!(
        harness
            .repository
            .get_task(&update_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Cancelled
    );
    let after_cancel = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    assert_eq!(
        after_cancel.lifecycle.active_version.as_deref(),
        Some("2.3.1")
    );
    assert!(after_cancel.managed.state.default_enabled);
    assert!(
        harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled
    );

    harness.health.healthy.store(false, Ordering::SeqCst);
    let (_, repair_task) = harness
        .confirm(
            PlanIntent::Repair {
                managed_mcp_id: managed.clone(),
            },
            "default_enabled_repair_rollback",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&repair_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Failed
    );
    let after_repair_failure = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    assert_eq!(
        after_repair_failure.lifecycle.active_version.as_deref(),
        Some("2.3.1")
    );
    assert!(after_repair_failure.managed.state.default_enabled);
    assert!(
        harness
            .sink
            .get(&projection.link_key)
            .await
            .unwrap()
            .unwrap()
            .entry
            .enabled
    );
}

#[tokio::test]
async fn effect_before_commit_restarts_preserve_snapshot_and_do_not_publish_before_health() {
    for phase in ["materialize", "pointer", "live_config"] {
        let harness = ManagedHarness::new().await;
        let digest = harness.save_version("2.3.1", '3').await;
        let (managed, task_id) = harness
            .confirm(
                PlanIntent::Install {
                    manifest_digest: digest,
                },
                phase,
            )
            .await;
        match phase {
            "materialize" => harness
                .distribution
                .block_install_once
                .store(true, Ordering::SeqCst),
            "pointer" => harness
                .distribution
                .block_activate_once
                .store(true, Ordering::SeqCst),
            "live_config" => harness.sink.block_commit_once.store(true, Ordering::SeqCst),
            _ => unreachable!(),
        }
        let service = harness.service.clone();
        let execution = tokio::spawn(async move { service.runner_tick().await });
        match phase {
            "materialize" => harness.distribution.install_entered.notified().await,
            "pointer" => harness.distribution.activate_entered.notified().await,
            "live_config" => tokio::time::timeout(
                Duration::from_secs(5),
                harness.sink.commit_entered.notified(),
            )
            .await
            .unwrap(),
            _ => unreachable!(),
        }
        if phase != "live_config" {
            assert!(harness.sink.entries.lock().unwrap().is_empty(), "{phase}");
        }
        execution.abort();
        let _ = execution.await;
        harness.clock.set(100_000);
        let runner = harness.restarted_runner();
        runner.recover_startup().await.unwrap();
        let recovered = harness.repository.get_task(&task_id).await.unwrap();
        let recovered_steps = harness.repository.list_task_steps(&task_id).await.unwrap();
        assert!(
            runner.tick().await.unwrap(),
            "{phase}: recovered={recovered:?} steps={recovered_steps:?}"
        );
        assert_eq!(
            harness.repository.get_task(&task_id).await.unwrap().status,
            TaskStatus::Succeeded,
            "{phase}"
        );
        assert_eq!(
            harness
                .distribution
                .active_version(&managed)
                .await
                .unwrap()
                .as_deref(),
            Some("2.3.1")
        );
    }
}

#[tokio::test]
async fn health_failure_rolls_back_and_uninstall_finalize_failure_forward_resumes() {
    let harness = ManagedHarness::new().await;
    let digest = harness.save_version("2.3.1", '4').await;
    harness.health.healthy.store(false, Ordering::SeqCst);
    let (managed, failed_task) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: digest.clone(),
            },
            "unhealthy",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    let failed_record = harness.repository.get_task(&failed_task).await.unwrap();
    let failed_steps = harness
        .repository
        .list_task_steps(&failed_task)
        .await
        .unwrap();
    assert_eq!(
        failed_record.status,
        TaskStatus::Failed,
        "task={failed_record:?} steps={failed_steps:?}"
    );
    assert!(harness
        .distribution
        .active_version(&managed)
        .await
        .unwrap()
        .is_none());
    harness.health.healthy.store(true, Ordering::SeqCst);
    let (_, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: digest,
            },
            "healthy",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    harness
        .distribution
        .fail_purge_once
        .store(true, Ordering::SeqCst);
    let (_, uninstall_task) = harness
        .confirm(
            PlanIntent::Uninstall {
                managed_mcp_id: managed.clone(),
                preserve_user_data: true,
            },
            "forward_uninstall",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&uninstall_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Queued
    );
    assert!(harness.service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&uninstall_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );
    assert!(harness
        .distribution
        .active_version(&managed)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn lifecycle_lease_releases_on_reject_and_queued_cancel_but_not_running_cancel() {
    let harness = ManagedHarness::new().await;
    let digest = harness.save_version("2.3.1", '8').await;
    let rejected_plan = harness
        .service
        .plan_create(
            &harness.context(),
            PlanCreateInput {
                intent: PlanIntent::Install {
                    manifest_digest: digest.clone(),
                },
                idempotency_key: "plan_rejected".to_string(),
            },
        )
        .await
        .unwrap();
    let rejected = harness
        .service
        .install_confirm(
            &harness.context(),
            InstallConfirmInput {
                plan_id: rejected_plan.plan_id,
                plan_digest: rejected_plan.plan_digest,
                decision: UserDecision::Reject,
                idempotency_key: "task_rejected".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, TaskStatus::Cancelled);

    let (_, queued_task) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: digest.clone(),
            },
            "queued_cancel",
        )
        .await;
    let queued = harness.repository.get_task(&queued_task).await.unwrap();
    let cancelled = harness
        .service
        .task_cancel(
            &harness.context(),
            TaskCancelInput {
                task_id: queued_task.clone(),
                expected_revision: queued.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    let repeated = harness
        .service
        .task_cancel(
            &harness.context(),
            TaskCancelInput {
                task_id: queued_task,
                expected_revision: cancelled.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(repeated.status, TaskStatus::Cancelled);

    harness
        .distribution
        .block_install_once
        .store(true, Ordering::SeqCst);
    let (_, running_task) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: digest.clone(),
            },
            "running_cancel",
        )
        .await;
    let service = harness.service.clone();
    let execution = tokio::spawn(async move { service.runner_tick().await });
    harness.distribution.install_entered.notified().await;
    let running = harness.repository.get_task(&running_task).await.unwrap();
    let cancelling = harness
        .service
        .task_cancel(
            &harness.context(),
            TaskCancelInput {
                task_id: running_task.clone(),
                expected_revision: running.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelling.status, TaskStatus::Cancelling);
    let conflict_plan = harness
        .service
        .plan_create(
            &harness.context(),
            PlanCreateInput {
                intent: PlanIntent::Install {
                    manifest_digest: digest.clone(),
                },
                idempotency_key: "plan_running_conflict".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        harness
            .service
            .install_confirm(
                &harness.context(),
                InstallConfirmInput {
                    plan_id: conflict_plan.plan_id,
                    plan_digest: conflict_plan.plan_digest,
                    decision: UserDecision::Confirm,
                    idempotency_key: "task_running_conflict".to_string(),
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanConflict
    );
    harness.distribution.install_release.notify_one();
    execution.await.unwrap().unwrap();
    assert_eq!(
        harness
            .repository
            .get_task(&running_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Cancelled
    );
    let (_, after_cancel) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: digest,
            },
            "after_running_cancel",
        )
        .await;
    assert_eq!(
        harness
            .repository
            .get_task(&after_cancel)
            .await
            .unwrap()
            .status,
        TaskStatus::Queued
    );
}

#[tokio::test]
async fn lifecycle_and_projection_mutations_are_mutually_exclusive_and_recovery_unblocks() {
    let harness = ManagedHarness::new().await;
    let v1 = harness.save_version("2.3.1", '9').await;
    let _v2 = harness.save_version("2.3.2", 'a').await;
    let (managed, _) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: v1,
            },
            "projection_install",
        )
        .await;
    harness.service.runner_tick().await.unwrap();
    let installed = harness
        .service
        .managed_get(
            &harness.context(),
            ManagedGetInput {
                managed_mcp_id: managed.clone(),
            },
        )
        .await
        .unwrap();

    harness.sink.block_commit_once.store(true, Ordering::SeqCst);
    let service = harness.service.clone();
    let managed_for_toggle = managed.clone();
    let revision = installed.summary.revision;
    let toggle = tokio::spawn(async move {
        service
            .set_default_enabled(
                &service.trusted_local_context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed_for_toggle,
                    enabled: true,
                    expected_revision: revision,
                },
            )
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        harness.sink.commit_entered.notified(),
    )
    .await
    .unwrap();
    let update_plan = harness
        .service
        .plan_create(
            &harness.context(),
            PlanCreateInput {
                intent: PlanIntent::Update {
                    managed_mcp_id: managed.clone(),
                    target_version: "2.3.2".to_string(),
                },
                idempotency_key: "plan_projection_busy".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        harness
            .service
            .install_confirm(
                &harness.context(),
                InstallConfirmInput {
                    plan_id: update_plan.plan_id,
                    plan_digest: update_plan.plan_digest,
                    decision: UserDecision::Confirm,
                    idempotency_key: "task_projection_busy".to_string(),
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanConflict
    );
    harness.sink.commit_release.notify_one();
    toggle.await.unwrap().unwrap();

    let (_, queued_update) = harness
        .confirm(
            PlanIntent::Update {
                managed_mcp_id: managed.clone(),
                target_version: "2.3.2".to_string(),
            },
            "lifecycle_blocks_toggle",
        )
        .await;
    let lifecycle_locked = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    assert_eq!(
        harness
            .service
            .set_default_enabled(
                &harness.context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed.clone(),
                    enabled: false,
                    expected_revision: lifecycle_locked.managed.revision,
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanConflict
    );
    let queued_update_record = harness.repository.get_task(&queued_update).await.unwrap();
    harness
        .service
        .task_cancel(
            &harness.context(),
            TaskCancelInput {
                task_id: queued_update,
                expected_revision: queued_update_record.revision,
            },
        )
        .await
        .unwrap();

    let current = harness
        .repository
        .get_managed_inventory(&managed)
        .await
        .unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&harness.database_path))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO projection_mutations(managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,created_at_ms,updated_at_ms) VALUES (?, ?, 1, 0, 'recovery_required', 200, 201)",
    )
    .bind(&managed)
    .bind(current.managed.revision)
    .execute(&pool)
    .await
    .unwrap();
    let repair_plan = harness
        .service
        .plan_create(
            &harness.context(),
            PlanCreateInput {
                intent: PlanIntent::Repair {
                    managed_mcp_id: managed.clone(),
                },
                idempotency_key: "plan_projection_recovery".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        harness
            .service
            .install_confirm(
                &harness.context(),
                InstallConfirmInput {
                    plan_id: repair_plan.plan_id,
                    plan_digest: repair_plan.plan_digest,
                    decision: UserDecision::Confirm,
                    idempotency_key: "task_projection_recovery".to_string(),
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanConflict
    );
    let recovery_result = harness
        .service
        .set_default_enabled(
            &harness.context(),
            SetDefaultEnabledInput {
                managed_mcp_id: managed.clone(),
                enabled: false,
                expected_revision: current.managed.revision,
            },
        )
        .await;
    match recovery_result {
        Ok(summary) => assert!(!summary.default_enabled),
        Err(error) => assert_eq!(error.code(), McpPlatformErrorCode::RevisionConflict),
    }
    assert!(!harness
        .repository
        .projection_recovery_required(&managed)
        .await
        .unwrap());
    pool.close().await;
    let (_, repair_task) = harness
        .confirm(
            PlanIntent::Repair {
                managed_mcp_id: managed,
            },
            "after_projection_recovery",
        )
        .await;
    assert_eq!(
        harness
            .repository
            .get_task(&repair_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Queued
    );
}

#[tokio::test]
async fn persistent_finalization_failure_stops_hot_loop_and_explicit_retry_preserves_steps() {
    let harness = ManagedHarness::new().await;
    let digest = harness.save_version("2.3.1", 'b').await;
    let (managed, install_task) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: digest,
            },
            "finalize_install",
        )
        .await;
    harness.service.runner_tick().await.unwrap();
    harness
        .distribution
        .always_fail_purge
        .store(true, Ordering::SeqCst);
    let (_, uninstall_task) = harness
        .confirm(
            PlanIntent::Uninstall {
                managed_mcp_id: managed.clone(),
                preserve_user_data: true,
            },
            "persistent_finalize",
        )
        .await;
    assert!(harness.service.runner_tick().await.unwrap());
    assert_eq!(
        harness
            .repository
            .get_task(&uninstall_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Queued
    );
    assert!(harness.service.runner_tick().await.unwrap());
    let recovery = harness.repository.get_task(&uninstall_task).await.unwrap();
    assert_eq!(recovery.status, TaskStatus::RecoveryRequired);
    assert!(!harness.service.runner_tick().await.unwrap());
    let preserved = harness
        .repository
        .list_task_steps(&uninstall_task)
        .await
        .unwrap();

    let retry = harness
        .service
        .task_retry(
            &harness.context(),
            TaskRetryInput {
                task_id: uninstall_task.clone(),
                expected_revision: recovery.revision,
                idempotency_key: "resume_finalize".to_string(),
            },
        )
        .await
        .unwrap();
    let replay = harness
        .service
        .task_retry(
            &harness.context(),
            TaskRetryInput {
                task_id: uninstall_task.clone(),
                expected_revision: retry.revision,
                idempotency_key: "resume_finalize".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(retry.task_id, replay.task_id);
    assert_eq!(
        harness
            .repository
            .get_task(&uninstall_task)
            .await
            .unwrap()
            .attempt_count,
        1
    );
    assert_eq!(
        harness
            .repository
            .list_task_steps(&uninstall_task)
            .await
            .unwrap(),
        preserved
    );
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&harness.database_path))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM task_step_history WHERE task_id = ?")
            .bind(&uninstall_task)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM health_observations WHERE task_id = ?")
            .bind(&install_task)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    harness
        .distribution
        .always_fail_purge
        .store(false, Ordering::SeqCst);
    assert!(harness.service.runner_tick().await.unwrap());
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
async fn ordinary_retry_reclaims_lifecycle_lease_atomically_and_archives_old_steps() {
    let harness = ManagedHarness::new().await;
    let digest = harness.save_version("2.3.1", 'c').await;
    harness.health.healthy.store(false, Ordering::SeqCst);
    let (_, failed_task) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: digest.clone(),
            },
            "retry_failed",
        )
        .await;
    harness.service.runner_tick().await.unwrap();
    let failed = harness.repository.get_task(&failed_task).await.unwrap();
    assert_eq!(failed.status, TaskStatus::Failed);

    let (_, occupying_task) = harness
        .confirm(
            PlanIntent::Install {
                manifest_digest: digest,
            },
            "retry_occupier",
        )
        .await;
    assert_eq!(
        harness
            .service
            .task_retry(
                &harness.context(),
                TaskRetryInput {
                    task_id: failed_task.clone(),
                    expected_revision: failed.revision,
                    idempotency_key: "retry_conflict".to_string(),
                },
            )
            .await
            .unwrap_err()
            .code(),
        McpPlatformErrorCode::PlanConflict
    );
    assert_eq!(
        harness
            .repository
            .get_task(&failed_task)
            .await
            .unwrap()
            .attempt_count,
        0
    );
    let occupying = harness.repository.get_task(&occupying_task).await.unwrap();
    harness
        .service
        .task_cancel(
            &harness.context(),
            TaskCancelInput {
                task_id: occupying_task,
                expected_revision: occupying.revision,
            },
        )
        .await
        .unwrap();
    let retried = harness
        .service
        .task_retry(
            &harness.context(),
            TaskRetryInput {
                task_id: failed_task.clone(),
                expected_revision: failed.revision,
                idempotency_key: "retry_success".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(retried.status, TaskStatus::Queued);
    assert!(harness
        .repository
        .list_task_steps(&failed_task)
        .await
        .unwrap()
        .is_empty());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&harness.database_path))
        .await
        .unwrap();
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM task_step_history WHERE task_id = ?")
            .bind(&failed_task)
            .fetch_one(&pool)
            .await
            .unwrap()
            > 0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM health_observations WHERE task_id = ?")
            .bind(&failed_task)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    harness.health.healthy.store(true, Ordering::SeqCst);
    harness.service.runner_tick().await.unwrap();
    assert_eq!(
        harness
            .repository
            .get_task(&failed_task)
            .await
            .unwrap()
            .status,
        TaskStatus::Succeeded
    );
}
