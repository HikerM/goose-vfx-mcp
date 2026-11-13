use anyhow::Result;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::time::Duration;

/// Keep the lease live while the OS flushes a file. The scoped thread finishes
/// before the caller can publish, release the transfer lock or remove the file.
pub(super) fn sync_with_lease(
    flush: impl FnOnce() -> std::io::Result<()> + Send,
    checkpoint: impl FnMut() -> Result<()>,
) -> Result<()> {
    wait_for_flush(flush, checkpoint, Duration::from_secs(3))
}

fn wait_for_flush(
    flush: impl FnOnce() -> std::io::Result<()> + Send,
    mut checkpoint: impl FnMut() -> Result<()>,
    interval: Duration,
) -> Result<()> {
    checkpoint()?;
    std::thread::scope(|scope| {
        let (sender, receiver) = sync_channel(1);
        scope.spawn(move || {
            let _ = sender.send(flush());
        });
        loop {
            match receiver.recv_timeout(interval) {
                Ok(result) => {
                    result?;
                    return checkpoint();
                }
                Err(RecvTimeoutError::Timeout) => checkpoint()?,
                Err(RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("File flush worker disconnected")
                }
            }
        }
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_flush_receives_lease_checkpoints_before_it_returns() {
        let (release, blocked) = sync_channel(1);
        let mut checks = 0;
        wait_for_flush(
            move || {
                blocked
                    .recv_timeout(Duration::from_secs(5))
                    .expect("lease renewal did not run during flush");
                Ok(())
            },
            || {
                checks += 1;
                if checks == 3 {
                    release.send(())?;
                }
                Ok(())
            },
            Duration::from_millis(1),
        )
        .unwrap();
        assert!(checks >= 4);
    }

    #[test]
    fn cancelled_checkpoint_cannot_release_ownership_before_flush_exits() {
        let (release, blocked) = sync_channel(1);
        let finished = AtomicBool::new(false);
        let finished_ref = &finished;
        let mut started = false;
        let result = wait_for_flush(
            move || {
                blocked
                    .recv_timeout(Duration::from_secs(5))
                    .expect("cancellation checkpoint did not run");
                finished_ref.store(true, Ordering::Release);
                Ok(())
            },
            || {
                if started {
                    release.send(())?;
                    return Err(InstallationInterrupted.into());
                }
                started = true;
                Ok(())
            },
            Duration::from_millis(1),
        );
        assert!(result.unwrap_err().is::<InstallationInterrupted>());
        assert!(finished.load(Ordering::Acquire));
    }
}
