use fs2::FileExt;
use lumina_local_inference::local_model_registry::{
    migrate_legacy_registry, LegacyAssetOwnership, LegacyImportDiagnosticCode,
    LegacySourceTraceability, LegacyVerificationState, LocalModelEntry, LocalModelRegistry,
    ModelSettings,
};
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    _temp: tempfile::TempDir,
    data: PathBuf,
    registry: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/legacy-registry-tests");
        std::fs::create_dir_all(&root).unwrap();
        let temp = tempfile::Builder::new()
            .prefix("registry-")
            .tempdir_in(&root)
            .unwrap();
        let data = temp.path().join("data");
        std::fs::create_dir_all(data.join("models")).unwrap();
        let registry = data.join("models/registry.json");
        Self {
            _temp: temp,
            data,
            registry,
        }
    }

    fn write(&self, records: Vec<Value>) -> Vec<u8> {
        let raw = json!({"models": records, "future_root": {"keep": "unchanged"}});
        let bytes = format!("  {}\n", serde_json::to_string_pretty(&raw).unwrap()).into_bytes();
        std::fs::write(&self.registry, &bytes).unwrap();
        bytes
    }

    fn record(&self, id: &str) -> Value {
        json!({
            "id": id,
            "repo_id": "historical/model",
            "filename": "original.gguf",
            "quantization": "Q4_K_M",
            "local_path": self.data.join("original.gguf"),
            "source_url": "https://example.invalid/resolve/main/original.gguf",
            "settings": ModelSettings::default(),
            "future_entry": [1, {"keep": true}],
        })
    }

    fn load(&self) -> LocalModelRegistry {
        LocalModelRegistry::load_from_data_dir(&self.data).unwrap()
    }

    fn raw(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.registry).unwrap()).unwrap()
    }

    fn child(&self, action: &str, name: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "legacy_registry_child",
                "--nocapture",
            ])
            .env("LUMINA_LEGACY_TEST_ROOT", &self.data)
            .env("LUMINA_LEGACY_TEST_ACTION", action)
            .env("LUMINA_LEGACY_TEST_NAME", name)
            .env("LUMINA_PATH_ROOT", self.data.parent().unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn finish_child(mut child: Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().unwrap().is_some() {
            let result = child.wait_with_output().unwrap();
            assert!(
                result.status.success(),
                "Child failed: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Registry child did not finish");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn imports_original_bytes_once_without_inventing_source_or_storage() {
    let fixture = Fixture::new();
    let bytes = fixture.write(vec![fixture.record("old-model")]);
    let first = fixture.load();
    let report = first.migration_report().unwrap();
    assert_eq!(std::fs::read(&report.backup_path).unwrap(), bytes);
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.aliases.len(), 1);
    let alias = &report.aliases[0];
    assert_eq!(alias.legacy_id, "old-model");
    assert_eq!(alias.declared_storage, None);
    assert_eq!(
        alias.verification_state,
        LegacyVerificationState::ImportedUnverified
    );
    assert_eq!(
        alias.source_traceability,
        LegacySourceTraceability::Unverified
    );
    assert_eq!(alias.assets[0].path, fixture.data.join("original.gguf"));
    assert_eq!(
        alias.assets[0].ownership,
        LegacyAssetOwnership::ReferencedOnly
    );
    assert!(alias.original_record.get("storage").is_none());
    let repeated = migrate_legacy_registry(&fixture.data).unwrap().unwrap();
    assert_eq!(report, &repeated);
    assert_eq!(std::fs::read(&fixture.registry).unwrap(), bytes);
    assert_eq!(
        std::fs::read_dir(fixture.data.join("model-lifecycle/legacy-imports/v1"))
            .unwrap()
            .count(),
        1
    );
    assert!(!fixture.data.join("model-lifecycle/models.db").exists());
    assert!(!fixture.data.join("model-lifecycle/blobs").exists());
}

#[test]
fn rejects_corrupt_and_duplicate_records_after_preserving_diagnostics() {
    for (bytes, expected) in [
        (
            b"{\"models\":[".to_vec(),
            LegacyImportDiagnosticCode::InvalidJson,
        ),
        (
            b"\xff\xfe".to_vec(),
            LegacyImportDiagnosticCode::InvalidJson,
        ),
        (b"{}".to_vec(), LegacyImportDiagnosticCode::InvalidRegistry),
        (
            b"{\"models\":[{}]}".to_vec(),
            LegacyImportDiagnosticCode::InvalidRecord,
        ),
    ] {
        let fixture = Fixture::new();
        std::fs::write(&fixture.registry, &bytes).unwrap();
        let report = migrate_legacy_registry(&fixture.data).unwrap().unwrap();
        assert_eq!(report.diagnostics[0].code, expected);
        assert_eq!(std::fs::read(&report.backup_path).unwrap(), bytes);
        let error = LocalModelRegistry::load_from_data_dir(&fixture.data).unwrap_err();
        assert!(error.to_string().contains("needs repair"));
        assert_eq!(std::fs::read(&fixture.registry).unwrap(), bytes);
        assert_eq!(
            migrate_legacy_registry(&fixture.data).unwrap().unwrap(),
            report
        );
    }
    let fixture = Fixture::new();
    let bytes = fixture.write(vec![
        fixture.record("duplicate"),
        fixture.record("duplicate"),
    ]);
    let report = migrate_legacy_registry(&fixture.data).unwrap().unwrap();
    assert_eq!(
        report.diagnostics[0].code,
        LegacyImportDiagnosticCode::DuplicateId
    );
    assert_eq!(report.aliases.len(), 2);
    assert!(LocalModelRegistry::load_from_data_dir(&fixture.data).is_err());
    assert_eq!(std::fs::read(&fixture.registry).unwrap(), bytes);
}

#[test]
fn updates_known_settings_without_losing_unknown_fields_or_missing_storage() {
    let fixture = Fixture::new();
    let mut record = fixture.record("old-model");
    record["settings"]["future_sampling"] = json!({"keep": [1, 2]});
    record["settings"]["sampling"]["future_temperature"] = json!(42);
    record["settings"]["chat_template"] = json!({"type": "auto", "future_template": true});
    record["settings"]["backend_id"] = json!("old-backend");
    let original = fixture.write(vec![record]);
    let mut registry = fixture.load();
    let first_backup = registry.migration_report().unwrap().backup_path.clone();
    let mut settings = registry.get_model_settings("old-model").unwrap().clone();
    settings.context_size = Some(32768);
    settings.backend_id = None;
    registry
        .update_model_settings("old-model", settings)
        .unwrap();
    let raw = fixture.raw();
    assert_eq!(raw["future_root"], json!({"keep": "unchanged"}));
    assert_eq!(raw["models"][0]["future_entry"], json!([1, {"keep": true}]));
    assert!(raw["models"][0].get("storage").is_none());
    assert_eq!(raw["models"][0]["settings"]["context_size"], 32768);
    assert_eq!(
        raw["models"][0]["settings"]["future_sampling"],
        json!({"keep": [1, 2]})
    );
    assert_eq!(
        raw["models"][0]["settings"]["sampling"]["future_temperature"],
        42
    );
    assert_eq!(
        raw["models"][0]["settings"]["chat_template"]["type"],
        "auto"
    );
    assert!(raw["models"][0]["settings"].get("backend_id").is_none());
    assert_eq!(std::fs::read(first_backup).unwrap(), original);
    registry.apply_safe_runtime_defaults().unwrap();
    registry.save().unwrap();
    assert_eq!(
        fixture
            .load()
            .get_model_settings("old-model")
            .unwrap()
            .context_size,
        Some(32768)
    );
}

#[test]
fn automatic_featured_sync_preserves_unavailable_models_and_user_settings() {
    let fixture = Fixture::new();
    let mut record = fixture.record("unavailable-old-model");
    record["settings"]["context_size"] = json!(16384);
    record["settings"]["draft_model"] = json!("user-draft-model");
    let original = fixture.write(vec![record]);
    let mut registry = fixture.load();
    let backup = registry.migration_report().unwrap().backup_path.clone();
    registry.sync_with_featured(Vec::new()).unwrap();
    assert_eq!(std::fs::read(&fixture.registry).unwrap(), original);
    let mut replacement: LocalModelEntry =
        serde_json::from_value(fixture.record("unavailable-old-model")).unwrap();
    replacement.local_path = fixture.data.join("new-location.gguf");
    registry.add_model(replacement).unwrap();
    let entry = fixture
        .load()
        .get_model("unavailable-old-model")
        .unwrap()
        .clone();
    assert_eq!(entry.settings.context_size, Some(16384));
    assert_eq!(
        entry.settings.draft_model.as_deref(),
        Some("user-draft-model")
    );
    assert_eq!(std::fs::read(backup).unwrap(), original);
}

#[test]
fn deletes_only_registration_for_all_historical_storage_declarations() {
    for storage in [
        None,
        Some("lumina_managed"),
        Some("hugging_face_cache"),
        Some("manual_path"),
    ] {
        let fixture = Fixture::new();
        let model = fixture.data.join("original.gguf");
        let linked = fixture.data.join("shared.gguf");
        let mmproj = fixture.data.join("shared-mmproj.gguf");
        std::fs::write(&model, b"original user model").unwrap();
        std::fs::hard_link(&model, &linked).unwrap();
        std::fs::write(&mmproj, b"shared projection").unwrap();
        let before = std::fs::metadata(&model).unwrap();
        let mut record = fixture.record("old-model");
        if let Some(storage) = storage {
            record["storage"] = json!(storage);
        }
        record["mmproj_path"] = json!(mmproj);
        record["shard_files"] = json!([{
            "filename": "shared.gguf", "local_path": linked,
            "source_url": "unknown", "size_bytes": 19,
        }]);
        let mut other = fixture.record("other-model");
        other["mmproj_path"] = json!(mmproj);
        fixture.write(vec![record, other]);
        fixture.load().delete_model("old-model").unwrap();
        let registry = fixture.load();
        assert!(!registry.has_model("old-model"));
        assert!(registry.has_model("other-model"));
        assert_eq!(std::fs::read(&model).unwrap(), b"original user model");
        assert_eq!(std::fs::read(&linked).unwrap(), b"original user model");
        assert_eq!(std::fs::read(&mmproj).unwrap(), b"shared projection");
        let after = std::fs::metadata(&model).unwrap();
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        assert_eq!(
            before.permissions().readonly(),
            after.permissions().readonly()
        );
    }
}

#[test]
fn stale_settings_and_delete_are_rejected_without_overwriting_a_newer_snapshot() {
    let fixture = Fixture::new();
    fixture.write(vec![fixture.record("old-model")]);
    let mut stale_settings = fixture.load();
    let mut stale_delete = fixture.load();
    let mut winner = fixture.load();
    let mut settings = winner.get_model_settings("old-model").unwrap().clone();
    settings.context_size = Some(65536);
    winner.update_model_settings("old-model", settings).unwrap();
    let committed = std::fs::read(&fixture.registry).unwrap();
    let error = stale_settings
        .update_model_settings("old-model", ModelSettings::default())
        .unwrap_err();
    assert!(error.to_string().contains("changed in another process"));
    assert!(stale_delete
        .delete_model("old-model")
        .unwrap_err()
        .to_string()
        .contains("changed in another process"));
    assert_eq!(std::fs::read(&fixture.registry).unwrap(), committed);
    assert_eq!(
        fixture
            .load()
            .get_model_settings("old-model")
            .unwrap()
            .context_size,
        Some(65536)
    );
}

#[test]
fn backup_or_ledger_corruption_prevents_registry_updates() {
    for corrupt_backup in [true, false] {
        let fixture = Fixture::new();
        let original = fixture.write(vec![fixture.record("old-model")]);
        let mut registry = fixture.load();
        let report = registry.migration_report().unwrap();
        let target = if corrupt_backup {
            &report.backup_path
        } else {
            &report.ledger_path
        };
        std::fs::write(target, b"corrupted backup metadata").unwrap();
        assert!(registry.delete_model("old-model").is_err());
        assert_eq!(std::fs::read(&fixture.registry).unwrap(), original);
        assert!(LocalModelRegistry::load_from_data_dir(&fixture.data).is_err());
    }
}

#[test]
fn interrupted_import_after_backup_resumes_and_ignores_unreferenced_snapshots() {
    let fixture = Fixture::new();
    let original = fixture.write(vec![fixture.record("old-model")]);
    let initial = fixture.load().migration_report().unwrap().clone();
    std::fs::remove_file(&initial.ledger_path).unwrap();
    assert_eq!(fixture.load().migration_report().unwrap(), &initial);
    let mut changed = fixture.load();
    let mut settings = changed.get_model_settings("old-model").unwrap().clone();
    settings.context_size = Some(16384);
    changed
        .update_model_settings("old-model", settings)
        .unwrap();
    let updated = changed.migration_report().unwrap().clone();
    assert_ne!(updated.migration_id, initial.migration_id);
    std::fs::write(&fixture.registry, &original).unwrap();
    let restored = fixture.load();
    assert_eq!(restored.migration_report().unwrap(), &initial);
    assert!(updated.ledger_path.exists());
    assert_eq!(
        restored
            .get_model_settings("old-model")
            .unwrap()
            .context_size,
        None
    );
}

#[test]
fn preserves_default_session_and_existing_lifecycle_database_bytes() {
    let fixture = Fixture::new();
    fixture.write(vec![fixture.record("old-model")]);
    let model_lifecycle = fixture.data.join("model-lifecycle");
    std::fs::create_dir_all(&model_lifecycle).unwrap();
    let paths = [
        fixture.data.join("config.yaml"),
        fixture.data.join("sessions.db"),
        model_lifecycle.join("models.db"),
        model_lifecycle.join("models.db-wal"),
    ];
    for path in &paths {
        std::fs::write(
            path,
            b"existing application-owned data; must remain untouched",
        )
        .unwrap();
    }
    fixture.load().apply_safe_runtime_defaults().unwrap();
    for path in paths {
        assert_eq!(
            std::fs::read(path).unwrap(),
            b"existing application-owned data; must remain untouched"
        );
    }
}

#[test]
fn competing_processes_commit_once_and_report_the_stale_writer() {
    let fixture = Fixture::new();
    fixture.write(vec![fixture.record("old-model")]);
    let first = fixture.child("update", "first");
    let second = fixture.child("update", "second");
    wait_for(&fixture.data.join("first.ready"));
    wait_for(&fixture.data.join("second.ready"));
    std::fs::write(fixture.data.join("continue"), b"go").unwrap();
    finish_child(first);
    finish_child(second);
    let results = ["first", "second"]
        .map(|name| std::fs::read_to_string(fixture.data.join(format!("{name}.result"))).unwrap());
    assert_eq!(
        results.iter().filter(|result| *result == "saved").count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.contains("changed in another process"))
            .count(),
        1
    );
    let expected = if results[0] == "saved" { 1024 } else { 2048 };
    assert_eq!(
        fixture
            .load()
            .get_model_settings("old-model")
            .unwrap()
            .context_size,
        Some(expected)
    );
}

#[test]
fn reader_waits_for_the_cross_process_registry_file_lock() {
    let fixture = Fixture::new();
    fixture.write(vec![fixture.record("old-model")]);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.registry.with_extension("json.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let mut child = fixture.child("read", "reader");
    wait_for(&fixture.data.join("reader.ready"));
    std::thread::sleep(Duration::from_millis(200));
    assert!(child.try_wait().unwrap().is_none());
    assert!(!fixture.data.join("reader.result").exists());
    drop(lock);
    finish_child(child);
    assert_eq!(
        std::fs::read_to_string(fixture.data.join("reader.result")).unwrap(),
        "read"
    );
}

#[test]
fn fresh_registry_creation_and_invalid_updates_obey_the_same_snapshot_rules() {
    let fixture = Fixture::new();
    let mut first = fixture.load();
    let mut stale = fixture.load();
    assert!(first.migration_report().is_none());
    assert!(!fixture.registry.exists());
    let entry: LocalModelEntry = serde_json::from_value(fixture.record("new-model")).unwrap();
    first.add_model(entry.clone()).unwrap();
    let committed = std::fs::read(&fixture.registry).unwrap();
    assert!(stale
        .add_model(entry.clone())
        .unwrap_err()
        .to_string()
        .contains("changed in another process"));
    first.models.push(entry);
    assert!(first
        .save()
        .unwrap_err()
        .to_string()
        .contains("duplicate model ID"));
    assert_eq!(std::fs::read(&fixture.registry).unwrap(), committed);
}

#[test]
fn provider_metadata_exposes_registry_corruption_instead_of_a_healthy_empty_list() {
    let fixture = Fixture::new();
    let original = b"{broken registry";
    std::fs::write(&fixture.registry, original).unwrap();
    finish_child(fixture.child("health", "health"));
    assert_eq!(
        std::fs::read_to_string(fixture.data.join("health.result")).unwrap(),
        "diagnosed"
    );
    assert_eq!(std::fs::read(&fixture.registry).unwrap(), original);
}

#[test]
fn shard_unknown_fields_follow_identity_when_reordered_added_and_removed() {
    let fixture = Fixture::new();
    let mut record = fixture.record("old-model");
    let shard = |directory: &str, checksum: &str| {
        json!({
            "filename": "weights.gguf",
            "local_path": fixture.data.join(directory).join("weights.gguf"),
            "source_url": format!("https://example.invalid/{directory}/weights.gguf"),
            "size_bytes": 100,
            "future_checksum": checksum,
        })
    };
    record["shard_files"] = json!([shard("a", "checksum-a"), shard("b", "checksum-b")]);
    let original = fixture.write(vec![record]);
    let mut registry = fixture.load();
    let backup = registry.migration_report().unwrap().backup_path.clone();
    let mut replacement = registry.get_model("old-model").unwrap().clone();
    replacement.shard_files.swap(0, 1);
    replacement.shard_files[0].size_bytes = 101;
    registry.add_model(replacement.clone()).unwrap();
    let raw = fixture.raw();
    assert_eq!(
        raw["models"][0]["shard_files"][0]["future_checksum"],
        "checksum-b"
    );
    assert_eq!(raw["models"][0]["shard_files"][0]["size_bytes"], 101);
    assert_eq!(
        raw["models"][0]["shard_files"][1]["future_checksum"],
        "checksum-a"
    );

    replacement
        .shard_files
        .push(serde_json::from_value(shard("c", "not-passed-to-typed-entry")).unwrap());
    registry.add_model(replacement.clone()).unwrap();
    let raw = fixture.raw();
    assert_eq!(
        raw["models"][0]["shard_files"][0]["future_checksum"],
        "checksum-b"
    );
    assert_eq!(
        raw["models"][0]["shard_files"][1]["future_checksum"],
        "checksum-a"
    );
    assert!(raw["models"][0]["shard_files"][2]
        .get("future_checksum")
        .is_none());

    replacement.shard_files.remove(0);
    registry.add_model(replacement).unwrap();
    let raw = fixture.raw();
    assert_eq!(raw["models"][0]["shard_files"].as_array().unwrap().len(), 2);
    assert_eq!(
        raw["models"][0]["shard_files"][0]["future_checksum"],
        "checksum-a"
    );
    assert!(raw["models"][0]["shard_files"][1]
        .get("future_checksum")
        .is_none());
    assert_eq!(std::fs::read(backup).unwrap(), original);
}

#[test]
fn ambiguous_shard_identity_does_not_overwrite_unknown_metadata() {
    let fixture = Fixture::new();
    let mut record = fixture.record("old-model");
    let first = json!({
        "filename": "duplicate.gguf", "local_path": fixture.data.join("duplicate.gguf"),
        "source_url": "unknown", "size_bytes": 100, "future_checksum": "first",
    });
    let mut second = first.clone();
    second["future_checksum"] = json!("second");
    record["shard_files"] = json!([first, second]);
    let original = fixture.write(vec![record]);
    let mut registry = fixture.load();
    registry.models[0].shard_files.remove(0);
    assert!(registry
        .save()
        .unwrap_err()
        .to_string()
        .contains("ambiguous duplicate shard identities"));
    assert_eq!(std::fs::read(&fixture.registry).unwrap(), original);
}

#[test]
fn pending_receipt_removes_only_a_new_unchanged_registration() {
    let fixture = Fixture::new();
    let original = fixture.write(vec![fixture.record("existing-model")]);
    let mut registry = fixture.load();
    let existing = registry.get_model("existing-model").unwrap().clone();
    assert!(registry.add_pending_model(existing).unwrap().is_none());
    assert_eq!(std::fs::read(&fixture.registry).unwrap(), original);

    let entry = serde_json::from_value(fixture.record("pending-model")).unwrap();
    let receipt = registry.add_pending_model(entry).unwrap().unwrap();
    assert!(fixture.raw()["models"][1]["_lumina_registration_id"]
        .as_str()
        .is_some());
    assert!(fixture
        .load()
        .remove_failed_pending_model(&receipt)
        .unwrap());
    let remaining = fixture.load();
    assert!(remaining.has_model("existing-model"));
    assert!(!remaining.has_model("pending-model"));
}

#[test]
fn pending_cleanup_preserves_unknown_edits_and_legacy_rewrites() {
    for remove_identity in [false, true] {
        let fixture = Fixture::new();
        let entry: LocalModelEntry =
            serde_json::from_value(fixture.record("pending-model")).unwrap();
        let receipt = fixture.load().add_pending_model(entry).unwrap().unwrap();
        let before = fixture.load().models[0].clone();
        let mut raw = fixture.raw();
        if remove_identity {
            raw["models"][0]
                .as_object_mut()
                .unwrap()
                .remove("_lumina_registration_id");
        } else {
            raw["models"][0]["future_user_note"] = json!({"retain": "user change"});
        }
        let edited = serde_json::to_vec_pretty(&raw).unwrap();
        std::fs::write(&fixture.registry, &edited).unwrap();
        let mut updated = fixture.load();
        assert_eq!(
            serde_json::to_value(&updated.models[0]).unwrap(),
            serde_json::to_value(before).unwrap()
        );
        assert!(!updated.remove_failed_pending_model(&receipt).unwrap());
        assert!(updated.has_model("pending-model"));
        assert_eq!(std::fs::read(&fixture.registry).unwrap(), edited);
    }
}

#[test]
fn pending_cleanup_does_not_remove_a_recreated_id_with_identical_known_fields() {
    let fixture = Fixture::new();
    let entry: LocalModelEntry = serde_json::from_value(fixture.record("pending-model")).unwrap();
    let receipt = fixture
        .load()
        .add_pending_model(entry.clone())
        .unwrap()
        .unwrap();
    let first_raw = fixture.raw()["models"][0].clone();
    let first_known = serde_json::to_value(&fixture.load().models[0]).unwrap();
    fixture.load().delete_model("pending-model").unwrap();
    fixture.load().add_model(entry).unwrap();
    let second_raw = fixture.raw()["models"][0].clone();
    let mut recreated = fixture.load();
    assert_eq!(
        serde_json::to_value(&recreated.models[0]).unwrap(),
        first_known
    );
    assert_ne!(
        first_raw["_lumina_registration_id"],
        second_raw["_lumina_registration_id"]
    );
    let committed = std::fs::read(&fixture.registry).unwrap();
    assert!(!recreated.remove_failed_pending_model(&receipt).unwrap());
    assert_eq!(std::fs::read(&fixture.registry).unwrap(), committed);
}

#[test]
fn pending_cleanup_retains_completed_files_and_uncommitted_user_settings() {
    let fixture = Fixture::new();
    let entry: LocalModelEntry = serde_json::from_value(fixture.record("pending-model")).unwrap();
    let receipt = fixture.load().add_pending_model(entry).unwrap().unwrap();
    let mut edited = fixture.load();
    edited.models[0].settings.context_size = Some(32768);
    assert!(!edited.remove_failed_pending_model(&receipt).unwrap());
    assert!(fixture.load().has_model("pending-model"));
    std::fs::write(fixture.data.join("original.gguf"), b"completed download").unwrap();
    assert!(!fixture
        .load()
        .remove_failed_pending_model(&receipt)
        .unwrap());
    assert_eq!(
        std::fs::read(fixture.data.join("original.gguf")).unwrap(),
        b"completed download"
    );
    assert!(fixture.load().has_model("pending-model"));
}

#[test]
#[ignore = "subprocess helper for legacy registry locking and CAS tests"]
fn legacy_registry_child() {
    let data = PathBuf::from(std::env::var_os("LUMINA_LEGACY_TEST_ROOT").expect("child data root"));
    let action = std::env::var("LUMINA_LEGACY_TEST_ACTION").unwrap();
    let name = std::env::var("LUMINA_LEGACY_TEST_NAME").unwrap();
    if action == "health" {
        use lumina_local_inference::{local_model_registry::get_registry, LocalInferenceProvider};
        use lumina_provider_types::base::ProviderDescriptor;
        assert!(get_registry()
            .lock()
            .unwrap_err()
            .to_string()
            .contains("needs repair"));
        let metadata = LocalInferenceProvider::metadata();
        assert!(metadata.description.contains("needs repair"));
        assert!(metadata
            .model_selection_hint
            .unwrap()
            .contains("needs repair"));
        std::fs::write(data.join(format!("{name}.result")), b"diagnosed").unwrap();
        return;
    }
    if action == "read" {
        std::fs::write(data.join(format!("{name}.ready")), b"ready").unwrap();
        let registry = LocalModelRegistry::load_from_data_dir(&data).unwrap();
        assert!(registry.has_model("old-model"));
        std::fs::write(data.join(format!("{name}.result")), b"read").unwrap();
        return;
    }
    let mut registry = LocalModelRegistry::load_from_data_dir(&data).unwrap();
    std::fs::write(data.join(format!("{name}.ready")), b"ready").unwrap();
    wait_for(&data.join("continue"));
    let mut settings = registry.get_model_settings("old-model").unwrap().clone();
    settings.context_size = Some(if name == "first" { 1024 } else { 2048 });
    let result = match registry.update_model_settings("old-model", settings) {
        Ok(()) => "saved".to_string(),
        Err(error) => format!("{error:#}"),
    };
    std::fs::write(data.join(format!("{name}.result")), result).unwrap();
}
