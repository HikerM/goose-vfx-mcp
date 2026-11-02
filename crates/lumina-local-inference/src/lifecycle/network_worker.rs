use anyhow::{ensure, Context, Result};
use lumina_download_manager::managed::{
    ManagedModelScopeTransport, ManagedTransportError, ManagedTransportErrorKind,
};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::Duration;

use super::cache_worker::{replay_completed_installation, InstallExecution};
use super::modelscope::{pinned_artifact_url, scoped_auth_headers};
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelScopeInstallRequest {
    pub job_id: String,
    pub installation_id: String,
    pub version_id: String,
}

fn source_error(error: ManagedTransportError) -> anyhow::Error {
    let code = match error.kind {
        ManagedTransportErrorKind::Forbidden => LifecycleErrorCode::SourceForbidden,
        ManagedTransportErrorKind::Authentication => LifecycleErrorCode::SourceAuthRequired,
        ManagedTransportErrorKind::NotFound => LifecycleErrorCode::SourceNotFound,
        ManagedTransportErrorKind::Timeout => LifecycleErrorCode::SourceTimeout,
        ManagedTransportErrorKind::RateLimited => LifecycleErrorCode::SourceRateLimited,
        ManagedTransportErrorKind::Unavailable => LifecycleErrorCode::SourceUnavailable,
        ManagedTransportErrorKind::RevisionChanged => LifecycleErrorCode::RevisionChanged,
        _ => LifecycleErrorCode::SourcePartialFailure,
    };
    ModelSourceError {
        code,
        http_status: error.http_status,
    }
    .into()
}

async fn with_lease<F: Fn() -> i64, T>(
    execution: &InstallExecution<'_, F>,
    future: impl Future<Output = T>,
) -> Result<T> {
    let future = std::pin::pin!(future);
    let mut future = future;
    loop {
        tokio::select! {
            result=&mut future => {execution.checkpoint()?; return Ok(result);},
            _=tokio::time::sleep(Duration::from_secs(3)) => {execution.checkpoint()?;}
        }
    }
}

async fn download_file<F: Fn() -> i64>(
    execution: &InstallExecution<'_, F>,
    client: &ManagedModelScopeTransport,
    plan: &InstallPlan,
    original: &FileTransferRecord,
    headers: &HeaderMap,
) -> Result<()> {
    let artifact = &plan.artifacts[&original.artifact_id];
    let snapshot = &plan.snapshots[&artifact.snapshot_id];
    let url = pinned_artifact_url(artifact, snapshot)?;
    let path = execution
        .store
        .transfer_file_path(&execution.lease.job_id, &original.relative_path)?;
    let mut output = super::files::open_exclusive_transfer(&path)?;
    let mut record = original.clone();
    if let FileTransferState::Failed { code } = record.state {
        record = if code == LifecycleErrorCode::HashMismatch {
            execution.repository.restart_file(
                &execution.lease,
                &record.artifact_id,
                record.version,
                FileRestartReason::HashMismatch,
                (execution.clock)(),
            )?
        } else {
            execution.repository.resume_failed_file(
                &execution.lease,
                &record.artifact_id,
                record.version,
                (execution.clock)(),
            )?
        };
    }
    let mut hash = Sha256::new();
    if record.checkpoint.completed_bytes > 0 {
        let prefix = if output.metadata()?.len() >= record.checkpoint.completed_bytes {
            Some(execution.hash_prefix(&mut output, record.checkpoint.completed_bytes)?)
        } else {
            None
        };
        let reason = if prefix.as_ref().is_none_or(|value| {
            Sha256Digest::from_sha256(value.clone()) != record.checkpoint.prefix_sha256
        }) {
            Some(FileRestartReason::PartialContentMismatch)
        } else if record.checkpoint.resume_validator == ResumeValidator::Unknown {
            Some(FileRestartReason::RangeUnavailable)
        } else {
            None
        };
        if let Some(reason) = reason {
            record = execution.repository.restart_file(
                &execution.lease,
                &record.artifact_id,
                record.version,
                reason,
                (execution.clock)(),
            )?;
        } else {
            hash = prefix.expect("Validated checkpoint prefix");
        }
    }
    execution.checkpoint()?;
    output.set_len(record.checkpoint.completed_bytes)?;
    output.seek(SeekFrom::Start(record.checkpoint.completed_bytes))?;
    let mut restarts = 0;
    let mut body = loop {
        execution.checkpoint()?;
        let etag = match &record.checkpoint.resume_validator {
            ResumeValidator::Unknown => None,
            ResumeValidator::StrongEtag { value } => Some(value.clone()),
        };
        let result = with_lease(
            execution,
            client.open_file(
                url.clone(),
                record.checkpoint.completed_bytes,
                record.expected_bytes,
                etag,
                headers,
            ),
        )
        .await?;
        match result {
            Ok(body) => break body,
            Err(error)
                if restarts == 0
                    && record.checkpoint.completed_bytes > 0
                    && matches!(
                        error.kind,
                        ManagedTransportErrorKind::ResumeUnavailable
                            | ManagedTransportErrorKind::ValidatorChanged
                    ) =>
            {
                let reason = if error.kind == ManagedTransportErrorKind::ValidatorChanged {
                    FileRestartReason::ValidatorChanged
                } else {
                    FileRestartReason::RangeUnavailable
                };
                record = execution.repository.restart_file(
                    &execution.lease,
                    &record.artifact_id,
                    record.version,
                    reason,
                    (execution.clock)(),
                )?;
                output.set_len(0)?;
                output.rewind()?;
                hash = Sha256::new();
                restarts += 1;
            }
            Err(error) => return Err(source_error(error)),
        }
    };
    ensure!(
        body.expected_bytes() == record.expected_bytes - record.checkpoint.completed_bytes,
        "Transfer response does not cover the remaining pinned artifact"
    );
    let validator = body
        .strong_etag()
        .map_or(ResumeValidator::Unknown, |value| {
            ResumeValidator::StrongEtag {
                value: value.to_string(),
            }
        });
    let mut completed = record.checkpoint.completed_bytes;
    let mut persisted = completed;
    loop {
        let chunk = with_lease(execution, body.next_chunk())
            .await?
            .map_err(source_error)?;
        let Some(chunk) = chunk else { break };
        execution.checkpoint()?;
        completed = completed
            .checked_add(chunk.len() as u64)
            .context("Network transfer byte count overflow")?;
        ensure!(
            completed <= record.expected_bytes,
            "Network transfer exceeded pinned size"
        );
        output.write_all(&chunk)?;
        hash.update(&chunk);
        if completed.saturating_sub(persisted) >= 8 * 1024 * 1024
            && completed < record.expected_bytes
        {
            output.sync_data()?;
            record = execution.repository.checkpoint_file(
                &execution.lease,
                &record.artifact_id,
                record.version,
                TransferCheckpoint {
                    completed_bytes: completed,
                    prefix_sha256: Sha256Digest::from_sha256(hash.clone()),
                    resume_validator: validator.clone(),
                },
                (execution.clock)(),
            )?;
            persisted = completed;
        }
    }
    ensure!(
        completed == record.expected_bytes,
        "Network transfer ended before pinned file completion"
    );
    let digest = Sha256Digest::from_sha256(hash);
    if artifact
        .source_sha256
        .as_ref()
        .is_some_and(|expected| expected != &digest)
    {
        return Err(ModelSourceError {
            code: LifecycleErrorCode::HashMismatch,
            http_status: None,
        }
        .into());
    }
    output.sync_all()?;
    execution.repository.checkpoint_file(
        &execution.lease,
        &record.artifact_id,
        record.version,
        TransferCheckpoint {
            completed_bytes: completed,
            prefix_sha256: digest,
            resume_validator: validator,
        },
        (execution.clock)(),
    )?;
    Ok(())
}

async fn execute<F: Fn() -> i64>(
    execution: &InstallExecution<'_, F>,
    request: &ModelScopeInstallRequest,
    plan: InstallPlan,
    headers: &HeaderMap,
) -> Result<InstalledVersion> {
    let mut job = execution.checkpoint()?;
    if job.stage == LifecycleStage::ReadyToInstall {
        job = execution.advance(LifecycleStage::Downloading)?;
    }
    if job.stage == LifecycleStage::Downloading {
        let client = ManagedModelScopeTransport::new().map_err(source_error)?;
        for file in execution.repository.file_transfers(&job.id)? {
            if matches!(
                file.state,
                FileTransferState::Downloaded | FileTransferState::Verified { .. }
            ) {
                continue;
            }
            if let Err(error) = download_file(execution, &client, &plan, &file, headers).await {
                execution.record_file_failure(&file.artifact_id, &error)?;
                return Err(error);
            }
        }
        execution.advance(LifecycleStage::Verifying)?;
    }
    execution.finish_installation(&request.installation_id, &request.version_id, plan)
}

pub async fn install_from_modelscope<F>(
    repository: Arc<LifecycleRepository>,
    store: Arc<InstallationStore>,
    request: ModelScopeInstallRequest,
    token: Option<String>,
    worker_name: String,
    clock: F,
) -> Result<InstalledVersion>
where
    F: Fn() -> i64 + Send + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        ensure!(
            !request.installation_id.trim().is_empty() && !request.version_id.trim().is_empty(),
            "Network installation has no installation or version identity"
        );
        let _lock = store.transfer_lock(&request.job_id)?;
        let job = repository
            .read_job(&request.job_id)?
            .context("Network installation job not found")?;
        if job.status == JobStatus::Cancelling {
            repository.transition(&job.id, job.version, JobStatus::Cancelled, clock())?;
            return Err(ModelSourceError {
                code: LifecycleErrorCode::Cancelled,
                http_status: None,
            }
            .into());
        }
        if job.status == JobStatus::Completed {
            return replay_completed_installation(
                &repository,
                &store,
                &job,
                &request.installation_id,
                &request.version_id,
            );
        }
        let plan = repository
            .read_plan(&job.plan_id)?
            .context("Network installation plan is missing")?;
        ensure!(
            plan.rules_version == PREPARED_GGUF_PLAN_RULES_VERSION
                && plan.digest()? == job.plan_digest,
            "Network installation requires its accepted immutable plan"
        );
        plan.validate_for_install(job.accepted_at, fs2::available_space(store.root())?)?;
        let headers = scoped_auth_headers(token.as_deref())?;
        let lease = repository.claim(&job.id, job.version, &worker_name, clock(), 120)?;
        let execution = InstallExecution {
            repository: &repository,
            store: &store,
            lease,
            clock,
        };
        let result = runtime.block_on(execute(&execution, &request, plan, &headers));
        if result.is_err() {
            execution.settle_failure()?;
        }
        result
    })
    .await
    .context("Network installation worker stopped")?
}
