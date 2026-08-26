use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::MetadataExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::Digest as _;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF,
    ERROR_LOCK_VIOLATION, ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FindClose, FindFirstFileW, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
    GetDiskFreeSpaceExW, GetDriveTypeW, GetFileInformationByHandle, GetFinalPathNameByHandleW,
    GetVolumeInformationByHandleW, GetVolumeNameForVolumeMountPointW, BY_HANDLE_FILE_INFORMATION,
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SYSTEM,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, WIN32_FIND_DATAW,
    WIN32_FIND_STREAM_DATA,
};

use super::super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::preflight::{
    ForbiddenFamilyArtifacts, ObservedRootSurface, OptionalFamilyArtifacts,
    PlatformStorageFamilyObservation, PlatformStoragePreflightClassification, PreflightSession,
    ReadOnlyStoragePreflightFacts, ReadOnlyStoragePreflightObserver, RequiredFamilyArtifacts,
};
use super::windows_managed_writer::ManagedRootCapability;
use super::{
    DSelectorRecord, LiveAncestorIdentity, LiveObservationEpoch, LiveRootIdentity,
    LiveRootSurfaceEvidence, LiveVolumeIdentity, PlatformLiveHandlePreflight, PlatformLocator,
    PlatformRootState, PlatformStorageContext, RuntimeStorageWriterIssuer,
    RuntimeStorageWriterWitness,
};

const WINDOWS_DRIVE_FIXED: u32 = 3;
const MAX_DIRECT_CHILDREN: usize = 128;
const DIRECTORY_TRAVERSE_ACCESS: u32 = 0x0000_0020;

const MAIN_DB_NAME: &str = "platform.db";
const PROVIDER_BINDING_NAME: &str = "platform.provider-binding";
const KEYRING_REFERENCE_NAME: &str = "platform.keyring-reference";
const ANCHOR_NAME: &str = "platform.anchor";
const LOCK_SIDECAR_NAME: &str = "platform.lock";
const INTERRUPTED_BOOTSTRAP_NAME: &str = "platform.bootstrap-interrupted";
const SELECTOR_ARTIFACT_NAME: &str = "platform.selector";
const CACHE_ARTIFACT_NAME: &str = "platform.cache";
const INSTALLATIONS_ARTIFACT_NAME: &str = "platform.installations";
const STAGING_ARTIFACT_NAME: &str = "platform.staging";
const QUARANTINE_ARTIFACT_NAME: &str = "platform.quarantine";
const TEMP_ARTIFACT_NAME: &str = "platform.temp";
const LEGACY_CACHE_DIRECTORY_NAME: &str = "cache";
const LEGACY_INSTALLATIONS_DIRECTORY_NAME: &str = "installations";
const UNCOMMITTED_STAGING_DIRECTORY_NAME: &str = "lumina-staging";
const UNCOMMITTED_STAGING_SESSION_MARKER_NAME: &str = "lumina-staging-session";
const MAX_MANAGED_LAYOUT_ENTRIES: usize = 10_000;

fn is_uncommitted_layout_artifact_name(name: &str) -> bool {
    name == UNCOMMITTED_STAGING_SESSION_MARKER_NAME
        || name.starts_with(UNCOMMITTED_STAGING_DIRECTORY_NAME)
        || matches!(name, "payload" | "tmp" | "session")
        || name.starts_with("lumina-payload")
        || name.starts_with("lumina-tmp")
        || name.starts_with("lumina-session")
}

fn integrity_error(message: &'static str) -> McpPlatformError {
    McpPlatformError::new(McpPlatformErrorCode::IntegrityError, message)
}

fn integrity_unavailable(message: &'static str) -> McpPlatformError {
    McpPlatformError::new(McpPlatformErrorCode::IntegrityUnavailable, message)
}

#[derive(Clone, PartialEq, Eq)]
struct LexicalSelectorPath {
    raw: String,
}

impl LexicalSelectorPath {
    fn parse(path: &Path) -> McpPlatformResult<Self> {
        let raw = path.to_str().ok_or_else(|| {
            integrity_unavailable("windows read-only preflight selector path is unsupported")
        })?;

        if raw.starts_with(r"\\") || raw.starts_with(r"//") {
            return Err(integrity_unavailable(
                "windows read-only preflight selector path is unsupported",
            ));
        }
        if raw.starts_with("\\\\?\\") || raw.starts_with("\\\\.\\") {
            return Err(integrity_unavailable(
                "windows read-only preflight selector path is unsupported",
            ));
        }
        if raw.len() < 3 {
            return Err(integrity_unavailable(
                "windows read-only preflight selector path is unsupported",
            ));
        }

        let bytes = raw.as_bytes();
        if !bytes[0].eq_ignore_ascii_case(&b'd') || bytes[1] != b':' || bytes[2] != b'\\' {
            return Err(integrity_unavailable(
                "windows read-only preflight selector path is unsupported",
            ));
        }

        if raw[2..].contains('/') || raw[2..].contains(':') {
            return Err(integrity_unavailable(
                "windows read-only preflight selector path is unsupported",
            ));
        }

        for component in raw.split('\\').skip(1) {
            if component.is_empty() || component == "." || component == ".." {
                return Err(integrity_unavailable(
                    "windows read-only preflight selector path is unsupported",
                ));
            }
            if component.ends_with('.') || component.ends_with(' ') {
                return Err(integrity_unavailable(
                    "windows read-only preflight selector path is unsupported",
                ));
            }
        }

        Ok(Self {
            raw: raw.to_string(),
        })
    }

    fn raw(&self) -> &str {
        &self.raw
    }

    fn as_path(&self) -> &Path {
        Path::new(&self.raw)
    }
}

impl fmt::Debug for LexicalSelectorPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LexicalSelectorPath([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct FileIdentity {
    volume_serial: u32,
    file_index_high: u32,
    file_index_low: u32,
}

impl FileIdentity {
    pub(super) fn from_file(file: &File) -> McpPlatformResult<Self> {
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        let handle = file.as_raw_handle() as HANDLE;
        let result = unsafe { GetFileInformationByHandle(handle, &mut information) };
        if result == 0 {
            return Err(integrity_unavailable(
                "windows read-only preflight evidence is unavailable",
            ));
        }
        Ok(Self {
            volume_serial: information.dwVolumeSerialNumber,
            file_index_high: information.nFileIndexHigh,
            file_index_low: information.nFileIndexLow,
        })
    }

    pub(super) fn link_count(file: &File) -> McpPlatformResult<u32> {
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        let handle = file.as_raw_handle() as HANDLE;
        let result = unsafe { GetFileInformationByHandle(handle, &mut information) };
        if result == 0 {
            return Err(integrity_unavailable(
                "windows read-only preflight evidence is unavailable",
            ));
        }
        Ok(information.nNumberOfLinks)
    }

    fn volume_identity(&self) -> LiveVolumeIdentity {
        LiveVolumeIdentity::new(format!("{:08x}", self.volume_serial))
            .expect("volume serial is always non-empty")
    }

    pub(super) fn durable_key(&self) -> String {
        format!(
            "{:08x}:{:08x}:{:08x}",
            self.volume_serial, self.file_index_high, self.file_index_low
        )
    }

    pub(super) fn root_identity(&self) -> LiveRootIdentity {
        LiveRootIdentity::new(self.durable_key()).expect("file identity is always non-empty")
    }

    fn ancestor_identity(&self) -> LiveAncestorIdentity {
        LiveAncestorIdentity::new(format!(
            "{:08x}:{:08x}:{:08x}",
            self.volume_serial, self.file_index_high, self.file_index_low
        ))
        .expect("file identity is always non-empty")
    }
}

impl fmt::Debug for FileIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FileIdentity([REDACTED])")
    }
}

#[derive(Clone, Copy)]
struct PathTagEvidence {
    attributes: u32,
    reparse_tag: u32,
}

pub(super) struct RootBinding {
    lexical: LexicalSelectorPath,
    expected_final_path: String,
    root_file: File,
    root_identity: FileIdentity,
    expected_volume_identity: FileIdentity,
    volume_identity: LiveVolumeIdentity,
    volume_identity_from_mount_point: bool,
    selector_revision: u64,
    observation_epoch: LiveObservationEpoch,
    selector_exact_match: bool,
    root_attributes: u32,
}

impl RootBinding {
    fn root_surface_evidence(&self) -> LiveRootSurfaceEvidence {
        LiveRootSurfaceEvidence::new(format!(
            "{}\0{:08x}",
            self.expected_final_path, self.root_attributes
        ))
        .expect("verified root surface evidence is non-empty")
    }

    fn lease_context(&self, locator: PlatformLocator) -> PlatformStorageContext {
        PlatformStorageContext::for_present_root_with_surface(
            locator,
            self.volume_identity.clone(),
            self.root_identity.root_identity(),
            self.root_surface_evidence(),
            self.observation_epoch,
        )
    }

    pub(super) fn managed_installation_anchor_binding(&self) -> String {
        // This is intentionally not a filesystem path: it binds the keyring
        // account to the verified volume, root file identity, and canonical
        // root surface observed by the Windows preflight.
        let mut digest = sha2::Sha256::new();
        use sha2::Digest as _;
        digest.update(b"lumina-managed-installations-anchor-v1\0");
        digest.update(self.volume_identity.opaque.as_bytes());
        digest.update([0]);
        digest.update(self.root_identity.root_identity().opaque.as_bytes());
        digest.update([0]);
        digest.update(self.expected_final_path.as_bytes());
        sha256_hex(&digest.finalize())
    }

    pub(super) fn managed_installation_lineage_binding(&self) -> String {
        let mut digest = sha2::Sha256::new();
        use sha2::Digest as _;
        digest.update(b"lumina-managed-installations-lineage-v1\0");
        digest.update(self.volume_identity.opaque.as_bytes());
        digest.update([0]);
        digest.update(self.expected_final_path.as_bytes());
        sha256_hex(&digest.finalize())
    }

    pub(super) fn managed_installation_root_identity(&self) -> String {
        self.root_identity.root_identity().opaque.clone()
    }

    #[cfg(test)]
    pub(super) fn set_volume_identity_for_testing(&mut self, identity: &str) {
        self.volume_identity = LiveVolumeIdentity::new(identity).expect("test volume identity");
        self.volume_identity_from_mount_point = false;
    }
}

struct ManagedStorageCapacityWitness {
    lexical: LexicalSelectorPath,
    root_file: File,
    binding: CapacityBinding,
}

#[derive(Clone, PartialEq, Eq)]
struct CapacityBinding {
    root_identity: FileIdentity,
    expected_volume_identity: FileIdentity,
    expected_final_path: String,
    root_attributes: u32,
}

struct WindowsRuntimeStorageWriterWitness {
    capability: ManagedRootCapability,
    locator: PlatformLocator,
    context: PlatformStorageContext,
}

impl WindowsRuntimeStorageWriterWitness {
    fn new(
        binding: RootBinding,
        locator: PlatformLocator,
        context: PlatformStorageContext,
    ) -> McpPlatformResult<Self> {
        let capability = ManagedRootCapability::from_verified_preflight(binding)?;
        let witness = Self {
            capability,
            locator,
            context,
        };
        witness.revalidate_live_binding()?;
        Ok(witness)
    }

    fn revalidate_live_binding(&self) -> McpPlatformResult<()> {
        self.capability
            .revalidate_for_context(&self.locator, &self.context)
    }

    fn reject_uncommitted(&self, error: McpPlatformError) -> McpPlatformResult<()> {
        if self.capability.discard_uncommitted().is_err() {
            return Err(integrity_unavailable(
                "windows runtime writer could not isolate an uncommitted object",
            ));
        }
        Err(error)
    }
}

impl RuntimeStorageWriterWitness for WindowsRuntimeStorageWriterWitness {
    fn create_staging(&self) -> McpPlatformResult<()> {
        if let Err(error) = self.revalidate_live_binding() {
            return self.reject_uncommitted(error);
        }
        if let Err(error) = self.capability.create_staging() {
            return self.reject_uncommitted(error);
        }
        if let Err(error) = self.revalidate_live_binding() {
            return self.reject_uncommitted(error);
        }
        Ok(())
    }

    fn write_verified_payload(&self, bytes: &[u8], expected_sha256: &str) -> McpPlatformResult<()> {
        if let Err(error) = self.revalidate_live_binding() {
            return self.reject_uncommitted(error);
        }
        if let Err(error) = self
            .capability
            .write_verified_payload(bytes, expected_sha256)
        {
            return self.reject_uncommitted(error);
        }
        if let Err(error) = self.revalidate_live_binding() {
            return self.reject_uncommitted(error);
        }
        Ok(())
    }

    fn verify_regular_payload(&self) -> McpPlatformResult<()> {
        if let Err(error) = self.revalidate_live_binding() {
            return self.reject_uncommitted(error);
        }
        if let Err(error) = self.capability.verify_regular_payload() {
            return self.reject_uncommitted(error);
        }
        if let Err(error) = self.revalidate_live_binding() {
            return self.reject_uncommitted(error);
        }
        Ok(())
    }

    fn atomic_promote(&self) -> McpPlatformResult<()> {
        if let Err(error) = self.revalidate_live_binding() {
            return self.reject_uncommitted(error);
        }
        self.reject_uncommitted(McpPlatformError::new(
            McpPlatformErrorCode::RuntimeControlUnavailable,
            "runtime writer operations require handle-relative I/O not implemented in this phase",
        ))
    }

    fn remove_lumina_owned_contents(&self) -> McpPlatformResult<()> {
        self.revalidate_live_binding()?;
        Err(McpPlatformError::new(
            McpPlatformErrorCode::RuntimeControlUnavailable,
            "runtime writer operations require handle-relative I/O not implemented in this phase",
        ))
    }

    fn create_directory_staging(
        &self,
        manifest: &super::DirectoryPayloadManifest,
    ) -> McpPlatformResult<()> {
        if let Err(error) = self.revalidate_live_binding() {
            return self.reject_uncommitted(error);
        }
        if let Err(error) = self.capability.create_directory_staging(manifest) {
            return self.reject_uncommitted(error);
        }
        self.revalidate_live_binding()
            .or_else(|error| self.reject_uncommitted(error))
    }

    fn materialize_verified_directory(
        &self,
        payloads: &[super::DirectoryPayload<'_>],
    ) -> McpPlatformResult<()> {
        if let Err(error) = self.revalidate_live_binding() {
            return self.reject_uncommitted(error);
        }
        if let Err(error) = self.capability.materialize_verified_directory(payloads) {
            return self.reject_uncommitted(error);
        }
        self.revalidate_live_binding()
            .or_else(|error| self.reject_uncommitted(error))
    }

    fn seal_staged_directory(&self) -> McpPlatformResult<()> {
        self.revalidate_live_binding()?;
        self.capability.seal_staged_directory()?;
        self.revalidate_live_binding()
    }

    fn promote_staged_directory(
        &self,
        committed_leaf: super::RuntimeStorageCommittedLeaf,
    ) -> McpPlatformResult<()> {
        self.revalidate_live_binding()?;
        self.capability.promote_staged_directory(committed_leaf)
    }

    fn promote_staged_managed_installation(&self) -> McpPlatformResult<()> {
        self.revalidate_live_binding()?;
        self.capability.promote_staged_managed_installation()
    }

    fn activate_verified_managed_installation(
        &self,
        managed_mcp_id: &str,
        version: &str,
        descriptor: &[u8],
        descriptor_sha256: &str,
    ) -> McpPlatformResult<()> {
        self.revalidate_live_binding()?;
        self.capability.activate_verified_managed_installation(
            managed_mcp_id,
            version,
            descriptor,
            descriptor_sha256,
        )
    }

    fn clear_managed_installation_activation(&self, managed_mcp_id: &str) -> McpPlatformResult<()> {
        self.revalidate_live_binding()?;
        self.capability
            .clear_managed_installation_activation(managed_mcp_id)
    }

    fn managed_installation_version_is_verified(
        &self,
        managed_mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<bool> {
        self.revalidate_live_binding()?;
        self.capability
            .managed_installation_version_is_verified(managed_mcp_id, version)
    }

    fn read_verified_managed_installation_activation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<Vec<u8>>> {
        self.revalidate_live_binding()?;
        self.capability
            .read_verified_managed_installation_activation(managed_mcp_id)
    }
}

struct AbsentBinding {
    lexical: LexicalSelectorPath,
    expected_ancestor_path: String,
    ancestor_file: File,
    ancestor_identity: FileIdentity,
    volume_identity: LiveVolumeIdentity,
    selector_revision: u64,
    observation_epoch: LiveObservationEpoch,
}

enum Binding {
    Present(RootBinding),
    Absent(AbsentBinding),
}

struct WindowsStoragePreflightAdapter;

impl WindowsStoragePreflightAdapter {
    const fn new() -> Self {
        Self
    }

    fn open_binding(locator: &PlatformLocator) -> McpPlatformResult<Binding> {
        let lexical = LexicalSelectorPath::parse(locator.root_path())?;
        let expected_volume_identity = open_expected_d_volume()?;

        match open_directory_handle(lexical.as_path()) {
            Ok(root_file) => {
                ensure_supported_filesystem(&root_file)?;
                let root_identity = FileIdentity::from_file(&root_file)?;
                ensure_same_volume(&root_identity, &expected_volume_identity)?;
                let volume_identity = mounted_d_volume_identity(root_identity.volume_serial)?;
                let expected_final_path = final_dos_path(&root_file)?;
                let root_attributes = file_attributes(&root_file)?;
                let selector_exact_match =
                    windows_normal_path_case_match(lexical.raw(), &expected_final_path);
                let observation_epoch = next_observation_epoch()?;

                Ok(Binding::Present(RootBinding {
                    lexical,
                    expected_final_path,
                    root_file,
                    root_identity,
                    expected_volume_identity,
                    volume_identity,
                    volume_identity_from_mount_point: true,
                    selector_revision: locator.selector().selector_revision(),
                    observation_epoch,
                    selector_exact_match,
                    root_attributes,
                }))
            }
            Err(error) if matches!(error.code(), McpPlatformErrorCode::NotFound) => {
                let (ancestor_path, ancestor_file) = nearest_existing_ancestor(&lexical)?;
                ensure_supported_filesystem(&ancestor_file)?;
                let expected_ancestor_path = final_dos_path(&ancestor_file)?;
                if ancestor_path != PathBuf::from(&expected_ancestor_path) {
                    return Err(integrity_unavailable(
                        "windows read-only preflight selector path is unsupported",
                    ));
                }
                let ancestor_identity = FileIdentity::from_file(&ancestor_file)?;
                ensure_same_volume(&ancestor_identity, &expected_volume_identity)?;
                let volume_identity = mounted_d_volume_identity(ancestor_identity.volume_serial)?;
                let observation_epoch = next_observation_epoch()?;

                Ok(Binding::Absent(AbsentBinding {
                    lexical,
                    expected_ancestor_path,
                    ancestor_file,
                    ancestor_identity,
                    volume_identity,
                    selector_revision: locator.selector().selector_revision(),
                    observation_epoch,
                }))
            }
            Err(error) => Err(error),
        }
    }
}

impl Default for WindowsStoragePreflightAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) fn acquire_runtime_writer_witness(
    _issuer: &RuntimeStorageWriterIssuer,
    locator: &PlatformLocator,
    context: &PlatformStorageContext,
) -> McpPlatformResult<Arc<dyn RuntimeStorageWriterWitness>> {
    if !context.matches_locator(locator) || context.root_state() != PlatformRootState::Present {
        return Err(integrity_error(
            "windows runtime writer lease context drifted",
        ));
    }
    recover_committed_cleanup_pending(locator)?;
    validate_managed_storage_locator_with_preflight(
        locator,
        &WindowsStoragePreflightAdapter::new(),
    )?;
    let Binding::Present(binding) = WindowsStoragePreflightAdapter::open_binding(locator)? else {
        return Err(integrity_error(
            "windows runtime writer lease root state drifted",
        ));
    };
    if !runtime_writer_observation_matches(&binding, locator, context) {
        return Err(integrity_error(
            "windows runtime writer lease context drifted",
        ));
    }
    let lease_context = binding.lease_context(locator.clone());
    let witness = WindowsRuntimeStorageWriterWitness::new(binding, locator.clone(), lease_context)?;
    Ok(Arc::new(witness))
}

pub(super) fn validate_managed_storage_root(root: &Path) -> McpPlatformResult<()> {
    let selector =
        DSelectorRecord::new("production-managed-storage-root", 1, 1, root).ok_or_else(|| {
            integrity_unavailable("windows read-only preflight selector path is unsupported")
        })?;
    let locator = PlatformLocator::from_selector(selector);
    recover_committed_cleanup_pending(&locator)?;
    validate_managed_storage_locator_with_preflight(
        &locator,
        &WindowsStoragePreflightAdapter::new(),
    )
}

trait ManagedInstallationPromotionWriter {
    fn create_directory_staging(
        &mut self,
        manifest: &super::DirectoryPayloadManifest,
    ) -> McpPlatformResult<()>;
    fn materialize_verified_directory(
        &mut self,
        payloads: &[super::DirectoryPayload<'_>],
    ) -> McpPlatformResult<()>;
    fn seal_staged_directory(&mut self) -> McpPlatformResult<()>;
    fn promote_direct_final_managed_installation(&mut self) -> McpPlatformResult<()>;
}

impl ManagedInstallationPromotionWriter for super::RuntimeStorageWriterLease {
    fn create_directory_staging(
        &mut self,
        manifest: &super::DirectoryPayloadManifest,
    ) -> McpPlatformResult<()> {
        self.create_directory_staging(manifest)
    }

    fn materialize_verified_directory(
        &mut self,
        payloads: &[super::DirectoryPayload<'_>],
    ) -> McpPlatformResult<()> {
        self.materialize_verified_directory(payloads)
    }

    fn seal_staged_directory(&mut self) -> McpPlatformResult<()> {
        self.seal_staged_directory()
    }

    fn promote_direct_final_managed_installation(&mut self) -> McpPlatformResult<()> {
        self.promote_staged_managed_installation()
    }
}

fn run_managed_installation_promotion_sequence(
    writer: &mut impl ManagedInstallationPromotionWriter,
    manifest: &super::DirectoryPayloadManifest,
    payloads: &[super::DirectoryPayload<'_>],
) -> McpPlatformResult<()> {
    writer.create_directory_staging(manifest)?;
    writer.materialize_verified_directory(payloads)?;
    writer.seal_staged_directory()?;
    writer.promote_direct_final_managed_installation()
}

/// Obtains a fresh, live writer lease only after the same D:-root preflight
/// used by production reads has accepted the root.  The lease never exposes a
/// path, so callers cannot turn this into a general filesystem writer.
pub(super) fn promote_verified_managed_installations(
    root: &Path,
    manifest: &super::DirectoryPayloadManifest,
    payloads: &[super::DirectoryPayload<'_>],
) -> McpPlatformResult<()> {
    let selector = DSelectorRecord::new("production-managed-storage-root", 1, 1, root)
        .ok_or_else(|| integrity_unavailable("windows managed storage is unavailable"))?;
    let locator = PlatformLocator::from_selector(selector);
    let adapter = WindowsStoragePreflightAdapter::new();
    recover_committed_cleanup_pending(&locator)?;
    validate_managed_storage_locator_with_preflight(&locator, &adapter)?;
    let context = adapter.observe_present_root(&locator)?;
    let mut lease = super::acquire_live_windows_runtime_writer_lease(&locator, &context)?;
    run_managed_installation_promotion_sequence(&mut lease, manifest, payloads)
}

/// Activates only a version that remains present and whose sealed descriptor
/// still matches the descriptor staged by this lease. The pointer replacement
/// is performed by the same root-bound handle capability as installation.
pub(super) fn activate_verified_managed_installation(
    root: &Path,
    managed_mcp_id: &str,
    version: &str,
    descriptor: &[u8],
    descriptor_sha256: &str,
) -> McpPlatformResult<()> {
    let selector = DSelectorRecord::new("production-managed-storage-root", 1, 1, root)
        .ok_or_else(|| integrity_unavailable("windows managed storage is unavailable"))?;
    let locator = PlatformLocator::from_selector(selector);
    let adapter = WindowsStoragePreflightAdapter::new();
    recover_committed_cleanup_pending(&locator)?;
    validate_managed_storage_locator_with_preflight(&locator, &adapter)?;
    let context = adapter.observe_present_root(&locator)?;
    let mut lease = super::acquire_live_windows_runtime_writer_lease(&locator, &context)?;
    lease.create_staging()?;
    lease.write_verified_bytes(
        super::RuntimeStorageSlot::Payload,
        descriptor,
        descriptor_sha256,
    )?;
    lease.verify_regular_file(super::RuntimeStorageSlot::Payload)?;
    lease.activate_verified_managed_installation(
        managed_mcp_id,
        version,
        descriptor,
        descriptor_sha256,
    )
}

pub(super) fn clear_managed_installation_activation(
    root: &Path,
    managed_mcp_id: &str,
) -> McpPlatformResult<()> {
    let selector = DSelectorRecord::new("production-managed-storage-root", 1, 1, root)
        .ok_or_else(|| integrity_unavailable("windows managed storage is unavailable"))?;
    let locator = PlatformLocator::from_selector(selector);
    let adapter = WindowsStoragePreflightAdapter::new();
    recover_committed_cleanup_pending(&locator)?;
    validate_managed_storage_locator_with_preflight(&locator, &adapter)?;
    let context = adapter.observe_present_root(&locator)?;
    let mut lease = super::acquire_live_windows_runtime_writer_lease(&locator, &context)?;
    lease.clear_managed_installation_activation(managed_mcp_id)
}

/// Opens the production root only after the read-only preflight has bound its
/// identity. The returned handle is used exclusively by the relative read
/// primitives; no child path is reopened by Win32 pathname resolution.
fn acquire_managed_read_root(root: &Path) -> McpPlatformResult<File> {
    acquire_managed_read_root_after_preflight(root, || {})
}

fn acquire_managed_read_root_after_preflight(
    root: &Path,
    after_preflight: impl FnOnce(),
) -> McpPlatformResult<File> {
    let selector =
        DSelectorRecord::new("production-managed-storage-root", 1, 1, root).ok_or_else(|| {
            integrity_unavailable("windows read-only preflight selector path is unsupported")
        })?;
    let locator = PlatformLocator::from_selector(selector);
    recover_committed_cleanup_pending(&locator)?;
    let binding = acquire_preflighted_managed_read_binding(&locator)?;
    after_preflight();
    acquire_read_root(&binding)
}

/// Runs the layout preflight over one root binding and retains that binding
/// until the relative reader owns its clone. The root path is only reopened to
/// compare identity and fail closed; it is never rebound as the read capability.
fn acquire_preflighted_managed_read_binding(
    locator: &PlatformLocator,
) -> McpPlatformResult<RootBinding> {
    let Binding::Present(binding) = WindowsStoragePreflightAdapter::open_binding(locator)? else {
        return Err(integrity_error(
            "windows managed storage read root state drifted",
        ));
    };
    let context = binding.lease_context(locator.clone());
    let observer = Arc::new(BoundManagedReadObserver { binding });
    let session = PreflightSession::new(locator, &context, observer.clone())?;
    let request = session.issue_read_only_request()?;
    let classification = request.consume()?;
    drop(request);
    drop(session);
    validate_managed_read_classification(classification)?;

    let observer = Arc::try_unwrap(observer).map_err(|_| {
        integrity_unavailable("windows managed storage read capability is unavailable")
    })?;
    Ok(observer.binding)
}

fn validate_managed_read_classification(
    classification: PlatformStoragePreflightClassification,
) -> McpPlatformResult<()> {
    match classification {
        PlatformStoragePreflightClassification::CommittedFamily => Ok(()),
        PlatformStoragePreflightClassification::RootAbsent
        | PlatformStoragePreflightClassification::RootPhysicallyEmpty
        | PlatformStoragePreflightClassification::RootNonempty
        | PlatformStoragePreflightClassification::RootInaccessible
        | PlatformStoragePreflightClassification::RootReparse
        | PlatformStoragePreflightClassification::RootAdsPresent
        | PlatformStoragePreflightClassification::RootHardlinkAlias
        | PlatformStoragePreflightClassification::RootHiddenOrSystemPresent
        | PlatformStoragePreflightClassification::RootIdentityMismatch
        | PlatformStoragePreflightClassification::PartialFamily
        | PlatformStoragePreflightClassification::ResidualPresent
        | PlatformStoragePreflightClassification::ReleasedSidecar
        | PlatformStoragePreflightClassification::Locked => {
            Err(integrity_unavailable("storage_recovery_required"))
        }
    }
}

fn acquire_read_root(binding: &RootBinding) -> McpPlatformResult<File> {
    revalidate_root_binding(binding)?;
    binding.root_file.try_clone().map_err(|_| {
        integrity_unavailable("windows managed storage read capability is unavailable")
    })
}

struct BoundManagedReadObserver {
    binding: RootBinding,
}

impl ReadOnlyStoragePreflightObserver for BoundManagedReadObserver {
    fn observe_read_only_preflight(
        &self,
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
    ) -> McpPlatformResult<ReadOnlyStoragePreflightFacts> {
        observe_present_root(locator, context, &self.binding)
    }
}

pub(super) fn read_anchored_managed_installation_metadata(
    root: &Path,
    managed_mcp_id: &str,
    version: &str,
    leaf: &str,
    maximum: u64,
) -> McpPlatformResult<Option<Vec<u8>>> {
    if !managed_storage_verified_version_exists(root, managed_mcp_id, version)? {
        return Ok(None);
    }
    let bytes = super::windows_managed_writer::read_regular_relative(
        acquire_managed_read_root(root)?,
        &[
            "platform.installations",
            managed_mcp_id,
            "versions",
            version,
            leaf,
        ],
        maximum,
    )?;
    if !managed_storage_verified_version_exists(root, managed_mcp_id, version)? {
        return Err(integrity_unavailable(
            "windows managed installation anchor changed during metadata read",
        ));
    }
    Ok(bytes)
}

pub(super) fn managed_storage_verified_version_exists(
    root: &Path,
    managed_mcp_id: &str,
    version: &str,
) -> McpPlatformResult<bool> {
    with_live_managed_writer_lease(root, |lease| {
        lease.managed_installation_version_is_verified(managed_mcp_id, version)
    })
}

pub(super) fn read_verified_managed_installation_activation(
    root: &Path,
    managed_mcp_id: &str,
) -> McpPlatformResult<Option<Vec<u8>>> {
    with_live_managed_writer_lease(root, |lease| {
        lease.read_verified_managed_installation_activation(managed_mcp_id)
    })
}

fn with_live_managed_writer_lease<T>(
    root: &Path,
    operation: impl FnOnce(&mut super::RuntimeStorageWriterLease) -> McpPlatformResult<T>,
) -> McpPlatformResult<T> {
    let selector = DSelectorRecord::new("production-managed-storage-root", 1, 1, root)
        .ok_or_else(|| integrity_unavailable("windows managed storage is unavailable"))?;
    let locator = PlatformLocator::from_selector(selector);
    let adapter = WindowsStoragePreflightAdapter::new();
    recover_committed_cleanup_pending(&locator)?;
    validate_managed_storage_locator_with_preflight(&locator, &adapter)?;
    let context = adapter.observe_present_root(&locator)?;
    let mut lease = super::acquire_live_windows_runtime_writer_lease(&locator, &context)?;
    operation(&mut lease)
}

/// Capacity evidence is deliberately narrower than the bootstrap preflight:
/// a live managed-distribution root contains Lumina's installation directories
/// and therefore is not expected to look like an empty/bootstrap root.  This
/// still proves the lexical D: selector, fixed local volume, NTFS filesystem,
/// exact final path, and absence of a root reparse point before asking Windows
/// for free space.
pub(super) fn managed_storage_available_bytes(root: &Path) -> McpPlatformResult<u64> {
    let lexical = LexicalSelectorPath::parse(root)?;
    let expected_volume_identity = open_expected_d_volume()?;
    let root_file = open_directory_handle(lexical.as_path())?;
    ensure_supported_filesystem(&root_file)?;
    let witness = ManagedStorageCapacityWitness {
        binding: CapacityBinding {
            root_identity: FileIdentity::from_file(&root_file)?,
            root_attributes: file_attributes(&root_file)?,
            expected_final_path: final_dos_path(&root_file)?,
            expected_volume_identity,
        },
        lexical,
        root_file,
    };
    validate_capacity_witness(&witness)?;

    let mut available = 0_u64;
    let result = unsafe {
        GetDiskFreeSpaceExW(
            wide(&witness.binding.expected_final_path).as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(integrity_unavailable(
            "windows managed storage capacity is unavailable",
        ));
    }
    revalidate_capacity_witness(&witness)?;
    Ok(available)
}

fn validate_managed_storage_locator_with_preflight(
    locator: &PlatformLocator,
    preflight: &dyn PlatformLiveHandlePreflight,
) -> McpPlatformResult<()> {
    let classification = preflight
        .begin_read_only_preflight_session(locator)?
        .issue_read_only_request()?
        .consume()?;

    match classification {
        PlatformStoragePreflightClassification::RootAbsent
        | PlatformStoragePreflightClassification::RootPhysicallyEmpty
        | PlatformStoragePreflightClassification::CommittedFamily => Ok(()),
        PlatformStoragePreflightClassification::RootNonempty
        | PlatformStoragePreflightClassification::RootInaccessible
        | PlatformStoragePreflightClassification::RootReparse
        | PlatformStoragePreflightClassification::RootAdsPresent
        | PlatformStoragePreflightClassification::RootHardlinkAlias
        | PlatformStoragePreflightClassification::RootHiddenOrSystemPresent
        | PlatformStoragePreflightClassification::RootIdentityMismatch
        | PlatformStoragePreflightClassification::PartialFamily
        | PlatformStoragePreflightClassification::ResidualPresent
        | PlatformStoragePreflightClassification::ReleasedSidecar
        | PlatformStoragePreflightClassification::Locked => {
            Err(integrity_unavailable("storage_recovery_required"))
        }
    }
}

/// A normal preflight may repair only a residue authenticated as belonging to
/// a committed version. The cleanup itself stays inside the root-bound writer
/// capability and never receives a child filesystem path.
fn recover_committed_cleanup_pending(locator: &PlatformLocator) -> McpPlatformResult<()> {
    let Binding::Present(binding) = WindowsStoragePreflightAdapter::open_binding(locator)? else {
        return Ok(());
    };
    if !binding.selector_exact_match {
        return Err(integrity_error(
            "windows read-only preflight root state drifted",
        ));
    }
    ManagedRootCapability::from_verified_preflight(binding)?.recover_committed_cleanup_pending()
}

impl PlatformLiveHandlePreflight for WindowsStoragePreflightAdapter {
    fn observe_absent_root(
        &self,
        locator: &PlatformLocator,
    ) -> McpPlatformResult<PlatformStorageContext> {
        match Self::open_binding(locator)? {
            Binding::Absent(binding) => Ok(PlatformStorageContext::for_absent_root(
                locator.clone(),
                binding.volume_identity.clone(),
                binding.ancestor_identity.ancestor_identity(),
                binding.observation_epoch,
            )),
            Binding::Present(_) => Err(integrity_error(
                "windows read-only preflight root state drifted",
            )),
        }
    }

    fn observe_present_root(
        &self,
        locator: &PlatformLocator,
    ) -> McpPlatformResult<PlatformStorageContext> {
        match Self::open_binding(locator)? {
            Binding::Present(binding) => Ok(binding.lease_context(locator.clone())),
            Binding::Absent(_) => Err(integrity_error(
                "windows read-only preflight root state drifted",
            )),
        }
    }

    fn begin_read_only_preflight_session(
        &self,
        locator: &PlatformLocator,
    ) -> McpPlatformResult<PreflightSession> {
        let (context, observer): (
            PlatformStorageContext,
            Arc<dyn ReadOnlyStoragePreflightObserver>,
        ) = match Self::open_binding(locator)? {
            Binding::Present(binding) => {
                let context = binding.lease_context(locator.clone());
                (context, Arc::new(WindowsReadOnlyObserver::Present(binding)))
            }
            Binding::Absent(binding) => {
                let context = PlatformStorageContext::for_absent_root(
                    locator.clone(),
                    binding.volume_identity.clone(),
                    binding.ancestor_identity.ancestor_identity(),
                    binding.observation_epoch,
                );
                (context, Arc::new(WindowsReadOnlyObserver::Absent(binding)))
            }
        };

        PreflightSession::new(locator, &context, observer)
    }
}

enum WindowsReadOnlyObserver {
    Present(RootBinding),
    Absent(AbsentBinding),
}

impl ReadOnlyStoragePreflightObserver for WindowsReadOnlyObserver {
    fn observe_read_only_preflight(
        &self,
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
    ) -> McpPlatformResult<ReadOnlyStoragePreflightFacts> {
        match self {
            Self::Present(binding) => observe_present_root(locator, context, binding),
            Self::Absent(binding) => observe_absent_root(locator, context, binding),
        }
    }
}

fn observe_absent_root(
    locator: &PlatformLocator,
    context: &PlatformStorageContext,
    binding: &AbsentBinding,
) -> McpPlatformResult<ReadOnlyStoragePreflightFacts> {
    if context.root_state() != PlatformRootState::Absent
        || locator.selector().selector_revision() != binding.selector_revision
        || context.observation_epoch() != binding.observation_epoch
    {
        return Err(integrity_error(
            "windows read-only preflight request drifted",
        ));
    }

    let current_ancestor_identity = FileIdentity::from_file(&binding.ancestor_file)?;
    if current_ancestor_identity != binding.ancestor_identity {
        return Err(integrity_error(
            "windows read-only preflight ancestor drifted",
        ));
    }
    if !windows_normal_path_case_match(
        &final_dos_path(&binding.ancestor_file)?,
        &binding.expected_ancestor_path,
    ) {
        return Err(integrity_error(
            "windows read-only preflight ancestor drifted",
        ));
    }
    revalidate_absent_root(binding)?;

    Ok(ReadOnlyStoragePreflightFacts::new(
        ObservedRootSurface::Absent,
        PlatformStorageFamilyObservation::empty(),
    ))
}

fn observe_present_root(
    locator: &PlatformLocator,
    context: &PlatformStorageContext,
    binding: &RootBinding,
) -> McpPlatformResult<ReadOnlyStoragePreflightFacts> {
    if context.root_state() != PlatformRootState::Present
        || locator.selector().selector_revision() != binding.selector_revision
        || context.observation_epoch() != binding.observation_epoch
    {
        return Err(integrity_error(
            "windows read-only preflight request drifted",
        ));
    }

    revalidate_root_binding(binding)?;

    if !binding.selector_exact_match {
        return Ok(ReadOnlyStoragePreflightFacts::new(
            ObservedRootSurface::IdentityMismatch,
            PlatformStorageFamilyObservation::empty(),
        ));
    }

    let root_tag = find_path_tag(binding.lexical.as_path())?;
    if root_tag.attributes & FILE_ATTRIBUTE_HIDDEN != 0
        || root_tag.attributes & FILE_ATTRIBUTE_SYSTEM != 0
    {
        return Ok(ReadOnlyStoragePreflightFacts::new(
            ObservedRootSurface::HiddenOrSystemPresent,
            PlatformStorageFamilyObservation::empty(),
        ));
    }
    if root_tag.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || root_tag.reparse_tag != 0 {
        return Ok(ReadOnlyStoragePreflightFacts::new(
            ObservedRootSurface::Reparse,
            PlatformStorageFamilyObservation::empty(),
        ));
    }
    if has_named_streams(Path::new(&binding.expected_final_path))? {
        return Ok(ReadOnlyStoragePreflightFacts::new(
            ObservedRootSurface::AdsPresent,
            PlatformStorageFamilyObservation::empty(),
        ));
    }

    let mut entries = match fs::read_dir(&binding.expected_final_path) {
        Ok(entries) => entries,
        Err(_) => {
            return Ok(ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Inaccessible,
                PlatformStorageFamilyObservation::empty(),
            ));
        }
    };
    let mut direct_children = 0usize;
    let mut saw_any_content = false;
    let mut required = RequiredFamilyArtifacts::none();
    let mut optional = OptionalFamilyArtifacts::none();
    let mut forbidden = ForbiddenFamilyArtifacts::none();
    let mut interrupted_bootstrap = false;
    let mut released_sidecar = false;
    let mut live_exclusive_lock = false;

    while let Some(entry) = entries.next() {
        let entry = entry.map_err(|_| {
            integrity_unavailable("windows read-only preflight evidence is unavailable")
        })?;
        direct_children += 1;
        if direct_children > MAX_DIRECT_CHILDREN {
            return Err(integrity_unavailable(
                "windows read-only preflight evidence is unavailable",
            ));
        }

        let child_path = entry.path();
        let child_name = entry.file_name();
        let child_name = child_name.to_string_lossy().into_owned();
        saw_any_content = true;

        let metadata = fs::symlink_metadata(&child_path).map_err(|_| {
            integrity_unavailable("windows read-only preflight evidence is unavailable")
        })?;
        let attributes = metadata.file_attributes();
        if attributes & FILE_ATTRIBUTE_HIDDEN != 0 || attributes & FILE_ATTRIBUTE_SYSTEM != 0 {
            return Ok(ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::HiddenOrSystemPresent,
                PlatformStorageFamilyObservation::empty(),
            ));
        }
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            let tag = find_path_tag(&child_path)?;
            if tag.reparse_tag != 0 {
                return Ok(ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::Reparse,
                    PlatformStorageFamilyObservation::empty(),
                ));
            }
        }

        match child_name.as_str() {
            LOCK_SIDECAR_NAME => match try_open_evidence_handle(&child_path, false) {
                OpenOutcome::Opened(file) => {
                    if has_named_streams(&child_path)? {
                        return Ok(ReadOnlyStoragePreflightFacts::new(
                            ObservedRootSurface::AdsPresent,
                            PlatformStorageFamilyObservation::empty(),
                        ));
                    }
                    if FileIdentity::link_count(&file)? > 1 {
                        return Ok(ReadOnlyStoragePreflightFacts::new(
                            ObservedRootSurface::HardlinkAlias,
                            PlatformStorageFamilyObservation::empty(),
                        ));
                    }
                    released_sidecar = true;
                }
                OpenOutcome::SharingViolation => {
                    live_exclusive_lock = true;
                }
                OpenOutcome::NotFound => {
                    return Err(integrity_unavailable(
                        "windows read-only preflight evidence is unavailable",
                    ));
                }
                OpenOutcome::Unavailable => {
                    return Err(integrity_unavailable(
                        "windows read-only preflight evidence is unavailable",
                    ));
                }
            },
            _ => {
                let child_is_directory = metadata.file_type().is_dir();
                let child_file = match try_open_evidence_handle(&child_path, child_is_directory) {
                    OpenOutcome::Opened(file) => file,
                    OpenOutcome::NotFound
                    | OpenOutcome::SharingViolation
                    | OpenOutcome::Unavailable => {
                        return Err(integrity_unavailable(
                            "windows read-only preflight evidence is unavailable",
                        ));
                    }
                };
                if !windows_normal_path_case_match(
                    &final_dos_path(&child_file)?,
                    &child_path.to_string_lossy(),
                ) {
                    return Ok(ReadOnlyStoragePreflightFacts::new(
                        ObservedRootSurface::IdentityMismatch,
                        PlatformStorageFamilyObservation::empty(),
                    ));
                }
                if has_named_streams(&child_path)? {
                    return Ok(ReadOnlyStoragePreflightFacts::new(
                        ObservedRootSurface::AdsPresent,
                        PlatformStorageFamilyObservation::empty(),
                    ));
                }
                if metadata.file_type().is_file() && FileIdentity::link_count(&child_file)? > 1 {
                    return Ok(ReadOnlyStoragePreflightFacts::new(
                        ObservedRootSurface::HardlinkAlias,
                        PlatformStorageFamilyObservation::empty(),
                    ));
                }

                match child_name.as_str() {
                    MAIN_DB_NAME if metadata.file_type().is_file() => {
                        required = required.with_main_db()
                    }
                    PROVIDER_BINDING_NAME if metadata.file_type().is_file() => {
                        required = required.with_provider_binding()
                    }
                    KEYRING_REFERENCE_NAME if metadata.file_type().is_file() => {
                        required = required.with_keyring_reference()
                    }
                    ANCHOR_NAME if metadata.file_type().is_file() => {
                        required = required.with_anchor()
                    }
                    "platform.db-wal" if metadata.file_type().is_file() => {
                        optional = optional.with_sqlite_wal()
                    }
                    "platform.db-shm" if metadata.file_type().is_file() => {
                        optional = optional.with_sqlite_shm()
                    }
                    "platform.db-journal" if metadata.file_type().is_file() => {
                        optional = optional.with_sqlite_journal()
                    }
                    INTERRUPTED_BOOTSTRAP_NAME => interrupted_bootstrap = true,
                    SELECTOR_ARTIFACT_NAME => {
                        forbidden = forbidden.with_selector_artifact_in_root();
                    }
                    CACHE_ARTIFACT_NAME => {
                        if !managed_cache_layout_is_committed(&child_path)? {
                            forbidden = forbidden.with_cache_artifact();
                        }
                    }
                    LEGACY_CACHE_DIRECTORY_NAME => {
                        if !managed_cache_layout_is_committed(&child_path)? {
                            forbidden = forbidden.with_cache_artifact();
                        }
                    }
                    INSTALLATIONS_ARTIFACT_NAME => {
                        if !managed_installations_layout_is_committed(&child_path)? {
                            forbidden = forbidden.with_installations_artifact();
                        }
                    }
                    LEGACY_INSTALLATIONS_DIRECTORY_NAME => {
                        if !managed_installations_layout_is_committed(&child_path)? {
                            forbidden = forbidden.with_installations_artifact();
                        }
                    }
                    STAGING_ARTIFACT_NAME => {
                        forbidden = forbidden.with_staging_artifact();
                    }
                    QUARANTINE_ARTIFACT_NAME => {
                        forbidden = forbidden.with_quarantine_artifact();
                    }
                    TEMP_ARTIFACT_NAME => {
                        forbidden = forbidden.with_temp_artifact();
                    }
                    _ if is_uncommitted_layout_artifact_name(&child_name) => {
                        forbidden = forbidden.with_staging_artifact();
                    }
                    _ if child_name.starts_with("platform.") => {
                        forbidden = forbidden.with_unknown_sidecar();
                    }
                    _ if child_name.starts_with("platform") => {
                        forbidden = forbidden.with_unknown_artifact();
                    }
                    _ => forbidden = forbidden.with_unknown_artifact(),
                }
            }
        }
    }

    revalidate_root_binding(binding)?;

    let mut family = PlatformStorageFamilyObservation::empty()
        .with_required(required)
        .with_optional(optional)
        .with_forbidden(forbidden);
    if interrupted_bootstrap {
        family = family.with_interrupted_bootstrap();
    }
    if released_sidecar {
        family = family.with_released_sidecar();
    }
    if live_exclusive_lock {
        family = family.with_live_exclusive_lock();
    }

    let root_surface = if !saw_any_content {
        ObservedRootSurface::PhysicallyEmpty
    } else {
        ObservedRootSurface::Nonempty
    };

    Ok(ReadOnlyStoragePreflightFacts::new(root_surface, family))
}

fn managed_cache_layout_is_committed(root: &Path) -> McpPlatformResult<bool> {
    let mut entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Ok(false),
    };
    let mut count = 0usize;
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(|_| {
            integrity_unavailable("windows read-only preflight evidence is unavailable")
        })?;
        count += 1;
        if count > MAX_MANAGED_LAYOUT_ENTRIES {
            return Ok(false);
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_lower_hex_sha256(&name) || !managed_layout_object_is_regular(&entry.path(), false)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn managed_installations_layout_is_committed(root: &Path) -> McpPlatformResult<bool> {
    let mut entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Ok(false),
    };
    let mut count = 0usize;
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(|_| {
            integrity_unavailable("windows read-only preflight evidence is unavailable")
        })?;
        count += 1;
        if count > MAX_MANAGED_LAYOUT_ENTRIES {
            return Ok(false);
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !managed_layout_segment_is_safe(&name)
            || !managed_layout_object_is_regular(&entry.path(), true)?
            || !managed_installation_is_committed(&entry.path())?
        {
            return Ok(false);
        }
    }
    Ok(count > 0)
}

fn managed_installation_is_committed(root: &Path) -> McpPlatformResult<bool> {
    let mut entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Ok(false),
    };
    let mut count = 0usize;
    let mut saw_versions = false;
    let mut saw_active = false;
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(|_| {
            integrity_unavailable("windows read-only preflight evidence is unavailable")
        })?;
        count += 1;
        if count > 2 {
            return Ok(false);
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        match name.as_ref() {
            "active.json"
                if !saw_active && managed_layout_object_is_regular(&entry.path(), false)? =>
            {
                saw_active = true;
            }
            "versions" if managed_layout_object_is_regular(&entry.path(), true)? => {
                if saw_versions {
                    return Ok(false);
                }
                saw_versions = managed_versions_layout_is_committed(&entry.path())?;
            }
            _ => return Ok(false),
        }
    }
    Ok(saw_versions)
}

fn managed_versions_layout_is_committed(root: &Path) -> McpPlatformResult<bool> {
    let mut entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Ok(false),
    };
    let mut count = 0usize;
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(|_| {
            integrity_unavailable("windows read-only preflight evidence is unavailable")
        })?;
        count += 1;
        if count > MAX_MANAGED_LAYOUT_ENTRIES {
            return Ok(false);
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let mut tree_entries = 0;
        if !managed_layout_segment_is_safe(&name)
            || !managed_layout_object_is_regular(&entry.path(), true)?
            || !managed_tree_is_regular(&entry.path(), &mut tree_entries)?
        {
            return Ok(false);
        }
    }
    Ok(count > 0)
}

fn managed_tree_is_regular(root: &Path, count: &mut usize) -> McpPlatformResult<bool> {
    let mut entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Ok(false),
    };
    let initial_count = *count;
    while let Some(entry) = entries.next() {
        let entry = entry.map_err(|_| {
            integrity_unavailable("windows read-only preflight evidence is unavailable")
        })?;
        *count += 1;
        if *count > MAX_MANAGED_LAYOUT_ENTRIES {
            return Ok(false);
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !managed_layout_segment_is_safe(&name) {
            return Ok(false);
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| {
            integrity_unavailable("windows read-only preflight evidence is unavailable")
        })?;
        if metadata.file_type().is_dir() {
            if !managed_layout_object_is_regular(&entry.path(), true)?
                || !managed_tree_is_regular(&entry.path(), count)?
            {
                return Ok(false);
            }
        } else if !managed_layout_object_is_regular(&entry.path(), false)? {
            return Ok(false);
        }
    }
    Ok(*count > initial_count)
}

fn managed_layout_object_is_regular(path: &Path, directory: bool) -> McpPlatformResult<bool> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        integrity_unavailable("windows read-only preflight evidence is unavailable")
    })?;
    if metadata.file_type().is_symlink()
        || metadata.file_type().is_dir() != directory
        || (!directory && !metadata.file_type().is_file())
        || metadata.file_attributes()
            & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM | FILE_ATTRIBUTE_REPARSE_POINT)
            != 0
        || has_named_streams(path)?
    {
        return Ok(false);
    }
    let file = match try_open_evidence_handle(path, directory) {
        OpenOutcome::Opened(file) => file,
        OpenOutcome::NotFound | OpenOutcome::SharingViolation | OpenOutcome::Unavailable => {
            return Ok(false);
        }
    };
    if !windows_normal_path_case_match(&final_dos_path(&file)?, &path.to_string_lossy())
        || (!directory && FileIdentity::link_count(&file)? > 1)
    {
        return Ok(false);
    }
    Ok(true)
}

fn windows_normal_path_case_match(expected: &str, observed: &str) -> bool {
    expected.eq_ignore_ascii_case(observed)
}

fn managed_layout_segment_is_safe(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains(['/', '\\', ':', '\0'])
        && !value.ends_with(['.', ' '])
        // A tilde is rejected rather than trying to distinguish a user chosen
        // name from a DOS 8.3 short-name alias of a sibling object.
        && !value.contains('~')
        && !is_windows_device_name(value)
}

fn is_windows_device_name(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .trim_end_matches(['.', ' '])
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || stem.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn open_expected_d_volume() -> McpPlatformResult<FileIdentity> {
    let root = wide("D:\\");
    let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
    if drive_type != WINDOWS_DRIVE_FIXED {
        return Err(integrity_unavailable(
            "windows read-only preflight volume is unsupported",
        ));
    }
    let volume_root = open_directory_handle(Path::new(r"D:\"))?;
    ensure_supported_filesystem(&volume_root)?;
    FileIdentity::from_file(&volume_root)
}

fn mounted_d_volume_identity(volume_serial: u32) -> McpPlatformResult<LiveVolumeIdentity> {
    let mount_point = wide("D:\\");
    let mut volume_name = vec![0_u16; 128];
    let result = unsafe {
        GetVolumeNameForVolumeMountPointW(
            mount_point.as_ptr(),
            volume_name.as_mut_ptr(),
            volume_name.len() as u32,
        )
    };
    if result == 0 {
        return Err(integrity_unavailable(
            "windows read-only preflight volume is unsupported",
        ));
    }
    let volume_name = nul_terminated_utf16(&volume_name);
    if !volume_name.starts_with(r"\\?\Volume{") || !volume_name.ends_with('\\') {
        return Err(integrity_unavailable(
            "windows read-only preflight volume is unsupported",
        ));
    }
    LiveVolumeIdentity::new(format!("{volume_name}\0{volume_serial:08x}"))
        .ok_or_else(|| integrity_unavailable("windows read-only preflight volume is unsupported"))
}

fn ensure_same_volume(actual: &FileIdentity, expected: &FileIdentity) -> McpPlatformResult<()> {
    if actual.volume_serial != expected.volume_serial {
        return Err(integrity_unavailable(
            "windows read-only preflight volume is unsupported",
        ));
    }
    Ok(())
}

fn ensure_supported_filesystem(file: &File) -> McpPlatformResult<()> {
    let mut filesystem_name = vec![0_u16; 32];
    let mut serial = 0_u32;
    let mut max_component_length = 0_u32;
    let mut flags = 0_u32;
    let result = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle() as HANDLE,
            std::ptr::null_mut(),
            0,
            &mut serial,
            &mut max_component_length,
            &mut flags,
            filesystem_name.as_mut_ptr(),
            filesystem_name.len() as u32,
        )
    };
    if result == 0 {
        return Err(integrity_unavailable(
            "windows read-only preflight volume is unsupported",
        ));
    }

    let filesystem_name = nul_terminated_utf16(&filesystem_name);
    if !filesystem_name.eq_ignore_ascii_case("NTFS") {
        return Err(integrity_unavailable(
            "windows read-only preflight volume is unsupported",
        ));
    }
    let _ = (serial, max_component_length, flags);
    Ok(())
}

fn nearest_existing_ancestor(lexical: &LexicalSelectorPath) -> McpPlatformResult<(PathBuf, File)> {
    let mut cursor = lexical.as_path().parent().map(Path::to_path_buf);
    while let Some(candidate) = cursor {
        match open_directory_handle(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if matches!(error.code(), McpPlatformErrorCode::NotFound) => {
                cursor = candidate.parent().map(Path::to_path_buf);
            }
            Err(_) => {
                return Err(integrity_unavailable(
                    "windows read-only preflight evidence is unavailable",
                ));
            }
        }
    }
    Err(integrity_unavailable(
        "windows read-only preflight evidence is unavailable",
    ))
}

fn open_directory_handle(path: &Path) -> McpPlatformResult<File> {
    open_handle(path, true)
}

const WRITER_ROOT_ACCESS: u32 =
    FILE_READ_ATTRIBUTES | 0x0000_0002 | 0x0000_0004 | 0x0000_0020 | 0x0010_0000;

fn open_writer_directory_handle(path: &Path) -> McpPlatformResult<File> {
    let mut options = OpenOptions::new();
    options
        .access_mode(WRITER_ROOT_ACCESS)
        // Windows checks sharing in both directions. By granting only read
        // sharing, this acquisition rejects a root handle which was already
        // opened for write or delete and prevents one from being opened for
        // the lifetime of this lease.
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);

    options
        .open(path)
        .map_err(|_| integrity_unavailable("windows runtime writer lease root is unavailable"))
}

fn open_handle(path: &Path, is_directory: bool) -> McpPlatformResult<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .access_mode(
            FILE_READ_ATTRIBUTES
                | if is_directory {
                    DIRECTORY_TRAVERSE_ACCESS
                } else {
                    0
                },
        )
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if is_directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        );

    options
        .open(path)
        .map_err(|error| match error.raw_os_error() {
            Some(code)
                if code as u32 == ERROR_FILE_NOT_FOUND || code as u32 == ERROR_PATH_NOT_FOUND =>
            {
                McpPlatformError::new(McpPlatformErrorCode::NotFound, "windows path not found")
            }
            Some(code) if code as u32 == ERROR_ACCESS_DENIED => {
                integrity_unavailable("windows read-only preflight evidence is unavailable")
            }
            _ => integrity_unavailable("windows read-only preflight evidence is unavailable"),
        })
}

enum OpenOutcome {
    Opened(File),
    NotFound,
    SharingViolation,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootPathProbe {
    Present,
    NotFound,
    Unavailable,
}

fn try_open_evidence_handle(path: &Path, is_directory: bool) -> OpenOutcome {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if is_directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        );

    match options.open(path) {
        Ok(file) => OpenOutcome::Opened(file),
        Err(error) => match error.raw_os_error() {
            Some(code)
                if code as u32 == ERROR_FILE_NOT_FOUND || code as u32 == ERROR_PATH_NOT_FOUND =>
            {
                OpenOutcome::NotFound
            }
            Some(code)
                if code as u32 == ERROR_SHARING_VIOLATION
                    || code as u32 == ERROR_LOCK_VIOLATION =>
            {
                OpenOutcome::SharingViolation
            }
            _ => OpenOutcome::Unavailable,
        },
    }
}

fn probe_root_path(path: &Path) -> RootPathProbe {
    let wide = wide_os(path.as_os_str());
    let mut find_data = WIN32_FIND_DATAW::default();
    let handle = unsafe { FindFirstFileW(wide.as_ptr(), &mut find_data) };
    if handle == INVALID_HANDLE_VALUE {
        return match unsafe { GetLastError() } {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => RootPathProbe::NotFound,
            ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION => {
                RootPathProbe::Unavailable
            }
            _ => RootPathProbe::Unavailable,
        };
    }
    unsafe {
        FindClose(handle);
    }
    RootPathProbe::Present
}

fn final_dos_path(file: &File) -> McpPlatformResult<String> {
    let handle = file.as_raw_handle() as HANDLE;
    let mut buffer = vec![0_u16; 512];
    let written = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED,
        )
    };
    if written == 0 {
        return Err(integrity_unavailable(
            "windows read-only preflight evidence is unavailable",
        ));
    }
    if written as usize >= buffer.len() {
        buffer.resize(written as usize + 1, 0);
        let rewritten = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED,
            )
        };
        if rewritten == 0 {
            return Err(integrity_unavailable(
                "windows read-only preflight evidence is unavailable",
            ));
        }
    }

    let final_path = nul_terminated_utf16(&buffer);
    final_path
        .strip_prefix("\\\\?\\")
        .map(str::to_string)
        .ok_or_else(|| {
            integrity_unavailable("windows read-only preflight selector path is unsupported")
        })
}

fn find_path_tag(path: &Path) -> McpPlatformResult<PathTagEvidence> {
    let wide = wide_os(path.as_os_str());
    let mut find_data = WIN32_FIND_DATAW::default();
    let handle = unsafe { FindFirstFileW(wide.as_ptr(), &mut find_data) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(integrity_unavailable(
            "windows read-only preflight evidence is unavailable",
        ));
    }
    unsafe {
        FindClose(handle);
    }
    Ok(PathTagEvidence {
        attributes: find_data.dwFileAttributes,
        reparse_tag: find_data.dwReserved0,
    })
}

fn has_named_streams(path: &Path) -> McpPlatformResult<bool> {
    let wide = wide_os(path.as_os_str());
    let mut stream_data = WIN32_FIND_STREAM_DATA::default();
    let handle = unsafe {
        FindFirstStreamW(
            wide.as_ptr(),
            FindStreamInfoStandard,
            &mut stream_data as *mut _ as *mut core::ffi::c_void,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return classify_find_first_stream_failure(unsafe { GetLastError() });
    }

    let result = loop {
        if stream_name(&stream_data.cStreamName) != "::$DATA" {
            break Ok(true);
        }
        let result = unsafe {
            FindNextStreamW(handle, &mut stream_data as *mut _ as *mut core::ffi::c_void)
        };
        match classify_find_next_stream_result(result, unsafe { GetLastError() }) {
            Ok(StreamEnumerationStep::Continue) => {}
            Ok(StreamEnumerationStep::Complete) => break Ok(false),
            Err(error) => break Err(error),
        }
    };

    unsafe {
        FindClose(handle);
    }

    result
}

fn classify_find_first_stream_failure(error_code: u32) -> McpPlatformResult<bool> {
    if error_code == ERROR_HANDLE_EOF {
        return Ok(false);
    }
    Err(integrity_unavailable(
        "windows read-only preflight evidence is unavailable",
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamEnumerationStep {
    Continue,
    Complete,
}

fn classify_find_next_stream_result(
    result: i32,
    error_code: u32,
) -> McpPlatformResult<StreamEnumerationStep> {
    if result != 0 {
        return Ok(StreamEnumerationStep::Continue);
    }
    if error_code == ERROR_HANDLE_EOF {
        return Ok(StreamEnumerationStep::Complete);
    }
    Err(integrity_unavailable(
        "windows read-only preflight evidence is unavailable",
    ))
}

pub(super) fn revalidate_root_binding(binding: &RootBinding) -> McpPlatformResult<()> {
    let current_identity = FileIdentity::from_file(&binding.root_file)?;
    if current_identity != binding.root_identity {
        return Err(integrity_error("windows read-only preflight root drifted"));
    }
    if !windows_normal_path_case_match(
        &final_dos_path(&binding.root_file)?,
        &binding.expected_final_path,
    ) {
        return Err(integrity_error("windows read-only preflight root drifted"));
    }
    ensure_same_volume(&current_identity, &binding.expected_volume_identity)?;
    if binding.volume_identity_from_mount_point
        && binding.volume_identity != mounted_d_volume_identity(current_identity.volume_serial)?
    {
        return Err(integrity_error(
            "windows read-only preflight volume drifted",
        ));
    }
    if file_attributes(&binding.root_file)? != binding.root_attributes {
        return Err(integrity_error("windows read-only preflight root drifted"));
    }

    let live_root = open_directory_handle(binding.lexical.as_path())?;
    let live_identity = FileIdentity::from_file(&live_root)?;
    if live_identity != binding.root_identity
        || !windows_normal_path_case_match(
            &final_dos_path(&live_root)?,
            &binding.expected_final_path,
        )
        || file_attributes(&live_root)? != binding.root_attributes
    {
        return Err(integrity_error("windows read-only preflight root drifted"));
    }
    ensure_same_volume(&live_identity, &binding.expected_volume_identity)?;
    if binding.volume_identity_from_mount_point
        && binding.volume_identity != mounted_d_volume_identity(live_identity.volume_serial)?
    {
        return Err(integrity_error(
            "windows read-only preflight volume drifted",
        ));
    }
    Ok(())
}

pub(super) fn acquire_writer_root(binding: &RootBinding) -> McpPlatformResult<File> {
    revalidate_root_binding(binding)?;
    let writer_root = open_writer_directory_handle(binding.lexical.as_path())?;
    revalidate_writer_root(binding, &writer_root)?;
    Ok(writer_root)
}

pub(super) fn revalidate_writer_root(
    binding: &RootBinding,
    writer_root: &File,
) -> McpPlatformResult<()> {
    revalidate_root_binding(binding)?;

    let writer_identity = FileIdentity::from_file(writer_root)?;
    if writer_identity != binding.root_identity
        || !windows_normal_path_case_match(
            &final_dos_path(writer_root)?,
            &binding.expected_final_path,
        )
        || file_attributes(writer_root)? != binding.root_attributes
    {
        return Err(integrity_error("windows runtime writer lease root drifted"));
    }
    ensure_same_volume(&writer_identity, &binding.expected_volume_identity)?;

    // `revalidate_root_binding` above performs the path-to-handle identity
    // comparison through a read-only probe. Do not open another writer lease
    // here: the live lease deliberately denies write/delete sharing, so a
    // second writer open would conflict with this session's own root lock.
    Ok(())
}

pub(super) fn runtime_writer_context_matches(
    binding: &RootBinding,
    locator: &PlatformLocator,
    context: &PlatformStorageContext,
) -> bool {
    context.matches_locator(locator)
        && context.root_state() == PlatformRootState::Present
        && locator.selector().selector_revision() == binding.selector_revision
        && context.observation_epoch() == binding.observation_epoch
        && context.volume_identity() == &binding.volume_identity
        && context.root_identity() == Some(&binding.root_identity.root_identity())
        && context.root_surface_evidence() == Some(&binding.root_surface_evidence())
        && binding.selector_exact_match
}

fn runtime_writer_observation_matches(
    binding: &RootBinding,
    locator: &PlatformLocator,
    context: &PlatformStorageContext,
) -> bool {
    context.matches_locator(locator)
        && context.root_state() == PlatformRootState::Present
        && locator.selector().selector_revision() == binding.selector_revision
        && context.volume_identity() == &binding.volume_identity
        && context.root_identity() == Some(&binding.root_identity.root_identity())
        && context.root_surface_evidence() == Some(&binding.root_surface_evidence())
        && binding.selector_exact_match
}

pub(super) fn file_attributes(file: &File) -> McpPlatformResult<u32> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let result =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut information) };
    if result == 0 {
        return Err(integrity_unavailable(
            "windows read-only preflight evidence is unavailable",
        ));
    }
    Ok(information.dwFileAttributes)
}

#[cfg(test)]
pub(super) fn test_root_binding(root_file: File) -> McpPlatformResult<RootBinding> {
    let root_identity = FileIdentity::from_file(&root_file)?;
    let expected_final_path = final_dos_path(&root_file)?;
    let root_attributes = file_attributes(&root_file)?;
    Ok(RootBinding {
        lexical: LexicalSelectorPath {
            raw: expected_final_path.clone(),
        },
        expected_final_path,
        root_file,
        expected_volume_identity: root_identity.clone(),
        volume_identity: root_identity.volume_identity(),
        volume_identity_from_mount_point: false,
        root_identity,
        selector_revision: 1,
        observation_epoch: LiveObservationEpoch::new(1).expect("test epoch is nonzero"),
        selector_exact_match: true,
        root_attributes,
    })
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum TestRootBindingPath {
    Production(PlatformStoragePreflightClassification),
    TestOnly(McpPlatformErrorCode),
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum TestRootWriterAcquisition {
    Available,
    Rejected(McpPlatformErrorCode),
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
struct TestRootMetadataDiagnostic {
    binding_path: TestRootBindingPath,
    final_dos_path: String,
    file_attributes: u32,
    directory_link_count: u32,
    is_reparse_point: bool,
    has_named_streams: bool,
    root_identity: String,
    volume_binding: String,
    managed_installation_anchor_binding: String,
    managed_installation_lineage_binding: String,
    selector_exact_match: bool,
    writer_root_acquisition: TestRootWriterAcquisition,
    writer_capability_acquisition: TestRootWriterAcquisition,
}

#[cfg(test)]
fn test_root_metadata_from_binding(
    binding_path: TestRootBindingPath,
    binding: RootBinding,
) -> McpPlatformResult<TestRootMetadataDiagnostic> {
    let root_tag = find_path_tag(binding.lexical.as_path())?;
    let final_dos_path = binding.expected_final_path.clone();
    let file_attributes = binding.root_attributes;
    let directory_link_count = FileIdentity::link_count(&binding.root_file)?;
    let is_reparse_point = file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || root_tag.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || root_tag.reparse_tag != 0;
    let has_named_streams = has_named_streams(Path::new(&final_dos_path))?;
    let root_identity = binding.root_identity.durable_key();
    let volume_binding = binding.volume_identity.opaque.clone();
    let managed_installation_anchor_binding = binding.managed_installation_anchor_binding();
    let managed_installation_lineage_binding = binding.managed_installation_lineage_binding();
    let selector_exact_match = binding.selector_exact_match;
    let writer_root_acquisition = match acquire_writer_root(&binding) {
        Ok(writer_root) => {
            drop(writer_root);
            TestRootWriterAcquisition::Available
        }
        Err(error) => TestRootWriterAcquisition::Rejected(error.code()),
    };
    let writer_capability_acquisition =
        match ManagedRootCapability::from_verified_preflight(binding) {
            Ok(capability) => {
                drop(capability);
                TestRootWriterAcquisition::Available
            }
            Err(error) => TestRootWriterAcquisition::Rejected(error.code()),
        };

    Ok(TestRootMetadataDiagnostic {
        binding_path,
        final_dos_path,
        file_attributes,
        directory_link_count,
        is_reparse_point,
        has_named_streams,
        root_identity,
        volume_binding,
        managed_installation_anchor_binding,
        managed_installation_lineage_binding,
        selector_exact_match,
        writer_root_acquisition,
        writer_capability_acquisition,
    })
}

#[cfg(test)]
fn test_fresh_tempfile_root_metadata(root: &Path) -> McpPlatformResult<TestRootMetadataDiagnostic> {
    let locator = PlatformLocator::from_selector(
        DSelectorRecord::new("temporary-root-diagnostic", 1, 1, root)
            .expect("fresh tempfile roots always have a path"),
    );
    let adapter = WindowsStoragePreflightAdapter::new();

    match adapter
        .begin_read_only_preflight_session(&locator)
        .and_then(|session| session.issue_read_only_request())
        .and_then(|request| request.consume())
    {
        Ok(classification) => match WindowsStoragePreflightAdapter::open_binding(&locator)? {
            Binding::Present(binding) => test_root_metadata_from_binding(
                TestRootBindingPath::Production(classification),
                binding,
            ),
            Binding::Absent(_) => Err(integrity_error(
                "windows test temporary root unexpectedly became absent",
            )),
        },
        Err(error) => test_root_metadata_from_binding(
            TestRootBindingPath::TestOnly(error.code()),
            test_root_binding(open_directory_handle(root)?)?,
        ),
    }
}

fn validate_capacity_witness(witness: &ManagedStorageCapacityWitness) -> McpPlatformResult<()> {
    validate_capacity_binding(&witness.lexical, &witness.binding)?;
    validate_capacity_root_surface(
        witness.lexical.as_path(),
        &witness.binding.expected_final_path,
    )
}

fn validate_capacity_binding(
    lexical: &LexicalSelectorPath,
    binding: &CapacityBinding,
) -> McpPlatformResult<()> {
    if !windows_normal_path_case_match(lexical.raw(), &binding.expected_final_path) {
        return Err(integrity_error("windows managed storage root drifted"));
    }
    ensure_same_volume(&binding.root_identity, &binding.expected_volume_identity)?;
    if binding.root_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(integrity_unavailable(
            "windows managed storage root is unavailable",
        ));
    }
    Ok(())
}

fn validate_capacity_root_surface(
    lexical: &Path,
    expected_final_path: &str,
) -> McpPlatformResult<()> {
    let root_tag = find_path_tag(lexical)?;
    if root_tag.attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
        || root_tag.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || root_tag.reparse_tag != 0
        || has_named_streams(Path::new(expected_final_path))?
    {
        return Err(integrity_unavailable(
            "windows managed storage root is unavailable",
        ));
    }
    Ok(())
}

fn revalidate_capacity_witness(witness: &ManagedStorageCapacityWitness) -> McpPlatformResult<()> {
    let current = CapacityBinding {
        root_identity: FileIdentity::from_file(&witness.root_file)?,
        root_attributes: file_attributes(&witness.root_file)?,
        expected_final_path: final_dos_path(&witness.root_file)?,
        expected_volume_identity: open_expected_d_volume()?,
    };
    ensure_capacity_binding_stable(&witness.binding, &current)?;

    let rebound = open_directory_handle(witness.lexical.as_path())?;
    let rebound = CapacityBinding {
        root_identity: FileIdentity::from_file(&rebound)?,
        root_attributes: file_attributes(&rebound)?,
        expected_final_path: final_dos_path(&rebound)?,
        expected_volume_identity: current.expected_volume_identity.clone(),
    };
    ensure_capacity_binding_stable(&witness.binding, &rebound)?;
    validate_capacity_root_surface(
        witness.lexical.as_path(),
        &witness.binding.expected_final_path,
    )
}

fn ensure_capacity_binding_stable(
    initial: &CapacityBinding,
    current: &CapacityBinding,
) -> McpPlatformResult<()> {
    if initial != current {
        return Err(integrity_error("windows managed storage root drifted"));
    }
    Ok(())
}

fn classify_absent_root_probe_result(probe: RootPathProbe) -> McpPlatformResult<()> {
    match probe {
        RootPathProbe::NotFound => Ok(()),
        RootPathProbe::Present => Err(integrity_error("windows read-only preflight root drifted")),
        RootPathProbe::Unavailable => Err(integrity_unavailable(
            "windows read-only preflight evidence is unavailable",
        )),
    }
}

fn revalidate_absent_root(binding: &AbsentBinding) -> McpPlatformResult<()> {
    classify_absent_root_probe_result(probe_root_path(binding.lexical.as_path()))
}

fn next_observation_epoch() -> McpPlatformResult<LiveObservationEpoch> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);
    let next = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
    LiveObservationEpoch::new(next)
        .ok_or_else(|| integrity_unavailable("windows read-only preflight evidence is unavailable"))
}

fn stream_name(buffer: &[u16]) -> String {
    nul_terminated_utf16(buffer)
}

fn nul_terminated_utf16(buffer: &[u16]) -> String {
    let length = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn wide_os(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use sha2::{Digest as _, Sha256};
    use windows_sys::Win32::Foundation::ERROR_INVALID_FUNCTION;

    use super::*;

    #[derive(Clone)]
    struct ProductionVersionPayload {
        relative_segments: Vec<String>,
        bytes: Vec<u8>,
    }

    fn production_version_payloads(
        managed_mcp_id: &str,
        version: &str,
        executable: &[u8],
    ) -> Vec<ProductionVersionPayload> {
        let descriptor = format!(r#"{{"version":"{version}"}}"#).into_bytes();
        let mut committed = vec![
            (".lumina-runtime-descriptor.json".to_owned(), descriptor),
            ("server.exe".to_owned(), executable.to_vec()),
        ];
        committed.sort_by(|left, right| left.0.cmp(&right.0));
        let mut tree = Sha256::new();
        for (leaf, bytes) in &committed {
            tree.update(leaf.as_bytes());
            tree.update([0]);
            tree.update((bytes.len() as u64).to_le_bytes());
            tree.update(sha256_hex(bytes).as_bytes());
        }
        let descriptor_sha256 = committed
            .iter()
            .find(|(leaf, _)| leaf == ".lumina-runtime-descriptor.json")
            .map(|(_, bytes)| sha256_hex(bytes))
            .expect("descriptor");
        committed.push((
            ".lumina-committed-version.json".to_owned(),
            serde_json::to_vec(&serde_json::json!({
                "descriptor_sha256": descriptor_sha256,
                "tree_sha256": sha256_hex(&tree.finalize()),
            }))
            .expect("evidence"),
        ));
        committed
            .into_iter()
            .map(|(leaf, bytes)| ProductionVersionPayload {
                relative_segments: vec![
                    managed_mcp_id.to_owned(),
                    "versions".to_owned(),
                    version.to_owned(),
                    leaf,
                ],
                bytes,
            })
            .collect()
    }

    fn production_manifest(
        payloads: &[ProductionVersionPayload],
    ) -> super::super::DirectoryPayloadManifest {
        super::super::DirectoryPayloadManifest {
            entries: payloads
                .iter()
                .map(|payload| super::super::DirectoryPayloadManifestEntry {
                    relative_segments: payload.relative_segments.clone(),
                    size: payload.bytes.len() as u64,
                    sha256: sha256_hex(&payload.bytes),
                })
                .collect(),
        }
    }

    #[test]
    fn find_next_stream_eof_is_the_only_normal_completion() {
        assert_eq!(
            classify_find_next_stream_result(0, ERROR_HANDLE_EOF)
                .expect("EOF must end stream enumeration"),
            StreamEnumerationStep::Complete
        );
    }

    #[test]
    fn find_next_stream_non_eof_failure_fails_closed() {
        for error_code in [ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND] {
            let error = classify_find_next_stream_result(0, error_code)
                .expect_err("a non-EOF FindNextStreamW failure must be rejected");
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        }
    }

    #[test]
    fn find_first_stream_eof_is_normal_empty_result() {
        assert!(!classify_find_first_stream_failure(ERROR_HANDLE_EOF)
            .expect("FindFirstStreamW EOF must mean no named streams"));
    }

    #[test]
    fn find_first_stream_non_eof_failure_fails_closed() {
        for error_code in [
            ERROR_ACCESS_DENIED,
            ERROR_FILE_NOT_FOUND,
            ERROR_INVALID_FUNCTION,
        ] {
            let error = classify_find_first_stream_failure(error_code)
                .expect_err("a non-EOF FindFirstStreamW failure must be rejected");
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        }
    }

    #[test]
    fn fresh_tempfile_root_metadata_distinguishes_fixture_eligibility_from_writer_state() {
        let temporary = tempfile::tempdir().expect("fresh tempfile root");
        let diagnostic = test_fresh_tempfile_root_metadata(temporary.path())
            .expect("fresh tempfile root metadata");
        eprintln!("fresh tempfile root metadata: {diagnostic:#?}");

        assert!(!diagnostic.final_dos_path.is_empty(), "{diagnostic:#?}");
        assert_ne!(diagnostic.file_attributes & 0x10, 0, "{diagnostic:#?}");
        assert_ne!(diagnostic.directory_link_count, 0, "{diagnostic:#?}");

        match &diagnostic.binding_path {
            TestRootBindingPath::Production(classification) => {
                assert_eq!(
                    *classification,
                    PlatformStoragePreflightClassification::RootPhysicallyEmpty,
                    "{diagnostic:#?}"
                );
                assert!(!diagnostic.is_reparse_point, "{diagnostic:#?}");
                assert!(!diagnostic.has_named_streams, "{diagnostic:#?}");
                assert!(diagnostic.selector_exact_match, "{diagnostic:#?}");
                assert_eq!(diagnostic.directory_link_count, 1, "{diagnostic:#?}");
            }
            TestRootBindingPath::TestOnly(error) => {
                assert_eq!(
                    *error,
                    McpPlatformErrorCode::IntegrityUnavailable,
                    "{diagnostic:#?}"
                );
            }
        }
    }

    #[test]
    #[ignore = "requires a writable fixed NTFS D: volume; run explicitly to diagnose tempfile-root preflight and writer acquisition"]
    fn fresh_d_tempfile_root_reports_production_preflight_and_writer_acquisition() {
        let temporary = tempfile::Builder::new()
            .prefix("lumina-tempfile-root-diagnostic-")
            .tempdir_in(Path::new(r"D:\"))
            .expect("create writable D: NTFS temporary root");
        let diagnostic = test_fresh_tempfile_root_metadata(temporary.path())
            .expect("fresh D: tempfile root metadata");
        eprintln!("fresh D: tempfile root metadata: {diagnostic:#?}");

        assert_eq!(
            diagnostic.binding_path,
            TestRootBindingPath::Production(
                PlatformStoragePreflightClassification::RootPhysicallyEmpty
            ),
            "{diagnostic:#?}"
        );
        assert!(!diagnostic.is_reparse_point, "{diagnostic:#?}");
        assert!(!diagnostic.has_named_streams, "{diagnostic:#?}");
        assert_eq!(diagnostic.directory_link_count, 1, "{diagnostic:#?}");
        assert!(diagnostic.selector_exact_match, "{diagnostic:#?}");
        assert_eq!(
            diagnostic.writer_root_acquisition,
            TestRootWriterAcquisition::Available,
            "{diagnostic:#?}"
        );
        assert_eq!(
            diagnostic.writer_capability_acquisition,
            TestRootWriterAcquisition::Available,
            "{diagnostic:#?}"
        );
    }

    #[test]
    fn runtime_writer_contract_routes_staging_through_managed_root_capability() {
        let source = include_str!("windows_storage_preflight.rs");
        let start = source
            .find("impl RuntimeStorageWriterWitness for WindowsRuntimeStorageWriterWitness")
            .unwrap();
        let end = source[start..].find("enum Binding").unwrap() + start;
        let writer = &source[start..end];
        for forbidden in [
            "fs::create_dir",
            "fs::rename",
            "fs::remove_",
            "OpenOptions::new",
        ] {
            assert!(
                !writer.contains(forbidden),
                "runtime writer must not regain path-based mutation: {forbidden}"
            );
        }
        assert!(writer.contains("capability.create_staging"));
        assert!(writer.contains("capability.write_verified_payload"));
        assert!(writer.contains("capability.verify_regular_payload"));
        assert!(writer.contains("RuntimeControlUnavailable"));
        assert!(source.contains("_issuer: &RuntimeStorageWriterIssuer"));
    }

    #[test]
    fn managed_installation_promotion_orchestration_uses_the_direct_final_writer_path() {
        struct RecordingWriter {
            calls: Vec<&'static str>,
        }

        impl ManagedInstallationPromotionWriter for RecordingWriter {
            fn create_directory_staging(
                &mut self,
                _: &super::super::DirectoryPayloadManifest,
            ) -> McpPlatformResult<()> {
                self.calls.push("staging");
                Ok(())
            }

            fn materialize_verified_directory(
                &mut self,
                _: &[super::super::DirectoryPayload<'_>],
            ) -> McpPlatformResult<()> {
                self.calls.push("materialize");
                Ok(())
            }

            fn seal_staged_directory(&mut self) -> McpPlatformResult<()> {
                self.calls.push("seal");
                Ok(())
            }

            fn promote_direct_final_managed_installation(&mut self) -> McpPlatformResult<()> {
                self.calls.push("direct-final-managed-promotion");
                Ok(())
            }
        }

        let manifest = super::super::DirectoryPayloadManifest {
            entries: Vec::new(),
        };
        let mut writer = RecordingWriter { calls: Vec::new() };
        run_managed_installation_promotion_sequence(&mut writer, &manifest, &[])
            .expect("recorded orchestration");
        assert_eq!(
            writer.calls,
            [
                "staging",
                "materialize",
                "seal",
                "direct-final-managed-promotion"
            ]
        );
    }

    #[test]
    #[ignore = "requires a writable NTFS D: volume; run explicitly for production filesystem coverage"]
    fn production_observation_rebinds_to_a_private_epoch_and_grants_a_writer_lease() {
        let temporary = tempfile::Builder::new()
            .prefix("lumina-writer-preflight-")
            .tempdir_in(Path::new(r"D:\"))
            .expect("create writable D: NTFS temporary root");
        let locator = PlatformLocator::from_selector(
            DSelectorRecord::new("writer-preflight", 1, 1, temporary.path()).unwrap(),
        );
        let adapter = WindowsStoragePreflightAdapter::new();
        let context = adapter
            .observe_present_root(&locator)
            .expect("observe managed root");
        let mut lease = super::super::acquire_live_windows_runtime_writer_lease(&locator, &context)
            .expect("a current production context must survive a fresh writer rebind");
        let bytes = b"production writer payload";
        let digest = sha256_hex(bytes);

        lease.create_staging().expect("staging");
        lease
            .write_verified_bytes(super::super::RuntimeStorageSlot::Payload, bytes, &digest)
            .expect("payload");
        lease
            .verify_regular_file(super::super::RuntimeStorageSlot::Payload)
            .expect("regular payload");
        assert_eq!(
            std::fs::read(temporary.path().join("lumina-staging").join("payload"))
                .expect("payload bytes"),
            bytes
        );
    }

    #[test]
    #[ignore = "requires a writable NTFS D: volume; run explicitly for production filesystem coverage"]
    fn production_d_root_directory_writer_materializes_and_promotes_a_committed_cache() {
        let temporary = tempfile::Builder::new()
            .prefix("lumina-directory-writer-")
            .tempdir_in(Path::new(r"D:\"))
            .expect("create writable D: NTFS temporary root");
        let locator = PlatformLocator::from_selector(
            DSelectorRecord::new("directory-writer", 1, 1, temporary.path()).unwrap(),
        );
        let adapter = WindowsStoragePreflightAdapter::new();
        let context = adapter
            .observe_present_root(&locator)
            .expect("observe managed root");
        let bytes = b"production directory cache";
        let digest = sha256_hex(bytes);
        let manifest = super::super::DirectoryPayloadManifest {
            entries: vec![super::super::DirectoryPayloadManifestEntry {
                relative_segments: vec![digest.clone()],
                size: bytes.len() as u64,
                sha256: digest.clone(),
            }],
        };
        let mut lease = super::super::acquire_live_windows_runtime_writer_lease(&locator, &context)
            .expect("writer lease");

        lease
            .create_directory_staging(&manifest)
            .expect("directory staging");
        lease
            .materialize_verified_directory(&[super::super::DirectoryPayload {
                relative_segments: vec![digest.clone()],
                bytes,
            }])
            .expect("materialize directory");
        lease.seal_staged_directory().expect("seal staged tree");
        lease
            .promote_staged_directory(super::super::RuntimeStorageCommittedLeaf::Cache)
            .expect("promote committed cache");

        assert_eq!(
            std::fs::read(temporary.path().join("platform.cache").join(digest))
                .expect("committed cache bytes"),
            bytes
        );
        assert!(!temporary
            .path()
            .join(UNCOMMITTED_STAGING_DIRECTORY_NAME)
            .exists());
        assert!(!temporary
            .path()
            .join(UNCOMMITTED_STAGING_SESSION_MARKER_NAME)
            .exists());
    }

    #[test]
    #[ignore = "requires a writable NTFS D: volume; this unit-test harness uses the test-only in-memory signer and is not production keyring evidence"]
    fn d_root_managed_installations_logic_harness_uses_test_only_signer() {
        let temporary = tempfile::Builder::new()
            .prefix("lumina-managed-installations-production-")
            .tempdir_in(Path::new(r"D:\"))
            .expect("create isolated writable D: NTFS temporary root");
        for artifact in [
            MAIN_DB_NAME,
            PROVIDER_BINDING_NAME,
            KEYRING_REFERENCE_NAME,
            ANCHOR_NAME,
        ] {
            std::fs::write(temporary.path().join(artifact), b"committed")
                .expect("committed platform family artifact");
        }

        let managed_mcp_id = "managed_000010";
        let first = production_version_payloads(managed_mcp_id, "1.0.0", b"production v1");
        let first_manifest = production_manifest(&first);
        super::promote_verified_managed_installations(
            temporary.path(),
            &first_manifest,
            &first
                .iter()
                .map(|payload| super::super::DirectoryPayload {
                    relative_segments: payload.relative_segments.clone(),
                    bytes: &payload.bytes,
                })
                .collect::<Vec<_>>(),
        )
        .expect("production writer promotes the first anchored installation");

        let second = production_version_payloads(managed_mcp_id, "2.0.0", b"production v2");
        let second_manifest = production_manifest(&second);
        super::super::windows_managed_writer::inject_cleanup_failure_for_testing();
        super::super::windows_managed_writer::inject_cleanup_failure_for_testing();
        super::promote_verified_managed_installations(
            temporary.path(),
            &second_manifest,
            &second
                .iter()
                .map(|payload| super::super::DirectoryPayload {
                    relative_segments: payload.relative_segments.clone(),
                    bytes: &payload.bytes,
                })
                .collect::<Vec<_>>(),
        )
        .expect("production writer appends the second anchored installation");
        assert!(temporary
            .path()
            .join(UNCOMMITTED_STAGING_DIRECTORY_NAME)
            .is_dir());
        assert!(temporary
            .path()
            .join(UNCOMMITTED_STAGING_SESSION_MARKER_NAME)
            .is_file());
        assert!(super::managed_storage_verified_version_exists(
            temporary.path(),
            managed_mcp_id,
            "1.0.0"
        )
        .expect("first version verified"));
        assert!(!temporary
            .path()
            .join(UNCOMMITTED_STAGING_DIRECTORY_NAME)
            .exists());
        assert!(!temporary
            .path()
            .join(UNCOMMITTED_STAGING_SESSION_MARKER_NAME)
            .exists());
        assert!(super::managed_storage_verified_version_exists(
            temporary.path(),
            managed_mcp_id,
            "2.0.0"
        )
        .expect("second version verified"));

        let descriptor = |payloads: &[ProductionVersionPayload]| {
            payloads
                .iter()
                .find(|payload| {
                    payload.relative_segments.last().map(String::as_str)
                        == Some(".lumina-runtime-descriptor.json")
                })
                .expect("descriptor payload")
                .bytes
                .clone()
        };
        let first_descriptor = descriptor(&first);
        let second_descriptor = descriptor(&second);
        super::activate_verified_managed_installation(
            temporary.path(),
            managed_mcp_id,
            "1.0.0",
            &first_descriptor,
            &sha256_hex(&first_descriptor),
        )
        .expect("activate first version");
        super::activate_verified_managed_installation(
            temporary.path(),
            managed_mcp_id,
            "2.0.0",
            &second_descriptor,
            &sha256_hex(&second_descriptor),
        )
        .expect("activate second version");
        super::activate_verified_managed_installation(
            temporary.path(),
            managed_mcp_id,
            "1.0.0",
            &first_descriptor,
            &sha256_hex(&first_descriptor),
        )
        .expect("rollback to first version");
        super::clear_managed_installation_activation(temporary.path(), managed_mcp_id)
            .expect("clear active version");
        assert!(super::read_verified_managed_installation_activation(
            temporary.path(),
            managed_mcp_id
        )
        .expect("cleared active pointer")
        .is_none());
        super::activate_verified_managed_installation(
            temporary.path(),
            managed_mcp_id,
            "2.0.0",
            &second_descriptor,
            &sha256_hex(&second_descriptor),
        )
        .expect("reactivate second version");

        let tampered_first = production_version_payloads(managed_mcp_id, "1.0.0", b"tampered v1");
        for payload in &tampered_first {
            std::fs::write(
                temporary
                    .path()
                    .join("platform.installations")
                    .join(payload.relative_segments.join("\\")),
                &payload.bytes,
            )
            .expect("overwrite descriptor payload and evidence together");
        }
        assert!(super::managed_storage_verified_version_exists(
            temporary.path(),
            managed_mcp_id,
            "1.0.0"
        )
        .is_err());
        assert!(super::activate_verified_managed_installation(
            temporary.path(),
            managed_mcp_id,
            "1.0.0",
            &first_descriptor,
            &sha256_hex(&first_descriptor),
        )
        .is_err());
        assert!(super::read_verified_managed_installation_activation(
            temporary.path(),
            managed_mcp_id
        )
        .expect("untampered active version remains readable")
        .is_some());

        let tampered_second = production_version_payloads(managed_mcp_id, "2.0.0", b"tampered v2");
        for payload in &tampered_second {
            std::fs::write(
                temporary
                    .path()
                    .join("platform.installations")
                    .join(payload.relative_segments.join("\\")),
                &payload.bytes,
            )
            .expect("overwrite active descriptor payload and evidence together");
        }
        assert!(super::read_verified_managed_installation_activation(
            temporary.path(),
            managed_mcp_id
        )
        .is_err());
    }

    #[test]
    #[ignore = "requires a writable NTFS D: volume; run explicitly for production filesystem coverage"]
    fn unavailable_atomic_promote_rolls_back_the_handle_relative_staging_session() {
        let temporary = tempfile::Builder::new()
            .prefix("lumina-writer-promote-rollback-")
            .tempdir_in(Path::new(r"D:\"))
            .expect("create writable D: NTFS temporary root");
        let locator = PlatformLocator::from_selector(
            DSelectorRecord::new("writer-promote-rollback", 1, 1, temporary.path()).unwrap(),
        );
        for artifact in [
            MAIN_DB_NAME,
            PROVIDER_BINDING_NAME,
            KEYRING_REFERENCE_NAME,
            ANCHOR_NAME,
        ] {
            std::fs::write(temporary.path().join(artifact), b"committed").expect("family");
        }
        let adapter = WindowsStoragePreflightAdapter::new();
        let context = adapter
            .observe_present_root(&locator)
            .expect("observe managed root");
        let mut lease = super::super::acquire_live_windows_runtime_writer_lease(&locator, &context)
            .expect("writer lease");
        let bytes = b"production writer payload";
        let digest = sha256_hex(bytes);

        lease.create_staging().expect("staging");
        lease
            .write_verified_bytes(super::super::RuntimeStorageSlot::Payload, bytes, &digest)
            .expect("payload");
        lease
            .verify_regular_file(super::super::RuntimeStorageSlot::Payload)
            .expect("regular payload");

        let error = lease
            .atomic_promote()
            .expect_err("promotion is unavailable");
        assert_eq!(
            error.code(),
            McpPlatformErrorCode::RuntimeControlUnavailable
        );
        assert!(!temporary.path().join("lumina-staging").exists());
        assert!(!temporary.path().join("lumina-staging-session").exists());
        assert_eq!(
            adapter
                .begin_read_only_preflight_session(&locator)
                .expect("reopen after cleanup")
                .issue_read_only_request()
                .expect("request after cleanup")
                .consume()
                .expect("classification after cleanup"),
            PlatformStoragePreflightClassification::CommittedFamily
        );
    }

    #[test]
    #[ignore = "requires a writable NTFS D: volume; run explicitly for production filesystem coverage"]
    fn production_preflight_quarantines_uncommitted_staging_before_writer_lease() {
        let temporary = tempfile::Builder::new()
            .prefix("lumina-uncommitted-residual-")
            .tempdir_in(Path::new(r"D:\"))
            .expect("create writable D: NTFS temporary root");
        for artifact in [
            MAIN_DB_NAME,
            PROVIDER_BINDING_NAME,
            KEYRING_REFERENCE_NAME,
            ANCHOR_NAME,
        ] {
            std::fs::write(temporary.path().join(artifact), b"committed").expect("family");
        }
        std::fs::create_dir(temporary.path().join(UNCOMMITTED_STAGING_DIRECTORY_NAME))
            .expect("foreign staging");
        let locator = PlatformLocator::from_selector(
            DSelectorRecord::new("uncommitted-residual", 1, 1, temporary.path()).unwrap(),
        );
        let adapter = WindowsStoragePreflightAdapter::new();
        let context = adapter
            .observe_present_root(&locator)
            .expect("observe production root");

        assert_eq!(
            adapter
                .begin_read_only_preflight_session(&locator)
                .expect("preflight session")
                .issue_read_only_request()
                .expect("preflight request")
                .consume()
                .expect("preflight classification"),
            PlatformStoragePreflightClassification::ResidualPresent
        );
        let validation = validate_managed_storage_locator_with_preflight(&locator, &adapter)
            .expect_err("uncommitted staging must require repair");
        assert_eq!(
            validation.code(),
            McpPlatformErrorCode::IntegrityUnavailable
        );
        assert_eq!(validation.message(), "storage_recovery_required");
        let lease = super::super::acquire_live_windows_runtime_writer_lease(&locator, &context)
            .expect_err("writer lease must not adopt foreign staging");
        assert_eq!(lease.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert_eq!(lease.message(), "storage_recovery_required");
    }

    #[test]
    #[ignore = "requires a writable NTFS D: volume; run explicitly for production filesystem coverage"]
    fn malformed_or_reparse_uncommitted_names_fail_closed_in_a_production_root() {
        let temporary = tempfile::Builder::new()
            .prefix("lumina-uncommitted-shapes-")
            .tempdir_in(Path::new(r"D:\"))
            .expect("create writable D: NTFS temporary root");
        for artifact in [
            MAIN_DB_NAME,
            PROVIDER_BINDING_NAME,
            KEYRING_REFERENCE_NAME,
            ANCHOR_NAME,
        ] {
            std::fs::write(temporary.path().join(artifact), b"committed").expect("family");
        }
        std::fs::create_dir(
            temporary
                .path()
                .join(UNCOMMITTED_STAGING_SESSION_MARKER_NAME),
        )
        .expect("malformed marker");
        let locator = PlatformLocator::from_selector(
            DSelectorRecord::new("uncommitted-shapes", 1, 1, temporary.path()).unwrap(),
        );
        let adapter = WindowsStoragePreflightAdapter::new();
        adapter
            .observe_present_root(&locator)
            .expect("observe production root");
        let malformed = validate_managed_storage_locator_with_preflight(&locator, &adapter)
            .expect_err("malformed marker must require repair");
        assert_eq!(malformed.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert_eq!(malformed.message(), "storage_recovery_required");

        std::fs::remove_dir(
            temporary
                .path()
                .join(UNCOMMITTED_STAGING_SESSION_MARKER_NAME),
        )
        .expect("remove malformed marker");
        let reparse_target = tempfile::tempdir_in(Path::new(r"D:\")).expect("reparse target");
        let reparse = temporary.path().join(UNCOMMITTED_STAGING_DIRECTORY_NAME);
        std::os::windows::fs::symlink_dir(reparse_target.path(), &reparse)
            .expect("create staging reparse point");
        let reparse_error = validate_managed_storage_locator_with_preflight(&locator, &adapter)
            .expect_err("staging reparse point must fail closed");
        assert_eq!(
            reparse_error.code(),
            McpPlatformErrorCode::IntegrityUnavailable
        );
        assert_eq!(reparse_error.message(), "storage_recovery_required");
    }

    #[test]
    fn committed_managed_distribution_layout_accepts_only_regular_cache_and_installations() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let cache = temporary.path().join(CACHE_ARTIFACT_NAME);
        let installations = temporary.path().join(INSTALLATIONS_ARTIFACT_NAME);
        fs::create_dir(&cache).expect("cache");
        assert!(managed_cache_layout_is_committed(&cache).expect("empty cache support layout"));
        fs::write(cache.join("a".repeat(64)), b"artifact").expect("cached artifact");
        fs::create_dir(&installations).expect("installations");
        assert!(!managed_installations_layout_is_committed(&installations)
            .expect("empty installations are not an anchored layout"));
        let installation = installations.join("managed-id");
        fs::create_dir(&installation).expect("installation");
        assert!(!managed_installations_layout_is_committed(&installations)
            .expect("uncommitted installation layout"));
        fs::create_dir(installation.join("versions")).expect("versions");
        assert!(!managed_installations_layout_is_committed(&installations)
            .expect("uncommitted versions layout"));
        let version = installation.join("versions").join("1.0.0");
        fs::create_dir(&version).expect("version");
        assert!(!managed_installations_layout_is_committed(&installations)
            .expect("uncommitted version layout"));
        fs::write(version.join("server.exe"), b"committed").expect("payload");

        assert!(managed_cache_layout_is_committed(&cache).expect("cache layout"));
        assert!(managed_installations_layout_is_committed(&installations)
            .expect("installations layout"));

        fs::write(cache.join("partial"), b"residual").expect("partial cache");
        assert!(!managed_cache_layout_is_committed(&cache).expect("residual cache layout"));
    }

    #[test]
    fn normal_windows_path_casing_is_accepted_but_short_names_are_not() {
        assert!(windows_normal_path_case_match(
            r"d:\Lumina\Managed",
            r"D:\lumina\managed"
        ));
        assert!(!windows_normal_path_case_match(
            r"D:\LUMINA~1\Managed",
            r"D:\Lumina Managed\Managed"
        ));
    }

    #[test]
    #[ignore = "requires a writable NTFS D: volume; run explicitly for production filesystem coverage"]
    fn production_writer_acquisition_rejects_a_replaced_observed_root_before_writing() {
        let temporary = tempfile::Builder::new()
            .prefix("lumina-writer-drift-")
            .tempdir_in(Path::new(r"D:\"))
            .expect("create writable D: NTFS temporary root");
        let root = temporary.path().to_path_buf();
        let locator = PlatformLocator::from_selector(
            DSelectorRecord::new("writer-root-drift", 1, 1, &root).unwrap(),
        );
        let adapter = WindowsStoragePreflightAdapter::new();
        let context = adapter
            .observe_present_root(&locator)
            .expect("observe managed root");
        let moved = root.with_extension("observed-root");
        std::fs::rename(&root, &moved).expect("move observed root");
        std::fs::create_dir(&root).expect("replacement root");

        assert!(
            super::super::acquire_live_windows_runtime_writer_lease(&locator, &context).is_err()
        );
        assert!(!root.join("lumina-staging").exists());
        assert!(!moved.join("lumina-staging").exists());

        std::fs::remove_dir(&root).expect("remove replacement root");
        std::fs::rename(&moved, &root).expect("restore temporary root");
    }

    #[test]
    #[ignore = "requires a writable NTFS D: volume; run explicitly for production filesystem coverage"]
    fn managed_read_root_fails_closed_when_the_preflighted_root_is_replaced() {
        let temporary = tempfile::Builder::new()
            .prefix("lumina-managed-read-drift-")
            .tempdir_in(Path::new(r"D:\"))
            .expect("create writable D: NTFS temporary root");
        let root = temporary.path().to_path_buf();
        for artifact in [
            MAIN_DB_NAME,
            PROVIDER_BINDING_NAME,
            KEYRING_REFERENCE_NAME,
            ANCHOR_NAME,
        ] {
            std::fs::write(root.join(artifact), b"committed").expect("committed root artifact");
        }
        let version = root
            .join("installations")
            .join("managed_000001")
            .join("versions")
            .join("1.0.0");
        std::fs::create_dir_all(&version).expect("nested managed version");
        std::fs::write(version.join("server.exe"), b"preflighted object").expect("managed payload");

        let observed = root.with_extension("preflight-observed");
        let replacement_marker = root.join("replacement-marker");
        let error = acquire_managed_read_root_after_preflight(&root, || {
            std::fs::rename(&root, &observed).expect("move preflighted root");
            std::fs::create_dir(&root).expect("replacement root");
            std::fs::write(&replacement_marker, b"must never be read").expect("replacement marker");
        })
        .expect_err("replacement between preflight and read must fail closed");
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(
            std::fs::read(&replacement_marker).expect("replacement marker remains untouched"),
            b"must never be read"
        );

        std::fs::remove_dir_all(&root).expect("remove replacement root");
        std::fs::rename(&observed, &root).expect("restore preflighted root");
    }

    struct ScriptedObserver {
        facts: Option<ReadOnlyStoragePreflightFacts>,
        error: Option<McpPlatformError>,
    }

    impl ReadOnlyStoragePreflightObserver for ScriptedObserver {
        fn observe_read_only_preflight(
            &self,
            _locator: &PlatformLocator,
            _context: &PlatformStorageContext,
        ) -> McpPlatformResult<ReadOnlyStoragePreflightFacts> {
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            Ok(self.facts.expect("scripted facts must exist"))
        }
    }

    struct ScriptedLiveHandlePreflight {
        locator: PlatformLocator,
        context: PlatformStorageContext,
        observer: Arc<dyn ReadOnlyStoragePreflightObserver>,
    }

    impl PlatformLiveHandlePreflight for ScriptedLiveHandlePreflight {
        fn observe_absent_root(
            &self,
            locator: &PlatformLocator,
        ) -> McpPlatformResult<PlatformStorageContext> {
            if self.context.root_state() == PlatformRootState::Absent
                && self.context.matches_locator(locator)
            {
                Ok(self.context.clone())
            } else {
                Err(integrity_error(
                    "windows read-only preflight root state drifted",
                ))
            }
        }

        fn observe_present_root(
            &self,
            locator: &PlatformLocator,
        ) -> McpPlatformResult<PlatformStorageContext> {
            if self.context.root_state() == PlatformRootState::Present
                && self.context.matches_locator(locator)
            {
                Ok(self.context.clone())
            } else {
                Err(integrity_error(
                    "windows read-only preflight root state drifted",
                ))
            }
        }

        fn begin_read_only_preflight_session(
            &self,
            locator: &PlatformLocator,
        ) -> McpPlatformResult<PreflightSession> {
            PreflightSession::new(locator, &self.context, Arc::clone(&self.observer))
        }
    }

    fn selector(root: &str) -> PlatformLocator {
        PlatformLocator::from_selector(
            DSelectorRecord::new("selector-1", 1, 9, PathBuf::from(root)).unwrap(),
        )
    }

    fn absent_context(locator: &PlatformLocator) -> PlatformStorageContext {
        PlatformStorageContext::for_absent_root(
            locator.clone(),
            LiveVolumeIdentity::new("volume-secret").unwrap(),
            LiveAncestorIdentity::new("ancestor-secret").unwrap(),
            LiveObservationEpoch::new(7).unwrap(),
        )
    }

    fn present_context(locator: &PlatformLocator) -> PlatformStorageContext {
        PlatformStorageContext::for_present_root(
            locator.clone(),
            LiveVolumeIdentity::new("volume-secret").unwrap(),
            LiveRootIdentity::new("root-secret").unwrap(),
            LiveObservationEpoch::new(8).unwrap(),
        )
    }

    fn scripted_session(
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
        facts: ReadOnlyStoragePreflightFacts,
    ) -> PreflightSession {
        let live = ScriptedLiveHandlePreflight {
            locator: locator.clone(),
            context: context.clone(),
            observer: Arc::new(ScriptedObserver {
                facts: Some(facts),
                error: None,
            }),
        };
        live.begin_read_only_preflight_session(locator).unwrap()
    }

    #[test]
    fn facts_only_chain_classifies_absent_empty_and_nonempty_roots() {
        let absent_locator = selector(r"D:\family\absent");
        let absent_session = scripted_session(
            &absent_locator,
            &absent_context(&absent_locator),
            ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Absent,
                PlatformStorageFamilyObservation::empty(),
            ),
        );
        let empty_locator = selector(r"D:\family\empty");
        let empty_session = scripted_session(
            &empty_locator,
            &present_context(&empty_locator),
            ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::PhysicallyEmpty,
                PlatformStorageFamilyObservation::empty(),
            ),
        );
        let nonempty_locator = selector(r"D:\family\nonempty");
        let nonempty_session = scripted_session(
            &nonempty_locator,
            &present_context(&nonempty_locator),
            ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty(),
            ),
        );

        assert_eq!(
            absent_session
                .issue_read_only_request()
                .unwrap()
                .consume()
                .unwrap(),
            PlatformStoragePreflightClassification::RootAbsent
        );
        assert_eq!(
            empty_session
                .issue_read_only_request()
                .unwrap()
                .consume()
                .unwrap(),
            PlatformStoragePreflightClassification::RootPhysicallyEmpty
        );
        assert_eq!(
            nonempty_session
                .issue_read_only_request()
                .unwrap()
                .consume()
                .unwrap(),
            PlatformStoragePreflightClassification::RootNonempty
        );
    }

    #[test]
    fn session_and_request_are_single_use_capabilities() {
        let locator = selector(r"D:\family\single-use");
        let session = scripted_session(
            &locator,
            &present_context(&locator),
            ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty(),
            ),
        );

        let request = session.issue_read_only_request().unwrap();
        assert_eq!(
            request.consume().unwrap(),
            PlatformStoragePreflightClassification::RootNonempty
        );
        assert_eq!(
            request.consume().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            session.issue_read_only_request().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    #[test]
    fn scripted_windows_contract_covers_surface_and_integrity_fail_closed_states() {
        let locator = selector(r"D:\family\states");
        let context = present_context(&locator);
        let states = [
            (
                ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::HiddenOrSystemPresent,
                    PlatformStorageFamilyObservation::empty(),
                ),
                PlatformStoragePreflightClassification::RootHiddenOrSystemPresent,
            ),
            (
                ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::Reparse,
                    PlatformStorageFamilyObservation::empty(),
                ),
                PlatformStoragePreflightClassification::RootReparse,
            ),
            (
                ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::AdsPresent,
                    PlatformStorageFamilyObservation::empty(),
                ),
                PlatformStoragePreflightClassification::RootAdsPresent,
            ),
            (
                ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::HardlinkAlias,
                    PlatformStorageFamilyObservation::empty(),
                ),
                PlatformStoragePreflightClassification::RootHardlinkAlias,
            ),
            (
                ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::Inaccessible,
                    PlatformStorageFamilyObservation::empty(),
                ),
                PlatformStoragePreflightClassification::RootInaccessible,
            ),
            (
                ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::IdentityMismatch,
                    PlatformStorageFamilyObservation::empty(),
                ),
                PlatformStoragePreflightClassification::RootIdentityMismatch,
            ),
        ];

        for (facts, expected) in states {
            let session = scripted_session(&locator, &context, facts);
            assert_eq!(
                session
                    .issue_read_only_request()
                    .unwrap()
                    .consume()
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn scripted_windows_contract_covers_forbidden_sidecars_and_lock_states() {
        let locator = selector(r"D:\family\sidecars");
        let context = present_context(&locator);
        let residual = scripted_session(
            &locator,
            &context,
            ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty().with_forbidden(
                    ForbiddenFamilyArtifacts::none()
                        .with_unknown_sidecar()
                        .with_cache_artifact(),
                ),
            ),
        );
        let released = scripted_session(
            &locator,
            &context,
            ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty().with_released_sidecar(),
            ),
        );
        let locked = scripted_session(
            &locator,
            &context,
            ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty()
                    .with_released_sidecar()
                    .with_live_exclusive_lock(),
            ),
        );

        assert_eq!(
            residual
                .issue_read_only_request()
                .unwrap()
                .consume()
                .unwrap(),
            PlatformStoragePreflightClassification::ResidualPresent
        );
        assert_eq!(
            released
                .issue_read_only_request()
                .unwrap()
                .consume()
                .unwrap(),
            PlatformStoragePreflightClassification::ReleasedSidecar
        );
        assert_eq!(
            locked.issue_read_only_request().unwrap().consume().unwrap(),
            PlatformStoragePreflightClassification::Locked
        );
    }

    #[test]
    fn managed_storage_root_validation_allows_fresh_and_committed_windows_roots() {
        let absent_locator = selector(r"D:\family\managed-absent");
        let empty_locator = selector(r"D:\family\managed-empty");
        let committed_locator = selector(r"D:\family\managed-committed");

        let absent = ScriptedLiveHandlePreflight {
            locator: absent_locator.clone(),
            context: absent_context(&absent_locator),
            observer: Arc::new(ScriptedObserver {
                facts: Some(ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::Absent,
                    PlatformStorageFamilyObservation::empty(),
                )),
                error: None,
            }),
        };
        let empty = ScriptedLiveHandlePreflight {
            locator: empty_locator.clone(),
            context: present_context(&empty_locator),
            observer: Arc::new(ScriptedObserver {
                facts: Some(ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::PhysicallyEmpty,
                    PlatformStorageFamilyObservation::empty(),
                )),
                error: None,
            }),
        };
        let committed = ScriptedLiveHandlePreflight {
            locator: committed_locator.clone(),
            context: present_context(&committed_locator),
            observer: Arc::new(ScriptedObserver {
                facts: Some(ReadOnlyStoragePreflightFacts::new(
                    ObservedRootSurface::Nonempty,
                    PlatformStorageFamilyObservation::empty().with_required(
                        RequiredFamilyArtifacts::none()
                            .with_main_db()
                            .with_provider_binding()
                            .with_keyring_reference()
                            .with_anchor(),
                    ),
                )),
                error: None,
            }),
        };

        assert!(validate_managed_storage_locator_with_preflight(&absent_locator, &absent,).is_ok());
        assert!(validate_managed_storage_locator_with_preflight(&empty_locator, &empty,).is_ok());
        assert!(
            validate_managed_storage_locator_with_preflight(&committed_locator, &committed,)
                .is_ok()
        );
    }

    #[test]
    fn managed_storage_root_validation_fail_closes_on_nonempty_or_reparse_roots() {
        let locator = selector(r"D:\family\managed-blocked");
        let context = present_context(&locator);
        let blocked_states = [
            ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Nonempty,
                PlatformStorageFamilyObservation::empty(),
            ),
            ReadOnlyStoragePreflightFacts::new(
                ObservedRootSurface::Reparse,
                PlatformStorageFamilyObservation::empty(),
            ),
        ];

        for facts in blocked_states {
            let preflight = ScriptedLiveHandlePreflight {
                locator: locator.clone(),
                context: context.clone(),
                observer: Arc::new(ScriptedObserver {
                    facts: Some(facts),
                    error: None,
                }),
            };

            let error =
                validate_managed_storage_locator_with_preflight(&locator, &preflight).unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        }
    }

    #[test]
    fn scripted_windows_contract_covers_child_acl_toctou_and_unsupported_fs_fail_closed() {
        let locator = selector(r"D:\family\fail-closed");
        let context = present_context(&locator);
        let child_acl = ScriptedLiveHandlePreflight {
            locator: locator.clone(),
            context: context.clone(),
            observer: Arc::new(ScriptedObserver {
                facts: None,
                error: Some(McpPlatformError::new(
                    McpPlatformErrorCode::IntegrityUnavailable,
                    "windows read-only preflight evidence is unavailable",
                )),
            }),
        };
        let session = child_acl
            .begin_read_only_preflight_session(&locator)
            .unwrap();
        assert_eq!(
            session
                .issue_read_only_request()
                .unwrap()
                .consume()
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityUnavailable
        );

        let drift = ScriptedLiveHandlePreflight {
            locator: locator.clone(),
            context: context.clone(),
            observer: Arc::new(ScriptedObserver {
                facts: None,
                error: Some(McpPlatformError::new(
                    McpPlatformErrorCode::IntegrityError,
                    "windows read-only preflight root drifted",
                )),
            }),
        };
        let drift_session = drift.begin_read_only_preflight_session(&locator).unwrap();
        assert_eq!(
            drift_session
                .issue_read_only_request()
                .unwrap()
                .consume()
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    #[test]
    fn real_adapter_rejects_unsupported_selector_shapes_before_io() {
        let adapter = WindowsStoragePreflightAdapter::new();
        let invalid_roots = [
            r"D:relative",
            r"\\server\share\root",
            r"\\?\D:\root",
            r"D:\root:ads",
            r"D:\trailing-dot.",
            r"D:\trailing-space ",
        ];

        for root in invalid_roots {
            let locator = selector(root);
            let error = adapter
                .begin_read_only_preflight_session(&locator)
                .unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        }
    }

    fn capacity_binding(
        volume_serial: u32,
        file_index: u32,
        final_path: &str,
        attributes: u32,
    ) -> CapacityBinding {
        CapacityBinding {
            root_identity: FileIdentity {
                volume_serial,
                file_index_high: 0,
                file_index_low: file_index,
            },
            expected_volume_identity: FileIdentity {
                volume_serial,
                file_index_high: 0,
                file_index_low: 1,
            },
            expected_final_path: final_path.to_string(),
            root_attributes: attributes,
        }
    }

    #[test]
    fn capacity_witness_fake_rejects_volume_mismatch_reparse_and_post_query_drift() {
        let lexical = LexicalSelectorPath::parse(Path::new(r"D:\managed")).unwrap();
        let stable = capacity_binding(7, 9, r"D:\managed", 0);
        validate_capacity_binding(&lexical, &stable).unwrap();

        let mut mounted_volume = stable.clone();
        mounted_volume.root_identity.volume_serial = 8;
        assert_eq!(
            validate_capacity_binding(&lexical, &mounted_volume)
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityUnavailable
        );

        let reparse = capacity_binding(7, 9, r"D:\managed", FILE_ATTRIBUTE_REPARSE_POINT);
        assert_eq!(
            validate_capacity_binding(&lexical, &reparse)
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityUnavailable
        );

        for drifted in [
            capacity_binding(7, 10, r"D:\managed", 0),
            capacity_binding(7, 9, r"D:\replacement", 0),
            capacity_binding(7, 9, r"D:\managed", FILE_ATTRIBUTE_HIDDEN),
        ] {
            assert_eq!(
                ensure_capacity_binding_stable(&stable, &drifted)
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::IntegrityError
            );
        }
    }

    #[test]
    fn capacity_path_gate_rejects_unc_non_d_and_ads_before_platform_io() {
        for invalid in [r"C:\managed", r"\\server\share\managed", r"D:\managed:ads"] {
            assert_eq!(
                LexicalSelectorPath::parse(Path::new(invalid))
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::IntegrityUnavailable
            );
        }
    }

    #[test]
    fn production_capacity_helper_queries_handle_final_path_then_revalidates() {
        let source = include_str!("windows_storage_preflight.rs");
        let start = source
            .find("pub(super) fn managed_storage_available_bytes")
            .unwrap();
        let end = source[start..]
            .find("fn validate_managed_storage_locator_with_preflight")
            .unwrap()
            + start;
        let helper = &source[start..end];
        let query = helper.find("GetDiskFreeSpaceExW").unwrap();
        let revalidate = helper
            .find("revalidate_capacity_witness(&witness)?")
            .unwrap();
        assert!(helper.contains("wide(&witness.binding.expected_final_path)"));
        assert!(query < revalidate);
    }

    #[test]
    fn real_d_root_capacity_check_is_available_only_when_the_host_exposes_fixed_ntfs_d() {
        let result = managed_storage_available_bytes(Path::new(r"D:\"));
        match result {
            Ok(_) => {}
            Err(error) => assert!(matches!(
                error.code(),
                McpPlatformErrorCode::IntegrityUnavailable
                    | McpPlatformErrorCode::IntegrityError
                    | McpPlatformErrorCode::NotFound
            )),
        }
    }

    #[test]
    fn real_adapter_contract_path_can_be_exercised_without_masking_real_integration() {
        let locator = selector(r"D:\real-adapter\contract");
        let adapter = WindowsStoragePreflightAdapter::new();

        let result = adapter.observe_absent_root(&locator);
        match result {
            Ok(context) => {
                assert_eq!(context.root_state(), PlatformRootState::Absent);
            }
            Err(error) => {
                assert!(
                    matches!(
                        error.code(),
                        McpPlatformErrorCode::IntegrityUnavailable
                            | McpPlatformErrorCode::NotFound
                            | McpPlatformErrorCode::IntegrityError
                    ),
                    "real adapter should either observe an absent D: root or fail closed"
                );
            }
        }
    }

    #[test]
    fn absent_root_probe_still_absent_confirms_absence() {
        assert_eq!(
            classify_absent_root_probe_result(RootPathProbe::NotFound),
            Ok(())
        );
    }

    #[test]
    fn absent_root_probe_root_reappears_fails_closed_without_path_leak() {
        let error = classify_absent_root_probe_result(RootPathProbe::Present).unwrap_err();

        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert_eq!(error.message(), "windows read-only preflight root drifted");
        assert!(!error.message().contains(r"D:\"));
        assert!(!error.message().contains("root-secret"));
    }

    #[test]
    fn absent_root_probe_unconfirmable_is_integrity_unavailable_without_path_leak() {
        let error = classify_absent_root_probe_result(RootPathProbe::Unavailable).unwrap_err();

        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert_eq!(
            error.message(),
            "windows read-only preflight evidence is unavailable"
        );
        assert!(!error.message().contains(r"D:\"));
        assert!(!error.message().contains("ancestor-secret"));
    }

    #[test]
    fn absent_root_revalidation_source_does_not_use_exists_shortcut() {
        let production_source = include_str!("windows_storage_preflight.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("windows_storage_preflight.rs must contain production source");
        assert!(!production_source.contains(".exists()"));
    }
}
