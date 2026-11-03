use super::*;
use anyhow::{Context, Result};
use lumina_sdk_types::custom_requests::*;

macro_rules! map_enum {
    ($source:ident,$target:ident,$($variant:ident),+)=>{
        impl From<$source> for $target {
            fn from(value:$source)->Self{match value{$($source::$variant=>$target::$variant,)+}}
        }
    }
}
map_enum!(
    JobStatus,
    ModelInstallationJobStatus,
    Queued,
    Running,
    Paused,
    Cancelling,
    Cancelled,
    Completed,
    Blocked,
    Failed
);
map_enum!(
    LifecycleStage,
    ModelInstallationStage,
    Discovered,
    RepositoryVerified,
    FilesDiscovered,
    ModelClassified,
    DependenciesResolved,
    CompatibilityChecked,
    ReadyToInstall,
    Downloading,
    Verifying,
    Installing,
    Installed,
    Starting,
    Validating
);
map_enum!(
    InstallationApplicationState,
    ModelInstallationServiceState,
    Dormant,
    Recovering,
    Ready,
    Stopped,
    Failed
);
map_enum!(
    ModelInstallationJobAction,
    InstallationJobAction,
    Pause,
    Resume,
    Cancel,
    Retry
);
map_enum!(
    RetryHistoryOrigin,
    ModelInstallationRetryHistoryOrigin,
    FromEnqueue,
    LegacyUntracked
);
map_enum!(
    InstallationFailureOrigin,
    ModelInstallationFailureOrigin,
    Credentials,
    Admission,
    Transfer,
    Verification,
    Commit
);
map_enum!(
    StorageReservationState,
    ModelInstallationStorageState,
    NeedsAdmission,
    Held,
    Released
);
map_enum!(
    FileRestartReason,
    ModelInstallationFileRestartReason,
    RangeUnavailable,
    ValidatorChanged,
    PartialContentMismatch,
    HashMismatch
);

impl From<FileTransferEvent> for ModelInstallationFileEventDto {
    fn from(event: FileTransferEvent) -> Self {
        use ModelInstallationFileStatus as Status;
        let file = event.file;
        let (status, failure_code) = match file.state {
            FileTransferState::Pending => (Status::Pending, None),
            FileTransferState::Downloading => (Status::Downloading, None),
            FileTransferState::Downloaded => (Status::Downloaded, None),
            FileTransferState::Verified { .. } => (Status::Verified, None),
            FileTransferState::Failed { code } => (Status::Failed, Some(code.into())),
        };
        Self {
            sequence: event.sequence,
            artifact_id: file.artifact_id,
            version: file.version,
            generation: file.generation,
            completed_bytes: file.checkpoint.completed_bytes,
            expected_bytes: file.expected_bytes,
            status,
            failure_code,
            restart_reason: file.restart_reason.map(Into::into),
            updated_at: file.updated_at,
        }
    }
}

impl From<StorageReservation> for ModelInstallationStorageDto {
    fn from(reservation: StorageReservation) -> Self {
        Self {
            job_id: reservation.job_id,
            plan_digest: reservation.plan_digest.as_str().into(),
            volume_id: reservation.volume_id,
            peak_bytes: reservation.peak_bytes,
            reserved_bytes: reservation.reserved_bytes,
            state: reservation.state.into(),
            version: reservation.version,
            created_at: reservation.created_at,
            updated_at: reservation.updated_at,
        }
    }
}
map_enum!(
    LifecycleErrorCode,
    ModelInstallationFailureCode,
    SourceNotFound,
    SourceAuthRequired,
    SourceForbidden,
    SourceTimeout,
    SourceRateLimited,
    SourceUnavailable,
    SourcePartialFailure,
    RevisionUnresolved,
    RevisionChanged,
    NoGguf,
    GgufInvalid,
    FormatForbidden,
    ShardIncomplete,
    DependencyMissing,
    DependencyIncompatible,
    DependencyUnverified,
    RuntimeUnsupported,
    HardwareInsufficient,
    DiskInsufficient,
    HashMismatch,
    DestinationLocked,
    PlanStale,
    StateConflict,
    DownloadFailed,
    InstallFailed,
    StartFailed,
    ValidationFailed,
    Cancelled
);

impl From<FileRetryBudget> for ModelInstallationFileRetryDto {
    fn from(budget: FileRetryBudget) -> Self {
        let next_retry_at = budget.next_retry_at();
        let state = match budget.state {
            FileRetryState::Ready => ModelInstallationFileRetryState::Ready,
            FileRetryState::Active => ModelInstallationFileRetryState::Active,
            FileRetryState::Waiting { .. } => ModelInstallationFileRetryState::Waiting,
            FileRetryState::Exhausted => ModelInstallationFileRetryState::Exhausted,
            FileRetryState::NeedsAction => ModelInstallationFileRetryState::NeedsAction,
            FileRetryState::Verified => ModelInstallationFileRetryState::Verified,
        };
        Self {
            job_id: budget.job_id,
            artifact_id: budget.artifact_id,
            policy_version: budget.policy_version,
            max_attempts: budget.max_attempts,
            cycle: budget.cycle,
            cycle_attempts: budget.cycle_attempts,
            recorded_attempts: budget.recorded_attempts,
            history_origin: budget.history_origin.into(),
            tracking_started_at: budget.tracking_started_at,
            state,
            next_retry_at,
            last_failure: budget
                .last_failure
                .map(|failure| ModelInstallationFileFailureDto {
                    code: failure.code.into(),
                    stage: failure.stage.into(),
                    failed_at: failure.failed_at,
                    retry_after_seconds: failure.retry_after_seconds,
                }),
            version: budget.version,
            updated_at: budget.updated_at,
        }
    }
}

impl From<LifecycleJob> for ModelInstallationJobDto {
    fn from(job: LifecycleJob) -> Self {
        Self {
            id: job.id,
            plan_id: job.plan_id,
            plan_digest: job.plan_digest.as_str().into(),
            stage: job.stage.into(),
            status: job.status.into(),
            version: job.version,
            generation: job.generation,
            accepted_at: job.accepted_at,
            updated_at: job.updated_at,
        }
    }
}

impl InstallationApplication {
    pub async fn submit_request(
        &self,
        request: ModelInstallationSubmitRequest,
    ) -> Result<ModelInstallationSubmitResponse> {
        let service = self.service().await?;
        let submission = InstallationSubmission {
            plan_id: request.plan_id,
            plan_digest: Sha256Digest::parse(request.plan_digest)?,
            installation_id: request.installation_id,
            idempotency_key: request.idempotency_key,
        };
        let job = service.submit(&submission).await?;
        let input = service
            .repository()
            .read_installation_input(&job.id)?
            .context("Accepted installation input is missing")?;
        Ok(ModelInstallationSubmitResponse {
            job: job.into(),
            version_id: input.input.version_id,
        })
    }
    pub async fn list_request(
        &self,
        request: ModelInstallationJobsListRequest,
    ) -> Result<ModelInstallationJobsListResponse> {
        anyhow::ensure!(
            (1..=1000).contains(&request.limit),
            "Job page size must be 1..=1000"
        );
        let Some(repository) = self.query_repository().await? else {
            return Ok(ModelInstallationJobsListResponse {
                jobs: Vec::new(),
                file_retries: Vec::new(),
                failures: Vec::new(),
                storage_reservations: Vec::new(),
                next_after_id: None,
                service_state: self.state().into(),
            });
        };
        let jobs = repository.list_jobs(request.after_id.as_deref(), request.limit)?;
        let next_after_id = (jobs.len() == request.limit as usize)
            .then(|| jobs.last().expect("Job page is nonempty").id.clone());
        let mut file_retries = Vec::new();
        let mut failures = Vec::new();
        let mut storage_reservations = Vec::new();
        for job in &jobs {
            if let Some(reservation) = repository.storage_reservation(&job.id)? {
                storage_reservations.push(reservation.into());
            }
            file_retries.extend(
                repository
                    .file_retry_budgets(&job.id)?
                    .into_iter()
                    .map(Into::into),
            );
            if let Some(record) = repository.latest_installation_failure(&job.id)? {
                failures.push(ModelInstallationFailureDto {
                    job_id: job.id.clone(),
                    job_version: record.job.version,
                    code: record.details.code.into(),
                    stage: record.job.stage.into(),
                    origin: record.details.origin.into(),
                    artifact_id: record.details.artifact_id,
                    message_key: record.failure.message_key,
                    message_args: record.failure.message_args,
                    retryable: record.details.code.retryable(),
                    recoverable: record.failure.recoverable,
                    cause_id: record
                        .failure
                        .cause_id
                        .context("Installation failure has no cause identity")?,
                    automatic_retry_at: record.details.automatic_retry_at,
                    is_current: *job == record.job,
                    occurred_at: record.occurred_at,
                });
            }
        }
        Ok(ModelInstallationJobsListResponse {
            jobs: jobs.into_iter().map(Into::into).collect(),
            file_retries,
            failures,
            storage_reservations,
            next_after_id,
            service_state: self.state().into(),
        })
    }
    async fn query_repository(&self) -> Result<Option<std::sync::Arc<LifecycleRepository>>> {
        if let Some(service) = self.started_service() {
            return Ok(Some(std::sync::Arc::clone(service.repository())));
        }
        let path = self.root().join("models.db");
        if !path.try_exists()? {
            return Ok(None);
        }
        tokio::task::spawn_blocking(move || {
            LifecycleRepository::open(&path).map(|repository| Some(std::sync::Arc::new(repository)))
        })
        .await
        .context("Installation job query failed")?
    }

    pub async fn events_request(
        &self,
        request: ModelInstallationJobEventsRequest,
    ) -> Result<ModelInstallationJobEventsResponse> {
        anyhow::ensure!(
            !request.job_id.is_empty(),
            "Installation job ID is required"
        );
        anyhow::ensure!(
            (1..=256).contains(&request.limit),
            "Event page size must be 1..=256"
        );
        anyhow::ensure!(
            request.after_job_sequence <= i64::MAX as u64
                && request.after_file_sequence <= i64::MAX as u64,
            "Event sequence exceeds the database range"
        );
        let repository = self.query_repository().await?;
        tokio::task::spawn_blocking(move || {
            let (mut jobs, mut files) = match repository {
                Some(repository) => (
                    repository.events(
                        &request.job_id,
                        request.after_job_sequence,
                        request.limit + 1,
                    )?,
                    repository.file_transfer_events(
                        &request.job_id,
                        request.after_file_sequence,
                        request.limit + 1,
                    )?,
                ),
                None => (Vec::new(), Vec::new()),
            };
            let limit = request.limit as usize;
            let has_more_job_events = jobs.len() > limit;
            let has_more_file_events = files.len() > limit;
            jobs.truncate(limit);
            files.truncate(limit);
            Ok(ModelInstallationJobEventsResponse {
                next_job_sequence: jobs
                    .last()
                    .map_or(request.after_job_sequence, |event| event.sequence),
                next_file_sequence: files
                    .last()
                    .map_or(request.after_file_sequence, |event| event.sequence),
                has_more_job_events,
                has_more_file_events,
                job_events: jobs
                    .into_iter()
                    .map(|event| ModelInstallationJobEventDto {
                        sequence: event.sequence,
                        job: event.job.into(),
                    })
                    .collect(),
                file_events: files.into_iter().map(Into::into).collect(),
            })
        })
        .await
        .context("Installation event query failed")?
    }

    pub async fn control_request(
        &self,
        request: ModelInstallationJobControlRequest,
    ) -> Result<ModelInstallationJobControlResponse> {
        let service = self.service().await?;
        let result = service.control_job(&InstallationControlRequest {
            job_id: request.job_id,
            expected_version: request.expected_version,
            action: request.action.into(),
            idempotency_key: request.idempotency_key,
        })?;
        Ok(ModelInstallationJobControlResponse {
            job: result.job.into(),
            replayed: result.replayed,
        })
    }
}
