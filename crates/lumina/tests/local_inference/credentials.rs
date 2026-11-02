use super::*;
use crate::config::Config;

fn config(directory: &tempfile::TempDir) -> Config {
    Config::new_with_file_secrets(
        directory.path().join("config.yaml"),
        directory.path().join("secrets.yaml"),
    )
    .unwrap()
}

#[test]
fn modelscope_absent_credentials_remain_optional_and_stored_credentials_are_read() {
    let _guard = env_lock::lock_env([("MODELSCOPE_TOKEN", None::<&str>)]);
    let directory = tempfile::tempdir().unwrap();
    let config = config(&directory);
    assert!(resolve_modelscope_credential(&config).unwrap().is_none());
    config
        .set_secret("MODELSCOPE_TOKEN", &"unit-stored-token")
        .unwrap();
    assert_eq!(
        resolve_modelscope_credential(&config).unwrap().as_deref(),
        Some("unit-stored-token")
    );
}

#[test]
fn modelscope_invalid_configuration_is_an_error_without_secret_values_in_diagnostics() {
    let _guard = env_lock::lock_env([("MODELSCOPE_TOKEN", None::<&str>)]);
    let directory = tempfile::tempdir().unwrap();
    let config = config(&directory);
    config
        .set_param(
            "MODELSCOPE_TOKEN",
            serde_json::json!({"private-sentinel":"unit-token"}),
        )
        .unwrap();
    let error = resolve_modelscope_credential(&config).unwrap_err();
    assert_eq!(
        error.to_string(),
        "ModelScope credential configuration is unavailable"
    );
    assert!(!format!("{error:?}").contains("private-sentinel"));
}

#[test]
fn modelscope_unreadable_secret_storage_does_not_become_anonymous_access() {
    let _guard = env_lock::lock_env([("MODELSCOPE_TOKEN", None::<&str>)]);
    let directory = tempfile::tempdir().unwrap();
    let config = config(&directory);
    std::fs::write(
        directory.path().join("secrets.yaml"),
        "{private-sentinel: [",
    )
    .unwrap();
    let error = resolve_modelscope_credential(&config).unwrap_err();
    assert_eq!(
        error.to_string(),
        "ModelScope credential storage is unavailable"
    );
    assert!(!format!("{error:?}").contains("private-sentinel"));
}
