use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use goose::custom_requests::{
    McpCatalogDetail, McpInstallConfirmRequest, McpManualPlanCreateRequest, McpTaskRef,
};
use goose::mcp_platform::{
    parse_manifest, Clock, IdGenerator, InstallConfirmInput, ManifestProof, ManifestRecord,
    ManualConnectionInput, ManualHttpAuth, ManualPlanCreateInput, ManualStdioProvider,
    ManualStdioSource, McpPlatformError, McpPlatformErrorCode, McpPlatformResult,
    McpPlatformService, McpPlatformServiceOptions, PlanCreateInput, PlanIntent,
    RemoteHttpNetworkPolicy, ResolvedManualStdioSource, SqliteMcpPlatformRepository, TrustTier,
    UnavailableRemoteHttpNetworkPolicy, UserDecision,
};

const REMOTE: &str =
    include_str!("../../../documentation/static/schemas/examples/remote-http.json");
const MANUAL: &str =
    include_str!("../../../documentation/static/schemas/examples/manual-stdio.json");

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
        })
    }
}

async fn service() -> (
    tempfile::TempDir,
    Arc<SqliteMcpPlatformRepository>,
    McpPlatformService,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path(directory.path().join("platform.db"))
            .await
            .unwrap(),
    );
    let service = service_for_repository(
        repository.clone(),
        Arc::new(UnavailableRemoteHttpNetworkPolicy),
    );
    (directory, repository, service)
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

#[tokio::test]
async fn catalog_and_sources_expose_core_eligibility_and_safe_cache_state() {
    let (_directory, repository, service) = service().await;
    let manifest = parse_manifest(REMOTE.as_bytes()).unwrap();
    repository
        .save_manifest(&ManifestRecord {
            verified: manifest,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            created_at_ms: 900,
        })
        .await
        .unwrap();
    let context = service.trusted_local_context();
    let page = service
        .catalog_list(&context, Default::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].version, "1.4.2");
    assert_eq!(page.items[0].manifest_digest.len(), 64);
    assert!(page.offline);
    assert!(page.local_persistence_only);

    let state = service.sources_policy_get(&context).await.unwrap();
    assert_eq!(state.sources[0].source_id, "local_persistence");
    assert_eq!(state.sources[0].manifest_count, 1);
    assert_eq!(
        state.sources[0].compatibility.compatible
            + state.sources[0].compatibility.restricted
            + state.sources[0].compatibility.denied,
        1
    );
}

#[tokio::test]
async fn manual_http_uses_the_same_final_network_policy_as_catalog_plans() {
    let (_directory, repository, _service) = service().await;
    let service = service_for_repository(repository.clone(), Arc::new(DenyExampleNetwork));
    let context = service.trusted_local_context();
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
    let (_directory, repository, _service) = service().await;
    let service = service_for_repository(repository.clone(), Arc::new(VerifiedPublicNetwork));
    let context = service.trusted_local_context();
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
async fn manual_http_default_denies_and_credentials_remain_unpersisted() {
    let (_directory, repository, service) = service().await;
    let context = service.trusted_local_context();
    let denied = service
        .manual_plan_create(
            &context,
            ManualPlanCreateInput {
                connection: ManualConnectionInput::RemoteHttp {
                    endpoint: "https://safe.example.com/mcp".to_string(),
                    auth: ManualHttpAuth::None,
                },
                idempotency_key: "default-deny".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        denied.code(),
        McpPlatformErrorCode::RemoteHttpPolicyUnavailable
    );
    assert!(repository.list_manifests().await.unwrap().is_empty());

    let remote = parse_manifest(REMOTE.as_bytes()).unwrap();
    let digest = remote.digest().to_string();
    repository
        .save_manifest(&ManifestRecord {
            verified: remote,
            proof: ManifestProof::LocalBytes,
            trust_tier: TrustTier::Local,
            created_at_ms: 1_000,
        })
        .await
        .unwrap();
    let ordinary_denied = service
        .plan_create(
            &context,
            PlanCreateInput {
                intent: PlanIntent::Register {
                    manifest_digest: digest,
                    installation_scope: goose::mcp_platform::InstallationScope::User,
                },
                idempotency_key: "ordinary-default-deny".to_string(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        ordinary_denied.code(),
        McpPlatformErrorCode::CredentialMissing
    );

    let service = service_for_repository(repository.clone(), Arc::new(VerifiedPublicNetwork));
    for auth_reference in ["opaque_handle_a", "opaque_handle_b"] {
        let error = service
            .manual_plan_create(
                &context,
                ManualPlanCreateInput {
                    connection: ManualConnectionInput::RemoteHttp {
                        endpoint: "https://safe.example.com/mcp".to_string(),
                        auth: ManualHttpAuth::BearerReference {
                            auth_reference: auth_reference.to_string(),
                        },
                    },
                    idempotency_key: format!("credential-{auth_reference}"),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::CredentialMissing);
    }
    assert_eq!(repository.list_manifests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn manual_stdio_is_opaque_and_provider_unavailability_is_closed() {
    let (_directory, _repository, service) = service().await;
    let context = service.trusted_local_context();
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

#[test]
fn wire_schemas_are_closed_and_never_accept_raw_execution_or_auth_material() {
    let manual_schema =
        serde_json::to_string(&schemars::schema_for!(McpManualPlanCreateRequest)).unwrap();
    for forbidden in [
        "executable",
        "argv",
        "headers",
        "headerName",
        "environmentKey",
        "credentialName",
        "credentialValue",
        "cwd",
        "command",
        "proxy",
        "socket",
        "tlsBypass",
        "dangerAcceptInvalidCerts",
    ] {
        assert!(!manual_schema.contains(forbidden), "found {forbidden}");
    }
    let detail_schema = serde_json::to_string(&schemars::schema_for!(McpCatalogDetail)).unwrap();
    assert!(!detail_schema.contains("headerName"));
    assert!(!detail_schema.contains("environmentKey"));
    assert!(!detail_schema.contains("credentialName"));
    let task_schema = serde_json::to_string(&schemars::schema_for!(McpTaskRef)).unwrap();
    for forbidden in ["stderr", "stackTrace", "daemonAddress", "absolutePath"] {
        assert!(!task_schema.contains(forbidden), "found {forbidden}");
    }

    let confirm = serde_json::json!({
        "planId":"plan_1",
        "planDigest":"a".repeat(64),
        "userDecision":"confirm",
        "idempotencyKey":"confirm_1",
        "docker":{"socket":"forged"}
    });
    assert!(serde_json::from_value::<McpInstallConfirmRequest>(confirm).is_err());
}
