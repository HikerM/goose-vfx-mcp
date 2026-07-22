use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{Stream, StreamExt as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_util::bytes::Bytes;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::manifest::ArchiveFormat;
use super::manifest::{Distribution, Entrypoint, Manifest};
use super::plan::ConnectionProjection;
use super::task::TaskOperation;

pub const MANAGED_DISTRIBUTION_ADAPTER_VERSION: &str = "1";
pub const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFetchEffect {
    pub operation_id: String,
    pub source_url: String,
    pub redirect_policy: RedirectPolicy,
    pub expected_sha256: String,
    pub expected_size_bytes: Option<u64>,
    pub maximum_size_bytes: u64,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedirectPolicy {
    DenyAll,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactVerificationEvidence {
    pub source_origin: String,
    pub artifact_digest: String,
    pub size_bytes: u64,
    pub adapter_id: String,
    pub adapter_version: String,
    pub platform_selector: String,
    pub verification_result: VerificationResult,
    pub installed_at_ms: i64,
    pub artifact_signature: ArtifactSignatureStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationResult {
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactSignatureStatus {
    NotDeclaredByManifestV1,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedArtifact {
    path: PathBuf,
    pub digest: String,
    pub size_bytes: u64,
}

impl FetchedArtifact {
    pub fn server_path(&self) -> &Path {
        &self.path
    }

    pub fn from_verified_cache_entry(
        path: PathBuf,
        digest: String,
        size_bytes: u64,
    ) -> McpPlatformResult<Self> {
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| repository_error())?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || is_reparse(&metadata)
            || metadata.len() != size_bytes
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(unsafe_fetch());
        }
        Ok(Self {
            path,
            digest,
            size_bytes,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveEntryKind {
    File,
    Directory,
    Symlink,
    Hardlink,
    ReparsePoint,
    Special,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntryMetadata {
    pub path: String,
    pub kind: ArchiveEntryKind,
    pub compressed_size: u64,
    pub expanded_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub maximum_files: usize,
    pub maximum_single_file_bytes: u64,
    pub maximum_total_bytes: u64,
    pub maximum_compression_ratio: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            maximum_files: 10_000,
            maximum_single_file_bytes: 256 * 1024 * 1024,
            maximum_total_bytes: 1024 * 1024 * 1024,
            maximum_compression_ratio: 200,
        }
    }
}

pub fn validate_archive_entries(
    entries: &[ArchiveEntryMetadata],
    limits: ArchiveLimits,
) -> McpPlatformResult<()> {
    if entries.len() > limits.maximum_files {
        return Err(unsafe_archive());
    }
    let mut total = 0_u64;
    let mut paths = HashSet::new();
    for entry in entries {
        if !matches!(
            entry.kind,
            ArchiveEntryKind::File | ArchiveEntryKind::Directory
        ) {
            return Err(unsafe_archive());
        }
        validate_archive_path(&entry.path)?;
        let folded = entry
            .path
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_lowercase();
        if !paths.insert(folded) {
            return Err(unsafe_archive());
        }
        if entry.expanded_size > limits.maximum_single_file_bytes {
            return Err(unsafe_archive());
        }
        total = total
            .checked_add(entry.expanded_size)
            .ok_or_else(unsafe_archive)?;
        if total > limits.maximum_total_bytes {
            return Err(unsafe_archive());
        }
        if entry.compressed_size == 0 {
            if entry.expanded_size > 0 {
                return Err(unsafe_archive());
            }
        } else {
            let maximum_expanded = entry
                .compressed_size
                .checked_mul(limits.maximum_compression_ratio)
                .ok_or_else(unsafe_archive)?;
            if entry.expanded_size > maximum_expanded {
                return Err(unsafe_archive());
            }
        }
    }
    Ok(())
}

pub fn validate_archive_path(value: &str) -> McpPlatformResult<PathBuf> {
    if value.is_empty()
        || value.starts_with(['/', '\\'])
        || value.starts_with("//")
        || value.starts_with("\\\\")
        || value.contains(':')
        || value.contains('\0')
    {
        return Err(unsafe_archive());
    }
    let normalized = value.replace('\\', "/");
    if normalized.contains("//") {
        return Err(unsafe_archive());
    }
    let without_trailing = normalized.strip_suffix('/').unwrap_or(&normalized);
    if without_trailing.is_empty() {
        return Err(unsafe_archive());
    }
    let path = Path::new(without_trailing);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(unsafe_archive());
    }
    if without_trailing.split('/').any(|component| {
        component.is_empty()
            || component == "."
            || component == ".."
            || unsafe_windows_component(component)
    }) {
        return Err(unsafe_archive());
    }
    Ok(path.to_path_buf())
}

fn unsafe_windows_component(component: &str) -> bool {
    if component.ends_with(['.', ' ']) {
        return true;
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(['.', ' '])
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.strip_prefix("COM").is_some_and(|value| {
            matches!(value, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || stem.strip_prefix("LPT").is_some_and(|value| {
            matches!(value, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
}

#[async_trait]
pub trait ArtifactFetcher: Send + Sync {
    async fn fetch(
        &self,
        effect: &ArtifactFetchEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<FetchedArtifact>;
}

pub struct ArtifactNetworkResponse {
    status_code: u16,
    content_length: Option<u64>,
    stream: Pin<Box<dyn Stream<Item = McpPlatformResult<Bytes>> + Send>>,
}

impl ArtifactNetworkResponse {
    pub fn new(
        status_code: u16,
        content_length: Option<u64>,
        stream: Pin<Box<dyn Stream<Item = McpPlatformResult<Bytes>> + Send>>,
    ) -> Self {
        Self {
            status_code,
            content_length,
            stream,
        }
    }
}

#[async_trait]
pub trait ArtifactNetworkClient: Send + Sync {
    async fn open(
        &self,
        effect: &ArtifactFetchEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ArtifactNetworkResponse>;
}

pub trait Verifier: Send + Sync {
    fn verify(
        &self,
        effect: &ArtifactFetchEffect,
        artifact: &FetchedArtifact,
    ) -> McpPlatformResult<()>;
}

#[async_trait]
pub trait Installer: Send + Sync {
    async fn materialize(
        &self,
        artifact: &FetchedArtifact,
        staging_root: &Path,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()>;
}

#[async_trait]
pub trait Activator: Send + Sync {
    async fn activate(
        &self,
        version_root: &Path,
        active_pointer: &Path,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()>;
}

#[async_trait]
pub trait Cleanup: Send + Sync {
    async fn remove_owned(
        &self,
        root: &Path,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()>;
}

#[derive(Debug, Clone)]
pub struct ManagedInstallEffect {
    pub task_id: String,
    pub managed_mcp_id: String,
    pub manifest: Manifest,
    pub source_url: String,
    pub expected_sha256: String,
    pub expected_size_bytes: Option<u64>,
    pub platform_selector: String,
    pub now_ms: i64,
    pub operation: TaskOperation,
    pub expected_tree_digest: Option<String>,
    pub rebuild_uncommitted_version: bool,
}

#[derive(Debug, Clone)]
pub struct ManagedInstallOutcome {
    pub installation_root: PathBuf,
    pub projection: ConnectionProjection,
    pub evidence: ArtifactVerificationEvidence,
    pub materialized_tree_digest: String,
    pub owned_relative_paths: Vec<String>,
    pub replaced_quarantine_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveRuntimeDescriptor {
    version: String,
    projection: ConnectionProjection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallationMarker {
    task_id: String,
    artifact_digest: String,
}

#[async_trait]
pub trait DistributionEffectAdapter: Send + Sync {
    fn adapter_version(&self) -> &'static str;
    async fn install(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome>;
    async fn inspect_installed(
        &self,
        effect: &ManagedInstallEffect,
    ) -> McpPlatformResult<ManagedInstallOutcome>;
    async fn version_exists(&self, managed_mcp_id: &str, version: &str) -> McpPlatformResult<bool>;
    async fn activate_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()>;
    async fn restore_activation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        task_id: &str,
    ) -> McpPlatformResult<()>;
    async fn active_version(&self, managed_mcp_id: &str) -> McpPlatformResult<Option<String>>;
    async fn remove_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()>;
    async fn quarantine_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<String>;
    async fn restore_quarantined(
        &self,
        managed_mcp_id: &str,
        version: &str,
        token: &str,
    ) -> McpPlatformResult<()>;
    async fn purge_quarantined(
        &self,
        token: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()>;
}

pub struct ProductionDistributionEffectAdapter {
    root: PathBuf,
    fetcher: Arc<dyn ArtifactFetcher>,
    verifier: Arc<dyn Verifier>,
    runtime: RuntimeCapabilities,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeCapabilities {
    node: Option<PathBuf>,
    python: Option<PathBuf>,
    python_major_minor: Option<(u8, u8)>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeCapabilitySnapshot {
    pub node_available: bool,
    pub python_major_minor: Option<(u8, u8)>,
}

impl RuntimeCapabilities {
    pub fn discover() -> Self {
        let node = resolve_executable(if cfg!(windows) { "node.exe" } else { "node" });
        let python = resolve_executable(if cfg!(windows) {
            "python.exe"
        } else {
            "python3"
        });
        let python_major_minor = python.as_deref().and_then(discover_python_version);
        Self {
            node,
            python,
            python_major_minor,
        }
    }

    pub fn for_test(
        node: Option<PathBuf>,
        python: Option<PathBuf>,
        python_major_minor: Option<(u8, u8)>,
    ) -> Self {
        Self {
            node,
            python,
            python_major_minor,
        }
    }
    pub fn snapshot(&self) -> RuntimeCapabilitySnapshot {
        RuntimeCapabilitySnapshot {
            node_available: self.node.is_some(),
            python_major_minor: self.python_major_minor,
        }
    }
}

impl ProductionDistributionEffectAdapter {
    pub fn new(root: PathBuf) -> McpPlatformResult<Self> {
        Self::new_with_runtime(root, RuntimeCapabilities::discover())
    }
    pub fn new_with_runtime(
        root: PathBuf,
        runtime: RuntimeCapabilities,
    ) -> McpPlatformResult<Self> {
        let (root, cache_root) = prepare_distribution_root(root)?;
        Ok(Self {
            fetcher: Arc::new(ProductionArtifactFetcher::new(cache_root)?),
            verifier: Arc::new(Sha256Verifier),
            root,
            runtime,
        })
    }

    pub fn new_with_ports(
        root: PathBuf,
        runtime: RuntimeCapabilities,
        fetcher: Arc<dyn ArtifactFetcher>,
        verifier: Arc<dyn Verifier>,
    ) -> McpPlatformResult<Self> {
        let (root, _) = prepare_distribution_root(root)?;
        Ok(Self {
            fetcher,
            verifier,
            root,
            runtime,
        })
    }

    pub fn with_runtime_capabilities(mut self, runtime: RuntimeCapabilities) -> Self {
        self.runtime = runtime;
        self
    }

    fn version_root(&self, managed_mcp_id: &str, version: &str) -> McpPlatformResult<PathBuf> {
        validate_storage_segment(managed_mcp_id)?;
        validate_storage_segment(version)?;
        Ok(self
            .root
            .join("installations")
            .join(managed_mcp_id)
            .join("versions")
            .join(version))
    }

    fn active_descriptor_path(&self, managed_mcp_id: &str) -> McpPlatformResult<PathBuf> {
        validate_storage_segment(managed_mcp_id)?;
        Ok(self
            .root
            .join("installations")
            .join(managed_mcp_id)
            .join("active.json"))
    }

    async fn verify_materialized_outcome(
        &self,
        effect: &ManagedInstallEffect,
        expected_tree_digest: &str,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        let (adapter_id, _, _, entrypoint, timeout) =
            distribution_materialization(&effect.manifest)?;
        let root = self.version_root(&effect.managed_mcp_id, effect.manifest.version.as_str())?;
        prepare_owned_directory(&self.root, &root, false)?;
        let evidence_path = root.join(".goose-artifact-evidence.json");
        let bytes = read_limited(&evidence_path, MAX_METADATA_BYTES)
            .await
            .map_err(|_| verification_error())?;
        let evidence: ArtifactVerificationEvidence =
            serde_json::from_slice(&bytes).map_err(|_| verification_error())?;
        if evidence.source_origin != source_origin(&effect.source_url)?
            || evidence.artifact_digest != effect.expected_sha256
            || effect
                .expected_size_bytes
                .is_some_and(|size| size != evidence.size_bytes)
            || evidence.adapter_id != adapter_id
            || evidence.adapter_version != MANAGED_DISTRIBUTION_ADAPTER_VERSION
            || evidence.platform_selector != effect.platform_selector
            || evidence.verification_result != VerificationResult::Verified
            || evidence.installed_at_ms != effect.now_ms
            || evidence.artifact_signature != ArtifactSignatureStatus::NotDeclaredByManifestV1
        {
            return Err(verification_error());
        }
        let entrypoint_path = resolve_verified_entrypoint(
            adapter_id,
            &effect.manifest,
            &root,
            &self.runtime,
            &effect.platform_selector,
            false,
        )
        .await?;
        verify_regular_contained(&root, &entrypoint_path).await?;
        let expected_projection = runtime_projection(
            &self.runtime,
            adapter_id,
            &effect.manifest,
            &root,
            &entrypoint_path,
            entrypoint,
            timeout,
        )?;
        let descriptor = read_runtime_descriptor(&root).await?;
        if descriptor.version != effect.manifest.version.as_str()
            || descriptor.projection != expected_projection
        {
            return Err(verification_error());
        }
        verify_installation_marker(&root, &effect.task_id, &effect.expected_sha256).await?;
        let materialized_tree_digest = compute_materialized_tree_digest(&root).await?;
        if materialized_tree_digest != expected_tree_digest {
            return Err(verification_error());
        }
        let replaced_quarantine_token = if let Some(token) = repair_token(effect) {
            owned_directory_exists(&self.root, &self.root.join("quarantine").join(&token))
                .await?
                .then_some(token)
        } else {
            None
        };
        Ok(ManagedInstallOutcome {
            installation_root: root,
            projection: expected_projection,
            evidence,
            materialized_tree_digest,
            owned_relative_paths: vec![".".to_string()],
            replaced_quarantine_token,
        })
    }
}

fn prepare_distribution_root(root: PathBuf) -> McpPlatformResult<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(&root).map_err(|_| repository_error())?;
    verify_normal_directory(&root)?;
    let root = std::fs::canonicalize(root).map_err(|_| repository_error())?;
    let cache_root = root.join("cache");
    prepare_owned_directory(&root, &cache_root, true)?;
    Ok((root, cache_root))
}

#[async_trait]
impl DistributionEffectAdapter for ProductionDistributionEffectAdapter {
    fn adapter_version(&self) -> &'static str {
        MANAGED_DISTRIBUTION_ADAPTER_VERSION
    }

    async fn install(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        let (adapter_id, format, strip, entrypoint, timeout) =
            distribution_materialization(&effect.manifest)?;
        let fetch = ArtifactFetchEffect {
            operation_id: effect.task_id.clone(),
            source_url: effect.source_url.clone(),
            redirect_policy: RedirectPolicy::DenyAll,
            expected_sha256: effect.expected_sha256.clone(),
            expected_size_bytes: effect.expected_size_bytes,
            maximum_size_bytes: DEFAULT_MAX_DOWNLOAD_BYTES,
            timeout_seconds: 120,
        };
        let artifact = self.fetcher.fetch(&fetch, cancellation).await?;
        self.verifier.verify(&fetch, &artifact)?;
        let evidence = ArtifactVerificationEvidence {
            source_origin: source_origin(&effect.source_url)?,
            artifact_digest: artifact.digest.clone(),
            size_bytes: artifact.size_bytes,
            adapter_id: adapter_id.to_string(),
            adapter_version: MANAGED_DISTRIBUTION_ADAPTER_VERSION.to_string(),
            platform_selector: effect.platform_selector.clone(),
            verification_result: VerificationResult::Verified,
            installed_at_ms: effect.now_ms,
            artifact_signature: ArtifactSignatureStatus::NotDeclaredByManifestV1,
        };
        let version = effect.manifest.version.as_str();
        let version_root = self.version_root(&effect.managed_mcp_id, version)?;
        let staging_root = self
            .root
            .join("staging")
            .join(format!("{}-{}", effect.managed_mcp_id, effect.task_id));
        prepare_owned_directory(&self.root, &version_root, false)?;
        prepare_owned_directory(&self.root, &staging_root, false)?;
        let repair_token = repair_token(effect);
        if effect.operation == TaskOperation::Repair {
            let token = repair_token.as_deref().ok_or_else(integrity_error)?;
            let quarantine_root = self.root.join("quarantine").join(token);
            let version_exists = owned_directory_exists(&self.root, &version_root).await?;
            let quarantine_exists = owned_directory_exists(&self.root, &quarantine_root).await?;
            match (version_exists, quarantine_exists) {
                (true, false) => {
                    let actual = self
                        .quarantine_version(
                            &effect.managed_mcp_id,
                            version,
                            &format!("{}-repair", effect.task_id),
                            cancellation,
                        )
                        .await?;
                    if actual != token {
                        return Err(integrity_error());
                    }
                }
                (true, true) => {
                    remove_owned_tree(&self.root, &version_root).await?;
                }
                (false, true) => {}
                (false, false) => {}
            }
        }
        if tokio::fs::symlink_metadata(&version_root).await.is_ok()
            && effect.operation != TaskOperation::Repair
            && effect.expected_tree_digest.is_none()
        {
            if !effect.rebuild_uncommitted_version {
                return Err(verification_error());
            }
            remove_owned_tree(&self.root, &version_root).await?;
        }
        let mut materialized_tree_digest = effect.expected_tree_digest.clone();
        if tokio::fs::symlink_metadata(&version_root).await.is_err() {
            if tokio::fs::metadata(&staging_root).await.is_ok() {
                tokio::fs::remove_dir_all(&staging_root)
                    .await
                    .map_err(|_| repository_error())?;
            }
            ArchiveInstaller {
                format,
                strip_components: strip,
                limits: ArchiveLimits::default(),
            }
            .materialize(&artifact, &staging_root, cancellation)
            .await?;
            let staged_entrypoint = resolve_verified_entrypoint(
                adapter_id,
                &effect.manifest,
                &staging_root,
                &self.runtime,
                &effect.platform_selector,
                true,
            )
            .await?;
            verify_regular_contained(&staging_root, &staged_entrypoint).await?;
            set_minimum_entrypoint_permissions(adapter_id, &staged_entrypoint).await?;
            let relative_entrypoint = staged_entrypoint
                .strip_prefix(&staging_root)
                .map_err(|_| unsafe_archive())?;
            let installed_entrypoint = version_root.join(relative_entrypoint);
            let projection = runtime_projection(
                &self.runtime,
                adapter_id,
                &effect.manifest,
                &version_root,
                &installed_entrypoint,
                entrypoint,
                timeout,
            )?;
            let runtime_descriptor = ActiveRuntimeDescriptor {
                version: version.to_string(),
                projection,
            };
            tokio::fs::write(
                staging_root.join(".goose-runtime-descriptor.json"),
                serde_json::to_vec(&runtime_descriptor).map_err(|_| repository_error())?,
            )
            .await
            .map_err(|_| repository_error())?;
            let evidence_path = staging_root.join(".goose-artifact-evidence.json");
            if tokio::fs::metadata(&evidence_path).await.is_ok() {
                return Err(unsafe_archive());
            }
            tokio::fs::write(
                &evidence_path,
                serde_json::to_vec(&evidence).map_err(|_| repository_error())?,
            )
            .await
            .map_err(|_| repository_error())?;
            let marker = InstallationMarker {
                task_id: effect.task_id.clone(),
                artifact_digest: effect.expected_sha256.clone(),
            };
            tokio::fs::write(
                staging_root.join(".goose-installation-owner.json"),
                serde_json::to_vec(&marker).map_err(|_| repository_error())?,
            )
            .await
            .map_err(|_| repository_error())?;
            materialized_tree_digest = Some(compute_materialized_tree_digest(&staging_root).await?);
            if let Some(parent) = version_root.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|_| repository_error())?;
            }
            tokio::fs::rename(&staging_root, &version_root)
                .await
                .map_err(|_| repository_error())?;
        }
        let materialized_tree_digest = materialized_tree_digest.ok_or_else(integrity_error)?;
        self.verify_materialized_outcome(effect, &materialized_tree_digest)
            .await
    }

    async fn inspect_installed(
        &self,
        effect: &ManagedInstallEffect,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        let expected = effect
            .expected_tree_digest
            .as_deref()
            .ok_or_else(integrity_error)?;
        self.verify_materialized_outcome(effect, expected).await
    }

    async fn version_exists(&self, managed_mcp_id: &str, version: &str) -> McpPlatformResult<bool> {
        let root = self.version_root(managed_mcp_id, version)?;
        prepare_owned_directory(&self.root, &root, false)?;
        match tokio::fs::symlink_metadata(root).await {
            Ok(metadata)
                if metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && !is_reparse(&metadata) =>
            {
                Ok(true)
            }
            Ok(_) => Err(unsafe_archive()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(repository_error()),
        }
    }

    async fn activate_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        validate_storage_segment(task_id)?;
        let version_root = self.version_root(managed_mcp_id, version)?;
        prepare_owned_directory(&self.root, &version_root, false)?;
        let descriptor = read_runtime_descriptor(&version_root).await?;
        if descriptor.version != version {
            return Err(integrity_error());
        }
        let destination = self.active_descriptor_path(managed_mcp_id)?;
        let parent = destination.parent().ok_or_else(integrity_error)?;
        prepare_owned_directory(&self.root, parent, true)?;
        verify_normal_file_or_absent(&destination)?;
        if let Ok(current) = read_limited(&destination, MAX_METADATA_BYTES).await {
            if serde_json::from_slice::<ActiveRuntimeDescriptor>(&current)
                .ok()
                .as_ref()
                == Some(&descriptor)
            {
                return Ok(());
            }
        }
        let temporary = parent.join(format!(".active-{task_id}.partial"));
        verify_normal_file_or_absent(&temporary)?;
        let result = async {
            let mut file = tokio::fs::File::create(&temporary)
                .await
                .map_err(|_| repository_error())?;
            file.write_all(&serde_json::to_vec(&descriptor).map_err(|_| repository_error())?)
                .await
                .map_err(|_| repository_error())?;
            file.sync_all().await.map_err(|_| repository_error())?;
            drop(file);
            atomic_replace(&temporary, &destination).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        result
    }

    async fn restore_activation(
        &self,
        managed_mcp_id: &str,
        previous_version: Option<&str>,
        task_id: &str,
    ) -> McpPlatformResult<()> {
        match previous_version {
            Some(version) => {
                self.activate_version(managed_mcp_id, version, task_id, &CancellationToken::new())
                    .await
            }
            None => {
                let path = self.active_descriptor_path(managed_mcp_id)?;
                if let Some(parent) = path.parent() {
                    prepare_owned_directory(&self.root, parent, false)?;
                }
                verify_normal_file_or_absent(&path)?;
                match tokio::fs::remove_file(path).await {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(_) => Err(repository_error()),
                }
            }
        }
    }

    async fn active_version(&self, managed_mcp_id: &str) -> McpPlatformResult<Option<String>> {
        let path = self.active_descriptor_path(managed_mcp_id)?;
        if let Some(parent) = path.parent() {
            prepare_owned_directory(&self.root, parent, false)?;
        }
        verify_normal_file_or_absent(&path)?;
        match tokio::fs::metadata(&path).await {
            Ok(_) => {
                let bytes = read_limited(&path, MAX_METADATA_BYTES).await?;
                Ok(Some(
                    serde_json::from_slice::<ActiveRuntimeDescriptor>(&bytes)
                        .map_err(|_| integrity_error())?
                        .version,
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(repository_error()),
        }
    }

    async fn remove_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        let root = self.version_root(managed_mcp_id, version)?;
        prepare_owned_directory(&self.root, &root, false)?;
        match tokio::fs::remove_dir_all(root).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(repository_error()),
        }
    }

    async fn quarantine_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<String> {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        validate_storage_segment(task_id)?;
        let source = self.version_root(managed_mcp_id, version)?;
        let token = format!("{managed_mcp_id}-{version}-{task_id}");
        validate_storage_segment(&token)?;
        let target = self.root.join("quarantine").join(&token);
        prepare_owned_directory(&self.root, &source, false)?;
        prepare_owned_directory(&self.root, &target, false)?;
        if tokio::fs::metadata(&target).await.is_ok() {
            return Ok(token);
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|_| repository_error())?;
        }
        tokio::fs::rename(source, target)
            .await
            .map_err(|_| repository_error())?;
        Ok(token)
    }

    async fn restore_quarantined(
        &self,
        managed_mcp_id: &str,
        version: &str,
        token: &str,
    ) -> McpPlatformResult<()> {
        validate_storage_segment(token)?;
        let source = self.root.join("quarantine").join(token);
        let target = self.version_root(managed_mcp_id, version)?;
        prepare_owned_directory(&self.root, &source, false)?;
        prepare_owned_directory(&self.root, &target, false)?;
        if tokio::fs::metadata(&target).await.is_ok() && tokio::fs::metadata(&source).await.is_err()
        {
            return Ok(());
        }
        if tokio::fs::metadata(&target).await.is_ok() {
            tokio::fs::remove_dir_all(&target)
                .await
                .map_err(|_| repository_error())?;
        }
        tokio::fs::rename(source, target)
            .await
            .map_err(|_| repository_error())
    }

    async fn purge_quarantined(
        &self,
        token: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        validate_storage_segment(token)?;
        let target = self.root.join("quarantine").join(token);
        prepare_owned_directory(&self.root, &target, false)?;
        match tokio::fs::remove_dir_all(target).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(repository_error()),
        }
    }
}

fn distribution_materialization(
    manifest: &Manifest,
) -> McpPlatformResult<(&'static str, ArchiveFormat, u8, &Entrypoint, Option<u64>)> {
    let timeout = match manifest.transport {
        super::manifest::Transport::Stdio {
            startup_timeout_seconds,
        } => startup_timeout_seconds,
        _ => {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::TransportMismatch,
                "managed distribution requires stdio transport",
            ))
        }
    };
    match &manifest.distribution {
        Distribution::Npm { entrypoint, .. } => {
            Ok(("npm", ArchiveFormat::TarGz, 1, entrypoint, timeout))
        }
        Distribution::PythonWheel { entrypoint, .. } => {
            Ok(("python_wheel", ArchiveFormat::Zip, 0, entrypoint, timeout))
        }
        Distribution::BinaryArchive {
            archive_format,
            strip_components,
            entrypoint,
            ..
        } => Ok((
            "binary_archive",
            *archive_format,
            *strip_components,
            entrypoint,
            timeout,
        )),
        _ => Err(McpPlatformError::new(
            McpPlatformErrorCode::UnknownAdapter,
            "manifest is not a managed distribution",
        )),
    }
}

fn resolve_entrypoint(root: &Path, entrypoint: &Entrypoint) -> McpPlatformResult<PathBuf> {
    resolve_managed_path(root, &entrypoint.executable)
}

async fn resolve_verified_entrypoint(
    adapter_id: &str,
    manifest: &Manifest,
    root: &Path,
    runtime: &RuntimeCapabilities,
    platform_selector: &str,
    materialize_runtime_helpers: bool,
) -> McpPlatformResult<PathBuf> {
    match (&manifest.distribution, adapter_id) {
        (
            Distribution::Npm {
                package,
                package_version,
                entrypoint,
                ..
            },
            "npm",
        ) => {
            let bytes = read_limited(&root.join("package.json"), MAX_METADATA_BYTES)
                .await
                .map_err(|_| verification_error())?;
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| verification_error())?;
            if value.get("name").and_then(serde_json::Value::as_str) != Some(package)
                || value.get("version").and_then(serde_json::Value::as_str)
                    != Some(package_version.as_str())
            {
                return Err(verification_error());
            }
            for key in ["dependencies", "optionalDependencies", "peerDependencies"] {
                match value.get(key) {
                    None => {}
                    Some(serde_json::Value::Object(items)) if items.is_empty() => {}
                    Some(_) => return Err(dependency_denied()),
                }
            }
            match value.get("scripts") {
                None => {}
                Some(serde_json::Value::Object(items)) if items.is_empty() => {}
                Some(_) => return Err(dependency_denied()),
            }
            if value.get("bundleDependencies").is_some()
                || value.get("bundledDependencies").is_some()
            {
                return Err(dependency_denied());
            }
            let bin_name = logical_bin_name(&entrypoint.executable)?;
            let bin_path = match value.get("bin") {
                Some(serde_json::Value::String(path))
                    if unscoped_package_name(package) == bin_name =>
                {
                    path.as_str()
                }
                Some(serde_json::Value::Object(entries)) => entries
                    .get(bin_name)
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(verification_error)?,
                _ => return Err(verification_error()),
            };
            let path = resolve_managed_path(root, bin_path.trim_start_matches("./"))?;
            verify_regular_contained(root, &path).await?;
            Ok(path)
        }
        (
            Distribution::PythonWheel {
                package,
                package_version,
                entrypoint,
                ..
            },
            "python_wheel",
        ) => {
            let dist_info = find_single_dist_info(root).await?;
            let bytes = read_limited(&dist_info.join("METADATA"), MAX_METADATA_BYTES)
                .await
                .map_err(|_| verification_error())?;
            let text = std::str::from_utf8(&bytes).map_err(|_| verification_error())?;
            let headers = parse_package_headers(text)?;
            if headers.contains_key("requires-dist") {
                return Err(dependency_denied());
            }
            let names = headers.get("name").ok_or_else(verification_error)?;
            let versions = headers.get("version").ok_or_else(verification_error)?;
            if names.len() != 1 || versions.len() != 1 {
                return Err(verification_error());
            }
            let normalize = |value: &str| value.replace(['-', '.', '_'], "_").to_ascii_lowercase();
            if normalize(&names[0]) != normalize(package) || versions[0] != package_version.as_str()
            {
                return Err(verification_error());
            }
            let wheel = String::from_utf8(
                read_limited(&dist_info.join("WHEEL"), MAX_METADATA_BYTES)
                    .await
                    .map_err(|_| verification_error())?,
            )
            .map_err(|_| verification_error())?;
            let wheel_headers = parse_package_headers(&wheel)?;
            let tags = wheel_headers.get("tag").ok_or_else(verification_error)?;
            if !tags
                .iter()
                .any(|tag| wheel_artifact_tag_compatible(tag, runtime, platform_selector))
            {
                return Err(runtime_incompatible());
            }
            let bin_name = logical_bin_name(&entrypoint.executable)?;
            let entry_points = String::from_utf8(
                read_limited(&dist_info.join("entry_points.txt"), MAX_METADATA_BYTES)
                    .await
                    .map_err(|_| verification_error())?,
            )
            .map_err(|_| verification_error())?;
            let target = parse_console_script(&entry_points, bin_name)?;
            let launcher = root.join(".goose-wheel-launcher.py");
            let target_path = root.join(".goose-wheel-target.json");
            let target_bytes = serde_json::to_vec(&target).map_err(|_| repository_error())?;
            if materialize_runtime_helpers {
                tokio::fs::write(&launcher, WHEEL_LAUNCHER)
                    .await
                    .map_err(|_| repository_error())?;
                tokio::fs::write(&target_path, &target_bytes)
                    .await
                    .map_err(|_| repository_error())?;
            } else if read_limited(&launcher, MAX_METADATA_BYTES).await?
                != WHEEL_LAUNCHER.as_bytes()
                || read_limited(&target_path, MAX_METADATA_BYTES).await? != target_bytes
            {
                return Err(verification_error());
            }
            Ok(launcher)
        }
        (Distribution::BinaryArchive { entrypoint, .. }, "binary_archive") => {
            resolve_entrypoint(root, entrypoint)
        }
        _ => Err(verification_error()),
    }
}

fn parse_package_headers(contents: &str) -> McpPlatformResult<HashMap<String, Vec<String>>> {
    let mut headers: HashMap<String, Vec<String>> = HashMap::new();
    let mut previous: Option<(String, usize)> = None;
    for line in contents.lines() {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            let (name, index) = previous.clone().ok_or_else(verification_error)?;
            let values = headers.get_mut(&name).ok_or_else(verification_error)?;
            values[index].push(' ');
            values[index].push_str(line.trim());
            continue;
        }
        let (name, value) = line.split_once(':').ok_or_else(verification_error)?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(verification_error());
        }
        let name = name.to_ascii_lowercase();
        let values = headers.entry(name.clone()).or_default();
        values.push(value.trim().to_string());
        previous = Some((name, values.len() - 1));
    }
    Ok(headers)
}

fn wheel_artifact_tag_compatible(
    tag: &str,
    runtime: &RuntimeCapabilities,
    platform_selector: &str,
) -> bool {
    let parts = tag.split('-').collect::<Vec<_>>();
    if parts.len() != 3 {
        return false;
    }
    let Some((major, minor)) = runtime.python_major_minor else {
        return false;
    };
    let python = parts[0];
    let abi = parts[1];
    let platform = parts[2];
    let python_compatible = python.split('.').any(|value| {
        if value == "py3" {
            major == 3 && abi == "none"
        } else {
            let exact = format!("cp{major}{minor}");
            value == exact && (abi == exact || matches!(abi, "none" | "abi3"))
        }
    });
    if !python_compatible {
        return false;
    }
    match platform_selector {
        "any/any" => platform.split('.').any(|value| value == "any"),
        "windows/x86_64" => platform.split('.').any(|value| value == "win_amd64"),
        "windows/aarch64" => platform.split('.').any(|value| value == "win_arm64"),
        "linux/x86_64" => platform
            .split('.')
            .any(|value| value == "linux_x86_64" || value.ends_with("linux_x86_64")),
        "linux/aarch64" => platform
            .split('.')
            .any(|value| value == "linux_aarch64" || value.ends_with("linux_aarch64")),
        "macos/x86_64" => platform.split('.').any(|value| {
            value.starts_with("macosx_")
                && (value.ends_with("_x86_64") || value.ends_with("_universal2"))
        }),
        "macos/aarch64" => platform.split('.').any(|value| {
            value.starts_with("macosx_")
                && (value.ends_with("_arm64") || value.ends_with("_universal2"))
        }),
        _ => false,
    }
}

fn logical_bin_name(executable: &str) -> McpPlatformResult<&str> {
    let value = executable
        .strip_prefix("${installation.bin}/")
        .ok_or_else(verification_error)?;
    if value.is_empty() || value.contains(['/', '\\', ':']) || value == "." || value == ".." {
        return Err(verification_error());
    }
    Ok(value)
}

fn unscoped_package_name(package: &str) -> &str {
    package.rsplit('/').next().unwrap_or(package)
}

async fn find_single_dist_info(root: &Path) -> McpPlatformResult<PathBuf> {
    let mut directories = tokio::fs::read_dir(root)
        .await
        .map_err(|_| verification_error())?;
    let mut found = None;
    while let Some(entry) = directories
        .next_entry()
        .await
        .map_err(|_| verification_error())?
    {
        if entry
            .file_type()
            .await
            .map_err(|_| verification_error())?
            .is_dir()
            && entry.file_name().to_string_lossy().ends_with(".dist-info")
        {
            if found.is_some() {
                return Err(verification_error());
            }
            found = Some(entry.path());
        }
    }
    found.ok_or_else(verification_error)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WheelConsoleTarget {
    module: String,
    function: String,
}

fn parse_console_script(
    contents: &str,
    expected_name: &str,
) -> McpPlatformResult<WheelConsoleTarget> {
    let mut in_console_scripts = false;
    let mut found = None;
    for raw in contents.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_console_scripts = line == "[console_scripts]";
            continue;
        }
        if !in_console_scripts || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(verification_error());
        };
        if name.trim() != expected_name {
            continue;
        }
        if found.is_some() {
            return Err(verification_error());
        }
        let (module, function) = value
            .trim()
            .split_once(':')
            .ok_or_else(verification_error)?;
        if !valid_python_path(module) || !valid_python_path(function) {
            return Err(verification_error());
        }
        found = Some(WheelConsoleTarget {
            module: module.to_string(),
            function: function.to_string(),
        });
    }
    found.ok_or_else(verification_error)
}

fn valid_python_path(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|segment| {
            let mut chars = segment.chars();
            chars
                .next()
                .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
                && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
        })
}

const WHEEL_LAUNCHER: &str = "import importlib,json,pathlib,sys\nroot=pathlib.Path(__file__).resolve().parent\ntarget=json.loads((root/'.goose-wheel-target.json').read_text(encoding='utf-8'))\nsys.path.insert(0,str(root))\nobj=importlib.import_module(target['module'])\nfor part in target['function'].split('.'):\n obj=getattr(obj,part)\nraise SystemExit(obj())\n";

fn resolve_managed_path(root: &Path, value: &str) -> McpPlatformResult<PathBuf> {
    let value = value
        .strip_prefix("${installation.root}/")
        .or_else(|| value.strip_prefix("${installation.bin}/"))
        .unwrap_or(value);
    let relative = validate_archive_path(value)?;
    Ok(root.join(relative))
}

async fn verify_regular_contained(root: &Path, path: &Path) -> McpPlatformResult<()> {
    if !path.starts_with(root) {
        return Err(unsafe_archive());
    }
    if let Some(parent) = path.parent() {
        prepare_owned_directory(root, parent, false)?;
    }
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| verification_error())?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() || is_reparse(&metadata)
    {
        return Err(verification_error());
    }
    Ok(())
}

async fn set_minimum_entrypoint_permissions(
    adapter_id: &str,
    path: &Path,
) -> McpPlatformResult<()> {
    #[cfg(unix)]
    if adapter_id == "binary_archive" {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| repository_error())?;
    }
    #[cfg(not(unix))]
    let _ = (adapter_id, path);
    Ok(())
}

fn validate_storage_segment(value: &str) -> McpPlatformResult<()> {
    if value.is_empty() || value.contains(['/', '\\', ':']) || value == "." || value == ".." {
        return Err(unsafe_archive());
    }
    Ok(())
}

fn prepare_owned_directory(
    owner_root: &Path,
    target: &Path,
    create_target: bool,
) -> McpPlatformResult<()> {
    let relative = target
        .strip_prefix(owner_root)
        .map_err(|_| unsafe_archive())?;
    verify_normal_directory(owner_root)?;
    let mut current = owner_root.to_path_buf();
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        if !matches!(component, Component::Normal(_)) {
            return Err(unsafe_archive());
        }
        current.push(component.as_os_str());
        let should_exist = create_target || index + 1 < component_count;
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse(&metadata)
                {
                    return Err(unsafe_archive());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && should_exist => {
                std::fs::create_dir(&current).map_err(|_| repository_error())?;
                verify_normal_directory(&current)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(repository_error()),
        }
    }
    Ok(())
}

fn verify_normal_directory(path: &Path) -> McpPlatformResult<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| repository_error())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse(&metadata) {
        return Err(unsafe_archive());
    }
    Ok(())
}

fn verify_normal_file_or_absent(path: &Path) -> McpPlatformResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_file()
                && !metadata.file_type().is_symlink()
                && !is_reparse(&metadata) =>
        {
            Ok(())
        }
        Ok(_) => Err(unsafe_archive()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(repository_error()),
    }
}

fn source_origin(value: &str) -> McpPlatformResult<String> {
    let url = Url::parse(value).map_err(|_| unsafe_fetch())?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(unsafe_fetch());
    }
    let host = url.host_str().ok_or_else(unsafe_fetch)?;
    Ok(match url.port() {
        Some(port) => format!("https://{host}:{port}"),
        None => format!("https://{host}"),
    })
}

fn repair_token(effect: &ManagedInstallEffect) -> Option<String> {
    (effect.operation == TaskOperation::Repair).then(|| {
        format!(
            "{}-{}-{}-repair",
            effect.managed_mcp_id,
            effect.manifest.version.as_str(),
            effect.task_id
        )
    })
}

fn resolve_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find_map(|candidate| {
            let metadata = std::fs::metadata(&candidate).ok()?;
            if metadata.is_file() {
                std::fs::canonicalize(candidate).ok()
            } else {
                None
            }
        })
}

fn discover_python_version(executable: &Path) -> Option<(u8, u8)> {
    let output = std::process::Command::new(executable)
        .args([
            "-I",
            "-c",
            "import sys;print(f'{sys.version_info.major}.{sys.version_info.minor}')",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = std::str::from_utf8(&output.stdout).ok()?.trim();
    let (major, minor) = value.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn runtime_projection(
    runtime: &RuntimeCapabilities,
    adapter_id: &str,
    manifest: &Manifest,
    root: &Path,
    entrypoint_path: &Path,
    entrypoint: &Entrypoint,
    timeout_seconds: Option<u64>,
) -> McpPlatformResult<ConnectionProjection> {
    let (executable, mut args) = match adapter_id {
        "npm" => (
            runtime
                .node
                .as_ref()
                .ok_or_else(runtime_incompatible)?
                .to_string_lossy()
                .into_owned(),
            vec![entrypoint_path.to_string_lossy().into_owned()],
        ),
        "python_wheel" => (
            runtime
                .python
                .as_ref()
                .ok_or_else(runtime_incompatible)?
                .to_string_lossy()
                .into_owned(),
            vec![
                "-I".to_string(),
                entrypoint_path.to_string_lossy().into_owned(),
            ],
        ),
        "binary_archive" => (entrypoint_path.to_string_lossy().into_owned(), Vec::new()),
        _ => return Err(runtime_incompatible()),
    };
    args.extend(entrypoint.args.clone());
    Ok(ConnectionProjection::ManagedStdio {
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        executable,
        args,
        environment_keys: entrypoint.environment_keys.clone(),
        cwd: match &entrypoint.cwd {
            Some(value) => Some(
                resolve_managed_path(root, value)?
                    .to_string_lossy()
                    .into_owned(),
            ),
            None => Some(root.to_string_lossy().into_owned()),
        },
        timeout_seconds,
    })
}

async fn read_runtime_descriptor(root: &Path) -> McpPlatformResult<ActiveRuntimeDescriptor> {
    let bytes = read_limited(
        &root.join(".goose-runtime-descriptor.json"),
        MAX_METADATA_BYTES,
    )
    .await
    .map_err(|_| verification_error())?;
    serde_json::from_slice(&bytes).map_err(|_| verification_error())
}

async fn verify_installation_marker(
    root: &Path,
    task_id: &str,
    artifact_digest: &str,
) -> McpPlatformResult<()> {
    let bytes = read_limited(
        &root.join(".goose-installation-owner.json"),
        MAX_METADATA_BYTES,
    )
    .await
    .map_err(|_| verification_error())?;
    let marker: InstallationMarker =
        serde_json::from_slice(&bytes).map_err(|_| verification_error())?;
    if marker.task_id != task_id || marker.artifact_digest != artifact_digest {
        return Err(verification_error());
    }
    Ok(())
}

async fn read_limited(path: &Path, maximum: u64) -> McpPlatformResult<Vec<u8>> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| repository_error())?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || is_reparse(&metadata)
        || metadata.len() > maximum
    {
        return Err(verification_error());
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| repository_error())?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(metadata.len()).map_err(|_| verification_error())?);
    let mut limited = (&mut file).take(maximum + 1);
    limited
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| repository_error())?;
    if bytes.len() as u64 > maximum || bytes.len() as u64 != metadata.len() {
        return Err(verification_error());
    }
    Ok(bytes)
}

async fn compute_materialized_tree_digest(root: &Path) -> McpPlatformResult<String> {
    verify_normal_directory(root)?;
    let mut pending = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut reader = tokio::fs::read_dir(&directory)
            .await
            .map_err(|_| repository_error())?;
        while let Some(entry) = reader.next_entry().await.map_err(|_| repository_error())? {
            let path = entry.path();
            let metadata = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|_| repository_error())?;
            if metadata.file_type().is_symlink() || is_reparse(&metadata) {
                return Err(verification_error());
            }
            let relative = path.strip_prefix(root).map_err(|_| verification_error())?;
            let relative = relative
                .components()
                .map(|component| match component {
                    Component::Normal(value) => value.to_str().ok_or_else(verification_error),
                    _ => Err(verification_error()),
                })
                .collect::<McpPlatformResult<Vec<_>>>()?
                .join("/");
            if metadata.is_dir() {
                entries.push((relative, b'd', 0_u64, None));
                pending.push(path);
            } else if metadata.is_file() {
                let size = metadata.len();
                let mut file = tokio::fs::File::open(&path)
                    .await
                    .map_err(|_| repository_error())?;
                let mut file_hasher = Sha256::new();
                let mut observed = 0_u64;
                let mut buffer = vec![0_u8; 64 * 1024];
                loop {
                    let read = file
                        .read(&mut buffer)
                        .await
                        .map_err(|_| repository_error())?;
                    if read == 0 {
                        break;
                    }
                    observed = observed
                        .checked_add(read as u64)
                        .ok_or_else(verification_error)?;
                    file_hasher.update(&buffer[..read]);
                }
                if observed != size {
                    return Err(verification_error());
                }
                entries.push((
                    relative,
                    b'f',
                    size,
                    Some(hex_digest(file_hasher.finalize().as_slice())),
                ));
            } else {
                return Err(verification_error());
            }
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut tree_hasher = Sha256::new();
    tree_hasher.update(b"goose-managed-tree-v1\0");
    for (relative, kind, size, digest) in entries {
        let path_bytes = relative.as_bytes();
        tree_hasher.update((path_bytes.len() as u64).to_be_bytes());
        tree_hasher.update(path_bytes);
        tree_hasher.update([kind]);
        tree_hasher.update(size.to_be_bytes());
        if let Some(digest) = digest {
            tree_hasher.update(digest.as_bytes());
        }
    }
    Ok(hex_digest(tree_hasher.finalize().as_slice()))
}

async fn remove_owned_tree(owner_root: &Path, target: &Path) -> McpPlatformResult<()> {
    prepare_owned_directory(owner_root, target, false)?;
    let metadata = tokio::fs::symlink_metadata(target)
        .await
        .map_err(|_| repository_error())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse(&metadata) {
        return Err(unsafe_archive());
    }
    tokio::fs::remove_dir_all(target)
        .await
        .map_err(|_| repository_error())
}

async fn owned_directory_exists(owner_root: &Path, target: &Path) -> McpPlatformResult<bool> {
    prepare_owned_directory(owner_root, target, false)?;
    match tokio::fs::symlink_metadata(target).await {
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && !is_reparse(&metadata) =>
        {
            Ok(true)
        }
        Ok(_) => Err(unsafe_archive()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(repository_error()),
    }
}

async fn atomic_replace(source: &Path, destination: &Path) -> McpPlatformResult<()> {
    let source = source.to_path_buf();
    let destination = destination.to_path_buf();
    tokio::task::spawn_blocking(move || atomic_replace_blocking(&source, &destination))
        .await
        .map_err(|_| repository_error())?
}

#[cfg(not(windows))]
fn atomic_replace_blocking(source: &Path, destination: &Path) -> McpPlatformResult<()> {
    std::fs::rename(source, destination).map_err(|_| repository_error())?;
    if let Some(parent) = destination.parent() {
        std::fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|_| repository_error())?;
    }
    Ok(())
}

#[cfg(windows)]
fn atomic_replace_blocking(source: &Path, destination: &Path) -> McpPlatformResult<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(repository_error())
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ArchiveInstaller {
    pub format: ArchiveFormat,
    pub strip_components: u8,
    pub limits: ArchiveLimits,
}

#[async_trait]
impl Installer for ArchiveInstaller {
    async fn materialize(
        &self,
        artifact: &FetchedArtifact,
        staging_root: &Path,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        let artifact_path = artifact.path.clone();
        let staging_root = staging_root.to_path_buf();
        let format = self.format;
        let strip_components = self.strip_components;
        let limits = self.limits;
        let cancellation = cancellation.clone();
        tokio::task::spawn_blocking(move || {
            extract_archive(
                &artifact_path,
                &staging_root,
                format,
                strip_components,
                limits,
                &cancellation,
            )
        })
        .await
        .map_err(|_| repository_error())??;
        Ok(())
    }
}

fn extract_archive(
    artifact: &Path,
    root: &Path,
    format: ArchiveFormat,
    strip: u8,
    limits: ArchiveLimits,
    cancellation: &CancellationToken,
) -> McpPlatformResult<()> {
    if root.exists() {
        return Err(unsafe_archive());
    }
    std::fs::create_dir_all(root).map_err(|_| repository_error())?;
    let result = match format {
        ArchiveFormat::Zip => extract_zip(artifact, root, strip, limits, cancellation),
        ArchiveFormat::TarGz => extract_tar(
            || {
                std::fs::File::open(artifact)
                    .map(|file| {
                        Box::new(flate2::read::GzDecoder::new(file)) as Box<dyn std::io::Read>
                    })
                    .map_err(|_| repository_error())
            },
            root,
            strip,
            limits,
            std::fs::metadata(artifact)
                .map_err(|_| repository_error())?
                .len(),
            cancellation,
        ),
        ArchiveFormat::TarXz => extract_tar(
            || {
                std::fs::File::open(artifact)
                    .map(|file| Box::new(xz2::read::XzDecoder::new(file)) as Box<dyn std::io::Read>)
                    .map_err(|_| repository_error())
            },
            root,
            strip,
            limits,
            std::fs::metadata(artifact)
                .map_err(|_| repository_error())?
                .len(),
            cancellation,
        ),
    };
    if result.is_err() {
        let _ = std::fs::remove_dir_all(root);
    }
    result
}

fn extract_zip(
    artifact: &Path,
    root: &Path,
    strip: u8,
    limits: ArchiveLimits,
    cancellation: &CancellationToken,
) -> McpPlatformResult<()> {
    let file = std::fs::File::open(artifact).map_err(|_| repository_error())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| unsafe_archive())?;
    if archive.len() > limits.maximum_files {
        return Err(unsafe_archive());
    }
    let mut metadata = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|_| unsafe_archive())?;
        let kind = if entry.is_symlink() {
            ArchiveEntryKind::Symlink
        } else if entry.is_dir() {
            ArchiveEntryKind::Directory
        } else if zip_regular(&entry) {
            ArchiveEntryKind::File
        } else {
            ArchiveEntryKind::Special
        };
        metadata.push(ArchiveEntryMetadata {
            path: stripped_path(entry.name(), strip)?,
            kind,
            compressed_size: entry.compressed_size(),
            expanded_size: entry.size(),
        });
    }
    validate_archive_entries(&metadata, limits)?;
    for (index, item) in metadata.iter().enumerate() {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        let relative = validate_archive_path(&item.path)?;
        let destination = root.join(&relative);
        if !destination.starts_with(root) {
            return Err(unsafe_archive());
        }
        if item.kind == ArchiveEntryKind::Directory {
            std::fs::create_dir_all(&destination).map_err(|_| repository_error())?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|_| repository_error())?;
        }
        let mut source = archive.by_index(index).map_err(|_| unsafe_archive())?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|_| unsafe_archive())?;
        copy_bounded(&mut source, &mut output, item.expanded_size, cancellation)?;
    }
    Ok(())
}

fn zip_regular<R: std::io::Read>(entry: &zip::read::ZipFile<'_, R>) -> bool {
    entry.unix_mode().is_none_or(|mode| {
        let kind = mode & 0o170000;
        kind == 0 || kind == 0o100000
    })
}

fn extract_tar(
    open: impl Fn() -> McpPlatformResult<Box<dyn std::io::Read>>,
    root: &Path,
    strip: u8,
    limits: ArchiveLimits,
    compressed_artifact_size: u64,
    cancellation: &CancellationToken,
) -> McpPlatformResult<()> {
    let mut archive = tar::Archive::new(open()?);
    let mut metadata = Vec::new();
    let mut paths = HashSet::new();
    let mut total = 0_u64;
    for entry in archive.entries().map_err(|_| unsafe_archive())? {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        if metadata.len() >= limits.maximum_files {
            return Err(unsafe_archive());
        }
        let entry = entry.map_err(|_| unsafe_archive())?;
        let kind = if entry.header().entry_type().is_file() {
            ArchiveEntryKind::File
        } else if entry.header().entry_type().is_dir() {
            ArchiveEntryKind::Directory
        } else if entry.header().entry_type().is_symlink() {
            ArchiveEntryKind::Symlink
        } else if entry.header().entry_type().is_hard_link() {
            ArchiveEntryKind::Hardlink
        } else {
            ArchiveEntryKind::Special
        };
        if !matches!(kind, ArchiveEntryKind::File | ArchiveEntryKind::Directory) {
            return Err(unsafe_archive());
        }
        let path = entry.path().map_err(|_| unsafe_archive())?;
        let path = path.to_str().ok_or_else(unsafe_archive)?;
        let path = stripped_path(path, strip)?;
        let folded = path.replace('\\', "/").trim_end_matches('/').to_lowercase();
        if !paths.insert(folded) || entry.size() > limits.maximum_single_file_bytes {
            return Err(unsafe_archive());
        }
        total = total.checked_add(entry.size()).ok_or_else(unsafe_archive)?;
        let maximum_expanded = compressed_artifact_size
            .checked_mul(limits.maximum_compression_ratio)
            .ok_or_else(unsafe_archive)?;
        if total > limits.maximum_total_bytes
            || compressed_artifact_size == 0
            || total > maximum_expanded
        {
            return Err(unsafe_archive());
        }
        metadata.push(ArchiveEntryMetadata {
            path,
            kind,
            compressed_size: entry.size().max(1),
            expanded_size: entry.size(),
        });
    }
    drop(archive);
    let mut archive = tar::Archive::new(open()?);
    for (entry, item) in archive
        .entries()
        .map_err(|_| unsafe_archive())?
        .zip(metadata.iter())
    {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        let mut entry = entry.map_err(|_| unsafe_archive())?;
        let relative = validate_archive_path(&item.path)?;
        let destination = root.join(relative);
        if !destination.starts_with(root) {
            return Err(unsafe_archive());
        }
        if item.kind == ArchiveEntryKind::Directory {
            std::fs::create_dir_all(&destination).map_err(|_| repository_error())?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|_| repository_error())?;
        }
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|_| unsafe_archive())?;
        copy_bounded(&mut entry, &mut output, item.expanded_size, cancellation)?;
    }
    Ok(())
}

fn stripped_path(value: &str, strip: u8) -> McpPlatformResult<String> {
    validate_archive_path(value)?;
    let normalized = value.replace('\\', "/");
    let normalized = normalized.strip_suffix('/').unwrap_or(&normalized);
    let components = normalized.split('/').collect::<Vec<_>>();
    let stripped = components
        .get(usize::from(strip)..)
        .ok_or_else(unsafe_archive)?;
    if stripped.is_empty() {
        return Err(unsafe_archive());
    }
    let result = stripped.join("/");
    validate_archive_path(&result)?;
    Ok(result)
}

fn copy_bounded<R: std::io::Read, W: std::io::Write>(
    source: &mut R,
    destination: &mut W,
    expected: u64,
    cancellation: &CancellationToken,
) -> McpPlatformResult<()> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut written = 0_u64;
    loop {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        let read = source.read(&mut buffer).map_err(|_| unsafe_archive())?;
        if read == 0 {
            break;
        }
        written = written
            .checked_add(read as u64)
            .ok_or_else(unsafe_archive)?;
        if written > expected {
            return Err(unsafe_archive());
        }
        destination
            .write_all(&buffer[..read])
            .map_err(|_| repository_error())?;
    }
    if written != expected {
        return Err(unsafe_archive());
    }
    Ok(())
}

pub struct ProductionArtifactFetcher {
    cache_root: PathBuf,
    digest_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    network: Arc<dyn ArtifactNetworkClient>,
}

impl ProductionArtifactFetcher {
    pub fn new(cache_root: PathBuf) -> McpPlatformResult<Self> {
        Self::new_with_network(cache_root, Arc::new(ProductionArtifactNetworkClient))
    }

    pub fn new_with_network(
        cache_root: PathBuf,
        network: Arc<dyn ArtifactNetworkClient>,
    ) -> McpPlatformResult<Self> {
        Ok(Self {
            cache_root,
            digest_locks: Mutex::new(HashMap::new()),
            network,
        })
    }

    fn lock_for(&self, digest: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.digest_locks
            .lock()
            .expect("artifact digest locks")
            .entry(digest.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProductionArtifactNetworkClient;

#[async_trait]
impl ArtifactNetworkClient for ProductionArtifactNetworkClient {
    async fn open(
        &self,
        effect: &ArtifactFetchEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ArtifactNetworkResponse> {
        let source = Url::parse(&effect.source_url).map_err(|_| unsafe_fetch())?;
        let host = source.host_str().ok_or_else(unsafe_fetch)?;
        let addresses = resolve_public_addresses(
            host,
            source.port_or_known_default().ok_or_else(unsafe_fetch)?,
        )
        .await?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_| repository_error())?;
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(cancelled()),
            response = client.get(source).timeout(std::time::Duration::from_secs(effect.timeout_seconds)).send() => response.map_err(|_| network_error())?,
        };
        let status_code = response.status().as_u16();
        let content_length = response.content_length();
        let stream = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|_| network_error()));
        Ok(ArtifactNetworkResponse::new(
            status_code,
            content_length,
            Box::pin(stream),
        ))
    }
}

#[async_trait]
impl ArtifactFetcher for ProductionArtifactFetcher {
    async fn fetch(
        &self,
        effect: &ArtifactFetchEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<FetchedArtifact> {
        validate_fetch_effect(effect)?;
        let digest_lock = self.lock_for(&effect.expected_sha256);
        let _guard = tokio::select! {
            _ = cancellation.cancelled() => return Err(cancelled()),
            guard = digest_lock.lock() => guard,
        };
        tokio::fs::create_dir_all(&self.cache_root)
            .await
            .map_err(|_| repository_error())?;
        verify_normal_directory(&self.cache_root)?;
        let destination = self.cache_root.join(&effect.expected_sha256);
        if let Ok(metadata) = tokio::fs::symlink_metadata(&destination).await {
            if !metadata.file_type().is_file()
                || metadata.file_type().is_symlink()
                || is_reparse(&metadata)
            {
                return Err(unsafe_fetch());
            }
            let validation: McpPlatformResult<FetchedArtifact> = async {
                enforce_size(effect, metadata.len())?;
                let artifact = hash_file(
                    destination.clone(),
                    metadata.len(),
                    effect.maximum_size_bytes,
                    cancellation,
                )
                .await?;
                Sha256Verifier.verify(effect, &artifact)?;
                Ok(artifact)
            }
            .await;
            match validation {
                Ok(artifact) => return Ok(artifact),
                Err(error) if error.code() == McpPlatformErrorCode::TaskNotCancellable => {
                    return Err(error)
                }
                Err(_) => tokio::fs::remove_file(&destination)
                    .await
                    .map_err(|_| repository_error())?,
            }
        }
        let temporary = self.cache_root.join(format!(
            ".{}-{}.partial",
            effect.expected_sha256, effect.operation_id
        ));
        let partial_created = AtomicBool::new(false);
        let result = async {
            match tokio::fs::symlink_metadata(&temporary).await {
                Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() && !is_reparse(&metadata) => tokio::fs::remove_file(&temporary).await.map_err(|_| repository_error())?,
                Ok(_) => return Err(unsafe_fetch()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(repository_error()),
            }
            let mut response = self.network.open(effect, cancellation).await?;
            if !(200..300).contains(&response.status_code) { return Err(network_error()); }
            if let Some(length) = response.content_length { enforce_size(effect, length)?; }
            let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).await.map_err(|_| repository_error())?;
            partial_created.store(true, Ordering::SeqCst);
            let mut hasher = Sha256::new();
            let mut size = 0_u64;
            while let Some(chunk) = tokio::select! { _ = cancellation.cancelled() => return Err(cancelled()), chunk = response.stream.next() => chunk } {
                let chunk = chunk?;
                size = size.checked_add(chunk.len() as u64).ok_or_else(unsafe_fetch)?;
                if size > effect.maximum_size_bytes || effect.expected_size_bytes.is_some_and(|expected| size > expected) { return Err(unsafe_fetch()); }
                hasher.update(&chunk);
                file.write_all(&chunk).await.map_err(|_| repository_error())?;
            }
            file.sync_all().await.map_err(|_| repository_error())?;
            drop(file);
            if effect.expected_size_bytes.is_some_and(|expected| expected != size) || hex_digest(hasher.finalize().as_slice()) != effect.expected_sha256 { return Err(verification_error()); }
            tokio::fs::rename(&temporary, &destination).await.map_err(|_| repository_error())?;
            Ok(FetchedArtifact { path: destination, digest: effect.expected_sha256.clone(), size_bytes: size })
        }.await;
        if result.is_err()
            && partial_created.load(Ordering::SeqCst)
            && verify_normal_file_or_absent(&temporary).is_ok()
        {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        result
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Sha256Verifier;

impl Verifier for Sha256Verifier {
    fn verify(
        &self,
        effect: &ArtifactFetchEffect,
        artifact: &FetchedArtifact,
    ) -> McpPlatformResult<()> {
        if artifact.digest != effect.expected_sha256
            || effect
                .expected_size_bytes
                .is_some_and(|size| size != artifact.size_bytes)
        {
            return Err(verification_error());
        }
        Ok(())
    }
}

async fn hash_file(
    path: PathBuf,
    size: u64,
    maximum_size: u64,
    cancellation: &CancellationToken,
) -> McpPlatformResult<FetchedArtifact> {
    if cancellation.is_cancelled() {
        return Err(cancelled());
    }
    if size > maximum_size {
        return Err(unsafe_fetch());
    }
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| repository_error())?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut hasher = Sha256::new();
    let mut read_total = 0_u64;
    loop {
        let read = tokio::select! { _ = cancellation.cancelled() => return Err(cancelled()), read = file.read(&mut buffer) => read.map_err(|_| repository_error())? };
        if read == 0 {
            break;
        }
        read_total = read_total
            .checked_add(read as u64)
            .ok_or_else(unsafe_fetch)?;
        if read_total > maximum_size {
            return Err(unsafe_fetch());
        }
        hasher.update(&buffer[..read]);
    }
    if read_total != size {
        return Err(verification_error());
    }
    Ok(FetchedArtifact {
        path,
        digest: hex_digest(hasher.finalize().as_slice()),
        size_bytes: size,
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_fetch_effect(effect: &ArtifactFetchEffect) -> McpPlatformResult<()> {
    let url = Url::parse(&effect.source_url).map_err(|_| unsafe_fetch())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || effect.redirect_policy != RedirectPolicy::DenyAll
        || effect.expected_sha256.len() != 64
        || !effect
            .expected_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || effect.maximum_size_bytes == 0
        || effect.timeout_seconds == 0
        || validate_storage_segment(&effect.operation_id).is_err()
    {
        return Err(unsafe_fetch());
    }
    Ok(())
}

async fn resolve_public_addresses(
    host: &str,
    port: u16,
) -> McpPlatformResult<Vec<std::net::SocketAddr>> {
    if matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "metadata.google.internal"
    ) {
        return Err(unsafe_fetch());
    }
    let addresses = if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        vec![std::net::SocketAddr::new(ip, port)]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| network_error())?
            .collect::<Vec<_>>()
    };
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
        return Err(unsafe_fetch());
    }
    Ok(addresses)
}

fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let octets = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_broadcast()
                || octets[0] == 0
                || octets[0] >= 224
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 198 && matches!(octets[1], 18 | 19 | 51))
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113))
        }
        std::net::IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_ip(std::net::IpAddr::V4(mapped));
            }
            let octets = ip.octets();
            let globally_routable_prefix = octets[0] & 0xe0 == 0x20;
            let documentation =
                (octets[0] == 0x20 && octets[1] == 0x01 && octets[2] == 0x0d && octets[3] == 0xb8)
                    || (octets[0] == 0x3f && octets[1] == 0xff && octets[2] < 0x10);
            let transition_or_special = (octets[0] == 0x20
                && octets[1] == 0x01
                && matches!(
                    octets[2],
                    0x00 | 0x01 | 0x02 | 0x03 | 0x04 | 0x05 | 0x10 | 0x20
                ))
                || (octets[0] == 0x20 && octets[1] == 0x02);
            globally_routable_prefix
                && !documentation
                && !transition_or_special
                && !(ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local()
                    || ip.is_multicast())
        }
    }
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn enforce_size(effect: &ArtifactFetchEffect, size: u64) -> McpPlatformResult<()> {
    if size > effect.maximum_size_bytes
        || effect
            .expected_size_bytes
            .is_some_and(|expected| expected != size)
    {
        return Err(unsafe_fetch());
    }
    Ok(())
}

const fn unsafe_archive() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PathTraversal,
        "archive violates managed extraction policy",
    )
}
const fn unsafe_fetch() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PolicyDenied,
        "artifact fetch violates managed network policy",
    )
}
const fn verification_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "artifact verification failed",
    )
}
const fn network_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RepositoryUnavailable,
        "artifact source is temporarily unavailable",
    )
}
const fn repository_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RepositoryUnavailable,
        "managed artifact storage is temporarily unavailable",
    )
}
const fn cancelled() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::TaskNotCancellable,
        "managed artifact operation was cancelled at a safe boundary",
    )
}
const fn runtime_incompatible() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::AdapterIncompatible,
        "required server-owned runtime capability is unavailable",
    )
}
const fn dependency_denied() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PolicyDenied,
        "managed package contains dependencies without an exact hashed lock",
    )
}
const fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "managed distribution state failed integrity validation",
    )
}
