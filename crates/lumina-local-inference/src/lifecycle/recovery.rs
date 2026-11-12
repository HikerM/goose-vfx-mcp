use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::{
    resume_installation_with_control, InstallationFailureDetails, InstallationFailureOrigin,
    InstallationSource, InstallationStore, InstallationWorkerControl, JobStatus,
    LifecycleErrorCode, LifecycleRepository,
};

pub type InstallationCredentials = Arc<dyn Fn() -> Result<Option<String>> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct RecoveryPageRequest {
    pub after_id: Option<String>,
    pub limit: u32,
    pub worker_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RecoveryDisposition {
    Completed { version_id: String },
    Cancelled,
    NeedsInputAdoption,
    DifferentStore,
    InvalidInput,
    CredentialsUnavailable,
    Failed { code: LifecycleErrorCode },
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryOutcome {
    pub job_id: String,
    pub disposition: RecoveryDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryPage {
    pub outcomes: Vec<RecoveryOutcome>,
    pub next_after_id: Option<String>,
}

pub async fn recover_installations_page<F>(
    repository: Arc<LifecycleRepository>,
    store: Arc<InstallationStore>,
    request: RecoveryPageRequest,
    credentials: InstallationCredentials,
    clock: F,
) -> Result<RecoveryPage>
where
    F: Fn() -> i64 + Clone + Send + 'static,
{
    ensure!(
        !request.worker_name.trim().is_empty(),
        "Recovery worker identity is missing"
    );
    let jobs = repository.recoverable_jobs(clock(), request.after_id.as_deref(), request.limit)?;
    let next_after_id = (jobs.len() == request.limit as usize)
        .then(|| jobs.last().expect("Recovery page is nonempty").id.clone());
    let mut outcomes = Vec::with_capacity(jobs.len());
    for job in jobs {
        let Some(_permit) = store.try_schedule(&job.id)? else {
            outcomes.push(RecoveryOutcome {
                job_id: job.id,
                disposition: RecoveryDisposition::Interrupted,
            });
            continue;
        };
        outcomes.push(
            recover_installation_job(
                Arc::clone(&repository),
                Arc::clone(&store),
                job.id,
                request.worker_name.clone(),
                Arc::clone(&credentials),
                clock.clone(),
                Arc::new(InstallationWorkerControl::default()),
            )
            .await?,
        );
    }
    Ok(RecoveryPage {
        outcomes,
        next_after_id,
    })
}

pub(crate) async fn recover_installation_job<F>(
    repository: Arc<LifecycleRepository>,
    store: Arc<InstallationStore>,
    job_id: String,
    worker_name: String,
    credentials: InstallationCredentials,
    clock: F,
    control: Arc<InstallationWorkerControl>,
) -> Result<RecoveryOutcome>
where
    F: Fn() -> i64 + Clone + Send + 'static,
{
    control.checkpoint()?;
    let outcome = |disposition| {
        Ok(RecoveryOutcome {
            job_id: job_id.clone(),
            disposition,
        })
    };
    let observed = repository
        .read_job(&job_id)?
        .context("Installation job not found")?;
    if !(matches!(observed.status, JobStatus::Queued | JobStatus::Cancelling)
        || (observed.status == JobStatus::Running
            && observed.lease_until.is_some_and(|until| until <= clock())))
    {
        return outcome(RecoveryDisposition::Interrupted);
    }
    let intent = match repository.read_installation_input(&job_id) {
        Ok(Some(intent)) => intent,
        Ok(None) => {
            return outcome(RecoveryDisposition::NeedsInputAdoption);
        }
        Err(_) => {
            return outcome(RecoveryDisposition::InvalidInput);
        }
    };
    if intent.input.store_root != store.root() {
        return outcome(RecoveryDisposition::DifferentStore);
    }
    let token = if observed.status != JobStatus::Cancelling
        && matches!(intent.input.source, InstallationSource::ModelScope {})
    {
        let provider = Arc::clone(&credentials);
        match tokio::task::spawn_blocking(move || provider()).await {
            Ok(Ok(token)) => token,
            _ => {
                control.checkpoint()?;
                let recorded = repository.block_installation(
                    &observed,
                    &store,
                    InstallationFailureDetails::operation(
                        LifecycleErrorCode::SourceAuthRequired,
                        InstallationFailureOrigin::Credentials,
                    ),
                    clock(),
                )?;
                if recorded.is_some() {
                    repository.release_inactive_storage(&job_id, &store, clock())?;
                }
                return outcome(if recorded.is_some() {
                    RecoveryDisposition::CredentialsUnavailable
                } else {
                    RecoveryDisposition::Interrupted
                });
            }
        }
    } else {
        None
    };
    let result = resume_installation_with_control(
        Arc::clone(&repository),
        Arc::clone(&store),
        job_id.clone(),
        token,
        worker_name,
        clock.clone(),
        control,
    )
    .await;
    let disposition = match result {
        Ok(installed) => RecoveryDisposition::Completed {
            version_id: installed.manifest.version_id,
        },
        Err(error) => {
            if error.is::<super::worker_control::InstallationInterrupted>() {
                return outcome(RecoveryDisposition::Interrupted);
            }
            let code = super::failure::installation_failure_code(&error);
            if code == LifecycleErrorCode::Cancelled {
                RecoveryDisposition::Cancelled
            } else {
                RecoveryDisposition::Failed { code }
            }
        }
    };
    outcome(disposition)
}
