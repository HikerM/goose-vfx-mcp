use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use std::collections::BTreeSet;
use std::fs::File;
use std::path::PathBuf;

use super::files::open_for_verification;

/// Pins runtime assets with read handles and shared file locks until dropped.
///
/// A remover must acquire an exclusive lock on the same files and separately
/// coordinate paths and a durable deletion intent. These advisory locks alone
/// do not implement uninstall or prevent uncoordinated Unix renames/writes.
#[derive(Debug)]
pub struct RuntimeAssetGuard {
    paths: Vec<PathBuf>,
    _files: Vec<File>,
}

impl RuntimeAssetGuard {
    pub fn acquire(paths: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let paths = paths
            .into_iter()
            .map(|path| {
                path.canonicalize()
                    .with_context(|| format!("Runtime asset is unavailable: {}", path.display()))
            })
            .collect::<Result<BTreeSet<_>>>()?
            .into_iter()
            .collect::<Vec<_>>();
        let mut files = Vec::with_capacity(paths.len());
        for path in &paths {
            ensure!(
                std::fs::metadata(path)?.is_file(),
                "Runtime asset is not a regular file: {}",
                path.display()
            );
            let file = open_for_verification(path)
                .with_context(|| format!("Runtime asset cannot be opened: {}", path.display()))?;
            ensure!(
                file.metadata()?.is_file(),
                "Opened runtime asset is not a regular file: {}",
                path.display()
            );
            FileExt::try_lock_shared(&file).with_context(|| {
                format!(
                    "Runtime asset shared lock is unavailable: {}",
                    path.display()
                )
            })?;
            files.push(file);
        }
        Ok(Self {
            paths,
            _files: files,
        })
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}
