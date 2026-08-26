#![forbid(unsafe_code)]

mod layout;
mod sqlite;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
pub use layout::{default_lumina_layout, discover_legacy_layouts, StorageLayout};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use serde_yaml::Value as YamlValue;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MIGRATION_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct MigrationRequest {
    pub sources: Vec<StorageLayout>,
    pub target: StorageLayout,
    pub dry_run: bool,
    pub migrate_secrets: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStatus {
    Complete,
    CompleteWithWarnings,
    AlreadyComplete,
    DryRun,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedArtifact {
    pub source: PathBuf,
    pub target: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    pub migration_version: u32,
    pub status: MigrationStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub source_names: Vec<String>,
    pub imported: Vec<ImportedArtifact>,
    pub skipped_existing: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

impl MigrationReport {
    fn new(sources: &[StorageLayout]) -> Self {
        let now = Utc::now();
        Self {
            migration_version: MIGRATION_VERSION,
            status: MigrationStatus::Complete,
            started_at: now,
            completed_at: now,
            source_names: sources.iter().map(|source| source.name.clone()).collect(),
            imported: Vec::new(),
            skipped_existing: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn finish(&mut self, dry_run: bool) {
        self.completed_at = Utc::now();
        self.status = if dry_run {
            MigrationStatus::DryRun
        } else if self.warnings.is_empty() {
            MigrationStatus::Complete
        } else {
            MigrationStatus::CompleteWithWarnings
        };
    }
}

pub async fn migrate(request: MigrationRequest) -> Result<MigrationReport> {
    let marker_path = request.target.state.join("legacy-migration-v1.json");
    if marker_path.is_file() && !request.dry_run {
        let marker = fs::read_to_string(&marker_path)
            .with_context(|| format!("failed to read {}", marker_path.display()))?;
        let mut report: MigrationReport = serde_json::from_str(&marker)
            .with_context(|| format!("failed to parse {}", marker_path.display()))?;
        report.status = MigrationStatus::AlreadyComplete;
        return Ok(report);
    }

    let lock = if request.dry_run {
        None
    } else {
        fs::create_dir_all(&request.target.state).with_context(|| {
            format!(
                "failed to create Lumina migration state directory {}",
                request.target.state.display()
            )
        })?;
        let lock_path = request.target.state.join("legacy-migration-v1.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("failed to open {}", lock_path.display()))?;
        lock.try_lock_exclusive()
            .context("another Lumina migration is already running")?;
        Some(lock)
    };

    let mut report = MigrationReport::new(&request.sources);
    let mut seen_sources = BTreeSet::new();

    for source in &request.sources {
        let source_identity = format!(
            "{}|{}|{}|{}",
            source.config.display(),
            source.data.display(),
            source.state.display(),
            source.desktop.display()
        );
        if !seen_sources.insert(source_identity) || !source.has_any_content() {
            continue;
        }

        let config_target = if source.name.starts_with("legacy-recipe-path:") {
            request.target.config.join("recipes")
        } else {
            request.target.config.clone()
        };
        migrate_tree(
            &source.config,
            &config_target,
            TreeKind::Config,
            &request,
            &mut report,
        )
        .await?;
        migrate_tree(
            &source.data,
            &request.target.data,
            TreeKind::Data,
            &request,
            &mut report,
        )
        .await?;
        migrate_tree(
            &source.state,
            &request.target.state,
            TreeKind::State,
            &request,
            &mut report,
        )
        .await?;
        migrate_tree(
            &source.desktop,
            &request.target.desktop,
            TreeKind::Desktop,
            &request,
            &mut report,
        )
        .await?;
    }

    if request.migrate_secrets {
        migrate_secret_store(&request, &mut report)?;
    }
    report_unmigrated_process_state(&mut report);
    report.finish(request.dry_run);

    if !request.dry_run {
        write_json_atomically(&marker_path, &report)?;
    }

    if let Some(lock) = lock {
        FileExt::unlock(&lock).context("failed to unlock the Lumina migration lock")?;
    }
    Ok(report)
}

fn report_unmigrated_process_state(report: &mut MigrationReport) {
    let mut legacy_environment_names = std::env::vars_os()
        .filter_map(|(key, _)| key.into_string().ok())
        .filter(|key| key.starts_with("GOOSE_") && key != "GOOSE_PATH_ROOT")
        .collect::<Vec<_>>();
    legacy_environment_names.sort();
    if !legacy_environment_names.is_empty() {
        report.warnings.push(format!(
            "legacy process environment was not persisted; rename or reconfigure these variables for Lumina: {}",
            legacy_environment_names.join(", ")
        ));
    }

    if Path::new(".goosehints").is_file() {
        report.warnings.push(
            "the current project contains .goosehints; review it and create .luminahints explicitly so project files are not modified without consent"
                .to_string(),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeKind {
    Config,
    Data,
    State,
    Desktop,
}

async fn migrate_tree(
    source_root: &Path,
    target_root: &Path,
    kind: TreeKind,
    request: &MigrationRequest,
    report: &mut MigrationReport,
) -> Result<()> {
    if !source_root.is_dir() || same_location(source_root, target_root) {
        return Ok(());
    }

    let mut pending = vec![source_root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to read legacy directory {}", directory.display()))?
        {
            let entry = entry?;
            let source = entry.path();
            let relative = source.strip_prefix(source_root)?;

            if should_skip(kind, relative) {
                report.warnings.push(format!(
                    "legacy artifact was intentionally not activated: {}",
                    source.display()
                ));
                continue;
            }

            let metadata = fs::symlink_metadata(&source)?;
            if metadata.file_type().is_symlink() {
                report.warnings.push(format!(
                    "legacy symbolic link was not followed: {}",
                    source.display()
                ));
                continue;
            }
            if metadata.is_dir() {
                pending.push(source);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }

            let target = archive_target(kind, relative, target_root);
            if target.exists() {
                report.skipped_existing.push(target.clone());
                report.warnings.push(format!(
                    "Lumina target already exists and was not overwritten: {}",
                    target.display()
                ));
                continue;
            }
            if request.dry_run {
                report.imported.push(ImportedArtifact {
                    source,
                    target,
                    sha256: String::new(),
                    bytes: metadata.len(),
                });
                continue;
            }

            if is_session_database(kind, relative) {
                sqlite::import_session_database(&source, &target).await?;
            } else if is_sqlite_database(relative) {
                sqlite::backup_database(&source, &target).await?;
            } else if is_yaml_to_transform(kind, relative) {
                transform_yaml_file(&source, &target, request)?;
            } else if is_desktop_settings(kind, relative) {
                transform_desktop_settings(&source, &target)?;
            } else if is_json_to_transform(kind, relative) {
                transform_json_file(&source, &target, request)?;
            } else {
                copy_file_atomically(&source, &target)?;
            }

            let (sha256, bytes) = hash_file(&target)?;
            report.imported.push(ImportedArtifact {
                source,
                target,
                sha256,
                bytes,
            });
        }
    }
    Ok(())
}

fn should_skip(kind: TreeKind, relative: &Path) -> bool {
    if relative
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| {
            name.ends_with(".db-wal") || name.ends_with(".db-shm") || name.ends_with(".db-journal")
        })
    {
        return true;
    }
    if kind == TreeKind::State {
        return relative.starts_with("logs")
            || relative == Path::new("telemetry_installation.json")
            || relative == Path::new("legacy-migration-v1.lock")
            || relative == Path::new("legacy-migration-v1.json");
    }
    kind == TreeKind::Config && relative == Path::new("secrets.yaml")
}

fn archive_target(kind: TreeKind, relative: &Path, target_root: &Path) -> PathBuf {
    if kind == TreeKind::Data && relative.starts_with(Path::new("mcp-platform")) {
        return target_root.join("legacy").join("v1").join(relative);
    }
    target_root.join(relative)
}

fn is_session_database(kind: TreeKind, relative: &Path) -> bool {
    kind == TreeKind::Data && relative == Path::new("sessions/sessions.db")
}

fn is_sqlite_database(relative: &Path) -> bool {
    relative
        .extension()
        .is_some_and(|extension| extension == "db")
}

fn is_yaml_to_transform(kind: TreeKind, relative: &Path) -> bool {
    kind == TreeKind::Config
        && relative
            .extension()
            .is_some_and(|extension| extension == "yaml" || extension == "yml")
}

fn is_desktop_settings(kind: TreeKind, relative: &Path) -> bool {
    kind == TreeKind::Desktop && relative == Path::new("settings.json")
}

fn is_json_to_transform(kind: TreeKind, relative: &Path) -> bool {
    relative
        .extension()
        .is_some_and(|extension| extension == "json")
        && !(kind == TreeKind::Data && relative.starts_with(Path::new("mcp-platform")))
}

fn transform_yaml_file(source: &Path, target: &Path, request: &MigrationRequest) -> Result<()> {
    let text = fs::read_to_string(source)?;
    let mut value: YamlValue = serde_yaml::from_str(&text)?;
    transform_yaml_value(&mut value, request);
    let output = serde_yaml::to_string(&value)?;
    write_bytes_atomically(target, output.as_bytes())
}

fn transform_yaml_value(value: &mut YamlValue, request: &MigrationRequest) {
    match value {
        YamlValue::Mapping(mapping) => {
            let old = std::mem::take(mapping);
            for (mut key, mut child) in old {
                if let YamlValue::String(text) = &mut key {
                    *text = migrate_key(text);
                }
                transform_yaml_value(&mut child, request);
                mapping.insert(key, child);
            }
        }
        YamlValue::Sequence(values) => {
            for child in values {
                transform_yaml_value(child, request);
            }
        }
        YamlValue::String(text) => {
            *text = migrate_string_value(text, request);
        }
        _ => {}
    }
}

fn migrate_key(key: &str) -> String {
    match key {
        "goose_provider" => "lumina_provider".to_string(),
        "goose_model" => "lumina_model".to_string(),
        "goose_mode" => "lumina_mode".to_string(),
        "externalGoosed" => "externalLuminad".to_string(),
        _ if key.starts_with("GOOSE_") => key.replacen("GOOSE_", "LUMINA_", 1),
        _ => key.to_string(),
    }
}

fn rewrite_path(value: &str, source: &Path, target: &Path) -> String {
    let source = source.to_string_lossy();
    if value.starts_with(source.as_ref()) {
        return value.replacen(source.as_ref(), target.to_string_lossy().as_ref(), 1);
    }
    value.to_string()
}

fn migrate_string_value(value: &str, request: &MigrationRequest) -> String {
    let mut migrated = match value {
        "goose" => "lumina".to_string(),
        "goosed" => "luminad".to_string(),
        _ if value.starts_with("goose://") => value.replacen("goose://", "lumina://", 1),
        _ => value.to_string(),
    };
    for source in &request.sources {
        migrated = rewrite_path(&migrated, &source.config, &request.target.config);
        migrated = rewrite_path(&migrated, &source.data, &request.target.data);
        migrated = rewrite_path(&migrated, &source.state, &request.target.state);
        migrated = rewrite_path(&migrated, &source.desktop, &request.target.desktop);
    }
    migrated
}

fn transform_json_file(source: &Path, target: &Path, request: &MigrationRequest) -> Result<()> {
    let text = fs::read_to_string(source)?;
    let mut value: JsonValue = serde_json::from_str(&text)?;
    transform_legacy_json_with_request(&mut value, request);
    write_json_atomically(target, &value)
}

fn transform_desktop_settings(source: &Path, target: &Path) -> Result<()> {
    let text = fs::read_to_string(source)?;
    let mut value: JsonValue = serde_json::from_str(&text)?;
    if let Some(object) = value.as_object_mut() {
        if !object.contains_key("externalLuminad") {
            if let Some(legacy) = object.remove("externalGoosed") {
                object.insert("externalLuminad".to_string(), legacy);
            }
        }
    }
    transform_legacy_json(&mut value);
    write_json_atomically(target, &value)
}

pub(crate) fn transform_legacy_json(value: &mut JsonValue) {
    match value {
        JsonValue::Object(object) => {
            let old = std::mem::take(object);
            for (key, mut child) in old {
                transform_legacy_json(&mut child);
                object.insert(migrate_key(&key), child);
            }
        }
        JsonValue::Array(values) => {
            for child in values {
                transform_legacy_json(child);
            }
        }
        _ => {}
    }
}

fn transform_legacy_json_with_request(value: &mut JsonValue, request: &MigrationRequest) {
    match value {
        JsonValue::Object(object) => {
            let old = std::mem::take(object);
            for (key, mut child) in old {
                transform_legacy_json_with_request(&mut child, request);
                object.insert(migrate_key(&key), child);
            }
        }
        JsonValue::Array(values) => {
            for child in values {
                transform_legacy_json_with_request(child, request);
            }
        }
        JsonValue::String(text) => {
            *text = migrate_string_value(text, request);
        }
        _ => {}
    }
}

fn migrate_secret_store(request: &MigrationRequest, report: &mut MigrationReport) -> Result<()> {
    let destination = keyring::Entry::new("lumina", "secrets")?;
    if destination.get_password().is_ok() {
        return Ok(());
    }

    let legacy = keyring::Entry::new("goose", "secrets")?;
    if let Ok(payload) = legacy.get_password() {
        if !request.dry_run {
            destination.set_password(&payload)?;
        }
        return Ok(());
    }

    for source in &request.sources {
        let secrets_file = source.config.join("secrets.yaml");
        if !secrets_file.is_file() {
            continue;
        }
        let payload = fs::read_to_string(&secrets_file)?;
        if !request.dry_run {
            destination.set_password(&payload)?;
        }
        report.warnings.push(
            "legacy plaintext secret storage was imported into the Lumina credential store; the source file was left unchanged"
                .to_string(),
        );
        return Ok(());
    }

    Ok(())
}

fn same_location(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn copy_file_atomically(source: &Path, target: &Path) -> Result<()> {
    let parent = target
        .parent()
        .context("migration target has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temporary = temporary_path(target);
    fs::copy(source, &temporary)?;
    fs::rename(&temporary, target)?;
    Ok(())
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_bytes_atomically(path, &bytes)
}

fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("migration target has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temporary = temporary_path(path);
    let mut file = File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn temporary_path(target: &Path) -> PathBuf {
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact");
    target.with_file_name(format!(
        ".{file_name}.lumina-migration-{}-{}.tmp",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        bytes += count as u64;
    }
    let digest = hasher.finalize();
    let sha256 = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok((sha256, bytes))
}
