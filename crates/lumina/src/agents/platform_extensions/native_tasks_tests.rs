use super::*;
use crate::config::LuminaMode;
use crate::media::NativeTaskTranscript;
use crate::project::{ProjectAssetStore, ProjectService};
use crate::session::{SessionManager, SessionType};
use lumina_providers::local_inference::lifecycle::{Sha256Digest, TaskResourceRejection};
use rmcp::model::RawContent;
use std::path::PathBuf;

fn text(result: &CallToolResult, index: usize) -> &str {
    match &result.content[index].raw {
        RawContent::Text(content) => &content.text,
        _ => panic!("expected text"),
    }
}

fn candidate_summary(
    asset_id: &str,
    created_at_ms: i64,
    kind: &str,
    file_name: Option<&str>,
) -> ProjectAssetSummary {
    ProjectAssetSummary {
        asset_id: asset_id.into(),
        project_id: "project-1".into(),
        source_session_id: "session-1".into(),
        job_id: "job-1".into(),
        work_item_id: None,
        run_id: None,
        created_at_ms,
        content_sha256: "a".repeat(64),
        kind: "media".into(),
        output: json!({"spec":{"kind":kind},"fileName":file_name}),
    }
}

#[test]
fn project_asset_candidates_apply_bounded_metadata_filters() {
    let image = candidate_summary("image", 100, "image", Some("Cat Portrait.PNG"));
    let audio = candidate_summary("audio", 200, "audio", Some("voice.wav"));
    let transcript = ProjectAssetSummary {
        kind: "transcript".into(),
        output: json!({"text":"cat"}),
        ..candidate_summary("transcript", 300, "transcript", None)
    };
    let query = ProjectAssetCandidates {
        after_asset_id: None,
        kind: Some("image".into()),
        created_before_ms: Some(200),
        created_after_ms: Some(50),
        file_name_contains: Some("cat".into()),
        limit: 10,
    };
    let needle = validate_candidate_query(&query).unwrap();
    assert!(candidate_matches(&image, &query, needle.as_deref()));
    assert!(!candidate_matches(&audio, &query, needle.as_deref()));
    assert!(!candidate_matches(&transcript, &query, needle.as_deref()));

    for invalid in [
        query_defaults(),
        ProjectAssetCandidates {
            kind: Some("unknown".into()),
            ..query_defaults()
        },
        ProjectAssetCandidates {
            created_after_ms: Some(10),
            created_before_ms: Some(10),
            ..query_defaults()
        },
        ProjectAssetCandidates {
            file_name_contains: Some("\n".into()),
            ..query_defaults()
        },
    ] {
        assert!(validate_candidate_query(&invalid).is_err());
    }
}

#[test]
fn edit_scope_hint_accepts_only_canonical_bounded_ranges() {
    assert_eq!(
        parse_edit_scope_hint("image_rect x=10 y=20 width=30 height=40").unwrap(),
        ProjectEditScope::ImageRect {
            x: 10,
            y: 20,
            width: 30,
            height: 40,
        }
    );
    assert_eq!(
        parse_edit_scope_hint("time_range startMicros=1000 durationMicros=2000").unwrap(),
        ProjectEditScope::TimeRange {
            start_micros: 1000,
            duration_micros: 2000,
        }
    );
    assert_eq!(
        parse_edit_scope_hint("time_range start_micros=1000 duration_micros=2000").unwrap(),
        ProjectEditScope::TimeRange {
            start_micros: 1000,
            duration_micros: 2000,
        }
    );
}

#[test]
fn edit_scope_hint_rejects_ambiguous_or_unsafe_language() {
    for hint in [
        "只修改前面一段",
        "image_rect x=10 y=20 width=30",
        "image_rect x=10 y=20 width=30 height=40 x=11",
        "time_range startMicros=-1 durationMicros=2000",
        "image_rect x=10 y=20 width=30 height=40 extra=1",
        "video startMicros=1000 durationMicros=2000",
    ] {
        assert!(parse_edit_scope_hint(hint).is_err(), "accepted: {hint}");
    }
}

fn query_defaults() -> ProjectAssetCandidates {
    ProjectAssetCandidates {
        after_asset_id: None,
        kind: None,
        created_before_ms: None,
        created_after_ms: None,
        file_name_contains: None,
        limit: 20,
    }
}

#[tokio::test]
async fn project_asset_candidates_query_real_project_assets() -> Result<()> {
    let root = tempfile::tempdir()?;
    let manager = Arc::new(SessionManager::new(root.path().join("data")));
    let project_path = root.path().join("project");
    std::fs::create_dir_all(&project_path)?;
    let project = ProjectService::new(Arc::clone(manager.storage()))
        .open_project(&project_path, Some("candidate test"))
        .await?;
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let session = manager
        .create_session(
            workspace,
            "candidate test".into(),
            SessionType::Hidden,
            LuminaMode::Auto,
        )
        .await?;
    manager
        .update(&session.id)
        .project_id(Some(project.id))
        .apply()
        .await?;
    let pool = manager.storage().pool().await?.clone();
    let assets = ProjectAssetStore::new(pool.clone());
    assets.initialize().await?;
    assets
        .freeze_job(&session.id, "candidate-job", None)
        .await?;

    let records = [
        (
            "00000000-0000-4000-8000-000000000101",
            json!({
                "kind":"media",
                "result":{
                    "resultId":"00000000-0000-4000-8000-000000000101",
                    "mime":"image/png",
                    "fileName":"cat.png",
                    "bytes":48,
                    "sha256":"a".repeat(64),
                    "spec":{"kind":"image","width":1,"height":1}
                },
                "configuration":{}
            }),
        ),
        (
            "00000000-0000-4000-8000-000000000102",
            json!({
                "kind":"media",
                "result":{
                    "resultId":"00000000-0000-4000-8000-000000000102",
                    "mime":"audio/wav",
                    "fileName":"voice-one.wav",
                    "bytes":48,
                    "sha256":"b".repeat(64),
                    "spec":{"kind":"audio","sampleRate":16000,"channels":1,"sampleFrames":2}
                },
                "configuration":{}
            }),
        ),
        (
            "00000000-0000-4000-8000-000000000103",
            json!({
                "kind":"media",
                "result":{
                    "resultId":"00000000-0000-4000-8000-000000000103",
                    "mime":"audio/wav",
                    "fileName":"voice-two.wav",
                    "bytes":48,
                    "sha256":"c".repeat(64),
                    "spec":{"kind":"audio","sampleRate":16000,"channels":1,"sampleFrames":2}
                },
                "configuration":{}
            }),
        ),
    ];
    for (asset_id, content) in &records {
        let mut tx = pool.begin().await?;
        ProjectAssetStore::record_in_tx(
            &mut tx,
            &session.id,
            "candidate-job",
            None,
            asset_id,
            content,
        )
        .await?;
        tx.commit().await?;
    }

    let unique = project_asset_candidates(
        &assets,
        &session.id,
        ProjectAssetCandidates {
            kind: Some("image".into()),
            file_name_contains: Some("cat".into()),
            ..query_defaults()
        },
    )
    .await?;
    assert_eq!(unique["status"], "unique");
    assert_eq!(unique["assets"][0]["assetId"], records[0].0);
    assert_eq!(unique["assets"][0]["output"]["spec"]["kind"], "image");

    let ambiguous = project_asset_candidates(
        &assets,
        &session.id,
        ProjectAssetCandidates {
            kind: Some("audio".into()),
            limit: 1,
            ..query_defaults()
        },
    )
    .await?;
    assert_eq!(ambiguous["status"], "ambiguous");
    assert_eq!(ambiguous["truncated"], true);
    assert_eq!(ambiguous["assets"].as_array().unwrap().len(), 1);

    let missing = project_asset_candidates(
        &assets,
        &session.id,
        ProjectAssetCandidates {
            kind: Some("video".into()),
            ..query_defaults()
        },
    )
    .await?;
    assert_eq!(missing["status"], "not_found");

    let source_session_id = session.id.clone();
    let client = NativeTasksClient::new(PlatformExtensionContext {
        extension_manager: None,
        session_manager: Arc::clone(&manager),
        session: Some(Arc::new(session)),
        use_login_shell_path: false,
    });
    let context = ToolCallContext::new(source_session_id, None, None);
    let via_tool = client
        .call_tool(
            &context,
            "project_asset_candidates",
            Some(json!({"kind":"image"}).as_object().unwrap().clone()),
            CancellationToken::new(),
        )
        .await?;
    assert_ne!(via_tool.is_error, Some(true));
    assert_eq!(
        serde_json::from_str::<Value>(text(&via_tool, 0))?["status"],
        "unique"
    );
    Ok(())
}

#[tokio::test]
async fn registered_extension_lists_real_empty_catalog_and_rejects_injected_scope() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/native-task-tool-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = tempfile::tempdir_in(dir.canonicalize().unwrap()).unwrap();
    let manager = Arc::new(SessionManager::new(tmp.path().into()));
    let session = manager
        .create_session(
            tmp.path().into(),
            "local models".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await
        .unwrap();
    let ctx = ToolCallContext::new(session.id.clone(), None, Some("call-1".into()));
    let def = &crate::agents::platform_extensions::PLATFORM_EXTENSIONS[EXTENSION_NAME];
    assert!(def.default_enabled && !def.hidden && !def.unprefixed_tools);
    let client = (def.client_factory)(PlatformExtensionContext {
        extension_manager: None,
        session_manager: manager.clone(),
        session: Some(Arc::new(session)),
        use_login_shell_path: false,
    });
    let seen = client
        .list_tools(&ctx.session_id, None, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        seen.tools
            .iter()
            .map(|t| t.name.as_ref())
            .collect::<Vec<_>>(),
        [
            "list",
            "status",
            "run",
            "reuse",
            "project_reuse",
            "project_assets",
            "project_asset_candidates",
            "project_asset"
        ]
    );
    let project_reuse = seen
        .tools
        .iter()
        .find(|tool| tool.name == "project_reuse")
        .expect("project_reuse tool is listed");
    let project_reuse_description = project_reuse
        .description
        .as_deref()
        .expect("project_reuse tool has a description");
    assert!(project_reuse_description.contains("single-stream AVI"));
    assert!(project_reuse_description.contains("MP4/WebM"));
    assert!(project_reuse_description.contains("editScopeHint"));
    let listed = client
        .call_tool(&ctx, "list", None, CancellationToken::new())
        .await
        .unwrap();
    assert_ne!(listed.is_error, Some(true));
    assert_eq!(
        serde_json::from_str::<Value>(text(&listed, 0)).unwrap(),
        json!({"tasks":[]})
    );
    let missing = ToolCallContext::new(ctx.session_id.clone(), None, None);
    let status = client
        .call_tool(
            &ctx,
            "status",
            Some(
                json!({"jobId":"native:missing"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(status.is_error, Some(true));
    assert_eq!(
        serde_json::from_str::<Value>(text(&status, 0)).unwrap(),
        json!({"jobId":"native:missing","status":"not_found"})
    );
    let status_without_call = client
        .call_tool(
            &missing,
            "status",
            Some(
                json!({"jobId":"native:missing"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(status_without_call.is_error, Some(true));
    let candidates_without_call = client
        .call_tool(
            &missing,
            "project_asset_candidates",
            Some(json!({"kind":"image"}).as_object().unwrap().clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_ne!(candidates_without_call.is_error, Some(true));
    assert_eq!(
        serde_json::from_str::<Value>(text(&candidates_without_call, 0)).unwrap()["status"],
        "not_found"
    );
    for (name, args) in [
        ("list", json!({"sessionId":"injected"})),
        (
            "run",
            json!({"identity":{},"parameters":{},"jobId":"attacker"}),
        ),
        ("reuse", json!({"snapshot":{"text":"untrusted"}})),
        (
            "project_reuse",
            json!({"snapshot":{"text":"untrusted"},"sourceSessionId":"injected"}),
        ),
    ] {
        let error = client
            .call_tool(
                &ctx,
                name,
                args.as_object().cloned(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(error.is_error, Some(true));
    }
    assert_eq!(
        client
            .call_tool(&missing, "list", None, CancellationToken::new())
            .await
            .unwrap()
            .is_error,
        Some(true)
    );
    let foreign = ToolCallContext::new("other".into(), None, Some("call-2".into()));
    assert_eq!(
        client
            .call_tool(&foreign, "list", None, CancellationToken::new())
            .await
            .unwrap()
            .is_error,
        Some(true)
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let result = client
        .call_tool(&ctx, "run", None, cancelled)
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    assert!(text(&result, 0).contains("cancelled"));
    manager
        .storage()
        .native_installations()
        .shutdown()
        .await
        .unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn cancellation_during_job_claim_waits_for_the_durable_terminal_state() {
    use std::time::Duration;
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/native-task-tool-tests");
    std::fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(base.canonicalize().unwrap()).unwrap();
    let manager = Arc::new(SessionManager::new(tmp.path().into()));
    let session = manager
        .create_session(
            tmp.path().into(),
            "cancelled task".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await
        .unwrap();
    let call = "cancel-while-persisting";
    let job = NativeTaskJobs::job_id(&session.id, call).unwrap();
    let ctx = ToolCallContext::new(session.id.clone(), None, Some(call.into()));
    let client = NativeTasksClient::new(PlatformExtensionContext {
        extension_manager: None,
        session_manager: Arc::clone(&manager),
        session: Some(Arc::new(session)),
        use_login_shell_path: false,
    });
    let pool = manager.storage().pool().await.unwrap();
    let mut writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let args = json!({
        "identity": {
            "installationId":"unknown","versionId":"test","manifestDigest":"test",
            "runtimeDigest":"test","componentBindingDigest":"test","taskVariant":"test","parameterSchemaId":"test"
        },
        "parameters":{}
    });
    let root = manager
        .storage()
        .media_output_directory()
        .join("native-task-locks");
    let lock_file = root.join(
        Sha256Digest::of_serializable(&(&ctx.session_id, &job))
            .unwrap()
            .as_str(),
    );
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let task = tokio::spawn(async move {
        client
            .call_tool(&ctx, "run", args.as_object().cloned(), signal)
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !lock_file.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM native_task_calls_v1")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    assert_eq!(count, 0);
    writer.commit().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        serde_json::from_str::<Value>(text(&result, 0)).unwrap(),
        json!({"status":"cancelled","jobId":job})
    );
    let state: String = sqlx::query_scalar("SELECT state FROM native_task_calls_v1 WHERE job_id=?")
        .bind(&job)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(state, "cancelled");
}

#[test]
fn result_formats_preserve_transcript_and_expose_admission_failure() {
    let transcript = NativeTaskTranscript {
        job_id: "asr-job".into(),
        session_id: "session".into(),
        association: MediaAssociation::ToolCall {
            tool_call_id: "call".into(),
        },
        generation_config_sha256: "0".repeat(64),
        bytes: 8,
        sha256: Sha256Digest::of_bytes(b"Speech.\n").as_str().into(),
        text: "Speech.\n".into(),
    };
    let expected_asset_id = transcript.project_asset_id().unwrap();
    let result = format_result(
        "asr-job",
        PreparedTaskExecution::Completed(ImportedTaskOutput::Transcript(transcript)),
    )
    .unwrap();
    assert_eq!(text(&result, 1), "Speech.\n");
    let status: Value = serde_json::from_str(text(&result, 0)).unwrap();
    assert_eq!(status["jobId"], "asr-job");
    assert_eq!(status["projectAssetId"], expected_asset_id);
    assert_eq!(
        status["projectAssetLookup"],
        json!({"tool":"project_asset","assetId":expected_asset_id})
    );
    let result = format_result(
        "other-job",
        PreparedTaskExecution::Unavailable(TaskResourceRejection {
            code: "insufficient_ram",
            detail: "Measured available memory is too small".into(),
        }),
    )
    .unwrap();
    assert_eq!(result.is_error, Some(true));
    let status: Value = serde_json::from_str(text(&result, 0)).unwrap();
    assert_eq!(status["status"], "unavailable");
    assert_eq!(status["reason"]["code"], "insufficient_ram");
}

#[test]
fn status_payload_preserves_state_and_attaches_recoverable_result_identity() {
    assert_eq!(
        status_payload(
            "native:job",
            "completed",
            Some(json!({"resultIds":["result-1"]})),
        ),
        json!({"jobId":"native:job","status":"completed","resultIds":["result-1"]})
    );
    assert_eq!(
        status_payload(
            "native:asr",
            "completed",
            Some(json!({
                "projectAssetId":"asset-1",
                "projectAssetLookup":{"tool":"project_asset","assetId":"asset-1"}
            })),
        ),
        json!({
            "jobId":"native:asr",
            "status":"completed",
            "projectAssetId":"asset-1",
            "projectAssetLookup":{"tool":"project_asset","assetId":"asset-1"}
        })
    );
    assert_eq!(
        status_payload("native:running", "running", None),
        json!({"jobId":"native:running","status":"running"})
    );
}

#[test]
fn initial_parameters_cannot_override_owner_fields() {
    for key in [
        "sessionId",
        "jobId",
        "identity",
        "runtime",
        "snapshot",
        "path",
    ] {
        let mut params = JsonObject::new();
        params.insert(key.into(), json!("injected"));
        assert!(validate_parameters(&params).is_err());
    }
    let mut params = JsonObject::new();
    params.insert("prompt".into(), json!("an apple"));
    assert!(validate_parameters(&params).is_ok());
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "Requires a managed PocketTTS installation and its external catalog"]
async fn chat_discovers_installed_external_speech_parameters() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let root = PathBuf::from(std::env::var("LUMINA_AUDIO_ACCEPTANCE_DIR")?).canonicalize()?;
    ensure!(
        root.parent() == Some(base.as_path()),
        "Expected an isolated validation installation"
    );
    let profile = std::env::var("LUMINA_AUDIO_ACCEPTANCE_PROFILE")?;
    let catalog_file = std::env::var("LUMINA_AUDIO_CATALOG")?;
    let catalog: Value = serde_json::from_reader(std::fs::File::open(catalog_file)?)?;
    let model = catalog["models"]
        .as_array()
        .context("Missing catalog models")?
        .iter()
        .find(|model| model["name"] == profile)
        .context("Profile missing from external catalog")?;
    let manager = Arc::new(SessionManager::new(root.clone()));
    let session = manager
        .create_session(
            root,
            "Installed speech parameters".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    let ctx = ToolCallContext::new(session.id.clone(), None, Some("discover-speech".into()));
    let client = NativeTasksClient::new(PlatformExtensionContext {
        extension_manager: None,
        session_manager: manager.clone(),
        session: Some(Arc::new(session)),
        use_login_shell_path: false,
    });
    let args = json!({"intent":"speech_synthesis"});
    let result = client
        .call_tool(
            &ctx,
            "list",
            args.as_object().cloned(),
            CancellationToken::new(),
        )
        .await?;
    ensure!(result.is_error != Some(true), "{}", text(&result, 0));
    let response: Value = serde_json::from_str(text(&result, 0))?;
    let expected = format!("acceptance-{profile}");
    let task = response["tasks"]
        .as_array()
        .context("Missing chat tasks")?
        .iter()
        .find(|task| task["identity"]["installationId"] == expected)
        .context("Installed model missing from chat")?;
    let parameters = &task["descriptor"]["parameters"];
    assert_eq!(parameters["binding"]["model"]["sha256"], model["sha256"]);
    assert_eq!(parameters["modelLanguage"], model["parameters"]["language"]);
    let temperature = model["parameters"]["temperature"]
        .as_f64()
        .context("Missing configured temperature")? as f32;
    assert_eq!(
        parameters["fields"]["temperature"]["default"],
        json!(temperature)
    );
    assert_eq!(
        task["identity"]["parameterSchemaId"],
        parameters["identity"]["parameterSchemaId"]
    );
    println!(
        "{}",
        json!({"identity":task["identity"],"language":parameters["modelLanguage"],"temperature":temperature})
    );
    manager.storage().native_installations().shutdown().await?;
    Ok(())
}
