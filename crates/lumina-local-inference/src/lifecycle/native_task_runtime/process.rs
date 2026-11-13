use super::*;
use std::process::{ExitStatus, Stdio};
use std::thread::JoinHandle;

const LOG_LIMIT: usize = 1024 * 1024;

pub(super) struct ProcessOwnership {
    pub assets: RuntimeAssetGuard,
    pub publication: Arc<PublicationLease>,
    pub retained: Vec<Box<dyn Send>>,
}

#[derive(Debug)]
pub(super) struct PublicationLease {
    permit: Arc<File>,
    admission: TaskResourceAdmission,
    pub snapshot_digest: Sha256Digest,
}

impl PublicationLease {
    pub(super) fn new(
        permit: File,
        admission: TaskResourceAdmission,
        snapshot_digest: Sha256Digest,
    ) -> Self {
        Self {
            permit: Arc::new(permit),
            admission,
            snapshot_digest,
        }
    }
    pub(super) fn temporary_limit(&self) -> u64 {
        self.admission.budget().temporary_output_bytes
    }
}

#[derive(Debug)]
pub(super) struct CapturedProcess {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub publication: Arc<PublicationLease>,
}

#[derive(Debug)]
pub struct NativeTaskFailure {
    pub reason: String,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl std::fmt::Display for NativeTaskFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}
impl std::error::Error for NativeTaskFailure {}

fn drain(
    reader: impl Read + Send + 'static,
    exceeded: Arc<AtomicBool>,
) -> Result<JoinHandle<std::io::Result<Vec<u8>>>> {
    Ok(std::thread::Builder::new()
        .name("lumina-native-task-log".into())
        .spawn(move || {
            let mut reader = reader;
            let mut retained = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let count = match reader.read(&mut buffer) {
                    Ok(count) => count,
                    Err(error) => {
                        exceeded.store(true, Ordering::Release);
                        return Err(error);
                    }
                };
                if count == 0 {
                    return Ok(retained);
                }
                let remaining = LOG_LIMIT - retained.len();
                retained.extend_from_slice(&buffer[..count.min(remaining)]);
                if count > remaining {
                    exceeded.store(true, Ordering::Release);
                }
            }
        })?)
}

#[cfg(windows)]
fn engine_temp_path(path: &Path) -> Result<PathBuf> {
    use std::path::{Component, Prefix};
    let mut parts = path.components();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        anyhow::bail!("Expected a pinned local disk directory");
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
        _ => anyhow::bail!("Expected a pinned local disk directory"),
    };
    ensure!(
        parts.next() == Some(Component::RootDir),
        "Expected an absolute scratch path"
    );
    // audio.cpp appends a literal "." to its model root. Win32 interprets that component
    // only in the ordinary path namespace. Verify both names resolve to the pinned directory.
    let ordinary = PathBuf::from(format!("{}:\\", char::from(drive))).join(parts.as_path());
    ensure!(
        ordinary.canonicalize()? == path,
        "Engine temporary path differs from the pinned directory"
    );
    Ok(ordinary)
}

pub(super) fn run_process(
    mut command: Command,
    mut ownership: ProcessOwnership,
    directory: Arc<output::GuardedTaskDirectory>,
    deadline: Instant,
    max_output_bytes: u64,
    cancelled: &AtomicBool,
) -> Result<CapturedProcess> {
    checkpoint(deadline, cancelled)?;
    let scratch = directory.scratch();
    #[cfg(windows)]
    let scratch = engine_temp_path(&scratch)?;
    let temporary_limit = ownership.publication.temporary_limit();
    command
        .env("TMP", &scratch)
        .env("TEMP", &scratch)
        .env("TMPDIR", &scratch)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ownership.retained.push(Box::new(Arc::clone(&directory)));
    ownership
        .retained
        .push(Box::new(Arc::clone(&ownership.publication)));
    let mut process = GuardedChild::spawn_single_process_task(
        &mut command,
        ownership.assets,
        Arc::clone(&ownership.publication.permit),
        ownership.retained,
    )?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout = drain(process.take_stdout()?, Arc::clone(&exceeded))?;
    let stderr = drain(process.take_stderr()?, Arc::clone(&exceeded))?;
    let outcome: Result<ExitStatus> = loop {
        if let Err(error) = checkpoint(deadline, cancelled) {
            break Err(error);
        }
        if exceeded.load(Ordering::Acquire) {
            break Err(anyhow::anyhow!(
                "Native task log exceeded its 1MiB per-stream limit or could not be read"
            ));
        }
        if let Err(error) = directory.inspect_bytes(max_output_bytes, temporary_limit) {
            break Err(error);
        }
        match process.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => (),
            Err(error) => break Err(error),
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let status = match &outcome {
        Ok(status) => *status,
        Err(_) => process.stop()?,
    };
    // The finite task is resumed in a single-process job, so its exit also
    // closes all inherited pipes before the drain threads are joined.
    drop(process);
    let stdout = stdout
        .join()
        .map_err(|_| anyhow::anyhow!("Task stdout reader failed"))??;
    let stderr = stderr
        .join()
        .map_err(|_| anyhow::anyhow!("Task stderr reader failed"))??;
    let reason = match outcome {
        Err(error) => Some(error.to_string()),
        Ok(_) if exceeded.load(Ordering::Acquire) => {
            Some("Native task log exceeded its 1MiB per-stream limit".into())
        }
        Ok(status) if !status.success() => {
            Some(format!("Native task failed with exit status {status}"))
        }
        Ok(_) => None,
    };
    if let Some(reason) = reason {
        return Err(NativeTaskFailure {
            reason,
            exit_code: status.code(),
            stdout,
            stderr,
        }
        .into());
    }
    directory.inspect_bytes(max_output_bytes, temporary_limit)?;
    Ok(CapturedProcess {
        stdout,
        stderr,
        publication: ownership.publication,
    })
}
