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
        Ok(ModelInstallationJobsListResponse {
            jobs: jobs.into_iter().map(Into::into).collect(),
            next_after_id,
            service_state: self.state().into(),
        })
    }
    pub async fn control_request(
        &self,
        request: ModelInstallationJobControlRequest,
    ) -> Result<ModelInstallationJobControlResponse> {
        let service = self.service().await?;
        let job = service.control_job(
            &request.job_id,
            request.expected_version,
            request.action.into(),
        )?;
        Ok(ModelInstallationJobControlResponse { job: job.into() })
    }
}
