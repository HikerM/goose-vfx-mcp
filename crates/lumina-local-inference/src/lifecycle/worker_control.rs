use anyhow::Result;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub struct InstallationWorkerControl {
    stopping: AtomicBool,
}

impl InstallationWorkerControl {
    pub fn request_stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub(crate) fn checkpoint(&self) -> Result<()> {
        if self.is_stopping() {
            return Err(InstallationInterrupted.into());
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct InstallationInterrupted;

impl Display for InstallationInterrupted {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Installation execution stopped for application shutdown")
    }
}

impl std::error::Error for InstallationInterrupted {}
