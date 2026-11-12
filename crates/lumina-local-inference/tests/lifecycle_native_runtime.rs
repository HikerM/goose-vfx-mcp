use lumina_local_inference::lifecycle::*;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

fn unregistered_launch() -> LaunchConfig {
    LaunchConfig {
        installation_id: "missing-installation".into(),
        version_id: "missing-version".into(),
        manifest_digest: Sha256Digest::of_bytes(b"not installed"),
        runtime: baseline_pairing_runtime().unwrap(),
        capabilities: BTreeSet::from([ModelCapability::Text]),
        component_files: BTreeMap::new(),
        parameters: RuntimeParameters::Specialized {
            schema_id: "not-a-product-runtime".into(),
            settings: BTreeMap::new(),
        },
        mode: RuntimeExecutionMode::Helper { port: 0 },
    }
}

#[tokio::test]
async fn unregistered_launch_never_creates_logs_or_spawns_a_binary() {
    let temp = tempfile::tempdir().unwrap();
    let repository = LifecycleRepository::open(&temp.path().join("lifecycle.db")).unwrap();
    let store = InstallationStore::open(&temp.path().join("store")).unwrap();
    let logs = temp.path().join("not-created");
    let result = NativeHelperRuntime::start(
        &repository,
        &store,
        unregistered_launch(),
        &temp.path().join("missing-helper"),
        &logs,
        Duration::from_secs(1),
    )
    .await;
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("registered installation"));
    assert!(!logs.exists());
    assert!(!store.root().join("locks/runtime/host.lock").exists());
}

#[tokio::test]
async fn zero_startup_timeout_is_rejected_before_preparation() {
    let temp = tempfile::tempdir().unwrap();
    let repository = LifecycleRepository::open(&temp.path().join("lifecycle.db")).unwrap();
    let store = InstallationStore::open(&temp.path().join("store")).unwrap();
    let logs = temp.path().join("not-created");
    let result = NativeHelperRuntime::start(
        &repository,
        &store,
        unregistered_launch(),
        &temp.path().join("missing-helper"),
        &logs,
        Duration::ZERO,
    )
    .await;
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("timeout must be positive"));
    assert!(!logs.exists());
}
