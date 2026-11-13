use anyhow::{ensure, Context, Result};
use lumina::media::{
    execute_prepared_task, prepare_reused_task, ImportedTaskOutput, MediaAssociation,
    PreparedTaskExecution, TaskReuseRequest,
};
use lumina::providers::local_inference::lifecycle::{
    InstallationApplication, NativeTaskCatalog, NativeTaskEngineRegistry, NativeTaskIntent,
    TaskParameterIdentity,
};
use lumina::session::SessionManager;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{fs::File, io::Read, path::Path};

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum TaskIntent {
    TextToImage,
    TextToVideo,
    TextToVideoWithAudio,
    FirstFrameToVideoWithAudio,
    SpeechSynthesis,
    Transcription,
}

pub(crate) async fn list_tasks(app: &InstallationApplication, intent: TaskIntent) -> Result<()> {
    let service = app.service().await?;
    service.wait_ready().await?;
    let intent = match intent {
        TaskIntent::TextToImage => NativeTaskIntent::TextToImage,
        TaskIntent::TextToVideo => NativeTaskIntent::TextToVideo,
        TaskIntent::TextToVideoWithAudio => NativeTaskIntent::TextToVideoWithAudio,
        TaskIntent::FirstFrameToVideoWithAudio => NativeTaskIntent::FirstFrameToVideoWithAudio,
        TaskIntent::SpeechSynthesis => NativeTaskIntent::SpeechSynthesis,
        TaskIntent::Transcription => NativeTaskIntent::Transcription,
    };
    println!(
        "{}",
        serde_json::to_string(&NativeTaskCatalog::active(service.repository())?.list(intent))?
    );
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTaskRequest {
    identity: TaskParameterIdentity,
    request: Value,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TaskRequest {
    New(NewTaskRequest),
    Reuse(TaskReuseRequest),
}

pub(crate) async fn run_task(
    app: &InstallationApplication,
    session_id: &str,
    message_id: &str,
    engine: &Path,
    request: &Path,
) -> Result<()> {
    let manager = SessionManager::instance();
    let session = manager.get_session(session_id, true).await?;
    ensure!(
        session.conversation.as_ref().is_some_and(|c| c
            .messages()
            .iter()
            .any(|m| m.id.as_deref() == Some(message_id))),
        "Message does not exist in the selected session"
    );
    let mut bytes = Vec::new();
    File::open(request)?.take(262145).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 262144, "Task request exceeds 256 KiB");
    let request: TaskRequest = serde_json::from_slice(&bytes)?;
    let service = app.service().await?;
    service.wait_ready().await?;
    let plan = match request {
        TaskRequest::New(request) => {
            let catalog = NativeTaskCatalog::for_version(
                service.repository(),
                &request.identity.installation_id,
                &request.identity.version_id,
            )?;
            catalog
                .prepare(&request.identity, request.request)
                .context("Invalid model-specific request")?
        }
        TaskRequest::Reuse(request) => {
            prepare_reused_task(&service, manager.storage(), session_id, request).await?
        }
    };
    ensure!(
        plan.session_id() == session_id,
        "Task belongs to another session"
    );
    let mut engines = NativeTaskEngineRegistry::default();
    engines.register(plan.runtime(), engine)?;
    let result = execute_prepared_task(
        &service,
        &engines,
        manager.storage(),
        session_id,
        plan,
        MediaAssociation::Message {
            message_id: message_id.into(),
        },
    )
    .await?;
    let (value, completed) = match result {
        PreparedTaskExecution::Completed(ImportedTaskOutput::Media(media)) => {
            (json!({"status":"completed","media":media}), true)
        }
        PreparedTaskExecution::Completed(ImportedTaskOutput::Transcript(t)) => (
            json!({"status":"completed","transcript":t.text,"jobId":t.job_id,"sessionId":t.session_id,"sha256":t.sha256,"generationConfigSha256":t.generation_config_sha256}),
            true,
        ),
        PreparedTaskExecution::NeedsProfiling(reason) => {
            (json!({"status":"needs_profiling","reason":reason}), false)
        }
        PreparedTaskExecution::Unavailable(reason) => {
            (json!({"status":"unavailable","reason":reason}), false)
        }
    };
    println!("{}", serde_json::to_string(&value)?);
    ensure!(completed, "Task did not run; see the resource assessment");
    Ok(())
}
