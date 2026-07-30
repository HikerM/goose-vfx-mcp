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
use super::external_distribution::{
    DirectProcessOutput, DirectProcessRequest, ExternalDistributionPorts, SupplyChainEvidence,
    EXTERNAL_DISTRIBUTION_ADAPTER_VERSION,
};
use super::manifest::ArchiveFormat;
use super::manifest::{
    digest_serializable, normalize_container_path, Distribution, Entrypoint, GitDevAdapter,
    Manifest, PermissionKind,
};
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
    pub external_acquisition: Option<ExternalManagedAcquisition>,
}

#[derive(Debug, Clone)]
pub enum ExternalManagedAcquisition {
    Docker {
        image: String,
        digest: String,
        mount_plan_digest: String,
    },
    GitDev {
        repository_origin: String,
        repository: String,
        commit: String,
        subdirectory: Option<String>,
        underlying_adapter: GitDevAdapter,
        acquisition_digest: String,
    },
}

#[derive(Debug, Clone)]
pub struct ManagedInstallOutcome {
    pub installation_root: PathBuf,
    pub projection: ConnectionProjection,
    pub evidence: ArtifactVerificationEvidence,
    pub materialized_tree_digest: String,
    pub owned_relative_paths: Vec<String>,
    pub replaced_quarantine_token: Option<String>,
    pub supply_chain_evidence: Option<SupplyChainEvidence>,
}

/// Configuration from one root-bound verification of the active managed
/// runtime. It contains no storage path or handle and must be reacquired for
/// every operation that can start or contact the managed runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedManagedRuntimeActivation {
    projection: ConnectionProjection,
}

impl VerifiedManagedRuntimeActivation {
    fn new(projection: ConnectionProjection) -> Self {
        Self { projection }
    }

    pub(crate) fn projection(&self) -> &ConnectionProjection {
        &self.projection
    }
}

/// A path-free capacity observation for the production managed root.
///
/// The required value is produced by the trusted plan-capacity contract. The
/// adapter does not accept a caller-selected path, volume, or reserve amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedStorageCapacity {
    pub required_peak_bytes: u64,
    pub available_bytes: u64,
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

/// Written into the sealed version tree before promotion.  This is not active
/// state: it binds the committed descriptor to the exact regular-file tree
/// that DirectoryWriter sealed and promoted.
#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsCommittedVersionEvidence {
    descriptor_sha256: String,
    tree_sha256: String,
}

#[async_trait]
pub trait DistributionEffectAdapter: Send + Sync {
    fn adapter_version(&self) -> &'static str;
    async fn check_managed_storage_capacity(
        &self,
        _required_peak_bytes: u64,
    ) -> McpPlatformResult<ManagedStorageCapacity> {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::IntegrityUnavailable,
            "managed storage capacity preflight is unavailable",
        ))
    }
    async fn install(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome>;
    async fn inspect_installed(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
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
    async fn acquire_verified_runtime_activation(
        &self,
        _managed_mcp_id: &str,
    ) -> McpPlatformResult<VerifiedManagedRuntimeActivation> {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::RuntimeControlUnavailable,
            "managed runtime activation verification is unavailable",
        ))
    }
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
    external: ExternalDistributionPorts,
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
        Self::new_with_runtime_and_external(root, runtime, ExternalDistributionPorts::production())
    }

    pub(crate) fn new_with_runtime_and_external(
        root: PathBuf,
        runtime: RuntimeCapabilities,
        external: ExternalDistributionPorts,
    ) -> McpPlatformResult<Self> {
        let (root, cache_root) = prepare_distribution_root(root)?;
        Ok(Self {
            fetcher: Arc::new(ProductionArtifactFetcher::new(cache_root)?),
            verifier: Arc::new(Sha256Verifier),
            root,
            runtime,
            external,
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
            external: ExternalDistributionPorts::production(),
        })
    }

    pub fn with_runtime_capabilities(mut self, runtime: RuntimeCapabilities) -> Self {
        self.runtime = runtime;
        self
    }

    fn version_root(&self, managed_mcp_id: &str, version: &str) -> McpPlatformResult<PathBuf> {
        validate_storage_segment(managed_mcp_id)?;
        validate_storage_segment(version)?;
        #[cfg(windows)]
        let installations = "platform.installations";
        #[cfg(not(windows))]
        let installations = "installations";
        Ok(self
            .root
            .join(installations)
            .join(managed_mcp_id)
            .join("versions")
            .join(version))
    }

    fn active_descriptor_path(&self, managed_mcp_id: &str) -> McpPlatformResult<PathBuf> {
        validate_storage_segment(managed_mcp_id)?;
        #[cfg(windows)]
        let installations = "platform.installations";
        #[cfg(not(windows))]
        let installations = "installations";
        Ok(self
            .root
            .join(installations)
            .join(managed_mcp_id)
            .join("active.json"))
    }

    #[cfg(windows)]
    fn read_version_metadata(
        &self,
        managed_mcp_id: &str,
        version: &str,
        leaf: &str,
    ) -> McpPlatformResult<Vec<u8>> {
        validate_storage_segment(managed_mcp_id)?;
        validate_storage_segment(version)?;
        crate::mcp_platform::storage_domain::read_anchored_managed_installation_metadata(
            &self.root,
            managed_mcp_id,
            version,
            leaf,
            MAX_METADATA_BYTES,
        )?
        .ok_or_else(repository_error)
    }

    async fn verify_materialized_outcome(
        &self,
        effect: &ManagedInstallEffect,
        expected_tree_digest: &str,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        let (adapter_id, _, _, entrypoint, timeout) =
            distribution_materialization(&effect.manifest)?;
        let root = self.version_root(&effect.managed_mcp_id, effect.manifest.version.as_str())?;
        #[cfg(windows)]
        let bytes = self
            .read_version_metadata(
                &effect.managed_mcp_id,
                effect.manifest.version.as_str(),
                ".goose-artifact-evidence.json",
            )
            .map_err(|_| verification_error())?;
        #[cfg(not(windows))]
        let bytes = {
            verify_owned_read_path(&self.root, &root)?;
            read_limited(
                &root.join(".goose-artifact-evidence.json"),
                MAX_METADATA_BYTES,
            )
            .await
            .map_err(|_| verification_error())?
        };
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
        #[cfg(windows)]
        let descriptor = serde_json::from_slice::<ActiveRuntimeDescriptor>(
            &self
                .read_version_metadata(
                    &effect.managed_mcp_id,
                    effect.manifest.version.as_str(),
                    ".goose-runtime-descriptor.json",
                )
                .map_err(|_| verification_error())?,
        )
        .map_err(|_| verification_error())?;
        #[cfg(not(windows))]
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
            supply_chain_evidence: None,
        })
    }

    #[cfg(windows)]
    async fn install_windows_via_directory_writer(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        revalidate_windows_managed_distribution_root(&self.root)?;
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        if effect.external_acquisition.is_some() {
            return Err(windows_managed_distribution_mutation_unavailable());
        }

        let (adapter_id, format, strip, entrypoint, timeout) =
            distribution_materialization(&effect.manifest)?;
        let version = effect.manifest.version.as_str();
        validate_storage_segment(&effect.managed_mcp_id)?;
        validate_storage_segment(version)?;
        validate_storage_segment(&effect.task_id)?;
        if self.version_exists(&effect.managed_mcp_id, version).await? {
            return Err(verification_error());
        }

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
        if artifact.digest != effect.expected_sha256
            || effect
                .expected_size_bytes
                .is_some_and(|size| size != artifact.size_bytes)
        {
            return Err(verification_error());
        }

        let scratch = tempfile::Builder::new()
            .prefix("goose-verified-materialization-")
            .tempdir()
            .map_err(|_| repository_error())?;
        let staging_root = scratch.path().join("payload");
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

        let version_root = self.version_root(&effect.managed_mcp_id, version)?;
        let installed_entrypoint = version_root.join(
            staged_entrypoint
                .strip_prefix(&staging_root)
                .map_err(|_| verification_error())?,
        );
        let projection = runtime_projection(
            &self.runtime,
            adapter_id,
            &effect.manifest,
            &version_root,
            &installed_entrypoint,
            entrypoint,
            timeout,
        )?;
        let evidence = ArtifactVerificationEvidence {
            source_origin: source_origin(&effect.source_url)?,
            artifact_digest: artifact.digest,
            size_bytes: artifact.size_bytes,
            adapter_id: adapter_id.to_string(),
            adapter_version: MANAGED_DISTRIBUTION_ADAPTER_VERSION.to_string(),
            platform_selector: effect.platform_selector.clone(),
            verification_result: VerificationResult::Verified,
            installed_at_ms: effect.now_ms,
            artifact_signature: ArtifactSignatureStatus::NotDeclaredByManifestV1,
        };
        let descriptor = ActiveRuntimeDescriptor {
            version: version.to_string(),
            projection: projection.clone(),
        };
        write_windows_materialization_metadata(
            &staging_root,
            &descriptor,
            &evidence,
            &InstallationMarker {
                task_id: effect.task_id.clone(),
                artifact_digest: effect.expected_sha256.clone(),
            },
        )?;

        let materialized_tree_digest = compute_materialized_tree_digest(&staging_root).await?;
        if effect
            .expected_tree_digest
            .as_ref()
            .is_some_and(|expected| expected != &materialized_tree_digest)
        {
            return Err(verification_error());
        }
        let (manifest, payloads) =
            windows_directory_writer_payloads(&staging_root, &effect.managed_mcp_id, version)?;
        let payload_refs = payloads
            .iter()
            .map(
                |payload| crate::mcp_platform::storage_domain::DirectoryPayload {
                    relative_segments: payload.relative_segments.clone(),
                    bytes: &payload.bytes,
                },
            )
            .collect::<Vec<_>>();
        crate::mcp_platform::storage_domain::promote_verified_managed_installations(
            &self.root,
            &manifest,
            &payload_refs,
        )?;

        Ok(ManagedInstallOutcome {
            installation_root: version_root,
            projection,
            evidence,
            materialized_tree_digest,
            owned_relative_paths: vec![".".to_string()],
            replaced_quarantine_token: None,
            supply_chain_evidence: None,
        })
    }

    async fn install_external(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        let version = effect.manifest.version.as_str();
        let version_root = self.version_root(&effect.managed_mcp_id, version)?;
        prepare_owned_directory(&self.root, &version_root, false)?;
        if effect.expected_tree_digest.is_some()
            && effect.operation != TaskOperation::Repair
            && owned_directory_exists(&self.root, &version_root).await?
        {
            return self.inspect_external(effect, cancellation).await;
        }
        let repair_token = repair_token(effect);
        if effect.operation == TaskOperation::Repair
            && owned_directory_exists(&self.root, &version_root).await?
        {
            self.quarantine_version(
                &effect.managed_mcp_id,
                version,
                &format!("{}-repair", effect.task_id),
                cancellation,
            )
            .await?;
        } else if owned_directory_exists(&self.root, &version_root).await? {
            if !effect.rebuild_uncommitted_version {
                return Err(verification_error());
            }
            remove_owned_tree(&self.root, &version_root).await?;
        }
        let staging_root = self
            .root
            .join("staging")
            .join(format!("{}-{}", effect.managed_mcp_id, effect.task_id));
        prepare_owned_directory(&self.root, &staging_root, false)?;
        if owned_directory_exists(&self.root, &staging_root).await? {
            remove_owned_tree(&self.root, &staging_root).await?;
        }
        tokio::fs::create_dir_all(&staging_root)
            .await
            .map_err(|_| repository_error())?;
        let (projection, evidence, external_evidence) = match effect
            .external_acquisition
            .as_ref()
            .ok_or_else(integrity_error)?
        {
            ExternalManagedAcquisition::Docker {
                image,
                digest,
                mount_plan_digest,
            } => {
                self.acquire_docker(effect, image, digest, mount_plan_digest, cancellation)
                    .await?
            }
            ExternalManagedAcquisition::GitDev {
                repository_origin,
                repository,
                commit,
                subdirectory,
                underlying_adapter,
                acquisition_digest,
            } => {
                self.acquire_git(
                    effect,
                    &staging_root,
                    &version_root,
                    repository_origin,
                    repository,
                    commit,
                    subdirectory.as_deref(),
                    *underlying_adapter,
                    acquisition_digest,
                    cancellation,
                )
                .await?
            }
        };
        let descriptor = ActiveRuntimeDescriptor {
            version: version.to_string(),
            projection: projection.clone(),
        };
        tokio::fs::write(
            staging_root.join(".goose-runtime-descriptor.json"),
            serde_json::to_vec(&descriptor).map_err(|_| repository_error())?,
        )
        .await
        .map_err(|_| repository_error())?;
        tokio::fs::write(
            staging_root.join(".goose-artifact-evidence.json"),
            serde_json::to_vec(&evidence).map_err(|_| repository_error())?,
        )
        .await
        .map_err(|_| repository_error())?;
        tokio::fs::write(
            staging_root.join(".goose-external-evidence.json"),
            serde_json::to_vec(&external_evidence).map_err(|_| repository_error())?,
        )
        .await
        .map_err(|_| repository_error())?;
        tokio::fs::write(
            staging_root.join(".goose-installation-owner.json"),
            serde_json::to_vec(&InstallationMarker {
                task_id: effect.task_id.clone(),
                artifact_digest: effect.expected_sha256.clone(),
            })
            .map_err(|_| repository_error())?,
        )
        .await
        .map_err(|_| repository_error())?;
        let materialized_tree_digest = compute_materialized_tree_digest(&staging_root).await?;
        if let Some(parent) = version_root.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|_| repository_error())?;
        }
        tokio::fs::rename(&staging_root, &version_root)
            .await
            .map_err(|_| repository_error())?;
        let supply_chain_evidence = Some(
            external_evidence.into_supply_chain(materialized_tree_digest.clone(), effect.now_ms),
        );
        Ok(ManagedInstallOutcome {
            installation_root: version_root,
            projection,
            evidence,
            materialized_tree_digest,
            owned_relative_paths: vec![".".to_string()],
            replaced_quarantine_token: repair_token,
            supply_chain_evidence,
        })
    }

    async fn inspect_external(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        let root = self.version_root(&effect.managed_mcp_id, effect.manifest.version.as_str())?;
        #[cfg(not(windows))]
        verify_owned_read_path(&self.root, &root)?;
        #[cfg(windows)]
        let evidence_bytes = self
            .read_version_metadata(
                &effect.managed_mcp_id,
                effect.manifest.version.as_str(),
                ".goose-artifact-evidence.json",
            )
            .map_err(|_| verification_error())?;
        #[cfg(not(windows))]
        let evidence_bytes = read_limited(
            &root.join(".goose-artifact-evidence.json"),
            MAX_METADATA_BYTES,
        )
        .await?;
        let evidence: ArtifactVerificationEvidence =
            serde_json::from_slice(&evidence_bytes).map_err(|_| verification_error())?;
        #[cfg(windows)]
        let external_evidence_bytes = self
            .read_version_metadata(
                &effect.managed_mcp_id,
                effect.manifest.version.as_str(),
                ".goose-external-evidence.json",
            )
            .map_err(|_| verification_error())?;
        #[cfg(not(windows))]
        let external_evidence_bytes = read_limited(
            &root.join(".goose-external-evidence.json"),
            MAX_METADATA_BYTES,
        )
        .await?;
        let external_evidence: ExternalEvidenceFile =
            serde_json::from_slice(&external_evidence_bytes).map_err(|_| verification_error())?;
        external_evidence.verify_authority(effect)?;
        #[cfg(windows)]
        {
            let marker: InstallationMarker = serde_json::from_slice(
                &self
                    .read_version_metadata(
                        &effect.managed_mcp_id,
                        effect.manifest.version.as_str(),
                        ".goose-installation-owner.json",
                    )
                    .map_err(|_| verification_error())?,
            )
            .map_err(|_| verification_error())?;
            if marker.task_id != effect.task_id || marker.artifact_digest != effect.expected_sha256
            {
                return Err(verification_error());
            }
        }
        #[cfg(not(windows))]
        verify_installation_marker(&root, &effect.task_id, &effect.expected_sha256).await?;
        let materialized_tree_digest = compute_materialized_tree_digest(&root).await?;
        if effect
            .expected_tree_digest
            .as_ref()
            .is_some_and(|expected| expected != &materialized_tree_digest)
        {
            return Err(verification_error());
        }
        let expected_projection = match (&external_evidence, &effect.manifest.distribution) {
            (
                ExternalEvidenceFile::Docker {
                    daemon_version,
                    rootless,
                    ..
                },
                Distribution::Docker { .. },
            ) => {
                if evidence.source_origin != source_origin(&effect.source_url)?
                    || evidence.artifact_digest != effect.expected_sha256
                    || evidence.size_bytes != 0
                    || evidence.adapter_id != "docker"
                    || evidence.adapter_version != EXTERNAL_DISTRIBUTION_ADAPTER_VERSION
                    || evidence.platform_selector != effect.platform_selector
                    || evidence.verification_result != VerificationResult::Verified
                    || evidence.installed_at_ms != effect.now_ms
                    || evidence.artifact_signature
                        != ArtifactSignatureStatus::NotDeclaredByManifestV1
                {
                    return Err(verification_error());
                }
                let observed_daemon = self.verify_docker_daemon(cancellation).await?;
                if observed_daemon.0 != *daemon_version || observed_daemon.1 != *rootless {
                    return Err(verification_error());
                }
                self.verify_docker_image(effect, cancellation).await?;
                self.docker_runtime_projection(effect)?
            }
            (
                ExternalEvidenceFile::GitDev {
                    materialized_tree_digest: expected_payload,
                    ..
                },
                Distribution::GitDev {
                    repository,
                    adapter,
                    entrypoint,
                    ..
                },
            ) => {
                if evidence.source_origin != source_origin(repository)?
                    || evidence.artifact_digest != effect.expected_sha256
                    || evidence.size_bytes == 0
                    || evidence.size_bytes > DEFAULT_MAX_DOWNLOAD_BYTES
                    || evidence.adapter_id != "git_dev"
                    || evidence.adapter_version != EXTERNAL_DISTRIBUTION_ADAPTER_VERSION
                    || evidence.platform_selector != effect.platform_selector
                    || evidence.verification_result != VerificationResult::Verified
                    || evidence.installed_at_ms != effect.now_ms
                    || evidence.artifact_signature
                        != ArtifactSignatureStatus::NotDeclaredByManifestV1
                    || expected_payload != &materialized_tree_digest
                {
                    return Err(verification_error());
                }
                let entrypoint_path = match adapter {
                    GitDevAdapter::Npm => resolve_git_npm_entrypoint(&root, entrypoint).await?,
                    GitDevAdapter::BinaryArchive => resolve_entrypoint(&root, entrypoint)?,
                    GitDevAdapter::PythonWheel | GitDevAdapter::Docker => {
                        return Err(verification_error());
                    }
                };
                verify_regular_contained(&root, &entrypoint_path).await?;
                git_runtime_projection(
                    &self.runtime,
                    &effect.manifest,
                    &root,
                    &entrypoint_path,
                    *adapter,
                )?
            }
            _ => return Err(verification_error()),
        };
        #[cfg(windows)]
        let descriptor: ActiveRuntimeDescriptor = serde_json::from_slice(
            &self
                .read_version_metadata(
                    &effect.managed_mcp_id,
                    effect.manifest.version.as_str(),
                    ".goose-runtime-descriptor.json",
                )
                .map_err(|_| verification_error())?,
        )
        .map_err(|_| verification_error())?;
        #[cfg(not(windows))]
        let descriptor = read_runtime_descriptor(&root).await?;
        if descriptor.version != effect.manifest.version.as_str()
            || descriptor.projection != expected_projection
        {
            return Err(verification_error());
        }
        let installed_at_ms = evidence.installed_at_ms;
        Ok(ManagedInstallOutcome {
            installation_root: root,
            projection: expected_projection,
            evidence,
            materialized_tree_digest: materialized_tree_digest.clone(),
            owned_relative_paths: vec![".".to_string()],
            replaced_quarantine_token: repair_token(effect),
            supply_chain_evidence: Some(
                external_evidence.into_supply_chain(materialized_tree_digest, installed_at_ms),
            ),
        })
    }

    async fn acquire_docker(
        &self,
        effect: &ManagedInstallEffect,
        image: &str,
        digest: &str,
        mount_plan_digest: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<(
        ConnectionProjection,
        ArtifactVerificationEvidence,
        ExternalEvidenceFile,
    )> {
        let capability = self.external.capabilities.docker()?;
        if !capability.policy_allowed {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::DaemonPolicyDenied,
                "Docker daemon policy denied the operation",
            ));
        }
        let (daemon_version, rootless) = self.verify_docker_daemon(cancellation).await?;
        let reference = format!("{image}@sha256:{digest}");
        let pull = self
            .run_docker(vec!["pull".to_string(), reference.clone()], cancellation)
            .await?;
        if !pull.success {
            return Err(classify_docker_failure(&pull.stderr));
        }
        self.verify_docker_image(effect, cancellation).await?;
        let Distribution::Docker { mounts, .. } = &effect.manifest.distribution else {
            return Err(integrity_error());
        };
        if digest_serializable(mounts)? != mount_plan_digest {
            return Err(integrity_error());
        }
        let projection = self.docker_runtime_projection(effect)?;
        let origin = source_origin(&effect.source_url)?;
        Ok((
            projection,
            ArtifactVerificationEvidence {
                source_origin: origin,
                artifact_digest: digest.to_string(),
                size_bytes: 0,
                adapter_id: "docker".to_string(),
                adapter_version: EXTERNAL_DISTRIBUTION_ADAPTER_VERSION.to_string(),
                platform_selector: effect.platform_selector.clone(),
                verification_result: VerificationResult::Verified,
                installed_at_ms: effect.now_ms,
                artifact_signature: ArtifactSignatureStatus::NotDeclaredByManifestV1,
            },
            ExternalEvidenceFile::Docker {
                image: image.to_string(),
                image_digest: digest.to_string(),
                daemon_version,
                rootless,
                mount_plan_digest: mount_plan_digest.to_string(),
            },
        ))
    }

    fn docker_runtime_projection(
        &self,
        effect: &ManagedInstallEffect,
    ) -> McpPlatformResult<ConnectionProjection> {
        let capability = self.external.capabilities.docker()?;
        let Some(ExternalManagedAcquisition::Docker { image, digest, .. }) =
            effect.external_acquisition.as_ref()
        else {
            return Err(integrity_error());
        };
        let Distribution::Docker {
            entrypoint, mounts, ..
        } = &effect.manifest.distribution
        else {
            return Err(integrity_error());
        };
        let reference = format!("{image}@sha256:{digest}");
        let mut argv = vec![
            "run".to_string(),
            "--rm".to_string(),
            "--interactive".to_string(),
            "--read-only".to_string(),
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--pids-limit".to_string(),
            "256".to_string(),
            "--label".to_string(),
            format!("dev.block.goose.managed={}", effect.managed_mcp_id),
        ];
        if !effect
            .manifest
            .permissions
            .iter()
            .any(|permission| permission.kind == PermissionKind::Network)
        {
            argv.extend(["--network".to_string(), "none".to_string()]);
        }
        for mount in mounts {
            let grant = self
                .external
                .mount_permissions
                .resolve(&mount.source_permission)?;
            let target = normalize_container_path(&mount.target)?;
            let metadata = std::fs::symlink_metadata(&grant.server_path).map_err(|_| {
                McpPlatformError::new(
                    McpPlatformErrorCode::MountPermissionDenied,
                    "filesystem permission grant target is unavailable",
                )
            })?;
            if metadata.file_type().is_symlink()
                || is_reparse(&metadata)
                || !(metadata.is_file() || metadata.is_dir())
                || (!mount.read_only && grant.kind != PermissionKind::FilesystemWrite)
                || (mount.read_only
                    && !matches!(
                        grant.kind,
                        PermissionKind::FilesystemRead | PermissionKind::FilesystemWrite
                    ))
            {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::MountPermissionDenied,
                    "filesystem permission grant violates Docker mount policy",
                ));
            }
            let source = grant.server_path.to_str().ok_or_else(|| {
                McpPlatformError::new(
                    McpPlatformErrorCode::MountPermissionDenied,
                    "filesystem permission grant path is not valid UTF-8",
                )
            })?;
            if source.contains(',') || source.chars().any(char::is_control) {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::MountPermissionDenied,
                    "filesystem permission grant path cannot be encoded safely",
                ));
            }
            let mut specification = format!("type=bind,src={source},dst={target}");
            if mount.read_only {
                specification.push_str(",readonly");
            }
            argv.extend(["--mount".to_string(), specification]);
        }
        for key in &entrypoint.environment_keys {
            argv.extend(["--env".to_string(), key.clone()]);
        }
        argv.extend([
            "--entrypoint".to_string(),
            entrypoint.executable.clone(),
            reference,
        ]);
        argv.extend(entrypoint.args.clone());
        let timeout_seconds = match effect.manifest.transport {
            super::manifest::Transport::Stdio {
                startup_timeout_seconds,
            } => startup_timeout_seconds,
            _ => return Err(integrity_error()),
        };
        Ok(ConnectionProjection::ManagedDockerStdio {
            name: effect.manifest.name.clone(),
            description: effect.manifest.description.clone(),
            executable: capability.executable().to_string_lossy().into_owned(),
            args: argv,
            cwd: None,
            timeout_seconds,
        })
    }

    async fn verify_docker_daemon(
        &self,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<(String, bool)> {
        let version = self
            .run_docker(
                vec![
                    "version".to_string(),
                    "--format".to_string(),
                    "{{.Server.Version}}".to_string(),
                ],
                cancellation,
            )
            .await?;
        let value = std::str::from_utf8(&version.stdout)
            .unwrap_or_default()
            .trim();
        if !version.success
            || value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::DockerUnavailable,
                "Docker daemon is unavailable or incompatible",
            ));
        }
        let info = self
            .run_docker(
                vec![
                    "info".to_string(),
                    "--format".to_string(),
                    "{{json .SecurityOptions}}".to_string(),
                ],
                cancellation,
            )
            .await?;
        let security = std::str::from_utf8(&info.stdout).unwrap_or_default();
        if !info.success || security.len() > 4096 || !security.trim_start().starts_with('[') {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::DockerUnavailable,
                "Docker daemon security capability is unavailable",
            ));
        }
        Ok((value.to_string(), security.contains("name=rootless")))
    }

    async fn verify_docker_image(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        let Some(ExternalManagedAcquisition::Docker { image, digest, .. }) =
            effect.external_acquisition.as_ref()
        else {
            return Err(integrity_error());
        };
        let reference = format!("{image}@sha256:{digest}");
        let output = self
            .run_docker(
                vec![
                    "image".to_string(),
                    "inspect".to_string(),
                    "--format".to_string(),
                    "{{json .RepoDigests}}\n{{.Id}}".to_string(),
                    reference.clone(),
                ],
                cancellation,
            )
            .await?;
        let mut lines = output.stdout.split(|byte| *byte == b'\n');
        let repo_digests = lines
            .next()
            .and_then(|line| serde_json::from_slice::<Vec<String>>(line).ok())
            .unwrap_or_default();
        let image_id = lines
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .unwrap_or_default();
        let valid_image_id = image_id.strip_prefix("sha256:").is_some_and(|value| {
            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        if !output.success
            || !repo_digests.iter().any(|candidate| candidate == &reference)
            || !valid_image_id
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::ImageDigestMismatch,
                "Docker image digest verification failed",
            ));
        }
        Ok(())
    }

    async fn run_docker(
        &self,
        argv: Vec<String>,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<DirectProcessOutput> {
        let executable = self
            .external
            .capabilities
            .docker()?
            .executable()
            .to_path_buf();
        self.external
            .process
            .run(
                DirectProcessRequest {
                    executable,
                    argv,
                    environment: self.docker_environment()?,
                    cwd: None,
                    stdin: None,
                },
                cancellation,
            )
            .await
    }

    fn docker_environment(&self) -> McpPlatformResult<std::collections::BTreeMap<String, String>> {
        #[cfg(windows)]
        {
            return Err(windows_managed_docker_inspection_unavailable());
        }

        #[cfg(not(windows))]
        {
            let home = self.root.join("docker-runtime").join("home");
            let config = self.root.join("docker-runtime").join("config");
            std::fs::create_dir_all(&home).map_err(|_| repository_error())?;
            std::fs::create_dir_all(&config).map_err(|_| repository_error())?;
            let home = home.to_str().ok_or_else(repository_error)?.to_string();
            let config = config.to_str().ok_or_else(repository_error)?.to_string();
            Ok(std::collections::BTreeMap::from([
                ("HOME".to_string(), home),
                ("DOCKER_CONFIG".to_string(), config),
            ]))
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn acquire_git(
        &self,
        effect: &ManagedInstallEffect,
        staging_root: &Path,
        version_root: &Path,
        repository_origin: &str,
        repository: &str,
        commit: &str,
        subdirectory: Option<&str>,
        underlying_adapter: GitDevAdapter,
        acquisition_digest: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<(
        ConnectionProjection,
        ArtifactVerificationEvidence,
        ExternalEvidenceFile,
    )> {
        if acquisition_digest != effect.expected_sha256
            || commit.len() != 40
            || !commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(integrity_error());
        }
        let parsed = Url::parse(repository).map_err(|_| git_origin_denied())?;
        let host = parsed.host_str().ok_or_else(git_origin_denied)?;
        let port = parsed
            .port_or_known_default()
            .ok_or_else(git_origin_denied)?;
        let addresses = self
            .external
            .network
            .resolve(host, port)
            .await
            .map_err(|_| git_origin_denied())?;
        let network_pin = GitNetworkPin::new(host, port, &addresses)?;
        let git_root = self.root.join("git").join(&effect.task_id);
        prepare_owned_directory(&self.root, &git_root, false)?;
        if owned_directory_exists(&self.root, &git_root).await? {
            remove_owned_tree(&self.root, &git_root).await?;
        }
        tokio::fs::create_dir_all(&git_root)
            .await
            .map_err(|_| repository_error())?;
        let result = self
            .acquire_git_inner(
                effect,
                staging_root,
                version_root,
                &git_root,
                repository,
                commit,
                subdirectory,
                underlying_adapter,
                acquisition_digest,
                &network_pin,
                cancellation,
            )
            .await;
        let cleanup = remove_owned_tree(&self.root, &git_root).await;
        match (result, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
        .map(
            |(projection, evidence, tree_id, materialized_tree_digest)| {
                (
                    projection,
                    evidence,
                    ExternalEvidenceFile::GitDev {
                        repository_origin: repository_origin.to_string(),
                        commit: commit.to_string(),
                        git_tree_id: tree_id,
                        materialized_tree_digest,
                    },
                )
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn acquire_git_inner(
        &self,
        effect: &ManagedInstallEffect,
        staging_root: &Path,
        version_root: &Path,
        git_root: &Path,
        repository: &str,
        commit: &str,
        subdirectory: Option<&str>,
        underlying_adapter: GitDevAdapter,
        acquisition_digest: &str,
        network_pin: &GitNetworkPin,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<(
        ConnectionProjection,
        ArtifactVerificationEvidence,
        String,
        String,
    )> {
        let bare = git_root.join("objects.git");
        let environment = git_environment(git_root)?;
        let hooks = git_root.join("hooks-disabled");
        let mut init_args = git_security_args(&hooks);
        init_args.extend([
            "init".to_string(),
            "--bare".to_string(),
            bare.to_string_lossy().into_owned(),
        ]);
        let init = self
            .run_git(init_args, environment.clone(), cancellation)
            .await?;
        if !init.success {
            return Err(git_unavailable());
        }
        let mut fetch_args = git_global_args(&bare, &hooks);
        for value in network_pin.config_values() {
            fetch_args.extend(["-c".to_string(), format!("http.curloptResolve={value}")]);
        }
        fetch_args.extend([
            "fetch".to_string(),
            "--no-tags".to_string(),
            "--no-recurse-submodules".to_string(),
            "--depth=1".to_string(),
            repository.to_string(),
            commit.to_string(),
        ]);
        let fetched = self
            .run_git(fetch_args, environment.clone(), cancellation)
            .await?;
        if !fetched.success {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::CommitUnavailable,
                "exact Git commit is unavailable",
            ));
        }
        let object_type = self
            .run_git(
                git_query_args(&bare, &hooks, ["cat-file", "-t", commit]),
                environment.clone(),
                cancellation,
            )
            .await?;
        if !object_type.success || object_type.stdout != b"commit\n" {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::CommitUnavailable,
                "Git object is not the exact requested commit",
            ));
        }
        let tree_selector = subdirectory
            .map(|path| format!("{commit}:{path}"))
            .unwrap_or_else(|| format!("{commit}^{{tree}}"));
        let tree = self
            .run_git(
                git_query_args(&bare, &hooks, ["rev-parse", &tree_selector]),
                environment.clone(),
                cancellation,
            )
            .await?;
        let tree_id = std::str::from_utf8(&tree.stdout)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !tree.success
            || tree_id.len() != 40
            || !tree_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(unsafe_repository_tree());
        }
        let listing = self
            .run_git(
                git_query_args(&bare, &hooks, ["ls-tree", "-r", "-l", "-z", &tree_id]),
                environment.clone(),
                cancellation,
            )
            .await?;
        if !listing.success {
            return Err(unsafe_repository_tree());
        }
        validate_git_tree_listing(&listing.stdout, ArchiveLimits::default())?;
        let archive_path = git_root.join("tree.tar.gz");
        let mut archive_args = git_global_args(&bare, &hooks);
        archive_args.extend([
            "archive".to_string(),
            "--format=tar.gz".to_string(),
            format!("--output={}", archive_path.to_string_lossy()),
            "--prefix=payload/".to_string(),
            tree_id.clone(),
        ]);
        let archived = self
            .run_git(archive_args, environment, cancellation)
            .await?;
        if !archived.success {
            return Err(unsafe_repository_tree());
        }
        let metadata = tokio::fs::symlink_metadata(&archive_path)
            .await
            .map_err(|_| unsafe_repository_tree())?;
        let archive = hash_file(
            archive_path,
            metadata.len(),
            DEFAULT_MAX_DOWNLOAD_BYTES,
            cancellation,
        )
        .await?;
        if owned_directory_exists(&self.root, staging_root).await? {
            remove_owned_tree(&self.root, staging_root).await?;
        }
        ArchiveInstaller {
            format: ArchiveFormat::TarGz,
            strip_components: 1,
            limits: ArchiveLimits::default(),
        }
        .materialize(&archive, staging_root, cancellation)
        .await?;
        let Distribution::GitDev { entrypoint, .. } = &effect.manifest.distribution else {
            return Err(integrity_error());
        };
        let entrypoint_path = match underlying_adapter {
            GitDevAdapter::Npm => resolve_git_npm_entrypoint(staging_root, entrypoint).await?,
            GitDevAdapter::BinaryArchive => resolve_entrypoint(staging_root, entrypoint)?,
            GitDevAdapter::PythonWheel => {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::OperationNotSupported,
                    "git development Python wheel lacks a closed wheel identity in manifest v1",
                ));
            }
            GitDevAdapter::Docker => {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::OperationNotSupported,
                    "git development Docker builds are forbidden",
                ));
            }
        };
        verify_regular_contained(staging_root, &entrypoint_path).await?;
        set_minimum_entrypoint_permissions(
            if underlying_adapter == GitDevAdapter::BinaryArchive {
                "binary_archive"
            } else {
                "npm"
            },
            &entrypoint_path,
        )
        .await?;
        let materialized_tree_digest = compute_materialized_tree_digest(staging_root).await?;
        let projection = git_runtime_projection(
            &self.runtime,
            &effect.manifest,
            version_root,
            &version_root.join(
                entrypoint_path
                    .strip_prefix(staging_root)
                    .map_err(|_| unsafe_repository_tree())?,
            ),
            underlying_adapter,
        )?;
        Ok((
            projection,
            ArtifactVerificationEvidence {
                source_origin: source_origin(repository)?,
                artifact_digest: acquisition_digest.to_string(),
                size_bytes: archive.size_bytes,
                adapter_id: "git_dev".to_string(),
                adapter_version: EXTERNAL_DISTRIBUTION_ADAPTER_VERSION.to_string(),
                platform_selector: effect.platform_selector.clone(),
                verification_result: VerificationResult::Verified,
                installed_at_ms: effect.now_ms,
                artifact_signature: ArtifactSignatureStatus::NotDeclaredByManifestV1,
            },
            tree_id,
            materialized_tree_digest,
        ))
    }

    async fn run_git(
        &self,
        argv: Vec<String>,
        environment: std::collections::BTreeMap<String, String>,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<DirectProcessOutput> {
        self.external
            .process
            .run(
                DirectProcessRequest {
                    executable: self.external.capabilities.git()?.executable().to_path_buf(),
                    argv,
                    environment,
                    cwd: None,
                    stdin: None,
                },
                cancellation,
            )
            .await
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ExternalEvidenceFile {
    Docker {
        image: String,
        image_digest: String,
        daemon_version: String,
        rootless: bool,
        mount_plan_digest: String,
    },
    GitDev {
        repository_origin: String,
        commit: String,
        git_tree_id: String,
        materialized_tree_digest: String,
    },
}

#[derive(Debug, Clone)]
struct GitNetworkPin {
    host: String,
    port: u16,
    addresses: Vec<std::net::IpAddr>,
}

impl GitNetworkPin {
    fn new(host: &str, port: u16, addresses: &[std::net::SocketAddr]) -> McpPlatformResult<Self> {
        if host.is_empty()
            || host.parse::<std::net::IpAddr>().is_ok()
            || !host.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
            })
            || addresses.is_empty()
            || addresses
                .iter()
                .any(|address| address.port() != port || !public_ip(address.ip()))
        {
            return Err(git_origin_denied());
        }
        let mut values = addresses
            .iter()
            .map(|address| address.ip())
            .collect::<Vec<_>>();
        values.sort();
        values.dedup();
        Ok(Self {
            host: host.to_string(),
            port,
            addresses: values,
        })
    }

    fn config_values(&self) -> Vec<String> {
        self.addresses
            .iter()
            .map(|address| match address {
                std::net::IpAddr::V4(address) => {
                    format!("{}:{}:{address}", self.host, self.port)
                }
                std::net::IpAddr::V6(address) => {
                    format!("{}:{}:[{address}]", self.host, self.port)
                }
            })
            .collect()
    }
}

impl ExternalEvidenceFile {
    fn verify_authority(&self, effect: &ManagedInstallEffect) -> McpPlatformResult<()> {
        let valid = match (self, effect.external_acquisition.as_ref()) {
            (
                Self::Docker {
                    image,
                    image_digest,
                    mount_plan_digest,
                    daemon_version,
                    ..
                },
                Some(ExternalManagedAcquisition::Docker {
                    image: expected_image,
                    digest,
                    mount_plan_digest: expected_mounts,
                }),
            ) => {
                image == expected_image
                    && image_digest == digest
                    && mount_plan_digest == expected_mounts
                    && !daemon_version.is_empty()
                    && daemon_version.len() <= 64
                    && daemon_version
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
            }
            (
                Self::GitDev {
                    repository_origin,
                    commit,
                    git_tree_id,
                    materialized_tree_digest,
                },
                Some(ExternalManagedAcquisition::GitDev {
                    repository_origin: expected_origin,
                    commit: expected_commit,
                    ..
                }),
            ) => {
                repository_origin == expected_origin
                    && commit == expected_commit
                    && git_tree_id.len() == 40
                    && git_tree_id
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    && materialized_tree_digest.len() == 64
                    && materialized_tree_digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(verification_error())
        }
    }

    fn into_supply_chain(self, _tree_digest: String, created_at_ms: i64) -> SupplyChainEvidence {
        match self {
            Self::Docker {
                image,
                image_digest,
                daemon_version,
                rootless,
                mount_plan_digest,
            } => SupplyChainEvidence::Docker {
                image,
                image_digest,
                adapter_version: EXTERNAL_DISTRIBUTION_ADAPTER_VERSION.to_string(),
                daemon_version,
                rootless,
                mount_plan_digest,
                created_at_ms,
            },
            Self::GitDev {
                repository_origin,
                commit,
                git_tree_id,
                materialized_tree_digest,
            } => SupplyChainEvidence::GitDev {
                repository_origin,
                commit,
                git_tree_id,
                materialized_tree_digest,
                adapter_version: EXTERNAL_DISTRIBUTION_ADAPTER_VERSION.to_string(),
                created_at_ms,
            },
        }
    }
}

fn classify_docker_failure(stderr: &[u8]) -> McpPlatformError {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if message.contains("unauthorized")
        || message.contains("authentication required")
        || message.contains("denied")
    {
        McpPlatformError::new(
            McpPlatformErrorCode::RegistryAuthRequired,
            "Docker registry authentication is required",
        )
    } else {
        McpPlatformError::new(
            McpPlatformErrorCode::RepositoryUnavailable,
            "Docker registry operation failed",
        )
    }
}

fn git_environment(root: &Path) -> McpPlatformResult<std::collections::BTreeMap<String, String>> {
    let home = root.join("home");
    let hooks = root.join("hooks-disabled");
    std::fs::create_dir_all(&home).map_err(|_| repository_error())?;
    std::fs::create_dir_all(&hooks).map_err(|_| repository_error())?;
    let global_config = root.join("global.gitconfig");
    std::fs::write(&global_config, b"").map_err(|_| repository_error())?;
    Ok(std::collections::BTreeMap::from([
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        (
            "GIT_CONFIG_GLOBAL".to_string(),
            global_config.to_string_lossy().into_owned(),
        ),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("GCM_INTERACTIVE".to_string(), "Never".to_string()),
        ("GIT_LFS_SKIP_SMUDGE".to_string(), "1".to_string()),
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
    ]))
}

fn git_global_args(git_dir: &Path, hooks_dir: &Path) -> Vec<String> {
    let mut args = git_security_args(hooks_dir);
    args.extend([
        "--git-dir".to_string(),
        git_dir.to_string_lossy().into_owned(),
    ]);
    args
}

fn git_security_args(hooks_dir: &Path) -> Vec<String> {
    vec![
        "--no-pager".to_string(),
        "--no-optional-locks".to_string(),
        "-c".to_string(),
        "credential.helper=".to_string(),
        "-c".to_string(),
        format!("core.hooksPath={}", hooks_dir.to_string_lossy()),
        "-c".to_string(),
        "protocol.file.allow=never".to_string(),
        "-c".to_string(),
        "http.followRedirects=false".to_string(),
        "-c".to_string(),
        "filter.lfs.required=false".to_string(),
        "-c".to_string(),
        "filter.lfs.smudge=".to_string(),
    ]
}

fn git_query_args<const N: usize>(
    git_dir: &Path,
    hooks_dir: &Path,
    values: [&str; N],
) -> Vec<String> {
    let mut result = git_global_args(git_dir, hooks_dir);
    result.extend(values.into_iter().map(str::to_string));
    result
}

fn validate_git_tree_listing(bytes: &[u8], limits: ArchiveLimits) -> McpPlatformResult<()> {
    let mut count = 0_usize;
    let mut total = 0_u64;
    let mut paths = HashSet::new();
    for raw in bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        count += 1;
        if count > limits.maximum_files {
            return Err(unsafe_repository_tree());
        }
        let text = std::str::from_utf8(raw).map_err(|_| unsafe_repository_tree())?;
        let (metadata, path) = text.split_once('\t').ok_or_else(unsafe_repository_tree)?;
        let fields = metadata.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 || fields[1] != "blob" || !matches!(fields[0], "100644" | "100755") {
            return Err(unsafe_repository_tree());
        }
        let size = fields[3]
            .parse::<u64>()
            .map_err(|_| unsafe_repository_tree())?;
        if size > limits.maximum_single_file_bytes {
            return Err(unsafe_repository_tree());
        }
        total = total.checked_add(size).ok_or_else(unsafe_repository_tree)?;
        if total > limits.maximum_total_bytes {
            return Err(unsafe_repository_tree());
        }
        validate_archive_path(path).map_err(|_| unsafe_repository_tree())?;
        if path
            .split('/')
            .any(|component| component.eq_ignore_ascii_case(".git"))
            || !paths.insert(path.replace('\\', "/").to_ascii_lowercase())
        {
            return Err(unsafe_repository_tree());
        }
    }
    if count == 0 {
        return Err(unsafe_repository_tree());
    }
    Ok(())
}

async fn resolve_git_npm_entrypoint(
    root: &Path,
    entrypoint: &Entrypoint,
) -> McpPlatformResult<PathBuf> {
    let bytes = read_limited(&root.join("package.json"), MAX_METADATA_BYTES)
        .await
        .map_err(|_| unsafe_repository_tree())?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| unsafe_repository_tree())?;
    for key in [
        "scripts",
        "dependencies",
        "optionalDependencies",
        "peerDependencies",
        "bundleDependencies",
        "bundledDependencies",
    ] {
        match value.get(key) {
            None => {}
            Some(serde_json::Value::Object(items)) if items.is_empty() => {}
            Some(serde_json::Value::Array(items)) if items.is_empty() => {}
            Some(_) => {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::PolicyDenied,
                    "git development npm input contains scripts or dependencies",
                ));
            }
        }
    }
    let bin_name = logical_bin_name(&entrypoint.executable)?;
    let bin_path = match value.get("bin") {
        Some(serde_json::Value::String(path)) => path.as_str(),
        Some(serde_json::Value::Object(entries)) => entries
            .get(bin_name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(unsafe_repository_tree)?,
        _ => return Err(unsafe_repository_tree()),
    };
    resolve_managed_path(root, bin_path.trim_start_matches("./"))
}

fn git_runtime_projection(
    runtime: &RuntimeCapabilities,
    manifest: &Manifest,
    root: &Path,
    entrypoint_path: &Path,
    adapter: GitDevAdapter,
) -> McpPlatformResult<ConnectionProjection> {
    let Distribution::GitDev { entrypoint, .. } = &manifest.distribution else {
        return Err(integrity_error());
    };
    let (executable, mut args) = match adapter {
        GitDevAdapter::Npm => (
            runtime
                .node
                .as_ref()
                .ok_or_else(runtime_incompatible)?
                .to_string_lossy()
                .into_owned(),
            vec![entrypoint_path.to_string_lossy().into_owned()],
        ),
        GitDevAdapter::BinaryArchive => {
            (entrypoint_path.to_string_lossy().into_owned(), Vec::new())
        }
        GitDevAdapter::PythonWheel | GitDevAdapter::Docker => {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::OperationNotSupported,
                "git development underlying adapter is not safely expressible",
            ));
        }
    };
    args.extend(entrypoint.args.clone());
    let timeout_seconds = match manifest.transport {
        super::manifest::Transport::Stdio {
            startup_timeout_seconds,
        } => startup_timeout_seconds,
        _ => return Err(integrity_error()),
    };
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

const fn git_origin_denied() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::GitOriginDenied,
        "Git origin violates the HTTPS and public-network policy",
    )
}

const fn git_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::GitUnavailable,
        "Git capability is unavailable",
    )
}

const fn unsafe_repository_tree() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::UnsafeRepositoryTree,
        "Git repository tree violates materialization policy",
    )
}

fn prepare_distribution_root(root: PathBuf) -> McpPlatformResult<(PathBuf, PathBuf)> {
    #[cfg(windows)]
    {
        // The Windows authority owns root creation and every mutation below it.
        // Do not turn a successful read-only preflight into a path-based bootstrap.
        crate::mcp_platform::storage_domain::validate_managed_storage_root(&root)?;
        verify_normal_directory(&root)?;
        let root = std::fs::canonicalize(root).map_err(|_| repository_error())?;
        return Ok((root.clone(), root.join("cache")));
    }

    #[cfg(not(windows))]
    {
        std::fs::create_dir_all(&root).map_err(|_| repository_error())?;
        verify_normal_directory(&root)?;
        let root = std::fs::canonicalize(root).map_err(|_| repository_error())?;
        let cache_root = root.join("cache");
        prepare_owned_directory(&root, &cache_root, true)?;
        Ok((root, cache_root))
    }
}

#[cfg(windows)]
fn revalidate_windows_managed_distribution_root(root: &Path) -> McpPlatformResult<()> {
    crate::mcp_platform::storage_domain::validate_managed_storage_root(root)
}

#[cfg(windows)]
fn windows_managed_distribution_mutation_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RuntimeControlUnavailable,
        "windows managed distribution mutation requires the handle-relative installation protocol",
    )
}

#[cfg(windows)]
fn windows_managed_docker_inspection_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RuntimeControlUnavailable,
        "windows Docker managed distribution inspection requires a read-only daemon capability",
    )
}

#[cfg(windows)]
fn windows_managed_read_inspection_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RuntimeControlUnavailable,
        "windows managed distribution inspection requires a handle-relative directory reader",
    )
}

#[async_trait]
impl DistributionEffectAdapter for ProductionDistributionEffectAdapter {
    fn adapter_version(&self) -> &'static str {
        MANAGED_DISTRIBUTION_ADAPTER_VERSION
    }

    async fn check_managed_storage_capacity(
        &self,
        required_peak_bytes: u64,
    ) -> McpPlatformResult<ManagedStorageCapacity> {
        #[cfg(windows)]
        {
            revalidate_windows_managed_distribution_root(&self.root)?;
            let available_bytes =
                crate::mcp_platform::storage_domain::managed_storage_available_bytes(&self.root)?;
            return checked_managed_storage_capacity(required_peak_bytes, available_bytes);
        }

        #[cfg(not(windows))]
        {
            let _ = required_peak_bytes;
            Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "managed storage capacity preflight is unavailable",
            ))
        }
    }

    async fn install(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        #[cfg(windows)]
        {
            return self
                .install_windows_via_directory_writer(effect, cancellation)
                .await;
        }

        #[cfg(not(windows))]
        {
            if effect.external_acquisition.is_some() {
                return self.install_external(effect, cancellation).await;
            }
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
                let quarantine_exists =
                    owned_directory_exists(&self.root, &quarantine_root).await?;
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
                materialized_tree_digest =
                    Some(compute_materialized_tree_digest(&staging_root).await?);
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
    }

    async fn inspect_installed(
        &self,
        effect: &ManagedInstallEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<ManagedInstallOutcome> {
        #[cfg(windows)]
        {
            revalidate_windows_managed_distribution_root(&self.root)?;
            if matches!(
                effect.external_acquisition.as_ref(),
                Some(ExternalManagedAcquisition::Docker { .. })
            ) {
                return Err(windows_managed_docker_inspection_unavailable());
            }
            return Err(windows_managed_read_inspection_unavailable());
        }

        if effect.external_acquisition.is_some() {
            return self.inspect_external(effect, cancellation).await;
        }
        let expected = effect
            .expected_tree_digest
            .as_deref()
            .ok_or_else(integrity_error)?;
        self.verify_materialized_outcome(effect, expected).await
    }

    async fn version_exists(&self, managed_mcp_id: &str, version: &str) -> McpPlatformResult<bool> {
        #[cfg(windows)]
        {
            validate_storage_segment(managed_mcp_id)?;
            validate_storage_segment(version)?;
            return crate::mcp_platform::storage_domain::managed_storage_verified_version_exists(
                &self.root,
                managed_mcp_id,
                version,
            );
        }

        #[cfg(not(windows))]
        {
            let root = self.version_root(managed_mcp_id, version)?;
            verify_owned_read_path(&self.root, &root)?;
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
    }

    async fn activate_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        task_id: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        #[cfg(windows)]
        {
            revalidate_windows_managed_distribution_root(&self.root)?;
            if cancellation.is_cancelled() {
                return Err(cancelled());
            }
            validate_storage_segment(managed_mcp_id)?;
            validate_storage_segment(version)?;
            validate_storage_segment(task_id)?;
            let descriptor = self.read_version_metadata(
                managed_mcp_id,
                version,
                ".goose-runtime-descriptor.json",
            )?;
            let parsed: ActiveRuntimeDescriptor =
                serde_json::from_slice(&descriptor).map_err(|_| integrity_error())?;
            if parsed.version != version {
                return Err(integrity_error());
            }
            return crate::mcp_platform::storage_domain::activate_verified_managed_installation(
                &self.root,
                managed_mcp_id,
                version,
                &descriptor,
                &hex_digest(&descriptor),
            );
        }

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
        #[cfg(windows)]
        {
            return match previous_version {
                Some(version) => {
                    self.activate_version(
                        managed_mcp_id,
                        version,
                        task_id,
                        &CancellationToken::new(),
                    )
                    .await
                }
                None => {
                    validate_storage_segment(managed_mcp_id)?;
                    validate_storage_segment(task_id)?;
                    crate::mcp_platform::storage_domain::clear_managed_installation_activation(
                        &self.root,
                        managed_mcp_id,
                    )
                }
            };
        }

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

    async fn acquire_verified_runtime_activation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<VerifiedManagedRuntimeActivation> {
        #[cfg(windows)]
        {
            let bytes =
                crate::mcp_platform::storage_domain::read_verified_managed_installation_activation(
                    &self.root,
                    managed_mcp_id,
                )?
                .ok_or_else(|| {
                    McpPlatformError::new(
                        McpPlatformErrorCode::RuntimeControlUnavailable,
                        "managed runtime has no verified active installation",
                    )
                })?;
            let descriptor = serde_json::from_slice::<ActiveRuntimeDescriptor>(&bytes)
                .map_err(|_| integrity_error())?;
            return Ok(VerifiedManagedRuntimeActivation::new(descriptor.projection));
        }

        #[cfg(not(windows))]
        {
            let _ = managed_mcp_id;
            Err(McpPlatformError::new(
                McpPlatformErrorCode::RuntimeControlUnavailable,
                "managed runtime activation verification is unavailable",
            ))
        }
    }

    async fn active_version(&self, managed_mcp_id: &str) -> McpPlatformResult<Option<String>> {
        #[cfg(windows)]
        {
            let bytes =
                crate::mcp_platform::storage_domain::read_verified_managed_installation_activation(
                    &self.root,
                    managed_mcp_id,
                )?;
            return bytes
                .map(|bytes| {
                    serde_json::from_slice::<ActiveRuntimeDescriptor>(&bytes)
                        .map(|descriptor| descriptor.version)
                        .map_err(|_| integrity_error())
                })
                .transpose();
        }

        #[cfg(not(windows))]
        {
            let path = self.active_descriptor_path(managed_mcp_id)?;
            if let Some(parent) = path.parent() {
                verify_owned_read_path(&self.root, parent)?;
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
    }

    async fn remove_version(
        &self,
        managed_mcp_id: &str,
        version: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        #[cfg(windows)]
        {
            let _ = (managed_mcp_id, version, cancellation);
            revalidate_windows_managed_distribution_root(&self.root)?;
            return Err(windows_managed_distribution_mutation_unavailable());
        }

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
        #[cfg(windows)]
        {
            let _ = (managed_mcp_id, version, task_id, cancellation);
            revalidate_windows_managed_distribution_root(&self.root)?;
            return Err(windows_managed_distribution_mutation_unavailable());
        }

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
        #[cfg(windows)]
        {
            let _ = (managed_mcp_id, version, token);
            revalidate_windows_managed_distribution_root(&self.root)?;
            return Err(windows_managed_distribution_mutation_unavailable());
        }

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
        #[cfg(windows)]
        {
            let _ = (token, cancellation);
            revalidate_windows_managed_distribution_root(&self.root)?;
            return Err(windows_managed_distribution_mutation_unavailable());
        }

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

fn checked_managed_storage_capacity(
    required_peak_bytes: u64,
    available_bytes: u64,
) -> McpPlatformResult<ManagedStorageCapacity> {
    if available_bytes < required_peak_bytes {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::IntegrityUnavailable,
            "managed storage capacity is insufficient",
        ));
    }
    Ok(ManagedStorageCapacity {
        required_peak_bytes,
        available_bytes,
    })
}

#[cfg(test)]
mod capacity_contract_tests {
    use super::*;

    #[test]
    fn capacity_math_accepts_zero_requirement_and_exact_boundary_but_rejects_shortfall() {
        assert_eq!(
            checked_managed_storage_capacity(0, 0).unwrap(),
            ManagedStorageCapacity {
                required_peak_bytes: 0,
                available_bytes: 0,
            }
        );
        assert_eq!(
            checked_managed_storage_capacity(u64::MAX, u64::MAX).unwrap(),
            ManagedStorageCapacity {
                required_peak_bytes: u64::MAX,
                available_bytes: u64::MAX,
            }
        );
        assert_eq!(
            checked_managed_storage_capacity(11, 10).unwrap_err().code(),
            McpPlatformErrorCode::IntegrityUnavailable
        );
    }

    #[test]
    fn distribution_trait_default_capacity_hook_is_an_explicit_rejecting_fake_guard() {
        let source = include_str!("managed_distribution.rs");
        let start = source.find("pub trait DistributionEffectAdapter").unwrap();
        let end = source[start..].find("async fn install(").unwrap() + start;
        let default_hook = &source[start..end];
        assert!(default_hook.contains("managed storage capacity preflight is unavailable"));
        assert!(!default_hook.contains("ManagedStorageCapacity {"));
    }

    #[test]
    fn windows_managed_install_routes_the_verified_tree_only_through_directory_writer() {
        let source = include_str!("managed_distribution.rs");
        let start = source
            .find("async fn install_windows_via_directory_writer")
            .unwrap();
        let end = source[start..].find("async fn install_external").unwrap() + start;
        let implementation = &source[start..end];

        assert!(implementation.contains("self.verifier.verify"));
        assert!(implementation.contains("windows_directory_writer_payloads"));
        assert!(implementation.contains("promote_verified_managed_installations"));
        assert!(implementation.contains("expected_tree_digest"));
        for direct_root_mutation in [
            "tokio::fs::rename",
            "tokio::fs::create_dir_all",
            "tokio::fs::remove_dir_all",
            "prepare_owned_directory(&self.root",
        ] {
            assert!(
                !implementation.contains(direct_root_mutation),
                "Windows managed installation must not bypass DirectoryWriter: {direct_root_mutation}"
            );
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
            ));
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
        verify_owned_read_path(root, parent)?;
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
    // A storage segment is passed to Win32 as one component. Reject every
    // spelling that can be normalized or resolved as a distinct DOS alias.
    if value.is_empty()
        || value.contains(['/', '\\', ':', '\0'])
        || value == "."
        || value == ".."
        || value.ends_with(['.', ' '])
        || value.contains('~')
        || unsafe_windows_component(value)
    {
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

fn verify_owned_read_path(owner_root: &Path, target: &Path) -> McpPlatformResult<()> {
    let relative = target
        .strip_prefix(owner_root)
        .map_err(|_| unsafe_archive())?;
    verify_normal_directory(owner_root)?;
    let mut current = owner_root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(unsafe_archive());
        }
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse(&metadata)
                {
                    return Err(unsafe_archive());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
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
            if is_task_local_installation_metadata(&relative) {
                if !metadata.is_file() {
                    return Err(verification_error());
                }
                continue;
            }
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

#[cfg(windows)]
struct WindowsDirectoryWriterPayload {
    relative_segments: Vec<String>,
    bytes: Vec<u8>,
}

/// The extraction scratch directory is outside the managed root.  It is
/// converted into an immutable byte manifest before the writer lease is
/// acquired, so no path from the scratch tree is ever handed to the Windows
/// managed-root writer.
#[cfg(windows)]
fn windows_directory_writer_payloads(
    root: &Path,
    managed_mcp_id: &str,
    version: &str,
) -> McpPlatformResult<(
    crate::mcp_platform::storage_domain::DirectoryPayloadManifest,
    Vec<WindowsDirectoryWriterPayload>,
)> {
    let mut pending = vec![root.to_path_buf()];
    let mut payloads = Vec::new();
    while let Some(directory) = pending.pop() {
        verify_normal_directory(&directory)?;
        let entries = std::fs::read_dir(&directory).map_err(|_| verification_error())?;
        for entry in entries {
            let entry = entry.map_err(|_| verification_error())?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| verification_error())?;
            if metadata.file_type().is_symlink() || is_reparse(&metadata) {
                return Err(verification_error());
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.is_file() || payloads.len() >= 256 {
                return Err(verification_error());
            }
            let relative = path.strip_prefix(root).map_err(|_| verification_error())?;
            let mut relative_segments = vec![
                managed_mcp_id.to_string(),
                "versions".to_string(),
                version.to_string(),
            ];
            for component in relative.components() {
                let Component::Normal(component) = component else {
                    return Err(verification_error());
                };
                let component = component.to_str().ok_or_else(verification_error)?;
                if component.len() > 64 {
                    return Err(verification_error());
                }
                validate_storage_segment(component)?;
                relative_segments.push(component.to_string());
            }
            if relative_segments.len() > 8 {
                return Err(verification_error());
            }
            let bytes = std::fs::read(&path).map_err(|_| verification_error())?;
            if bytes.len() as u64 != metadata.len() {
                return Err(verification_error());
            }
            payloads.push(WindowsDirectoryWriterPayload {
                relative_segments,
                bytes,
            });
        }
    }
    payloads.sort_by(|left, right| left.relative_segments.cmp(&right.relative_segments));
    if payloads.is_empty() {
        return Err(verification_error());
    }
    let descriptor = payloads
        .iter()
        .find(|payload| {
            payload.relative_segments.len() == 4
                && payload.relative_segments[0] == managed_mcp_id
                && payload.relative_segments[1] == "versions"
                && payload.relative_segments[2] == version
                && payload.relative_segments[3] == ".goose-runtime-descriptor.json"
        })
        .ok_or_else(verification_error)?;
    let evidence = WindowsCommittedVersionEvidence {
        descriptor_sha256: hex_digest(&descriptor.bytes),
        tree_sha256: windows_directory_tree_digest(&payloads),
    };
    let commitment = serde_json::to_vec(&evidence).map_err(|_| verification_error())?;
    payloads.push(WindowsDirectoryWriterPayload {
        relative_segments: vec![
            managed_mcp_id.to_string(),
            "versions".to_string(),
            version.to_string(),
            ".goose-committed-version.json".to_string(),
        ],
        bytes: commitment,
    });
    payloads.sort_by(|left, right| left.relative_segments.cmp(&right.relative_segments));
    let entries = payloads
        .iter()
        .map(
            |payload| crate::mcp_platform::storage_domain::DirectoryPayloadManifestEntry {
                relative_segments: payload.relative_segments.clone(),
                size: payload.bytes.len() as u64,
                sha256: hex_digest(Sha256::digest(&payload.bytes).as_slice()),
            },
        )
        .collect();
    Ok((
        crate::mcp_platform::storage_domain::DirectoryPayloadManifest { entries },
        payloads,
    ))
}

#[cfg(windows)]
fn windows_directory_tree_digest(payloads: &[WindowsDirectoryWriterPayload]) -> String {
    let mut digest = Sha256::new();
    for payload in payloads {
        for segment in payload.relative_segments.iter().skip(3) {
            digest.update(segment.as_bytes());
            digest.update([0]);
        }
        digest.update((payload.bytes.len() as u64).to_le_bytes());
        digest.update(hex_digest(&payload.bytes).as_bytes());
    }
    hex_digest(&digest.finalize())
}

#[cfg(windows)]
fn write_windows_materialization_metadata(
    root: &Path,
    descriptor: &ActiveRuntimeDescriptor,
    evidence: &ArtifactVerificationEvidence,
    marker: &InstallationMarker,
) -> McpPlatformResult<()> {
    for (name, bytes) in [
        (
            ".goose-runtime-descriptor.json",
            serde_json::to_vec(descriptor).map_err(|_| repository_error())?,
        ),
        (
            ".goose-artifact-evidence.json",
            serde_json::to_vec(evidence).map_err(|_| repository_error())?,
        ),
        (
            ".goose-installation-owner.json",
            serde_json::to_vec(marker).map_err(|_| repository_error())?,
        ),
    ] {
        let path = root.join(name);
        if path.exists() {
            return Err(verification_error());
        }
        std::fs::write(path, bytes).map_err(|_| repository_error())?;
    }
    Ok(())
}

fn is_task_local_installation_metadata(relative: &str) -> bool {
    matches!(
        relative,
        ".goose-runtime-descriptor.json"
            | ".goose-artifact-evidence.json"
            | ".goose-external-evidence.json"
            | ".goose-installation-owner.json"
    )
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
        #[cfg(windows)]
        {
            let _ = (effect, cancellation);
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::RuntimeControlUnavailable,
                "windows managed artifact cache requires a handle-relative cache capability",
            ));
        }

        #[cfg(not(windows))]
        {
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
                        return Err(error);
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

pub(crate) async fn resolve_public_addresses(
    host: &str,
    port: u16,
) -> McpPlatformResult<Vec<std::net::SocketAddr>> {
    if matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "metadata.google.internal"
    ) {
        return Err(unsafe_fetch());
    }
    let literal_host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let addresses = if let Ok(ip) = literal_host.parse::<std::net::IpAddr>() {
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

#[cfg(test)]
mod external_adapter_tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, AtomicU64};

    use crate::agents::ExtensionConfig;
    use crate::mcp_platform::external_distribution::{
        DirectProcessRunner, DockerToolCapability, ExternalToolCapabilities, GitToolCapability,
        MountPermissionResolver, PublicNetworkResolver, ResolvedMountGrant,
    };
    use crate::mcp_platform::repository::{ManifestRecord, SqliteMcpPlatformRepository};
    use crate::mcp_platform::service::{
        Clock, IdGenerator, InstallConfirmInput, ManagedGetInput, ManagedSupplyChainSummary,
        McpPlatformService, McpPlatformServiceOptions, PlanCreateInput, PlanIntent, UserDecision,
    };
    use crate::mcp_platform::{
        ConfigAuthRequirementResolver, ConfigProjectionSink, CoreTransportProjectionAdapter,
        EmptyHostIntegrationAdapter, HealthAdapterResult, HealthCheckAdapter, HealthDetailCode,
        HealthExecution, HealthResultCode, LifecyclePorts, ManifestProof,
        RuntimeCapabilitySnapshot, SafeRegistrationEffectAdapter, TaskStatus, TrustTier,
    };

    #[derive(Default)]
    struct RecordingProcess {
        requests: Mutex<Vec<DirectProcessRequest>>,
        image_mismatch: AtomicBool,
        unsafe_git_tree: AtomicBool,
    }

    #[async_trait]
    impl DirectProcessRunner for RecordingProcess {
        async fn run(
            &self,
            request: DirectProcessRequest,
            cancellation: &CancellationToken,
        ) -> McpPlatformResult<DirectProcessOutput> {
            if cancellation.is_cancelled() {
                return Err(cancelled());
            }
            let output = match request.argv.as_slice() {
                [command, format, _] if command == "version" && format == "--format" => {
                    b"25.0.3\n".to_vec()
                }
                [command, format, _] if command == "info" && format == "--format" => {
                    br#"["name=rootless"]"#.to_vec()
                }
                [command, reference] if command == "pull" => {
                    assert!(reference.contains("@sha256:"));
                    Vec::new()
                }
                [image, inspect, format, _, reference]
                    if image == "image" && inspect == "inspect" && format == "--format" =>
                {
                    let reference = if self.image_mismatch.load(Ordering::SeqCst) {
                        "registry.example/other@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                    } else {
                        reference
                    };
                    format!("[\"{reference}\"]\nsha256:{}\n", "b".repeat(64)).into_bytes()
                }
                args if args.iter().any(|argument| argument == "init") => Vec::new(),
                args if args.iter().any(|argument| argument == "fetch") => Vec::new(),
                args if args.iter().any(|argument| argument == "cat-file") => b"commit\n".to_vec(),
                args if args.iter().any(|argument| argument == "rev-parse") => {
                    format!("{}\n", "d".repeat(40)).into_bytes()
                }
                args if args.iter().any(|argument| argument == "ls-tree") => {
                    if self.unsafe_git_tree.load(Ordering::SeqCst) {
                        format!("120000 blob {} 4\tdir/link\0", "e".repeat(40)).into_bytes()
                    } else {
                        format!(
                            "100644 blob {} 40\tpackage.json\0100755 blob {} 4\tdir/server.js\0",
                            "e".repeat(40),
                            "f".repeat(40)
                        )
                        .into_bytes()
                    }
                }
                args if args.iter().any(|argument| argument == "archive") => {
                    let output = args
                        .iter()
                        .find_map(|argument| argument.strip_prefix("--output="))
                        .unwrap();
                    let file = std::fs::File::create(output).unwrap();
                    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
                    let mut archive = tar::Builder::new(encoder);
                    let package = br#"{"bin":{"server":"dir/server.js"}}"#;
                    let mut header = tar::Header::new_gnu();
                    header.set_size(package.len() as u64);
                    header.set_mode(0o644);
                    header.set_cksum();
                    archive
                        .append_data(&mut header, "payload/package.json", &package[..])
                        .unwrap();
                    let server = b"mcp\n";
                    let mut header = tar::Header::new_gnu();
                    header.set_size(server.len() as u64);
                    header.set_mode(0o755);
                    header.set_cksum();
                    archive
                        .append_data(&mut header, "payload/dir/server.js", &server[..])
                        .unwrap();
                    archive.into_inner().unwrap().finish().unwrap();
                    Vec::new()
                }
                args => panic!("unexpected Docker request: {args:?}"),
            };
            self.requests.lock().unwrap().push(request);
            Ok(DirectProcessOutput {
                success: true,
                stdout: output,
                stderr: Vec::new(),
            })
        }
    }

    struct NoMounts;
    impl MountPermissionResolver for NoMounts {
        fn resolve(&self, _permission_id: &str) -> McpPlatformResult<ResolvedMountGrant> {
            Err(verification_error())
        }
    }

    struct PublicDns;
    #[async_trait]
    impl PublicNetworkResolver for PublicDns {
        async fn resolve(
            &self,
            _host: &str,
            port: u16,
        ) -> McpPlatformResult<Vec<std::net::SocketAddr>> {
            Ok(vec![std::net::SocketAddr::new(
                "93.184.216.34".parse().unwrap(),
                port,
            )])
        }
    }

    fn docker_effect() -> ManagedInstallEffect {
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../documentation/static/schemas/examples/manual-stdio.json"
        ))
        .unwrap();
        let digest = "a".repeat(64);
        value["id"] = serde_json::json!("local.example.docker-test");
        value["host_integrations"] = serde_json::json!([]);
        value["permissions"] = serde_json::json!([
            {"id":"spawn","kind":"process_spawn","reason":"Spawn Docker.","required":true},
            {"id":"docker","kind":"docker","reason":"Use Docker.","required":true},
            {"id":"network","kind":"network","reason":"Pull image.","required":true}
        ]);
        value["distribution"] = serde_json::json!({
            "type":"docker",
            "image":"registry.example/team/server",
            "digest":{"algorithm":"sha256","value":digest},
            "entrypoint":{"executable":"/server","args":["--stdio"],"environment_keys":[]},
            "mounts":[]
        });
        let manifest = crate::mcp_platform::parse_manifest(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .manifest()
            .clone();
        ManagedInstallEffect {
            task_id: "task_000001".to_string(),
            managed_mcp_id: "managed_000001".to_string(),
            manifest,
            source_url: "https://registry.example".to_string(),
            expected_sha256: digest.clone(),
            expected_size_bytes: None,
            platform_selector: "docker/immutable".to_string(),
            now_ms: 100,
            operation: TaskOperation::Install,
            expected_tree_digest: None,
            rebuild_uncommitted_version: false,
            external_acquisition: Some(ExternalManagedAcquisition::Docker {
                image: "registry.example/team/server".to_string(),
                digest,
                mount_plan_digest: digest_serializable(&Vec::<
                    crate::mcp_platform::manifest::DockerMount,
                >::new())
                .unwrap(),
            }),
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_production_cache_fetcher_fails_closed_before_path_io() {
        let fetcher = ProductionArtifactFetcher::new(PathBuf::from(r"D:\\untrusted-cache"))
            .expect("constructing the fetcher is inert");
        let error = fetcher
            .fetch(
                &ArtifactFetchEffect {
                    operation_id: "operation_000001".to_string(),
                    source_url: "https://example.invalid/artifact".to_string(),
                    redirect_policy: RedirectPolicy::DenyAll,
                    expected_sha256: "0".repeat(64),
                    expected_size_bytes: None,
                    maximum_size_bytes: 1,
                    timeout_seconds: 1,
                },
                &CancellationToken::new(),
            )
            .await
            .expect_err("Windows cache writes require a handle-relative capability");
        assert_eq!(
            error.code(),
            McpPlatformErrorCode::RuntimeControlUnavailable
        );
    }

    fn docker_adapter(
        root: PathBuf,
        process: Arc<RecordingProcess>,
    ) -> ProductionDistributionEffectAdapter {
        docker_adapter_with_policy(root, process, true)
    }

    #[cfg(windows)]
    fn recursive_root_snapshot(root: &Path) -> Vec<(PathBuf, bool, Vec<u8>)> {
        fn visit(root: &Path, directory: &Path, snapshot: &mut Vec<(PathBuf, bool, Vec<u8>)>) {
            let mut entries = std::fs::read_dir(directory)
                .expect("read managed storage directory")
                .map(|entry| entry.expect("managed storage entry"))
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .expect("managed storage entry remains below root")
                    .to_path_buf();
                if entry
                    .file_type()
                    .expect("managed storage entry type")
                    .is_dir()
                {
                    snapshot.push((relative, true, Vec::new()));
                    visit(root, &path, snapshot);
                } else {
                    snapshot.push((
                        relative,
                        false,
                        std::fs::read(path).expect("managed storage file"),
                    ));
                }
            }
        }

        let mut snapshot = Vec::new();
        visit(root, root, &mut snapshot);
        snapshot
    }

    #[cfg(windows)]
    fn write_committed_windows_version(
        version_root: &Path,
        descriptor: &ActiveRuntimeDescriptor,
        payload: (&str, &[u8]),
    ) {
        let descriptor_bytes = serde_json::to_vec(descriptor).expect("runtime descriptor");
        let payloads = vec![
            WindowsDirectoryWriterPayload {
                relative_segments: vec![
                    "managed_000001".to_string(),
                    "versions".to_string(),
                    "1.0.0".to_string(),
                    ".goose-runtime-descriptor.json".to_string(),
                ],
                bytes: descriptor_bytes.clone(),
            },
            WindowsDirectoryWriterPayload {
                relative_segments: vec![
                    "managed_000001".to_string(),
                    "versions".to_string(),
                    "1.0.0".to_string(),
                    payload.0.to_string(),
                ],
                bytes: payload.1.to_vec(),
            },
        ];
        let evidence = WindowsCommittedVersionEvidence {
            descriptor_sha256: hex_digest(&descriptor_bytes),
            tree_sha256: windows_directory_tree_digest(&payloads),
        };
        std::fs::write(
            version_root.join(".goose-runtime-descriptor.json"),
            descriptor_bytes,
        )
        .expect("committed runtime descriptor");
        std::fs::write(
            version_root.join(".goose-committed-version.json"),
            serde_json::to_vec(&evidence).expect("committed version evidence"),
        )
        .expect("committed version evidence");
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires a writable D: volume; run manually with --ignored on a supported Windows host"]
    async fn windows_read_paths_leave_an_initialized_root_unchanged() {
        let temporary = match tempfile::Builder::new()
            .prefix("goose-managed-read-only-")
            .tempdir_in(std::path::Path::new(r"D:\"))
        {
            Ok(temporary) => temporary,
            Err(error) => panic!("create D managed root: {error}"),
        };
        for artifact in [
            "platform.db",
            "platform.provider-binding",
            "platform.keyring-reference",
            "platform.anchor",
        ] {
            std::fs::write(temporary.path().join(artifact), b"committed")
                .expect("committed platform artifact");
        }
        let version_root = temporary
            .path()
            .join("platform.installations")
            .join("managed_000001")
            .join("versions")
            .join("1.0.0");
        std::fs::create_dir_all(version_root.join("nested")).expect("nested managed version");
        std::fs::write(version_root.join("nested").join("server.exe"), b"committed")
            .expect("committed nested payload");
        let active = ActiveRuntimeDescriptor {
            version: "1.0.0".to_string(),
            projection: ConnectionProjection::ManagedDockerStdio {
                name: "managed-test".to_string(),
                description: "managed test".to_string(),
                executable: "docker".to_string(),
                args: Vec::new(),
                cwd: None,
                timeout_seconds: None,
            },
        };
        write_committed_windows_version(
            &version_root,
            &active,
            ("nested/server.exe", b"committed"),
        );
        std::fs::write(
            temporary
                .path()
                .join("platform.installations")
                .join("managed_000001")
                .join("active.json"),
            serde_json::to_vec(&active).expect("active descriptor"),
        )
        .expect("committed active descriptor");

        let process = Arc::new(RecordingProcess::default());
        let adapter = docker_adapter(temporary.path().to_path_buf(), Arc::clone(&process));
        let before = recursive_root_snapshot(temporary.path());

        assert!(adapter
            .version_exists("managed_000001", "1.0.0")
            .await
            .expect("existing version is installed"));
        assert!(!adapter
            .version_exists("managed_000001", "2.0.0")
            .await
            .expect("missing version is not installed"));
        assert_eq!(
            adapter
                .active_version("managed_000001")
                .await
                .expect("active metadata is readable"),
            Some("1.0.0".to_string())
        );
        let docker_inspection = adapter
            .inspect_installed(&docker_effect(), &CancellationToken::new())
            .await
            .expect_err("Windows Docker inspection must fail before a daemon call");
        assert_eq!(
            docker_inspection.code(),
            McpPlatformErrorCode::RuntimeControlUnavailable
        );
        assert_eq!(
            docker_inspection.message(),
            "windows Docker managed distribution inspection requires a read-only daemon capability"
        );

        let mut non_external = docker_effect();
        non_external.external_acquisition = None;
        non_external.expected_tree_digest = Some("0".repeat(64));
        assert!(adapter
            .inspect_installed(&non_external, &CancellationToken::new())
            .await
            .is_err());

        assert_eq!(recursive_root_snapshot(temporary.path()), before);
        assert!(process.requests.lock().expect("requests").is_empty());
        for artifact in ["cache", "staging", "goose-staging", "docker-runtime"] {
            assert!(!temporary.path().join(artifact).exists(), "{artifact}");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires a writable D: volume; run manually with --ignored on a supported Windows host"]
    async fn windows_production_install_fails_closed_before_cache_or_legacy_staging_writes() {
        let temporary = match tempfile::Builder::new()
            .prefix("goose-managed-distribution-")
            .tempdir_in(std::path::Path::new(r"D:\"))
        {
            Ok(temporary) => temporary,
            Err(error) => panic!("create D managed root: {error}"),
        };
        for artifact in [
            "platform.db",
            "platform.provider-binding",
            "platform.keyring-reference",
            "platform.anchor",
        ] {
            std::fs::write(temporary.path().join(artifact), b"committed")
                .expect("committed platform artifact");
        }
        let version_root = temporary
            .path()
            .join("platform.installations")
            .join("managed_000001")
            .join("versions")
            .join("1.0.0");
        std::fs::create_dir_all(&version_root).expect("committed installation version");
        std::fs::write(version_root.join("server.exe"), b"committed")
            .expect("committed installation payload");
        let descriptor = ActiveRuntimeDescriptor {
            version: "1.0.0".to_string(),
            projection: ConnectionProjection::ManagedDockerStdio {
                name: "managed-test".to_string(),
                description: "managed test".to_string(),
                executable: "docker".to_string(),
                args: Vec::new(),
                cwd: None,
                timeout_seconds: None,
            },
        };
        write_committed_windows_version(&version_root, &descriptor, ("server.exe", b"committed"));

        let process = Arc::new(RecordingProcess::default());
        let adapter = docker_adapter(temporary.path().to_path_buf(), Arc::clone(&process));
        assert!(adapter
            .version_exists("managed_000001", "1.0.0")
            .await
            .expect("committed runtime discovery"));
        assert_eq!(
            adapter
                .active_version("managed_000001")
                .await
                .expect("committed runtime activation discovery"),
            None
        );
        let error = adapter
            .install(&docker_effect(), &CancellationToken::new())
            .await
            .expect_err("legacy path-based installation must be unavailable on Windows");

        assert_eq!(
            error.code(),
            McpPlatformErrorCode::RuntimeControlUnavailable
        );
        assert_eq!(
            error.message(),
            "windows managed distribution mutation requires the handle-relative installation protocol"
        );
        for error in [
            adapter
                .activate_version(
                    "managed_000001",
                    "1.0.0",
                    "task_000001",
                    &CancellationToken::new(),
                )
                .await
                .expect_err("activation must not regain path-based mutation"),
            adapter
                .restore_activation("managed_000001", None, "task_000001")
                .await
                .expect_err("activation rollback must not regain path-based mutation"),
            adapter
                .remove_version("managed_000001", "1.0.0", &CancellationToken::new())
                .await
                .expect_err("uninstall must not regain path-based mutation"),
            adapter
                .quarantine_version(
                    "managed_000001",
                    "1.0.0",
                    "task_000001",
                    &CancellationToken::new(),
                )
                .await
                .expect_err("repair must not regain path-based mutation"),
            adapter
                .restore_quarantined("managed_000001", "1.0.0", "task_000001")
                .await
                .expect_err("repair rollback must not regain path-based mutation"),
            adapter
                .purge_quarantined("task_000001", &CancellationToken::new())
                .await
                .expect_err("quarantine cleanup must not regain path-based mutation"),
        ] {
            assert_eq!(
                error.code(),
                McpPlatformErrorCode::RuntimeControlUnavailable
            );
        }
        assert!(process.requests.lock().expect("requests").is_empty());
        assert!(!temporary.path().join("cache").exists());
        assert!(!temporary.path().join("staging").exists());
        assert!(!temporary.path().join("goose-staging").exists());
        assert!(version_root.is_dir());
    }

    fn docker_adapter_with_policy(
        root: PathBuf,
        process: Arc<RecordingProcess>,
        policy_allowed: bool,
    ) -> ProductionDistributionEffectAdapter {
        let capabilities = ExternalToolCapabilities::for_test(
            Some(DockerToolCapability::for_test(
                PathBuf::from("server-owned-docker"),
                "ignored",
                false,
                policy_allowed,
            )),
            Some(GitToolCapability::for_test(PathBuf::from(
                "server-owned-git",
            ))),
        );
        ProductionDistributionEffectAdapter::new_with_runtime_and_external(
            root,
            RuntimeCapabilities::for_test(None, None, None),
            ExternalDistributionPorts {
                process,
                mount_permissions: Arc::new(NoMounts),
                capabilities,
                network: Arc::new(PublicDns),
            },
        )
        .unwrap()
    }

    fn git_effect() -> ManagedInstallEffect {
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../documentation/static/schemas/examples/manual-stdio.json"
        ))
        .unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let acquisition = "c".repeat(64);
        value["id"] = serde_json::json!("local.example.git-test");
        value["host_integrations"] = serde_json::json!([]);
        value["permissions"] = serde_json::json!([
            {"id":"spawn","kind":"process_spawn","reason":"Spawn Git.","required":true},
            {"id":"network","kind":"network","reason":"Fetch commit.","required":true}
        ]);
        value["distribution"] = serde_json::json!({
            "type":"git_dev",
            "repository":"https://git.example/team/server.git",
            "commit":commit,
            "adapter":"npm",
            "entrypoint":{"executable":"${installation.bin}/server","args":["--stdio"],"environment_keys":[]}
        });
        let manifest = crate::mcp_platform::parse_manifest(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .manifest()
            .clone();
        ManagedInstallEffect {
            task_id: "task_git_000001".to_string(),
            managed_mcp_id: "managed_git_000001".to_string(),
            manifest,
            source_url: "https://git.example/team/server.git".to_string(),
            expected_sha256: acquisition.clone(),
            expected_size_bytes: None,
            platform_selector: "git_dev/exact_commit".to_string(),
            now_ms: 200,
            operation: TaskOperation::Install,
            expected_tree_digest: None,
            rebuild_uncommitted_version: false,
            external_acquisition: Some(ExternalManagedAcquisition::GitDev {
                repository_origin: "https://git.example".to_string(),
                repository: "https://git.example/team/server.git".to_string(),
                commit: commit.to_string(),
                subdirectory: None,
                underlying_adapter: GitDevAdapter::Npm,
                acquisition_digest: acquisition,
            }),
        }
    }

    fn git_adapter(
        root: PathBuf,
        process: Arc<RecordingProcess>,
    ) -> ProductionDistributionEffectAdapter {
        let capabilities = ExternalToolCapabilities::for_test(
            None,
            Some(GitToolCapability::for_test(PathBuf::from(
                "server-owned-git",
            ))),
        );
        ProductionDistributionEffectAdapter::new_with_runtime_and_external(
            root,
            RuntimeCapabilities::for_test(Some(PathBuf::from("server-owned-node")), None, None),
            ExternalDistributionPorts {
                process,
                mount_permissions: Arc::new(NoMounts),
                capabilities,
                network: Arc::new(PublicDns),
            },
        )
        .unwrap()
    }

    struct FixedClock(AtomicI64);

    impl Clock for FixedClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[derive(Default)]
    struct SequenceIds(AtomicU64);

    impl IdGenerator for SequenceIds {
        fn next_id(&self, prefix: &str) -> String {
            format!("{prefix}_{:06}", self.0.fetch_add(1, Ordering::SeqCst))
        }
    }

    struct HealthyExternalProjection;

    #[async_trait]
    impl HealthCheckAdapter for HealthyExternalProjection {
        fn adapter_id(&self) -> &'static str {
            "mcp_health"
        }

        fn adapter_version(&self) -> &'static str {
            "1"
        }

        async fn run(
            &self,
            _execution: HealthExecution,
            _cancellation: CancellationToken,
        ) -> McpPlatformResult<HealthAdapterResult> {
            Ok(HealthAdapterResult {
                result_code: HealthResultCode::Healthy,
                latency_ms: 1,
                capabilities_digest: None,
                tools_digest: None,
                detail_code: HealthDetailCode::McpInitializeSucceeded,
            })
        }
    }

    async fn install_through_service(
        service: &McpPlatformService,
        repository: &SqliteMcpPlatformRepository,
        manifest: &Manifest,
        trust_tier: TrustTier,
        key: &str,
    ) -> crate::mcp_platform::service::ManagedMcpDetail {
        let verified =
            crate::mcp_platform::parse_manifest(&serde_json::to_vec(manifest).unwrap()).unwrap();
        let manifest_digest = verified.digest().to_string();
        repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier,
                source_metadata: Default::default(),
                created_at_ms: 1_000,
            })
            .await
            .unwrap();
        let context = service.trusted_local_context();
        let plan = service
            .plan_create(
                &context,
                PlanCreateInput {
                    intent: PlanIntent::Install { manifest_digest },
                    idempotency_key: format!("plan_{key}"),
                },
            )
            .await
            .unwrap();
        let managed_mcp_id = plan.target.managed_mcp_id.clone().unwrap();
        let task = service
            .install_confirm(
                &context,
                InstallConfirmInput {
                    plan_id: plan.plan_id,
                    plan_digest: plan.plan_digest,
                    decision: UserDecision::Confirm,
                    idempotency_key: format!("task_{key}"),
                },
            )
            .await
            .unwrap();
        assert!(service.runner_tick().await.unwrap());
        assert_eq!(
            repository.get_task(&task.task_id).await.unwrap().status,
            TaskStatus::Succeeded
        );
        service
            .managed_get(&context, ManagedGetInput { managed_mcp_id })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn service_runner_sqlite_uses_production_adapter_for_docker_and_git_dev() {
        let directory = tempfile::tempdir().unwrap();
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_path(directory.path().join("platform.db"))
                .await
                .unwrap(),
        );
        let process = Arc::new(RecordingProcess::default());
        let capabilities = ExternalToolCapabilities::for_test(
            Some(DockerToolCapability::for_test(
                PathBuf::from("server-owned-docker"),
                "ignored",
                false,
                true,
            )),
            Some(GitToolCapability::for_test(PathBuf::from(
                "server-owned-git",
            ))),
        );
        let distribution = Arc::new(
            ProductionDistributionEffectAdapter::new_with_runtime_and_external(
                directory.path().join("managed"),
                RuntimeCapabilities::for_test(Some(PathBuf::from("server-owned-node")), None, None),
                ExternalDistributionPorts {
                    process: process.clone(),
                    mount_permissions: Arc::new(NoMounts),
                    capabilities,
                    network: Arc::new(PublicDns),
                },
            )
            .unwrap(),
        );
        let config = Arc::new(
            crate::config::Config::new_with_file_secrets(
                directory.path().join("config.yaml"),
                directory.path().join("secrets.yaml"),
            )
            .unwrap(),
        );
        let ports = LifecyclePorts {
            registration: Arc::new(SafeRegistrationEffectAdapter::default()),
            host_integration: Arc::new(EmptyHostIntegrationAdapter),
            transport: Arc::new(CoreTransportProjectionAdapter),
            auth: Arc::new(ConfigAuthRequirementResolver),
            health: Arc::new(HealthyExternalProjection),
            projection_sink: Arc::new(ConfigProjectionSink::with_config(config.clone())),
        };
        let service = McpPlatformService::new_with_external_capabilities(
            repository.clone(),
            Arc::new(FixedClock(AtomicI64::new(1_000))),
            Arc::new(SequenceIds::default()),
            McpPlatformServiceOptions {
                compatibility_target: crate::mcp_platform::CompatibilityTarget {
                    platform: crate::mcp_platform::manifest::Platform::Linux,
                    arch: crate::mcp_platform::manifest::Architecture::X86_64,
                },
                plan_ttl_ms: 60_000,
                development_mode: true,
                docker_daemon_policy_allowed: true,
            },
            ports,
            distribution,
            RuntimeCapabilitySnapshot {
                node_available: true,
                python_major_minor: None,
            },
            true,
            true,
        );

        let docker = install_through_service(
            &service,
            &repository,
            &docker_effect().manifest,
            TrustTier::Official,
            "production_docker",
        )
        .await;
        assert!(matches!(
            docker.supply_chain,
            Some(ManagedSupplyChainSummary::Docker {
                image_digest,
                daemon_version,
                rootless: true,
                ..
            }) if image_digest == "a".repeat(64) && daemon_version == "25.0.3"
        ));
        let docker_projection = crate::config::extensions::get_extension_entry_by_key_with_config(
            &config,
            &docker.extension_config_key,
        )
        .unwrap();
        match docker_projection.config {
            ExtensionConfig::Stdio {
                cmd, args, envs, ..
            } => {
                assert_eq!(cmd, "server-owned-docker");
                assert!(envs.has_managed_docker_isolation());
                assert!(args.iter().any(|argument| {
                    argument == &format!("registry.example/team/server@sha256:{}", "a".repeat(64))
                }));
            }
            projection => panic!("unexpected Docker projection: {projection:?}"),
        }

        let git = install_through_service(
            &service,
            &repository,
            &git_effect().manifest,
            TrustTier::Local,
            "production_git",
        )
        .await;
        assert!(matches!(
            git.supply_chain,
            Some(ManagedSupplyChainSummary::GitDev {
                commit,
                git_tree_id,
                ..
            }) if commit == "0123456789abcdef0123456789abcdef01234567"
                && git_tree_id == "d".repeat(40)
        ));
        let git_projection = crate::config::extensions::get_extension_entry_by_key_with_config(
            &config,
            &git.extension_config_key,
        )
        .unwrap();
        match git_projection.config {
            ExtensionConfig::Stdio { cmd, args, .. } => {
                assert_eq!(cmd, "server-owned-node");
                assert!(args
                    .iter()
                    .any(|argument| argument.ends_with("dir/server.js")));
            }
            projection => panic!("unexpected GitDev projection: {projection:?}"),
        }

        let requests = process.requests.lock().unwrap();
        assert!(requests
            .iter()
            .any(|request| request.argv.iter().any(|argument| argument == "pull")));
        assert!(requests
            .iter()
            .any(|request| request.argv.iter().any(|argument| argument == "fetch")));
        assert!(requests
            .iter()
            .any(|request| request.argv.iter().any(|argument| argument == "archive")));
    }

    #[tokio::test]
    async fn docker_inspect_rebuilds_projection_and_rejects_tampered_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let process = Arc::new(RecordingProcess::default());
        let adapter = docker_adapter(directory.path().join("managed"), process.clone());
        let mut effect = docker_effect();
        let installed = adapter
            .install(&effect, &CancellationToken::new())
            .await
            .unwrap();
        effect.expected_tree_digest = Some(installed.materialized_tree_digest.clone());
        assert_eq!(
            adapter
                .inspect_installed(&effect, &CancellationToken::new())
                .await
                .unwrap()
                .projection,
            installed.projection
        );
        let root = installed.installation_root;
        for (name, mutation) in [
            (
                ".goose-runtime-descriptor.json",
                ("projection.executable", serde_json::json!("host-command")),
            ),
            (
                ".goose-artifact-evidence.json",
                ("installed_at_ms", serde_json::json!(999)),
            ),
            (
                ".goose-external-evidence.json",
                ("image_digest", serde_json::json!("f".repeat(64))),
            ),
            (
                ".goose-installation-owner.json",
                ("task_id", serde_json::json!("other-task")),
            ),
        ] {
            let path = root.join(name);
            let original = tokio::fs::read(&path).await.unwrap();
            let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
            let (field, replacement) = mutation;
            if let Some((parent, child)) = field.split_once('.') {
                value[parent][child] = replacement;
            } else {
                value[field] = replacement;
            }
            tokio::fs::write(&path, serde_json::to_vec(&value).unwrap())
                .await
                .unwrap();
            assert_eq!(
                adapter
                    .inspect_installed(&effect, &CancellationToken::new())
                    .await
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::IntegrityError,
                "{name}"
            );
            tokio::fs::write(path, original).await.unwrap();
        }
        let requests = process.requests.lock().unwrap();
        assert!(requests.iter().all(|request| {
            request.environment.keys().collect::<Vec<_>>() == vec!["DOCKER_CONFIG", "HOME"]
                && request.cwd.is_none()
                && request.stdin.is_none()
        }));
        let run = match &installed.projection {
            ConnectionProjection::ManagedDockerStdio { args, .. } => args,
            projection => panic!("unexpected projection: {projection:?}"),
        };
        for required in ["--rm", "--read-only", "--cap-drop", "--security-opt"] {
            assert!(run.iter().any(|argument| argument == required));
        }
        for forbidden in [
            "--privileged",
            "--network=host",
            "--pid=host",
            "--ipc=host",
            "--device",
            "/var/run/docker.sock",
        ] {
            assert!(!run.iter().any(|argument| argument.contains(forbidden)));
        }
    }

    #[tokio::test]
    async fn docker_inspect_rejects_exact_repo_digest_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let process = Arc::new(RecordingProcess::default());
        process.image_mismatch.store(true, Ordering::SeqCst);
        let adapter = docker_adapter(directory.path().join("managed"), process);
        assert_eq!(
            adapter
                .install(&docker_effect(), &CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::ImageDigestMismatch
        );
    }

    #[tokio::test]
    async fn docker_execution_honors_injected_server_policy() {
        let directory = tempfile::tempdir().unwrap();
        let process = Arc::new(RecordingProcess::default());
        let adapter =
            docker_adapter_with_policy(directory.path().join("managed"), process.clone(), false);
        assert_eq!(
            adapter
                .install(&docker_effect(), &CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::DaemonPolicyDenied
        );
        assert!(process.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn git_fetch_is_dns_pinned_isolated_and_recursively_audited() {
        let directory = tempfile::tempdir().unwrap();
        let process = Arc::new(RecordingProcess::default());
        let adapter = git_adapter(directory.path().join("managed"), process.clone());
        let mut effect = git_effect();
        let installed = adapter
            .install(&effect, &CancellationToken::new())
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "git install failed: {error:?}; requests={:?}",
                    process.requests.lock().unwrap()
                )
            });
        assert!(matches!(
            installed.supply_chain_evidence,
            Some(SupplyChainEvidence::GitDev { .. })
        ));
        let requests = process.requests.lock().unwrap();
        assert!(requests.iter().all(|request| {
            request.argv.windows(2).any(|pair| {
                pair[0] == "-c"
                    && pair[1].starts_with("core.hooksPath=")
                    && pair[1].ends_with("hooks-disabled")
            }) && request
                .argv
                .windows(2)
                .any(|pair| pair == ["-c", "credential.helper="])
                && request
                    .argv
                    .windows(2)
                    .any(|pair| pair == ["-c", "protocol.file.allow=never"])
        }));
        let fetch = requests
            .iter()
            .find(|request| request.argv.iter().any(|argument| argument == "fetch"))
            .unwrap();
        assert!(fetch
            .argv
            .iter()
            .any(|argument| argument == "http.curloptResolve=git.example:443:93.184.216.34"));
        assert!(fetch
            .argv
            .windows(2)
            .any(|pair| pair == ["--no-recurse-submodules", "--depth=1"]));
        assert!(fetch.environment.keys().all(|key| {
            matches!(
                key.as_str(),
                "GIT_CONFIG_NOSYSTEM"
                    | "GIT_CONFIG_GLOBAL"
                    | "GIT_TERMINAL_PROMPT"
                    | "GCM_INTERACTIVE"
                    | "HOME"
                    | "GIT_LFS_SKIP_SMUDGE"
            )
        }));
        let listing = requests
            .iter()
            .find(|request| request.argv.iter().any(|argument| argument == "ls-tree"))
            .unwrap();
        assert!(listing
            .argv
            .windows(4)
            .any(|arguments| { arguments == ["ls-tree", "-r", "-l", "-z"] }));
        drop(requests);
        effect.expected_tree_digest = Some(installed.materialized_tree_digest.clone());
        let evidence_path = installed
            .installation_root
            .join(".goose-external-evidence.json");
        let original = tokio::fs::read(&evidence_path).await.unwrap();
        for (field, value) in [
            ("commit", serde_json::json!("f".repeat(40))),
            (
                "materialized_tree_digest",
                serde_json::json!("0".repeat(64)),
            ),
        ] {
            let mut evidence: serde_json::Value = serde_json::from_slice(&original).unwrap();
            evidence[field] = value;
            tokio::fs::write(&evidence_path, serde_json::to_vec(&evidence).unwrap())
                .await
                .unwrap();
            assert_eq!(
                adapter
                    .inspect_installed(&effect, &CancellationToken::new())
                    .await
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::IntegrityError,
                "{field}"
            );
        }
        tokio::fs::write(evidence_path, original).await.unwrap();
    }

    #[tokio::test]
    async fn git_fetch_rejects_unsafe_nested_tree_before_archive() {
        let directory = tempfile::tempdir().unwrap();
        let process = Arc::new(RecordingProcess::default());
        process.unsafe_git_tree.store(true, Ordering::SeqCst);
        let adapter = git_adapter(directory.path().join("managed"), process.clone());
        assert_eq!(
            adapter
                .install(&git_effect(), &CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::UnsafeRepositoryTree
        );
        assert!(!process
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.argv.iter().any(|argument| argument == "archive")));
    }

    #[tokio::test]
    async fn materialized_tree_authority_ignores_only_verified_task_metadata() {
        let root = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(root.path().join("payload"))
            .await
            .unwrap();
        tokio::fs::write(root.path().join("payload/server"), b"content")
            .await
            .unwrap();
        for name in [
            ".goose-runtime-descriptor.json",
            ".goose-artifact-evidence.json",
            ".goose-external-evidence.json",
            ".goose-installation-owner.json",
        ] {
            tokio::fs::write(root.path().join(name), b"task-one")
                .await
                .unwrap();
        }
        let initial = compute_materialized_tree_digest(root.path()).await.unwrap();
        for name in [
            ".goose-runtime-descriptor.json",
            ".goose-artifact-evidence.json",
            ".goose-external-evidence.json",
            ".goose-installation-owner.json",
        ] {
            tokio::fs::write(root.path().join(name), b"task-two")
                .await
                .unwrap();
        }
        assert_eq!(
            compute_materialized_tree_digest(root.path()).await.unwrap(),
            initial
        );
        tokio::fs::write(root.path().join("payload/server"), b"tampered")
            .await
            .unwrap();
        assert_ne!(
            compute_materialized_tree_digest(root.path()).await.unwrap(),
            initial
        );
    }

    #[test]
    fn git_network_pin_rejects_rebinding_to_private_addresses_and_emits_curl_pins() {
        let public = "93.184.216.34:443".parse().unwrap();
        let pin = GitNetworkPin::new("git.example", 443, &[public]).unwrap();
        assert_eq!(pin.config_values(), vec!["git.example:443:93.184.216.34"]);
        for rebound in ["127.0.0.1:443", "10.0.0.1:443", "169.254.1.1:443"] {
            assert_eq!(
                GitNetworkPin::new("git.example", 443, &[rebound.parse().unwrap()])
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::GitOriginDenied
            );
        }
    }

    #[test]
    fn recursive_git_tree_listing_accepts_nested_blobs_and_rejects_links_and_escape() {
        let limits = ArchiveLimits::default();
        let nested = concat!(
            "100644 blob 0123456789012345678901234567890123456789 4\troot.txt\0",
            "100755 blob 1123456789012345678901234567890123456789 8\tdir/server\0"
        );
        validate_git_tree_listing(nested.as_bytes(), limits).unwrap();
        for unsafe_listing in [
            "120000 blob 0123456789012345678901234567890123456789 4\tdir/link\0",
            "160000 commit 0123456789012345678901234567890123456789 -\tdir/submodule\0",
            "100644 blob 0123456789012345678901234567890123456789 4\tdir/../escape\0",
            "100644 blob 0123456789012345678901234567890123456789 4\tA\0100644 blob 1123456789012345678901234567890123456789 4\ta\0",
        ] {
            assert_eq!(
                validate_git_tree_listing(unsafe_listing.as_bytes(), limits)
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::UnsafeRepositoryTree
            );
        }
    }

    #[test]
    fn storage_segments_reject_windows_normalization_and_short_name_aliases() {
        for segment in [
            "",
            ".",
            "..",
            "foo.",
            "foo ",
            "MANAGE~1",
            "a/b",
            "a\\b",
            "a:b",
            "NUL",
            "CON.txt",
            "PRN.log",
            "COM1",
            "COM1.log",
            "LPT9.data",
        ] {
            assert!(validate_storage_segment(segment).is_err(), "{segment:?}");
        }
        for segment in ["managed_000001", "1.0.0", "release-2026_07"] {
            assert!(validate_storage_segment(segment).is_ok(), "{segment:?}");
        }
    }
}
