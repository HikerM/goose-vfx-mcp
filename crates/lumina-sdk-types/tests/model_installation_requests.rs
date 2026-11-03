use lumina_sdk_types::custom_requests::*;
use serde_json::json;

#[test]
fn installation_submission_rejects_credentials_destinations_and_download_urls() {
    let request = json!({"planId":"plan:unit","planDigest":"a".repeat(64),"installationId":"installation:unit","idempotencyKey":"key:unit"});
    assert!(serde_json::from_value::<ModelInstallationSubmitRequest>(request.clone()).is_ok());
    for field in ["token", "authorization", "url", "storeRoot", "versionId"] {
        let mut changed = request.clone();
        changed[field] = json!("not accepted");
        assert!(serde_json::from_value::<ModelInstallationSubmitRequest>(changed).is_err());
    }
}

#[test]
fn installation_controls_require_a_version_and_a_known_action() {
    assert!(
        serde_json::from_value::<ModelInstallationJobControlRequest>(
            json!({"jobId":"job:unit","action":"pause","idempotencyKey":"key:unit"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelInstallationJobControlRequest>(
            json!({"jobId":"job:unit","expectedVersion":1,"action":"reset","idempotencyKey":"key:unit"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelInstallationJobControlRequest>(
            json!({"jobId":"job:unit","expectedVersion":-1,"action":"pause","idempotencyKey":"key:unit"})
        )
        .is_err()
    );
    let request: ModelInstallationJobsListRequest = serde_json::from_value(json!({})).unwrap();
    assert_eq!(request.limit, 64);
    assert_eq!(ModelInstallationJobsListRequest::default().limit, 64);
    assert!(request.after_id.is_none());
}

#[test]
fn installation_controls_require_the_callers_idempotency_key() {
    assert!(
        serde_json::from_value::<ModelInstallationJobControlRequest>(
            json!({"jobId":"job:unit","expectedVersion":1,"action":"pause"})
        )
        .is_err()
    );
    let request = json!({"jobId":"job:unit","expectedVersion":1,"action":"pause","idempotencyKey":"key:unit"});
    assert!(serde_json::from_value::<ModelInstallationJobControlRequest>(request.clone()).is_ok());
    for field in ["token", "storeRoot", "url"] {
        let mut changed = request.clone();
        changed[field] = json!("not accepted");
        assert!(serde_json::from_value::<ModelInstallationJobControlRequest>(changed).is_err());
    }
}

#[test]
fn installation_queries_decode_old_responses_and_expose_typed_retry_state() {
    let old: ModelInstallationJobsListResponse = serde_json::from_value(json!({
        "jobs":[],"nextAfterId":null,"serviceState":"dormant"
    }))
    .unwrap();
    assert!(old.file_retries.is_empty());
    assert!(old.failures.is_empty());
    assert!(old.storage_reservations.is_empty());
    let dto: ModelInstallationFileRetryDto = serde_json::from_value(json!({
        "jobId":"job:unit","artifactId":"file:unit","policyVersion":"installation-file-retry-v1",
        "maxAttempts":3,"cycle":1,"cycleAttempts":2,"recordedAttempts":5,
        "historyOrigin":"from_enqueue","trackingStartedAt":10,"state":"waiting",
        "nextRetryAt":50,"lastFailure":{"code":"SOURCE_RATE_LIMITED","stage":"downloading","failedAt":20,"retryAfterSeconds":30},
        "version":4,"updatedAt":20
    })).unwrap();
    assert!(matches!(
        dto.state,
        ModelInstallationFileRetryState::Waiting
    ));
    assert!(matches!(
        dto.last_failure.unwrap().code,
        ModelInstallationFailureCode::SourceRateLimited
    ));
    assert_eq!(dto.next_retry_at, Some(50));
}

#[test]
fn storage_query_wire_data_reports_reserved_peak_and_release_without_store_paths() {
    let storage:ModelInstallationStorageDto=serde_json::from_value(json!({
        "jobId":"job:unit","planDigest":"a".repeat(64),"volumeId":"windows-volume:12345678",
        "peakBytes":1024,"reservedBytes":0,"state":"released","version":2,"createdAt":10,"updatedAt":20
    })).unwrap();
    assert!(matches!(
        storage.state,
        ModelInstallationStorageState::Released
    ));
    assert_eq!(storage.peak_bytes, 1024);
    assert_eq!(storage.reserved_bytes, 0);
    assert!(serde_json::to_value(storage)
        .unwrap()
        .get("storeRoot")
        .is_none());
}

#[test]
fn installation_failure_wire_data_carries_recovery_and_history_identity() {
    let failure: ModelInstallationFailureDto = serde_json::from_value(json!({
        "jobId":"job:unit","jobVersion":4,"code":"SOURCE_AUTH_REQUIRED","stage":"ready_to_install",
        "origin":"credentials","artifactId":null,"messageKey":"models.installation.failure.source_auth_required",
        "messageArgs":{},"retryable":false,"recoverable":true,"causeId":"installation-failure:job:unit:4",
        "automaticRetryAt":null,"isCurrent":true,"occurredAt":50
    })).unwrap();
    assert!(matches!(
        failure.origin,
        ModelInstallationFailureOrigin::Credentials
    ));
    assert!(matches!(
        failure.code,
        ModelInstallationFailureCode::SourceAuthRequired
    ));
    assert!(failure.is_current && failure.recoverable && !failure.retryable);
    assert_eq!(failure.job_version, 4);
}
