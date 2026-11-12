use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OnceCell;

use super::{
    InstallationCredentials, InstallationService, InstallationServiceState, InstallationStore,
    LifecycleRepository,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationApplicationState {
    Dormant,
    Recovering,
    Ready,
    Stopped,
    Failed,
}

pub struct InstallationApplication {
    root: PathBuf,
    credentials: InstallationCredentials,
    service: OnceCell<Arc<InstallationService>>,
    initialization_failed: AtomicBool,
}

impl InstallationApplication {
    pub fn new(data_dir: &Path, credentials: InstallationCredentials) -> Self {
        Self {
            root: data_dir.join("model-lifecycle"),
            credentials,
            service: OnceCell::new(),
            initialization_failed: AtomicBool::new(false),
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub(super) async fn modelscope_credentials(&self) -> Result<Option<String>> {
        let credentials = Arc::clone(&self.credentials);
        tokio::task::spawn_blocking(move || credentials())
            .await
            .map_err(|_| super::ModelSourceError {
                code: super::LifecycleErrorCode::SourceAuthRequired,
                http_status: None,
            })?
            .map_err(|_| {
                super::ModelSourceError {
                    code: super::LifecycleErrorCode::SourceAuthRequired,
                    http_status: None,
                }
                .into()
            })
    }
    pub(crate) fn started_service(&self) -> Option<&Arc<InstallationService>> {
        self.service.get()
    }
    pub fn state(&self) -> InstallationApplicationState {
        if let Some(service) = self.service.get() {
            match service.state() {
                InstallationServiceState::Recovering => InstallationApplicationState::Recovering,
                InstallationServiceState::Ready => InstallationApplicationState::Ready,
                InstallationServiceState::Stopped => InstallationApplicationState::Stopped,
                InstallationServiceState::Failed => InstallationApplicationState::Failed,
            }
        } else if self.initialization_failed.load(Ordering::Acquire) {
            InstallationApplicationState::Failed
        } else {
            InstallationApplicationState::Dormant
        }
    }
    pub async fn start_if_present(&self) -> Result<()> {
        let result = async {
            if self.root.join("models.db").try_exists()? {
                self.service().await?;
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            self.initialization_failed.store(true, Ordering::Release);
        }
        result
    }
    pub async fn service(&self) -> Result<Arc<InstallationService>> {
        let result = self
            .service
            .get_or_try_init(|| async {
                let root = self.root.clone();
                let (repository, store) = tokio::task::spawn_blocking(move || -> Result<_> {
                    let store = Arc::new(InstallationStore::open(&root)?);
                    let repository = Arc::new(LifecycleRepository::open(&root.join("models.db"))?);
                    Ok((repository, store))
                })
                .await
                .map_err(|_| anyhow!("Installation storage initialization failed"))??;
                InstallationService::start(
                    repository,
                    store,
                    Arc::clone(&self.credentials),
                    Arc::new(|| chrono::Utc::now().timestamp()),
                    Duration::from_secs(5),
                )
            })
            .await
            .cloned();
        self.initialization_failed
            .store(result.is_err(), Ordering::Release);
        result
    }
    pub async fn shutdown(&self) -> Result<()> {
        if let Some(service) = self.service.get() {
            service.shutdown().await?;
        }
        Ok(())
    }
}
