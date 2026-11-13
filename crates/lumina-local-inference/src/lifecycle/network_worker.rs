use anyhow::{ensure, Context, Result};
use futures::{stream, StreamExt, TryStreamExt};
use lumina_download_manager::managed::{
    ManagedModelScopeTransport, ManagedTransportError, ManagedTransportErrorKind, PinnedHttpRange,
};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::future::Future;
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::Duration;

use super::cache_worker::{replay_completed_installation, InstallExecution};
use super::installer::TransferSlot;
use super::modelscope::{pinned_artifact_url, scoped_auth_headers};
use super::network_ranges::{open_file_ranges, RangeResponse, RANGE_BYTES};
use super::*;

const FILE_CONCURRENCY: usize = 2;

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
    anyhow::Error::new(ModelSourceError {
        code,
        http_status: error.http_status,
    })
    .context(error)
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

async fn acquire_slot<F: Fn() -> i64>(
    execution: &InstallExecution<'_, F>,
    slot: TransferSlot,
) -> Result<File> {
    loop {
        execution.checkpoint()?;
        if let Some(permit) = execution.store.try_transfer_slot(slot)? {
            return Ok(permit);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn open_download<'a, F: Fn() -> i64>(
    execution: &'a InstallExecution<'_, F>,
    client: &'a ManagedModelScopeTransport,
    url: reqwest::Url,
    record: &FileTransferRecord,
    headers: &'a HeaderMap,
    whole_file: bool,
) -> Result<RangeResponse<'a>> {
    let etag = match &record.checkpoint.resume_validator {
        ResumeValidator::Unknown => None,
        ResumeValidator::StrongEtag { value } => Some(value.clone()),
    };
    let request = move |range: PinnedHttpRange, whole_file| {
        let url = url.clone();
        async move {
            let permit = acquire_slot(execution, TransferSlot::Http).await?;
            let body = if whole_file {
                with_lease(
                    execution,
                    client.open_file(url, range.start, range.total, range.if_range, headers),
                )
                .await?
            } else {
                with_lease(execution, client.open_range(url, &range, headers)).await?
            }
            .map_err(source_error)?;
            let expected_bytes = body.expected_bytes();
            let validator = body
                .strong_etag()
                .map_or(ResumeValidator::Unknown, |value| {
                    ResumeValidator::StrongEtag {
                        value: value.to_owned(),
                    }
                });
            let chunks = stream::try_unfold((body, permit), move |(mut body, permit)| async move {
                let chunk = with_lease(execution, body.next_chunk())
                    .await?
                    .map_err(source_error)?;
                Ok(chunk.map(|chunk| (chunk, (body, permit))))
            })
            .boxed_local();
            Ok(RangeResponse {
                expected_bytes,
                validator,
                chunks,
            })
        }
    };
    if whole_file {
        request(
            PinnedHttpRange {
                start: record.checkpoint.completed_bytes,
                length: record.expected_bytes - record.checkpoint.completed_bytes,
                total: record.expected_bytes,
                if_range: etag,
            },
            true,
        )
        .await
    } else {
        open_file_ranges(
            record.checkpoint.completed_bytes,
            record.expected_bytes,
            etag,
            request,
        )
        .await
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
    if let FilePurpose::RuntimeDistributionConfig(profile) = artifact.purpose {
        return execution.copy_runtime_spec(plan, original, profile);
    }
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
    let validator = loop {
        let result = async {
            let mut download = open_download(
                execution,
                client,
                url.clone(),
                &record,
                headers,
                restarts != 0,
            )
            .await?;
            let mut completed = record.checkpoint.completed_bytes;
            let mut persisted = completed;
            while let Some(chunk) = download.chunks.next().await {
                let chunk = chunk?;
                execution.checkpoint()?;
                completed = completed
                    .checked_add(chunk.len() as u64)
                    .context("Network transfer byte count overflow")?;
                ensure!(
                    completed <= record.expected_bytes,
                    "Network transfer exceeded pinned size"
                );
                execution.repository.require_installation_space(
                    &execution.lease.job_id,
                    execution.store,
                    chunk.len() as u64,
                )?;
                output.write_all(&chunk)?;
                hash.update(&chunk);
                if completed.saturating_sub(persisted) >= RANGE_BYTES
                    && completed < record.expected_bytes
                {
                    super::worker_control::sync_with_lease(
                        || output.sync_data(),
                        || execution.checkpoint().map(|_| ()),
                    )?;
                    record = execution.repository.checkpoint_synced_file(
                        &execution.lease,
                        &record.artifact_id,
                        record.version,
                        TransferCheckpoint {
                            completed_bytes: completed,
                            prefix_sha256: Sha256Digest::from_sha256(hash.clone()),
                            resume_validator: download.validator.clone(),
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
            Ok::<_, anyhow::Error>(download.validator)
        }
        .await;
        match result {
            Ok(validator) => break validator,
            Err(error) => {
                let kind = error
                    .downcast_ref::<ManagedTransportError>()
                    .map(|error| error.kind);
                if restarts != 0
                    || !matches!(
                        kind,
                        Some(
                            ManagedTransportErrorKind::ResumeUnavailable
                                | ManagedTransportErrorKind::ValidatorChanged
                        )
                    )
                {
                    return Err(error);
                }
                let reason = if kind == Some(ManagedTransportErrorKind::ValidatorChanged) {
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
        }
    };
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
    super::worker_control::sync_with_lease(
        || output.sync_all(),
        || execution.checkpoint().map(|_| ()),
    )?;
    execution.repository.checkpoint_synced_file(
        &execution.lease,
        &record.artifact_id,
        record.version,
        TransferCheckpoint {
            completed_bytes: record.expected_bytes,
            prefix_sha256: digest,
            resume_validator: validator,
        },
        (execution.clock)(),
    )?;
    Ok(())
}

async fn download_files<F, Download, DownloadFuture>(
    execution: &InstallExecution<'_, F>,
    files: Vec<FileTransferRecord>,
    download: Download,
) -> Result<()>
where
    F: Fn() -> i64,
    Download: Fn(FileTransferRecord) -> DownloadFuture,
    DownloadFuture: Future<Output = Result<()>>,
{
    stream::iter(files.into_iter().filter(|file| {
        !matches!(
            file.state,
            FileTransferState::Downloaded | FileTransferState::Verified { .. }
        )
    }))
    .map(|file| {
        let download = &download;
        async move {
            let _permit = acquire_slot(execution, TransferSlot::File).await?;
            execution.repository.begin_file_attempt(
                &execution.lease,
                &file.artifact_id,
                (execution.clock)(),
            )?;
            let artifact_id = file.artifact_id.clone();
            if let Err(error) = download(file).await {
                execution.record_file_failure(&artifact_id, &error)?;
                return Err(error);
            }
            Ok(())
        }
    })
    .buffer_unordered(FILE_CONCURRENCY)
    .try_for_each(|()| std::future::ready(Ok(())))
    .await
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
        let files = execution.repository.file_transfers(&job.id)?;
        download_files(execution, files, |file| {
            let client = &client;
            let plan = &plan;
            async move { download_file(execution, client, plan, &file, headers).await }
        })
        .await?;
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
    install_from_modelscope_controlled(
        repository,
        store,
        request,
        token,
        worker_name,
        clock,
        Arc::new(InstallationWorkerControl::default()),
    )
    .await
}

pub(crate) async fn install_from_modelscope_controlled<F>(
    repository: Arc<LifecycleRepository>,
    store: Arc<InstallationStore>,
    request: ModelScopeInstallRequest,
    token: Option<String>,
    worker_name: String,
    clock: F,
    control: Arc<InstallationWorkerControl>,
) -> Result<InstalledVersion>
where
    F: Fn() -> i64 + Send + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let result = (|| {
            control.checkpoint()?;
            ensure!(
                !request.installation_id.trim().is_empty() && !request.version_id.trim().is_empty(),
                "Network installation has no installation or version identity"
            );
            let _lock = store.transfer_lock(&request.job_id)?;
            let job = repository
                .read_job(&request.job_id)?
                .context("Network installation job not found")?;
            if let Some(intent) = repository.read_installation_input(&job.id)? {
                intent.input.validate_destination(
                    &store,
                    &request.installation_id,
                    &request.version_id,
                )?;
            }
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
                plan.is_managed_install_plan() && plan.digest()? == job.plan_digest,
                "Network installation requires its accepted immutable plan"
            );
            super::failure::validate_installation_store_admission(
                &repository,
                &store,
                &job,
                &plan,
                clock(),
            )?;
            let headers = match scoped_auth_headers(token.as_deref()) {
                Ok(headers) => headers,
                Err(error) => {
                    repository.block_installation(
                        &job,
                        &store,
                        InstallationFailureDetails::operation(
                            super::failure::installation_failure_code(&error),
                            InstallationFailureOrigin::Credentials,
                        ),
                        clock(),
                    )?;
                    return Err(error);
                }
            };
            let input = InstallationInput::new(
                &store,
                request.installation_id.clone(),
                request.version_id.clone(),
                InstallationSource::ModelScope {},
            )?;
            repository.adopt_installation_input(&job.id, &input, clock())?;
            let lease = repository.claim(&job.id, job.version, &worker_name, clock(), 120)?;
            let execution = InstallExecution {
                repository: &repository,
                store: &store,
                lease,
                clock: &clock,
                control,
            };
            let result = runtime.block_on(execute(&execution, &request, plan, &headers));
            if let Err(error) = &result {
                execution.settle_failure(error)?;
            }
            result
        })();
        repository.release_inactive_storage(&request.job_id, &store, clock())?;
        result
    })
    .await
    .context("Network installation worker stopped")?
}

#[cfg(test)]
#[path = "../../tests/support/network_file_downloads.rs"]
mod tests;
