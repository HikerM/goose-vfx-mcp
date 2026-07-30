use crate as goose;

use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use goose::mcp_platform::intake::{
    ApprovedStdioCandidateInput, CatalogPlanningCandidateInput, IntakeGateReason,
    IntakeLifecycleState, IntakeSourceFacet, IntakeTransport, LegacyQuarantineCandidateInput,
    ManualHttpsCandidateInput,
};
use goose::mcp_platform::{
    Clock, IdGenerator, ManualStdioProvider, ManualStdioSource, McpPlatformError,
    McpPlatformErrorCode, McpPlatformResult, McpPlatformService, McpPlatformServiceOptions,
    RemoteHttpNetworkPolicy, RequestContext, ResolvedManualStdioSource,
    SqliteMcpPlatformRepository, TrustTier,
};

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
struct CountingNetworkPolicy {
    validate_calls: AtomicUsize,
    plan_calls: AtomicUsize,
    client_calls: AtomicUsize,
}

#[async_trait]
impl RemoteHttpNetworkPolicy for CountingNetworkPolicy {
    fn validate_endpoint(&self, _endpoint: &str) -> McpPlatformResult<()> {
        self.validate_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn validate_for_plan(
        &self,
        _endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<()> {
        self.plan_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn secure_client(
        &self,
        _endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<goose::mcp_platform::ManagedRemoteHttpClient> {
        self.client_calls.fetch_add(1, Ordering::SeqCst);
        Err(McpPlatformError::new(
            McpPlatformErrorCode::RemoteHttpPolicyUnavailable,
            "not used in secure intake regression test",
        ))
    }
}

#[derive(Default)]
struct CountingManualStdioProvider {
    list_calls: AtomicUsize,
    resolve_calls: AtomicUsize,
}

impl ManualStdioProvider for CountingManualStdioProvider {
    fn list_sources(&self) -> McpPlatformResult<Vec<ManualStdioSource>> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    fn resolve(&self, _source_id: &str) -> McpPlatformResult<ResolvedManualStdioSource> {
        self.resolve_calls.fetch_add(1, Ordering::SeqCst);
        Err(McpPlatformError::new(
            McpPlatformErrorCode::ManualStdioProviderUnavailable,
            "not used in secure intake regression test",
        ))
    }
}

async fn service(
    policy: Arc<CountingNetworkPolicy>,
    provider: Arc<CountingManualStdioProvider>,
) -> (
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
    let service = McpPlatformService::new_with_remote_http_network_policy(
        repository.clone(),
        Arc::new(TestClock(AtomicI64::new(10))),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions::default(),
        policy,
    )
    .with_manual_stdio_provider(provider);
    (directory, repository, service)
}

async fn open_repository_pool(
    repository: &Arc<SqliteMcpPlatformRepository>,
) -> sqlx::Pool<sqlx::Sqlite> {
    let path = repository
        .database_path()
        .expect("file-backed repository required for intake regression checks");
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .unwrap()
}

fn legacy_private_reference(kind_label: &str, raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"goose.mcp-platform.intake.private-reference.v1");
    hasher.update([0]);
    hasher.update(kind_label.as_bytes());
    hasher.update([0xff]);
    hasher.update(raw.len().to_le_bytes());
    hasher.update(raw.as_bytes());
    format!(
        "oprv1_{kind_label}_{}",
        goose::utils::bytes_to_hex(hasher.finalize())
    )
}

fn legacy_intake_hash_hex(label: &str, parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"goose.mcp-platform.intake.v1");
    hasher.update([0]);
    hasher.update(label.as_bytes());
    for part in parts {
        hasher.update([0xff]);
        hasher.update((*part).len().to_le_bytes());
        hasher.update(*part);
    }
    goose::utils::bytes_to_hex(hasher.finalize())
}

fn legacy_descriptor_digest(
    descriptor: &goose::mcp_platform::intake::IntakeConfigurationDescriptor,
    private_reference: &str,
) -> String {
    let encoded = serde_json::to_vec(descriptor).unwrap();
    legacy_intake_hash_hex("descriptor", &[&encoded, private_reference.as_bytes()])
}

#[tokio::test]
async fn secure_intake_submissions_persist_without_network_or_provider_resolution() {
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let (_directory, _repository, service) = service(policy.clone(), provider.clone()).await;
    let context = test_context("i2a-secure-submit");

    let manual = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://safe.example.com/".to_string(),
                private_endpoint_ref: "opaque_endpoint_ref_1".to_string(),
            },
            "manual-secure-1",
        )
        .await
        .unwrap();
    assert_eq!(manual.source_facet, IntakeSourceFacet::ManualHttpsCandidate);
    assert_eq!(manual.transport, IntakeTransport::StreamableHttp);
    assert_eq!(
        manual.lifecycle_state,
        IntakeLifecycleState::AwaitingConsent
    );

    let stdio = service
        .intake_submit_approved_stdio_candidate(
            &context,
            ApprovedStdioCandidateInput::Candidate {
                provider_source_ref: "trusted_stdio_1".to_string(),
                private_payload_ref: "opaque_payload_ref_1".to_string(),
                declared_mcp_id: Some("fixture_stdio".to_string()),
                declared_version: Some("1.0.0".to_string()),
            },
            "stdio-secure-1",
        )
        .await
        .unwrap();
    assert_eq!(
        stdio.source_facet,
        IntakeSourceFacet::ApprovedStdioCandidate
    );
    assert_eq!(stdio.transport, IntakeTransport::Stdio);
    assert_eq!(stdio.lifecycle_state, IntakeLifecycleState::Submitted);

    assert_eq!(policy.validate_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.plan_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.client_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.list_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.resolve_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn legacy_quarantine_is_recorded_as_distinct_non_secure_flow() {
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let (_directory, _repository, service) = service(policy.clone(), provider.clone()).await;
    let context = test_context("i2a-legacy-quarantine");

    let legacy = service
        .intake_submit_legacy_quarantine(
            &context,
            LegacyQuarantineCandidateInput {
                legacy_flow: "manual_plan_create".to_string(),
            },
            "legacy-flow-1",
        )
        .await
        .unwrap();
    assert_eq!(legacy.source_facet, IntakeSourceFacet::LegacyQuarantine);
    assert_eq!(legacy.transport, IntakeTransport::Legacy);
    assert_eq!(
        legacy.lifecycle_state,
        IntakeLifecycleState::LegacyQuarantine
    );

    assert_eq!(policy.validate_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.resolve_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn candidate_state_conflict_and_manifest_identity_conflict_are_independent() {
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let (_directory, _repository, service) = service(policy, provider).await;
    let context = test_context("i2a-conflicts");

    let manual = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://safe.example.com/".to_string(),
                private_endpoint_ref: "opaque_endpoint_ref_2".to_string(),
            },
            "manual-conflict",
        )
        .await
        .unwrap();
    let state_error = service
        .intake_record_binding_ready(&manual.candidate_id, "fixture@1.0.0", None)
        .await
        .unwrap_err();
    assert_eq!(
        state_error.code(),
        McpPlatformErrorCode::CandidateStateConflict
    );
    let manual = service
        .intake_get_candidate(&manual.candidate_id)
        .await
        .unwrap();
    assert_eq!(
        manual.gate_reason,
        Some(IntakeGateReason::CandidateStateConflict)
    );

    let candidate_a = service
        .intake_submit_catalog_planning_candidate(
            &context,
            CatalogPlanningCandidateInput {
                source_id: "local_persistence".to_string(),
                mcp_id: "fixture_catalog".to_string(),
                version: "1.0.0".to_string(),
                private_manifest_ref: Some("opaque_manifest_ref_a".to_string()),
            },
            "catalog-a",
        )
        .await
        .unwrap();
    let candidate_b = service
        .intake_submit_catalog_planning_candidate(
            &context,
            CatalogPlanningCandidateInput {
                source_id: "local_persistence".to_string(),
                mcp_id: "fixture_catalog_other".to_string(),
                version: "1.0.0".to_string(),
                private_manifest_ref: Some("opaque_manifest_ref_b".to_string()),
            },
            "catalog-b",
        )
        .await
        .unwrap();

    let candidate_a = service
        .intake_grant_consent(&candidate_a.candidate_id, "consent-a")
        .await
        .unwrap();
    let candidate_b = service
        .intake_grant_consent(&candidate_b.candidate_id, "consent-b")
        .await
        .unwrap();
    let candidate_a = service
        .intake_grant_approval(&candidate_a.candidate_id, "approval-a")
        .await
        .unwrap();
    let candidate_b = service
        .intake_grant_approval(&candidate_b.candidate_id, "approval-b")
        .await
        .unwrap();
    let candidate_a = service
        .intake_record_binding_ready(&candidate_a.candidate_id, "fixture_catalog@1.0.0", None)
        .await
        .unwrap();
    assert_eq!(
        candidate_a.lifecycle_state,
        IntakeLifecycleState::BindingReady
    );

    let identity_error = service
        .intake_record_binding_ready(&candidate_b.candidate_id, "fixture_catalog@1.0.0", None)
        .await
        .unwrap_err();
    assert_eq!(
        identity_error.code(),
        McpPlatformErrorCode::ManifestIdentityConflict
    );
    let candidate_b = service
        .intake_get_candidate(&candidate_b.candidate_id)
        .await
        .unwrap();
    assert_eq!(
        candidate_b.gate_reason,
        Some(IntakeGateReason::ManifestIdentityConflict)
    );
}

#[tokio::test]
async fn typed_unavailable_intake_inputs_fail_closed_without_legacy_fallback() {
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let (_directory, _repository, service) = service(policy.clone(), provider.clone()).await;
    let context = test_context("i2a-unavailable");

    let remote_error = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::RemoteHttpPolicyUnavailable,
            "manual-unavailable",
        )
        .await
        .unwrap_err();
    assert_eq!(
        remote_error.code(),
        McpPlatformErrorCode::RemoteHttpPolicyUnavailable
    );

    let stdio_error = service
        .intake_submit_approved_stdio_candidate(
            &context,
            ApprovedStdioCandidateInput::ManualStdioProviderUnavailable,
            "stdio-unavailable",
        )
        .await
        .unwrap_err();
    assert_eq!(
        stdio_error.code(),
        McpPlatformErrorCode::ManualStdioProviderUnavailable
    );

    let empty_provider = service
        .intake_submit_approved_stdio_candidate(
            &context,
            ApprovedStdioCandidateInput::EmptyProvider,
            "stdio-empty",
        )
        .await
        .unwrap_err();
    assert_eq!(
        empty_provider.code(),
        McpPlatformErrorCode::OperationNotSupported
    );

    assert_eq!(policy.validate_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.plan_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.client_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.resolve_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn non_stdio_candidates_reject_registry_owned_payload_digest_input() {
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let (_directory, _repository, service) = service(policy, provider).await;
    let context = test_context("i2a-non-stdio-digest");

    let candidate = service
        .intake_submit_catalog_planning_candidate(
            &context,
            CatalogPlanningCandidateInput {
                source_id: "local_persistence".to_string(),
                mcp_id: "fixture_catalog_digest".to_string(),
                version: "1.0.0".to_string(),
                private_manifest_ref: Some("opaque_manifest_ref_digest".to_string()),
            },
            "catalog-digest",
        )
        .await
        .unwrap();
    let candidate = service
        .intake_grant_consent(&candidate.candidate_id, "consent-digest")
        .await
        .unwrap();
    let candidate = service
        .intake_grant_approval(&candidate.candidate_id, "approval-digest")
        .await
        .unwrap();
    let malicious_digest = "a".repeat(64);

    let error = service
        .intake_record_binding_ready(
            &candidate.candidate_id,
            "fixture_catalog_digest@1.0.0",
            Some(malicious_digest.as_str()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);

    let current = service
        .intake_get_candidate(&candidate.candidate_id)
        .await
        .unwrap();
    assert_eq!(
        current.lifecycle_state,
        IntakeLifecycleState::ApprovalGranted
    );
    assert_eq!(current.registry_owned_payload_digest, None);
}

#[tokio::test]
async fn approved_stdio_binding_fails_closed_without_trusted_payload_store() {
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let (_directory, _repository, service) = service(policy, provider).await;
    let context = test_context("i2a-stdio-binding-unavailable");

    let candidate = service
        .intake_submit_approved_stdio_candidate(
            &context,
            ApprovedStdioCandidateInput::Candidate {
                provider_source_ref: "trusted_stdio_binding".to_string(),
                private_payload_ref: "opaque_payload_ref_binding".to_string(),
                declared_mcp_id: Some("fixture_stdio_binding".to_string()),
                declared_version: Some("1.0.0".to_string()),
            },
            "stdio-binding-unavailable",
        )
        .await
        .unwrap();
    let candidate = service
        .intake_grant_consent(&candidate.candidate_id, "consent-stdio-binding")
        .await
        .unwrap();
    let candidate = service
        .intake_grant_approval(&candidate.candidate_id, "approval-stdio-binding")
        .await
        .unwrap();
    let malicious_digest = "b".repeat(64);

    let digest_error = service
        .intake_record_binding_ready(
            &candidate.candidate_id,
            "fixture_stdio_binding@1.0.0",
            Some(malicious_digest.as_str()),
        )
        .await
        .unwrap_err();
    assert_eq!(digest_error.code(), McpPlatformErrorCode::InvalidRequest);

    let unavailable = service
        .intake_record_binding_ready(&candidate.candidate_id, "fixture_stdio_binding@1.0.0", None)
        .await
        .unwrap_err();
    assert_eq!(
        unavailable.code(),
        McpPlatformErrorCode::OperationNotSupported
    );

    let current = service
        .intake_get_candidate(&candidate.candidate_id)
        .await
        .unwrap();
    assert_eq!(
        current.lifecycle_state,
        IntakeLifecycleState::ApprovalGranted
    );
    assert_eq!(current.manifest_identity_binding, None);
    assert_eq!(current.registry_owned_payload_digest, None);
}

#[tokio::test]
async fn private_reference_inputs_reject_urls_paths_tokens_and_queries() {
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let (_directory, _repository, service) = service(policy, provider).await;
    let context = test_context("i2a-private-ref-reject");

    for (index, invalid_reference) in [
        "https://safe.example.com/mcp?token=1",
        "C:\\secret\\path",
        "\\\\server\\share",
        "opaque_token_ref",
        "Bearer_ref",
        "opaque_query_ref",
    ]
    .into_iter()
    .enumerate()
    {
        let error = service
            .intake_submit_catalog_planning_candidate(
                &context,
                CatalogPlanningCandidateInput {
                    source_id: "local_persistence".to_string(),
                    mcp_id: format!("fixture_{index}"),
                    version: "1.0.0".to_string(),
                    private_manifest_ref: Some(invalid_reference.to_string()),
                },
                &format!("catalog-private-ref-{index}"),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);
    }
}

#[tokio::test]
async fn service_mints_private_references_before_persisting_and_forged_values_fail_closed() {
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let (_directory, repository, service) = service(policy, provider).await;
    let context = test_context("i2a-private-ref-persist");

    let raw_reference = "opaque_endpoint_ref_persist";
    let candidate = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://safe.example.com/".to_string(),
                private_endpoint_ref: raw_reference.to_string(),
            },
            "manual-private-ref-persist",
        )
        .await
        .unwrap();

    let pool = open_repository_pool(&repository).await;
    let stored_reference: String = sqlx::query_scalar(
        "SELECT private_reference FROM mcp_intake_configuration_refs WHERE configuration_ref = ?",
    )
    .bind(&candidate.redacted_configuration_ref)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_ne!(stored_reference, raw_reference);
    assert!(stored_reference.starts_with("oprv2_manual_https_endpoint_"));

    sqlx::query(
        "UPDATE mcp_intake_configuration_refs SET private_reference = 'forged_private_reference' WHERE configuration_ref = ?",
    )
    .bind(&candidate.redacted_configuration_ref)
    .execute(&pool)
    .await
    .unwrap();

    let error = service
        .intake_get_candidate(&candidate.candidate_id)
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
}

#[tokio::test]
async fn repeated_raw_private_reference_submissions_mint_distinct_handles_and_digest_bindings() {
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let (_directory, repository, service) = service(policy, provider).await;
    let context = test_context("i2a-private-ref-repeated");

    let raw_reference = "opaque_endpoint_ref_repeat";
    let first = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://safe.example.com/".to_string(),
                private_endpoint_ref: raw_reference.to_string(),
            },
            "manual-private-ref-repeat-1",
        )
        .await
        .unwrap();
    let second = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://safe.example.com/".to_string(),
                private_endpoint_ref: raw_reference.to_string(),
            },
            "manual-private-ref-repeat-2",
        )
        .await
        .unwrap();

    let pool = open_repository_pool(&repository).await;
    let first_row: (String, String) = sqlx::query_as(
        "SELECT private_reference, descriptor_digest FROM mcp_intake_configuration_refs WHERE configuration_ref = ?",
    )
    .bind(&first.redacted_configuration_ref)
    .fetch_one(&pool)
    .await
    .unwrap();
    let second_row: (String, String) = sqlx::query_as(
        "SELECT private_reference, descriptor_digest FROM mcp_intake_configuration_refs WHERE configuration_ref = ?",
    )
    .bind(&second.redacted_configuration_ref)
    .fetch_one(&pool)
    .await
    .unwrap();
    let raw_sha = crate::utils::bytes_to_hex(Sha256::digest(raw_reference.as_bytes()));
    let legacy_reference = legacy_private_reference("manual_https_endpoint", raw_reference);
    let legacy_descriptor = legacy_descriptor_digest(
        &goose::mcp_platform::intake::IntakeConfigurationDescriptor::ManualHttpsCandidate {
            display_origin: "https://safe.example.com".to_string(),
        },
        &legacy_reference,
    );

    assert_ne!(first_row.0, second_row.0);
    assert_ne!(first_row.1, second_row.1);
    assert_ne!(first_row.0, raw_reference);
    assert_ne!(second_row.0, raw_reference);
    assert_ne!(first_row.0, legacy_reference);
    assert_ne!(second_row.0, legacy_reference);
    assert_ne!(first_row.1, legacy_descriptor);
    assert_ne!(second_row.1, legacy_descriptor);
    assert!(first_row.0.starts_with("oprv2_manual_https_endpoint_"));
    assert!(second_row.0.starts_with("oprv2_manual_https_endpoint_"));
    assert!(!first_row.0.contains(raw_reference));
    assert!(!second_row.0.contains(raw_reference));
    assert!(!first_row.0.contains(&raw_sha));
    assert!(!second_row.0.contains(&raw_sha));
}
