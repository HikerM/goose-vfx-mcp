use super::PlatformExtensionContext;
use crate::agents::mcp_client::{Error, McpClientTrait};
use crate::agents::tool_execution::ToolCallContext;
use crate::media::{
    configured_task_engines, execute_prepared_task_controlled, native_result_link,
    prepare_reused_task, resolve_media_patch, ImportedTaskOutput, MediaAssociation,
    MediaResultStore, NativeTaskClaim, NativeTaskJobs, NativeTaskTerminal, PreparedTaskExecution,
    TaskReuseRequest,
};
use crate::session::session_manager::SessionStorage;
use anyhow::{bail, ensure, Context, Result};
use async_trait::async_trait;
use lumina_providers::local_inference::lifecycle::{
    InstallationService, NativeTaskCatalog, NativeTaskIntent, NativeTaskParameterContract,
    NativeTaskPlan, TaskParameterIdentity,
};
use rmcp::model::{
    CallToolResult, Content, Implementation, InitializeResult, JsonObject, ListToolsResult,
    ServerCapabilities, Tool, ToolAnnotations,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub const EXTENSION_NAME: &str = "native_tasks";
const INTENTS: [NativeTaskIntent; 6] = [
    NativeTaskIntent::TextToImage,
    NativeTaskIntent::TextToVideo,
    NativeTaskIntent::TextToVideoWithAudio,
    NativeTaskIntent::FirstFrameToVideoWithAudio,
    NativeTaskIntent::SpeechSynthesis,
    NativeTaskIntent::Transcription,
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    intent: Option<NativeTaskIntent>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    identity: TaskParameterIdentity,
    parameters: JsonObject,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Reuse {
    identity: TaskParameterIdentity,
    parent_job_id: String,
    parent_result_id: Option<String>,
    patch: JsonObject,
}

enum Request {
    Run(Run),
    Reuse(Reuse),
}

impl Request {
    fn identity(&self) -> &TaskParameterIdentity {
        match self {
            Self::Run(v) => &v.identity,
            Self::Reuse(v) => &v.identity,
        }
    }

    async fn prepare(
        self,
        storage: &SessionStorage,
        session: &str,
        job: &str,
    ) -> Result<(NativeTaskPlan, Arc<InstallationService>)> {
        let installation = storage.native_installations().service().await?;
        installation.wait_ready().await?;
        let plan = match self {
            Self::Run(run) => {
                let media = MediaResultStore::new(storage.pool().await?.clone());
                prepare_first(&installation, &media, session, job, run).await?
            }
            Self::Reuse(reuse) => {
                let request = TaskReuseRequest {
                    identity: reuse.identity,
                    parent_job_id: reuse.parent_job_id,
                    parent_result_id: reuse.parent_result_id,
                    new_job_id: job.into(),
                    patch: reuse.patch,
                };
                prepare_reused_task(&installation, storage, session, request).await?
            }
        };
        Ok((plan, installation))
    }
}

pub struct NativeTasksClient {
    info: InitializeResult,
    context: PlatformExtensionContext,
}

impl NativeTasksClient {
    pub fn new(context: PlatformExtensionContext) -> Self {
        let info=InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(EXTENSION_NAME,"1.0.0").with_title("Local Model Tasks"))
            .with_instructions("Use list to discover an installed task's exact identity and parameter contract. Keep references to run results for subsequent reuse. Speech references, ASR audio and first frames are session media result UUIDs.".to_string());
        Self { info, context }
    }

    async fn dispatch(
        &self,
        ctx: &ToolCallContext,
        name: &str,
        arguments: Option<JsonObject>,
        cancel: CancellationToken,
    ) -> Result<CallToolResult> {
        ensure!(!cancel.is_cancelled(), "Native task call cancelled");
        ensure!(
            self.context
                .session
                .as_ref()
                .is_none_or(|session| session.id == ctx.session_id),
            "Task belongs to another session"
        );
        let call = ctx
            .tool_call_request_id
            .as_deref()
            .filter(|id| {
                !id.trim().is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control)
            })
            .context("Native tasks require a valid session tool call ID")?;
        let session = &ctx.session_id;
        self.context
            .session_manager
            .get_session(session, false)
            .await
            .context("Native task session is unavailable")?;
        let arguments = Value::Object(arguments.unwrap_or_default());
        let storage = self.context.session_manager.storage();
        if name == "list" {
            let list: List = serde_json::from_value(arguments)?;
            let installation = storage.native_installations().service().await?;
            installation.wait_ready().await?;
            let repo = Arc::clone(installation.repository());
            let descriptions = tokio::task::spawn_blocking(move || -> Result<_> {
                let catalog = NativeTaskCatalog::active(&repo)?;
                Ok(catalog.list_matching(list.intent))
            })
            .await
            .context("Native task catalog worker failed")??;
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string(&json!({"tasks":descriptions}))?,
            )]));
        }
        let request = match name {
            "run" => Request::Run(serde_json::from_value(arguments.clone())?),
            "reuse" => Request::Reuse(serde_json::from_value(arguments.clone())?),
            _ => bail!("Unknown native task tool"),
        };
        let job = NativeTaskJobs::job_id(session, call)?;
        let storage = Arc::clone(storage);
        let session = session.to_owned();
        let call = call.to_owned();
        let method = name.to_owned();
        // Disconnect can race the initial SQLite insert. The coordinator must own both
        // that write and the last state change, including any publication already handed off.
        tokio::spawn(async move {
            let jobs = NativeTaskJobs::new(storage.pool().await?.clone());
            let attempt = match jobs
                .claim(
                    &storage.media_output_directory(),
                    &session,
                    &call,
                    &method,
                    &arguments,
                    request.identity(),
                )
                .await?
            {
                NativeTaskClaim::Execute(attempt) => attempt,
                NativeTaskClaim::Replay(output) => {
                    return format_result(&job, PreparedTaskExecution::Completed(output))
                }
                NativeTaskClaim::Busy => return call_status(&job, "busy"),
                NativeTaskClaim::Stopped(terminal) => return call_status(&job, terminal.as_str()),
            };
            let result = async {
                let (plan, installation) = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => bail!("Native task cancelled"),
                    prepared = request.prepare(&storage, &session, &job) => prepared?,
                };
                let engines = configured_task_engines(plan.runtime())?;
                let association = MediaAssociation::ToolCall { tool_call_id: call };
                execute_prepared_task_controlled(
                    &installation,
                    &engines,
                    &storage,
                    &session,
                    plan,
                    association,
                    cancel.clone(),
                )
                .await
            }
            .await;
            let terminal = match &result {
                Ok(PreparedTaskExecution::Completed(_)) => NativeTaskTerminal::Completed,
                Ok(
                    PreparedTaskExecution::NeedsProfiling(_)
                    | PreparedTaskExecution::Unavailable(_),
                ) => NativeTaskTerminal::Deferred,
                Err(_) if cancel.is_cancelled() => NativeTaskTerminal::Cancelled,
                Err(_) => NativeTaskTerminal::Failed,
            };
            attempt.finish(terminal).await?;
            if terminal == NativeTaskTerminal::Cancelled {
                return call_status(&job, terminal.as_str());
            }
            format_result(&job, result?)
        })
        .await
        .context("Native task coordinator failed")?
    }
}

fn call_status(job: &str, state: &str) -> Result<CallToolResult> {
    Ok(CallToolResult::error(vec![Content::text(
        serde_json::to_string(&json!({"jobId":job,"status":state}))?,
    )]))
}

fn validate_parameters(parameters: &JsonObject) -> Result<()> {
    for name in [
        "jobId",
        "sessionId",
        "parameters",
        "identity",
        "runtime",
        "snapshot",
        "path",
    ] {
        ensure!(
            !parameters.contains_key(name),
            "Reserved task parameter: {name}"
        );
    }
    Ok(())
}

async fn prepare_first(
    installation: &InstallationService,
    media: &MediaResultStore,
    session: &str,
    job: &str,
    request: Run,
) -> Result<NativeTaskPlan> {
    let Run {
        identity,
        mut parameters,
    } = request;
    validate_parameters(&parameters)?;
    let repo = Arc::clone(installation.repository());
    let selected = identity.clone();
    let catalog = tokio::task::spawn_blocking(move || {
        NativeTaskCatalog::for_version(&repo, &selected.installation_id, &selected.version_id)
    })
    .await
    .context("Native task version lookup failed")??;
    let desc = catalog.describe(&identity)?;
    resolve_media_patch(media, session, &mut parameters, &desc.descriptor).await?;
    let request = match desc.descriptor {
        NativeTaskParameterContract::Generation(_) => {
            let task = match desc.intent {
                NativeTaskIntent::TextToImage => "image",
                NativeTaskIntent::TextToVideo
                | NativeTaskIntent::TextToVideoWithAudio
                | NativeTaskIntent::FirstFrameToVideoWithAudio => "video",
                _ => bail!("Unsupported installed generation task"),
            };
            json!({"sessionId":session,"jobId":job,"parameters":{"task":task,"parameters":parameters}})
        }
        NativeTaskParameterContract::PocketTts(_) | NativeTaskParameterContract::Qwen3Asr(_) => {
            parameters.insert("sessionId".into(), json!(session));
            parameters.insert("jobId".into(), json!(job));
            Value::Object(parameters)
        }
    };
    catalog.prepare(&identity, request)
}

fn format_result(job: &str, result: PreparedTaskExecution) -> Result<CallToolResult> {
    match result {
        PreparedTaskExecution::NeedsProfiling(detail) => {
            Ok(CallToolResult::error(vec![Content::text(
                serde_json::to_string(
                    &json!({"status":"needs_profiling","jobId":job,"reason":detail}),
                )?,
            )]))
        }
        PreparedTaskExecution::Unavailable(detail) => {
            Ok(CallToolResult::error(vec![Content::text(
                serde_json::to_string(
                    &json!({"status":"unavailable","jobId":job,"reason":detail}),
                )?,
            )]))
        }
        PreparedTaskExecution::Completed(ImportedTaskOutput::Media(results)) => {
            let ids: Vec<_> = results.iter().map(|r| r.result_id.as_str()).collect();
            let mut content = vec![Content::text(serde_json::to_string(
                &json!({"status":"completed","jobId":job,"resultIds":ids}),
            )?)];
            content.extend(results.iter().map(native_result_link));
            Ok(CallToolResult::success(content))
        }
        PreparedTaskExecution::Completed(ImportedTaskOutput::Transcript(transcript)) => {
            Ok(CallToolResult::success(vec![
                Content::text(serde_json::to_string(
                    &json!({"status":"completed","jobId":job,"transcriptSha256":transcript.sha256}),
                )?),
                Content::text(transcript.text),
            ]))
        }
    }
}

fn tools() -> Vec<Tool> {
    let identity_fields = [
        "installationId",
        "versionId",
        "manifestDigest",
        "runtimeDigest",
        "componentBindingDigest",
        "taskVariant",
        "parameterSchemaId",
    ];
    let identity_properties: JsonObject = identity_fields
        .iter()
        .map(|key| (key.to_string(), json!({"type":"string"})))
        .collect();
    let identity = json!({"type":"object","properties":identity_properties,"required":identity_fields,"additionalProperties":false});
    let names = serde_json::to_value(INTENTS).expect("finite task intents");
    let schema = |properties: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        })
        .as_object()
        .expect("tool schema object")
        .clone()
    };
    let run_parameters = json!({
        "type": "object",
        "description": "Fields in this model's installed descriptor. Supply session result UUID strings for its session media reference fields."
    });
    let reuse_fields = json!({
        "identity": identity,
        "parentJobId": {"type":"string","description":"The jobId returned by an earlier completed task in this session."},
        "parentResultId": {"type":"string","format":"uuid"},
        "patch": {"type":"object","minProperties":1,"maxProperties":1,"description":"One model-specific field. Media inputs are session result UUIDs."}
    });
    let definitions = [
        ("list", "List installed local task models, exact identities and supported parameters.",
            schema(json!({"intent":{"type":"string","enum":names}}), &[]), true),
        ("run", "Run an installed local task. Supply its identity and model-specific parameters.",
            schema(json!({"identity":identity,"parameters":run_parameters}), &["identity","parameters"]), false),
        ("reuse", "Create a new task from an earlier job in this session, changing one field. Image and video require a parent result. Speech and transcription use the parent job.",
            schema(reuse_fields,&["identity","parentJobId","patch"]), false),
    ];
    definitions
        .into_iter()
        .map(|(name, description, schema, read_only)| {
            let annotations = ToolAnnotations::from_raw(
                None,
                Some(read_only),
                Some(false),
                Some(read_only),
                Some(false),
            );
            Tool::new(name, description, schema).annotate(annotations)
        })
        .collect()
}

#[async_trait]
impl McpClientTrait for NativeTasksClient {
    async fn close(&self) -> Result<(), Error> {
        Ok(())
    }
    async fn list_tools(
        &self,
        _session_id: &str,
        _next_cursor: Option<String>,
        _cancel: CancellationToken,
    ) -> Result<ListToolsResult, Error> {
        Ok(ListToolsResult {
            tools: tools(),
            next_cursor: None,
            meta: None,
        })
    }
    async fn call_tool(
        &self,
        ctx: &ToolCallContext,
        name: &str,
        arguments: Option<JsonObject>,
        cancel: CancellationToken,
    ) -> Result<CallToolResult, Error> {
        let signal = cancel.child_token();
        let _on_disconnect = signal.clone().drop_guard();
        let dispatch = self.dispatch(ctx, name, arguments, signal);
        let result = if name == "list" {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(anyhow::anyhow!("Native task call cancelled")),
                result = dispatch => result,
            }
        } else {
            dispatch.await
        };
        Ok(result
            .unwrap_or_else(|error| CallToolResult::error(vec![Content::text(error.to_string())])))
    }
    fn get_info(&self) -> Option<&InitializeResult> {
        Some(&self.info)
    }
}

#[cfg(test)]
#[path = "native_tasks_tests.rs"]
mod tests;
