use super::PlatformExtensionContext;
use crate::agents::mcp_client::{Error, McpClientTrait};
use crate::agents::tool_execution::ToolCallContext;
use crate::media::{
    configured_task_engines, execute_prepared_task_controlled,
    execute_project_prepared_task_controlled, native_result_link, prepare_project_reused_task,
    prepare_reused_task, resolve_media_patch, ImportedTaskOutput, MediaAssociation,
    MediaResultStore, NativeTaskClaim, NativeTaskJobs, NativeTaskTerminal, PreparedProjectTask,
    PreparedTaskExecution, ProjectEditScope, ProjectTaskReuseRequest, TaskReuseRequest,
};
use crate::project::{ProjectAssetRecord, ProjectAssetStore, ProjectAssetSummary};
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
const INTENTS: [NativeTaskIntent; 7] = [
    NativeTaskIntent::TextToImage,
    NativeTaskIntent::TextToVideo,
    NativeTaskIntent::TextToVideoWithAudio,
    NativeTaskIntent::FirstFrameToVideoWithAudio,
    NativeTaskIntent::SpeechSynthesis,
    NativeTaskIntent::Transcription,
    NativeTaskIntent::TextToMusic,
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    intent: Option<NativeTaskIntent>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProjectAssets {
    after_asset_id: Option<String>,
    #[serde(default = "default_project_asset_limit")]
    limit: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProjectAssetCandidates {
    after_asset_id: Option<String>,
    kind: Option<String>,
    created_before_ms: Option<i64>,
    created_after_ms: Option<i64>,
    file_name_contains: Option<String>,
    #[serde(default = "default_project_asset_candidate_limit")]
    limit: u32,
}

fn default_project_asset_limit() -> u32 {
    20
}

fn default_project_asset_candidate_limit() -> u32 {
    20
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProjectAsset {
    asset_id: String,
    #[serde(default)]
    text_offset: usize,
    #[serde(default = "default_project_asset_text_limit")]
    text_limit: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Status {
    job_id: String,
}

fn default_project_asset_text_limit() -> u32 {
    4096
}

fn project_asset_summary(record: &ProjectAssetSummary) -> Value {
    json!({
        "assetId": record.asset_id,
        "projectId": record.project_id,
        "sourceSessionId": record.source_session_id,
        "jobId": record.job_id,
        "workItemId": record.work_item_id,
        "runId": record.run_id,
        "createdAtMs": record.created_at_ms,
        "archiveContentSha256": record.content_sha256,
        "kind": record.kind,
        "output": record.output,
    })
}

fn project_asset_projection(record: ProjectAssetRecord, query: &ProjectAsset) -> Result<Value> {
    ensure!(
        (1..=16384).contains(&query.text_limit),
        "Transcript page size must be between 1 and 16384 Unicode scalar characters"
    );
    let kind = record.content.get("kind").and_then(Value::as_str);
    let output = match kind {
        Some("media") => {
            ensure!(
                query.text_offset == 0 && query.text_limit == default_project_asset_text_limit(),
                "Text pagination applies only to transcript assets"
            );
            record
                .content
                .get("result")
                .context("Archived media result is missing")?
                .clone()
        }
        Some("transcript") => {
            let transcript = record.content["transcript"]
                .as_object()
                .context("Archived transcript is missing")?;
            let text = transcript
                .get("text")
                .and_then(Value::as_str)
                .context("Archived transcript text is missing")?;
            let total = text.chars().count();
            ensure!(
                query.text_offset <= total,
                "Transcript offset exceeds its Unicode scalar character length"
            );
            let page: String = text
                .chars()
                .skip(query.text_offset)
                .take(query.text_limit as usize)
                .collect();
            let end = query.text_offset + page.chars().count();
            let mut output: JsonObject = transcript
                .iter()
                .filter(|(key, _)| key.as_str() != "text")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            output.insert("text".into(), json!(page));
            output.insert("textOffset".into(), json!(query.text_offset));
            output.insert("textLimit".into(), json!(query.text_limit));
            output.insert("textTotalChars".into(), json!(total));
            output.insert(
                "textTruncated".into(),
                json!(query.text_offset != 0 || end < total),
            );
            output.insert("nextTextOffset".into(), json!((end < total).then_some(end)));
            Value::Object(output)
        }
        _ => bail!("Unsupported project asset content"),
    };
    let lineage = record
        .content
        .get("projectLineage")
        .filter(|value| !value.is_null())
        .map(|value| {
            json!({
                "reuseKind":"parameters",
                "parentAssetId":value.get("parentAssetId"),
                "parentAssetContentSha256":value.get("parentAssetContentSha256"),
                "patch":value.get("patch"),
            })
        });
    Ok(json!({"status":"found", "projection":true, "asset":{
        "assetId":record.asset_id, "projectId":record.project_id,
        "sourceSessionId":record.source_session_id, "jobId":record.job_id,
        "workItemId":record.work_item_id, "runId":record.run_id,
        "createdAtMs":record.created_at_ms, "archiveContentSha256":record.content_sha256,
        "kind":kind, "output":output, "configuration":record.content.get("configuration"),
        "projectLineage":lineage
    }}))
}

const MAX_PROJECT_ASSET_CANDIDATE_PAGES: usize = 10;

fn candidate_kind_matches(summary: &ProjectAssetSummary, requested: Option<&str>) -> bool {
    let Some(requested) = requested else {
        return true;
    };
    summary.kind == requested
        || (summary.kind == "media"
            && summary.output.get("kind").and_then(Value::as_str) == Some(requested))
}

fn candidate_matches(
    summary: &ProjectAssetSummary,
    query: &ProjectAssetCandidates,
    file_name_needle: Option<&str>,
) -> bool {
    candidate_kind_matches(summary, query.kind.as_deref())
        && query
            .created_after_ms
            .is_none_or(|value| summary.created_at_ms >= value)
        && query
            .created_before_ms
            .is_none_or(|value| summary.created_at_ms < value)
        && file_name_needle.is_none_or(|needle| {
            summary
                .output
                .get("fileName")
                .and_then(Value::as_str)
                .is_some_and(|file_name| file_name.to_lowercase().contains(needle))
        })
}

fn validate_candidate_query(query: &ProjectAssetCandidates) -> Result<Option<String>> {
    ensure!(
        (1..=100).contains(&query.limit),
        "Project asset candidate limit must be between 1 and 100"
    );
    ensure!(
        query.kind.is_some()
            || query.created_before_ms.is_some()
            || query.created_after_ms.is_some()
            || query.file_name_contains.is_some(),
        "Project asset candidates require at least one filter"
    );
    if let (Some(after), Some(before)) = (query.created_after_ms, query.created_before_ms) {
        ensure!(
            after < before,
            "Project asset candidate time range is empty"
        );
    }
    if let Some(kind) = query.kind.as_deref() {
        ensure!(
            matches!(kind, "media" | "transcript" | "image" | "audio" | "video"),
            "Unsupported project asset candidate kind"
        );
    }
    query
        .file_name_contains
        .as_deref()
        .map(|value| {
            ensure!(
                !value.trim().is_empty()
                    && value.len() <= 128
                    && !value.chars().any(char::is_control),
                "Invalid project asset file name filter"
            );
            Ok(value.to_lowercase())
        })
        .transpose()
}

async fn project_asset_candidates(
    assets: &ProjectAssetStore,
    session: &str,
    query: ProjectAssetCandidates,
) -> Result<Value> {
    let file_name_needle = validate_candidate_query(&query)?;
    let mut after = query.after_asset_id.clone();
    let mut matches = Vec::new();
    let mut scanned_through = None;
    let mut exhausted = false;
    for _ in 0..MAX_PROJECT_ASSET_CANDIDATE_PAGES {
        let page = assets.list(session, after.as_deref(), 100).await?;
        if page.is_empty() {
            exhausted = true;
            break;
        }
        let page_len = page.len();
        for summary in page {
            scanned_through = Some(summary.asset_id.clone());
            if candidate_matches(&summary, &query, file_name_needle.as_deref()) {
                matches.push(summary);
                if matches.len() > query.limit as usize {
                    break;
                }
            }
        }
        if matches.len() > query.limit as usize {
            break;
        }
        if page_len < 100 {
            exhausted = true;
            break;
        }
        after = scanned_through.clone();
    }
    let truncated = matches.len() > query.limit as usize;
    let ambiguous = matches.len() > 1 || truncated || !exhausted;
    matches.truncate(query.limit as usize);
    let status = if !exhausted {
        "ambiguous"
    } else if matches.is_empty() {
        "not_found"
    } else if ambiguous {
        "ambiguous"
    } else {
        "unique"
    };
    let assets: Vec<_> = matches.iter().map(project_asset_summary).collect();
    Ok(json!({
        "status": status,
        "assets": assets,
        "truncated": truncated,
        "scannedThroughAssetId": scanned_through.clone(),
        "nextAfterAssetId": (!exhausted).then_some(scanned_through).flatten(),
    }))
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProjectReuse {
    identity: TaskParameterIdentity,
    parent_asset_id: String,
    patch: JsonObject,
    #[serde(default)]
    edit_scope: Option<ProjectEditScope>,
    #[serde(default)]
    edit_replacement_asset_id: Option<String>,
}

enum Request {
    Run(Run),
    Reuse(Reuse),
    ProjectReuse(ProjectReuse),
}

enum PreparedNativeTask {
    Session(NativeTaskPlan),
    Project(PreparedProjectTask),
}

impl PreparedNativeTask {
    fn plan(&self) -> &NativeTaskPlan {
        match self {
            Self::Session(plan) => plan,
            Self::Project(task) => task.plan(),
        }
    }
}

impl Request {
    fn identity(&self) -> &TaskParameterIdentity {
        match self {
            Self::Run(v) => &v.identity,
            Self::Reuse(v) => &v.identity,
            Self::ProjectReuse(v) => &v.identity,
        }
    }

    async fn prepare(
        self,
        storage: &SessionStorage,
        session: &str,
        job: &str,
    ) -> Result<(PreparedNativeTask, Arc<InstallationService>)> {
        let installation = storage.native_installations().service().await?;
        installation.wait_ready().await?;
        let plan = match self {
            Self::Run(run) => {
                let media = MediaResultStore::new(storage.pool().await?.clone());
                PreparedNativeTask::Session(
                    prepare_first(&installation, &media, session, job, run).await?,
                )
            }
            Self::Reuse(reuse) => {
                let request = TaskReuseRequest {
                    identity: reuse.identity,
                    parent_job_id: reuse.parent_job_id,
                    parent_result_id: reuse.parent_result_id,
                    new_job_id: job.into(),
                    patch: reuse.patch,
                };
                PreparedNativeTask::Session(
                    prepare_reused_task(&installation, storage, session, request).await?,
                )
            }
            Self::ProjectReuse(reuse) => {
                let request = ProjectTaskReuseRequest {
                    identity: reuse.identity,
                    parent_asset_id: reuse.parent_asset_id,
                    new_job_id: job.into(),
                    patch: reuse.patch,
                    edit_scope: reuse.edit_scope,
                    edit_replacement_asset_id: reuse.edit_replacement_asset_id,
                };
                PreparedNativeTask::Project(
                    prepare_project_reused_task(&installation, storage, session, request).await?,
                )
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
        .with_instructions("Use list to discover an installed task's exact identity and parameter contract. Use status with a returned jobId to inspect a durable task state. Keep references to run results for subsequent reuse. Run/reuse media inputs are session result UUIDs. Use project_assets/project_asset to find archived assets in this project, then project_reuse to regenerate with one changed parameter; replacement media inputs are project asset UUIDs. A bounded image_rect edit requires an image replacement; a time_range edit requires a matching WAV audio or single-stream AVI video replacement and never silently falls back to full regeneration.".to_string());
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
        let session = &ctx.session_id;
        self.context
            .session_manager
            .get_session(session, false)
            .await
            .context("Native task session is unavailable")?;
        let arguments = Value::Object(arguments.unwrap_or_default());
        let storage = self.context.session_manager.storage();
        if name == "status" {
            let query: Status = serde_json::from_value(arguments)?;
            let jobs = NativeTaskJobs::new(storage.pool().await?.clone());
            let state = jobs
                .reconcile_running(&storage.media_output_directory(), session, &query.job_id)
                .await?;
            let Some(state) = state else {
                return Ok(CallToolResult::error(vec![Content::text(
                    serde_json::to_string(&json!({
                        "jobId": query.job_id,
                        "status": "not_found"
                    }))?,
                )]));
            };
            let summary = if state == "completed" {
                jobs.completed_summary(session, &query.job_id).await?
            } else {
                None
            };
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string(&status_payload(&query.job_id, &state, summary))?,
            )]));
        }
        if name == "project_asset_candidates" {
            let query: ProjectAssetCandidates = serde_json::from_value(arguments)?;
            let assets = ProjectAssetStore::new(storage.pool().await?.clone());
            let response = project_asset_candidates(&assets, session, query).await?;
            ensure!(!cancel.is_cancelled(), "Native task call cancelled");
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string(&response)?,
            )]));
        }
        let call = ctx
            .tool_call_request_id
            .as_deref()
            .filter(|id| {
                !id.trim().is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control)
            })
            .context("Native tasks require a valid session tool call ID")?;
        if matches!(name, "project_assets" | "project_asset") {
            let assets = ProjectAssetStore::new(storage.pool().await?.clone());
            let response = if name == "project_assets" {
                let query: ProjectAssets = serde_json::from_value(arguments)?;
                ensure!(
                    (1..=100).contains(&query.limit),
                    "Project asset page size must be between 1 and 100"
                );
                let records = assets
                    .list(session, query.after_asset_id.as_deref(), query.limit)
                    .await?;
                let next = (records.len() == query.limit as usize)
                    .then(|| records.last().map(|record| record.asset_id.clone()))
                    .flatten();
                let summaries: Vec<_> = records.iter().map(project_asset_summary).collect();
                json!({"assets": summaries, "nextAfterAssetId": next})
            } else {
                let query: ProjectAsset = serde_json::from_value(arguments)?;
                ensure!(
                    (1..=16384).contains(&query.text_limit),
                    "Transcript page size must be between 1 and 16384 Unicode scalar characters"
                );
                match assets.read(session, &query.asset_id).await? {
                    Some(record) => project_asset_projection(record, &query)?,
                    None => json!({"status": "not_found"}),
                }
            };
            ensure!(!cancel.is_cancelled(), "Native task call cancelled");
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string(&response)?,
            )]));
        }
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
            "project_reuse" => Request::ProjectReuse(serde_json::from_value(arguments.clone())?),
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
                let (prepared, installation) = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => bail!("Native task cancelled"),
                    prepared = request.prepare(&storage, &session, &job) => prepared?,
                };
                let engines = configured_task_engines(prepared.plan().runtime())?;
                let association = MediaAssociation::ToolCall { tool_call_id: call };
                match prepared {
                    PreparedNativeTask::Session(plan) => {
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
                    PreparedNativeTask::Project(task) => {
                        execute_project_prepared_task_controlled(
                            &installation,
                            &engines,
                            &storage,
                            &session,
                            task,
                            association,
                            cancel.clone(),
                        )
                        .await
                    }
                }
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

fn status_payload(job: &str, state: &str, summary: Option<Value>) -> Value {
    let mut payload = json!({"jobId": job, "status": state});
    if let Some(Value::Object(extra)) = summary {
        if let Some(payload) = payload.as_object_mut() {
            payload.extend(extra);
        }
    }
    payload
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
        NativeTaskParameterContract::PocketTts(_)
        | NativeTaskParameterContract::Qwen3Asr(_)
        | NativeTaskParameterContract::StableAudioMusic(_) => {
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
            let project_asset_id = transcript.project_asset_id()?;
            Ok(CallToolResult::success(vec![
                Content::text(serde_json::to_string(&json!({
                    "status":"completed",
                    "jobId":job,
                    "transcriptSha256":transcript.sha256,
                    "projectAssetId":project_asset_id.clone(),
                    "projectAssetLookup": {
                        "tool":"project_asset",
                        "assetId":project_asset_id,
                    },
                }))?),
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
        ("status", "Read the durable state of a native task by its returned job ID.",
            schema(json!({"jobId":{"type":"string","pattern":"^native:"}}), &["jobId"]), true),
        ("run", "Run an installed local task. Supply its identity and model-specific parameters.",
            schema(json!({"identity":identity,"parameters":run_parameters}), &["identity","parameters"]), false),
        ("reuse", "Create a new task from an earlier job in this session, changing one field. Image and video require a parent result. Speech and transcription use the parent job.",
            schema(reuse_fields,&["identity","parentJobId","patch"]), false),
        ("project_reuse", "Regenerate from an archived asset in the current project, changing exactly one model-specific parameter and retaining the original. Use the parent's exact installed identity. Replacement media fields accept project asset UUIDs. An image_rect editScope with editReplacementAssetId performs a bounded local image edit. A time_range editScope performs a bounded WAV audio or single-stream AVI video edit when the parent and replacement formats match; MP4/WebM and audio-video dual-stream AVI remain fail-closed. Local edits never silently fall back to full regeneration, and execution still requires a measured resource profile.",
            schema(json!({"identity":identity,"parentAssetId":{"type":"string","format":"uuid"},"patch":{"type":"object","minProperties":1,"maxProperties":1,"description":"One field from the installed model descriptor. Media replacements are project asset UUIDs."},"editScope":{"oneOf":[{"type":"object","properties":{"kind":{"const":"image_rect"},"x":{"type":"integer","minimum":0},"y":{"type":"integer","minimum":0},"width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1}},"required":["kind","x","y","width","height"],"additionalProperties":false},{"type":"object","description":"Bounded edit of a matching WAV audio or single-stream AVI video result.","properties":{"kind":{"const":"time_range"},"startMicros":{"type":"integer","minimum":0},"durationMicros":{"type":"integer","minimum":1}},"required":["kind","startMicros","durationMicros"],"additionalProperties":false}]},"editReplacementAssetId":{"type":"string","format":"uuid"}}), &["identity","parentAssetId","patch"]), false),
        ("project_assets", "List archived asset metadata in the current session's project. Use project_asset to read one immutable result/version. This does not grant cross-session generation or file access.",
            schema(json!({"afterAssetId":{"type":"string","format":"uuid"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}}), &[]), true),
        ("project_asset_candidates", "Find archived project assets with bounded metadata filters. Returns unique only when exactly one match is proven; multiple matches, truncated scans, and scan limits return ambiguous so the caller must inspect candidates and choose an explicit assetId.",
            schema(json!({"afterAssetId":{"type":"string","format":"uuid"},"kind":{"type":"string","enum":["media","image","audio","video","transcript"]},"createdAfterMs":{"type":"integer"},"createdBeforeMs":{"type":"integer"},"fileNameContains":{"type":"string","maxLength":128},"limit":{"type":"integer","minimum":1,"maximum":100,"default":20}}), &[]), true),
        ("project_asset", "Read a projection of one archived result/version with its original configuration in the current session's project. Transcript text is paged in Unicode scalar characters; follow nextTextOffset. archiveContentSha256 identifies the full archive, not the projection. Media rejects nondefault text pagination. Unknown and other-project assets are not found. Reading does not edit, regenerate or grant file access.",
            schema(json!({"assetId":{"type":"string","format":"uuid"},"textOffset":{"type":"integer","minimum":0,"default":0},"textLimit":{"type":"integer","minimum":1,"maximum":16384,"default":4096}}), &["assetId"]), true),
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
        let result = if matches!(
            name,
            "list" | "status" | "project_assets" | "project_asset_candidates" | "project_asset"
        ) {
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

#[cfg(test)]
#[path = "native_tasks_project_assets_tests.rs"]
mod project_assets_tests;
