use crate as lumina;

use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use lumina::mcp_platform::credential_authority::RemoteInspectionTargetAuthority;
use lumina::mcp_platform::intake::{
    ApprovedStdioCandidateInput, CatalogPlanningCandidateInput, ManualHttpsCandidateInput,
};
use lumina::mcp_platform::intake_inspection::{
    classify_failure, InspectionConflictCode, InspectionFactCode, InspectionFailureFamily,
    InspectionState, InspectionSummaryCode,
};
use lumina::mcp_platform::intake_remote_inspection::{
    RemoteInspectionNoLiveProofKind, RemoteInspectionPurpose, RemoteInspectionSafeSubcode,
    RemoteInspectionSafeSummary,
};
use lumina::mcp_platform::service::port::{
    BindRemoteInspectionPendingCommit, ClaimCommittedRemoteInspectionAttempt,
    FinalizeRemoteInspectionConsumption, RecordRemoteInspectionCommitOutcome,
    ReserveOrLoadRemoteInspection, TombstoneRemoteInspectionPrebind,
};
use lumina::mcp_platform::{
    Clock, IdGenerator, ManualStdioProvider, ManualStdioSource, McpPlatformError,
    McpPlatformErrorCode, McpPlatformResult, McpPlatformService, McpPlatformServiceOptions,
    RemoteHttpNetworkPolicy, RequestContext, ResolvedManualStdioSource,
    SqliteMcpPlatformRepository,
};

fn test_context(label: &str) -> RequestContext {
    RequestContext::local_authenticated_client(label.to_string())
}

struct TestClock(AtomicI64);

impl TestClock {
    fn set(&self, value: i64) {
        self.0.store(value, Ordering::SeqCst);
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
    ) -> McpPlatformResult<lumina::mcp_platform::ManagedRemoteHttpClient> {
        self.client_calls.fetch_add(1, Ordering::SeqCst);
        Err(McpPlatformError::new(
            McpPlatformErrorCode::RemoteHttpPolicyUnavailable,
            "batch1 inspection must not request a remote client",
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
            "batch1 inspection must not resolve stdio providers",
        ))
    }
}

async fn service(
    now_ms: i64,
) -> (
    tempfile::TempDir,
    Arc<SqliteMcpPlatformRepository>,
    Arc<TestClock>,
    Arc<CountingNetworkPolicy>,
    Arc<CountingManualStdioProvider>,
    McpPlatformService,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path(directory.path().join("platform.db"))
            .await
            .unwrap(),
    );
    let clock = Arc::new(TestClock(AtomicI64::new(now_ms)));
    let policy = Arc::new(CountingNetworkPolicy::default());
    let provider = Arc::new(CountingManualStdioProvider::default());
    let service = McpPlatformService::new_with_remote_http_network_policy(
        repository.clone(),
        clock.clone(),
        Arc::new(TestIds::default()),
        McpPlatformServiceOptions::default(),
        policy.clone(),
    )
    .with_manual_stdio_provider(provider.clone());
    (directory, repository, clock, policy, provider, service)
}

async fn open_repository_pool(
    repository: &Arc<SqliteMcpPlatformRepository>,
) -> sqlx::Pool<sqlx::Sqlite> {
    let path = repository
        .database_path()
        .expect("file-backed repository required for sqlite tamper checks");
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .unwrap()
}

async fn max_schema_version(pool: &sqlx::Pool<sqlx::Sqlite>) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(version), 0) FROM schema_version")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn schema_version_count(pool: &sqlx::Pool<sqlx::Sqlite>, version: i64) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM schema_version WHERE version = ?")
        .bind(version)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn assert_remote_tables_empty(repository: &Arc<SqliteMcpPlatformRepository>) {
    for (table, count) in repository.remote_inspection_table_counts().await.unwrap() {
        assert_eq!(count, 0, "{table} should remain empty");
    }
}

async fn assert_snapshot_read_fails_closed(
    repository: &Arc<SqliteMcpPlatformRepository>,
    snapshot_id: &str,
) {
    let error = repository
        .get_intake_inspection_snapshot(snapshot_id)
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
}

fn normalize_sql_fragment(sql: &str) -> String {
    let mut tokens = Vec::new();
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '\'' || ch == '"' {
            let quote = ch;
            let mut token = String::new();
            token.push(ch);
            while let Some(next) = chars.next() {
                token.push(next);
                if next == quote {
                    if chars.peek().copied() == Some(quote) {
                        token.push(chars.next().expect("peeked quote must exist"));
                        continue;
                    }
                    break;
                }
            }
            tokens.push(token.to_ascii_lowercase());
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            let mut token = String::new();
            token.push(ch);
            while let Some(next) = chars.peek().copied() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    token.push(chars.next().expect("peeked token char must exist"));
                } else {
                    break;
                }
            }
            tokens.push(token.to_ascii_lowercase());
            continue;
        }
        if matches!(ch, '<' | '>' | '!' | '=') && chars.peek().copied() == Some('=') {
            let mut token = String::new();
            token.push(ch);
            token.push(chars.next().expect("peeked operator suffix must exist"));
            tokens.push(token);
            continue;
        }
        tokens.push(ch.to_string());
    }
    tokens.join(" ")
}

fn find_matching_paren(sql: &str, open_index: usize) -> usize {
    let mut depth = 0_i32;
    let mut quote = None;
    let mut chars = sql[open_index..].char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        let index = open_index + offset;
        if let Some(active_quote) = quote {
            if ch == active_quote {
                if chars.peek().map(|(_, next)| *next) == Some(active_quote) {
                    chars.next();
                    continue;
                }
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return index;
                }
            }
            _ => {}
        }
    }
    panic!("unmatched parenthesis in SQL fragment");
}

fn split_top_level_sql_items(sql: &str) -> Vec<String> {
    let open_index = sql.find('(').expect("create table must contain columns");
    let close_index = find_matching_paren(sql, open_index);
    let body = &sql[open_index + 1..close_index];
    let mut items = Vec::new();
    let mut current = String::new();
    let mut depth = 0_i32;
    let mut quote = None;
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            current.push(ch);
            if ch == active_quote {
                if chars.peek().copied() == Some(active_quote) {
                    current.push(chars.next().expect("peeked escaped quote must exist"));
                    continue;
                }
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                current.push(ch);
            }
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                let item = current.trim();
                if !item.is_empty() {
                    items.push(item.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let item = current.trim();
    if !item.is_empty() {
        items.push(item.to_string());
    }
    items
}

fn extract_create_table_sql<'a>(version_block: &'a str, table_name: &str) -> &'a str {
    let marker = format!("CREATE TABLE {table_name} (");
    let start = version_block
        .find(&marker)
        .unwrap_or_else(|| panic!("missing table {table_name}"));
    let sql = &version_block[start..];
    let close_index = find_matching_paren(
        sql,
        sql.find('(')
            .expect("create table marker must include opening paren"),
    );
    &sql[..=close_index]
}

fn assert_create_table_has_top_level_item(
    version_block: &str,
    table_name: &str,
    expected_item_sql: &str,
) {
    let normalized_expected = normalize_sql_fragment(expected_item_sql);
    let normalized_items: Vec<String> =
        split_top_level_sql_items(extract_create_table_sql(version_block, table_name))
            .into_iter()
            .map(|item| normalize_sql_fragment(&item))
            .collect();
    assert!(
        normalized_items
            .iter()
            .any(|item| item == &normalized_expected),
        "table {table_name} missing top-level item {expected_item_sql}; got {normalized_items:?}",
    );
}

#[tokio::test]
async fn manual_https_requires_valid_single_use_consent_and_records_nonreusable_snapshot() {
    let (_directory, repository, _clock, policy, provider, service) = service(10).await;
    let context = test_context("i2b-manual-consent");
    let candidate = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://safe.example.com/".to_string(),
                private_endpoint_ref: "opaque_endpoint_1".to_string(),
            },
            "manual-i2b-1",
        )
        .await
        .unwrap();

    let missing = service
        .intake_request_batch1_inspection_snapshot(
            &candidate.candidate_id,
            "inspection_consent_404",
        )
        .await
        .unwrap_err();
    assert_eq!(missing.code(), McpPlatformErrorCode::NotFound);

    let consent = service
        .intake_grant_batch1_inspection_consent(&candidate.candidate_id, "seed_manual_1", 100)
        .await
        .unwrap();
    let snapshot = service
        .intake_request_batch1_inspection_snapshot(&candidate.candidate_id, &consent.consent_id)
        .await
        .unwrap();
    assert_eq!(
        snapshot.inspection_state,
        InspectionState::InspectionBlocked
    );
    assert_eq!(
        snapshot.summary_code,
        InspectionSummaryCode::NetworkExecutionUnavailable
    );
    assert_eq!(snapshot.reusable, false);

    let stored = repository
        .get_intake_inspection_snapshot(&snapshot.snapshot_id)
        .await
        .unwrap();
    assert_eq!(stored.reusable, false);

    let consumed = service
        .intake_request_batch1_inspection_snapshot(&candidate.candidate_id, &consent.consent_id)
        .await
        .unwrap_err();
    assert_eq!(
        consumed.code(),
        McpPlatformErrorCode::CandidateStateConflict
    );

    assert_eq!(policy.validate_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.plan_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.client_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.list_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.resolve_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn inspection_expiry_and_candidate_revision_drift_fail_closed() {
    let (_directory, _repository, clock, _policy, _provider, service) = service(10).await;
    let context = test_context("i2b-expiry-drift");
    let expired_candidate = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://safe.example.com/".to_string(),
                private_endpoint_ref: "opaque_endpoint_2".to_string(),
            },
            "manual-i2b-2",
        )
        .await
        .unwrap();
    let expired = service
        .intake_grant_batch1_inspection_consent(
            &expired_candidate.candidate_id,
            "seed_manual_2",
            11,
        )
        .await
        .unwrap();
    clock.set(12);
    let expiry_error = service
        .intake_request_batch1_inspection_snapshot(
            &expired_candidate.candidate_id,
            &expired.consent_id,
        )
        .await
        .unwrap_err();
    assert_eq!(
        expiry_error.code(),
        McpPlatformErrorCode::CandidateStateConflict
    );

    clock.set(20);
    let drift_candidate = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://drift.example.com/".to_string(),
                private_endpoint_ref: "opaque_endpoint_3".to_string(),
            },
            "manual-i2b-3",
        )
        .await
        .unwrap();
    let drift = service
        .intake_grant_batch1_inspection_consent(&drift_candidate.candidate_id, "seed_manual_3", 200)
        .await
        .unwrap();
    service
        .intake_grant_consent(&drift_candidate.candidate_id, "i2a_drift")
        .await
        .unwrap();
    let drift_error = service
        .intake_request_batch1_inspection_snapshot(&drift_candidate.candidate_id, &drift.consent_id)
        .await
        .unwrap_err();
    assert_eq!(
        drift_error.code(),
        McpPlatformErrorCode::CandidateStateConflict
    );
}

#[tokio::test]
async fn catalog_and_manual_https_batch1_paths_stay_zero_network_and_only_record_blocked_snapshots()
{
    let (_directory, _repository, _clock, policy, provider, service) = service(10).await;
    let context = test_context("i2b-remote-blocked");
    let catalog = service
        .intake_submit_catalog_planning_candidate(
            &context,
            CatalogPlanningCandidateInput {
                source_id: "local_persistence".to_string(),
                mcp_id: "fixture_catalog_i2b".to_string(),
                version: "1.0.0".to_string(),
                private_manifest_ref: Some("opaque_manifest_i2b".to_string()),
            },
            "catalog-i2b-1",
        )
        .await
        .unwrap();
    let manual = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://blocked.example.com/".to_string(),
                private_endpoint_ref: "opaque_endpoint_4".to_string(),
            },
            "manual-i2b-4",
        )
        .await
        .unwrap();

    let catalog_consent = service
        .intake_grant_batch1_inspection_consent(&catalog.candidate_id, "seed_catalog_1", 100)
        .await
        .unwrap();
    let manual_consent = service
        .intake_grant_batch1_inspection_consent(&manual.candidate_id, "seed_manual_4", 100)
        .await
        .unwrap();

    let catalog_snapshot = service
        .intake_request_batch1_inspection_snapshot(
            &catalog.candidate_id,
            &catalog_consent.consent_id,
        )
        .await
        .unwrap();
    let manual_snapshot = service
        .intake_request_batch1_inspection_snapshot(&manual.candidate_id, &manual_consent.consent_id)
        .await
        .unwrap();

    for snapshot in [catalog_snapshot, manual_snapshot] {
        assert_eq!(
            snapshot.inspection_state,
            InspectionState::InspectionBlocked
        );
        assert_eq!(
            snapshot.summary_code,
            InspectionSummaryCode::NetworkExecutionUnavailable
        );
        assert_eq!(
            snapshot.conflict_codes,
            vec![InspectionConflictCode::NetworkExecutionUnavailable]
        );
        assert_eq!(snapshot.reusable, false);
    }

    assert_eq!(policy.validate_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.plan_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.client_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.list_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.resolve_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn approved_stdio_batch1_snapshot_uses_only_local_metadata() {
    let (_directory, _repository, _clock, policy, provider, service) = service(10).await;
    let context = test_context("i2b-stdio-local");
    let candidate = service
        .intake_submit_approved_stdio_candidate(
            &context,
            ApprovedStdioCandidateInput::Candidate {
                provider_source_ref: "trusted_stdio_i2b".to_string(),
                private_payload_ref: "opaque_payload_i2b".to_string(),
                declared_mcp_id: Some("fixture_stdio_i2b".to_string()),
                declared_version: Some("1.0.0".to_string()),
            },
            "stdio-i2b-1",
        )
        .await
        .unwrap();
    let consent = service
        .intake_grant_batch1_inspection_consent(&candidate.candidate_id, "seed_stdio_1", 100)
        .await
        .unwrap();
    let snapshot = service
        .intake_request_batch1_inspection_snapshot(&candidate.candidate_id, &consent.consent_id)
        .await
        .unwrap();

    assert_eq!(
        snapshot.inspection_state,
        InspectionState::InspectionRecorded
    );
    assert_eq!(
        snapshot.summary_code,
        InspectionSummaryCode::LocalMetadataOnly
    );
    assert_eq!(
        snapshot.conflict_codes,
        Vec::<InspectionConflictCode>::new()
    );
    assert!(snapshot
        .fact_codes
        .contains(&InspectionFactCode::PayloadUnavailable));
    assert!(snapshot
        .fact_codes
        .contains(&InspectionFactCode::BindingUnavailable));
    assert!(snapshot
        .fact_codes
        .contains(&InspectionFactCode::ProviderMetadataUnavailable));
    assert!(snapshot
        .fact_codes
        .contains(&InspectionFactCode::ManifestUnavailable));
    assert_eq!(snapshot.reusable, false);

    assert_eq!(policy.validate_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.plan_calls.load(Ordering::SeqCst), 0);
    assert_eq!(policy.client_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.list_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.resolve_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cross_candidate_and_policy_reuse_are_rejected_fail_closed() {
    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let context = test_context("i2b-cross-reuse");
    let manual = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://cross.example.com/".to_string(),
                private_endpoint_ref: "opaque_endpoint_5".to_string(),
            },
            "manual-i2b-5",
        )
        .await
        .unwrap();
    let stdio = service
        .intake_submit_approved_stdio_candidate(
            &context,
            ApprovedStdioCandidateInput::Candidate {
                provider_source_ref: "trusted_stdio_i2b_2".to_string(),
                private_payload_ref: "opaque_payload_i2b_2".to_string(),
                declared_mcp_id: Some("fixture_stdio_i2b_2".to_string()),
                declared_version: Some("1.0.0".to_string()),
            },
            "stdio-i2b-2",
        )
        .await
        .unwrap();
    let consent = service
        .intake_grant_batch1_inspection_consent(&manual.candidate_id, "seed_cross_1", 100)
        .await
        .unwrap();

    let wrong_candidate = service
        .intake_request_batch1_inspection_snapshot(&stdio.candidate_id, &consent.consent_id)
        .await
        .unwrap_err();
    assert_eq!(
        wrong_candidate.code(),
        McpPlatformErrorCode::CandidateStateConflict
    );

    let pool = open_repository_pool(&repository).await;
    sqlx::query(
        "UPDATE mcp_intake_inspection_consents SET policy_revision = 2 WHERE consent_id = ?",
    )
    .bind(&consent.consent_id)
    .execute(&pool)
    .await
    .unwrap();
    let policy_drift = service
        .intake_request_batch1_inspection_snapshot(&manual.candidate_id, &consent.consent_id)
        .await
        .unwrap_err();
    assert_eq!(policy_drift.code(), McpPlatformErrorCode::IntegrityError);
}

#[tokio::test]
async fn failure_priority_and_unknown_snapshot_enums_fail_closed() {
    let failure = classify_failure(&[
        InspectionConflictCode::NetworkExecutionUnavailable,
        InspectionConflictCode::CandidateIdentityConflict,
        InspectionConflictCode::PolicyRevisionDrift,
    ]);
    assert_eq!(failure.state, InspectionState::InspectionBlocked);
    assert_eq!(
        failure.failure_family,
        Some(InspectionFailureFamily::PolicyDrift)
    );
    assert_eq!(failure.summary_code, InspectionSummaryCode::PolicyDrift);

    let classification = classify_failure(&[InspectionConflictCode::ClassificationUnavailable]);
    assert_eq!(classification.failure_family, None);
    assert_eq!(
        classification.summary_code,
        InspectionSummaryCode::ClassificationUnavailable
    );

    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let context = test_context("i2b-unknown-enum");
    let candidate = service
        .intake_submit_approved_stdio_candidate(
            &context,
            ApprovedStdioCandidateInput::Candidate {
                provider_source_ref: "trusted_stdio_i2b_3".to_string(),
                private_payload_ref: "opaque_payload_i2b_3".to_string(),
                declared_mcp_id: Some("fixture_stdio_i2b_3".to_string()),
                declared_version: Some("1.0.0".to_string()),
            },
            "stdio-i2b-3",
        )
        .await
        .unwrap();
    let consent = service
        .intake_grant_batch1_inspection_consent(&candidate.candidate_id, "seed_stdio_3", 100)
        .await
        .unwrap();
    let snapshot = service
        .intake_request_batch1_inspection_snapshot(&candidate.candidate_id, &consent.consent_id)
        .await
        .unwrap();

    let pool = open_repository_pool(&repository).await;
    sqlx::query("UPDATE mcp_intake_inspection_snapshots SET inspection_state = 'inspection_pending' WHERE snapshot_id = ?")
        .bind(&snapshot.snapshot_id)
        .execute(&pool)
        .await
        .unwrap();
    let error = repository
        .get_intake_inspection_snapshot(&snapshot.snapshot_id)
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
}

#[tokio::test]
async fn tampered_snapshot_priority_mismatch_fails_closed() {
    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let context = test_context("i2b-tamper-priority");
    let candidate = service
        .intake_submit_manual_https_candidate(
            &context,
            ManualHttpsCandidateInput::Candidate {
                display_origin: "https://priority.example.com/".to_string(),
                private_endpoint_ref: "opaque_endpoint_priority".to_string(),
            },
            "manual-i2b-priority",
        )
        .await
        .unwrap();
    let consent = service
        .intake_grant_batch1_inspection_consent(&candidate.candidate_id, "seed_priority", 100)
        .await
        .unwrap();
    let snapshot = service
        .intake_request_batch1_inspection_snapshot(&candidate.candidate_id, &consent.consent_id)
        .await
        .unwrap();

    let pool = open_repository_pool(&repository).await;
    sqlx::query(
        "UPDATE mcp_intake_inspection_snapshots \
         SET conflict_codes_json = ?, failure_family = ?, summary_code = ? \
         WHERE snapshot_id = ?",
    )
    .bind("[\"policy_revision_drift\",\"candidate_identity_conflict\"]")
    .bind("identity_conflict")
    .bind("identity_conflict")
    .bind(&snapshot.snapshot_id)
    .execute(&pool)
    .await
    .unwrap();

    assert_snapshot_read_fails_closed(&repository, &snapshot.snapshot_id).await;
}

#[tokio::test]
async fn tampered_stdio_snapshot_missing_required_fact_fails_closed() {
    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let context = test_context("i2b-tamper-facts");
    let candidate = service
        .intake_submit_approved_stdio_candidate(
            &context,
            ApprovedStdioCandidateInput::Candidate {
                provider_source_ref: "trusted_stdio_i2b_tamper".to_string(),
                private_payload_ref: "opaque_payload_i2b_tamper".to_string(),
                declared_mcp_id: Some("fixture_stdio_i2b_tamper".to_string()),
                declared_version: Some("1.0.0".to_string()),
            },
            "stdio-i2b-tamper-facts",
        )
        .await
        .unwrap();
    let consent = service
        .intake_grant_batch1_inspection_consent(&candidate.candidate_id, "seed_stdio_facts", 100)
        .await
        .unwrap();
    let snapshot = service
        .intake_request_batch1_inspection_snapshot(&candidate.candidate_id, &consent.consent_id)
        .await
        .unwrap();

    let pool = open_repository_pool(&repository).await;
    sqlx::query(
        "UPDATE mcp_intake_inspection_snapshots SET fact_codes_json = ? WHERE snapshot_id = ?",
    )
    .bind(
        "[\"approved_stdio_local_metadata_observed\",\
          \"provider_metadata_unavailable\",\
          \"payload_unavailable\",\
          \"binding_unavailable\"]",
    )
    .bind(&snapshot.snapshot_id)
    .execute(&pool)
    .await
    .unwrap();

    assert_snapshot_read_fails_closed(&repository, &snapshot.snapshot_id).await;
}

#[tokio::test]
async fn tampered_snapshot_surface_phase_purpose_mismatch_fails_closed() {
    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let context = test_context("i2b-tamper-surface-phase");
    let candidate = service
        .intake_submit_approved_stdio_candidate(
            &context,
            ApprovedStdioCandidateInput::Candidate {
                provider_source_ref: "trusted_stdio_i2b_surface".to_string(),
                private_payload_ref: "opaque_payload_i2b_surface".to_string(),
                declared_mcp_id: Some("fixture_stdio_i2b_surface".to_string()),
                declared_version: Some("1.0.0".to_string()),
            },
            "stdio-i2b-surface-phase",
        )
        .await
        .unwrap();
    let consent = service
        .intake_grant_batch1_inspection_consent(
            &candidate.candidate_id,
            "seed_stdio_surface_phase",
            100,
        )
        .await
        .unwrap();
    let snapshot = service
        .intake_request_batch1_inspection_snapshot(&candidate.candidate_id, &consent.consent_id)
        .await
        .unwrap();

    let pool = open_repository_pool(&repository).await;
    sqlx::query(
        "UPDATE mcp_intake_inspection_snapshots \
         SET observation_surface = ?, operation_phase = ? \
         WHERE snapshot_id = ?",
    )
    .bind("candidate_metadata")
    .bind("batch1_zero_egress")
    .bind(&snapshot.snapshot_id)
    .execute(&pool)
    .await
    .unwrap();

    assert_snapshot_read_fails_closed(&repository, &snapshot.snapshot_id).await;
}

#[test]
fn migration_is_append_only_and_has_no_raw_diagnostic_columns() {
    let migrations = include_str!("repository/sqlite/migrations.rs");
    assert!(migrations.contains("pub const CURRENT_SCHEMA_VERSION: i64 = 26;"));
    assert!(migrations.contains("const V22_STATEMENTS"));
    assert!(migrations.contains("const V23_STATEMENTS"));
    assert!(migrations.contains("const V24_STATEMENTS"));
    assert!(migrations.contains("const V25_STATEMENTS"));
    assert!(migrations.contains("const V26_STATEMENTS"));
    let v24_start = migrations.find("const V24_STATEMENTS").unwrap();
    let v25_start = migrations.find("const V25_STATEMENTS").unwrap();
    let v26_start = migrations.find("const V26_STATEMENTS").unwrap();
    let v9_start = migrations.find("const V9_STATEMENTS").unwrap();
    let v24 = &migrations[v24_start..v25_start];
    let v25 = &migrations[v25_start..v26_start];
    let v26 = &migrations[v26_start..v9_start];
    assert!(!v24.contains("ALTER TABLE mcp_intake_"));
    assert!(!v24.contains("remote_inspection"));
    assert!(!v24.contains("raw_diagnostic"));
    assert!(!v24.contains("diagnostic_json"));
    for forbidden in [
        " url ",
        " url,",
        " url)",
        " raw_target",
        " raw_url",
        " raw_authority",
        " raw_host",
        " raw_ip",
        " raw_path",
        " raw_query",
        " headers ",
        " body ",
        " cookie ",
        " payload ",
        " diagnostic_json",
        " raw_diagnostic",
    ] {
        assert!(!v25.contains(forbidden), "v25 leaked {forbidden}");
    }
    assert!(v25.contains("claim_expires_at_ms INTEGER NOT NULL"));
    assert!(v25.contains("CHECK(claim_expires_at_ms > claimed_at_ms)"));
    assert!(v25.contains("owner_started_at_ms IS NULL OR claimed_at_ms <= owner_started_at_ms"));
    assert!(
        v25.contains("owner_finished_at_ms IS NULL OR owner_started_at_ms <= owner_finished_at_ms")
    );
    assert!(v25.contains("claim_expires_at_ms <= finalized_at_ms"));
    assert_create_table_has_top_level_item(
        v25,
        "mcp_intake_remote_inspection_consents",
        "CHECK(expires_at_ms > created_at_ms)",
    );
    assert_create_table_has_top_level_item(
        v25,
        "mcp_intake_remote_inspection_attempts",
        "CHECK(owner_started_at_ms IS NULL OR owner_started_at_ms < claim_expires_at_ms)",
    );
    assert!(v26.contains("mcp_intake_remote_inspection_scope_lineage_anchors_v26"));
    assert!(v26.contains("mcp_intake_remote_inspection_bindings_b26"));
    assert!(v26.contains("mcp_intake_remote_inspection_commit_events_v26"));
    assert!(v26.contains("mcp_intake_remote_inspection_consents_v26_manual_only"));
    assert!(v26.contains("mcp_intake_remote_inspection_snapshots_v26_manual_only"));
    assert!(v26.contains("mcp_intake_remote_inspection_reservations_validate_insert_v26"));
    assert!(v26.contains("mcp_intake_remote_inspection_reservations_validate_update_v26"));
}

#[test]
fn batch1_static_zero_network_gate_and_no_public_wire_hold() {
    let inspection = include_str!("intake_inspection.rs").to_ascii_lowercase();
    let seam = include_str!("intake_inspection_network.rs").to_ascii_lowercase();
    for forbidden in [
        "reqwest", "hyper", "socket", "dns", "tls", "client", "resolver",
    ] {
        assert!(
            !inspection.contains(forbidden),
            "inspection model leaked {forbidden}"
        );
        assert!(
            !seam.contains(forbidden),
            "inspection seam leaked {forbidden}"
        );
    }

    let application = include_str!("service/application.rs");
    assert!(application.contains("pub(crate) async fn intake_grant_batch1_inspection_consent"));
    assert!(application.contains("pub(crate) async fn intake_request_batch1_inspection_snapshot"));
    assert!(!application.contains("pub async fn intake_grant_batch1_inspection_consent"));
    assert!(!application.contains("pub async fn intake_request_batch1_inspection_snapshot"));
    assert!(!application.contains("InspectionNetworkOwner"));
    assert!(!application.contains("NoNetworkInspectionOwner"));

    let acp = include_str!("../acp/server/mcp_platform.rs");
    assert!(!acp.contains("intake_grant_batch1_inspection_consent"));
    assert!(!acp.contains("intake_request_batch1_inspection_snapshot"));

    let ui_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/desktop/src/acp/mcp-platform.ts");
    let ui = std::fs::read_to_string(ui_path).unwrap();
    assert!(!ui.contains("intakeGrantBatch1InspectionConsent"));
    assert!(!ui.contains("intakeRequestBatch1InspectionSnapshot"));
}

#[test]
fn batch2_2_static_private_boundary_holds() {
    let module = include_str!("intake_remote_inspection.rs");
    for forbidden in [
        "RemoteHttpNetworkPolicy",
        "ManagedRemoteResolver",
        "ManagedRemoteHttpClient",
    ] {
        assert!(
            !module.contains(forbidden),
            "private module leaked {forbidden}"
        );
    }

    let root = include_str!("mod.rs");
    assert!(root.contains("pub(crate) mod intake_remote_inspection;"));
    assert!(!root.contains("pub mod intake_remote_inspection;"));

    let service_mod = include_str!("service/mod.rs");
    let service_port = include_str!("service/port.rs");
    let application = include_str!("service/application.rs");
    let authority = include_str!("credential_authority.rs");
    let intake = include_str!("intake.rs");
    let remote_inspection = include_str!("intake_remote_inspection.rs");
    let repository_source = include_str!("repository/sqlite/intake_repository.rs");
    let regression_source = include_str!("regression_mcp_platform_i2b.rs");
    assert!(!service_mod.contains("pub use port::RemoteInspectionRepositoryPort"));
    assert!(service_port.contains("trait RemoteInspectionRepositoryPort"));
    assert!(application.contains("remote_inspection_repository"));
    for (source_name, source) in [
        ("service/port.rs", service_port),
        ("service/application.rs", application),
        ("credential_authority.rs", authority),
        ("repository/sqlite/intake_repository.rs", repository_source),
    ] {
        for forbidden in [
            "RemoteInspectionStorageVerifier",
            "storage_verifier",
            "default_remote_inspection_storage_verifier",
            "restore_remote_inspection_",
        ] {
            assert!(
                !source.contains(forbidden),
                "{source_name} retained forbidden seam {forbidden}"
            );
        }
    }
    assert!(intake.contains("struct AuthorityCommitOutcome"));
    assert!(!intake.contains("enum AuthorityCommitOutcome"));
    assert!(intake.contains("enum AuthorityCommitOutcomeKind"));
    assert!(intake.contains("kind: AuthorityCommitOutcomeKind"));
    assert!(!intake.contains("pub(crate) kind: AuthorityCommitOutcomeKind"));
    assert!(intake.contains("fn committed() -> Self"));
    assert!(intake.contains("fn reconcile_required() -> Self"));
    assert!(intake.contains("fn blocked_no_live(proof: VerifiedNoLiveProof) -> Self"));
    assert!(intake.contains("fn is_committed(&self) -> bool"));
    assert!(intake.contains("fn is_reconcile_required(&self) -> bool"));
    assert!(intake.contains("fn blocked_proof(&self) -> Option<&VerifiedNoLiveProof>"));
    for (source_name, source) in [
        ("service/application.rs", application),
        ("intake_remote_inspection.rs", remote_inspection),
        ("repository/sqlite/intake_repository.rs", repository_source),
        ("regression_mcp_platform_i2b.rs", regression_source),
    ] {
        for forbidden in [
            "AuthorityCommitOutcome::Committed",
            "AuthorityCommitOutcome::ReconcileRequired",
            "AuthorityCommitOutcome::BlockedNoLive",
        ] {
            assert!(
                !source.contains(forbidden),
                "{source_name} retained direct outcome variant construction or matching {forbidden}"
            );
        }
    }
    assert!(authority.contains("fn committed(&self) -> AuthorityCommitOutcome"));
    assert!(authority.contains("fn reconcile_required(&self) -> AuthorityCommitOutcome"));
    assert!(authority.contains(
        "fn blocked_no_live(&self, proof: VerifiedNoLiveProof) -> AuthorityCommitOutcome"
    ));
    for gate in [
        "gate_validated_remote_inspection_reserve_v26",
        "gate_validated_remote_inspection_tombstone_v26",
        "gate_validated_remote_inspection_bind_v26",
        "gate_validated_remote_inspection_commit_outcome_v26",
        "gate_validated_remote_inspection_claim_v26",
        "gate_validated_remote_inspection_owner_started_v26",
        "gate_validated_remote_inspection_owner_finished_v26",
        "gate_validated_remote_inspection_finalize_v26",
    ] {
        assert!(
            application.contains(gate),
            "application missing gate {gate}"
        );
    }
    assert!(
        repository_source.contains("tamper_remote_attempt_owner_finished_before_started"),
        "repository missing test-only tamper fixture"
    );
    for forbidden in [
        format!(
            "{}{}",
            "SELECT COUNT(*) FROM mcp_intake_remote_", "inspection_"
        ),
        format!(
            "{}{}",
            "SELECT purpose FROM mcp_intake_remote_", "inspection_"
        ),
        format!("{}{}", "UPDATE mcp_intake_remote_", "inspection_"),
        format!("{}{}", "INSERT INTO mcp_intake_remote_", "inspection_"),
        format!("{}{}", "DELETE FROM mcp_intake_remote_", "inspection_"),
    ] {
        assert!(
            !regression_source.contains(&forbidden),
            "regression retained direct remote table sql {forbidden}"
        );
    }
    for forbidden in [
        "save_remote_inspection_consent(",
        "claim_remote_inspection_attempt(",
        "record_remote_inspection_attempt_owner_started(",
        "record_remote_inspection_attempt_owner_finished(",
        "finalize_remote_inspection_attempt(",
    ] {
        assert!(
            !application.contains(forbidden),
            "application retained old remote wrapper path {forbidden}"
        );
    }

    let acp = include_str!("../acp/server/mcp_platform.rs");
    assert!(!acp.contains("remoteInspection"));
    assert!(!acp.contains("remote_inspection"));

    let ui_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/desktop/src/acp/mcp-platform.ts");
    let ui = std::fs::read_to_string(ui_path).unwrap();
    assert!(!ui.contains("remoteInspection"));
    assert!(!ui.contains("remote_inspection"));
}

#[tokio::test]
async fn batch2_2_new_repository_materializes_v26_schema() {
    let (_directory, repository, _clock, _policy, _provider, _service) = service(10).await;
    let pool = open_repository_pool(&repository).await;
    assert_eq!(max_schema_version(&pool).await, 26);
    let has_update_trigger = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name = ?",
    )
    .bind("mcp_intake_remote_inspection_reservations_validate_update_v26")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(has_update_trigger, 1);
}

#[tokio::test]
async fn batch2_2_v26_reopen_is_idempotent() {
    let (_directory, repository, _clock, _policy, _provider, _service) = service(10).await;
    let path = repository.database_path().unwrap().to_path_buf();
    repository.close().await;

    let reopened = Arc::new(SqliteMcpPlatformRepository::open_path(&path).await.unwrap());
    let pool = open_repository_pool(&reopened).await;
    assert_eq!(max_schema_version(&pool).await, 26);
    assert_eq!(schema_version_count(&pool, 26).await, 1);
    reopened.close().await;
}

#[tokio::test]
async fn batch2_2_complete_v25_reopens_and_upgrades_to_v26() {
    let (_directory, repository, _clock, _policy, _provider, _service) = service(10).await;
    let path = repository.database_path().unwrap().to_path_buf();
    repository
        .rewrite_remote_inspection_schema_to_v25_for_migration_test()
        .await
        .unwrap();
    repository.close().await;

    let reopened = Arc::new(SqliteMcpPlatformRepository::open_path(&path).await.unwrap());
    let pool = open_repository_pool(&reopened).await;
    assert_eq!(max_schema_version(&pool).await, 26);
    assert_eq!(schema_version_count(&pool, 26).await, 1);
    reopened.close().await;
}

#[tokio::test]
async fn batch2_2_invalid_v26_shape_fails_closed_on_reopen() {
    let (_directory, repository, _clock, _policy, _provider, _service) = service(10).await;
    let path = repository.database_path().unwrap().to_path_buf();
    repository
        .tamper_drop_remote_inspection_v26_update_trigger()
        .await
        .unwrap();
    repository.close().await;

    let error = SqliteMcpPlatformRepository::open_path(&path)
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
}

#[tokio::test]
async fn batch2_2_catalog_remote_gate_rejects_before_any_remote_write() {
    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let candidate = service
        .intake_submit_catalog_planning_candidate(
            &test_context("catalog_remote_gate"),
            CatalogPlanningCandidateInput {
                source_id: "catalog_source".to_string(),
                mcp_id: "catalog_mcp".to_string(),
                version: "1.0.0".to_string(),
                private_manifest_ref: None,
            },
            "catalog_remote_gate",
        )
        .await
        .unwrap();
    let error = service
        .gate_validated_remote_inspection_reserve_v26(ReserveOrLoadRemoteInspection {
            reservation_id: "catalog_remote_reservation",
            candidate_id: &candidate.candidate_id,
            candidate_revision: candidate.revision,
            candidate_lifecycle_state: candidate.lifecycle_state,
            source_facet: candidate.source_facet,
            transport: candidate.transport,
            purpose: RemoteInspectionPurpose::PrivateRemoteCheckV26,
            policy_revision: 1,
            intent_fingerprint: "catalog_intent",
            intent_fingerprint_key_id: "catalog_intent_key",
            now_ms: 11,
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::OperationNotSupported);
    assert_eq!(error.message(), "remote_inspection_transport_unsupported");
    assert_remote_tables_empty(&repository).await;
}

#[tokio::test]
async fn batch2_2_catalog_batch1_paths_keep_zero_remote_writes_and_backup_fails_closed() {
    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let candidate = service
        .intake_submit_catalog_planning_candidate(
            &test_context("catalog_batch1_zero_remote"),
            CatalogPlanningCandidateInput {
                source_id: "catalog_batch1_source".to_string(),
                mcp_id: "catalog_batch1_mcp".to_string(),
                version: "1.0.0".to_string(),
                private_manifest_ref: None,
            },
            "catalog_batch1_zero_remote",
        )
        .await
        .unwrap();
    let consent = service
        .intake_grant_batch1_inspection_consent(&candidate.candidate_id, "catalog_seed", 100)
        .await
        .unwrap();
    let _snapshot = service
        .intake_request_batch1_inspection_snapshot(&candidate.candidate_id, &consent.consent_id)
        .await
        .unwrap();
    assert_remote_tables_empty(&repository).await;

    let error = repository
        .tamper_try_insert_catalog_remote_consent(
            "catalog_remote_consent",
            &candidate.candidate_id,
            candidate.revision,
            10,
            20,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
    assert_remote_tables_empty(&repository).await;
}

#[tokio::test]
async fn batch2_2_remote_generation_and_tuple_tamper_fail_closed() {
    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let private_reference = repository
        .mint_intake_private_reference(
            lumina::mcp_platform::intake::IntakePrivateReferenceKind::ManualHttpsEndpoint,
        )
        .unwrap();
    repository
        .save_intake_configuration_ref(lumina::mcp_platform::intake::SaveIntakeConfigurationRef {
            configuration_ref: "cfg_b22_remote_tamper",
            descriptor:
                &lumina::mcp_platform::intake::IntakeConfigurationDescriptor::ManualHttpsCandidate {
                    display_origin: "https://tamper.example.com".to_string(),
                },
            private_reference: Some(private_reference.as_str()),
            descriptor_digest: &"b".repeat(64),
            now_ms: 10,
        })
        .await
        .unwrap();
    let candidate = repository
        .save_intake_candidate(lumina::mcp_platform::intake::SaveIntakeCandidate {
            candidate_id: "candidate_b22_remote_tamper",
            submission_binding: &"3".repeat(64),
            source_facet: lumina::mcp_platform::intake::IntakeSourceFacet::ManualHttpsCandidate,
            transport: lumina::mcp_platform::intake::IntakeTransport::StreamableHttp,
            initial_state: lumina::mcp_platform::intake::IntakeLifecycleState::AwaitingConsent,
            redacted_configuration_ref: "cfg_b22_remote_tamper",
            descriptor_digest: &"b".repeat(64),
            now_ms: 11,
        })
        .await
        .unwrap();
    let purpose =
        RemoteInspectionPurpose::derive(candidate.source_facet, candidate.transport).unwrap();
    let policy_revision =
        lumina::mcp_platform::intake_remote_inspection::trusted_remote_inspection_policy_revision(
            purpose,
        )
        .unwrap();
    let authority = RemoteInspectionTargetAuthority::in_memory_for_testing();
    let reservation = service
        .gate_validated_remote_inspection_reserve_v26(ReserveOrLoadRemoteInspection {
            reservation_id: "remote_reservation_tamper",
            candidate_id: &candidate.candidate_id,
            candidate_revision: candidate.revision,
            candidate_lifecycle_state: candidate.lifecycle_state,
            source_facet: candidate.source_facet,
            transport: candidate.transport,
            purpose,
            policy_revision,
            intent_fingerprint: "intent_tamper",
            intent_fingerprint_key_id: "intent_tamper_key",
            now_ms: 12,
        })
        .await
        .unwrap();

    let gap = repository
        .tamper_try_insert_generation_gap_reservation(&reservation.reservation_id, 13)
        .await
        .unwrap_err();
    assert_eq!(gap.code(), McpPlatformErrorCode::IntegrityError);
    let wrong_prebind = repository
        .tamper_try_prebind_tombstone_wrong_proof_kind(
            &reservation.reservation_id,
            reservation.generation,
            14,
        )
        .await
        .unwrap_err();
    assert_eq!(wrong_prebind.code(), McpPlatformErrorCode::IntegrityError);

    let tuple = authority.mint_verified_opaque_tuple();
    let proof = authority.verify_no_live_proof(
        &tuple,
        &reservation.reservation_id,
        reservation.generation,
        "intent_tamper",
        lumina::mcp_platform::intake_remote_inspection::RemoteInspectionNoLiveProofKind::Tombstoned,
        14,
    );
    let _reservation = service
        .gate_validated_remote_inspection_tombstone_v26(TombstoneRemoteInspectionPrebind {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            proof: &proof,
            now_ms: 14,
        })
        .await
        .unwrap();

    let reuse = repository
        .tamper_try_reuse_remote_intent_fingerprint(&reservation.reservation_id, 15)
        .await
        .unwrap_err();
    assert_eq!(reuse.code(), McpPlatformErrorCode::IntegrityError);
}

#[test]
fn batch2_1_manual_https_prevalidation_accepts_and_rejects_samples() {
    let valid = lumina::mcp_platform::intake_remote_inspection::canonicalize_manual_https_target(
        "https://ExAmPle.com.:8443/a/b",
    )
    .unwrap();
    assert_eq!(valid.as_str(), "https://example.com:8443/a/b");
    assert_eq!(valid.path(), "/a/b");

    let rejected =
        lumina::mcp_platform::intake_remote_inspection::canonicalize_manual_https_target(
            "https://127.0.0.1/mcp",
        )
        .unwrap_err();
    assert_eq!(
        rejected.subcode,
        lumina::mcp_platform::intake_remote_inspection::RemoteInspectionSafeSubcode::IpLiteralRejected
    );

    for target in [
        "https://example.com/a/../b//",
        "https://example.com/./a",
        "https://example.com/../a",
        "https://example.com/a\\b",
    ] {
        let rejected =
            lumina::mcp_platform::intake_remote_inspection::canonicalize_manual_https_target(
                target,
            )
            .unwrap_err();
        assert_eq!(
            rejected.subcode,
            lumina::mcp_platform::intake_remote_inspection::RemoteInspectionSafeSubcode::InvalidUrl
        );
    }
}

#[tokio::test]
async fn batch2_1_remote_attempts_enforce_uniqueness_and_finalize_fail_closed() {
    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let private_reference = repository
        .mint_intake_private_reference(
            lumina::mcp_platform::intake::IntakePrivateReferenceKind::ManualHttpsEndpoint,
        )
        .unwrap();
    repository
        .save_intake_configuration_ref(lumina::mcp_platform::intake::SaveIntakeConfigurationRef {
            configuration_ref: "cfg_b21_remote",
            descriptor:
                &lumina::mcp_platform::intake::IntakeConfigurationDescriptor::ManualHttpsCandidate {
                    display_origin: "https://safe.example.com".to_string(),
                },
            private_reference: Some(private_reference.as_str()),
            descriptor_digest: &"9".repeat(64),
            now_ms: 10,
        })
        .await
        .unwrap();
    let candidate = repository
        .save_intake_candidate(lumina::mcp_platform::intake::SaveIntakeCandidate {
            candidate_id: "candidate_b21_remote",
            submission_binding: &"1".repeat(64),
            source_facet: lumina::mcp_platform::intake::IntakeSourceFacet::ManualHttpsCandidate,
            transport: lumina::mcp_platform::intake::IntakeTransport::StreamableHttp,
            initial_state: lumina::mcp_platform::intake::IntakeLifecycleState::AwaitingConsent,
            redacted_configuration_ref: "cfg_b21_remote",
            descriptor_digest: &"9".repeat(64),
            now_ms: 11,
        })
        .await
        .unwrap();
    let purpose =
        RemoteInspectionPurpose::derive(candidate.source_facet, candidate.transport).unwrap();
    let policy_revision =
        lumina::mcp_platform::intake_remote_inspection::trusted_remote_inspection_policy_revision(
            purpose,
        )
        .unwrap();
    let authority = RemoteInspectionTargetAuthority::in_memory_for_testing();
    let reservation = service
        .gate_validated_remote_inspection_reserve_v26(ReserveOrLoadRemoteInspection {
            reservation_id: "remote_reservation_1",
            candidate_id: &candidate.candidate_id,
            candidate_revision: candidate.revision,
            candidate_lifecycle_state: candidate.lifecycle_state,
            source_facet: candidate.source_facet,
            transport: candidate.transport,
            purpose,
            policy_revision,
            intent_fingerprint: "intent_1",
            intent_fingerprint_key_id: "intent_key_1",
            now_ms: 12,
        })
        .await
        .unwrap();
    let tuple = authority.mint_verified_opaque_tuple();
    let reservation = service
        .gate_validated_remote_inspection_bind_v26(BindRemoteInspectionPendingCommit {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_1",
            candidate_lifecycle_state: candidate.lifecycle_state,
            expires_at_ms: 20,
            verified_tuple: &tuple,
            now_ms: 12,
        })
        .await
        .unwrap();
    let binding_update = repository
        .tamper_try_update_remote_binding_target_handle("remote_consent_1")
        .await
        .unwrap_err();
    assert_eq!(binding_update.code(), McpPlatformErrorCode::IntegrityError);
    let mismatched_commit = repository
        .tamper_try_insert_mismatched_commit_tuple("remote_consent_1", 12)
        .await
        .unwrap_err();
    assert_eq!(
        mismatched_commit.code(),
        McpPlatformErrorCode::IntegrityError
    );
    let reservation = service
        .gate_validated_remote_inspection_commit_outcome_v26(RecordRemoteInspectionCommitOutcome {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_1",
            outcome: &authority.committed(),
            now_ms: 12,
        })
        .await
        .unwrap();

    let _claimed = service
        .gate_validated_remote_inspection_claim_v26(ClaimCommittedRemoteInspectionAttempt {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_1",
            attempt_id: "remote_attempt_1",
            claim_nonce: "claim_nonce_1",
            claimed_at_ms: 13,
            claim_expires_at_ms: 14,
        })
        .await
        .unwrap();
    let duplicate = service
        .gate_validated_remote_inspection_claim_v26(ClaimCommittedRemoteInspectionAttempt {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_1",
            attempt_id: "remote_attempt_2",
            claim_nonce: "claim_nonce_2",
            claimed_at_ms: 13,
            claim_expires_at_ms: 18,
        })
        .await
        .unwrap_err();
    assert_eq!(
        duplicate.code(),
        McpPlatformErrorCode::CandidateStateConflict
    );

    let recovered = service
        .gate_validated_remote_inspection_claim_v26(ClaimCommittedRemoteInspectionAttempt {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_1",
            attempt_id: "remote_attempt_2",
            claim_nonce: "claim_nonce_2",
            claimed_at_ms: 15,
            claim_expires_at_ms: 18,
        })
        .await
        .unwrap();
    let stale = repository
        .get_remote_inspection_attempt("remote_attempt_1")
        .await
        .unwrap();
    assert_eq!(
        stale.state,
        lumina::mcp_platform::intake_remote_inspection::RemoteInspectionAttemptState::Finalized
    );
    assert_eq!(stale.finalized_at_ms, Some(15));
    assert_eq!(stale.owner_started_at_ms, None);
    assert_eq!(stale.owner_finished_at_ms, None);
    let stale_artifacts = repository
        .remote_inspection_attempt_artifact_counts("remote_attempt_1")
        .await
        .unwrap();
    assert_eq!(stale_artifacts, (0, 0));
    let recovered_artifacts = repository
        .remote_inspection_attempt_artifact_counts(&recovered.attempt_id)
        .await
        .unwrap();
    assert_eq!(recovered_artifacts, (0, 0));
    let reservation_events = repository
        .remote_inspection_reservation_events(&reservation.reservation_id, reservation.generation)
        .await
        .unwrap();
    assert_eq!(
        reservation_events[reservation_events.len() - 2],
        (
            5,
            "claim_expired".to_string(),
            "claimed".to_string(),
            "committed_claimable".to_string(),
            Some("remote_attempt_1".to_string()),
        )
    );
    assert_eq!(
        reservation_events[reservation_events.len() - 1],
        (
            6,
            "claimed".to_string(),
            "committed_claimable".to_string(),
            "claimed".to_string(),
            Some("remote_attempt_2".to_string()),
        )
    );
    let raced = service
        .gate_validated_remote_inspection_claim_v26(ClaimCommittedRemoteInspectionAttempt {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_1",
            attempt_id: "remote_attempt_2_race",
            claim_nonce: "claim_nonce_2_race",
            claimed_at_ms: 15,
            claim_expires_at_ms: 19,
        })
        .await
        .unwrap_err();
    assert_eq!(raced.code(), McpPlatformErrorCode::CandidateStateConflict);

    service
        .gate_validated_remote_inspection_owner_started_v26(
            &recovered.attempt_id,
            "claim_nonce_2",
            16,
        )
        .await
        .unwrap();
    service
        .gate_validated_remote_inspection_owner_finished_v26(
            &recovered.attempt_id,
            "claim_nonce_2",
            17,
        )
        .await
        .unwrap();
    let consumption = service
        .gate_validated_remote_inspection_finalize_v26(FinalizeRemoteInspectionConsumption {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_1",
            attempt_id: &recovered.attempt_id,
            snapshot_id: "remote_snapshot_1",
            claim_nonce: "claim_nonce_2",
            safe_subcode: RemoteInspectionSafeSubcode::ClassificationUnavailable,
            safe_summary: RemoteInspectionSafeSummary::ClassificationUnavailable,
            finalized_at_ms: 21,
        })
        .await
        .unwrap();
    assert_eq!(consumption.consent_id, "remote_consent_1");

    let snapshot = repository
        .get_remote_inspection_snapshot("remote_snapshot_1")
        .await
        .unwrap();
    assert_eq!(
        snapshot.purpose,
        RemoteInspectionPurpose::PrivateRemoteCheckV26
    );
    assert_eq!(
        snapshot.safe_subcode,
        RemoteInspectionSafeSubcode::ClassificationUnavailable
    );
    assert!(snapshot.has_authority_record_ref);
    let consent = repository
        .get_remote_inspection_consent("remote_consent_1")
        .await
        .unwrap();
    assert_eq!(
        consent.purpose,
        RemoteInspectionPurpose::PrivateRemoteCheckV26
    );
    assert!(consent.has_authority_record_ref);
    assert_eq!(snapshot.authority_provider, consent.authority_provider);
    assert_eq!(snapshot.provider_key_epoch, consent.provider_key_epoch);
    assert_eq!(snapshot.generation, consent.generation);
    let consent_purpose = repository
        .remote_inspection_persisted_consent_purpose("remote_consent_1")
        .await
        .unwrap();
    let snapshot_purpose = repository
        .remote_inspection_persisted_snapshot_purpose("remote_snapshot_1")
        .await
        .unwrap();
    assert_eq!(consent_purpose, "private_remote_check_v25");
    assert_eq!(snapshot_purpose, "private_remote_check_v25");
    let consent_update = repository
        .tamper_try_update_catalog_remote_consent("remote_consent_1")
        .await
        .unwrap_err();
    assert_eq!(consent_update.code(), McpPlatformErrorCode::IntegrityError);
    let snapshot_update = repository
        .tamper_try_update_catalog_remote_snapshot("remote_snapshot_1")
        .await
        .unwrap_err();
    assert_eq!(snapshot_update.code(), McpPlatformErrorCode::IntegrityError);

    let consumed = service
        .gate_validated_remote_inspection_claim_v26(ClaimCommittedRemoteInspectionAttempt {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_1",
            attempt_id: "remote_attempt_3",
            claim_nonce: "claim_nonce_3",
            claimed_at_ms: 22,
            claim_expires_at_ms: 23,
        })
        .await
        .unwrap_err();
    assert_eq!(
        consumed.code(),
        McpPlatformErrorCode::CandidateStateConflict
    );
}

#[tokio::test]
async fn batch2_1_remote_attempt_time_regression_fails_closed() {
    let (_directory, repository, _clock, _policy, _provider, service) = service(10).await;
    let private_reference = repository
        .mint_intake_private_reference(
            lumina::mcp_platform::intake::IntakePrivateReferenceKind::ManualHttpsEndpoint,
        )
        .unwrap();
    repository
        .save_intake_configuration_ref(lumina::mcp_platform::intake::SaveIntakeConfigurationRef {
            configuration_ref: "cfg_b21_remote_time",
            descriptor:
                &lumina::mcp_platform::intake::IntakeConfigurationDescriptor::ManualHttpsCandidate {
                    display_origin: "https://safe.example.com".to_string(),
                },
            private_reference: Some(private_reference.as_str()),
            descriptor_digest: &"a".repeat(64),
            now_ms: 10,
        })
        .await
        .unwrap();
    let candidate = repository
        .save_intake_candidate(lumina::mcp_platform::intake::SaveIntakeCandidate {
            candidate_id: "candidate_b21_remote_time",
            submission_binding: &"2".repeat(64),
            source_facet: lumina::mcp_platform::intake::IntakeSourceFacet::ManualHttpsCandidate,
            transport: lumina::mcp_platform::intake::IntakeTransport::StreamableHttp,
            initial_state: lumina::mcp_platform::intake::IntakeLifecycleState::AwaitingConsent,
            redacted_configuration_ref: "cfg_b21_remote_time",
            descriptor_digest: &"a".repeat(64),
            now_ms: 11,
        })
        .await
        .unwrap();
    let purpose =
        RemoteInspectionPurpose::derive(candidate.source_facet, candidate.transport).unwrap();
    let policy_revision =
        lumina::mcp_platform::intake_remote_inspection::trusted_remote_inspection_policy_revision(
            purpose,
        )
        .unwrap();
    let authority = RemoteInspectionTargetAuthority::in_memory_for_testing();
    let reservation = service
        .gate_validated_remote_inspection_reserve_v26(ReserveOrLoadRemoteInspection {
            reservation_id: "remote_reservation_time",
            candidate_id: &candidate.candidate_id,
            candidate_revision: candidate.revision,
            candidate_lifecycle_state: candidate.lifecycle_state,
            source_facet: candidate.source_facet,
            transport: candidate.transport,
            purpose,
            policy_revision,
            intent_fingerprint: "intent_time",
            intent_fingerprint_key_id: "intent_key_time",
            now_ms: 12,
        })
        .await
        .unwrap();
    let tuple = authority.mint_verified_opaque_tuple();
    let reservation = service
        .gate_validated_remote_inspection_bind_v26(BindRemoteInspectionPendingCommit {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_time",
            candidate_lifecycle_state: candidate.lifecycle_state,
            expires_at_ms: 100,
            verified_tuple: &tuple,
            now_ms: 12,
        })
        .await
        .unwrap();
    let reservation = service
        .gate_validated_remote_inspection_commit_outcome_v26(RecordRemoteInspectionCommitOutcome {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_time",
            outcome: &authority.committed(),
            now_ms: 12,
        })
        .await
        .unwrap();

    let attempt = service
        .gate_validated_remote_inspection_claim_v26(ClaimCommittedRemoteInspectionAttempt {
            reservation_id: &reservation.reservation_id,
            generation: reservation.generation,
            consent_id: "remote_consent_time",
            attempt_id: "remote_attempt_time",
            claim_nonce: "claim_nonce_time",
            claimed_at_ms: 13,
            claim_expires_at_ms: 18,
        })
        .await
        .unwrap();
    service
        .gate_validated_remote_inspection_owner_started_v26(
            &attempt.attempt_id,
            "claim_nonce_time",
            16,
        )
        .await
        .unwrap();
    repository
        .tamper_remote_attempt_owner_finished_before_started(&attempt.attempt_id, 15)
        .await
        .unwrap();
    let error = repository
        .get_remote_inspection_attempt(&attempt.attempt_id)
        .await
        .unwrap_err();
    assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
}
