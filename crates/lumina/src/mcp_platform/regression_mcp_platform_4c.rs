#![cfg(all(test, feature = "rustls-tls"))]

use crate as lumina;
use lumina::agents::{ExtensionConfig, ExtensionManager, ToolCallContext};
use lumina::mcp_platform::lifecycle::{CoreTransportProjectionAdapter, TransportProjectionAdapter};
use lumina::mcp_platform::manifest::Transport;
use lumina::mcp_platform::remote_https_fixture::RemoteHttpsFixture;
use lumina::mcp_platform::remote_https_test_policy::{
    FixtureHttpsManifestSourceFetcher, FixtureRemoteHttpNetworkPolicy,
};
use lumina::mcp_platform::{
    HttpsManifestConfirmInput, HttpsManifestPlanReviewInput, HttpsManifestPrepareInput,
    InMemoryIntegritySigner, InstallConfirmInput, ManagedListInput, ManifestSourceContext,
    McpPlatformService, McpPlatformServiceOptions, RequestContext, SqliteMcpPlatformRepository,
    SystemClock, TaskGetInput, TaskStatus, UserDecision,
};
use rmcp::model::CallToolRequestParams;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Ids(AtomicU64);
impl lumina::mcp_platform::IdGenerator for Ids {
    fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}_{}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

#[tokio::test]
async fn https_install_reaches_real_fixture_mcp_tool_call() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let fixture = RemoteHttpsFixture::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path_with_integrity_signer(
            dir.path().join("platform.db"),
            InMemoryIntegritySigner::new_for_testing([0x4c; 32]),
        )
        .await
        .unwrap(),
    );
    let service = McpPlatformService::new_with_remote_http_network_policy(
        repository.clone(),
        Arc::new(SystemClock),
        Arc::new(Ids(AtomicU64::new(1))),
        McpPlatformServiceOptions::default(),
        Arc::new(FixtureRemoteHttpNetworkPolicy::new(&fixture)),
    )
    .with_https_manifest_fetcher(Arc::new(FixtureHttpsManifestSourceFetcher::new(&fixture)));
    let context = RequestContext::authenticated_transport_client(
        "https-fixture".into(),
        "same-actor-transport-session".into(),
    );
    let expected_url_id =
        lumina::utils::bytes_to_hex(Sha256::digest(fixture.manifest_url.as_bytes()));

    let prepared = service
        .https_manifest_prepare(
            &context,
            HttpsManifestPrepareInput {
                url: fixture.manifest_url.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(prepared.preview.version, "1.0.0");
    assert_eq!(prepared.preview.manifest_id, "lumina.fixture");
    assert!(!prepared.preview.parsed_digest.is_empty());
    assert!(!prepared.preview.raw_digest.is_empty());
    let confirmed = service
        .https_manifest_confirm(
            &context,
            HttpsManifestConfirmInput {
                provision_id: prepared.provision_id.clone(),
                token: prepared.server_token,
                confirm: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(confirmed.manifest_digest, prepared.preview.parsed_digest);
    assert_eq!(confirmed.preview.version, "1.0.0");
    assert_eq!(
        confirmed.preview.parsed_digest,
        prepared.preview.parsed_digest
    );
    let confirmed_provision = repository
        .get_consumed_https_manifest_provision(
            &confirmed.provision_id,
            context.actor(),
            context.transport_session_binding().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(confirmed_provision.requested_url_id, expected_url_id);
    assert_eq!(confirmed_provision.final_url_id, expected_url_id);
    assert!(!fixture.manifest_url.contains("manual"));

    let confirmed_provision_id = confirmed.provision_id.clone();
    let confirmed_manifest_digest = confirmed.manifest_digest.clone();
    let review = service
        .https_manifest_plan_review(
            &context,
            HttpsManifestPlanReviewInput {
                provision_id: confirmed_provision_id.clone(),
                expected_manifest_digest: confirmed_manifest_digest.clone(),
                idempotency_key: "https-fixture-plan".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(review.manifest.version.as_str(), "1.0.0");
    assert_eq!(review.plan.manifest_digest(), confirmed.manifest_digest);
    assert_eq!(review.plan.manifest_version(), "1.0.0");
    assert_eq!(review.manifest.id, "lumina.fixture");
    match review.plan.source_context() {
        Some(ManifestSourceContext::HttpsProvision { binding }) => {
            assert_eq!(binding.provision_id, confirmed_provision_id);
            assert_eq!(binding.requested_url_id, expected_url_id);
            assert_eq!(binding.final_url_id, expected_url_id);
            assert_eq!(binding.parsed_digest, confirmed.preview.parsed_digest);
            assert_eq!(binding.raw_digest, confirmed.preview.raw_digest);
            assert_eq!(
                binding.redirect_chain_digest,
                confirmed.preview.redirect_chain_digest
            );
            assert_eq!(
                binding.dns_evidence_digest,
                confirmed.preview.dns_evidence_digest
            );
        }
        source_context => panic!("expected HTTPS source binding, got {source_context:?}"),
    }
    match &review.manifest.transport {
        Transport::StreamableHttp { url, .. } => {
            assert_eq!(url, &fixture.mcp_url);
        }
        transport => panic!("expected streamable HTTP fixture transport, got {transport:?}"),
    }
    let task = service
        .install_confirm(
            &context,
            InstallConfirmInput {
                plan_id: review.plan_id,
                plan_digest: review.plan_digest,
                decision: UserDecision::Confirm,
                idempotency_key: "https-fixture-install".into(),
            },
        )
        .await
        .unwrap();
    let initial = service
        .task_get(
            &context,
            TaskGetInput {
                task_id: task.task_id.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(initial.task_id, task.task_id);
    assert_eq!(initial.status, TaskStatus::Queued);
    assert_eq!(initial.progress, 0);
    let mut observed_statuses = vec![initial.status];
    const MAX_RUNNER_TICKS: usize = 32;
    let mut completed = None;
    for _ in 0..MAX_RUNNER_TICKS {
        service.runner_tick().await.unwrap();
        let current = service
            .task_get(
                &context,
                TaskGetInput {
                    task_id: task.task_id.clone(),
                },
            )
            .await
            .unwrap();
        if observed_statuses.last() != Some(&current.status) {
            observed_statuses.push(current.status);
        }
        if matches!(
            current.status,
            TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled
        ) {
            completed = Some(current);
            break;
        }
    }
    let completed =
        completed.expect("fixture install task did not complete within bounded runner ticks");
    assert_eq!(completed.status, TaskStatus::Succeeded);
    assert!(observed_statuses.contains(&TaskStatus::Running));
    assert_eq!(completed.progress, 100);

    let managed = service
        .managed_list(&context, ManagedListInput::default())
        .await
        .unwrap();
    let managed = managed
        .items
        .iter()
        .find(|item| item.mcp_id == "lumina.fixture")
        .expect("successful install missing managed projection");
    assert_eq!(managed.active_version.as_deref(), Some("1.0.0"));
    assert_eq!(
        managed.installation,
        lumina::mcp_platform::InstallationState::Installed
    );
    assert_eq!(
        managed.registration,
        lumina::mcp_platform::RegistrationState::Registered
    );

    let projection = repository
        .get_connection_projection(&managed.managed_mcp_id)
        .await
        .unwrap();
    assert_eq!(
        projection.manifest_digest.as_deref(),
        Some(confirmed_manifest_digest.as_str())
    );
    assert_eq!(
        projection.projection_digest,
        lumina::mcp_platform::projection_config_digest(&projection.projection).unwrap()
    );
    assert_eq!(projection.projection_digest.len(), 64);
    assert!(projection
        .projection_digest
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(managed.active_version.as_deref(), Some("1.0.0"));
    let extension_config = CoreTransportProjectionAdapter
        .extension_config(&projection.projection, &projection.link_key)
        .unwrap();
    let ExtensionConfig::ManagedStreamableHttp { name, uri, .. } = &extension_config else {
        panic!(
            "managed HTTPS projection did not produce streamable HTTP config: {extension_config:?}"
        );
    };
    assert_eq!(name, &projection.link_key);
    assert_eq!(uri, &fixture.mcp_url);

    let policy = Arc::new(FixtureRemoteHttpNetworkPolicy::new(&fixture));
    let manager = Arc::new(
        ExtensionManager::new_without_provider_with_managed_remote_http_policy(
            dir.path().to_path_buf(),
            policy,
        ),
    );
    manager
        .add_extension(extension_config, None, None, Some("fixture-session"))
        .await
        .unwrap();
    let tools = manager
        .get_prefixed_tools("fixture-session", Some(projection.link_key.clone()))
        .await
        .unwrap();
    let prefixed_tool = format!("{}__get_code", projection.link_key);
    assert!(tools.iter().any(|tool| tool.name == prefixed_tool));
    let result = manager
        .dispatch_tool_call(
            &ToolCallContext::new("fixture-session".into(), None, Some("request".into())),
            CallToolRequestParams::new(prefixed_tool).with_arguments(serde_json::Map::new()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let result = result.result.await.unwrap();
    assert_eq!(
        result.content[0].as_text().unwrap().text,
        "MCP_FIXTURE_CODE"
    );
}
