use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

use super::{
    InstallationControlOutcome, InstallationControlRequest, InstallationCredentials,
    InstallationStore, InstallationSubmission, InstallationWorkerControl, LifecycleJob,
    LifecycleRepository,
};

pub type InstallationClock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationServiceState {
    Recovering,
    Ready,
    Stopped,
    Failed,
}

struct ServiceWorker {
    repository: Arc<LifecycleRepository>,
    store: Arc<InstallationStore>,
    credentials: InstallationCredentials,
    clock: InstallationClock,
    wake: Arc<Notify>,
    control: Arc<InstallationWorkerControl>,
    state: watch::Sender<InstallationServiceState>,
    owner: String,
    poll_interval: Duration,
}

impl ServiceWorker {
    async fn run(&self) -> Result<()> {
        loop {
            let mut storage_cursor = None;
            loop {
                if self.control.is_stopping() {
                    return Ok(());
                }
                let page = self.repository.reconcile_installation_storage(
                    &self.store,
                    (self.clock)(),
                    storage_cursor.as_deref(),
                    64,
                )?;
                storage_cursor = page.next_after_id;
                if storage_cursor.is_none() {
                    break;
                }
            }
            let mut cursor = None;
            loop {
                if self.control.is_stopping() {
                    return Ok(());
                }
                let jobs =
                    self.repository
                        .recoverable_jobs((self.clock)(), cursor.as_deref(), 64)?;
                let full = jobs.len() == 64;
                for job in jobs {
                    if self.control.is_stopping() {
                        return Ok(());
                    }
                    cursor = Some(job.id.clone());
                    let clock = Arc::clone(&self.clock);
                    let result = super::recovery::recover_installation_job(
                        Arc::clone(&self.repository),
                        Arc::clone(&self.store),
                        job.id,
                        self.owner.clone(),
                        Arc::clone(&self.credentials),
                        move || clock(),
                        Arc::clone(&self.control),
                    )
                    .await;
                    if let Err(error) = result {
                        if !self.control.is_stopping() {
                            return Err(error);
                        }
                    }
                }
                if !full {
                    break;
                }
            }
            if self.control.is_stopping() {
                return Ok(());
            }
            self.state.send_replace(InstallationServiceState::Ready);
            tokio::select! {_ = self.wake.notified()=>{}, _ = tokio::time::sleep(self.poll_interval)=>{}}
        }
    }
}

pub struct InstallationService {
    repository: Arc<LifecycleRepository>,
    store: Arc<InstallationStore>,
    clock: InstallationClock,
    wake: Arc<Notify>,
    control: Arc<InstallationWorkerControl>,
    state: watch::Receiver<InstallationServiceState>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl InstallationService {
    pub fn start(
        repository: Arc<LifecycleRepository>,
        store: Arc<InstallationStore>,
        credentials: InstallationCredentials,
        clock: InstallationClock,
        poll_interval: Duration,
    ) -> Result<Arc<Self>> {
        ensure!(
            (Duration::from_millis(10)..=Duration::from_secs(300)).contains(&poll_interval),
            "Invalid installation worker polling interval"
        );
        let (sender, state) = watch::channel(InstallationServiceState::Recovering);
        let wake = Arc::new(Notify::new());
        let control = Arc::new(InstallationWorkerControl::default());
        let worker = ServiceWorker {
            repository: Arc::clone(&repository),
            store: Arc::clone(&store),
            credentials,
            clock: Arc::clone(&clock),
            wake: Arc::clone(&wake),
            control: Arc::clone(&control),
            state: sender,
            owner: format!("installation-service:{}", uuid::Uuid::new_v4()),
            poll_interval,
        };
        let runtime = tokio::runtime::Handle::try_current()
            .context("Installation service requires an application runtime")?;
        let handle = runtime.spawn(async move {
            let result = worker.run().await;
            if let Err(error) = &result {
                tracing::error!(error=%error,"Installation background worker failed");
            }
            worker.state.send_replace(if result.is_ok() {
                InstallationServiceState::Stopped
            } else {
                InstallationServiceState::Failed
            });
        });
        Ok(Arc::new(Self {
            repository,
            store,
            clock,
            wake,
            control,
            state,
            worker: Mutex::new(Some(handle)),
        }))
    }

    pub fn state(&self) -> InstallationServiceState {
        *self.state.borrow()
    }
    pub fn subscribe_state(&self) -> watch::Receiver<InstallationServiceState> {
        self.state.clone()
    }
    pub fn repository(&self) -> &Arc<LifecycleRepository> {
        &self.repository
    }
    pub fn store(&self) -> &Arc<InstallationStore> {
        &self.store
    }

    pub async fn wait_ready(&self) -> Result<()> {
        let mut state = self.state.clone();
        loop {
            let value = *state.borrow_and_update();
            match value {
                InstallationServiceState::Ready => return Ok(()),
                InstallationServiceState::Recovering => state
                    .changed()
                    .await
                    .context("Installation worker stopped during startup recovery")?,
                InstallationServiceState::Stopped | InstallationServiceState::Failed => {
                    return Err(anyhow!("Installation service is unavailable"))
                }
            }
        }
    }

    pub async fn submit(&self, request: &InstallationSubmission) -> Result<LifecycleJob> {
        self.wait_ready().await?;
        ensure!(
            !self.control.is_stopping(),
            "Installation service is stopping"
        );
        let job =
            self.repository
                .accept_modelscope_installation(request, &self.store, (self.clock)())?;
        self.wake.notify_one();
        Ok(job)
    }

    pub fn control_job(
        &self,
        request: &InstallationControlRequest,
    ) -> Result<InstallationControlOutcome> {
        ensure!(
            !self.control.is_stopping(),
            "Installation service is stopping"
        );
        ensure!(
            matches!(
                self.state(),
                InstallationServiceState::Recovering | InstallationServiceState::Ready
            ),
            "Installation service is unavailable"
        );
        let changed = self
            .repository
            .control_installation(request, &self.store, (self.clock)())?;
        self.wake.notify_one();
        Ok(changed)
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.control.request_stop();
        self.wake.notify_one();
        let handle = self
            .worker
            .lock()
            .map_err(|_| anyhow!("Installation worker lock poisoned"))?
            .take();
        if let Some(handle) = handle {
            handle
                .await
                .context("Installation worker failed during shutdown")?;
        }
        ensure!(
            self.state() != InstallationServiceState::Failed,
            "Installation worker failed"
        );
        Ok(())
    }
}

impl Drop for InstallationService {
    fn drop(&mut self) {
        self.control.request_stop();
        self.wake.notify_one();
    }
}
