use std::process::Command;

fn cargo_manifest(lumina_manifest_dir: &str) -> String {
    format!(
        r#"[package]
name = "lumina-public-api-fixture"
version = "0.0.0"
edition = "2021"

[dependencies]
lumina = {{ path = "{lumina_manifest_dir}" }}
"#
    )
}

const PUBLIC_WRITE_FIXTURE: &str = r#"use lumina::mcp_platform::{
    McpPlatformRepositoryPort, SqliteMcpPlatformRepository,
};

async fn forbidden() {
    let repository = SqliteMcpPlatformRepository::open_path("platform.db").await.unwrap();
    let _ = SqliteMcpPlatformRepository::open_path_with_integrity_signer(todo!(), todo!()).await;
    let _ = repository.save_plan(todo!()).await;
    let _ = repository.create_task(todo!()).await;
    let _ = repository.transition_task(todo!()).await;
    let _ = repository.create_profile(todo!()).await;
    let _ = repository.consume_profile_application_token(todo!(), todo!(), todo!()).await;
}
"#;

#[test]
fn public_repository_write_surface_is_not_exported() {
    let workspace = tempfile::tempdir().unwrap();
    let fixture_dir = workspace.path().join("fixture");
    std::fs::create_dir_all(fixture_dir.join("src")).unwrap();

    let lumina_manifest_dir = env!("CARGO_MANIFEST_DIR").replace('\\', "/");
    std::fs::write(
        fixture_dir.join("Cargo.toml"),
        cargo_manifest(&lumina_manifest_dir),
    )
    .unwrap();
    std::fs::write(fixture_dir.join("src").join("lib.rs"), PUBLIC_WRITE_FIXTURE).unwrap();

    let output = Command::new("cargo")
        .args(["check", "--quiet", "--color", "never"])
        .current_dir(&fixture_dir)
        .env(
            "CARGO_TARGET_DIR",
            r"D:\DevTools\Lumina\buildcache-r12a-r10\public-api-fixtures",
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
        "McpPlatformRepositoryPort",
        "open_path_with_integrity_signer",
        "save_plan",
        "create_task",
        "transition_task",
        "create_profile",
        "consume_profile_application_token",
    ] {
        assert!(
            combined.contains(needle),
            "expected compile failure output to mention {needle}, got:\n{combined}"
        );
    }
}
