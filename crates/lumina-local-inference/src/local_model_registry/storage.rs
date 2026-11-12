use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use super::migration::{preserve_snapshot, LegacyImportReport};
use super::LocalModelRegistry;

const REGISTRATION_ID_FIELD: &str = "_lumina_registration_id";

#[derive(Debug, Clone)]
pub struct PendingModelRegistration {
    pub(super) model_id: String,
    data_dir: PathBuf,
    snapshot_digest: String,
    registration_id: String,
    raw_record: Value,
}

#[derive(Debug, Clone)]
pub(super) struct RegistrySnapshot {
    data_dir: PathBuf,
    digest: Option<String>,
    raw: Value,
    typed: Value,
    pub report: Option<LegacyImportReport>,
}

pub(super) fn digest(bytes: &[u8]) -> String {
    crate::lifecycle::Sha256Digest::of_bytes(bytes)
        .as_str()
        .to_owned()
}

pub(super) fn data_directory(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "Model data directory must be absolute");
    std::fs::create_dir_all(path)?;
    path.canonicalize()
        .context("Model data directory is unavailable")
}

fn directory(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    match std::fs::create_dir(&path) {
        Ok(()) => sync_directory(parent)?,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Registry metadata directory must not be a link: {}",
        path.display()
    );
    Ok(path)
}

pub(super) fn migration_directory(data_dir: &Path, migration_id: &str) -> Result<PathBuf> {
    let root = directory(data_dir, "model-lifecycle")?;
    let root = directory(&root, "legacy-imports")?;
    let root = directory(&root, "v1")?;
    directory(&root, migration_id)
}

pub(super) fn registry_lock(path: &Path) -> Result<File> {
    let parent = path.parent().context("Registry has no parent directory")?;
    let data_dir = parent.parent().context("Registry has no data directory")?;
    directory(data_dir, "models")?;
    let lock_path = path.with_extension("json.lock");
    if let Ok(metadata) = std::fs::symlink_metadata(&lock_path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Registry lock must be a regular file"
        );
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock_exclusive()?;
    Ok(lock)
}

pub(super) fn read_registry(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Registry must be a regular file: {}",
                path.display()
            );
            Ok(Some(std::fs::read(path)?))
        }
    }
}

pub(super) fn immutable_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(existing) = read_registry(path)? {
        ensure!(
            digest(&existing) == digest(bytes) && existing == bytes,
            "Immutable registry backup or ledger differs: {}",
            path.display()
        );
        return Ok(());
    }
    let parent = path.parent().context("Backup has no parent directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(path)?;
    sync_directory(parent)?;
    let actual = std::fs::read(path)?;
    ensure!(
        digest(&actual) == digest(bytes) && actual == bytes,
        "Registry backup readback failed: {}",
        path.display()
    );
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(super) fn load(data_dir: &Path) -> Result<LocalModelRegistry> {
    let data_dir = data_directory(data_dir)?;
    let path = data_dir.join("models/registry.json");
    let _lock = registry_lock(&path)?;
    let bytes = read_registry(&path)?;
    let report = bytes
        .as_ref()
        .map(|bytes| preserve_snapshot(&data_dir, &path, bytes))
        .transpose()?;
    if let Some(report) = &report {
        report.require_valid()?;
    }
    let raw = match &bytes {
        Some(bytes) => serde_json::from_slice(bytes)?,
        None => serde_json::json!({ "models": [] }),
    };
    let mut registry: LocalModelRegistry = serde_json::from_value(raw.clone())?;
    let typed = serde_json::to_value(&registry)?;
    registry.snapshot = Some(RegistrySnapshot {
        data_dir,
        digest: bytes.as_ref().map(|bytes| digest(bytes)),
        raw,
        typed,
        report,
    });
    Ok(registry)
}

pub(super) fn save(registry: &mut LocalModelRegistry) -> Result<()> {
    let snapshot = registry
        .snapshot
        .as_ref()
        .context("Registry must be loaded before it can be saved")?;
    let path = snapshot.data_dir.join("models/registry.json");
    let _lock = registry_lock(&path)?;
    let current = read_registry(&path)?;
    ensure!(
        current.as_ref().map(|bytes| digest(bytes)) == snapshot.digest,
        "Registry changed in another process; reload before retrying this update"
    );
    if let Some(bytes) = &current {
        preserve_snapshot(&snapshot.data_dir, &path, bytes)?.require_valid()?;
    }
    let typed = serde_json::to_value(&*registry)?;
    if current.is_some() && typed == snapshot.typed {
        return Ok(());
    }
    let raw = merge_registry(snapshot, &typed)?;
    let bytes = serde_json::to_vec_pretty(&raw)?;
    // A crash after preparing this snapshot leaves an unreferenced ledger entry;
    // readers only use the entry matching the actual registry bytes.
    let report = preserve_snapshot(&snapshot.data_dir, &path, &bytes)?;
    report.require_valid()?;
    if current.as_deref() != Some(bytes.as_slice()) {
        let parent = path.parent().context("Registry has no parent directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path)?;
        sync_directory(parent)?;
        ensure!(
            std::fs::read(&path)? == bytes,
            "Registry write readback failed"
        );
    }
    registry.snapshot = Some(RegistrySnapshot {
        data_dir: snapshot.data_dir.clone(),
        digest: Some(digest(&bytes)),
        raw,
        typed,
        report: Some(report),
    });
    Ok(())
}

pub(super) fn pending_registration(
    registry: &LocalModelRegistry,
    model_id: &str,
) -> Result<PendingModelRegistration> {
    let snapshot = registry
        .snapshot
        .as_ref()
        .context("Pending registration has no saved snapshot")?;
    let raw_record = raw_record(snapshot, model_id).context("Pending registration disappeared")?;
    let registration_id = raw_record[REGISTRATION_ID_FIELD]
        .as_str()
        .context("Pending registration has no unique identity")?;
    Ok(PendingModelRegistration {
        model_id: model_id.to_string(),
        data_dir: snapshot.data_dir.clone(),
        snapshot_digest: snapshot
            .digest
            .clone()
            .context("Pending registration was not saved")?,
        registration_id: registration_id.to_string(),
        raw_record: raw_record.clone(),
    })
}

pub(super) fn owns_pending_registration(
    registry: &LocalModelRegistry,
    receipt: &PendingModelRegistration,
) -> Result<bool> {
    let Some(snapshot) = &registry.snapshot else {
        return Ok(false);
    };
    Ok(snapshot.data_dir == receipt.data_dir
        && snapshot.digest.as_ref() == Some(&receipt.snapshot_digest)
        && raw_record(snapshot, &receipt.model_id).is_some_and(|current| {
            current == &receipt.raw_record
                && current[REGISTRATION_ID_FIELD].as_str() == Some(receipt.registration_id.as_str())
        })
        && serde_json::to_value(registry)? == snapshot.typed)
}

fn raw_record<'a>(snapshot: &'a RegistrySnapshot, model_id: &str) -> Option<&'a Value> {
    snapshot.raw["models"]
        .as_array()?
        .iter()
        .find(|entry| entry["id"].as_str() == Some(model_id))
}

fn merge_registry(snapshot: &RegistrySnapshot, after: &Value) -> Result<Value> {
    let before = snapshot.typed["models"]
        .as_array()
        .context("Registry snapshot has no models")?;
    let original = snapshot.raw["models"]
        .as_array()
        .context("Registry snapshot has no raw models")?;
    let old: BTreeMap<_, _> = before
        .iter()
        .zip(original)
        .map(|(typed, raw)| (typed["id"].as_str().unwrap(), (typed, raw)))
        .collect();
    let mut ids = BTreeSet::new();
    let mut models = Vec::new();
    for entry in after["models"]
        .as_array()
        .context("Registry update has no models")?
    {
        let id = entry["id"].as_str().context("Registry model has no ID")?;
        ensure!(
            ids.insert(id),
            "Registry update contains a duplicate model ID"
        );
        models.push(match old.get(id) {
            Some((before, raw)) => merge_model(raw, before, entry)?,
            None => {
                let mut entry = entry.clone();
                // New registrations need distinct identities even when all known
                // fields match. Old writers dropping this field revoke cleanup.
                entry[REGISTRATION_ID_FIELD] = Value::String(uuid::Uuid::new_v4().to_string());
                entry
            }
        });
    }
    let mut raw = snapshot.raw.clone();
    raw["models"] = Value::Array(models);
    Ok(raw)
}

fn merge_model(raw: &Value, before: &Value, after: &Value) -> Result<Value> {
    let mut merged = merge_changes(raw, before, after);
    if before["shard_files"] != after["shard_files"] {
        let before = before["shard_files"]
            .as_array()
            .context("Model snapshot has no shard list")?;
        let original = raw
            .get("shard_files")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        ensure!(
            original.len() == before.len(),
            "Model shard snapshot differs from its original records"
        );
        let mut old = BTreeMap::new();
        for (typed, raw) in before.iter().zip(original) {
            ensure!(
                old.insert(shard_identity(typed)?, (typed, raw)).is_none(),
                "Model snapshot contains ambiguous duplicate shard identities"
            );
        }
        let mut identities = BTreeSet::new();
        let mut shards = Vec::new();
        for shard in after["shard_files"]
            .as_array()
            .context("Model update has no shard list")?
        {
            let identity = shard_identity(shard)?;
            ensure!(
                identities.insert(identity),
                "Model update contains duplicate shard identities"
            );
            shards.push(match old.get(&identity) {
                Some((before, raw)) => merge_changes(raw, before, shard),
                None => shard.clone(),
            });
        }
        merged["shard_files"] = Value::Array(shards);
    }
    Ok(merged)
}

fn shard_identity(shard: &Value) -> Result<(&str, &str)> {
    Ok((
        shard["filename"]
            .as_str()
            .context("Shard has no filename")?,
        shard["local_path"]
            .as_str()
            .context("Shard has no local path")?,
    ))
}

fn merge_changes(raw: &Value, before: &Value, after: &Value) -> Value {
    if before == after {
        return raw.clone();
    }
    match (raw, before, after) {
        (Value::Object(raw), Value::Object(before), Value::Object(after)) => {
            let mut result = raw.clone();
            for key in before.keys().filter(|key| !after.contains_key(*key)) {
                result.remove(key);
            }
            for (key, value) in after {
                if before.get(key) != Some(value) {
                    let merged = match (raw.get(key), before.get(key)) {
                        (Some(raw), Some(before)) => merge_changes(raw, before, value),
                        _ => value.clone(),
                    };
                    result.insert(key.clone(), merged);
                }
            }
            Value::Object(result)
        }
        _ => after.clone(),
    }
}
