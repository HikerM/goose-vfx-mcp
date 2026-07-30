use crate as goose;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use goose::mcp_platform::{
    parse_manifest, Clock, IdGenerator, InMemoryIntegritySigner, InstallConfirmInput,
    InstallationScope, ManifestProof, ManifestRecord, ManifestSourceMetadata,
    ManualConnectionInput, ManualHttpAuth, ManualPlanCreateInput, ManualStdioProvider,
    ManualStdioSource, McpPlatformError, McpPlatformErrorCode, McpPlatformResult,
    McpPlatformService, McpPlatformServiceOptions, PlanCreateInput, PlanIntent, ProfileCreateInput,
    RemoteHttpNetworkPolicy, RequestContext, ResolvedManualStdioSource,
    SqliteMcpPlatformRepository, TrustTier, UnavailableRemoteHttpNetworkPolicy, UserDecision,
};

const REMOTE: &str =
    include_str!("../../../../documentation/static/schemas/examples/remote-http.json");
const MANUAL: &str =
    include_str!("../../../../documentation/static/schemas/examples/manual-stdio.json");

fn integrity_signer() -> Arc<dyn goose::mcp_platform::IntegritySigner> {
    InMemoryIntegritySigner::new_for_testing([0x4a; 32])
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

struct DenyExampleNetwork;

#[async_trait]
impl RemoteHttpNetworkPolicy for DenyExampleNetwork {
    fn validate_endpoint(&self, endpoint: &str) -> McpPlatformResult<()> {
        if endpoint.contains("mcp.example.com") {
            Err(McpPlatformError::new(
                McpPlatformErrorCode::UnsafeUrl,
                "fake DNS policy resolved the endpoint to a denied private address",
            ))
        } else {
            VerifiedPublicNetwork.validate_endpoint(endpoint)
        }
    }

    async fn validate_for_plan(
        &self,
        endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<()> {
        self.validate_endpoint(endpoint)
    }
}

struct VerifiedPublicNetwork;

#[async_trait]
impl RemoteHttpNetworkPolicy for VerifiedPublicNetwork {
    fn validate_endpoint(&self, endpoint: &str) -> McpPlatformResult<()> {
        let url = url::Url::parse(endpoint).map_err(|_| {
            McpPlatformError::new(McpPlatformErrorCode::UnsafeUrl, "test policy rejected URL")
        })?;
        let host = url.host_str().unwrap_or_default();
        if url.scheme() != "https"
            || host.eq_ignore_ascii_case("localhost")
            || host.parse::<std::net::IpAddr>().is_ok()
            || host.ends_with(".internal")
        {
            Err(McpPlatformError::new(
                McpPlatformErrorCode::UnsafeUrl,
                "test policy rejected non-public peer",
            ))
        } else {
            Ok(())
        }
    }

    async fn validate_for_plan(
        &self,
        endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<()> {
        self.validate_endpoint(endpoint)
    }
}

struct FakeStdioProvider;

impl ManualStdioProvider for FakeStdioProvider {
    fn list_sources(&self) -> McpPlatformResult<Vec<ManualStdioSource>> {
        Ok(vec![ManualStdioSource {
            source_id: "trusted_stdio_1".to_string(),
            display_name: "Trusted local stdio MCP".to_string(),
            publisher_name: "Fixture publisher".to_string(),
            trust_tier: TrustTier::Local,
            compatible: true,
        }])
    }

    fn resolve(&self, source_id: &str) -> McpPlatformResult<ResolvedManualStdioSource> {
        if source_id != "trusted_stdio_1" {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::NotFound,
                "manual stdio source was not found",
            ));
        }
        Ok(ResolvedManualStdioSource {
            verified: parse_manifest(MANUAL.as_bytes()).unwrap(),
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: ManifestSourceMetadata::local_persistence(),
        })
    }
}

fn service_for_repository(
    repository: Arc<SqliteMcpPlatformRepository>,
    policy: Arc<dyn RemoteHttpNetworkPolicy>,
) -> McpPlatformService {
    McpPlatformService::new_with_remote_http_network_policy(
        repository,
        Arc::new(TestClock(AtomicI64::new(1_000))),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions::default(),
        policy,
    )
}

async fn repository() -> (tempfile::TempDir, PathBuf, Arc<SqliteMcpPlatformRepository>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("platform.db");
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            path.clone(),
            integrity_signer(),
        )
        .await
        .unwrap(),
    );
    (directory, path, repository)
}

async fn service() -> (
    tempfile::TempDir,
    PathBuf,
    Arc<SqliteMcpPlatformRepository>,
    McpPlatformService,
) {
    let (directory, path, repository) = repository().await;
    let service = service_for_repository(
        repository.clone(),
        Arc::new(UnavailableRemoteHttpNetworkPolicy),
    );
    (directory, path, repository, service)
}

async fn save_remote_manifest(repository: &SqliteMcpPlatformRepository) -> String {
    let manifest = parse_manifest(REMOTE.as_bytes()).unwrap();
    let digest = manifest.digest().to_string();
    repository
        .save_manifest(&ManifestRecord {
            verified: manifest,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: Default::default(),
            created_at_ms: 900,
        })
        .await
        .unwrap();
    digest
}

async fn profile_apply_plan_exists_for_idempotency_key(path: &Path, idempotency_key: &str) -> bool {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(1) FROM mcp_profile_apply_plans WHERE idempotency_key=?",
    )
    .bind(idempotency_key)
    .fetch_one(&pool)
    .await
    .unwrap();
    pool.close().await;
    count > 0
}

fn public_service(repository: Arc<SqliteMcpPlatformRepository>) -> McpPlatformService {
    McpPlatformService::new(
        repository,
        Arc::new(TestClock(AtomicI64::new(1_000))),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions::default(),
    )
}

fn public_production_service(repository: Arc<SqliteMcpPlatformRepository>) -> McpPlatformService {
    McpPlatformService::production(repository)
}

#[tokio::test]
async fn manual_http_uses_the_same_final_network_policy_as_catalog_plans() {
    let (_directory, _path, repository, _service) = service().await;
    let service = service_for_repository(repository.clone(), Arc::new(DenyExampleNetwork));
    let context = test_context("mcp-platform-4a-manual-http");
    let error = service
        .manual_plan_create(
            &context,
            ManualPlanCreateInput {
                connection: ManualConnectionInput::RemoteHttp {
                    endpoint: "https://mcp.example.com/v1".to_string(),
                    auth: ManualHttpAuth::None,
                },
                idempotency_key: "manual-denied".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::UnsafeUrl);
    assert!(repository.list_manifests().await.unwrap().is_empty());

    let manifest = parse_manifest(REMOTE.as_bytes()).unwrap();
    let digest = manifest.digest().to_string();
    repository
        .save_manifest(&ManifestRecord {
            verified: manifest,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            source_metadata: Default::default(),
            created_at_ms: 900,
        })
        .await
        .unwrap();
    let error = service
        .plan_create(
            &context,
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest: digest,
                    installation_scope: goose::mcp_platform::InstallationScope::User,
                },
                idempotency_key: "catalog-denied".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::UnsafeUrl);
}

#[tokio::test]
async fn manual_http_rejects_loopback_private_literals_and_persists_a_disabled_plan() {
    let (_directory, _path, repository, _service) = service().await;
    let service = service_for_repository(repository.clone(), Arc::new(VerifiedPublicNetwork));
    let context = test_context("mcp-platform-4a-loopback");
    for endpoint in [
        "https://localhost/mcp",
        "https://127.0.0.1/mcp",
        "https://10.0.0.1/mcp",
        "https://metadata.internal/mcp",
        "http://mcp.example.com/mcp",
    ] {
        let error = service
            .manual_plan_create(
                &context,
                ManualPlanCreateInput {
                    connection: ManualConnectionInput::RemoteHttp {
                        endpoint: endpoint.to_string(),
                        auth: ManualHttpAuth::None,
                    },
                    idempotency_key: format!("deny-{}", endpoint.len()),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::UnsafeUrl);
        assert!(repository.list_manifests().await.unwrap().is_empty());
    }

    let review = service
        .manual_plan_create(
            &context,
            ManualPlanCreateInput {
                connection: ManualConnectionInput::RemoteHttp {
                    endpoint: "https://safe.example.com/mcp".to_string(),
                    auth: ManualHttpAuth::None,
                },
                idempotency_key: "safe-http-plan".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(!review.plan.default_enabled());
    assert_eq!(review.plan.manifest_digest().len(), 64);
    let task = service
        .install_confirm(
            &context,
            InstallConfirmInput {
                plan_id: review.plan_id,
                plan_digest: review.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "safe-http-confirm".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(task.status, goose::mcp_platform::TaskStatus::Queued);
    assert!(task.redacted_error.is_none());
}

#[tokio::test]
async fn manual_stdio_is_opaque_and_provider_unavailability_is_closed() {
    let (_directory, _path, _repository, service) = service().await;
    let context = test_context("mcp-platform-4a-stdio");
    let sources = service.manual_stdio_sources_list(&context).unwrap();
    assert!(!sources.available);
    assert!(sources.items.is_empty());
    let error = service
        .manual_plan_create(
            &context,
            ManualPlanCreateInput {
                connection: ManualConnectionInput::StdioProvider {
                    source_id: "anything".to_string(),
                },
                idempotency_key: "unsupported-stdio".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        McpPlatformErrorCode::ManualStdioProviderUnavailable
    );

    let service = service.with_manual_stdio_provider(Arc::new(FakeStdioProvider));
    let sources = service.manual_stdio_sources_list(&context).unwrap();
    assert!(sources.available);
    assert_eq!(sources.items[0].source_id, "trusted_stdio_1");
    let review = service
        .manual_plan_create(
            &context,
            ManualPlanCreateInput {
                connection: ManualConnectionInput::StdioProvider {
                    source_id: "trusted_stdio_1".to_string(),
                },
                idempotency_key: "opaque-stdio".to_string(),
            },
        )
        .await
        .unwrap();
    assert!(!review.plan.default_enabled());
}

#[tokio::test]
async fn public_service_constructors_are_read_only_even_with_public_request_context() {
    let (_directory, path, repository, _fixture_service) = service().await;
    let digest = save_remote_manifest(repository.as_ref()).await;
    let context = test_context("public-service-read-only");

    let service = public_service(repository.clone());
    let production = public_production_service(repository.clone());
    let generated_ids = TestIds::default();
    let profile_apply_plan_id = generated_ids.next_id("profile_plan");
    let profile_apply_confirmation = generated_ids.next_id("profile_apply_confirmation");

    let catalog = service
        .catalog_list(&context, Default::default())
        .await
        .unwrap();
    assert_eq!(catalog.items.len(), 1);
    assert_eq!(catalog.items[0].manifest_digest, digest);

    for error in [
        service
            .manual_plan_create(
                &context,
                ManualPlanCreateInput {
                    connection: ManualConnectionInput::RemoteHttp {
                        endpoint: "https://safe.example.com/mcp".to_string(),
                        auth: ManualHttpAuth::None,
                    },
                    idempotency_key: "public-manual".to_string(),
                },
            )
            .await
            .unwrap_err(),
        service
            .plan_create(
                &context,
                PlanCreateInput {
                    intent: PlanIntent::Register {
                        manifest_digest: digest.clone(),
                        installation_scope: InstallationScope::User,
                    },
                    idempotency_key: "public-register".to_string(),
                },
            )
            .await
            .unwrap_err(),
        service
            .profile_create(
                &context,
                ProfileCreateInput {
                    name: "Forbidden".to_string(),
                    description: String::new(),
                    managed_mcp_ids: Vec::new(),
                    idempotency_key: "public-profile".to_string(),
                },
            )
            .await
            .unwrap_err(),
        service
            .profile_update(
                &context,
                goose::mcp_platform::ProfileUpdateInput {
                    profile_id: "profile_1".to_string(),
                    expected_revision: 1,
                    name: "Forbidden".to_string(),
                    description: String::new(),
                    managed_mcp_ids: Vec::new(),
                    idempotency_key: "public-profile-update".to_string(),
                },
            )
            .await
            .unwrap_err(),
        service
            .profile_restore(
                &context,
                goose::mcp_platform::ProfileRestoreInput {
                    profile_id: "profile_1".to_string(),
                    source_revision: 1,
                    expected_revision: 1,
                    idempotency_key: "public-profile-restore".to_string(),
                },
            )
            .await
            .unwrap_err(),
        service
            .profile_archive(
                &context,
                goose::mcp_platform::ProfileArchiveInput {
                    profile_id: "profile_1".to_string(),
                    expected_revision: 1,
                    idempotency_key: "public-profile-archive".to_string(),
                },
            )
            .await
            .unwrap_err(),
        service
            .profile_apply_plan_create(
                &context,
                goose::mcp_platform::ProfileApplyPlanInput {
                    profile_id: "profile_1".to_string(),
                    profile_revision: 1,
                    idempotency_key: "public-profile-apply-plan".to_string(),
                },
            )
            .await
            .unwrap_err(),
        service
            .profile_apply_confirm(
                &context,
                &profile_apply_plan_id,
                &profile_apply_confirmation,
                true,
            )
            .await
            .unwrap_err(),
        production
            .plan_create(
                &context,
                PlanCreateInput {
                    intent: PlanIntent::Register {
                        manifest_digest: digest,
                        installation_scope: InstallationScope::User,
                    },
                    idempotency_key: "public-production-register".to_string(),
                },
            )
            .await
            .unwrap_err(),
    ] {
        assert_eq!(error.code(), McpPlatformErrorCode::PolicyDenied);
    }

    assert!(repository
        .get_plan_by_idempotency_key("public-register")
        .await
        .unwrap()
        .is_none());
    assert!(repository
        .get_plan_by_idempotency_key("public-production-register")
        .await
        .unwrap()
        .is_none());
    assert!(repository.list_profiles(true).await.unwrap().is_empty());
    assert!(
        !profile_apply_plan_exists_for_idempotency_key(&path, "public-profile-apply-plan").await
    );
}

#[test]
fn public_service_api_no_longer_exposes_di_escape_hatches() {
    let source = include_str!("service/application.rs");
    assert!(!source.contains("pub fn new_with_remote_http_network_policy("));
    assert!(!source.contains("pub fn with_manual_stdio_provider("));
}
