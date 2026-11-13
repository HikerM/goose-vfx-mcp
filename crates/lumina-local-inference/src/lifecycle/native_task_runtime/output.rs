use super::super::files::{pin_directory, require_single_link_file};
use super::process::CapturedProcess;
use super::*;
use std::collections::BTreeSet;
use std::io::{Seek, SeekFrom};

const SCRATCH: &str = ".engine-temp";

pub(super) struct GuardedTaskDirectory {
    publication_root: PathBuf,
    guards: Vec<File>,
    temporary: Option<tempfile::TempDir>,
}

impl GuardedTaskDirectory {
    pub(super) fn create(root: &Path) -> Result<Self> {
        let mut guards = pin_directory_tree(root)?;
        let publication_root = root.canonicalize()?;
        let temporary = tempfile::Builder::new()
            .prefix(".lumina-native-task-")
            .tempdir_in(&publication_root)?;
        let scratch = temporary.path().join(SCRATCH);
        std::fs::create_dir(&scratch)?;
        guards.extend(pin_directory_tree(&scratch)?);
        Ok(Self {
            publication_root,
            guards,
            temporary: Some(temporary),
        })
    }
    pub(super) fn path(&self) -> &Path {
        self.temporary.as_ref().expect("live task directory").path()
    }
    pub(super) fn publication_root(&self) -> &Path {
        &self.publication_root
    }
    pub(super) fn scratch(&self) -> PathBuf {
        self.path().join(SCRATCH)
    }

    /// A watchdog, not an OS disk quota. The admitted profile must account for
    /// native write bursts and publication copies separately.
    pub(super) fn inspect_bytes(
        &self,
        maximum: u64,
        temporary_limit: u64,
    ) -> Result<BTreeSet<String>> {
        let mut total = 0u64;
        let mut names = BTreeSet::new();
        for entry in std::fs::read_dir(self.path())? {
            let entry = entry?;
            if entry.file_name() == SCRATCH {
                continue;
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Native task produced a non-UTF8 filename"))?;
            let relative = RepositoryPath::parse(&name)?;
            ensure!(
                !relative.as_str().contains('/'),
                "Native task output must be a direct child"
            );
            let path = checked_local_path(self.path(), &relative)?;
            let metadata = std::fs::symlink_metadata(path)?;
            ensure!(
                metadata.is_file(),
                "Native task output contains a directory or redirect"
            );
            total = total
                .checked_add(metadata.len())
                .context("Native task output size overflow")?;
            ensure!(
                total <= maximum,
                "Native task exceeded its total output byte limit"
            );
            names.insert(name);
            ensure!(names.len() <= 1024, "Native task produced too many files");
        }
        let mut remaining = temporary_limit
            .checked_sub(total)
            .context("Native task exceeded its temporary disk budget")?;
        let mut entries = 0;
        inspect_scratch(&self.scratch(), &mut remaining, &mut entries, 0)?;
        Ok(names)
    }
}

fn inspect_scratch(
    path: &Path,
    remaining: &mut u64,
    entries: &mut usize,
    depth: u32,
) -> Result<()> {
    ensure!(
        depth <= 16,
        "Native task temporary directory nesting exceeds the limit"
    );
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        *entries += 1;
        ensure!(
            *entries <= 1024,
            "Native task produced too many temporary entries"
        );
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        ensure!(
            !super::super::files::is_redirect(&metadata),
            "Redirect in native task temporary files"
        );
        if metadata.is_dir() {
            // The scratch root is pinned at creation. Hold each child while traversing it.
            let _guard = match pin_directory(&entry.path()) {
                Ok(guard) => guard,
                Err(e)
                    if e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue
                }
                Err(e) => return Err(e),
            };
            inspect_scratch(&entry.path(), remaining, entries, depth + 1)?;
        } else {
            ensure!(metadata.is_file(), "Invalid native task temporary file");
            *remaining = remaining
                .checked_sub(metadata.len())
                .context("Native task exceeded its temporary disk budget")?;
        }
    }
    Ok(())
}

impl Drop for GuardedTaskDirectory {
    fn drop(&mut self) {
        // Close directory handles before tempfile removes this uniquely created directory.
        self.guards.clear();
        self.temporary.take();
    }
}

pub(super) fn pin_file(path: &Path) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Task asset is not a regular file");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Task asset is a filesystem redirect"
        );
    }
    Ok(file)
}

pub struct NativeTaskOutputFile {
    file: File,
    name: String,
    bytes: u64,
    sha256: Sha256Digest,
}

impl NativeTaskOutputFile {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn size_bytes(&self) -> u64 {
        self.bytes
    }
    pub fn sha256(&self) -> &Sha256Digest {
        &self.sha256
    }
    /// The original read-only handle and its clone share a cursor. The consumer
    /// must hold the NativeTaskOutput through publication and read them serially.
    pub fn try_clone_reader(&self) -> Result<File> {
        let mut reader = self.file.try_clone()?;
        reader.rewind()?;
        Ok(reader)
    }
    pub fn read_range(&mut self, offset: u64, length: usize) -> Result<Vec<u8>> {
        ensure!(
            length <= 1024 * 1024,
            "Task output reads are limited to 1MiB"
        );
        ensure!(offset <= self.bytes, "Task output read starts beyond EOF");
        let length = u64::try_from(length)?.min(self.bytes - offset) as usize;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; length];
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

/// Holds original output handles and the guarded temporary directory until the
/// caller has finished format validation/copying. It contains no READY assertion.
pub struct NativeTaskOutput {
    files: Vec<NativeTaskOutputFile>,
    _directory: Arc<GuardedTaskDirectory>,
    publication: Arc<process::PublicationLease>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl NativeTaskOutput {
    pub fn snapshot_digest(&self) -> &Sha256Digest {
        &self.publication.snapshot_digest
    }
    pub fn files(&self) -> &[NativeTaskOutputFile] {
        &self.files
    }
    pub fn files_mut(&mut self) -> &mut [NativeTaskOutputFile] {
        &mut self.files
    }
}

pub(super) fn collect_outputs(
    directory: Arc<GuardedTaskDirectory>,
    expected: Vec<String>,
    maximum: u64,
    captured: CapturedProcess,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<NativeTaskOutput> {
    checkpoint(deadline, cancelled)?;
    let names = directory.inspect_bytes(maximum, captured.publication.temporary_limit())?;
    ensure!(
        names == expected.iter().cloned().collect(),
        "Native task outputs differ from the typed task layout"
    );
    let mut files = Vec::new();
    let mut total = 0u64;
    for name in expected {
        let path = checked_local_path(directory.path(), &RepositoryPath::parse(&name)?)?;
        let mut file = pin_file(&path)?;
        require_single_link_file(&file)?;
        let size = file.metadata()?.len();
        total = total
            .checked_add(size)
            .context("Native task output size overflow")?;
        ensure!(
            total <= maximum,
            "Native task output grew beyond its admission"
        );
        let mut hash = Sha256::new();
        let mut read_bytes = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            checkpoint(deadline, cancelled)?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            read_bytes = read_bytes
                .checked_add(count as u64)
                .context("Output size overflow")?;
            ensure!(
                read_bytes <= size,
                "Native task output changed after process exit"
            );
            hash.update(&buffer[..count]);
        }
        ensure!(
            read_bytes == size && file.metadata()?.len() == size,
            "Native task output changed after process exit"
        );
        file.rewind()?;
        files.push(NativeTaskOutputFile {
            file,
            name,
            bytes: size,
            sha256: Sha256Digest::from_sha256(hash),
        });
    }
    Ok(NativeTaskOutput {
        files,
        _directory: directory,
        publication: captured.publication,
        stdout: captured.stdout,
        stderr: captured.stderr,
    })
}
