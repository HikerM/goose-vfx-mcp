use super::*;
use crate::config::LuminaMode;
use crate::project::ProjectService;
use crate::session::{SessionManager, SessionType};
use rmcp::model::RawContent;

const FIRST: &str = "12345678-1234-4234-8234-123456789abc";
const SECOND: &str = "22345678-1234-4234-8234-123456789abc";
const MISSING: &str = "32345678-1234-4234-8234-123456789abc";

fn response(result: &CallToolResult) -> Value {
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let RawContent::Text(text) = &result.content[0].raw else {
        panic!("Expected text")
    };
    serde_json::from_str(&text.text).unwrap()
}

#[test]
fn project_asset_tools_are_read_only_closed_schemas_with_bounded_pagination() {
    let tools = tools();
    for name in ["project_assets", "project_asset"] {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap();
        assert_eq!(tool.input_schema["additionalProperties"], json!(false));
        let wire = serde_json::to_value(tool).unwrap();
        assert_eq!(wire["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(wire["annotations"]["destructiveHint"], json!(false));
        assert!(!tool.input_schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("projectId"));
        assert!(!tool.input_schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("sessionId"));
    }
    let list = tools
        .iter()
        .find(|tool| tool.name == "project_assets")
        .unwrap();
    assert_eq!(
        list.input_schema["properties"]["limit"],
        json!({"type":"integer","minimum":1,"maximum":100,"default":20})
    );
    assert_eq!(
        serde_json::from_value::<ProjectAssets>(json!({}))
            .unwrap()
            .limit,
        20
    );
    for field in ["sessionId", "projectId", "path"] {
        let mut list = json!({});
        list[field] = json!("injected");
        assert!(serde_json::from_value::<ProjectAssets>(list).is_err());
        let mut read = json!({"assetId":FIRST});
        read[field] = json!("injected");
        assert!(serde_json::from_value::<ProjectAsset>(read).is_err());
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    manager: Arc<SessionManager>,
    client: NativeTasksClient,
    source: String,
    same_project: String,
    other_project: String,
}

impl Fixture {
    async fn new() -> Self {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/lifecycle-validation/native-task-project-assets-tests");
        std::fs::create_dir_all(&base).unwrap();
        let directory = tempfile::tempdir_in(base.canonicalize().unwrap()).unwrap();
        let manager = Arc::new(SessionManager::new(directory.path().join("storage")));
        let projects = ProjectService::new(manager.storage().clone());
        let mut sessions = Vec::new();
        for (name, root) in [("source", "one"), ("reader", "one"), ("other", "two")] {
            let path = directory.path().join(root);
            std::fs::create_dir_all(&path).unwrap();
            let project = projects.open_project(&path, Some(root)).await.unwrap();
            let session = manager
                .create_session(path, name.into(), SessionType::User, LuminaMode::Auto)
                .await
                .unwrap();
            manager
                .update(&session.id)
                .project_id(Some(project.id))
                .apply()
                .await
                .unwrap();
            sessions.push(session.id);
        }
        let pool = manager.storage().pool().await.unwrap().clone();
        let assets = ProjectAssetStore::new(pool.clone());
        assets.initialize().await.unwrap();
        for (index, id) in [FIRST, SECOND].into_iter().enumerate() {
            let job = format!("asset-job-{index}");
            assets.freeze_job(&sessions[0], &job, None).await.unwrap();
            let mut tx = pool.begin().await.unwrap();
            ProjectAssetStore::record_in_tx(&mut tx, &sessions[0], &job, None, id, &json!({
                "kind":"media",
                "result":{"resultId":id,"sessionId":sessions[0],"kind":"audio","mime":"audio/wav","fileName":"music.wav","bytes":705644,"sha256":"a".repeat(64),"spec":{"kind":"audio","sampleRate":44100,"channels":2,"sampleFrames":176400}},
                "configuration":{"canonicalSnapshot":"private full parameter snapshot"}
            })).await.unwrap();
            tx.commit().await.unwrap();
        }
        let client = NativeTasksClient::new(PlatformExtensionContext {
            extension_manager: None,
            session_manager: manager.clone(),
            session: None,
            use_login_shell_path: false,
        });
        Self {
            _directory: directory,
            manager,
            client,
            source: sessions[0].clone(),
            same_project: sessions[1].clone(),
            other_project: sessions[2].clone(),
        }
    }

    async fn call(&self, session: &str, name: &str, arguments: Value) -> CallToolResult {
        let ctx = ToolCallContext::new(session.into(), None, Some("asset-query".into()));
        self.client
            .call_tool(
                &ctx,
                name,
                arguments.as_object().cloned(),
                CancellationToken::new(),
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn project_assets_are_discoverable_from_a_new_session_without_leaking_full_page_configuration(
) {
    let f = Fixture::new().await;
    let first = response(
        &f.call(&f.same_project, "project_assets", json!({"limit":1}))
            .await,
    );
    assert_eq!(first["assets"].as_array().unwrap().len(), 1);
    assert_eq!(first["assets"][0]["assetId"], FIRST);
    assert_eq!(first["assets"][0]["sourceSessionId"], f.source);
    assert_eq!(first["assets"][0]["output"]["mime"], "audio/wav");
    assert_eq!(first["nextAfterAssetId"], FIRST);
    assert!(!first.to_string().contains("canonicalSnapshot"));
    assert!(!first
        .to_string()
        .contains("private full parameter snapshot"));
    let rest = response(
        &f.call(
            &f.same_project,
            "project_assets",
            json!({"afterAssetId":FIRST}),
        )
        .await,
    );
    assert_eq!(rest["assets"].as_array().unwrap().len(), 1);
    assert_eq!(rest["assets"][0]["assetId"], SECOND);
    assert!(rest["nextAfterAssetId"].is_null());
    let found = response(
        &f.call(&f.same_project, "project_asset", json!({"assetId":FIRST}))
            .await,
    );
    assert_eq!(found["status"], "found");
    assert_eq!(found["asset"]["sourceSessionId"], f.source);
    assert_eq!(found["projection"], true);
    assert_eq!(
        found["asset"]["configuration"]["canonicalSnapshot"],
        "private full parameter snapshot"
    );
    assert!(found["asset"].get("contentSha256").is_none());
    assert!(found["asset"]["archiveContentSha256"].is_string());
    let cross = response(
        &f.call(&f.other_project, "project_asset", json!({"assetId":FIRST}))
            .await,
    );
    let missing = response(
        &f.call(
            &f.other_project,
            "project_asset",
            json!({"assetId":MISSING}),
        )
        .await,
    );
    assert_eq!(cross, json!({"status":"not_found"}));
    assert_eq!(cross, missing);
    let other = response(&f.call(&f.other_project, "project_assets", json!({})).await);
    assert_eq!(other["assets"], json!([]));
    f.manager
        .storage()
        .native_installations()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn project_asset_query_rejects_scope_injection_and_invalid_page_bounds() {
    let f = Fixture::new().await;
    for args in [
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"limit":-1}),
        json!({"limit":1.5}),
        json!({"limit":null}),
        json!({"afterAssetId":"../secret"}),
        json!({"sessionId":f.other_project}),
        json!({"projectId":"project_other"}),
    ] {
        assert_eq!(
            f.call(&f.same_project, "project_assets", args)
                .await
                .is_error,
            Some(true)
        );
    }
    for limit in [1, 100] {
        assert_ne!(
            f.call(&f.same_project, "project_assets", json!({"limit":limit}))
                .await
                .is_error,
            Some(true)
        );
    }
    for args in [
        json!({"assetId":FIRST,"sessionId":f.source}),
        json!({"assetId":FIRST,"projectId":"project_other"}),
        json!({"assetId":"D:/private.wav"}),
        json!({"assetId":FIRST,"textOffset":1}),
        json!({"assetId":FIRST,"textLimit":1}),
        json!({"assetId":FIRST,"textLimit":0}),
        json!({"assetId":FIRST,"textLimit":16385}),
    ] {
        assert_eq!(
            f.call(&f.same_project, "project_asset", args)
                .await
                .is_error,
            Some(true)
        );
    }
    f.manager
        .storage()
        .native_installations()
        .shutdown()
        .await
        .unwrap();
}

fn transcript_record(text: &str) -> ProjectAssetRecord {
    ProjectAssetRecord {
        asset_id: FIRST.into(),
        project_id: "project_one".into(),
        source_session_id: "source".into(),
        job_id: "job".into(),
        work_item_id: None,
        run_id: None,
        created_at_ms: 1,
        content_sha256: "a".repeat(64),
        content: json!({"kind":"transcript","transcript":{
            "sessionId":"source","jobId":"job","text":text,
            "bytes":text.len(),"sha256":"b".repeat(64)
        },"configuration":{"parameterSchemaId":"schema","identityJson":"{}","snapshotJson":"{\"seed\":42}","snapshotSha256":"c".repeat(64)}}),
    }
}

#[test]
fn transcript_projection_pages_unicode_scalars_and_preserves_archive_identity_and_configuration() {
    let record = transcript_record("A猫😀e\u{301}");
    let page = |offset, limit| {
        project_asset_projection(
            record.clone(),
            &ProjectAsset {
                asset_id: FIRST.into(),
                text_offset: offset,
                text_limit: limit,
            },
        )
    };
    let first = page(1, 2).unwrap();
    assert_eq!(first["projection"], true);
    assert_eq!(first["asset"]["output"]["text"], "猫😀");
    assert_eq!(first["asset"]["output"]["textTotalChars"], 5);
    assert_eq!(first["asset"]["output"]["nextTextOffset"], 3);
    assert_eq!(first["asset"]["output"]["textTruncated"], true);
    assert_eq!(
        first["asset"]["output"]["bytes"],
        record.content["transcript"]["bytes"]
    );
    assert_eq!(
        first["asset"]["archiveContentSha256"],
        record.content_sha256
    );
    assert_eq!(
        first["asset"]["configuration"],
        record.content["configuration"]
    );
    let last = page(3, 2).unwrap();
    assert_eq!(last["asset"]["output"]["text"], "e\u{301}");
    assert!(last["asset"]["output"]["nextTextOffset"].is_null());
    assert_eq!(last["asset"]["output"]["textTruncated"], true);
    assert_eq!(
        page(0, 4096).unwrap()["asset"]["output"]["textTruncated"],
        false
    );
    assert!(page(6, 1).is_err());
    assert!(page(0, 0).is_err());
    assert!(page(0, 16385).is_err());
    let request: ProjectAsset = serde_json::from_value(json!({"assetId":FIRST})).unwrap();
    let long = project_asset_projection(transcript_record(&"猫".repeat(4097)), &request).unwrap();
    assert_eq!(
        long["asset"]["output"]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        4096
    );
    assert_eq!(long["asset"]["output"]["nextTextOffset"], 4096);
    assert_eq!(long["asset"]["output"]["textTruncated"], true);
}
