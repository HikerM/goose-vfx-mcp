//! Internal sealed storage-domain foundation for future `d_root` batches.
//!
//! Source-level regression signals live here, but a future compile-fail
//! harness still owns full negative API proof instead of doctest claims.

use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::error::{McpPlatformError, McpPlatformResult};

#[path = "storage_preflight.rs"]
mod preflight;
use preflight::PreflightSession;

#[cfg(windows)]
#[path = "windows_storage_preflight.rs"]
mod windows_authority;

#[cfg(windows)]
#[path = "windows_managed_writer.rs"]
mod windows_managed_writer;

#[cfg(windows)]
pub(crate) fn validate_managed_storage_root(root: &Path) -> McpPlatformResult<()> {
    windows_authority::validate_managed_storage_root(root)
}

/// Returns capacity only after the Windows authority has rebound the supplied
/// managed root to a local D: volume. The caller never supplies a volume or a
/// path to probe beyond the production adapter's private root.
#[cfg(windows)]
pub(crate) fn managed_storage_available_bytes(root: &Path) -> McpPlatformResult<u64> {
    windows_authority::managed_storage_available_bytes(root)
}

/// Reads metadata only after the current version has been proven present in
/// the signed managed-installation anchor. The Windows authority rechecks the
/// root binding after the read so callers cannot treat a physical final tree as
/// committed merely because its layout looks valid.
#[cfg(windows)]
pub(crate) fn read_anchored_managed_installation_metadata(
    root: &Path,
    managed_mcp_id: &str,
    version: &str,
    leaf: &str,
    maximum: u64,
) -> McpPlatformResult<Option<Vec<u8>>> {
    windows_authority::read_anchored_managed_installation_metadata(
        root,
        managed_mcp_id,
        version,
        leaf,
        maximum,
    )
}

#[cfg(windows)]
pub(crate) fn managed_storage_verified_version_exists(
    root: &Path,
    managed_mcp_id: &str,
    version: &str,
) -> McpPlatformResult<bool> {
    windows_authority::managed_storage_verified_version_exists(root, managed_mcp_id, version)
}

#[cfg(windows)]
pub(crate) fn read_verified_managed_installation_activation(
    root: &Path,
    managed_mcp_id: &str,
) -> McpPlatformResult<Option<Vec<u8>>> {
    windows_authority::read_verified_managed_installation_activation(root, managed_mcp_id)
}

/// Commits the first sealed managed-installations tree below a preflight
/// verified Windows managed root.  The caller supplies only an already closed
/// manifest and the bytes it names; no path or writer capability escapes this
/// module.
#[cfg(windows)]
pub(crate) fn promote_verified_managed_installations(
    root: &Path,
    manifest: &DirectoryPayloadManifest,
    payloads: &[DirectoryPayload<'_>],
) -> McpPlatformResult<()> {
    windows_authority::promote_verified_managed_installations(root, manifest, payloads)
}

#[cfg(windows)]
pub(crate) fn activate_verified_managed_installation(
    root: &Path,
    managed_mcp_id: &str,
    version: &str,
    descriptor: &[u8],
    descriptor_sha256: &str,
) -> McpPlatformResult<()> {
    windows_authority::activate_verified_managed_installation(
        root,
        managed_mcp_id,
        version,
        descriptor,
        descriptor_sha256,
    )
}

#[cfg(windows)]
pub(crate) fn clear_managed_installation_activation(
    root: &Path,
    managed_mcp_id: &str,
) -> McpPlatformResult<()> {
    windows_authority::clear_managed_installation_activation(root, managed_mcp_id)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct LiveObservationEpoch(NonZeroU64);

impl LiveObservationEpoch {
    fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Debug for LiveObservationEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("LiveObservationEpoch")
            .field(&self.get())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct LiveVolumeIdentity {
    opaque: String,
}

impl LiveVolumeIdentity {
    fn new(opaque: impl Into<String>) -> Option<Self> {
        let opaque = opaque.into();
        (!opaque.is_empty()).then_some(Self { opaque })
    }
}

impl fmt::Debug for LiveVolumeIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LiveVolumeIdentity([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct LiveAncestorIdentity {
    opaque: String,
}

impl LiveAncestorIdentity {
    fn new(opaque: impl Into<String>) -> Option<Self> {
        let opaque = opaque.into();
        (!opaque.is_empty()).then_some(Self { opaque })
    }
}

impl fmt::Debug for LiveAncestorIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LiveAncestorIdentity([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct LiveRootIdentity {
    opaque: String,
}

impl LiveRootIdentity {
    fn new(opaque: impl Into<String>) -> Option<Self> {
        let opaque = opaque.into();
        (!opaque.is_empty()).then_some(Self { opaque })
    }
}

impl fmt::Debug for LiveRootIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LiveRootIdentity([REDACTED])")
    }
}

/// Opaque, non-serializable evidence for the observed root surface.  The
/// Windows authority binds the canonical final name and attribute word into
/// this value so a later writer acquisition cannot silently substitute a root
/// with the same file identity but a different surface.
#[derive(Clone, PartialEq, Eq, Hash)]
struct LiveRootSurfaceEvidence {
    opaque: String,
}

impl LiveRootSurfaceEvidence {
    fn new(opaque: impl Into<String>) -> Option<Self> {
        let opaque = opaque.into();
        (!opaque.is_empty()).then_some(Self { opaque })
    }
}

impl fmt::Debug for LiveRootSurfaceEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LiveRootSurfaceEvidence([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PlatformRootState {
    Absent,
    Present,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SelectorKind {
    DRoot,
}

#[derive(Clone, PartialEq, Eq)]
struct DSelectorRecord {
    kind: SelectorKind,
    selector_id: String,
    schema_version: u32,
    selector_revision: u64,
    root_path: PathBuf,
}

impl DSelectorRecord {
    fn new(
        selector_id: impl Into<String>,
        schema_version: u32,
        selector_revision: u64,
        root_path: impl Into<PathBuf>,
    ) -> Option<Self> {
        let selector_id = selector_id.into();
        let root_path = root_path.into();
        if selector_id.is_empty() || root_path.as_os_str().is_empty() {
            return None;
        }
        Some(Self {
            kind: SelectorKind::DRoot,
            selector_id,
            schema_version,
            selector_revision,
            root_path,
        })
    }

    fn root_path(&self) -> &Path {
        &self.root_path
    }

    fn schema_version(&self) -> u32 {
        self.schema_version
    }

    fn selector_revision(&self) -> u64 {
        self.selector_revision
    }

    fn same_selector(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.selector_id == other.selector_id
            && self.schema_version == other.schema_version
            && self.selector_revision == other.selector_revision
            && self.root_path == other.root_path
    }
}

impl fmt::Debug for DSelectorRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DSelectorRecord")
            .field("kind", &self.kind)
            .field("selector_id", &"[REDACTED]")
            .field("schema_version", &self.schema_version)
            .field("selector_revision", &self.selector_revision)
            .field("root_path", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PlatformLocator {
    selector: DSelectorRecord,
}

impl PlatformLocator {
    fn from_selector(selector: DSelectorRecord) -> Self {
        Self { selector }
    }

    fn selector(&self) -> &DSelectorRecord {
        &self.selector
    }

    fn root_path(&self) -> &Path {
        self.selector.root_path()
    }
}

impl fmt::Debug for PlatformLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformLocator")
            .field("selector", &self.selector)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PlatformStorageContext {
    locator: PlatformLocator,
    root_state: PlatformRootState,
    volume_identity: LiveVolumeIdentity,
    ancestor_identity: Option<LiveAncestorIdentity>,
    root_identity: Option<LiveRootIdentity>,
    root_surface_evidence: Option<LiveRootSurfaceEvidence>,
    observation_epoch: LiveObservationEpoch,
}

impl PlatformStorageContext {
    fn for_absent_root(
        locator: PlatformLocator,
        volume_identity: LiveVolumeIdentity,
        ancestor_identity: LiveAncestorIdentity,
        observation_epoch: LiveObservationEpoch,
    ) -> Self {
        Self {
            locator,
            root_state: PlatformRootState::Absent,
            volume_identity,
            ancestor_identity: Some(ancestor_identity),
            root_identity: None,
            root_surface_evidence: None,
            observation_epoch,
        }
    }

    fn for_present_root(
        locator: PlatformLocator,
        volume_identity: LiveVolumeIdentity,
        root_identity: LiveRootIdentity,
        observation_epoch: LiveObservationEpoch,
    ) -> Self {
        Self {
            locator,
            root_state: PlatformRootState::Present,
            volume_identity,
            ancestor_identity: None,
            root_identity: Some(root_identity),
            root_surface_evidence: None,
            observation_epoch,
        }
    }

    fn for_present_root_with_surface(
        locator: PlatformLocator,
        volume_identity: LiveVolumeIdentity,
        root_identity: LiveRootIdentity,
        root_surface_evidence: LiveRootSurfaceEvidence,
        observation_epoch: LiveObservationEpoch,
    ) -> Self {
        Self {
            locator,
            root_state: PlatformRootState::Present,
            volume_identity,
            ancestor_identity: None,
            root_identity: Some(root_identity),
            root_surface_evidence: Some(root_surface_evidence),
            observation_epoch,
        }
    }

    fn locator(&self) -> &PlatformLocator {
        &self.locator
    }

    fn root_state(&self) -> PlatformRootState {
        self.root_state
    }

    fn volume_identity(&self) -> &LiveVolumeIdentity {
        &self.volume_identity
    }

    fn ancestor_identity(&self) -> Option<&LiveAncestorIdentity> {
        self.ancestor_identity.as_ref()
    }

    fn root_identity(&self) -> Option<&LiveRootIdentity> {
        self.root_identity.as_ref()
    }

    fn root_surface_evidence(&self) -> Option<&LiveRootSurfaceEvidence> {
        self.root_surface_evidence.as_ref()
    }

    fn observation_epoch(&self) -> LiveObservationEpoch {
        self.observation_epoch
    }

    fn matches_locator(&self, locator: &PlatformLocator) -> bool {
        self.locator.selector.same_selector(locator.selector())
    }
}

impl fmt::Debug for PlatformStorageContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let root_identity = self.root_identity.as_ref().map(|_| "[REDACTED]");
        let root_surface_evidence = self.root_surface_evidence.as_ref().map(|_| "[REDACTED]");
        let ancestor_identity = self.ancestor_identity.as_ref().map(|_| "[REDACTED]");
        formatter
            .debug_struct("PlatformStorageContext")
            .field("locator", &self.locator)
            .field("root_state", &self.root_state)
            .field("volume_identity", &self.volume_identity)
            .field("ancestor_identity", &ancestor_identity)
            .field("root_identity", &root_identity)
            .field("root_surface_evidence", &root_surface_evidence)
            .field("observation_epoch", &self.observation_epoch)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PlatformBlockedReason {
    ApprovalRequired,
    ApprovalExpired,
    ApprovalCancelled,
    ApprovalAlreadyConsumed,
    ApprovalIntentMismatch,
    RootIdentityMismatch,
    SelectorMismatch,
    VolumeIdentityMismatch,
    AncestorIdentityMismatch,
    ObservationEpochChanged,
    ContextStateMismatch,
}

impl PlatformBlockedReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "approval_required",
            Self::ApprovalExpired => "approval_expired",
            Self::ApprovalCancelled => "approval_cancelled",
            Self::ApprovalAlreadyConsumed => "approval_already_consumed",
            Self::ApprovalIntentMismatch => "approval_intent_mismatch",
            Self::RootIdentityMismatch => "root_identity_mismatch",
            Self::SelectorMismatch => "selector_mismatch",
            Self::VolumeIdentityMismatch => "volume_identity_mismatch",
            Self::AncestorIdentityMismatch => "ancestor_identity_mismatch",
            Self::ObservationEpochChanged => "observation_epoch_changed",
            Self::ContextStateMismatch => "context_state_mismatch",
        }
    }
}

mod capability_seal {
    #[derive(PartialEq, Eq)]
    pub(super) struct ApprovalIntentSeal(());

    impl ApprovalIntentSeal {
        pub(super) const fn new() -> Self {
            Self(())
        }
    }

    #[derive(PartialEq, Eq)]
    pub(super) struct ApprovedRootCapabilitySeal(());

    impl ApprovedRootCapabilitySeal {
        pub(super) const fn new() -> Self {
            Self(())
        }
    }

    #[derive(PartialEq, Eq)]
    pub(super) struct RuntimeStorageWriterLeaseSeal(());

    impl RuntimeStorageWriterLeaseSeal {
        pub(super) const fn new() -> Self {
            Self(())
        }
    }
}

/// The fixed, Lumina-owned names a runtime writer may address.
///
/// This is deliberately not a path abstraction. New runtime layouts require a
/// new enum variant and a corresponding review of the Windows implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mcp_platform) enum RuntimeStorageSlot {
    Payload,
}

/// One regular file admitted to a directory materialization transaction.
///
/// The contract is deliberately data-only: every relative component is a
/// single Windows leaf, `size` is exact, and `sha256` is lowercase hex. The
/// Windows witness additionally enforces bounded depth, file count and total
/// bytes before it creates the staging directory. It materializes only regular
/// unnamed streams, then rejects any reparse point, ADS, hardlink, special
/// file or unexpected directory entry during handle-based validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp_platform) struct DirectoryPayloadManifestEntry {
    pub(in crate::mcp_platform) relative_segments: Vec<String>,
    pub(in crate::mcp_platform) size: u64,
    pub(in crate::mcp_platform) sha256: String,
}

/// The complete, closed set of files a directory writer may materialize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp_platform) struct DirectoryPayloadManifest {
    pub(in crate::mcp_platform) entries: Vec<DirectoryPayloadManifestEntry>,
}

/// Bytes supplied for one manifest entry. Callers cannot supply a path: the
/// witness matches these components against the already-validated manifest.
pub(in crate::mcp_platform) struct DirectoryPayload<'a> {
    pub(in crate::mcp_platform) relative_segments: Vec<String>,
    pub(in crate::mcp_platform) bytes: &'a [u8],
}

/// The only committed directory leaves this primitive can create. They map to
/// layouts already understood by read-only managed-storage preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mcp_platform) enum RuntimeStorageCommittedLeaf {
    Cache,
    Installations,
}

/// Opaque evidence retained by the Windows implementation while a lease is
/// live. It intentionally has no path, selector, or serialization surface.
trait RuntimeStorageWriterWitness: Send + Sync {
    fn create_staging(&self) -> McpPlatformResult<()>;
    fn write_verified_payload(&self, bytes: &[u8], expected_sha256: &str) -> McpPlatformResult<()>;
    fn verify_regular_payload(&self) -> McpPlatformResult<()>;
    fn atomic_promote(&self) -> McpPlatformResult<()>;
    fn remove_lumina_owned_contents(&self) -> McpPlatformResult<()>;
    fn create_directory_staging(
        &self,
        manifest: &DirectoryPayloadManifest,
    ) -> McpPlatformResult<()>;
    fn materialize_verified_directory(
        &self,
        payloads: &[DirectoryPayload<'_>],
    ) -> McpPlatformResult<()>;
    fn seal_staged_directory(&self) -> McpPlatformResult<()>;
    fn promote_staged_directory(
        &self,
        committed_leaf: RuntimeStorageCommittedLeaf,
    ) -> McpPlatformResult<()>;
    fn promote_staged_managed_installation(&self) -> McpPlatformResult<()>;
    fn activate_verified_managed_installation(
        &self,
        managed_mcp_id: &str,
        version: &str,
        descriptor: &[u8],
        descriptor_sha256: &str,
    ) -> McpPlatformResult<()>;
    fn clear_managed_installation_activation(&self, managed_mcp_id: &str) -> McpPlatformResult<()>;
    fn managed_installation_version_is_verified(
        &self,
        managed_mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<bool>;
    fn read_verified_managed_installation_activation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<Vec<u8>>>;
}

/// A private issuer token required to obtain the Windows witness.
///
/// The type is visible only so the Windows implementation can require it as a
/// parameter. Its field and constructor are private to this module, so sibling
/// adapters cannot forge one to obtain a witness or mint a lease.
struct RuntimeStorageWriterIssuer {
    seal: capability_seal::RuntimeStorageWriterLeaseSeal,
}

impl RuntimeStorageWriterIssuer {
    fn new() -> Self {
        Self {
            seal: capability_seal::RuntimeStorageWriterLeaseSeal::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeWriterLeaseState {
    Fresh,
    StagingCreated,
    PayloadWritten,
    PayloadVerified,
    DirectoryStagingCreated,
    DirectoryMaterialized,
    DirectorySealed,
    Retired,
}

/// A single runtime-write transaction bound to a still-live Windows witness.
///
/// The lease is intentionally neither `Clone` nor serializable. Every typed
/// operation re-enters the witness, which must revalidate the open handle and
/// all identity bindings before it can touch disk. The type exposes neither a
/// root nor a path and can address only the fixed runtime namespace above.
pub(in crate::mcp_platform) struct RuntimeStorageWriterLease {
    seal: capability_seal::RuntimeStorageWriterLeaseSeal,
    witness: Arc<dyn RuntimeStorageWriterWitness>,
    state: RuntimeWriterLeaseState,
}

impl RuntimeStorageWriterLease {
    fn acquire(
        _issuer: RuntimeStorageWriterIssuer,
        witness: Arc<dyn RuntimeStorageWriterWitness>,
    ) -> Self {
        Self {
            seal: capability_seal::RuntimeStorageWriterLeaseSeal::new(),
            witness,
            state: RuntimeWriterLeaseState::Fresh,
        }
    }

    pub(in crate::mcp_platform) fn create_staging(&mut self) -> McpPlatformResult<()> {
        self.require_state(RuntimeWriterLeaseState::Fresh)?;
        self.witness.create_staging()?;
        self.state = RuntimeWriterLeaseState::StagingCreated;
        Ok(())
    }

    pub(in crate::mcp_platform) fn write_verified_bytes(
        &mut self,
        slot: RuntimeStorageSlot,
        bytes: &[u8],
        expected_sha256: &str,
    ) -> McpPlatformResult<()> {
        if slot != RuntimeStorageSlot::Payload
            || bytes.is_empty()
            || !is_lower_hex_sha256(expected_sha256)
        {
            return Err(runtime_writer_error());
        }
        self.require_state(RuntimeWriterLeaseState::StagingCreated)?;
        self.witness
            .write_verified_payload(bytes, expected_sha256)?;
        self.state = RuntimeWriterLeaseState::PayloadWritten;
        Ok(())
    }

    pub(in crate::mcp_platform) fn verify_regular_file(
        &mut self,
        slot: RuntimeStorageSlot,
    ) -> McpPlatformResult<()> {
        if slot != RuntimeStorageSlot::Payload {
            return Err(runtime_writer_error());
        }
        self.require_state(RuntimeWriterLeaseState::PayloadWritten)?;
        self.witness.verify_regular_payload()?;
        self.state = RuntimeWriterLeaseState::PayloadVerified;
        Ok(())
    }

    pub(in crate::mcp_platform) fn atomic_promote(&mut self) -> McpPlatformResult<()> {
        self.require_state(RuntimeWriterLeaseState::PayloadVerified)?;
        self.witness.atomic_promote()?;
        self.state = RuntimeWriterLeaseState::Retired;
        Ok(())
    }

    pub(in crate::mcp_platform) fn remove_lumina_owned_contents(
        &mut self,
    ) -> McpPlatformResult<()> {
        self.require_state(RuntimeWriterLeaseState::Fresh)?;
        self.witness.remove_lumina_owned_contents()?;
        self.state = RuntimeWriterLeaseState::Retired;
        Ok(())
    }

    pub(in crate::mcp_platform) fn create_directory_staging(
        &mut self,
        manifest: &DirectoryPayloadManifest,
    ) -> McpPlatformResult<()> {
        self.require_state(RuntimeWriterLeaseState::Fresh)?;
        self.witness.create_directory_staging(manifest)?;
        self.state = RuntimeWriterLeaseState::DirectoryStagingCreated;
        Ok(())
    }

    pub(in crate::mcp_platform) fn materialize_verified_directory(
        &mut self,
        payloads: &[DirectoryPayload<'_>],
    ) -> McpPlatformResult<()> {
        self.require_state(RuntimeWriterLeaseState::DirectoryStagingCreated)?;
        self.witness.materialize_verified_directory(payloads)?;
        self.state = RuntimeWriterLeaseState::DirectoryMaterialized;
        Ok(())
    }

    pub(in crate::mcp_platform) fn seal_staged_directory(&mut self) -> McpPlatformResult<()> {
        self.require_state(RuntimeWriterLeaseState::DirectoryMaterialized)?;
        self.witness.seal_staged_directory()?;
        self.state = RuntimeWriterLeaseState::DirectorySealed;
        Ok(())
    }

    pub(in crate::mcp_platform) fn promote_staged_directory(
        &mut self,
        committed_leaf: RuntimeStorageCommittedLeaf,
    ) -> McpPlatformResult<()> {
        self.require_state(RuntimeWriterLeaseState::DirectorySealed)?;
        self.witness.promote_staged_directory(committed_leaf)?;
        self.state = RuntimeWriterLeaseState::Retired;
        Ok(())
    }

    pub(in crate::mcp_platform) fn promote_staged_managed_installation(
        &mut self,
    ) -> McpPlatformResult<()> {
        self.require_state(RuntimeWriterLeaseState::DirectorySealed)?;
        self.witness.promote_staged_managed_installation()?;
        self.state = RuntimeWriterLeaseState::Retired;
        Ok(())
    }

    pub(in crate::mcp_platform) fn activate_verified_managed_installation(
        &mut self,
        managed_mcp_id: &str,
        version: &str,
        descriptor: &[u8],
        descriptor_sha256: &str,
    ) -> McpPlatformResult<()> {
        if descriptor.is_empty() || !is_lower_hex_sha256(descriptor_sha256) {
            return Err(runtime_writer_error());
        }
        self.require_state(RuntimeWriterLeaseState::PayloadVerified)?;
        self.witness.activate_verified_managed_installation(
            managed_mcp_id,
            version,
            descriptor,
            descriptor_sha256,
        )?;
        self.state = RuntimeWriterLeaseState::Retired;
        Ok(())
    }

    pub(in crate::mcp_platform) fn clear_managed_installation_activation(
        &mut self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<()> {
        self.require_state(RuntimeWriterLeaseState::Fresh)?;
        self.witness
            .clear_managed_installation_activation(managed_mcp_id)?;
        self.state = RuntimeWriterLeaseState::Retired;
        Ok(())
    }

    pub(in crate::mcp_platform) fn managed_installation_version_is_verified(
        &mut self,
        managed_mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<bool> {
        self.require_state(RuntimeWriterLeaseState::Fresh)?;
        let verified = self
            .witness
            .managed_installation_version_is_verified(managed_mcp_id, version)?;
        self.state = RuntimeWriterLeaseState::Retired;
        Ok(verified)
    }

    pub(in crate::mcp_platform) fn read_verified_managed_installation_activation(
        &mut self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<Vec<u8>>> {
        self.require_state(RuntimeWriterLeaseState::Fresh)?;
        let active = self
            .witness
            .read_verified_managed_installation_activation(managed_mcp_id)?;
        self.state = RuntimeWriterLeaseState::Retired;
        Ok(active)
    }

    fn require_state(&self, required_state: RuntimeWriterLeaseState) -> McpPlatformResult<()> {
        if self.state != required_state {
            return Err(runtime_writer_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
fn acquire_live_windows_runtime_writer_lease(
    locator: &PlatformLocator,
    context: &PlatformStorageContext,
) -> McpPlatformResult<RuntimeStorageWriterLease> {
    let issuer = RuntimeStorageWriterIssuer::new();
    let witness = windows_authority::acquire_runtime_writer_witness(&issuer, locator, context)?;
    Ok(RuntimeStorageWriterLease::acquire(issuer, witness))
}

impl fmt::Debug for RuntimeStorageWriterLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeStorageWriterLease")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn runtime_writer_error() -> McpPlatformError {
    McpPlatformError::new(
        super::error::McpPlatformErrorCode::IntegrityUnavailable,
        "runtime storage writer lease is not valid for this operation",
    )
}

#[derive(PartialEq, Eq)]
struct ApprovalIntent {
    seal: capability_seal::ApprovalIntentSeal,
    locator: PlatformLocator,
    volume_identity: LiveVolumeIdentity,
    ancestor_identity: LiveAncestorIdentity,
    observed_absent_epoch: LiveObservationEpoch,
    issued_at_ms: u64,
    expires_at_ms: u64,
    cancelled_at_ms: Option<u64>,
    consumed_at_ms: Option<u64>,
}

impl ApprovalIntent {
    fn issue_for_bootstrap(
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
        issued_at_ms: u64,
        expires_at_ms: u64,
    ) -> Result<Self, PlatformBlockedReason> {
        if expires_at_ms <= issued_at_ms {
            return Err(PlatformBlockedReason::ApprovalExpired);
        }
        if !context.matches_locator(locator) {
            return Err(PlatformBlockedReason::SelectorMismatch);
        }
        if context.root_state() != PlatformRootState::Absent {
            return Err(PlatformBlockedReason::ContextStateMismatch);
        }
        let Some(ancestor_identity) = context.ancestor_identity().cloned() else {
            return Err(PlatformBlockedReason::ApprovalIntentMismatch);
        };
        Ok(Self {
            seal: capability_seal::ApprovalIntentSeal::new(),
            locator: locator.clone(),
            volume_identity: context.volume_identity().clone(),
            ancestor_identity,
            observed_absent_epoch: context.observation_epoch(),
            issued_at_ms,
            expires_at_ms,
            cancelled_at_ms: None,
            consumed_at_ms: None,
        })
    }

    fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    fn cancel(&mut self, cancelled_at_ms: u64) -> bool {
        if self.cancelled_at_ms.is_some() || self.consumed_at_ms.is_some() {
            return false;
        }
        self.cancelled_at_ms = Some(cancelled_at_ms);
        true
    }

    fn recheck_for_bootstrap(
        &self,
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
        now_ms: u64,
    ) -> Result<(), PlatformBlockedReason> {
        self.ensure_active(now_ms)?;
        if !self.locator.selector.same_selector(locator.selector())
            || !context.matches_locator(locator)
        {
            return Err(PlatformBlockedReason::SelectorMismatch);
        }
        if context.root_state() != PlatformRootState::Absent {
            return Err(PlatformBlockedReason::ApprovalIntentMismatch);
        }
        let Some(ancestor_identity) = context.ancestor_identity() else {
            return Err(PlatformBlockedReason::ApprovalIntentMismatch);
        };
        if context.volume_identity() != &self.volume_identity {
            return Err(PlatformBlockedReason::VolumeIdentityMismatch);
        }
        if ancestor_identity != &self.ancestor_identity {
            return Err(PlatformBlockedReason::AncestorIdentityMismatch);
        }
        if context.observation_epoch() != self.observed_absent_epoch {
            return Err(PlatformBlockedReason::ObservationEpochChanged);
        }
        Ok(())
    }

    fn consume_for_bootstrap(
        &mut self,
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
        now_ms: u64,
    ) -> Result<(), PlatformBlockedReason> {
        self.recheck_for_bootstrap(locator, context, now_ms)?;
        if self.consumed_at_ms.is_some() {
            return Err(PlatformBlockedReason::ApprovalAlreadyConsumed);
        }
        self.consumed_at_ms = Some(now_ms);
        Ok(())
    }

    fn ensure_active(&self, now_ms: u64) -> Result<(), PlatformBlockedReason> {
        if self.cancelled_at_ms.is_some() {
            return Err(PlatformBlockedReason::ApprovalCancelled);
        }
        if self.consumed_at_ms.is_some() {
            return Err(PlatformBlockedReason::ApprovalAlreadyConsumed);
        }
        if now_ms > self.expires_at_ms {
            return Err(PlatformBlockedReason::ApprovalExpired);
        }
        Ok(())
    }
}

impl fmt::Debug for ApprovalIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovalIntent")
            .field("locator", &self.locator)
            .field("volume_identity", &self.volume_identity)
            .field("ancestor_identity", &self.ancestor_identity)
            .field("observed_absent_epoch", &self.observed_absent_epoch)
            .field("issued_at_ms", &self.issued_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("cancelled_at_ms", &self.cancelled_at_ms)
            .field("consumed_at_ms", &self.consumed_at_ms)
            .finish()
    }
}

#[derive(PartialEq, Eq)]
struct ApprovedRootCapability {
    seal: capability_seal::ApprovedRootCapabilitySeal,
    locator: PlatformLocator,
    volume_identity: LiveVolumeIdentity,
    root_identity: LiveRootIdentity,
    observed_present_epoch: LiveObservationEpoch,
    issued_at_ms: u64,
    expires_at_ms: u64,
    cancelled_at_ms: Option<u64>,
}

impl ApprovedRootCapability {
    fn issue_for_existing_root(
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
        issued_at_ms: u64,
        expires_at_ms: u64,
    ) -> Result<Self, PlatformBlockedReason> {
        if expires_at_ms <= issued_at_ms {
            return Err(PlatformBlockedReason::ApprovalExpired);
        }
        if !context.matches_locator(locator) {
            return Err(PlatformBlockedReason::SelectorMismatch);
        }
        if context.root_state() != PlatformRootState::Present {
            return Err(PlatformBlockedReason::ContextStateMismatch);
        }
        let Some(root_identity) = context.root_identity().cloned() else {
            return Err(PlatformBlockedReason::RootIdentityMismatch);
        };
        Ok(Self {
            seal: capability_seal::ApprovedRootCapabilitySeal::new(),
            locator: locator.clone(),
            volume_identity: context.volume_identity().clone(),
            root_identity,
            observed_present_epoch: context.observation_epoch(),
            issued_at_ms,
            expires_at_ms,
            cancelled_at_ms: None,
        })
    }

    fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    fn cancel(&mut self, cancelled_at_ms: u64) -> bool {
        if self.cancelled_at_ms.is_some() {
            return false;
        }
        self.cancelled_at_ms = Some(cancelled_at_ms);
        true
    }

    fn recheck_for_existing_root(
        &self,
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
        now_ms: u64,
    ) -> Result<(), PlatformBlockedReason> {
        if self.cancelled_at_ms.is_some() {
            return Err(PlatformBlockedReason::ApprovalCancelled);
        }
        if now_ms > self.expires_at_ms {
            return Err(PlatformBlockedReason::ApprovalExpired);
        }
        if !self.locator.selector.same_selector(locator.selector())
            || !context.matches_locator(locator)
        {
            return Err(PlatformBlockedReason::SelectorMismatch);
        }
        if context.root_state() != PlatformRootState::Present {
            return Err(PlatformBlockedReason::RootIdentityMismatch);
        }
        let Some(root_identity) = context.root_identity() else {
            return Err(PlatformBlockedReason::RootIdentityMismatch);
        };
        if context.volume_identity() != &self.volume_identity {
            return Err(PlatformBlockedReason::VolumeIdentityMismatch);
        }
        if root_identity != &self.root_identity {
            return Err(PlatformBlockedReason::RootIdentityMismatch);
        }
        if context.observation_epoch() != self.observed_present_epoch {
            return Err(PlatformBlockedReason::ObservationEpochChanged);
        }
        Ok(())
    }
}

impl fmt::Debug for ApprovedRootCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovedRootCapability")
            .field("locator", &self.locator)
            .field("volume_identity", &self.volume_identity)
            .field("root_identity", &self.root_identity)
            .field("observed_present_epoch", &self.observed_present_epoch)
            .field("issued_at_ms", &self.issued_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("cancelled_at_ms", &self.cancelled_at_ms)
            .finish()
    }
}

trait ExternalDSelectorStore: Send + Sync {
    fn load_d_selector(&self) -> McpPlatformResult<Option<DSelectorRecord>>;
}

trait PlatformLiveHandlePreflight: Send + Sync {
    fn observe_absent_root(
        &self,
        locator: &PlatformLocator,
    ) -> McpPlatformResult<PlatformStorageContext>;

    fn observe_present_root(
        &self,
        locator: &PlatformLocator,
    ) -> McpPlatformResult<PlatformStorageContext>;

    fn begin_read_only_preflight_session(
        &self,
        locator: &PlatformLocator,
    ) -> McpPlatformResult<PreflightSession>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const MODULE_SOURCE: &str = include_str!("storage_domain.rs");

    fn selector(root: &str) -> DSelectorRecord {
        DSelectorRecord::new("selector-1", 1, 7, PathBuf::from(root)).unwrap()
    }

    fn absent_locator_and_context() -> (PlatformLocator, PlatformStorageContext) {
        let locator =
            PlatformLocator::from_selector(selector(r"D:\anchor-keyring-binding\platform.db\root"));
        let context = PlatformStorageContext::for_absent_root(
            locator.clone(),
            LiveVolumeIdentity::new("volume-secret").unwrap(),
            LiveAncestorIdentity::new("ancestor-secret").unwrap(),
            LiveObservationEpoch::new(11).unwrap(),
        );
        (locator, context)
    }

    fn present_locator_and_context() -> (PlatformLocator, PlatformStorageContext) {
        let locator =
            PlatformLocator::from_selector(selector(r"D:\anchor-keyring-binding\platform.db\root"));
        let context = PlatformStorageContext::for_present_root(
            locator.clone(),
            LiveVolumeIdentity::new("volume-secret").unwrap(),
            LiveRootIdentity::new("root-secret").unwrap(),
            LiveObservationEpoch::new(17).unwrap(),
        );
        (locator, context)
    }

    fn assert_source_does_not_declare_clone_or_copy(struct_name: &str) {
        let struct_line = MODULE_SOURCE
            .lines()
            .position(|line| line.contains(&format!("struct {struct_name}")))
            .unwrap_or_else(|| panic!("missing struct declaration for {struct_name}"));
        let lines: Vec<_> = MODULE_SOURCE.lines().collect();

        for attribute_line in lines[..struct_line].iter().rev() {
            let attribute_line = attribute_line.trim();
            if attribute_line.is_empty() || !attribute_line.starts_with("#[") {
                break;
            }
            assert!(
                !attribute_line.contains("Clone"),
                "{struct_name} source regression: derive(Clone) reappeared; \
                 this source-level check is only a signal, not a compile-fail proof"
            );
            assert!(
                !attribute_line.contains("Copy"),
                "{struct_name} source regression: derive(Copy) reappeared; \
                 this source-level check is only a signal, not a compile-fail proof"
            );
        }

        assert!(
            !MODULE_SOURCE.contains(&format!("impl Clone for {struct_name}")),
            "{struct_name} source regression: manual Clone impl reappeared; \
             this source-level check is only a signal, not a compile-fail proof"
        );
        assert!(
            !MODULE_SOURCE.contains(&format!("impl Copy for {struct_name}")),
            "{struct_name} source regression: manual Copy impl reappeared; \
             this source-level check is only a signal, not a compile-fail proof"
        );
    }

    fn assert_source_does_not_use_pub_crate(item_kind: &str, item_name: &str) {
        assert!(
            !MODULE_SOURCE.contains(&format!("pub(crate) {item_kind} {item_name}")),
            "{item_name} source regression: crate-wide visibility reappeared"
        );
    }

    #[derive(Default)]
    struct WitnessState {
        live: bool,
        calls: Vec<&'static str>,
    }

    struct TestRuntimeWriterWitness {
        state: Mutex<WitnessState>,
    }

    impl TestRuntimeWriterWitness {
        fn live() -> Self {
            Self {
                state: Mutex::new(WitnessState {
                    live: true,
                    calls: Vec::new(),
                }),
            }
        }

        fn invalidate(&self) {
            self.state.lock().unwrap().live = false;
        }

        fn call(&self, name: &'static str) -> McpPlatformResult<()> {
            let mut state = self.state.lock().unwrap();
            if !state.live {
                return Err(runtime_writer_error());
            }
            state.calls.push(name);
            Ok(())
        }
    }

    impl RuntimeStorageWriterWitness for TestRuntimeWriterWitness {
        fn create_staging(&self) -> McpPlatformResult<()> {
            self.call("staging")
        }

        fn write_verified_payload(&self, _: &[u8], _: &str) -> McpPlatformResult<()> {
            self.call("write")
        }

        fn verify_regular_payload(&self) -> McpPlatformResult<()> {
            self.call("verify")
        }

        fn atomic_promote(&self) -> McpPlatformResult<()> {
            self.call("promote")
        }

        fn remove_lumina_owned_contents(&self) -> McpPlatformResult<()> {
            self.call("remove")
        }

        fn create_directory_staging(&self, _: &DirectoryPayloadManifest) -> McpPlatformResult<()> {
            self.call("directory-staging")
        }

        fn materialize_verified_directory(
            &self,
            _: &[DirectoryPayload<'_>],
        ) -> McpPlatformResult<()> {
            self.call("directory-materialize")
        }

        fn seal_staged_directory(&self) -> McpPlatformResult<()> {
            self.call("directory-seal")
        }

        fn promote_staged_directory(
            &self,
            _: RuntimeStorageCommittedLeaf,
        ) -> McpPlatformResult<()> {
            self.call("directory-promote")
        }

        fn promote_staged_managed_installation(&self) -> McpPlatformResult<()> {
            self.call("managed-installation-promote")
        }

        fn activate_verified_managed_installation(
            &self,
            _: &str,
            _: &str,
            _: &[u8],
            _: &str,
        ) -> McpPlatformResult<()> {
            self.call("managed-installation-activate")
        }

        fn clear_managed_installation_activation(&self, _: &str) -> McpPlatformResult<()> {
            self.call("managed-installation-clear-active")
        }

        fn managed_installation_version_is_verified(
            &self,
            _: &str,
            _: &str,
        ) -> McpPlatformResult<bool> {
            self.call("managed-installation-version-verified")?;
            Ok(true)
        }

        fn read_verified_managed_installation_activation(
            &self,
            _: &str,
        ) -> McpPlatformResult<Option<Vec<u8>>> {
            self.call("managed-installation-read-active")?;
            Ok(None)
        }
    }

    #[test]
    fn approval_types_require_matching_root_state_apis() {
        let (absent_locator, absent_context) = absent_locator_and_context();
        let (present_locator, present_context) = present_locator_and_context();

        let intent = ApprovalIntent::issue_for_bootstrap(&absent_locator, &absent_context, 10, 20);
        let capability = ApprovedRootCapability::issue_for_existing_root(
            &present_locator,
            &present_context,
            10,
            20,
        );

        assert!(intent.is_ok());
        assert!(capability.is_ok());
        assert_eq!(
            ApprovalIntent::issue_for_bootstrap(&present_locator, &present_context, 10, 20),
            Err(PlatformBlockedReason::ContextStateMismatch)
        );
        assert_eq!(
            ApprovedRootCapability::issue_for_existing_root(
                &absent_locator,
                &absent_context,
                10,
                20,
            ),
            Err(PlatformBlockedReason::ContextStateMismatch)
        );
    }

    #[test]
    fn debug_output_redacts_paths_and_live_identities() {
        let (absent_locator, absent_context) = absent_locator_and_context();
        let (present_locator, present_context) = present_locator_and_context();
        let intent =
            ApprovalIntent::issue_for_bootstrap(&absent_locator, &absent_context, 10, 20).unwrap();
        let capability = ApprovedRootCapability::issue_for_existing_root(
            &present_locator,
            &present_context,
            10,
            20,
        )
        .unwrap();

        let debug_text = format!(
            "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
            absent_locator,
            absent_context,
            present_context,
            intent,
            capability,
            present_locator.selector()
        );

        for sensitive in [
            r"D:\anchor-keyring-binding\platform.db\root",
            "platform.db",
            "anchor",
            "keyring",
            "binding",
            "volume-secret",
            "ancestor-secret",
            "root-secret",
        ] {
            assert!(
                !debug_text.contains(sensitive),
                "debug output leaked sensitive value: {sensitive}"
            );
        }
    }

    #[test]
    fn approval_intent_tracks_expiry_cancel_and_single_use() {
        let (locator, context) = absent_locator_and_context();
        let mut intent = ApprovalIntent::issue_for_bootstrap(&locator, &context, 10, 20).unwrap();

        assert_eq!(intent.issued_at_ms(), 10);
        assert_eq!(intent.expires_at_ms(), 20);
        assert_eq!(intent.recheck_for_bootstrap(&locator, &context, 20), Ok(()));
        assert_eq!(intent.consume_for_bootstrap(&locator, &context, 20), Ok(()));
        assert_eq!(
            intent.consume_for_bootstrap(&locator, &context, 20),
            Err(PlatformBlockedReason::ApprovalAlreadyConsumed)
        );

        let mut cancelled =
            ApprovalIntent::issue_for_bootstrap(&locator, &context, 10, 20).unwrap();
        assert!(cancelled.cancel(15));
        assert_eq!(
            cancelled.recheck_for_bootstrap(&locator, &context, 15),
            Err(PlatformBlockedReason::ApprovalCancelled)
        );

        let expired = ApprovalIntent::issue_for_bootstrap(&locator, &context, 10, 20).unwrap();
        assert_eq!(
            expired.recheck_for_bootstrap(&locator, &context, 21),
            Err(PlatformBlockedReason::ApprovalExpired)
        );
    }

    #[test]
    fn approved_root_capability_rechecks_present_root_and_expiry() {
        let (locator, context) = present_locator_and_context();
        let capability =
            ApprovedRootCapability::issue_for_existing_root(&locator, &context, 30, 40).unwrap();

        assert_eq!(capability.issued_at_ms(), 30);
        assert_eq!(capability.expires_at_ms(), 40);
        assert_eq!(
            capability.recheck_for_existing_root(&locator, &context, 39),
            Ok(())
        );

        let drifted_context = PlatformStorageContext::for_present_root(
            locator.clone(),
            LiveVolumeIdentity::new("volume-secret").unwrap(),
            LiveRootIdentity::new("other-root-secret").unwrap(),
            LiveObservationEpoch::new(17).unwrap(),
        );
        assert_eq!(
            capability.recheck_for_existing_root(&locator, &drifted_context, 39),
            Err(PlatformBlockedReason::RootIdentityMismatch)
        );

        let mut cancelled =
            ApprovedRootCapability::issue_for_existing_root(&locator, &context, 30, 40).unwrap();
        assert!(cancelled.cancel(35));
        assert_eq!(
            cancelled.recheck_for_existing_root(&locator, &context, 35),
            Err(PlatformBlockedReason::ApprovalCancelled)
        );
        assert_eq!(
            capability.recheck_for_existing_root(&locator, &context, 41),
            Err(PlatformBlockedReason::ApprovalExpired)
        );
    }

    #[test]
    fn approval_capability_source_does_not_declare_clone_or_copy() {
        assert_source_does_not_declare_clone_or_copy("ApprovalIntent");
        assert_source_does_not_declare_clone_or_copy("ApprovedRootCapability");
    }

    #[test]
    fn runtime_writer_lease_rechecks_its_live_witness_and_cannot_be_reused() {
        let witness = Arc::new(TestRuntimeWriterWitness::live());
        let mut lease =
            RuntimeStorageWriterLease::acquire(RuntimeStorageWriterIssuer::new(), witness.clone());
        let digest = "a".repeat(64);

        assert!(lease.create_staging().is_ok());
        assert!(lease
            .write_verified_bytes(RuntimeStorageSlot::Payload, b"verified", &digest)
            .is_ok());
        assert!(lease
            .verify_regular_file(RuntimeStorageSlot::Payload)
            .is_ok());
        assert!(lease.atomic_promote().is_ok());
        assert!(lease.atomic_promote().is_err());
        assert!(lease.create_staging().is_err());
        assert_eq!(
            witness.state.lock().unwrap().calls,
            vec!["staging", "write", "verify", "promote"]
        );
    }

    #[test]
    fn directory_promotion_requires_a_seal_before_the_witness_is_called() {
        let witness = Arc::new(TestRuntimeWriterWitness::live());
        let mut lease =
            RuntimeStorageWriterLease::acquire(RuntimeStorageWriterIssuer::new(), witness.clone());
        let bytes = b"directory payload";
        let digest = "a".repeat(64);
        let manifest = DirectoryPayloadManifest {
            entries: vec![DirectoryPayloadManifestEntry {
                relative_segments: vec![digest.clone()],
                size: bytes.len() as u64,
                sha256: digest.clone(),
            }],
        };

        lease
            .create_directory_staging(&manifest)
            .expect("directory staging");
        lease
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec![digest],
                bytes,
            }])
            .expect("directory materialization");
        assert!(lease
            .promote_staged_directory(RuntimeStorageCommittedLeaf::Cache)
            .is_err());
        lease.seal_staged_directory().expect("directory seal");
        lease
            .promote_staged_directory(RuntimeStorageCommittedLeaf::Cache)
            .expect("sealed promotion");
        assert_eq!(
            witness.state.lock().unwrap().calls,
            vec![
                "directory-staging",
                "directory-materialize",
                "directory-seal",
                "directory-promote"
            ]
        );
    }

    #[test]
    fn runtime_writer_lease_fails_closed_when_live_identity_evidence_drifts() {
        for drift in [
            "root", "volume", "ancestor", "context", "epoch", "reparse", "ads", "hardlink",
        ] {
            let witness = Arc::new(TestRuntimeWriterWitness::live());
            let mut lease = RuntimeStorageWriterLease::acquire(
                RuntimeStorageWriterIssuer::new(),
                witness.clone(),
            );
            assert!(lease.create_staging().is_ok(), "{drift}");
            witness.invalidate();
            assert!(
                lease
                    .write_verified_bytes(RuntimeStorageSlot::Payload, b"verified", &"a".repeat(64))
                    .is_err(),
                "{drift} drift must fail closed"
            );
        }
    }

    #[test]
    fn runtime_writer_lease_is_internal_nonclone_and_has_no_root_surface() {
        assert_source_does_not_declare_clone_or_copy("RuntimeStorageWriterLease");
        let declaration = MODULE_SOURCE
            .lines()
            .find(|line| {
                line.trim_start()
                    .starts_with("pub(in crate::mcp_platform) struct RuntimeStorageWriterLease")
            })
            .unwrap();
        assert!(declaration.contains("pub(in crate::mcp_platform)"));
        let start = MODULE_SOURCE
            .find("impl RuntimeStorageWriterLease {")
            .unwrap();
        let end = MODULE_SOURCE[start..]
            .find("impl fmt::Debug for RuntimeStorageWriterLease")
            .unwrap()
            + start;
        let api = &MODULE_SOURCE[start..end];
        assert!(!api.contains("pub(in crate::mcp_platform) fn acquire("));
        assert!(!api.contains("Path"));
        assert!(!api.contains("root"));
    }

    #[test]
    fn storage_domain_internal_items_do_not_widen_back_to_pub_crate() {
        for (item_kind, item_name) in [
            ("struct", "LiveObservationEpoch"),
            ("struct", "LiveVolumeIdentity"),
            ("struct", "LiveAncestorIdentity"),
            ("struct", "LiveRootIdentity"),
            ("enum", "PlatformRootState"),
            ("struct", "DSelectorRecord"),
            ("struct", "PlatformLocator"),
            ("struct", "PlatformStorageContext"),
            ("enum", "PlatformBlockedReason"),
            ("struct", "ApprovalIntent"),
            ("struct", "ApprovedRootCapability"),
            ("struct", "RuntimeStorageWriterLease"),
            ("trait", "ExternalDSelectorStore"),
            ("trait", "PlatformLiveHandlePreflight"),
        ] {
            assert_source_does_not_use_pub_crate(item_kind, item_name);
        }
    }

    #[test]
    fn platform_locator_requires_selector_record_instead_of_bare_path() {
        let selector = selector(r"D:\root");
        let locator = PlatformLocator::from_selector(selector.clone());

        assert_eq!(locator.root_path(), Path::new(r"D:\root"));
        assert_eq!(locator.selector().schema_version(), 1);
        assert_eq!(locator.selector().selector_revision(), 7);
        assert_eq!(locator.selector().root_path(), Path::new(r"D:\root"));
        assert_eq!(
            PlatformBlockedReason::ApprovalAlreadyConsumed.as_str(),
            "approval_already_consumed"
        );
    }
}
