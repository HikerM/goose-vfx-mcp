use anyhow::{ensure, Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::backend::{BackendLoadedModel, LocalInferenceBackend};
use crate::lifecycle::RuntimeAssetGuard;
use crate::local_model_registry::ModelSettings;
use crate::ResolvedModelPaths;
use lumina_provider_types::errors::ProviderError;

mod gguf;

pub(crate) struct LoadedModelHandle {
    // Native destructors may still read mapped assets, so the guard drops last.
    backend: Box<dyn BackendLoadedModel>,
    _assets: RuntimeAssetGuard,
}

impl LoadedModelHandle {
    pub(crate) fn load(
        backend: &dyn LocalInferenceBackend,
        model_id: &str,
        resolved: &ResolvedModelPaths,
        settings: &ModelSettings,
    ) -> Result<Self, ProviderError> {
        let assets = acquire_assets(backend.id(), resolved).map_err(|error| {
            ProviderError::ExecutionError(format!("Cannot protect local model assets: {error:#}"))
        })?;
        let backend = backend.load_model(model_id, resolved, settings)?;
        Ok(Self {
            backend,
            _assets: assets,
        })
    }

    pub(crate) fn as_mut(&mut self) -> &mut dyn BackendLoadedModel {
        self.backend.as_mut()
    }
}

fn acquire_assets(backend_id: &str, resolved: &ResolvedModelPaths) -> Result<RuntimeAssetGuard> {
    let mut paths = resolved.shard_paths.clone();
    if let Some(projector) = &resolved.mmproj_path {
        paths.push(projector.clone());
    }
    if backend_id == crate::mlx::MLX_BACKEND_ID {
        let mut visited = BTreeSet::new();
        collect_model_directory(&resolved.model_path, &mut visited, &mut paths)?;
        return RuntimeAssetGuard::acquire(paths);
    }

    paths.push(resolved.model_path.clone());
    let initial = RuntimeAssetGuard::acquire(paths.clone())?;
    paths.extend(native_shards(&resolved.model_path)?);
    let complete = RuntimeAssetGuard::acquire(paths)?;
    drop(initial);
    Ok(complete)
}

fn native_shards(path: &Path) -> Result<Vec<PathBuf>> {
    let Some(count) = gguf::native_split_count(path)? else {
        return Ok(Vec::new());
    };
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("GGUF shard filename must be UTF-8")?;
    let suffix = format!("-00001-of-{count:05}.gguf");
    let prefix = filename
        .strip_suffix(&suffix)
        .filter(|prefix| !prefix.is_empty())
        .context("GGUF filename differs from native split metadata")?;
    // Preserve the snapshot path: canonicalizing an HF symlink loses sibling names.
    Ok((2..=count)
        .map(|index| path.with_file_name(format!("{prefix}-{index:05}-of-{count:05}.gguf")))
        .collect())
}

#[cfg(any(feature = "mlx", test))]
pub(crate) fn acquire_mlx_assets(path: &Path) -> Result<RuntimeAssetGuard> {
    let mut files = Vec::new();
    collect_model_directory(path, &mut BTreeSet::new(), &mut files)?;
    RuntimeAssetGuard::acquire(files)
}

fn collect_model_directory(
    path: &Path,
    visited: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    let root = if path.is_dir() {
        path
    } else {
        path.parent()
            .context("MLX model path has no parent directory")?
    };
    collect_directory(root, visited, files)
}

fn collect_directory(
    directory: &Path,
    visited: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    if !visited.insert(directory.canonicalize()?) {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = std::fs::metadata(&path)?;
        if metadata.is_dir() {
            collect_directory(&path, visited, files)?;
        } else {
            ensure!(metadata.is_file(), "MLX model asset is not a regular file");
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/support/runtime_asset_binding.rs"]
mod tests;
