use std::process::Command;

use goose::custom_requests::{McpCatalogDetail, McpInstallConfirmRequest};

fn cargo_manifest(goose_manifest_dir: &str) -> String {
    format!(
        r#"[package]
name = "goose-profile-public-api-fixture"
version = "0.0.0"
edition = "2021"

[dependencies]
goose = {{ path = "{goose_manifest_dir}" }}
"#
    )
}

const PROFILE_INTERNAL_FIXTURE: &str = r#"use goose::mcp_platform::{
    ConsumedProfileApplication, McpPlatformService, ProfileApplicationMarker,
    SqliteMcpPlatformRepository,
};
use std::sync::Arc;

async fn forbidden(service: &McpPlatformService) {
    let _ = service
        .consume_profile_application_token(todo!(), "profile_application_1")
        .await;
    let _ = service
        .hydrate_profile_application(todo!(), &ProfileApplicationMarker {
            application_id: "profile_application_1".to_string(),
            profile_id: "profile_1".to_string(),
            profile_revision: 1,
            plan_digest: "a".repeat(64),
            merge_policy: "replace_managed_only".to_string(),
            original_session_id: "session_1".to_string(),
            managed_references: Vec::new(),
        }, "session_1")
        .await;
    let application: &ConsumedProfileApplication = todo!();
    let _ = service
        .verify_profile_application_runtime(todo!(), &application)
        .await;
    let _ = service
        .finish_profile_application("profile_application_1", Some("session_1"), "created", "session_created")
        .await;
    let _ = service.profile_application_cleanup_state("session_1").await;
}

async fn build() {
    let repository = Arc::new(SqliteMcpPlatformRepository::open_path("platform.db").await.unwrap());
    let service = McpPlatformService::production(repository);
    forbidden(&service).await;
}
"#;

#[test]
fn internal_profile_application_surface_is_not_exported_to_external_crates() {
    let workspace = tempfile::tempdir().unwrap();
    let fixture_dir = workspace.path().join("fixture");
    std::fs::create_dir_all(fixture_dir.join("src")).unwrap();

    let goose_manifest_dir = env!("CARGO_MANIFEST_DIR").replace('\\', "/");
    std::fs::write(
        fixture_dir.join("Cargo.toml"),
        cargo_manifest(&goose_manifest_dir),
    )
    .unwrap();
    std::fs::write(
        fixture_dir.join("src").join("lib.rs"),
        PROFILE_INTERNAL_FIXTURE,
    )
    .unwrap();

    let output = Command::new("cargo")
        .args(["check", "--quiet", "--color", "never"])
        .current_dir(&fixture_dir)
        .env(
            "CARGO_TARGET_DIR",
            r"D:\DevTools\Goose\buildcache-r12a-r10\public-api-fixtures",
        )
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "external fixture unexpectedly compiled:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for needle in [
        "consume_profile_application_token",
        "hydrate_profile_application",
        "verify_profile_application_runtime",
        "finish_profile_application",
        "profile_application_cleanup_state",
    ] {
        assert!(
            combined.contains(needle),
            "expected compile failure output to mention {needle}, got:\n{combined}"
        );
    }
}

#[test]
fn wire_schemas_are_closed_and_never_accept_raw_execution_or_auth_material() {
    let manual_schema = serde_json::to_string(&schemars::schema_for!(
        goose::custom_requests::McpManualPlanCreateRequest
    ))
    .unwrap();
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
    let task_schema =
        serde_json::to_string(&schemars::schema_for!(goose::custom_requests::McpTaskRef)).unwrap();
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
