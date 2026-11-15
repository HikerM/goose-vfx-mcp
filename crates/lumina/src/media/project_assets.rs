use anyhow::Result;
use serde_json::json;
use sqlx::{Sqlite, Transaction};

use super::{MediaResultRef, MediaSource};
use crate::project::ProjectAssetStore;

#[cfg(feature = "local-inference")]
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ProjectOutput<'a> {
    Media {
        result: &'a MediaResultRef,
    },
    Transcript {
        transcript: &'a super::NativeTaskTranscript,
    },
}

#[cfg(feature = "local-inference")]
#[derive(serde::Serialize)]
struct ProjectArchive<'a> {
    #[serde(flatten)]
    output: ProjectOutput<'a>,
    configuration: &'a super::task_configuration::ArchivedTaskConfiguration,
    #[serde(rename = "projectLineage", skip_serializing_if = "Option::is_none")]
    lineage: Option<&'a super::project_task_reuse::VerifiedProjectTaskAuthorization>,
}

#[cfg(feature = "local-inference")]
fn archive_value(archive: &ProjectArchive<'_>) -> Result<serde_json::Value> {
    let limit = archive
        .lineage
        .and_then(|authorization| authorization.payload_tier())
        .map_or(32 * 1024 * 1024, |tier| tier.output_archive_limit());
    super::project_payload::serialized_len(archive, limit)?;
    Ok(serde_json::to_value(archive)?)
}

pub(super) async fn record_media(
    tx: &mut Transaction<'_, Sqlite>,
    result: &MediaResultRef,
) -> Result<()> {
    let call = match &result.source {
        MediaSource::Mcp { tool_call_id, .. } => Some(tool_call_id.as_str()),
        MediaSource::LocalModel { .. } => None,
    };
    if !ProjectAssetStore::has_project_scope_in_tx(tx, &result.session_id, &result.job_id, call)
        .await?
    {
        return Ok(());
    }
    let content = match &result.source {
        MediaSource::Mcp { .. } => {
            json!({"kind":"media", "result":result, "configuration":serde_json::Value::Null})
        }
        MediaSource::LocalModel { .. } => {
            #[cfg(feature = "local-inference")]
            {
                let archive = super::task_configuration::ArchivedTaskConfiguration::read(
                    tx,
                    &result.session_id,
                    &result.job_id,
                )
                .await?;
                let record = archive.validate(&result.session_id, &result.job_id)?;
                record.verify_source(&result.session_id, &result.job_id, &result.source)?;
                let lineage = super::project_task_reuse::project_lineage(tx, &record).await?;
                archive_value(&ProjectArchive {
                    output: ProjectOutput::Media { result },
                    configuration: &archive,
                    lineage: lineage.as_ref(),
                })?
            }
            #[cfg(not(feature = "local-inference"))]
            anyhow::bail!("Local task configuration verification is unavailable in this build")
        }
    };
    ProjectAssetStore::record_in_tx(
        tx,
        &result.session_id,
        &result.job_id,
        call,
        &result.result_id,
        &content,
    )
    .await
}

#[cfg(feature = "local-inference")]
pub(super) async fn record_transcript(
    tx: &mut Transaction<'_, Sqlite>,
    transcript: &super::NativeTaskTranscript,
) -> Result<()> {
    use anyhow::ensure;
    use lumina_providers::local_inference::lifecycle::Sha256Digest;
    use sha2::{Digest, Sha256};
    if !ProjectAssetStore::has_project_scope_in_tx(
        tx,
        &transcript.session_id,
        &transcript.job_id,
        None,
    )
    .await?
    {
        return Ok(());
    }
    let archive = super::task_configuration::ArchivedTaskConfiguration::read(
        tx,
        &transcript.session_id,
        &transcript.job_id,
    )
    .await?;
    let record = archive.validate(&transcript.session_id, &transcript.job_id)?;
    ensure!(
        matches!(
            record.snapshot(),
            super::TaskConfigurationSnapshot::Qwen3Asr(_)
        ) && record.generation_config_sha256() == transcript.generation_config_sha256
            && transcript.bytes == transcript.text.len() as u64
            && transcript.bytes <= super::MAX_TASK_TRANSCRIPT_BYTES
            && Sha256Digest::of_bytes(transcript.text.as_bytes()).as_str() == transcript.sha256,
        "Project transcript differs from its archived task configuration or output"
    );
    let identity = serde_json::to_vec(&(
        "project-transcript-v1",
        &transcript.session_id,
        &transcript.job_id,
        &transcript.generation_config_sha256,
        &transcript.sha256,
    ))?;
    let digest = Sha256::digest(identity);
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    let asset_id = uuid::Uuid::from_bytes(bytes).to_string();
    let lineage = super::project_task_reuse::project_lineage(tx, &record).await?;
    let content = archive_value(&ProjectArchive {
        output: ProjectOutput::Transcript { transcript },
        configuration: &archive,
        lineage: lineage.as_ref(),
    })?;
    ProjectAssetStore::record_in_tx(
        tx,
        &transcript.session_id,
        &transcript.job_id,
        None,
        &asset_id,
        &content,
    )
    .await
}

#[cfg(all(test, feature = "local-inference"))]
#[path = "project_assets_tests.rs"]
mod tests;

#[cfg(all(test, feature = "local-inference", windows))]
#[path = "../../tests/support/project_assets_probe.rs"]
mod production_probe;
