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
        if !self.root().join("models.db").try_exists()? {
            return Ok(ModelInstallationJobsListResponse {
                jobs: Vec::new(),
                file_retries: Vec::new(),
                next_after_id: None,
                service_state: self.state().into(),
            });
        }
        let repository = if let Some(service) = self.started_service() {
            std::sync::Arc::clone(service.repository())
        } else {
            let path = self.root().join("models.db");
            tokio::task::spawn_blocking(move || {
                LifecycleRepository::open(&path).map(std::sync::Arc::new)
            })
            .await
            .context("Installation job query failed")??
        };
        let jobs = repository.list_jobs(request.after_id.as_deref(), request.limit)?;
        let next_after_id = (jobs.len() == request.limit as usize)
            .then(|| jobs.last().expect("Job page is nonempty").id.clone());
        let mut file_retries = Vec::new();
        for job in &jobs {
            file_retries.extend(
                repository
                    .file_retry_budgets(&job.id)?
                    .into_iter()
                    .map(Into::into),
            );
        }
        Ok(ModelInstallationJobsListResponse {
            jobs: jobs.into_iter().map(Into::into).collect(),
            file_retries,
            next_after_id,
            service_state: self.state().into(),
        })
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
