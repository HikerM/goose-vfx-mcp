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
            json!({"jobId":"job:unit","action":"pause"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelInstallationJobControlRequest>(
            json!({"jobId":"job:unit","expectedVersion":1,"action":"reset"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelInstallationJobControlRequest>(
            json!({"jobId":"job:unit","expectedVersion":-1,"action":"pause"})
        )
        .is_err()
    );
    let request: ModelInstallationJobsListRequest = serde_json::from_value(json!({})).unwrap();
    assert_eq!(request.limit, 64);
    assert_eq!(ModelInstallationJobsListRequest::default().limit, 64);
    assert!(request.after_id.is_none());
}
