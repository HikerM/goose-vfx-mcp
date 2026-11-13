use super::worker_control::InstallationInterrupted;
use super::InstallationWorkerControl;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    build_installation_manifest, FileRestartReason, FileTransferRecord, FileTransferState,
    InstallPlan, InstallationCommitStage, InstallationStore, InstalledVersion, JobStatus,
    LifecycleErrorCode, LifecycleJob, LifecycleRepository, LifecycleStage, ModelSourceError,
    ResumeValidator, Sha256Digest, TransferCheckpoint, WorkerLease,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheInstallRequest {
    pub job_id: String,
    pub installation_id: String,
    pub version_id: String,
    pub source_files: BTreeMap<String, PathBuf>,
}

pub(crate) fn replay_completed_installation(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    job: &LifecycleJob,
    installation_id: &str,
    version_id: &str,
) -> Result<InstalledVersion> {
    let journal = repository
        .installation_commit(&job.id)?
        .context("Completed job has no installation commit")?;
    ensure!(
        journal.stage == InstallationCommitStage::Registered
            && journal.manifest.installation_id == installation_id
            && journal.manifest.version_id == version_id
            && journal.manifest.plan.digest()? == job.plan_digest,
        "Completed installation belongs to a different request"
    );
    let installed = repository
        .installed_version(version_id)?
        .context("Completed installation version is missing")?;
    ensure!(
        installed.manifest_digest == journal.manifest_digest,
        "Completed installation has a different manifest"
    );
    store.verify_installed_version(&installed)?;
    Ok(installed)
}

pub(crate) struct InstallExecution<'a, F> {
    pub(crate) repository: &'a LifecycleRepository,
    pub(crate) store: &'a InstallationStore,
    pub(crate) lease: WorkerLease,
    pub(crate) clock: F,
    pub(crate) control: Arc<InstallationWorkerControl>,
}

impl<F: Fn() -> i64> InstallExecution<'_, F> {
    pub(crate) fn checkpoint(&self) -> Result<LifecycleJob> {
        self.control.checkpoint()?;
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

    pub(crate) fn advance(&self, stage: LifecycleStage) -> Result<LifecycleJob> {
        let job = self.checkpoint()?;
        self.repository
            .advance(&self.lease, job.version, stage, (self.clock)())
    }

    pub(crate) fn hash_prefix(&self, file: &mut File, bytes: u64) -> Result<Sha256> {
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

    pub(crate) fn copy_runtime_spec(
        &self,
        plan: &InstallPlan,
        record: &FileTransferRecord,
        profile: super::AudioTaskProfile,
    ) -> Result<()> {
        let artifact = &plan.artifacts[&record.artifact_id];
        ensure!(
            profile.matches_spec_artifact(artifact),
            "Runtime spec is not pinned to this installation"
        );
        let bytes = profile.runtime_spec_bytes()?;
        let path = self
            .store
            .transfer_file_path(&self.lease.job_id, &record.relative_path)?;
        let mut target = super::files::open_exclusive_transfer(&path)?;
        self.checkpoint()?;
        // A packaged spec is at most 16 KiB, so its only durable checkpoint is the complete file.
        let record = if record.checkpoint.completed_bytes > 0
            || matches!(record.state, FileTransferState::Failed { .. })
        {
            self.repository.restart_file(
                &self.lease,
                &record.artifact_id,
                record.version,
                FileRestartReason::PartialContentMismatch,
                (self.clock)(),
            )?
        } else {
            record.clone()
        };
        self.repository.require_installation_space(
            &self.lease.job_id,
            self.store,
            bytes.len() as u64,
        )?;
        target.set_len(0)?;
        target.rewind()?;
        target.write_all(bytes)?;
        super::worker_control::sync_with_lease(
            || target.sync_all(),
            || self.checkpoint().map(|_| ()),
        )?;
        self.repository.checkpoint_synced_file(
            &self.lease,
            &record.artifact_id,
            record.version,
            TransferCheckpoint {
                completed_bytes: bytes.len() as u64,
                prefix_sha256: Sha256Digest::of_bytes(bytes),
                resume_validator: ResumeValidator::Unknown,
            },
            (self.clock)(),
        )?;
        Ok(())
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
            self.repository.require_installation_space(
                &self.lease.job_id,
                self.store,
                count as u64,
            )?;
            output.write_all(&buffer[..count])?;
            hasher.update(&buffer[..count]);
            if completed.saturating_sub(persisted) >= 64 * 1024 * 1024
                && completed < record.expected_bytes
            {
                super::worker_control::sync_with_lease(
                    || output.sync_data(),
                    || self.checkpoint().map(|_| ()),
                )?;
                record = self.repository.checkpoint_synced_file(
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
        super::worker_control::sync_with_lease(
            || output.sync_all(),
            || self.checkpoint().map(|_| ()),
        )?;
        self.repository.checkpoint_synced_file(
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

    pub(crate) fn record_file_failure(
        &self,
        artifact_id: &str,
        error: &anyhow::Error,
    ) -> Result<()> {
        if error.is::<InstallationInterrupted>() {
            return Ok(());
        }
        let job = self
            .repository
            .read_job(&self.lease.job_id)?
            .context("Cache job disappeared")?;
        if LifecycleRepository::validate_lease(&job, &self.lease, (self.clock)()).is_ok() {
            let record = self
                .repository
                .read_file_transfer(&job.id, artifact_id)?
                .context("Cache file disappeared")?;
            let code = super::failure::installation_failure_code(error);
            self.repository.fail_installation_file(
                &self.lease,
                artifact_id,
                record.version,
                super::FileAttemptFailure {
                    code,
                    retry_after_seconds: error
                        .downcast_ref::<lumina_download_manager::managed::ManagedTransportError>()
                        .and_then(|transport| transport.retry_after_seconds),
                },
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
                self.repository.begin_file_attempt(
                    &self.lease,
                    &file.artifact_id,
                    (self.clock)(),
                )?;
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
                } else if let super::FilePurpose::RuntimeDistributionConfig(profile) =
                    plan.artifacts[&file.artifact_id].purpose
                {
                    self.copy_runtime_spec(&plan, &file, profile)
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
            self.advance(LifecycleStage::Verifying)?;
        }
        self.finish_installation(&request.installation_id, &request.version_id, plan)
    }

    pub(crate) fn finish_installation(
        &self,
        installation_id: &str,
        version_id: &str,
        plan: InstallPlan,
    ) -> Result<InstalledVersion> {
        let mut job = self.checkpoint()?;
        let transfer_root = self.store.transfer_directory(&self.lease.job_id)?;
        if job.stage == LifecycleStage::Verifying {
            for file in self.repository.file_transfers(&self.lease.job_id)? {
                self.checkpoint()?;
                self.repository.begin_file_attempt(
                    &self.lease,
                    &file.artifact_id,
                    (self.clock)(),
                )?;
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
            if let Some(journal) = self.repository.installation_commit(&self.lease.job_id)? {
                ensure!(
                    journal.manifest.installation_id == installation_id
                        && journal.manifest.version_id == version_id,
                    "Recovered installation belongs to different requested identities"
                );
                journal.manifest
            } else {
                build_installation_manifest(
                    installation_id.to_string(),
                    version_id.to_string(),
                    plan,
                    &self.repository.file_transfers(&self.lease.job_id)?,
                    job.accepted_at,
                    (self.clock)(),
                )?
            };
        let job = self.checkpoint()?;
        self.store.prepare_with_control(
            self.repository,
            &self.lease,
            job.version,
            &manifest,
            &transfer_root,
            (&self.clock, &self.control),
        )?;
        self.store
            .commit_with_control(self.repository, &self.lease, &self.clock, &self.control)
    }

    pub(crate) fn settle_failure(&self, error: &anyhow::Error) -> Result<()> {
        let job = self
            .repository
            .read_job(&self.lease.job_id)?
            .context("Failed installation job disappeared")?;
        if job.status == JobStatus::Cancelling {
            self.repository.transition(
                &job.id,
                job.version,
                JobStatus::Cancelled,
                (self.clock)(),
            )?;
        } else if self.control.is_stopping() {
            self.repository
                .release_worker_for_recovery(&self.lease, (self.clock)())?;
        } else if LifecycleRepository::validate_lease(&job, &self.lease, (self.clock)()).is_ok() {
            self.repository.fail_installation_operation(
                &self.lease,
                job.version,
                super::InstallationFailureDetails::operation(
                    super::failure::installation_failure_code(error),
                    super::InstallationFailureOrigin::for_stage(job.stage),
                ),
                (self.clock)(),
            )?;
        }
        Ok(())
    }
}

pub fn install_from_local_cache(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    request: &CacheInstallRequest,
    worker_name: &str,
    clock: impl Fn() -> i64,
) -> Result<InstalledVersion> {
    install_from_local_cache_controlled(
        repository,
        store,
        request,
        worker_name,
        clock,
        Arc::new(InstallationWorkerControl::default()),
    )
}

pub(crate) fn install_from_local_cache_controlled(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    request: &CacheInstallRequest,
    worker_name: &str,
    clock: impl Fn() -> i64,
    control: Arc<InstallationWorkerControl>,
) -> Result<InstalledVersion> {
    let result = (|| {
        control.checkpoint()?;
        ensure!(
            !request.installation_id.trim().is_empty() && !request.version_id.trim().is_empty(),
            "Cache installation has no installation or version identity"
        );
        let _transfer_lock = store.transfer_lock(&request.job_id)?;
        let job = repository
            .read_job(&request.job_id)?
            .context("Cache installation job not found")?;
        if let Some(intent) = repository.read_installation_input(&job.id)? {
            intent.input.validate_destination(
                store,
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
                repository,
                store,
                &job,
                &request.installation_id,
                &request.version_id,
            );
        }
        let plan = repository
            .read_plan(&job.plan_id)?
            .context("Cache installation plan is missing")?;
        ensure!(
            plan.is_managed_install_plan() && plan.digest()? == job.plan_digest,
            "Cache import requires the accepted frozen installation plan"
        );
        super::failure::validate_installation_store_admission(
            repository,
            store,
            &job,
            &plan,
            clock(),
        )?;
        let input = super::InstallationInput::new(
            store,
            request.installation_id.clone(),
            request.version_id.clone(),
            super::InstallationSource::LocalCache {
                files: request.source_files.clone(),
            },
        )?;
        input.validate_plan(&plan)?;
        repository.adopt_installation_input(&job.id, &input, clock())?;
        let lease = repository.claim(&job.id, job.version, worker_name, clock(), 120)?;
        let execution = InstallExecution {
            repository,
            store,
            lease,
            clock: &clock,
            control,
        };
        let result = execution.install(request, plan);
        if let Err(error) = &result {
            execution.settle_failure(error)?;
        }
        result
    })();
    repository.release_inactive_storage(&request.job_id, store, clock())?;
    result
}
