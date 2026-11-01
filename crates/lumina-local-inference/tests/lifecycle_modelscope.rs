use lumina_local_inference::lifecycle::*;
use serde_json::json;

fn snapshot() -> RepositorySnapshot {
    RepositorySnapshot {
        repository: ModelScopeRepository::parse("test-owner/test-model").unwrap(),
        requested_revision: "master".to_string(),
        resolved_revision: "a".repeat(40),
        files_digest: Sha256Digest::of_bytes(b"unpopulated"),
        fetched_at: 100,
    }
}

fn payload(files: serde_json::Value) -> serde_json::Value {
    json!({ "Code": 200, "Data": {"Files": files} })
}

fn file(path: &str) -> serde_json::Value {
    json!({"Path": path, "Type": "blob", "Size": 100, "Sha256": "b".repeat(64), "Mode": "33188"})
}

fn parse(payload: serde_json::Value) -> anyhow::Result<RepositoryInventory> {
    parse_modelscope_inventory(&serde_json::to_vec(&payload).unwrap(), snapshot())
}

#[test]
fn pinned_inventory_is_canonical_and_downloads_remain_pinned() {
    let first = parse(payload(json!([file("q4/model.gguf"), file("config.json")]))).unwrap();
    let reordered = parse(payload(json!([file("config.json"), file("q4/model.gguf")]))).unwrap();
    assert_eq!(first.snapshot.files_digest, reordered.snapshot.files_digest);
    let url = first
        .download_url(&RepositoryPath::parse("q4/model.gguf").unwrap())
        .unwrap();
    assert_eq!(url.host_str(), Some("modelscope.cn"));
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["Revision"], "a".repeat(40));
    assert_eq!(query["FilePath"], "q4/model.gguf");
    assert!(first
        .download_url(&RepositoryPath::parse("absent.gguf").unwrap())
        .is_err());
    let mut corrupt = first;
    corrupt.files.get_mut("q4/model.gguf").unwrap().size_bytes = Some(101);
    assert!(corrupt.validate().is_err());
}

#[test]
fn refuses_mutable_revision_and_incomplete_or_malformed_inventories() {
    let mut mutable = snapshot();
    mutable.resolved_revision = "master".to_string();
    assert!(
        parse_modelscope_inventory(&serde_json::to_vec(&payload(json!([]))).unwrap(), mutable)
            .is_err()
    );
    for bad in [
        json!({"Code": 200}),
        json!({"Code": 200, "Data": {}}),
        json!({"Code": 200, "Data": {"Files": [], "HasMore": true}}),
        json!({"Code": 200, "Data": {"Files": [], "NextCursor": "page2"}}),
        json!({"Code": 200, "Data": {"Files": [], "NextPageToken": "page2"}}),
        json!({"Code": 200, "Data": {"Files": [], "TotalCount": 10}}),
    ] {
        let error = parse(bad).unwrap_err();
        assert_eq!(
            error.downcast_ref::<ModelSourceError>().unwrap().code,
            LifecycleErrorCode::SourcePartialFailure
        );
    }
    let capped = (0..3000)
        .map(|index| file(&format!("weights-{index}.gguf")))
        .collect::<Vec<_>>();
    let error = parse(payload(json!(capped))).unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::SourcePartialFailure
    );
}

#[test]
fn preserves_unknown_sizes_hashes_and_real_empty_repository() {
    let inventory = parse(payload(json!([
        {"Path":"folder", "Type":"tree"},
        {"Path":"folder/model.gguf", "Type":"blob"}
    ])))
    .unwrap();
    assert_eq!(inventory.files.len(), 1);
    let file = &inventory.files["folder/model.gguf"];
    assert_eq!(file.size_bytes, None);
    assert_eq!(file.sha256, None);
    assert!(parse(payload(json!([]))).unwrap().files.is_empty());
}

#[test]
fn refuses_invalid_git_paths_duplicate_entries_symlinks_and_invalid_hashes() {
    for bad_files in [
        json!([file("../model.gguf")]),
        json!([file("/model.gguf")]),
        json!([file("folder/./model.gguf")]),
        json!([file("folder//model.gguf")]),
        json!([file("model\0.gguf")]),
        json!([file("model.gguf"), file("model.gguf")]),
        json!([{"Path":"model.gguf", "Type":"commit"}]),
        json!([{"Path":"model.gguf", "Type":"blob", "Mode":"40960"}]),
        json!([{"Path":"model.gguf", "Type":"blob", "Sha256":"bad"}]),
    ] {
        assert!(parse(payload(bad_files)).is_err());
    }
}

#[test]
fn remote_git_names_are_preserved_without_becoming_install_paths() {
    let paths = [
        "samples\\ComfyUI_00001_.webp",
        "NUL.gguf",
        "C:relative.gguf",
        "weights.gguf:stream",
        "trailing./model.gguf",
        "control\npreview.png",
    ];
    let mut entries = paths.iter().map(|path| file(path)).collect::<Vec<_>>();
    entries.push(file("main.gguf"));
    let inventory = parse(payload(json!(entries))).unwrap();
    assert_eq!(inventory.files.len(), paths.len() + 1);
    assert_eq!(inventory.path_diagnostics().len(), paths.len());
    for path in paths {
        let observed = &inventory.files[path].path;
        assert_eq!(observed.as_str(), path);
        assert!(observed.materializable_path().is_err());
        assert!(RepositoryPath::parse(path).is_err());
    }
    assert!(!inventory.files.contains_key("samples/ComfyUI_00001_.webp"));
    assert!(inventory
        .download_url(&RepositoryPath::parse("main.gguf").unwrap())
        .is_ok());
    let mut modified = inventory.clone();
    modified
        .files
        .get_mut("samples\\ComfyUI_00001_.webp")
        .unwrap()
        .size_bytes = Some(101);
    assert!(modified.validate().is_err());
    let mut changed_entries = entries;
    changed_entries[0]["Size"] = json!(101);
    let changed = parse(payload(json!(changed_entries))).unwrap();
    assert_ne!(
        inventory.snapshot.files_digest,
        changed.snapshot.files_digest
    );
    let round_trip: RepositoryInventory =
        serde_json::from_slice(&serde_json::to_vec(&inventory).unwrap()).unwrap();
    assert_eq!(round_trip, inventory);
    round_trip.validate().unwrap();
}

#[test]
fn case_distinct_git_files_keep_their_exact_source_identity() {
    let inventory = parse(payload(json!([file("model.gguf"), file("MODEL.gguf")]))).unwrap();
    assert_eq!(inventory.files.len(), 2);
    assert!(inventory.path_diagnostics().is_empty());
    for path in ["model.gguf", "MODEL.gguf"] {
        let url = inventory
            .download_url(&RepositoryPath::parse(path).unwrap())
            .unwrap();
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "FilePath")
                .unwrap()
                .1,
            path
        );
    }
}

#[test]
fn remote_names_have_protocol_bounds_and_selected_names_remain_strict() {
    for path in ["", "/model.gguf", "../x", "a/../x", "a/./x", "a//x", "x\0y"] {
        assert!(ObservedRepositoryPath::parse(path).is_err());
    }
    assert!(ObservedRepositoryPath::parse("x".repeat(16 * 1024 + 1)).is_err());
    for path in [
        "folder\\..\\outside.gguf",
        "C:/model.gguf",
        "CON/model.gguf",
        "model.gguf ",
    ] {
        assert!(ObservedRepositoryPath::parse(path)
            .unwrap()
            .materializable_path()
            .is_err());
    }
}

#[test]
fn observed_artifact_ids_bind_exact_source_paths_and_ignore_fetch_time() {
    let mut inventory = parse(payload(json!([file("model.gguf"), file("MODEL.gguf")]))).unwrap();
    let path = RepositoryPath::parse("model.gguf").unwrap();
    let artifact = inventory.artifact(&path, FilePurpose::GgufWeight).unwrap();
    inventory.snapshot.fetched_at += 100;
    inventory.snapshot.requested_revision = "refs/heads/master".into();
    assert_eq!(
        artifact,
        inventory.artifact(&path, FilePurpose::GgufWeight).unwrap()
    );
    let other_case = inventory
        .artifact(
            &RepositoryPath::parse("MODEL.gguf").unwrap(),
            FilePurpose::GgufWeight,
        )
        .unwrap();
    assert_ne!(artifact.id, other_case.id);
    assert_ne!(
        installed_artifact_path(&artifact.id, artifact.purpose, &artifact.path).unwrap(),
        installed_artifact_path(&other_case.id, other_case.purpose, &other_case.path).unwrap()
    );
    inventory.snapshot.resolved_revision = "c".repeat(40);
    assert_ne!(
        artifact.id,
        inventory
            .artifact(&path, FilePurpose::GgufWeight)
            .unwrap()
            .id
    );
}

#[test]
fn observed_artifact_factory_rejects_absent_files_and_wrong_purposes() {
    let inventory = parse(payload(json!([file("main.gguf"), file("config.json")]))).unwrap();
    assert!(inventory
        .artifact(
            &RepositoryPath::parse("missing.gguf").unwrap(),
            FilePurpose::GgufWeight
        )
        .is_err());
    assert!(inventory
        .artifact(
            &RepositoryPath::parse("config.json").unwrap(),
            FilePurpose::GgufWeight
        )
        .is_err());
    assert!(inventory
        .artifact(
            &RepositoryPath::parse("main.gguf").unwrap(),
            FilePurpose::RuntimeConfig
        )
        .is_err());
    let config = inventory
        .artifact(
            &RepositoryPath::parse("config.json").unwrap(),
            FilePurpose::RuntimeConfig,
        )
        .unwrap();
    assert_eq!(config.size_bytes, Some(100));
}

#[test]
fn source_errors_are_typed_and_never_echo_remote_messages() {
    for (status, code) in [
        (401, LifecycleErrorCode::SourceAuthRequired),
        (403, LifecycleErrorCode::SourceForbidden),
        (404, LifecycleErrorCode::SourceNotFound),
        (408, LifecycleErrorCode::SourceTimeout),
        (429, LifecycleErrorCode::SourceRateLimited),
        (503, LifecycleErrorCode::SourceUnavailable),
    ] {
        let error = parse(json!({"Code":status, "Message":"credential-that-must-not-be-echoed"}))
            .unwrap_err();
        assert_eq!(error.downcast_ref::<ModelSourceError>().unwrap().code, code);
        assert!(!format!("{error:?}").contains("credential-that-must-not-be-echoed"));
    }
}

#[tokio::test]
#[ignore = "requires access to the actual ModelScope service; persists pinned inventory evidence"]
async fn real_modelscope_revision_inventory() {
    let repository = ModelScopeRepository::parse("Qwen/Qwen2.5-0.5B-Instruct-GGUF").unwrap();
    let inventory = ModelScopeProvider::new()
        .unwrap()
        .inspect_at(
            repository,
            "master".to_string(),
            "4537f71740dd2c56a15ffa12e5fbb9f15095f20c".to_string(),
            chrono::Utc::now().timestamp(),
            None,
        )
        .await
        .unwrap();
    let file = &inventory.files["qwen2.5-0.5b-instruct-q4_0.gguf"];
    assert_eq!(file.size_bytes, Some(428_730_208));
    assert_eq!(
        file.sha256.as_ref().unwrap().as_str(),
        "7671c0c304e6ce5a7fc577bcb12aba01e2c155cc2efd29b2213c95b18edaf6ed"
    );
    let root = std::path::PathBuf::from(std::env::var("LUMINA_PATH_ROOT").unwrap());
    assert!(root.is_absolute());
    std::fs::create_dir_all(root.join("evidence")).unwrap();
    std::fs::write(
        root.join("evidence/modelscope-inventory.json"),
        serde_json::to_vec_pretty(&inventory).unwrap(),
    )
    .unwrap();
    println!(
        "pinned files={}; inventory digest={}",
        inventory.files.len(),
        inventory.snapshot.files_digest.as_str()
    );
}
