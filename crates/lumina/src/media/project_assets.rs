use anyhow::Result;
use serde_json::json;
use sqlx::{Sqlite, Transaction};

use super::{MediaResultRef, MediaSource};
use crate::project::ProjectAssetStore;

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
    let configuration = match &result.source {
        MediaSource::Mcp { .. } => serde_json::Value::Null,
        MediaSource::LocalModel { .. } => {
            #[cfg(feature = "local-inference")]
            {
                let archive = super::task_configuration::ArchivedTaskConfiguration::read(
                    tx,
                    &result.session_id,
                    &result.job_id,
                )
                .await?;
                archive
                    .validate(&result.session_id, &result.job_id)?
                    .verify_source(&result.session_id, &result.job_id, &result.source)?;
                serde_json::to_value(archive)?
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
        &json!({"kind":"media", "result":result, "configuration":configuration}),
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
    ProjectAssetStore::record_in_tx(
        tx,
        &transcript.session_id,
        &transcript.job_id,
        None,
        &asset_id,
        &json!({"kind":"transcript", "transcript":transcript, "configuration":archive}),
    )
    .await
}

#[cfg(all(test, feature = "local-inference"))]
#[path = "project_assets_tests.rs"]
mod tests;

#[cfg(all(test, feature = "local-inference", windows))]
#[path = "../../tests/support/project_assets_probe.rs"]
mod production_probe;
