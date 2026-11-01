use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{
    build_installation_manifest, FileRestartReason, FileTransferRecord, FileTransferState,
    InstallPlan, InstallationCommitStage, InstallationStore, InstalledVersion, JobStatus,
    LifecycleErrorCode, LifecycleJob, LifecycleRepository, LifecycleStage, ModelSourceError,
    ResumeValidator, Sha256Digest, TransferCheckpoint, WorkerLease,
    PREPARED_GGUF_PLAN_RULES_VERSION,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheInstallRequest {
    pub job_id: String,
    pub installation_id: String,
    pub version_id: String,
    pub source_files: BTreeMap<String, PathBuf>,
}

struct CacheExecution<'a, F> {
    repository: &'a LifecycleRepository,
    store: &'a InstallationStore,
    lease: WorkerLease,
    clock: F,
}

impl<F: Fn() -> i64> CacheExecution<'_, F> {
    fn checkpoint(&self) -> Result<LifecycleJob> {
        let now = (self.clock)();
        let job = self
            .repository
            .read_job(&self.lease.job_id)?
            .context("Cache installation job disappeared")?;
        LifecycleRepository::validate_lease(&job, &self.lease, now)?;
        if job
            .lease_until
            .context("Cache worker has no lease expiry")?
            .saturating_sub(now)
            <= 30
        {
            self.repository
                .renew_lease(&self.lease, job.version, now, 120)
        } else {
            Ok(job)
        }
    }

    fn advance(&self, stage: LifecycleStage) -> Result<LifecycleJob> {
        let job = self.checkpoint()?;
        self.repository
            .advance(&self.lease, job.version, stage, (self.clock)())
    }

    fn hash_prefix(&self, file: &mut File, bytes: u64) -> Result<Sha256> {
        file.rewind()?;
        let mut hasher = Sha256::new();
        let mut remaining = bytes;
        let mut buffer = vec![0; 1024 * 1024];
        while remaining > 0 {
            self.checkpoint()?;
            let limit = remaining.min(buffer.len() as u64) as usize;
            let count = file.read(&mut buffer[..limit])?;
            ensure!(count > 0, "Cache transfer checkpoint is truncated");
            hasher.update(&buffer[..count]);
            remaining -= count as u64;
        }
        Ok(hasher)
    }

    fn copy_file(
        &self,
        plan: &InstallPlan,
        record: &FileTransferRecord,
        source_path: &Path,
    ) -> Result<()> {
        let expected = plan.artifacts[&record.artifact_id]
            .source_sha256
            .as_ref()
            .context("Local cache import requires a pinned source checksum")?;
        let mut source = super::files::open_for_verification(source_path)?;
        let before = source.metadata()?;
        ensure!(
            before.is_file() && before.len() == record.expected_bytes,
            "Local cache source does not match the pinned file size"
        );
        let target = self
            .store
            .transfer_file_path(&self.lease.job_id, &record.relative_path)?;
        let mut output = super::files::open_exclusive_transfer(&target)?;
        let mut record = record.clone();
        let mut hasher = Sha256::new();
        if record.checkpoint.resume_validator != ResumeValidator::Unknown {
            record = self.repository.restart_file(
                &self.lease,
                &record.artifact_id,
                record.version,
                FileRestartReason::RangeUnavailable,
                (self.clock)(),
            )?;
        }
        if record.checkpoint.completed_bytes > 0 {
            let prefix = if output.metadata()?.len() >= record.checkpoint.completed_bytes {
                Some(self.hash_prefix(&mut output, record.checkpoint.completed_bytes)?)
            } else {
                None
            };
            if let Some(prefix) = prefix.filter(|hash| {
                Sha256Digest::from_sha256(hash.clone()) == record.checkpoint.prefix_sha256
            }) {
                hasher = prefix;
            } else {
                record = self.repository.restart_file(
                    &self.lease,
                    &record.artifact_id,
                    record.version,
                    FileRestartReason::PartialContentMismatch,
                    (self.clock)(),
                )?;
            }
        }
        if matches!(record.state, FileTransferState::Failed { .. }) {
            record = self.repository.restart_file(
                &self.lease,
                &record.artifact_id,
                record.version,
                FileRestartReason::HashMismatch,
                (self.clock)(),
            )?;
            hasher = Sha256::new();
        }
        self.checkpoint()?;
        let mut completed = record.checkpoint.completed_bytes;
        output.set_len(completed)?;
        output.seek(SeekFrom::Start(completed))?;
        source.seek(SeekFrom::Start(completed))?;
        let mut persisted = completed;
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            self.checkpoint()?;
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            completed = completed
                .checked_add(count as u64)
                .context("Local cache byte count overflow")?;
            ensure!(
                completed <= record.expected_bytes,
                "Local cache source grew during import"
            );
            output.write_all(&buffer[..count])?;
            hasher.update(&buffer[..count]);
            if completed.saturating_sub(persisted) >= 64 * 1024 * 1024
                && completed < record.expected_bytes
            {
                output.sync_data()?;
                record = self.repository.checkpoint_file(
                    &self.lease,
                    &record.artifact_id,
                    record.version,
                    TransferCheckpoint {
                        completed_bytes: completed,
                        prefix_sha256: Sha256Digest::from_sha256(hasher.clone()),
                        resume_validator: ResumeValidator::Unknown,
                    },
                    (self.clock)(),
                )?;
                persisted = completed;
            }
        }
        let after = source.metadata()?;
        ensure!(
            completed == record.expected_bytes
                && after.len() == before.len()
                && after.modified()? == before.modified()?,
            "Local cache source changed during import"
        );
        let actual = Sha256Digest::from_sha256(hasher);
        if &actual != expected {
            return Err(ModelSourceError {
                code: LifecycleErrorCode::HashMismatch,
                http_status: None,
            }
            .into());
        }
        output.sync_all()?;
        self.repository.checkpoint_file(
            &self.lease,
            &record.artifact_id,
            record.version,
            TransferCheckpoint {
                completed_bytes: completed,
                prefix_sha256: actual,
                resume_validator: ResumeValidator::Unknown,
            },
            (self.clock)(),
        )?;
        Ok(())
    }

    fn record_file_failure(&self, artifact_id: &str, error: &anyhow::Error) -> Result<()> {
        let job = self
            .repository
            .read_job(&self.lease.job_id)?
            .context("Cache job disappeared")?;
        if LifecycleRepository::validate_lease(&job, &self.lease, (self.clock)()).is_ok() {
            let record = self
                .repository
                .read_file_transfer(&job.id, artifact_id)?
                .context("Cache file disappeared")?;
            let code = error
                .downcast_ref::<ModelSourceError>()
                .map_or(LifecycleErrorCode::InstallFailed, |error| error.code);
            self.repository.fail_file(
                &self.lease,
                artifact_id,
                record.version,
                code,
                (self.clock)(),
            )?;
        }
        Ok(())
    }

    fn install(
        &self,
        request: &CacheInstallRequest,
        plan: InstallPlan,
    ) -> Result<InstalledVersion> {
        let mut job = self.checkpoint()?;
        if job.stage == LifecycleStage::ReadyToInstall {
            job = self.advance(LifecycleStage::Downloading)?;
        }
        let transfer_root = self.store.transfer_directory(&request.job_id)?;
        if job.stage == LifecycleStage::Downloading {
            for file in self.repository.file_transfers(&request.job_id)? {
                self.checkpoint()?;
                let result = if matches!(
                    file.state,
                    FileTransferState::Downloaded | FileTransferState::Verified { .. }
                ) {
                    super::files::verify_artifact_file_with_progress(
                        &plan,
                        &file.artifact_id,
                        &transfer_root,
                        &file.relative_path,
                        |_| self.checkpoint().map(|_| ()),
                    )
                    .map(|_| ())
                } else {
                    self.copy_file(
                        &plan,
                        &file,
                        request
                            .source_files
                            .get(&file.artifact_id)
                            .context("Cache import is missing a source file")?,
                    )
                };
                if let Err(error) = result {
                    self.record_file_failure(&file.artifact_id, &error)?;
                    return Err(error);
                }
            }
            job = self.advance(LifecycleStage::Verifying)?;
        }
        if job.stage == LifecycleStage::Verifying {
            for file in self.repository.file_transfers(&request.job_id)? {
                self.checkpoint()?;
                let verified = match super::files::verify_artifact_file_with_progress(
                    &plan,
                    &file.artifact_id,
                    &transfer_root,
                    &file.relative_path,
                    |_| self.checkpoint().map(|_| ()),
                ) {
                    Ok(verified) => verified,
                    Err(error) => {
                        self.record_file_failure(&file.artifact_id, &error)?;
                        return Err(error);
                    }
                };
                self.repository.record_verified_file(
                    &self.lease,
                    &file.artifact_id,
                    file.version,
                    &verified,
                    (self.clock)(),
                )?;
            }
            job = self.advance(LifecycleStage::Installing)?;
        }
        ensure!(
            job.stage == LifecycleStage::Installing,
            "Cache worker cannot execute this lifecycle stage"
        );
        let manifest =
            if let Some(journal) = self.repository.installation_commit(&request.job_id)? {
                ensure!(
                    journal.manifest.installation_id == request.installation_id
                        && journal.manifest.version_id == request.version_id,
                    "Recovered installation belongs to different requested identities"
                );
                journal.manifest
            } else {
                build_installation_manifest(
                    request.installation_id.clone(),
                    request.version_id.clone(),
                    plan,
                    &self.repository.file_transfers(&request.job_id)?,
                    job.accepted_at,
                    (self.clock)(),
                )?
            };
        let job = self.checkpoint()?;
        self.store.prepare(
            self.repository,
            &self.lease,
            job.version,
            &manifest,
            &transfer_root,
            &self.clock,
        )?;
        self.store.commit(self.repository, &self.lease, &self.clock)
    }
}

pub fn install_from_local_cache(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    request: &CacheInstallRequest,
    worker_name: &str,
    clock: impl Fn() -> i64,
) -> Result<InstalledVersion> {
    ensure!(
        !request.installation_id.trim().is_empty() && !request.version_id.trim().is_empty(),
        "Cache installation has no installation or version identity"
    );
    let _transfer_lock = store.transfer_lock(&request.job_id)?;
    let job = repository
        .read_job(&request.job_id)?
        .context("Cache installation job not found")?;
    if job.status == JobStatus::Cancelling {
        repository.transition(&job.id, job.version, JobStatus::Cancelled, clock())?;
        return Err(ModelSourceError {
            code: LifecycleErrorCode::Cancelled,
            http_status: None,
        }
        .into());
    }
    if job.status == JobStatus::Completed {
        let journal = repository
            .installation_commit(&job.id)?
            .context("Completed cache job has no installation commit")?;
        ensure!(
            journal.stage == InstallationCommitStage::Registered
                && journal.manifest.installation_id == request.installation_id
                && journal.manifest.version_id == request.version_id
                && journal.manifest.plan.digest()? == job.plan_digest,
            "Completed cache installation belongs to a different request"
        );
        let installed = repository
            .installed_version(&request.version_id)?
            .context("Completed cache installation version is missing")?;
        ensure!(
            installed.manifest_digest == journal.manifest_digest,
            "Completed cache installation has a different manifest"
        );
        store.verify_installed_version(&installed)?;
        return Ok(installed);
    }
    let plan = repository
        .read_plan(&job.plan_id)?
        .context("Cache installation plan is missing")?;
    ensure!(
        plan.rules_version == PREPARED_GGUF_PLAN_RULES_VERSION && plan.digest()? == job.plan_digest,
        "Cache import requires the accepted frozen installation plan"
    );
    plan.validate_for_install(job.accepted_at, fs2::available_space(store.root())?)?;
    ensure!(
        plan.artifacts.keys().eq(request.source_files.keys()),
        "Cache source selection does not match the installation plan"
    );
    ensure!(plan.artifacts.values().all(|artifact| artifact.source_sha256.is_some()), "Local cache import requires pinned source checksums; unproven historical files require migration review");
    let lease = repository.claim(&job.id, job.version, worker_name, clock(), 120)?;
    let execution = CacheExecution {
        repository,
        store,
        lease,
        clock,
    };
    let result = execution.install(request, plan);
    if result.is_err() {
        let job = repository
            .read_job(&request.job_id)?
            .context("Failed cache job disappeared")?;
        if job.status == JobStatus::Cancelling {
            repository.transition(
                &job.id,
                job.version,
                JobStatus::Cancelled,
                (execution.clock)(),
            )?;
        } else if LifecycleRepository::validate_lease(&job, &execution.lease, (execution.clock)())
            .is_ok()
        {
            repository.transition(&job.id, job.version, JobStatus::Failed, (execution.clock)())?;
        }
    }
    result
}
