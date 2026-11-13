use anyhow::{bail, ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::{
    AudioTaskMediaReference, EffectiveGenerationParameters, GenerationMediaInput,
    GenerationOutputReference, GenerationParameterService, GenerationParameters,
    GenerationResultRecord, InstallationService, NativeTaskCatalog, NativeTaskParameterContract,
    NativeTaskPlan, PocketTtsParameterService, Qwen3AsrParameterService, SessionMediaParameterKind,
    Sha256Digest, TaskParameterIdentity,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::sync::Arc;

use super::{
    domain::validate_id, MediaKind, MediaResultStore, MediaSpec, TaskConfigurationRecord,
    TaskConfigurationSnapshot, TaskConfigurationStore,
};
use crate::session::session_manager::SessionStorage;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskReuseRequest {
    pub identity: TaskParameterIdentity,
    pub parent_job_id: String,
    pub parent_result_id: Option<String>,
    pub new_job_id: String,
    /// Exactly one writable parameter. A replacement media input is a session result UUID.
    pub patch: Map<String, Value>,
}

impl TaskReuseRequest {
    fn validate(&self) -> Result<()> {
        validate_id(&self.parent_job_id)?;
        validate_id(&self.new_job_id)?;
        ensure!(
            self.parent_job_id != self.new_job_id,
            "Task reuse requires a new job"
        );
        ensure!(
            self.patch.len() == 1 && self.patch.values().all(|v| !v.is_null()),
            "Provide exactly one non-null task parameter"
        );
        if let Some(id) = &self.parent_result_id {
            canonical_uuid(id)?;
        }
        Ok(())
    }
}

fn canonical_uuid(id: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id),
        "Expected a canonical media result UUID"
    );
    Ok(())
}

/// Reads the authenticated session's immutable configuration. Execution rechecks the installation,
/// input files and resources when the caller submits the returned plan.
pub async fn prepare_reused_task(
    installation: &InstallationService,
    storage: &SessionStorage,
    authenticated_session_id: &str,
    request: TaskReuseRequest,
) -> Result<NativeTaskPlan> {
    validate_id(authenticated_session_id)?;
    request.validate()?;
    let pool = storage.pool().await?;
    let configurations = TaskConfigurationStore::new(pool.clone());
    let parent = configurations
        .read(authenticated_session_id, &request.parent_job_id)
        .await?
        .context("Parent configuration is unavailable in this session")?;
    ensure!(
        parent.identity() == &request.identity,
        "Parent configuration does not match the selected model identity"
    );
    ensure!(
        configurations
            .read(authenticated_session_id, &request.new_job_id)
            .await?
            .is_none(),
        "New job already has a configuration in this session"
    );
    installation.wait_ready().await?;
    let repository = Arc::clone(installation.repository());
    let identity = request.identity.clone();
    let registered = tokio::task::spawn_blocking(move || {
        NativeTaskCatalog::for_version(
            &repository,
            &identity.installation_id,
            &identity.version_id,
        )?
        .describe(&identity)
    })
    .await
    .context("Task registration lookup failed")??;
    prepare_registered(
        &MediaResultStore::new(pool.clone()),
        &configurations,
        authenticated_session_id,
        &parent,
        request,
        registered.descriptor,
    )
    .await
}

pub(crate) async fn resolve_media_patch(
    media: &MediaResultStore,
    session: &str,
    parameters: &mut Map<String, Value>,
    contract: &NativeTaskParameterContract,
) -> Result<()> {
    for (field, value) in parameters.iter_mut() {
        let kind = match contract.session_media_kind(field)? {
            None => continue,
            Some(SessionMediaParameterKind::Audio) => MediaKind::Audio,
            Some(SessionMediaParameterKind::Image) => MediaKind::Image,
        };
        resolve_reference(media, session, value, kind)
            .await
            .with_context(|| format!("Invalid session media for {field}"))?;
    }
    Ok(())
}

async fn resolve_reference(
    media: &MediaResultStore,
    session: &str,
    value: &mut Value,
    kind: MediaKind,
) -> Result<()> {
    let id = value
        .as_str()
        .context("Replacement media input must be a session result UUID")?;
    canonical_uuid(id)?;
    let content = media.open_content(session, id).await?;
    let result = content.result();
    ensure!(
        result.session_id == session && result.result_id == id && result.kind == kind,
        "Replacement media belongs to another scope or kind"
    );
    let sha256 = Sha256Digest::parse(result.sha256.as_str())?;
    *value = match (&result.spec, kind) {
        (MediaSpec::Image { width, height }, MediaKind::Image) => {
            serde_json::to_value(GenerationMediaInput {
                session_id: session.into(),
                result_id: id.into(),
                sha256,
                mime_type: result.mime.clone(),
                size_bytes: result.bytes,
                width: *width,
                height: *height,
            })?
        }
        (
            MediaSpec::Audio {
                sample_rate,
                channels,
                sample_frames,
                ..
            },
            MediaKind::Audio,
        ) => serde_json::to_value(AudioTaskMediaReference {
            session_id: session.into(),
            result_id: id.into(),
            sha256,
            mime_type: result.mime.clone(),
            size_bytes: result.bytes,
            sample_rate: *sample_rate,
            channels: u32::from(*channels),
            sample_frames: *sample_frames,
        })?,
        _ => bail!("Replacement media dimensions do not match the task parameter"),
    };
    Ok(())
}

async fn prepare_registered(
    media: &MediaResultStore,
    configurations: &TaskConfigurationStore,
    session: &str,
    parent: &TaskConfigurationRecord,
    mut request: TaskReuseRequest,
    registered: NativeTaskParameterContract,
) -> Result<NativeTaskPlan> {
    ensure!(
        parent.session_id() == session
            && parent.job_id() == request.parent_job_id
            && parent.identity() == &request.identity,
        "Parent configuration differs from the selected session, job or model identity"
    );
    let digest = Sha256Digest::parse(parent.generation_config_sha256())?;
    resolve_media_patch(media, session, &mut request.patch, &registered).await?;
    match (parent.snapshot(), registered) {
        (
            TaskConfigurationSnapshot::Generation(snapshot),
            NativeTaskParameterContract::Generation(desc),
        ) => {
            let id = request
                .parent_result_id
                .as_deref()
                .context("Generation reuse requires a parent result")?;
            let (task, kind) = match &snapshot.parameters {
                EffectiveGenerationParameters::Image(_) => ("image", MediaKind::Image),
                EffectiveGenerationParameters::Video(_) => ("video", MediaKind::Video),
            };
            let content = media.open_content(session, id).await?;
            let result = content.result();
            ensure!(
                result.session_id == session
                    && result.result_id == id
                    && result.job_id == parent.job_id()
                    && result.kind == kind,
                "Parent media result differs from the selected job or task kind"
            );
            ensure!(
                configurations.verify_result(result).await? == *parent,
                "Parent media does not match the original model configuration"
            );
            let patch: GenerationParameters = serde_json::from_value(
                serde_json::json!({"task":task,"parameters":request.patch}),
            )?;
            let record = GenerationResultRecord {
                snapshot: (**snapshot).clone(),
                snapshot_digest: digest,
                outputs: vec![GenerationOutputReference {
                    session_id: session.into(),
                    result_id: result.result_id.clone(),
                    sha256: Sha256Digest::parse(result.sha256.as_str())?,
                }],
            };
            Ok(GenerationParameterService::new(desc.profile, desc.binding)?
                .reuse_result(session, &record, id, &request.new_job_id, patch)?
                .into())
        }
        (
            TaskConfigurationSnapshot::PocketTts(snapshot),
            NativeTaskParameterContract::PocketTts(desc),
        ) => {
            ensure!(
                request.parent_result_id.is_none(),
                "Speech reuse selects its parent configuration by job"
            );
            Ok(PocketTtsParameterService::new(desc.binding)?
                .reuse_configuration(
                    session,
                    snapshot,
                    &digest,
                    &request.new_job_id,
                    serde_json::from_value(Value::Object(request.patch))?,
                )?
                .into())
        }
        (
            TaskConfigurationSnapshot::Qwen3Asr(snapshot),
            NativeTaskParameterContract::Qwen3Asr(desc),
        ) => {
            ensure!(
                request.parent_result_id.is_none(),
                "Transcription reuse selects its parent configuration by job"
            );
            Ok(Qwen3AsrParameterService::new(desc.binding)?
                .reuse_configuration(
                    session,
                    snapshot,
                    &digest,
                    &request.new_job_id,
                    serde_json::from_value(Value::Object(request.patch))?,
                )?
                .into())
        }
        _ => bail!("Parent configuration and the registered task adapter differ"),
    }
}

#[cfg(test)]
#[path = "task_reuse_tests.rs"]
mod tests;
