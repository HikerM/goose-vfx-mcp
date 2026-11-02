use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::{
    resume_installation, InstallationSource, InstallationStore, LifecycleErrorCode,
    LifecycleRepository, ModelSourceError,
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
        let intent = match repository.read_installation_input(&job.id) {
            Ok(Some(intent)) => intent,
            Ok(None) => {
                outcomes.push(RecoveryOutcome {
                    job_id: job.id,
                    disposition: RecoveryDisposition::NeedsInputAdoption,
                });
                continue;
            }
            Err(_) => {
                outcomes.push(RecoveryOutcome {
                    job_id: job.id,
                    disposition: RecoveryDisposition::InvalidInput,
                });
                continue;
            }
        };
        if intent.input.store_root != store.root() {
            outcomes.push(RecoveryOutcome {
                job_id: job.id,
                disposition: RecoveryDisposition::DifferentStore,
            });
            continue;
        }
        let token = if matches!(intent.input.source, InstallationSource::ModelScope {}) {
            let provider = Arc::clone(&credentials);
            match tokio::task::spawn_blocking(move || provider()).await {
                Ok(Ok(token)) => token,
                _ => {
                    outcomes.push(RecoveryOutcome {
                        job_id: job.id,
                        disposition: RecoveryDisposition::CredentialsUnavailable,
                    });
                    continue;
                }
            }
        } else {
            None
        };
        let result = resume_installation(
            Arc::clone(&repository),
            Arc::clone(&store),
            job.id.clone(),
            token,
            request.worker_name.clone(),
            clock.clone(),
        )
        .await;
        let disposition = match result {
            Ok(installed) => RecoveryDisposition::Completed {
                version_id: installed.manifest.version_id,
            },
            Err(error) => {
                let code = error
                    .downcast_ref::<ModelSourceError>()
                    .map_or(LifecycleErrorCode::InstallFailed, |source| source.code);
                if code == LifecycleErrorCode::Cancelled {
                    RecoveryDisposition::Cancelled
                } else {
                    RecoveryDisposition::Failed { code }
                }
            }
        };
        outcomes.push(RecoveryOutcome {
            job_id: job.id,
            disposition,
        });
    }
    Ok(RecoveryPage {
        outcomes,
        next_after_id,
    })
}
