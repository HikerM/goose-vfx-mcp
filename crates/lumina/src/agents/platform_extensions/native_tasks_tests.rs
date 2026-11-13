use super::*;
use crate::config::LuminaMode;
use crate::media::NativeTaskTranscript;
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
        ["list", "run", "reuse"]
    );
    let listed = client
        .call_tool(&ctx, "list", None, CancellationToken::new())
        .await
        .unwrap();
    assert_ne!(listed.is_error, Some(true));
    assert_eq!(
        serde_json::from_str::<Value>(text(&listed, 0)).unwrap(),
        json!({"tasks":[]})
    );
    for (name, args) in [
        ("list", json!({"sessionId":"injected"})),
        (
            "run",
            json!({"identity":{},"parameters":{},"jobId":"attacker"}),
        ),
        ("reuse", json!({"snapshot":{"text":"untrusted"}})),
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
    let missing = ToolCallContext::new(ctx.session_id.clone(), None, None);
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
    let result = format_result(
        "asr-job",
        PreparedTaskExecution::Completed(ImportedTaskOutput::Transcript(NativeTaskTranscript {
            job_id: "asr-job".into(),
            session_id: "session".into(),
            association: MediaAssociation::ToolCall {
                tool_call_id: "call".into(),
            },
            generation_config_sha256: "0".repeat(64),
            bytes: 8,
            sha256: Sha256Digest::of_bytes(b"Speech.\n").as_str().into(),
            text: "Speech.\n".into(),
        })),
    )
    .unwrap();
    assert_eq!(text(&result, 1), "Speech.\n");
    assert_eq!(
        serde_json::from_str::<Value>(text(&result, 0)).unwrap()["jobId"],
        "asr-job"
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
