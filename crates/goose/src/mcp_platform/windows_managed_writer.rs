//! Handle-relative, fail-closed mutation primitives for a verified managed root.
//!
//! This module intentionally provides no path or raw-handle surface. It is a
//! narrow staging primitive, not an installation or cleanup implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::cell::RefCell;

use sha2::{Digest as _, Sha256};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SYSTEM, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use super::super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::windows_authority::{
    acquire_writer_root, file_attributes, revalidate_writer_root, runtime_writer_context_matches,
    FileIdentity, RootBinding,
};
use super::{
    DirectoryPayload, DirectoryPayloadManifest, DirectoryPayloadManifestEntry, PlatformLocator,
    PlatformStorageContext, RuntimeStorageCommittedLeaf,
};
#[cfg(not(test))]
use crate::mcp_platform::repository::system_integrity_signer;
use crate::mcp_platform::repository::IntegritySigner;

const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
const OBJ_DONT_REPARSE: u32 = 0x0000_1000;
const FILE_CREATE: u32 = 0x0000_0002;
const FILE_OPEN: u32 = 0x0000_0001;
const FILE_CREATED: usize = 0x0000_0002;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_READ_DATA: u32 = 0x0000_0001;
const FILE_WRITE_DATA: u32 = 0x0000_0002;
const FILE_ADD_FILE: u32 = 0x0000_0002;
const FILE_ADD_SUBDIRECTORY: u32 = 0x0000_0004;
const FILE_TRAVERSE: u32 = 0x0000_0020;
const DELETE: u32 = 0x0001_0000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_STREAM_INFORMATION: u32 = 22;
const FILE_DISPOSITION_INFORMATION: u32 = 13;
const FILE_RENAME_INFORMATION: u32 = 10;
const FILE_DIRECTORY_INFORMATION: u32 = 1;

const STAGING_DIRECTORY: &str = "goose-staging";
const STAGING_SESSION_MARKER: &str = "goose-staging-session";
const PAYLOAD_FILE: &str = "payload";
const COMMITTED_VERSION_EVIDENCE: &str = ".goose-committed-version.json";
const MANAGED_ANCHOR_DOMAIN: &str = "managed-installations-root-v1";
const MANAGED_ANCHOR_ROOT_PREFIX: &str = "goose-managed-installations-anchor-v1:";
const MANAGED_LINEAGE_DOMAIN: &str = "managed-installations-lineage-v1";
const MANAGED_LINEAGE_ROOT_PREFIX: &str = "goose-managed-installations-lineage-v1:";
const MAX_DIRECTORY_DEPTH: usize = 8;
const MAX_DIRECTORY_FILES: usize = 256;
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DIRECTORY_SESSION_HANDLES: usize =
    2 + MAX_DIRECTORY_FILES + MAX_DIRECTORY_FILES * (MAX_DIRECTORY_DEPTH - 1);
const STATUS_NO_MORE_FILES: i32 = 0x8000_0006_u32 as i32;

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TestFailurePoint {
    InitialIdentity,
    CleanupDisposition,
    CleanupAfterDescendantDisposition,
    PartialStagingTreeDisposition,
    CleanupMarkerDisposition,
    CleanupCheckpoint,
    ClearActivationFinalize,
    PreparedRename,
    RenamedBeforeCommit,
    PartialWrite,
    PostVerification,
    RootRevalidation,
    CleanupStagingBindingRevalidation,
    RootRevalidationAfterStagingDisposition,
    FinalTreeReleaseDisposition,
    FinalTreeRootReleaseDisposition,
    PublishThenErrorAtCommitted,
    PreparedRollbackPublish,
    PreparedRollbackConfirmation,
}

#[cfg(test)]
thread_local! {
    static TEST_FAILURES: RefCell<Vec<TestFailurePoint>> = const { RefCell::new(Vec::new()) };
    static TEST_MANAGED_PROMOTION_STEPS: RefCell<Vec<ManagedPromotionStep>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedPromotionStep {
    DirectFinalTree,
    PreparedAnchor,
    CommittedAnchor,
    FinalTreeReleased,
}

#[cfg(test)]
fn inject_test_failure(point: TestFailurePoint) {
    TEST_FAILURES.with(|failures| failures.borrow_mut().push(point));
}

#[cfg(test)]
pub(super) fn inject_cleanup_failure_for_testing() {
    inject_test_failure(TestFailurePoint::CleanupDisposition);
}

#[cfg(test)]
fn take_test_failure(point: TestFailurePoint) -> bool {
    TEST_FAILURES.with(|failures| {
        let mut failures = failures.borrow_mut();
        failures
            .iter()
            .position(|candidate| *candidate == point)
            .map(|index| failures.remove(index))
            .is_some()
    })
}

#[cfg(test)]
fn record_managed_promotion_step(step: ManagedPromotionStep) {
    TEST_MANAGED_PROMOTION_STEPS.with(|steps| steps.borrow_mut().push(step));
}

#[cfg(test)]
fn take_managed_promotion_steps() -> Vec<ManagedPromotionStep> {
    TEST_MANAGED_PROMOTION_STEPS.with(|steps| std::mem::take(&mut *steps.borrow_mut()))
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *mut UnicodeString,
    attributes: u32,
    security_descriptor: *mut c_void,
    security_quality_of_service: *mut c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: i32,
    information: usize,
}

#[repr(C)]
struct FileDispositionInformation {
    delete_file: u8,
}

#[repr(C)]
struct FileRenameInformation {
    replace_if_exists: u8,
    root_directory: HANDLE,
    file_name_length: u32,
    file_name: [u16; 0],
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        file_handle: *mut HANDLE,
        desired_access: u32,
        object_attributes: *mut ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        allocation_size: *mut i64,
        file_attributes: u32,
        share_access: u32,
        create_disposition: u32,
        create_options: u32,
        ea_buffer: *mut c_void,
        ea_length: u32,
    ) -> i32;
    fn NtQueryInformationFile(
        file_handle: HANDLE,
        io_status_block: *mut IoStatusBlock,
        file_information: *mut c_void,
        length: u32,
        file_information_class: u32,
    ) -> i32;
    fn NtSetInformationFile(
        file_handle: HANDLE,
        io_status_block: *mut IoStatusBlock,
        file_information: *mut c_void,
        length: u32,
        file_information_class: u32,
    ) -> i32;
    fn NtQueryDirectoryFile(
        file_handle: HANDLE,
        event: HANDLE,
        apc_routine: *mut c_void,
        apc_context: *mut c_void,
        io_status_block: *mut IoStatusBlock,
        file_information: *mut c_void,
        length: u32,
        file_information_class: u32,
        return_single_entry: u8,
        file_name: *mut UnicodeString,
        restart_scan: u8,
    ) -> i32;
}

fn unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityUnavailable,
        "windows managed root writer evidence is unavailable",
    )
}

fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "windows managed root writer rejected unsafe storage state",
    )
}

fn cleanup_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityUnavailable,
        "windows managed root writer could not isolate an uncommitted object",
    )
}

fn cleanup_pending_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RollbackIncomplete,
        "managed installation committed; cleanup pending",
    )
}

fn prepared_recovery_required_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RollbackIncomplete,
        "managed installation requires authenticated prepared recovery",
    )
}

#[derive(Default)]
struct WriterSession {
    recovery_marker: Option<SessionObject>,
    staging: Option<SessionObject>,
    payload: Option<SessionObject>,
    expected_sha256: Option<String>,
    directory_manifest: Option<DirectoryPayloadManifest>,
    directory_entries: Vec<OwnedRelativeObject>,
    directory_entry_paths: Vec<Vec<String>>,
    directory_seal: Option<DirectorySeal>,
    recovery_required: bool,
}

/// Evidence that every object in the staging tree is still held by this
/// session with the sharing mode established at atomic creation.  Windows
/// cannot revoke a pre-existing writer, so the only promotable trees are ones
/// for which this session denied write/delete sharing from their creation.
#[derive(Clone)]
struct DirectorySeal {
    staging_identity: FileIdentity,
    entry_identities: Vec<FileIdentity>,
    marker: Vec<u8>,
}

enum SessionObject {
    Pending(PendingSessionObject),
    Ready(OwnedRelativeObject),
}

/// A newly-created object that has not yet supplied enough evidence to be
/// usable. It remains session-owned and delete-on-close until it is promoted
/// to `OwnedRelativeObject`.
struct PendingSessionObject {
    file: Option<File>,
    delete_on_drop: bool,
    directory: bool,
    desired_access: u32,
    share_access: u32,
}

impl Drop for PendingSessionObject {
    fn drop(&mut self) {
        if self.delete_on_drop {
            if let Some(file) = self.file.as_ref() {
                let _ = set_delete_on_close(file, true);
            }
        }
    }
}

impl WriterSession {
    /// A prepared checkpoint remains the only authority to reconcile these
    /// objects. Once the final tree has been released, this process must not
    /// let ordinary session teardown erase the marker or staging evidence.
    fn retain_for_authenticated_recovery(&mut self) {
        self.recovery_required = true;
        for entry in &mut self.directory_entries {
            entry.file.delete_on_drop = false;
        }
        for object in [
            self.payload.as_mut(),
            self.staging.as_mut(),
            self.recovery_marker.as_mut(),
        ] {
            match object {
                Some(SessionObject::Pending(pending)) => pending.delete_on_drop = false,
                Some(SessionObject::Ready(ready)) => ready.file.delete_on_drop = false,
                None => {}
            }
        }
    }
}

struct DeleteOnDropFile {
    file: File,
    delete_on_drop: bool,
}

impl std::ops::Deref for DeleteOnDropFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl std::ops::DerefMut for DeleteOnDropFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

impl Drop for DeleteOnDropFile {
    fn drop(&mut self) {
        if self.delete_on_drop {
            let _ = set_delete_on_close(&self.file, true);
        }
    }
}

struct OwnedRelativeObject {
    file: DeleteOnDropFile,
    identity: FileIdentity,
    directory: bool,
    desired_access: u32,
    share_access: u32,
}

/// A final tree created directly below its committed parent.  It remains
/// delete-on-close until its caller has completed the root-bound commit.
struct SealedFinalTree {
    root: Option<OwnedRelativeObject>,
    entries: Vec<OwnedRelativeObject>,
    paths: Vec<Vec<String>>,
}

impl SealedFinalTree {
    fn root(&self) -> McpPlatformResult<&OwnedRelativeObject> {
        self.root.as_ref().ok_or_else(integrity_error)
    }

    fn root_mut(&mut self) -> McpPlatformResult<&mut OwnedRelativeObject> {
        self.root.as_mut().ok_or_else(integrity_error)
    }

    fn descendant_release_order(&self) -> Vec<usize> {
        let mut release_order = (0..self.entries.len()).collect::<Vec<_>>();
        release_order.sort_unstable_by_key(|index| std::cmp::Reverse(self.paths[*index].len()));
        release_order
    }

    fn release(&mut self) -> McpPlatformResult<()> {
        let mut released = Vec::with_capacity(self.entries.len());
        for index in self.descendant_release_order() {
            if let Err(error) = release_final_object(&mut self.entries[index]) {
                return compensate_final_release(&mut self.entries, &released).and(Err(error));
            }
            released.push(index);
        }

        #[cfg(test)]
        if take_test_failure(TestFailurePoint::FinalTreeRootReleaseDisposition) {
            return compensate_final_release(&mut self.entries, &released)
                .and(Err(cleanup_error()));
        }
        if let Err(error) = release_final_object(self.root_mut()?) {
            return compensate_final_release(&mut self.entries, &released).and(Err(error));
        }
        Ok(())
    }

    fn destroy_delete_on_close(mut self) -> McpPlatformResult<()> {
        let mut armed_descendants = Vec::with_capacity(self.entries.len());
        for index in self.descendant_release_order() {
            if let Err(error) = arm_final_object_for_delete(&mut self.entries[index]) {
                return self.restore_after_failed_delete(&armed_descendants, false, error);
            }
            armed_descendants.push(index);
        }
        if let Err(error) = arm_final_object_for_delete(self.root_mut()?) {
            return self.restore_after_failed_delete(&armed_descendants, true, error);
        }

        while !self.entries.is_empty() {
            let index = self
                .entries
                .iter()
                .enumerate()
                .max_by_key(|(index, _)| self.paths[*index].len())
                .map(|(index, _)| index)
                .ok_or_else(integrity_error)?;
            let entry = self.entries.swap_remove(index);
            self.paths.swap_remove(index);
            drop(entry);
        }
        drop(self.root.take().ok_or_else(integrity_error)?);
        Ok(())
    }

    fn restore_after_failed_delete(
        mut self,
        armed_descendants: &[usize],
        root_armed: bool,
        original_error: McpPlatformError,
    ) -> McpPlatformResult<()> {
        let mut restored = Ok(());
        if root_armed {
            restored = disarm_final_object(self.root_mut()?);
        }
        for index in armed_descendants.iter().rev() {
            if let Some(entry) = self.entries.get_mut(*index) {
                if restored.is_ok() {
                    restored = disarm_final_object(entry);
                } else {
                    entry.file.delete_on_drop = false;
                }
            }
        }
        if restored.is_err() {
            return Err(cleanup_error());
        }
        Err(original_error)
    }
}

fn release_final_object(object: &mut OwnedRelativeObject) -> McpPlatformResult<()> {
    #[cfg(test)]
    if take_test_failure(TestFailurePoint::FinalTreeReleaseDisposition) {
        return Err(cleanup_error());
    }
    set_delete_on_close(&object.file, false)?;
    object.file.delete_on_drop = false;
    Ok(())
}

fn arm_final_object_for_delete(object: &mut OwnedRelativeObject) -> McpPlatformResult<()> {
    set_delete_on_close(&object.file, true)?;
    object.file.delete_on_drop = false;
    Ok(())
}

fn disarm_final_object(object: &mut OwnedRelativeObject) -> McpPlatformResult<()> {
    let result = set_delete_on_close(&object.file, false);
    object.file.delete_on_drop = false;
    result
}

fn compensate_final_release(
    entries: &mut [OwnedRelativeObject],
    released: &[usize],
) -> McpPlatformResult<()> {
    for index in released.iter().rev() {
        let entry = entries.get_mut(*index).ok_or_else(integrity_error)?;
        set_delete_on_close(&entry.file, true)?;
        entry.file.delete_on_drop = true;
    }
    Ok(())
}

impl Drop for SealedFinalTree {
    fn drop(&mut self) {
        // Destruction is only permitted through `destroy_delete_on_close`,
        // which reports every kernel disposition failure. An unexpected drop
        // keeps the complete tree rather than deleting a prefix of it.
        if let Some(root) = self.root.as_mut() {
            root.file.delete_on_drop = false;
        }
        for entry in &mut self.entries {
            entry.file.delete_on_drop = false;
        }
    }
}

fn final_tree_version_object(
    tree: &SealedFinalTree,
    index: Option<usize>,
) -> McpPlatformResult<&OwnedRelativeObject> {
    index
        .map(|index| tree.entries.get(index).ok_or_else(integrity_error))
        .transpose()?
        .map_or_else(|| tree.root(), Ok)
}

/// The only mutable authority for one Windows preflight-verified root.
///
/// It is neither cloneable nor serializable, does not expose the root path or
/// handle, and is constructible only from the private preflight binding. Its
/// root handle remains open from acquisition through session teardown with
/// read-only sharing. Every staging directory, descendant, payload, and
/// recovery marker is created relative to that root with the same sharing
/// restriction and remains open through seal validation and no-replace
/// promotion.
pub(super) struct ManagedRootCapability {
    binding: RootBinding,
    writer_root: File,
    session: Mutex<WriterSession>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedRootAnchor {
    format_version: u8,
    managed: BTreeMap<String, ManagedAnchorState>,
    #[serde(default)]
    cleanup_pending: Option<CommittedCleanupPending>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedAnchorState {
    versions: BTreeMap<String, AnchoredVersion>,
    active: Option<AnchoredActive>,
    #[serde(default)]
    active_clear_pending: Option<AnchoredActive>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AnchoredVersion {
    descriptor_sha256: String,
    tree_sha256: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AnchoredActive {
    version: String,
    descriptor_sha256: String,
}

/// The final version has been anchored, but the transaction residue has not
/// yet been removed. This checkpoint is authenticated by the root-bound
/// signer; it is deliberately not a claim that arbitrary staging can be
/// adopted.
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CommittedCleanupPending {
    managed_mcp_id: String,
    version: String,
    marker_sha256: String,
    staging_expected: bool,
    #[serde(default)]
    phase: CleanupTransactionPhase,
    #[serde(default)]
    expected_version: Option<AnchoredVersion>,
    #[serde(default)]
    marker_identity: Option<String>,
    #[serde(default)]
    prepared_version_identity: Option<String>,
    #[serde(default)]
    staging_identity: Option<String>,
    #[serde(default)]
    staging_snapshot: Option<BTreeMap<String, CleanupStagingObject>>,
}

/// The identity of one staging descendant at the instant its deletion was
/// root-authorized.  Recovery may delete only the still-present subset of
/// this authenticated inventory.
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CleanupStagingObject {
    identity: String,
    directory: bool,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedRootLineage {
    format_version: u8,
    root_identity: String,
}

/// A root-authorized promotion has three durable boundaries.  `Prepared` is
/// written after the final no-replace creation, `Committed` only after that
/// sealed final version has been anchored, and `CleanupComplete` records that
/// the exact residue is gone before the checkpoint is finally removed.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CleanupTransactionPhase {
    Prepared,
    PreparedStagingDeleteAuthorized,
    PreparedMarkerDeleteAuthorized,
    Committed,
    CommittedStagingDeleteAuthorized,
    CommittedMarkerDeleteAuthorized,
    CleanupComplete,
}

enum CleanupPendingMigration {
    Checkpoint(Option<crate::mcp_platform::repository::AnchorCheckpoint>),
    Cleared,
}

impl Default for CleanupTransactionPhase {
    fn default() -> Self {
        Self::Committed
    }
}

impl ManagedRootAnchor {
    fn empty() -> Self {
        Self {
            format_version: 6,
            managed: BTreeMap::new(),
            cleanup_pending: None,
        }
    }

    fn encode(&self, signer: &dyn IntegritySigner) -> McpPlatformResult<String> {
        let canonical = serde_json::to_vec(self).map_err(|_| integrity_error())?;
        let mac = signer.sign(MANAGED_ANCHOR_DOMAIN, &canonical)?;
        let envelope = serde_json::json!({ "state": self, "mac": mac });
        Ok(format!(
            "{MANAGED_ANCHOR_ROOT_PREFIX}{}",
            serde_json::to_string(&envelope).map_err(|_| integrity_error())?
        ))
    }

    fn decode(root: &str, signer: &dyn IntegritySigner) -> McpPlatformResult<Self> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            state: ManagedRootAnchor,
            mac: String,
        }
        let encoded = root
            .strip_prefix(MANAGED_ANCHOR_ROOT_PREFIX)
            .ok_or_else(integrity_error)?;
        let envelope: Envelope = serde_json::from_str(encoded).map_err(|_| integrity_error())?;
        if !matches!(envelope.state.format_version, 1..=6) || !is_lower_hex_sha256(&envelope.mac) {
            return Err(integrity_error());
        }
        if envelope.state.format_version == 1 && envelope.state.cleanup_pending.is_some() {
            return Err(integrity_error());
        }
        let canonical = serde_json::to_vec(&envelope.state).map_err(|_| integrity_error())?;
        signer.verify(MANAGED_ANCHOR_DOMAIN, &canonical, &envelope.mac)?;
        Ok(envelope.state)
    }
}

fn ensure_no_active_clear_pending(anchor: &ManagedRootAnchor) -> McpPlatformResult<()> {
    if anchor
        .managed
        .values()
        .any(|managed| managed.active_clear_pending.is_some())
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn checked_prepared_identity(identity: &Option<String>) -> McpPlatformResult<&str> {
    let identity = identity.as_deref().ok_or_else(integrity_error)?;
    let valid = identity.len() == 26
        && identity
            .split(':')
            .all(|part| part.len() == 8 && part.bytes().all(|byte| byte.is_ascii_hexdigit()));
    if !valid {
        return Err(integrity_error());
    }
    Ok(identity)
}

fn verify_prepared_marker_identity(
    root: &File,
    pending: &CommittedCleanupPending,
) -> McpPlatformResult<()> {
    let marker = open_relative_file_for_mutation(root, STAGING_SESSION_MARKER)?
        .ok_or_else(integrity_error)?;
    if hex_digest(&read_verified_file(&marker)?) != pending.marker_sha256
        || FileIdentity::from_file(&marker)?.durable_key()
            != checked_prepared_identity(&pending.marker_identity)?
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn managed_anchor_signer(binding: &RootBinding, allow_create: bool) -> Arc<dyn IntegritySigner> {
    let binding = binding.managed_installation_anchor_binding();
    #[cfg(test)]
    {
        let _ = allow_create;
        static TEST_SIGNERS: OnceLock<Mutex<BTreeMap<String, Arc<dyn IntegritySigner>>>> =
            OnceLock::new();
        let signers = TEST_SIGNERS.get_or_init(|| Mutex::new(BTreeMap::new()));
        let mut signers = signers.lock().expect("managed anchor test signer registry");
        return signers
            .entry(binding.clone())
            .or_insert_with(|| {
                crate::mcp_platform::repository::InMemoryIntegritySigner::new_for_testing_with_path_binding(
                    [0x6d; 32],
                    binding,
                )
            })
            .clone();
    }
    #[cfg(not(test))]
    {
        system_integrity_signer(&binding, allow_create)
    }
}

fn managed_lineage_signer(binding: &RootBinding, allow_create: bool) -> Arc<dyn IntegritySigner> {
    let binding = binding.managed_installation_lineage_binding();
    #[cfg(test)]
    {
        let _ = allow_create;
        static TEST_SIGNERS: OnceLock<Mutex<BTreeMap<String, Arc<dyn IntegritySigner>>>> =
            OnceLock::new();
        let signers = TEST_SIGNERS.get_or_init(|| Mutex::new(BTreeMap::new()));
        let mut signers = signers
            .lock()
            .expect("managed lineage test signer registry");
        return signers
            .entry(binding.clone())
            .or_insert_with(|| {
                crate::mcp_platform::repository::InMemoryIntegritySigner::new_for_testing_with_path_binding(
                    [0x6e; 32],
                    binding,
                )
            })
            .clone();
    }
    #[cfg(not(test))]
    {
        system_integrity_signer(&binding, allow_create)
    }
}

impl ManagedRootLineage {
    fn current(binding: &RootBinding) -> Self {
        Self {
            format_version: 1,
            root_identity: binding.managed_installation_root_identity(),
        }
    }

    fn encode(&self, signer: &dyn IntegritySigner) -> McpPlatformResult<String> {
        let canonical = serde_json::to_vec(self).map_err(|_| integrity_error())?;
        let mac = signer.sign(MANAGED_LINEAGE_DOMAIN, &canonical)?;
        let envelope = serde_json::json!({ "state": self, "mac": mac });
        Ok(format!(
            "{MANAGED_LINEAGE_ROOT_PREFIX}{}",
            serde_json::to_string(&envelope).map_err(|_| integrity_error())?
        ))
    }

    fn decode(root: &str, signer: &dyn IntegritySigner) -> McpPlatformResult<Self> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            state: ManagedRootLineage,
            mac: String,
        }
        let encoded = root
            .strip_prefix(MANAGED_LINEAGE_ROOT_PREFIX)
            .ok_or_else(integrity_error)?;
        let envelope: Envelope = serde_json::from_str(encoded).map_err(|_| integrity_error())?;
        if envelope.state.format_version != 1
            || envelope.state.root_identity.is_empty()
            || !is_lower_hex_sha256(&envelope.mac)
        {
            return Err(integrity_error());
        }
        let canonical = serde_json::to_vec(&envelope.state).map_err(|_| integrity_error())?;
        signer.verify(MANAGED_LINEAGE_DOMAIN, &canonical, &envelope.mac)?;
        Ok(envelope.state)
    }
}

fn load_managed_root_lineage(
    binding: &RootBinding,
    allow_create: bool,
) -> McpPlatformResult<(
    Arc<dyn IntegritySigner>,
    Option<crate::mcp_platform::repository::AnchorCheckpoint>,
    Option<ManagedRootLineage>,
)> {
    let expected_binding = binding.managed_installation_lineage_binding();
    let signer = managed_lineage_signer(binding, allow_create);
    if let Some(error) = signer.health().availability_error() {
        return Err(error);
    }
    let identity = signer.identity()?;
    if identity.path_binding != expected_binding {
        return Err(integrity_error());
    }
    let checkpoint = signer.checkpoint()?;
    let lineage = checkpoint
        .as_ref()
        .map(|checkpoint| ManagedRootLineage::decode(&checkpoint.root, signer.as_ref()))
        .transpose()?;
    Ok((signer, checkpoint, lineage))
}

fn verify_managed_root_lineage(
    binding: &RootBinding,
    require_existing: bool,
) -> McpPlatformResult<()> {
    // A fresh locator has no keyring entry yet. `allow_create` only creates
    // an in-memory candidate; it becomes durable exclusively on publish.
    let (_, _, lineage) = load_managed_root_lineage(binding, true)?;
    match lineage {
        Some(lineage) if lineage.root_identity == binding.managed_installation_root_identity() => {
            Ok(())
        }
        Some(_) => Err(integrity_error()),
        None if require_existing => Err(integrity_error()),
        None => Ok(()),
    }
}

fn establish_managed_root_lineage(binding: &RootBinding) -> McpPlatformResult<()> {
    let (signer, checkpoint, lineage) = load_managed_root_lineage(binding, true)?;
    match lineage {
        Some(lineage) if lineage.root_identity == binding.managed_installation_root_identity() => {
            Ok(())
        }
        Some(_) => Err(integrity_error()),
        None => {
            let identity = signer.identity()?;
            let next = crate::mcp_platform::repository::AnchorCheckpoint {
                identity,
                sequence: checkpoint
                    .as_ref()
                    .map_or(0, |checkpoint| checkpoint.sequence.saturating_add(1)),
                root: ManagedRootLineage::current(binding).encode(signer.as_ref())?,
            };
            signer.publish(checkpoint.as_ref(), &next)
        }
    }
}

fn load_managed_anchor(
    binding: &RootBinding,
    allow_create: bool,
    require_existing: bool,
) -> McpPlatformResult<(
    Arc<dyn IntegritySigner>,
    Option<crate::mcp_platform::repository::AnchorCheckpoint>,
    ManagedRootAnchor,
)> {
    let expected_binding = binding.managed_installation_anchor_binding();
    let signer = managed_anchor_signer(binding, allow_create);
    if let Some(error) = signer.health().availability_error() {
        return Err(error);
    }
    let identity = signer.identity()?;
    if identity.path_binding != expected_binding {
        return Err(integrity_error());
    }
    let checkpoint = signer.checkpoint()?;
    let state = match checkpoint.as_ref() {
        Some(checkpoint) => ManagedRootAnchor::decode(&checkpoint.root, signer.as_ref())?,
        None if require_existing => return Err(integrity_error()),
        None => ManagedRootAnchor::empty(),
    };
    Ok((signer, checkpoint, state))
}

fn publish_managed_anchor(
    signer: &dyn IntegritySigner,
    expected: Option<&crate::mcp_platform::repository::AnchorCheckpoint>,
    state: &ManagedRootAnchor,
) -> McpPlatformResult<crate::mcp_platform::repository::AnchorCheckpoint> {
    let identity = signer.identity()?;
    let next = crate::mcp_platform::repository::AnchorCheckpoint {
        identity,
        sequence: expected.map_or(0, |checkpoint| checkpoint.sequence.saturating_add(1)),
        root: state.encode(signer)?,
    };
    let publish_result = signer.publish(expected, &next);
    #[cfg(test)]
    let publish_result = publish_result.and_then(|_| {
        if state
            .cleanup_pending
            .as_ref()
            .is_some_and(|pending| pending.phase == CleanupTransactionPhase::Committed)
            && take_test_failure(TestFailurePoint::PublishThenErrorAtCommitted)
        {
            Err(unavailable())
        } else {
            Ok(())
        }
    });
    match publish_result {
        Ok(()) => Ok(next),
        Err(error) => match signer.checkpoint() {
            Ok(Some(observed)) if observed == next => Ok(next),
            _ => Err(error),
        },
    }
}

/// Reaffirms the already-durable prepared state without ever replacing it
/// with the pre-transaction anchor.  After final-tree release has failed,
/// replacing the checkpoint would make a successful-but-unobserved publish
/// able to strand the staging witness without recovery authority.
fn reaffirm_prepared_anchor(
    signer: &dyn IntegritySigner,
    expected: Option<&crate::mcp_platform::repository::AnchorCheckpoint>,
    state: &ManagedRootAnchor,
) -> McpPlatformResult<()> {
    #[cfg(test)]
    if take_test_failure(TestFailurePoint::PreparedRollbackPublish) {
        return Err(unavailable());
    }
    let reaffirmed = publish_managed_anchor(signer, expected, state)?;
    #[cfg(test)]
    if take_test_failure(TestFailurePoint::PreparedRollbackConfirmation) {
        return Err(unavailable());
    }
    match signer.checkpoint() {
        Ok(Some(observed)) if observed == reaffirmed => Ok(()),
        _ => Err(unavailable()),
    }
}

fn parse_active_version(bytes: &[u8]) -> McpPlatformResult<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| integrity_error())?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(integrity_error)?;
    validated_directory_segment(version)?;
    Ok(version.to_owned())
}

/// Verifies the mutable pointer against the root-authorized active state before
/// another operation is allowed to supersede it. An in-progress clear is never
/// adopted by another mutation; only `clear_managed_installation_activation`
/// can finish its authenticated transition.
fn verify_current_active_pointer(
    managed: &File,
    managed_anchor: &ManagedAnchorState,
) -> McpPlatformResult<()> {
    if managed_anchor.active_clear_pending.is_some() {
        return Err(integrity_error());
    }
    let active = open_relative_file_for_mutation(managed, "active.json")?;
    match (active, managed_anchor.active.as_ref()) {
        (None, None) => Ok(()),
        (None, Some(_)) | (Some(_), None) => Err(integrity_error()),
        (Some(active), Some(active_anchor)) => {
            let active_bytes = read_verified_file(&active)?;
            let version = parse_active_version(&active_bytes)?;
            if active_anchor.version != version
                || active_anchor.descriptor_sha256 != hex_digest(&active_bytes)
            {
                return Err(integrity_error());
            }
            let versions = open_relative_directory_for_mutation(managed, "versions")?;
            let target = open_relative_directory_for_mutation(&versions, &version)?;
            verify_anchored_version(
                &target,
                &active_bytes,
                &version,
                managed_anchor
                    .versions
                    .get(&version)
                    .ok_or_else(integrity_error)?,
            )
        }
    }
}

impl ManagedRootCapability {
    pub(super) fn from_verified_preflight(binding: RootBinding) -> McpPlatformResult<Self> {
        let writer_root = acquire_writer_root(&binding)?;
        verify_managed_root_lineage(
            &binding,
            relative_object_exists(&writer_root, "platform.installations")?,
        )?;
        let capability = Self {
            binding,
            writer_root,
            session: Mutex::new(WriterSession::default()),
        };
        capability.revalidate()?;
        Ok(capability)
    }

    pub(super) fn revalidate(&self) -> McpPlatformResult<()> {
        #[cfg(test)]
        if take_test_failure(TestFailurePoint::RootRevalidation) {
            return Err(integrity_error());
        }
        revalidate_writer_root(&self.binding, &self.writer_root)?;
        let attributes = file_attributes(&self.writer_root)?;
        if attributes
            & (FILE_ATTRIBUTE_DIRECTORY
                | FILE_ATTRIBUTE_HIDDEN
                | FILE_ATTRIBUTE_SYSTEM
                | FILE_ATTRIBUTE_REPARSE_POINT)
            != FILE_ATTRIBUTE_DIRECTORY
        {
            return Err(integrity_error());
        }
        if FileIdentity::link_count(&self.writer_root)? != 1 {
            return Err(integrity_error());
        }
        if has_named_streams(&self.writer_root)? {
            return Err(integrity_error());
        }
        Ok(())
    }

    pub(super) fn revalidate_for_context(
        &self,
        locator: &PlatformLocator,
        context: &PlatformStorageContext,
    ) -> McpPlatformResult<()> {
        if !runtime_writer_context_matches(&self.binding, locator, context) {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "windows runtime writer lease context drifted",
            ));
        }
        self.revalidate()
    }

    pub(super) fn discard_uncommitted(&self) -> McpPlatformResult<()> {
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        discard_uncommitted(&mut session)
    }

    #[cfg(test)]
    fn abandon_uncommitted_for_crash_testing(&self) {
        let mut session = self.session.lock().expect("test session lock");
        for mut entry in session.directory_entries.drain(..) {
            entry.file.delete_on_drop = false;
        }
        for object in [
            session.payload.take(),
            session.staging.take(),
            session.recovery_marker.take(),
        ] {
            if let Some(SessionObject::Ready(mut object)) = object {
                object.file.delete_on_drop = false;
            }
        }
        session.directory_entry_paths.clear();
        session.directory_manifest = None;
        session.directory_seal = None;
    }

    /// Retries only residue authenticated as the aftermath of an already
    /// committed version. A marker or staging tree that does not exactly match
    /// this root-bound checkpoint is left untouched and fails closed.
    pub(super) fn recover_committed_cleanup_pending(&self) -> McpPlatformResult<()> {
        self.revalidate()?;
        let marker_present =
            open_relative_file_for_mutation(&self.writer_root, STAGING_SESSION_MARKER)?.is_some();
        let staging_present =
            open_relative_for_mutation(&self.writer_root, STAGING_DIRECTORY, true)?.is_some();
        let (signer, checkpoint, mut anchor) =
            load_managed_anchor(&self.binding, false, marker_present || staging_present)?;
        let Some(mut pending) = anchor.cleanup_pending.clone() else {
            return if marker_present || staging_present {
                Err(integrity_error())
            } else {
                Ok(())
            };
        };
        if !is_lower_hex_sha256(&pending.marker_sha256)
            || validated_directory_segment(&pending.managed_mcp_id).is_err()
            || validated_directory_segment(&pending.version).is_err()
        {
            return Err(integrity_error());
        }
        let checkpoint = match self.migrate_cleanup_pending_identities(
            signer.as_ref(),
            checkpoint.as_ref(),
            &mut anchor,
            &mut pending,
        )? {
            CleanupPendingMigration::Checkpoint(checkpoint) => checkpoint,
            CleanupPendingMigration::Cleared => return Ok(()),
        };
        if pending.phase == CleanupTransactionPhase::CleanupComplete {
            return self.finish_committed_cleanup(
                signer.as_ref(),
                checkpoint.as_ref(),
                &mut anchor,
                &pending,
            );
        }
        if !matches!(
            pending.phase,
            CleanupTransactionPhase::CleanupComplete
                | CleanupTransactionPhase::PreparedMarkerDeleteAuthorized
                | CleanupTransactionPhase::CommittedMarkerDeleteAuthorized
        ) {
            checked_prepared_identity(&pending.marker_identity)?;
            checked_prepared_identity(&pending.prepared_version_identity)?;
            verify_prepared_marker_identity(&self.writer_root, &pending)?;
        }
        let target = open_pending_committed_target(
            &self.writer_root,
            &pending.managed_mcp_id,
            &pending.version,
        )?;
        match pending.phase {
            CleanupTransactionPhase::Prepared => {
                return self.abort_prepared_cleanup(
                    signer.as_ref(),
                    checkpoint.as_ref(),
                    &mut anchor,
                    &pending,
                );
            }
            CleanupTransactionPhase::PreparedStagingDeleteAuthorized
            | CleanupTransactionPhase::PreparedMarkerDeleteAuthorized
                if target.is_none() =>
            {
                return self.abort_prepared_cleanup(
                    signer.as_ref(),
                    checkpoint.as_ref(),
                    &mut anchor,
                    &pending,
                );
            }
            CleanupTransactionPhase::PreparedStagingDeleteAuthorized
            | CleanupTransactionPhase::PreparedMarkerDeleteAuthorized => {
                return Err(integrity_error());
            }
            CleanupTransactionPhase::Committed
            | CleanupTransactionPhase::CommittedStagingDeleteAuthorized
            | CleanupTransactionPhase::CommittedMarkerDeleteAuthorized
            | CleanupTransactionPhase::CleanupComplete
                if target.is_none() =>
            {
                return Err(integrity_error());
            }
            _ => {}
        }
        let target = target.ok_or_else(integrity_error)?;
        let descriptor = read_relative_file(&target, ".goose-runtime-descriptor.json")?
            .ok_or_else(integrity_error)?;
        match pending.phase {
            CleanupTransactionPhase::Prepared => {
                return self.abort_prepared_cleanup(
                    signer.as_ref(),
                    checkpoint.as_ref(),
                    &mut anchor,
                    &pending,
                );
            }
            CleanupTransactionPhase::PreparedStagingDeleteAuthorized
            | CleanupTransactionPhase::PreparedMarkerDeleteAuthorized => {
                return Err(integrity_error());
            }
            CleanupTransactionPhase::CleanupComplete => return Err(integrity_error()),
            CleanupTransactionPhase::Committed
            | CleanupTransactionPhase::CommittedStagingDeleteAuthorized
            | CleanupTransactionPhase::CommittedMarkerDeleteAuthorized => {
                let version_anchor = anchor
                    .managed
                    .get(&pending.managed_mcp_id)
                    .and_then(|managed| managed.versions.get(&pending.version))
                    .ok_or_else(integrity_error)?;
                if pending
                    .expected_version
                    .as_ref()
                    .is_some_and(|expected| expected != version_anchor)
                {
                    return Err(integrity_error());
                }
                if FileIdentity::from_file(&target)?.durable_key()
                    != checked_prepared_identity(&pending.prepared_version_identity)?
                {
                    return Err(integrity_error());
                }
                verify_anchored_version(&target, &descriptor, &pending.version, version_anchor)?;
            }
        }

        self.finish_committed_cleanup(signer.as_ref(), checkpoint.as_ref(), &mut anchor, &pending)
    }

    /// Formats 1--5 did not bind the deletion objects to their authorization.
    /// They can only be cleared after all such objects are already absent.
    fn migrate_cleanup_pending_identities(
        &self,
        signer: &dyn IntegritySigner,
        checkpoint: Option<&crate::mcp_platform::repository::AnchorCheckpoint>,
        anchor: &mut ManagedRootAnchor,
        pending: &mut CommittedCleanupPending,
    ) -> McpPlatformResult<CleanupPendingMigration> {
        if anchor.format_version <= 5 {
            let marker_present =
                open_relative_file_for_mutation(&self.writer_root, STAGING_SESSION_MARKER)?
                    .is_some();
            let staging_present =
                open_relative_for_mutation(&self.writer_root, STAGING_DIRECTORY, true)?.is_some();
            if marker_present || staging_present {
                return Err(integrity_error());
            }
            anchor.cleanup_pending = None;
            anchor.format_version = 6;
            publish_managed_anchor(signer, checkpoint, anchor)?;
            return Ok(CleanupPendingMigration::Cleared);
        }
        if pending.phase == CleanupTransactionPhase::CleanupComplete {
            return Ok(CleanupPendingMigration::Checkpoint(checkpoint.cloned()));
        }
        let needs_staging_snapshot = pending.staging_expected
            && matches!(
                pending.phase,
                CleanupTransactionPhase::PreparedStagingDeleteAuthorized
                    | CleanupTransactionPhase::CommittedStagingDeleteAuthorized
            )
            && pending.staging_snapshot.is_none();
        let needs_prepared_identity = !matches!(
            pending.phase,
            CleanupTransactionPhase::PreparedMarkerDeleteAuthorized
        );
        if pending.marker_identity.is_some()
            && (!needs_prepared_identity || pending.prepared_version_identity.is_some())
            && !needs_staging_snapshot
        {
            return Ok(CleanupPendingMigration::Checkpoint(checkpoint.cloned()));
        }

        let marker = open_relative_file_for_mutation(&self.writer_root, STAGING_SESSION_MARKER)?
            .ok_or_else(integrity_error)?;
        if hex_digest(&read_verified_file(&marker)?) != pending.marker_sha256 {
            return Err(integrity_error());
        }
        pending.marker_identity = Some(FileIdentity::from_file(&marker)?.durable_key());

        if needs_prepared_identity && pending.prepared_version_identity.is_none() {
            if let Some(target) = open_pending_committed_target(
                &self.writer_root,
                &pending.managed_mcp_id,
                &pending.version,
            )? {
                let descriptor = read_relative_file(&target, ".goose-runtime-descriptor.json")?
                    .ok_or_else(integrity_error)?;
                let actual = anchored_version_from_tree(&target, &pending.version)?;
                match pending.phase {
                    CleanupTransactionPhase::Prepared => {
                        if pending.expected_version.as_ref() != Some(&actual) {
                            return Err(integrity_error());
                        }
                    }
                    CleanupTransactionPhase::Committed
                    | CleanupTransactionPhase::CommittedStagingDeleteAuthorized
                    | CleanupTransactionPhase::CommittedMarkerDeleteAuthorized => {
                        let expected = anchor
                            .managed
                            .get(&pending.managed_mcp_id)
                            .and_then(|managed| managed.versions.get(&pending.version))
                            .ok_or_else(integrity_error)?;
                        verify_anchored_version(&target, &descriptor, &pending.version, expected)?;
                    }
                    _ => return Err(integrity_error()),
                }
                pending.prepared_version_identity =
                    Some(FileIdentity::from_file(&target)?.durable_key());
            } else if matches!(
                pending.phase,
                CleanupTransactionPhase::Prepared
                    | CleanupTransactionPhase::PreparedStagingDeleteAuthorized
                    | CleanupTransactionPhase::PreparedMarkerDeleteAuthorized
            ) {
                let staging =
                    open_relative_directory_for_mutation(&self.writer_root, STAGING_DIRECTORY)?;
                let target = open_prepared_staging_target(&staging, pending)?;
                if pending.expected_version.as_ref()
                    != Some(&anchored_version_from_tree(&target, &pending.version)?)
                {
                    return Err(integrity_error());
                }
                pending.prepared_version_identity =
                    Some(FileIdentity::from_file(&target)?.durable_key());
            } else {
                return Err(integrity_error());
            }
        }

        if needs_staging_snapshot {
            // A format 6 checkpoint must have persisted the exact inventory
            // before any deletion authorization. Rebuilding it would turn
            // mutable current state into a new deletion grant.
            return Err(integrity_error());
        }

        anchor.cleanup_pending = Some(pending.clone());
        anchor.format_version = 6;
        Ok(CleanupPendingMigration::Checkpoint(Some(
            publish_managed_anchor(signer, checkpoint, anchor)?,
        )))
    }

    fn open_verified_cleanup_marker_for_delete(
        &self,
        pending: &CommittedCleanupPending,
    ) -> McpPlatformResult<Option<File>> {
        self.revalidate()?;
        let Some(marker) =
            open_relative_file_for_mutation(&self.writer_root, STAGING_SESSION_MARKER)?
        else {
            return Ok(None);
        };
        if hex_digest(&read_verified_file(&marker)?) != pending.marker_sha256
            || FileIdentity::from_file(&marker)?.durable_key()
                != checked_prepared_identity(&pending.marker_identity)?
        {
            return Err(integrity_error());
        }
        Ok(Some(marker))
    }

    fn revalidate_staging_delete_binding(
        &self,
        staging: &File,
        pending: &CommittedCleanupPending,
    ) -> McpPlatformResult<()> {
        self.revalidate()?;
        #[cfg(test)]
        if take_test_failure(TestFailurePoint::CleanupStagingBindingRevalidation) {
            return Err(integrity_error());
        }
        let expected_identity = checked_prepared_identity(&pending.staging_identity)?;
        if FileIdentity::from_file(staging)?.durable_key() != expected_identity {
            return Err(integrity_error());
        }
        let live_staging =
            open_relative_directory_for_mutation(&self.writer_root, STAGING_DIRECTORY)?;
        if FileIdentity::from_file(&live_staging)?.durable_key() != expected_identity {
            return Err(integrity_error());
        }
        Ok(())
    }

    fn delete_pending_staging_tree(
        &self,
        staging: File,
        pending: &CommittedCleanupPending,
    ) -> McpPlatformResult<()> {
        self.revalidate_staging_delete_binding(&staging, pending)?;
        let snapshot = pending
            .staging_snapshot
            .as_ref()
            .ok_or_else(integrity_error)?;
        let staging_binding = staging.try_clone().map_err(|_| unavailable())?;
        delete_authenticated_staging_directory(
            self,
            &staging_binding,
            staging,
            &[],
            snapshot,
            pending,
        )
    }

    fn finish_committed_cleanup(
        &self,
        signer: &dyn IntegritySigner,
        checkpoint: Option<&crate::mcp_platform::repository::AnchorCheckpoint>,
        anchor: &mut ManagedRootAnchor,
        pending: &CommittedCleanupPending,
    ) -> McpPlatformResult<()> {
        let marker_present =
            open_relative_file_for_mutation(&self.writer_root, STAGING_SESSION_MARKER)?.is_some();
        let staging_present =
            open_relative_for_mutation(&self.writer_root, STAGING_DIRECTORY, true)?.is_some();
        if pending.phase == CleanupTransactionPhase::CleanupComplete {
            if marker_present || staging_present {
                return Err(integrity_error());
            }
            anchor.cleanup_pending = None;
            anchor.format_version = 6;
            publish_managed_anchor(signer, checkpoint, anchor)?;
            return Ok(());
        }

        match pending.phase {
            CleanupTransactionPhase::Committed => {
                if !marker_present || (pending.staging_expected != staging_present) {
                    return Err(integrity_error());
                }
                verify_prepared_marker_identity(&self.writer_root, pending)?;
                if pending.staging_expected {
                    let staging =
                        open_relative_for_mutation(&self.writer_root, STAGING_DIRECTORY, true)?
                            .ok_or_else(integrity_error)?;
                    verify_pending_staging_tree(&staging, &pending.managed_mcp_id)?;
                    let staging_identity = FileIdentity::from_file(&staging)?.durable_key();
                    anchor
                        .cleanup_pending
                        .as_mut()
                        .ok_or_else(integrity_error)?
                        .staging_identity = Some(staging_identity);
                    anchor
                        .cleanup_pending
                        .as_mut()
                        .ok_or_else(integrity_error)?
                        .staging_snapshot = Some(capture_staging_snapshot(&staging)?);
                }
                anchor
                    .cleanup_pending
                    .as_mut()
                    .ok_or_else(integrity_error)?
                    .phase = if pending.staging_expected {
                    CleanupTransactionPhase::CommittedStagingDeleteAuthorized
                } else {
                    CleanupTransactionPhase::CommittedMarkerDeleteAuthorized
                };
                anchor.format_version = 6;
                let authorized = publish_managed_anchor(signer, checkpoint, anchor)?;
                let authorized_pending =
                    anchor.cleanup_pending.clone().ok_or_else(integrity_error)?;
                return self.finish_committed_cleanup(
                    signer,
                    Some(&authorized),
                    anchor,
                    &authorized_pending,
                );
            }
            CleanupTransactionPhase::CommittedStagingDeleteAuthorized => {
                if !marker_present {
                    return Err(integrity_error());
                }
                verify_prepared_marker_identity(&self.writer_root, pending)?;
                if staging_present {
                    let staging =
                        open_relative_for_mutation(&self.writer_root, STAGING_DIRECTORY, true)?
                            .ok_or_else(integrity_error)?;
                    self.delete_pending_staging_tree(staging, pending)?;
                }
                self.revalidate()?;
                anchor
                    .cleanup_pending
                    .as_mut()
                    .ok_or_else(integrity_error)?
                    .phase = CleanupTransactionPhase::CommittedMarkerDeleteAuthorized;
                anchor.format_version = 6;
                let authorized = publish_managed_anchor(signer, checkpoint, anchor)?;
                let authorized_pending =
                    anchor.cleanup_pending.clone().ok_or_else(integrity_error)?;
                return self.finish_committed_cleanup(
                    signer,
                    Some(&authorized),
                    anchor,
                    &authorized_pending,
                );
            }
            CleanupTransactionPhase::CommittedMarkerDeleteAuthorized => {
                if staging_present {
                    return Err(integrity_error());
                }
                if let Some(marker) = self.open_verified_cleanup_marker_for_delete(pending)? {
                    #[cfg(test)]
                    if take_test_failure(TestFailurePoint::CleanupMarkerDisposition) {
                        return Err(cleanup_error());
                    }
                    set_delete_on_close(&marker, true)?;
                }
                self.revalidate()?;
                anchor
                    .cleanup_pending
                    .as_mut()
                    .ok_or_else(integrity_error)?
                    .phase = CleanupTransactionPhase::CleanupComplete;
                anchor.format_version = 6;
                let complete = publish_managed_anchor(signer, checkpoint, anchor)?;
                anchor.cleanup_pending = None;
                publish_managed_anchor(signer, Some(&complete), anchor)?;
                Ok(())
            }
            _ => Err(integrity_error()),
        }
    }

    fn abort_prepared_cleanup(
        &self,
        signer: &dyn IntegritySigner,
        checkpoint: Option<&crate::mcp_platform::repository::AnchorCheckpoint>,
        anchor: &mut ManagedRootAnchor,
        pending: &CommittedCleanupPending,
    ) -> McpPlatformResult<()> {
        let expected = pending
            .expected_version
            .as_ref()
            .ok_or_else(integrity_error)?;
        let managed_anchor = anchor
            .managed
            .get(&pending.managed_mcp_id)
            .ok_or_else(integrity_error)?;
        if managed_anchor.versions.contains_key(&pending.version) {
            return Err(integrity_error());
        }
        match pending.phase {
            CleanupTransactionPhase::Prepared => {
                let staging =
                    open_relative_directory_for_mutation(&self.writer_root, STAGING_DIRECTORY)?;
                verify_prepared_marker_identity(&self.writer_root, pending)?;
                self.revalidate_staging_delete_binding(&staging, pending)?;
                if let Some(target) = open_pending_committed_target(
                    &self.writer_root,
                    &pending.managed_mcp_id,
                    &pending.version,
                )? {
                    if anchored_version_from_tree(&target, &pending.version)? != *expected
                        || FileIdentity::from_file(&target)?.durable_key()
                            != checked_prepared_identity(&pending.prepared_version_identity)?
                    {
                        return Err(integrity_error());
                    }
                    delete_verified_tree(target)?;
                    self.revalidate()?;
                }
                anchor
                    .cleanup_pending
                    .as_mut()
                    .ok_or_else(integrity_error)?
                    .phase = CleanupTransactionPhase::PreparedStagingDeleteAuthorized;
                anchor.format_version = 6;
                let authorized = publish_managed_anchor(signer, checkpoint, anchor)?;
                let authorized_pending =
                    anchor.cleanup_pending.clone().ok_or_else(integrity_error)?;
                self.abort_prepared_cleanup(signer, Some(&authorized), anchor, &authorized_pending)
            }
            CleanupTransactionPhase::PreparedStagingDeleteAuthorized => {
                verify_prepared_marker_identity(&self.writer_root, pending)?;
                if let Some(staging) =
                    open_relative_for_mutation(&self.writer_root, STAGING_DIRECTORY, true)?
                {
                    self.delete_pending_staging_tree(staging, pending)?;
                }
                self.revalidate()?;
                anchor
                    .cleanup_pending
                    .as_mut()
                    .ok_or_else(integrity_error)?
                    .phase = CleanupTransactionPhase::PreparedMarkerDeleteAuthorized;
                anchor.format_version = 6;
                let authorized = publish_managed_anchor(signer, checkpoint, anchor)?;
                let authorized_pending =
                    anchor.cleanup_pending.clone().ok_or_else(integrity_error)?;
                self.abort_prepared_cleanup(signer, Some(&authorized), anchor, &authorized_pending)
            }
            CleanupTransactionPhase::PreparedMarkerDeleteAuthorized => {
                if open_relative_for_mutation(&self.writer_root, STAGING_DIRECTORY, true)?.is_some()
                {
                    return Err(integrity_error());
                }
                if let Some(marker) = self.open_verified_cleanup_marker_for_delete(pending)? {
                    #[cfg(test)]
                    if take_test_failure(TestFailurePoint::CleanupMarkerDisposition) {
                        return Err(cleanup_error());
                    }
                    set_delete_on_close(&marker, true)?;
                }
                self.revalidate()?;
                anchor.cleanup_pending = None;
                anchor.format_version = 6;
                publish_managed_anchor(signer, checkpoint, anchor).map(|_| ())
            }
            _ => Err(integrity_error()),
        }
    }

    pub(super) fn create_staging(&self) -> McpPlatformResult<()> {
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        let (_, _, anchor) = load_managed_anchor(&self.binding, false, false)?;
        ensure_no_active_clear_pending(&anchor)?;
        if session.recovery_marker.is_some() || session.staging.is_some() {
            return Err(integrity_error());
        }
        if let Err(error) = create_relative(
            &self.writer_root,
            STAGING_SESSION_MARKER,
            false,
            FILE_READ_ATTRIBUTES | FILE_READ_DATA | FILE_WRITE_DATA | DELETE | SYNCHRONIZE,
            &mut session.recovery_marker,
        ) {
            return reject_session(&mut session, error);
        }
        let marker_validation = session
            .recovery_marker
            .as_ref()
            .ok_or_else(integrity_error)
            .and_then(|marker| verify_session_object(marker, false));
        if let Err(error) = marker_validation {
            return reject_session(&mut session, error);
        }
        if let Some(SessionObject::Ready(marker)) = session.recovery_marker.as_mut() {
            // This marker is the durable fail-closed witness if descendant
            // cleanup cannot finish during unwinding.
            marker.file.delete_on_drop = false;
        } else {
            return reject_session(&mut session, integrity_error());
        }
        if let Err(error) = create_relative(
            &self.writer_root,
            STAGING_DIRECTORY,
            true,
            FILE_READ_ATTRIBUTES
                | FILE_READ_DATA
                | FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_TRAVERSE
                | DELETE
                | SYNCHRONIZE,
            &mut session.staging,
        ) {
            return reject_session(&mut session, error);
        }
        let staging_validation = session
            .staging
            .as_ref()
            .ok_or_else(integrity_error)
            .and_then(|staging| verify_session_object(staging, true));
        if let Err(error) = staging_validation {
            return reject_session(&mut session, error);
        }
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        Ok(())
    }

    pub(super) fn write_verified_payload(
        &self,
        bytes: &[u8],
        expected_sha256: &str,
    ) -> McpPlatformResult<()> {
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        if hex_digest(bytes) != expected_sha256 {
            return reject_session(&mut session, integrity_error());
        }
        let staging_validation = session
            .staging
            .as_ref()
            .ok_or_else(integrity_error)
            .and_then(|staging| verify_session_object(staging, true));
        if let Err(error) = staging_validation {
            return reject_session(&mut session, error);
        }
        if session.payload.is_some() {
            return Err(integrity_error());
        }
        let staging = ready_session_object(session.staging.as_ref(), true)?
            .file
            .try_clone()
            .map_err(|_| unavailable())?;
        let payload_creation = create_relative(
            &staging,
            PAYLOAD_FILE,
            false,
            FILE_READ_ATTRIBUTES | FILE_READ_DATA | FILE_WRITE_DATA | DELETE | SYNCHRONIZE,
            &mut session.payload,
        );
        if let Err(error) = payload_creation {
            return reject_session(&mut session, error);
        }
        let write_result = (|| {
            let payload = ready_session_object_mut(session.payload.as_mut(), false)?;
            verify_relative_object(payload, false)?;
            #[cfg(test)]
            if take_test_failure(TestFailurePoint::PartialWrite) {
                payload
                    .file
                    .write_all(&bytes[..bytes.len().min(1)])
                    .map_err(|_| unavailable())?;
                return Err(unavailable());
            }
            payload.file.write_all(bytes).map_err(|_| unavailable())?;
            payload.file.flush().map_err(|_| unavailable())?;
            verify_relative_object(payload, false)
        })();
        if let Err(error) = write_result {
            return reject_session(&mut session, error);
        }
        session.expected_sha256 = Some(expected_sha256.to_owned());
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        Ok(())
    }

    pub(super) fn verify_regular_payload(&self) -> McpPlatformResult<()> {
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        let expected_sha256 = match session.expected_sha256.as_deref() {
            Some(expected_sha256) => expected_sha256.to_owned(),
            None => return reject_session(&mut session, integrity_error()),
        };
        let verification_result = (|| {
            let payload = ready_session_object_mut(session.payload.as_mut(), false)?;
            verify_relative_object(payload, false)?;
            payload
                .file
                .seek(SeekFrom::Start(0))
                .map_err(|_| unavailable())?;
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 8192];
            loop {
                let read = payload.file.read(&mut buffer).map_err(|_| unavailable())?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
            }
            payload
                .file
                .seek(SeekFrom::Start(0))
                .map_err(|_| unavailable())?;
            #[cfg(test)]
            if take_test_failure(TestFailurePoint::PostVerification) {
                return Err(integrity_error());
            }
            if hex_encoded_digest(&digest.finalize()) != expected_sha256 {
                return Err(integrity_error());
            }
            Ok(())
        })();
        if let Err(error) = verification_result {
            return reject_session(&mut session, error);
        }
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        Ok(())
    }

    pub(super) fn create_directory_staging(
        &self,
        manifest: &DirectoryPayloadManifest,
    ) -> McpPlatformResult<()> {
        validate_directory_manifest(manifest)?;
        self.create_staging()?;
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        let result = (|| {
            let marker = ready_session_object_mut(session.recovery_marker.as_mut(), false)?;
            marker
                .file
                .write_all(&manifest_marker(manifest))
                .map_err(|_| unavailable())?;
            marker.file.flush().map_err(|_| unavailable())?;
            verify_relative_object(marker, false)?;
            session.directory_manifest = Some(manifest.clone());
            Ok(())
        })();
        if let Err(error) = result {
            return reject_session(&mut session, error);
        }
        Ok(())
    }

    pub(super) fn materialize_verified_directory(
        &self,
        payloads: &[DirectoryPayload<'_>],
    ) -> McpPlatformResult<()> {
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        let result = materialize_directory_session(&mut session, payloads);
        if let Err(error) = result {
            return reject_session(&mut session, error);
        }
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        Ok(())
    }

    pub(super) fn seal_staged_directory(&self) -> McpPlatformResult<()> {
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        let result = seal_directory_session(&mut session);
        if let Err(error) = result {
            return Err(error);
        }
        if let Err(error) = self.revalidate() {
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn promote_staged_directory(
        &self,
        committed_leaf: RuntimeStorageCommittedLeaf,
    ) -> McpPlatformResult<()> {
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        let (_, _, anchor) = load_managed_anchor(&self.binding, false, false)?;
        ensure_no_active_clear_pending(&anchor)?;
        let result = (|| {
            let manifest = session
                .directory_manifest
                .as_ref()
                .ok_or_else(integrity_error)?;
            validate_manifest_for_committed_leaf(manifest, committed_leaf)?;
            let seal = session.directory_seal.clone().ok_or_else(integrity_error)?;
            verify_directory_seal(&session, &seal)?;
            verify_directory_session(&mut session)?;
            let destination = committed_leaf_name(committed_leaf);
            let mut final_tree =
                create_sealed_final_tree(&session, &self.writer_root, destination, &[])?;
            self.revalidate()?;
            verify_sealed_final_tree(
                &final_tree,
                &session
                    .directory_manifest
                    .as_ref()
                    .ok_or_else(integrity_error)?
                    .entries
                    .iter()
                    .map(|entry| (entry, entry.relative_segments.clone()))
                    .collect::<Vec<_>>(),
            )?;
            discard_uncommitted_staging(&mut session)?;
            discard_session_object(&mut session.recovery_marker, false)?;
            session.directory_manifest = None;
            session.directory_seal = None;
            if let Err(error) = final_tree.release() {
                return final_tree.destroy_delete_on_close().and(Err(error));
            }
            Ok(())
        })();
        result
    }

    /// Promotes one sealed version.  The first version can claim the whole
    /// installations leaf; later versions are appended below the already
    /// committed managed id with no-replace semantics.
    pub(super) fn promote_staged_managed_installation(&self) -> McpPlatformResult<()> {
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        let result = (|| {
            let manifest = session
                .directory_manifest
                .clone()
                .ok_or_else(integrity_error)?;
            let (managed_mcp_id, version) = managed_installation_manifest_target(&manifest)?;
            let seal = session.directory_seal.clone().ok_or_else(integrity_error)?;
            verify_directory_seal(&session, &seal)?;
            verify_directory_session(&mut session)?;

            let initial_installations =
                !relative_object_exists(&self.writer_root, "platform.installations")?;
            let version_path = vec![
                managed_mcp_id.clone(),
                "versions".to_owned(),
                version.clone(),
            ];
            let marker_identity = ready_session_object(session.recovery_marker.as_ref(), false)?
                .identity
                .durable_key();
            let staging_identity = ready_session_object(session.staging.as_ref(), true)?
                .identity
                .durable_key();
            establish_managed_root_lineage(&self.binding)?;
            let (signer, checkpoint, mut anchor) =
                load_managed_anchor(&self.binding, initial_installations, !initial_installations)?;
            if initial_installations && checkpoint.is_some() {
                // A root-authorized managed state without its installations
                // tree is deletion/recreation evidence, never a new root.
                return Err(integrity_error());
            }
            if anchor.cleanup_pending.is_some() {
                return Err(integrity_error());
            }
            ensure_no_active_clear_pending(&anchor)?;
            let managed_anchor =
                anchor
                    .managed
                    .entry(managed_mcp_id.clone())
                    .or_insert_with(|| ManagedAnchorState {
                        versions: BTreeMap::new(),
                        active: None,
                        active_clear_pending: None,
                    });
            if managed_anchor.versions.contains_key(&version) {
                return Err(integrity_error());
            }

            let mut final_tree = if initial_installations {
                create_sealed_final_tree(
                    &session,
                    &self.writer_root,
                    "platform.installations",
                    &[],
                )?
            } else {
                let installations = open_relative_directory_for_mutation(
                    &self.writer_root,
                    "platform.installations",
                )?;
                let managed =
                    open_relative_directory_for_mutation(&installations, &managed_mcp_id)?;
                let versions = open_relative_directory_for_mutation(&managed, "versions")?;
                create_sealed_final_tree(&session, &versions, &version, &version_path)?
            };
            #[cfg(test)]
            record_managed_promotion_step(ManagedPromotionStep::DirectFinalTree);
            let final_version_index = initial_installations
                .then(|| {
                    final_tree
                        .paths
                        .iter()
                        .position(|path| path == &version_path)
                        .ok_or_else(integrity_error)
                })
                .transpose()?;
            let anchored_version = anchored_version_from_tree(
                &final_tree_version_object(&final_tree, final_version_index)?.file,
                &version,
            )?;
            let cleanup_pending = CommittedCleanupPending {
                managed_mcp_id: managed_mcp_id.clone(),
                version: version.clone(),
                marker_sha256: hex_digest(&manifest_marker(&manifest)),
                staging_expected: true,
                phase: CleanupTransactionPhase::Prepared,
                expected_version: Some(anchored_version.clone()),
                marker_identity: Some(marker_identity),
                prepared_version_identity: Some(
                    final_tree_version_object(&final_tree, final_version_index)?
                        .identity
                        .durable_key(),
                ),
                staging_identity: Some(staging_identity),
                staging_snapshot: Some(capture_session_staging_snapshot(&session)?),
            };

            // This checkpoint binds the still-delete-on-close final object,
            // marker, and sealed staging inventory before it can be anchored.
            anchor.cleanup_pending = Some(cleanup_pending);
            anchor.format_version = 6;
            let prepared_checkpoint =
                publish_managed_anchor(signer.as_ref(), checkpoint.as_ref(), &anchor)?;
            #[cfg(test)]
            record_managed_promotion_step(ManagedPromotionStep::PreparedAnchor);

            #[cfg(test)]
            if take_test_failure(TestFailurePoint::PreparedRename) {
                return Err(unavailable());
            }

            if let Err(_release_error) = final_tree.release() {
                // No committed anchor exists yet.  The prepared checkpoint is
                // the sole authenticated authority for this residue.  It must
                // survive even if a best-effort reaffirmation cannot be read
                // back: publishing the old anchor here could succeed before
                // its confirmation fails, permanently stranding the marker
                // and staging tree without recovery authority.
                if final_tree.destroy_delete_on_close().is_err() {
                    session.retain_for_authenticated_recovery();
                    return Err(prepared_recovery_required_error());
                }
                let _ =
                    reaffirm_prepared_anchor(signer.as_ref(), Some(&prepared_checkpoint), &anchor);
                session.retain_for_authenticated_recovery();
                return Err(prepared_recovery_required_error());
            }
            #[cfg(test)]
            record_managed_promotion_step(ManagedPromotionStep::FinalTreeReleased);

            // A committed anchor is never published while any final-tree
            // handle remains delete-on-close. If this process now dies, the
            // prepared checkpoint can remove the uncommitted tree, while a
            // committed checkpoint always points at a released tree.
            if let Err(error) = self.revalidate() {
                session.retain_for_authenticated_recovery();
                return Err(error);
            }
            match anchored_version_from_tree(
                &final_tree_version_object(&final_tree, final_version_index)?.file,
                &version,
            ) {
                Ok(observed) if observed == anchored_version => {}
                Ok(_) => {
                    session.retain_for_authenticated_recovery();
                    return Err(integrity_error());
                }
                Err(error) => {
                    session.retain_for_authenticated_recovery();
                    return Err(error);
                }
            }
            #[cfg(test)]
            if take_test_failure(TestFailurePoint::RenamedBeforeCommit) {
                session.retain_for_authenticated_recovery();
                return Err(unavailable());
            }

            anchor
                .managed
                .get_mut(&managed_mcp_id)
                .ok_or_else(integrity_error)?
                .versions
                .insert(version.clone(), anchored_version);

            anchor
                .cleanup_pending
                .as_mut()
                .ok_or_else(integrity_error)?
                .phase = CleanupTransactionPhase::Committed;
            anchor.format_version = 6;
            let committed_checkpoint = match publish_managed_anchor(
                signer.as_ref(),
                Some(&prepared_checkpoint),
                &anchor,
            ) {
                Ok(checkpoint) => checkpoint,
                Err(error) => {
                    session.retain_for_authenticated_recovery();
                    return Err(error);
                }
            };
            #[cfg(test)]
            record_managed_promotion_step(ManagedPromotionStep::CommittedAnchor);

            // Every residue deletion is separately authorized by the
            // root-bound checkpoint before its kernel disposition is issued.
            // A restart can therefore distinguish expected disappearance from
            // a missing prerequisite of an earlier phase.
            anchor
                .cleanup_pending
                .as_mut()
                .ok_or_else(integrity_error)?
                .phase = CleanupTransactionPhase::CommittedStagingDeleteAuthorized;
            let cleanup_checkpoint =
                publish_managed_anchor(signer.as_ref(), Some(&committed_checkpoint), &anchor)?;
            if discard_uncommitted_staging(&mut session).is_err() {
                return Err(cleanup_pending_error());
            }
            self.revalidate()?;
            anchor
                .cleanup_pending
                .as_mut()
                .ok_or_else(integrity_error)?
                .phase = CleanupTransactionPhase::CommittedMarkerDeleteAuthorized;
            let marker_checkpoint =
                publish_managed_anchor(signer.as_ref(), Some(&cleanup_checkpoint), &anchor)?;
            if discard_recovery_marker(&mut session).is_err() {
                return Err(cleanup_pending_error());
            }
            self.revalidate()?;
            #[cfg(test)]
            if take_test_failure(TestFailurePoint::CleanupCheckpoint) {
                return Err(unavailable());
            }
            anchor
                .cleanup_pending
                .as_mut()
                .ok_or_else(integrity_error)?
                .phase = CleanupTransactionPhase::CleanupComplete;
            let complete_checkpoint =
                publish_managed_anchor(signer.as_ref(), Some(&marker_checkpoint), &anchor)?;
            anchor.cleanup_pending = None;
            publish_managed_anchor(signer.as_ref(), Some(&complete_checkpoint), &anchor)?;
            Ok(())
        })();
        if result.is_err() {
            return result;
        }
        // A successful no-replace promotion is irrevocable. Do not turn a
        // committed install into a retryable error merely because a later
        // root revalidation races with post-commit residue cleanup.
        Ok(())
    }

    /// Atomically replaces `active.json` only after the target version and its
    /// descriptor are reached through no-reparse handles and still equal the
    /// payload sealed by this lease.
    pub(super) fn activate_verified_managed_installation(
        &self,
        managed_mcp_id: &str,
        version: &str,
        descriptor: &[u8],
        descriptor_sha256: &str,
    ) -> McpPlatformResult<()> {
        if hex_digest(descriptor) != descriptor_sha256 {
            return Err(integrity_error());
        }
        let mut session = self.session.lock().map_err(|_| unavailable())?;
        if let Err(error) = self.revalidate() {
            return reject_session(&mut session, error);
        }
        let result = (|| {
            let expected = session
                .expected_sha256
                .as_deref()
                .ok_or_else(integrity_error)?;
            if expected != descriptor_sha256 {
                return Err(integrity_error());
            }
            let payload = ready_session_object_mut(session.payload.as_mut(), false)?;
            verify_session_write_lock(payload, false)?;
            verify_file_bytes(&mut payload.file, descriptor)?;
            let installations =
                open_relative_directory_for_mutation(&self.writer_root, "platform.installations")?;
            let managed = open_relative_directory_for_mutation(&installations, managed_mcp_id)?;
            let versions = open_relative_directory_for_mutation(&managed, "versions")?;
            let target = open_relative_directory_for_mutation(&versions, version)?;
            if read_relative_file(&target, ".goose-runtime-descriptor.json")?.as_deref()
                != Some(descriptor)
            {
                return Err(integrity_error());
            }
            let (signer, checkpoint, mut anchor) = load_managed_anchor(&self.binding, false, true)?;
            ensure_no_active_clear_pending(&anchor)?;
            let managed_anchor = anchor
                .managed
                .get(managed_mcp_id)
                .ok_or_else(integrity_error)?;
            verify_current_active_pointer(&managed, managed_anchor)?;
            let version_anchor = managed_anchor
                .versions
                .get(version)
                .cloned()
                .ok_or_else(integrity_error)?;
            verify_anchored_version(&target, descriptor, version, &version_anchor)?;
            anchor
                .managed
                .get_mut(managed_mcp_id)
                .ok_or_else(integrity_error)?
                .active = Some(AnchoredActive {
                version: version.to_owned(),
                descriptor_sha256: descriptor_sha256.to_owned(),
            });
            anchor.format_version = 6;
            // Publish before replacing the mutable pointer. If the pointer
            // replacement is interrupted, readers fail closed instead of
            // accepting a pointer that was never root-authorized.
            publish_managed_anchor(signer.as_ref(), checkpoint.as_ref(), &anchor)?;
            rename_relative_replace(&payload.file, &managed, "active.json")?;
            payload.file.delete_on_drop = false;
            session.payload.take();
            discard_uncommitted(&mut session)
        })();
        if result.is_err() {
            return reject_session(&mut session, result.unwrap_err());
        }
        self.revalidate()
    }

    pub(super) fn clear_managed_installation_activation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<()> {
        let session = self.session.lock().map_err(|_| unavailable())?;
        if session.staging.is_some()
            || session.payload.is_some()
            || session.directory_manifest.is_some()
        {
            return Err(integrity_error());
        }
        self.revalidate()?;
        let installations =
            open_relative_directory_for_mutation(&self.writer_root, "platform.installations")?;
        let managed = open_relative_directory_for_mutation(&installations, managed_mcp_id)?;
        let (signer, checkpoint, mut anchor) = load_managed_anchor(&self.binding, false, true)?;
        let managed_anchor = anchor
            .managed
            .get(managed_mcp_id)
            .ok_or_else(integrity_error)?;
        if let Some(pending) = managed_anchor.active_clear_pending.clone() {
            if managed_anchor.active.as_ref() != Some(&pending) {
                return Err(integrity_error());
            }
            if let Some(active) = open_relative_file_for_mutation(&managed, "active.json")? {
                let active_bytes = read_verified_file(&active)?;
                if pending.version != parse_active_version(&active_bytes)?
                    || pending.descriptor_sha256 != hex_digest(&active_bytes)
                {
                    return Err(integrity_error());
                }
                let versions = open_relative_directory_for_mutation(&managed, "versions")?;
                let target = open_relative_directory_for_mutation(&versions, &pending.version)?;
                verify_anchored_version(
                    &target,
                    &active_bytes,
                    &pending.version,
                    managed_anchor
                        .versions
                        .get(&pending.version)
                        .ok_or_else(integrity_error)?,
                )?;
                set_delete_on_close(&active, true)?;
                drop(active);
            }
            let state = anchor
                .managed
                .get_mut(managed_mcp_id)
                .ok_or_else(integrity_error)?;
            state.active = None;
            state.active_clear_pending = None;
            anchor.format_version = 6;
            publish_managed_anchor(signer.as_ref(), checkpoint.as_ref(), &anchor)?;
            return self.revalidate();
        }
        verify_current_active_pointer(&managed, managed_anchor)?;
        let Some(active_anchor) = managed_anchor.active.clone() else {
            return Ok(());
        };
        anchor
            .managed
            .get_mut(managed_mcp_id)
            .ok_or_else(integrity_error)?
            .active_clear_pending = Some(active_anchor);
        anchor.format_version = 6;
        let prepared = publish_managed_anchor(signer.as_ref(), checkpoint.as_ref(), &anchor)?;
        let active = open_relative_file_for_mutation(&managed, "active.json")?
            .ok_or_else(integrity_error)?;
        set_delete_on_close(&active, true)?;
        drop(active);
        #[cfg(test)]
        if take_test_failure(TestFailurePoint::ClearActivationFinalize) {
            return Err(unavailable());
        }
        let state = anchor
            .managed
            .get_mut(managed_mcp_id)
            .ok_or_else(integrity_error)?;
        state.active = None;
        state.active_clear_pending = None;
        publish_managed_anchor(signer.as_ref(), Some(&prepared), &anchor)?;
        self.revalidate()
    }

    pub(super) fn managed_installation_version_is_verified(
        &self,
        managed_mcp_id: &str,
        version: &str,
    ) -> McpPlatformResult<bool> {
        let session = self.session.lock().map_err(|_| unavailable())?;
        if session.staging.is_some()
            || session.payload.is_some()
            || session.directory_manifest.is_some()
        {
            return Err(integrity_error());
        }
        self.revalidate()?;
        let installations =
            open_relative_directory_for_mutation(&self.writer_root, "platform.installations")?;
        let managed = open_relative_directory_for_mutation(&installations, managed_mcp_id)?;
        let versions = open_relative_directory_for_mutation(&managed, "versions")?;
        let Some(target) = open_relative_for_read(&versions, version, true)? else {
            return Ok(false);
        };
        let (target, _) = verify_read_handle(target, true)?;
        let descriptor = read_relative_file(&target, ".goose-runtime-descriptor.json")?;
        let (_, _, anchor) = load_managed_anchor(&self.binding, false, true)?;
        let verified = descriptor
            .as_deref()
            .map(|descriptor| {
                let version_anchor = anchor
                    .managed
                    .get(managed_mcp_id)
                    .and_then(|managed| managed.versions.get(version))
                    .ok_or_else(integrity_error)?;
                verify_anchored_version(&target, descriptor, version, version_anchor)
            })
            .transpose()?
            .is_some();
        self.revalidate()?;
        Ok(verified)
    }

    pub(super) fn read_verified_managed_installation_activation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<Option<Vec<u8>>> {
        let session = self.session.lock().map_err(|_| unavailable())?;
        if session.staging.is_some()
            || session.payload.is_some()
            || session.directory_manifest.is_some()
        {
            return Err(integrity_error());
        }
        self.revalidate()?;
        let installations =
            open_relative_directory_for_mutation(&self.writer_root, "platform.installations")?;
        let managed = open_relative_directory_for_mutation(&installations, managed_mcp_id)?;
        let Some(active) = open_relative_for_read(&managed, "active.json", false)? else {
            let (_, _, anchor) = load_managed_anchor(&self.binding, false, true)?;
            if anchor.managed.get(managed_mcp_id).is_some_and(|managed| {
                managed.active.is_some() || managed.active_clear_pending.is_some()
            }) {
                return Err(integrity_error());
            }
            return Ok(None);
        };
        let (active, _) = verify_read_handle(active, false)?;
        let bytes = read_verified_file(&active)?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| integrity_error())?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(integrity_error)?;
        validated_directory_segment(version)?;
        let (_, _, anchor) = load_managed_anchor(&self.binding, false, true)?;
        let managed_anchor = anchor
            .managed
            .get(managed_mcp_id)
            .ok_or_else(integrity_error)?;
        if managed_anchor.active_clear_pending.is_some() {
            return Err(integrity_error());
        }
        let active_anchor = managed_anchor.active.as_ref().ok_or_else(integrity_error)?;
        if active_anchor.version != version || active_anchor.descriptor_sha256 != hex_digest(&bytes)
        {
            return Err(integrity_error());
        }
        let versions = open_relative_directory_for_mutation(&managed, "versions")?;
        let target = open_relative_directory_for_mutation(&versions, version)?;
        verify_anchored_version(
            &target,
            &bytes,
            version,
            managed_anchor
                .versions
                .get(version)
                .ok_or_else(integrity_error)?,
        )?;
        self.revalidate()?;
        Ok(Some(bytes))
    }
}

impl std::fmt::Debug for ManagedRootCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedRootCapability")
            .finish_non_exhaustive()
    }
}

/// Reads a regular file through one root-bound, no-reparse handle walk.
///
/// The caller never receives a child path or handle. Each component is opened
/// relative to the preceding verified directory handle, so replacing a path
/// after preflight cannot redirect the eventual read.
pub(super) fn read_regular_relative(
    root: File,
    components: &[&str],
    maximum: u64,
) -> McpPlatformResult<Option<Vec<u8>>> {
    let (root_guard, root_identity) = verify_read_handle(root, true)?;
    let mut current = root_guard.try_clone().map_err(|_| unavailable())?;
    if components.is_empty() {
        return Err(integrity_error());
    }

    for (index, component) in components.iter().enumerate() {
        let directory = index + 1 < components.len();
        let Some(next) = open_relative_for_read(&current, component, directory)? else {
            return Ok(None);
        };
        let (verified, _) = verify_read_handle(next, directory)?;
        current = verified;
    }

    let identity = FileIdentity::from_file(&current)?;
    let length = current.metadata().map_err(|_| unavailable())?.len();
    if length > maximum {
        return Err(integrity_error());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length).map_err(|_| unavailable())?);
    let mut limited = current.take(maximum.saturating_add(1));
    limited.read_to_end(&mut bytes).map_err(|_| unavailable())?;
    let file = limited.into_inner();
    if bytes.len() as u64 != length
        || bytes.len() as u64 > maximum
        || FileIdentity::from_file(&file)? != identity
        || FileIdentity::from_file(&root_guard)? != root_identity
    {
        return Err(integrity_error());
    }
    verify_read_handle(file, false)?;
    Ok(Some(bytes))
}

fn open_relative_for_read(
    parent: &File,
    leaf: &str,
    directory: bool,
) -> McpPlatformResult<Option<File>> {
    let mut name = validated_read_leaf_name(leaf)?;
    let mut unicode = UnicodeString {
        length: ((name.len() - 1) * 2)
            .try_into()
            .map_err(|_| unavailable())?,
        maximum_length: (name.len() * 2).try_into().map_err(|_| unavailable())?,
        buffer: name.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle() as HANDLE,
        object_name: &mut unicode,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_READ_ATTRIBUTES
                | SYNCHRONIZE
                | if directory {
                    FILE_READ_DATA | FILE_TRAVERSE
                } else {
                    FILE_READ_DATA
                },
            &mut attributes,
            &mut io_status,
            std::ptr::null_mut(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_REPARSE_POINT
                | if directory {
                    FILE_DIRECTORY_FILE
                } else {
                    FILE_NON_DIRECTORY_FILE
                },
            std::ptr::null_mut(),
            0,
        )
    };
    if status == STATUS_OBJECT_NAME_NOT_FOUND
        || status == STATUS_OBJECT_PATH_NOT_FOUND
        || io_status.status == STATUS_OBJECT_NAME_NOT_FOUND
        || io_status.status == STATUS_OBJECT_PATH_NOT_FOUND
    {
        close_if_valid_handle(handle);
        return Ok(None);
    }
    if status < 0 || io_status.status < 0 || !is_valid_handle(handle) {
        close_if_valid_handle(handle);
        return Err(integrity_error());
    }
    Ok(Some(unsafe { File::from_raw_handle(handle as _) }))
}

fn verify_read_handle(file: File, directory: bool) -> McpPlatformResult<(File, FileIdentity)> {
    let identity = FileIdentity::from_file(&file)?;
    let attributes = file_attributes(&file)?;
    let expected = if directory {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        0
    };
    if attributes
        & (FILE_ATTRIBUTE_DIRECTORY
            | FILE_ATTRIBUTE_HIDDEN
            | FILE_ATTRIBUTE_SYSTEM
            | FILE_ATTRIBUTE_REPARSE_POINT)
        != expected
        || (!directory && FileIdentity::link_count(&file)? != 1)
        || has_named_streams(&file)?
    {
        return Err(integrity_error());
    }
    Ok((file, identity))
}

fn validated_read_leaf_name(value: &str) -> McpPlatformResult<Vec<u16>> {
    if value.is_empty()
        || value.contains(['/', '\\', ':', '\0', '~'])
        || value == "."
        || value == ".."
        || value.ends_with(['.', ' '])
        || is_windows_device_name(value)
    {
        return Err(integrity_error());
    }
    let mut encoded: Vec<u16> = value.encode_utf16().collect();
    encoded.push(0);
    Ok(encoded)
}

const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034_u32 as i32;
const STATUS_OBJECT_PATH_NOT_FOUND: i32 = 0xC000_003A_u32 as i32;

fn validate_directory_manifest(manifest: &DirectoryPayloadManifest) -> McpPlatformResult<()> {
    if manifest.entries.is_empty() || manifest.entries.len() > MAX_DIRECTORY_FILES {
        return Err(integrity_error());
    }
    let mut total = 0_u64;
    let mut paths = BTreeSet::new();
    for entry in &manifest.entries {
        if entry.relative_segments.is_empty()
            || entry.relative_segments.len() > MAX_DIRECTORY_DEPTH
            || !is_lower_hex_sha256(&entry.sha256)
        {
            return Err(integrity_error());
        }
        total = total.checked_add(entry.size).ok_or_else(integrity_error)?;
        if total > MAX_DIRECTORY_BYTES
            || entry
                .relative_segments
                .iter()
                .any(|segment| validated_directory_segment(segment).is_err())
            || !paths.insert(entry.relative_segments.clone())
        {
            return Err(integrity_error());
        }
    }
    Ok(())
}

fn validated_directory_segment(value: &str) -> McpPlatformResult<()> {
    if value.is_empty()
        || value.len() > 64
        || value.ends_with(['.', ' '])
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || is_windows_device_name(value)
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn manifest_marker(manifest: &DirectoryPayloadManifest) -> Vec<u8> {
    let mut digest = Sha256::new();
    for entry in &manifest.entries {
        for segment in &entry.relative_segments {
            digest.update(segment.as_bytes());
            digest.update([0]);
        }
        digest.update(entry.size.to_le_bytes());
        digest.update(entry.sha256.as_bytes());
    }
    format!(
        "goose-directory-session-v1:{}",
        hex_digest(&digest.finalize())
    )
    .into_bytes()
}

fn materialize_directory_session(
    session: &mut WriterSession,
    payloads: &[DirectoryPayload<'_>],
) -> McpPlatformResult<()> {
    if !session.directory_entries.is_empty()
        || session.payload.is_some()
        || session.directory_seal.is_some()
    {
        return Err(integrity_error());
    }
    let manifest = session
        .directory_manifest
        .as_ref()
        .ok_or_else(integrity_error)?;
    validate_directory_manifest(manifest)?;
    validate_directory_payloads(payloads, manifest.entries.len())?;
    if payloads.len() != manifest.entries.len() {
        return Err(integrity_error());
    }
    let mut sources = BTreeMap::new();
    for payload in payloads {
        if sources
            .insert(payload.relative_segments.as_slice(), payload.bytes)
            .is_some()
        {
            return Err(integrity_error());
        }
    }
    if !manifest
        .entries
        .iter()
        .all(|entry| sources.contains_key(&entry.relative_segments.as_slice()))
    {
        return Err(integrity_error());
    }

    let mut directories = BTreeSet::new();
    for entry in &manifest.entries {
        for depth in 1..entry.relative_segments.len() {
            directories.insert(entry.relative_segments[..depth].to_vec());
        }
    }
    let mut directory_handles: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    for directory in directories {
        let parent = &directory[..directory.len() - 1];
        let parent_file = directory_parent_file(session, &directory_handles, parent)?;
        let mut slot = None;
        create_relative(
            parent_file,
            directory.last().ok_or_else(integrity_error)?,
            true,
            FILE_READ_ATTRIBUTES
                | FILE_READ_DATA
                | FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_TRAVERSE
                | DELETE
                | SYNCHRONIZE,
            &mut slot,
        )?;
        let object = take_ready_object(&mut slot, true)?;
        let index = session.directory_entries.len();
        session.directory_entries.push(object);
        session.directory_entry_paths.push(directory.clone());
        directory_handles.insert(directory, index);
    }

    for entry in &manifest.entries {
        let bytes = sources
            .get(&entry.relative_segments.as_slice())
            .copied()
            .ok_or_else(integrity_error)?;
        if bytes.len() as u64 != entry.size || hex_digest(bytes) != entry.sha256 {
            return Err(integrity_error());
        }
        let parent = &entry.relative_segments[..entry.relative_segments.len() - 1];
        let parent_file = directory_parent_file(session, &directory_handles, parent)?;
        let mut slot = None;
        create_relative(
            parent_file,
            entry.relative_segments.last().ok_or_else(integrity_error)?,
            false,
            FILE_READ_ATTRIBUTES | FILE_READ_DATA | FILE_WRITE_DATA | DELETE | SYNCHRONIZE,
            &mut slot,
        )?;
        let mut object = take_ready_object(&mut slot, false)?;
        #[cfg(test)]
        if take_test_failure(TestFailurePoint::PartialWrite) {
            object
                .file
                .write_all(&bytes[..bytes.len().min(1)])
                .map_err(|_| unavailable())?;
            return Err(unavailable());
        }
        object.file.write_all(bytes).map_err(|_| unavailable())?;
        object.file.flush().map_err(|_| unavailable())?;
        verify_relative_object(&object, false)?;
        session.directory_entries.push(object);
        session
            .directory_entry_paths
            .push(entry.relative_segments.clone());
    }
    Ok(())
}

fn validate_directory_payloads(
    payloads: &[DirectoryPayload<'_>],
    expected_entries: usize,
) -> McpPlatformResult<()> {
    if payloads.len() != expected_entries || payloads.len() > MAX_DIRECTORY_FILES {
        return Err(integrity_error());
    }
    let mut total = 0_u64;
    let mut paths = BTreeSet::new();
    for payload in payloads {
        if payload.relative_segments.is_empty()
            || payload.relative_segments.len() > MAX_DIRECTORY_DEPTH
            || payload
                .relative_segments
                .iter()
                .any(|segment| validated_directory_segment(segment).is_err())
            || !paths.insert(payload.relative_segments.as_slice())
        {
            return Err(integrity_error());
        }
        total = total
            .checked_add(u64::try_from(payload.bytes.len()).map_err(|_| integrity_error())?)
            .ok_or_else(integrity_error)?;
        if total > MAX_DIRECTORY_BYTES {
            return Err(integrity_error());
        }
    }
    Ok(())
}

fn directory_parent_file<'a>(
    session: &'a WriterSession,
    directories: &BTreeMap<Vec<String>, usize>,
    parent: &[String],
) -> McpPlatformResult<&'a File> {
    if parent.is_empty() {
        return Ok(&ready_session_object(session.staging.as_ref(), true)?.file);
    }
    let index = directories
        .get(&parent.to_vec())
        .copied()
        .ok_or_else(integrity_error)?;
    let object = session
        .directory_entries
        .get(index)
        .ok_or_else(integrity_error)?;
    if !object.directory {
        return Err(integrity_error());
    }
    Ok(&object.file)
}

fn take_ready_object(
    slot: &mut Option<SessionObject>,
    directory: bool,
) -> McpPlatformResult<OwnedRelativeObject> {
    match slot.take() {
        Some(SessionObject::Ready(object)) if object.directory == directory => Ok(object),
        _ => Err(integrity_error()),
    }
}

fn verify_directory_session(session: &mut WriterSession) -> McpPlatformResult<()> {
    let manifest = session
        .directory_manifest
        .as_ref()
        .ok_or_else(integrity_error)?;
    let directory_count = manifest
        .entries
        .iter()
        .flat_map(|entry| {
            (1..entry.relative_segments.len())
                .map(move |depth| entry.relative_segments[..depth].to_vec())
        })
        .collect::<BTreeSet<_>>()
        .len();
    if session.directory_entries.len() != manifest.entries.len() + directory_count {
        return Err(integrity_error());
    }
    if directory_count
        .checked_add(manifest.entries.len())
        .and_then(|count| count.checked_add(2))
        .filter(|count| *count <= MAX_DIRECTORY_SESSION_HANDLES)
        .is_none()
    {
        return Err(integrity_error());
    }
    let marker = ready_session_object_mut(session.recovery_marker.as_mut(), false)?;
    verify_relative_object(marker, false)?;
    marker
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|_| unavailable())?;
    let mut marker_bytes = Vec::new();
    marker
        .file
        .read_to_end(&mut marker_bytes)
        .map_err(|_| unavailable())?;
    marker
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|_| unavailable())?;
    if marker_bytes != manifest_marker(manifest) {
        return Err(integrity_error());
    }

    for (object, path) in session
        .directory_entries
        .iter_mut()
        .zip(&session.directory_entry_paths)
    {
        verify_relative_object(object, object.directory)?;
        if !object.directory {
            let entry = manifest
                .entries
                .iter()
                .find(|entry| entry.relative_segments == *path)
                .ok_or_else(integrity_error)?;
            verify_file_digest(&mut object.file, entry)?;
        }
    }
    verify_directory_enumeration(session)
}

fn seal_directory_session(session: &mut WriterSession) -> McpPlatformResult<()> {
    if session.directory_seal.is_some() {
        return Err(integrity_error());
    }
    verify_directory_session(session)?;
    let staging_identity = {
        let staging = ready_session_object(session.staging.as_ref(), true)?;
        verify_session_write_lock(staging, true)?;
        staging.identity.clone()
    };
    {
        let marker = ready_session_object(session.recovery_marker.as_ref(), false)?;
        verify_session_write_lock(marker, false)?;
    }
    let mut entry_identities = Vec::with_capacity(session.directory_entries.len());
    for entry in &session.directory_entries {
        verify_session_write_lock(entry, entry.directory)?;
        entry_identities.push(entry.identity.clone());
    }
    let marker = manifest_marker(
        session
            .directory_manifest
            .as_ref()
            .ok_or_else(integrity_error)?,
    );
    session.directory_seal = Some(DirectorySeal {
        staging_identity,
        entry_identities,
        marker,
    });
    Ok(())
}

fn verify_directory_seal(session: &WriterSession, seal: &DirectorySeal) -> McpPlatformResult<()> {
    let staging = ready_session_object(session.staging.as_ref(), true)?;
    let marker = ready_session_object(session.recovery_marker.as_ref(), false)?;
    if staging.identity != seal.staging_identity
        || session.directory_entries.len() != seal.entry_identities.len()
        || manifest_marker(
            session
                .directory_manifest
                .as_ref()
                .ok_or_else(integrity_error)?,
        ) != seal.marker
    {
        return Err(integrity_error());
    }
    verify_session_write_lock(staging, true)?;
    verify_session_write_lock(marker, false)?;
    for (entry, identity) in session.directory_entries.iter().zip(&seal.entry_identities) {
        if entry.identity != *identity {
            return Err(integrity_error());
        }
        verify_session_write_lock(entry, entry.directory)?;
    }
    Ok(())
}

fn verify_session_write_lock(
    object: &OwnedRelativeObject,
    expected_directory: bool,
) -> McpPlatformResult<()> {
    if object.share_access != FILE_SHARE_READ
        || object.desired_access & DELETE == 0
        || object.directory != expected_directory
    {
        return Err(integrity_error());
    }
    let required_access = if expected_directory {
        FILE_READ_ATTRIBUTES | FILE_READ_DATA | FILE_TRAVERSE
    } else {
        FILE_READ_ATTRIBUTES | FILE_READ_DATA
    };
    if object.desired_access & required_access != required_access {
        return Err(integrity_error());
    }
    verify_relative_object(object, expected_directory)
}

fn verify_file_digest(
    file: &mut DeleteOnDropFile,
    entry: &DirectoryPayloadManifestEntry,
) -> McpPlatformResult<()> {
    if file.metadata().map_err(|_| unavailable())?.len() != entry.size {
        return Err(integrity_error());
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| unavailable())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer).map_err(|_| unavailable())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| unavailable())?;
    if hex_encoded_digest(&digest.finalize()) != entry.sha256 {
        return Err(integrity_error());
    }
    Ok(())
}

fn verify_directory_enumeration(session: &WriterSession) -> McpPlatformResult<()> {
    let mut directories = vec![(
        Vec::new(),
        &ready_session_object(session.staging.as_ref(), true)?.file,
    )];
    for (object, path) in session
        .directory_entries
        .iter()
        .zip(&session.directory_entry_paths)
    {
        if object.directory {
            directories.push((path.clone(), &object.file));
        }
    }
    for (parent, handle) in directories {
        let expected = expected_direct_children(session, &parent);
        if enumerate_directory_names(handle)? != expected {
            return Err(integrity_error());
        }
    }
    Ok(())
}

fn expected_direct_children(session: &WriterSession, parent: &[String]) -> BTreeSet<String> {
    session
        .directory_entry_paths
        .iter()
        .filter_map(|path| {
            (path.len() == parent.len() + 1 && path.starts_with(parent))
                .then(|| path.last().cloned())
                .flatten()
        })
        .collect()
}

fn create_sealed_final_tree(
    session: &WriterSession,
    parent: &File,
    leaf: &str,
    source_prefix: &[String],
) -> McpPlatformResult<SealedFinalTree> {
    let manifest = session
        .directory_manifest
        .as_ref()
        .ok_or_else(integrity_error)?;
    let mut root_slot = None;
    create_relative(
        parent,
        leaf,
        true,
        FILE_READ_ATTRIBUTES
            | FILE_READ_DATA
            | FILE_ADD_FILE
            | FILE_ADD_SUBDIRECTORY
            | FILE_TRAVERSE
            | DELETE
            | SYNCHRONIZE,
        &mut root_slot,
    )?;
    let root = take_ready_object(&mut root_slot, true)?;
    let mut tree = SealedFinalTree {
        root: Some(root),
        entries: Vec::new(),
        paths: Vec::new(),
    };
    let expected = manifest
        .entries
        .iter()
        .filter_map(|entry| {
            entry
                .relative_segments
                .strip_prefix(source_prefix)
                .filter(|path| !path.is_empty())
                .map(|path| (entry, path.to_vec()))
        })
        .collect::<Vec<_>>();
    if expected.is_empty() {
        return Err(integrity_error());
    }
    let mut directories = BTreeSet::new();
    for (_, path) in &expected {
        for depth in 1..path.len() {
            directories.insert(path[..depth].to_vec());
        }
    }
    let mut directory_handles: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    for directory in directories {
        let parent_file = if directory.len() == 1 {
            tree.root()?.file.try_clone().map_err(|_| unavailable())?
        } else {
            let parent = directory[..directory.len() - 1].to_vec();
            tree.entries[*directory_handles.get(&parent).ok_or_else(integrity_error)?]
                .file
                .try_clone()
                .map_err(|_| unavailable())?
        };
        let mut slot = None;
        create_relative(
            &parent_file,
            directory.last().ok_or_else(integrity_error)?,
            true,
            FILE_READ_ATTRIBUTES
                | FILE_READ_DATA
                | FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_TRAVERSE
                | DELETE
                | SYNCHRONIZE,
            &mut slot,
        )?;
        let index = tree.entries.len();
        tree.entries.push(take_ready_object(&mut slot, true)?);
        tree.paths.push(directory.clone());
        directory_handles.insert(directory, index);
    }
    for (entry, path) in &expected {
        let parent_file = if path.len() == 1 {
            tree.root()?.file.try_clone().map_err(|_| unavailable())?
        } else {
            let parent = path[..path.len() - 1].to_vec();
            tree.entries[*directory_handles.get(&parent).ok_or_else(integrity_error)?]
                .file
                .try_clone()
                .map_err(|_| unavailable())?
        };
        let source_index = session
            .directory_entry_paths
            .iter()
            .position(|source_path| source_path == &entry.relative_segments)
            .ok_or_else(integrity_error)?;
        let source = session
            .directory_entries
            .get(source_index)
            .ok_or_else(integrity_error)?;
        if source.directory {
            return Err(integrity_error());
        }
        verify_session_write_lock(source, false)?;
        let bytes = read_verified_file(&source.file)?;
        if bytes.len() as u64 != entry.size || hex_digest(&bytes) != entry.sha256 {
            return Err(integrity_error());
        }
        let mut slot = None;
        create_relative(
            &parent_file,
            path.last().ok_or_else(integrity_error)?,
            false,
            FILE_READ_ATTRIBUTES | FILE_READ_DATA | FILE_WRITE_DATA | DELETE | SYNCHRONIZE,
            &mut slot,
        )?;
        let mut object = take_ready_object(&mut slot, false)?;
        object.file.write_all(&bytes).map_err(|_| unavailable())?;
        object.file.flush().map_err(|_| unavailable())?;
        verify_file_digest(&mut object.file, entry)?;
        tree.entries.push(object);
        tree.paths.push(path.clone());
    }
    verify_sealed_final_tree(&tree, &expected)?;
    Ok(tree)
}

fn verify_sealed_final_tree(
    tree: &SealedFinalTree,
    expected: &[(&DirectoryPayloadManifestEntry, Vec<String>)],
) -> McpPlatformResult<()> {
    verify_session_write_lock(tree.root()?, true)?;
    if tree.entries.len() != tree.paths.len()
        || tree.entries.len()
            != expected.len()
                + expected
                    .iter()
                    .flat_map(|(_, path)| (1..path.len()).map(|depth| path[..depth].to_vec()))
                    .collect::<BTreeSet<_>>()
                    .len()
    {
        return Err(integrity_error());
    }
    let mut directories = vec![(Vec::new(), &tree.root()?.file)];
    for (entry, path) in tree.entries.iter().zip(&tree.paths) {
        verify_session_write_lock(entry, entry.directory)?;
        if entry.directory {
            directories.push((path.clone(), &entry.file));
        }
    }
    for (parent, handle) in directories {
        let names = tree
            .paths
            .iter()
            .filter_map(|path| {
                (path.len() == parent.len() + 1 && path.starts_with(&parent))
                    .then(|| path.last().cloned())
                    .flatten()
            })
            .collect();
        if enumerate_directory_names(handle)? != names {
            return Err(integrity_error());
        }
    }
    for (manifest_entry, path) in expected {
        let index = tree
            .paths
            .iter()
            .position(|candidate| candidate == path)
            .ok_or_else(integrity_error)?;
        let entry = tree.entries.get(index).ok_or_else(integrity_error)?;
        if entry.directory {
            return Err(integrity_error());
        }
        let mut file = DeleteOnDropFile {
            file: entry.file.try_clone().map_err(|_| unavailable())?,
            delete_on_drop: false,
        };
        verify_file_digest(&mut file, manifest_entry)?;
    }
    Ok(())
}

fn enumerate_directory_names(directory: &File) -> McpPlatformResult<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    let mut saw_current_directory = false;
    let mut saw_parent_directory = false;
    let mut restart = 1_u8;
    loop {
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut io_status = IoStatusBlock {
            status: 0,
            information: 0,
        };
        let status = unsafe {
            NtQueryDirectoryFile(
                directory.as_raw_handle() as HANDLE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut io_status,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                FILE_DIRECTORY_INFORMATION,
                0,
                std::ptr::null_mut(),
                restart,
            )
        };
        restart = 0;
        if status == STATUS_NO_MORE_FILES || io_status.status == STATUS_NO_MORE_FILES {
            break;
        }
        if status < 0 || io_status.status < 0 {
            return Err(unavailable());
        }
        parse_directory_information_buffer(
            &buffer,
            io_status.information,
            &mut names,
            &mut saw_current_directory,
            &mut saw_parent_directory,
        )?;
    }
    Ok(names)
}

fn parse_directory_information_buffer(
    buffer: &[u8],
    information: usize,
    names: &mut BTreeSet<String>,
    saw_current_directory: &mut bool,
    saw_parent_directory: &mut bool,
) -> McpPlatformResult<()> {
    if information == 0 || information > buffer.len() {
        return Err(unavailable());
    }

    let mut offset = 0_usize;
    loop {
        if offset
            .checked_add(64)
            .is_none_or(|entry_start| entry_start > information)
        {
            return Err(unavailable());
        }
        let entry = &buffer[offset..information];
        let name_length =
            u32::from_ne_bytes(entry[60..64].try_into().map_err(|_| unavailable())?) as usize;
        if name_length % 2 != 0 {
            return Err(unavailable());
        }
        let name_end = offset
            .checked_add(64)
            .and_then(|name_start| name_start.checked_add(name_length))
            .filter(|name_end| *name_end <= information)
            .ok_or_else(unavailable)?;
        let next = u32::from_ne_bytes(entry[0..4].try_into().map_err(|_| unavailable())?) as usize;

        let next_offset = if next == 0 {
            if name_end != information {
                return Err(unavailable());
            }
            None
        } else {
            if next < 64 || next % 8 != 0 {
                return Err(unavailable());
            }
            let entry_end = offset.checked_add(next).ok_or_else(unavailable)?;
            if name_end > entry_end || entry_end >= information {
                return Err(unavailable());
            }
            Some(entry_end)
        };

        let name = String::from_utf16(
            &buffer[offset + 64..name_end]
                .chunks_exact(2)
                .map(|unit| u16::from_ne_bytes([unit[0], unit[1]]))
                .collect::<Vec<_>>(),
        )
        .map_err(|_| unavailable())?;
        match name.as_str() {
            "." if *saw_current_directory => return Err(integrity_error()),
            "." => *saw_current_directory = true,
            ".." if *saw_parent_directory => return Err(integrity_error()),
            ".." => *saw_parent_directory = true,
            _ => {
                validated_directory_segment(&name)?;
                if !names.insert(name)
                    || names.len() > MAX_DIRECTORY_FILES + MAX_DIRECTORY_FILES * MAX_DIRECTORY_DEPTH
                {
                    return Err(integrity_error());
                }
            }
        }

        match next_offset {
            Some(next_offset) => offset = next_offset,
            None => return Ok(()),
        }
    }
}

fn committed_leaf_name(committed_leaf: RuntimeStorageCommittedLeaf) -> &'static str {
    match committed_leaf {
        RuntimeStorageCommittedLeaf::Cache => "platform.cache",
        RuntimeStorageCommittedLeaf::Installations => "platform.installations",
    }
}

fn validate_manifest_for_committed_leaf(
    manifest: &DirectoryPayloadManifest,
    committed_leaf: RuntimeStorageCommittedLeaf,
) -> McpPlatformResult<()> {
    validate_directory_manifest(manifest)?;
    match committed_leaf {
        RuntimeStorageCommittedLeaf::Cache => {
            if manifest.entries.iter().all(|entry| {
                entry.relative_segments.len() == 1 && entry.relative_segments[0] == entry.sha256
            }) {
                Ok(())
            } else {
                Err(integrity_error())
            }
        }
        RuntimeStorageCommittedLeaf::Installations => {
            let installations = manifest.entries.iter().all(|entry| {
                (entry.relative_segments.len() >= 4 && entry.relative_segments[1] == "versions")
                    || (entry.relative_segments.len() == 2
                        && entry.relative_segments[1] == "active.json")
            });
            if installations {
                Ok(())
            } else {
                Err(integrity_error())
            }
        }
    }
}

fn managed_installation_manifest_target(
    manifest: &DirectoryPayloadManifest,
) -> McpPlatformResult<(String, String)> {
    validate_manifest_for_committed_leaf(manifest, RuntimeStorageCommittedLeaf::Installations)?;
    let first = manifest.entries.first().ok_or_else(integrity_error)?;
    let managed_mcp_id = first
        .relative_segments
        .first()
        .ok_or_else(integrity_error)?
        .clone();
    let version = first
        .relative_segments
        .get(2)
        .ok_or_else(integrity_error)?
        .clone();
    if manifest.entries.iter().all(|entry| {
        entry.relative_segments.len() >= 4
            && entry.relative_segments[0] == managed_mcp_id
            && entry.relative_segments[1] == "versions"
            && entry.relative_segments[2] == version
    }) {
        Ok((managed_mcp_id, version))
    } else {
        Err(integrity_error())
    }
}

fn relative_object_exists(parent: &File, leaf: &str) -> McpPlatformResult<bool> {
    for directory in [true, false] {
        match open_relative_for_read(parent, leaf, directory) {
            Ok(Some(_)) => return Ok(true),
            Ok(None) => {}
            Err(_) => return Err(integrity_error()),
        }
    }
    Ok(false)
}

fn rename_relative_replace(
    source: &File,
    destination_parent: &File,
    leaf: &str,
) -> McpPlatformResult<()> {
    rename_relative(source, destination_parent, leaf, true)
}

fn rename_relative(
    source: &File,
    destination_parent: &File,
    leaf: &str,
    replace_if_exists: bool,
) -> McpPlatformResult<()> {
    let name = validated_leaf_name(leaf)?;
    let name_bytes = (name.len() - 1).checked_mul(2).ok_or_else(unavailable)?;
    let base = std::mem::offset_of!(FileRenameInformation, file_name);
    let mut buffer = vec![0_u8; base + name_bytes];
    let information = buffer.as_mut_ptr().cast::<FileRenameInformation>();
    unsafe {
        (*information).replace_if_exists = u8::from(replace_if_exists);
        (*information).root_directory = destination_parent.as_raw_handle() as HANDLE;
        (*information).file_name_length = u32::try_from(name_bytes).map_err(|_| unavailable())?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr().cast::<u8>(),
            buffer.as_mut_ptr().add(base),
            name_bytes,
        );
    }
    let mut io_status = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let status = unsafe {
        NtSetInformationFile(
            source.as_raw_handle() as HANDLE,
            &mut io_status,
            buffer.as_mut_ptr().cast(),
            buffer.len().try_into().map_err(|_| unavailable())?,
            FILE_RENAME_INFORMATION,
        )
    };
    if status < 0 || io_status.status < 0 {
        return Err(integrity_error());
    }
    Ok(())
}

fn open_relative_directory_for_mutation(parent: &File, leaf: &str) -> McpPlatformResult<File> {
    open_relative_for_mutation(parent, leaf, true)?.ok_or_else(integrity_error)
}

fn open_relative_file_for_mutation(parent: &File, leaf: &str) -> McpPlatformResult<Option<File>> {
    open_relative_for_mutation(parent, leaf, false)
}

fn open_relative_for_mutation(
    parent: &File,
    leaf: &str,
    directory: bool,
) -> McpPlatformResult<Option<File>> {
    let mut name = validated_leaf_name(leaf)?;
    let mut unicode = UnicodeString {
        length: ((name.len() - 1) * 2)
            .try_into()
            .map_err(|_| unavailable())?,
        maximum_length: (name.len() * 2).try_into().map_err(|_| unavailable())?,
        buffer: name.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle() as HANDLE,
        object_name: &mut unicode,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_READ_ATTRIBUTES
                | DELETE
                | SYNCHRONIZE
                | if directory {
                    FILE_READ_DATA | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_TRAVERSE
                } else {
                    FILE_READ_DATA
                },
            &mut attributes,
            &mut io_status,
            std::ptr::null_mut(),
            if directory {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            },
            FILE_SHARE_READ,
            FILE_OPEN,
            FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_REPARSE_POINT
                | if directory {
                    FILE_DIRECTORY_FILE
                } else {
                    FILE_NON_DIRECTORY_FILE
                },
            std::ptr::null_mut(),
            0,
        )
    };
    if status == STATUS_OBJECT_NAME_NOT_FOUND
        || status == STATUS_OBJECT_PATH_NOT_FOUND
        || io_status.status == STATUS_OBJECT_NAME_NOT_FOUND
        || io_status.status == STATUS_OBJECT_PATH_NOT_FOUND
    {
        close_if_valid_handle(handle);
        return Ok(None);
    }
    if status < 0 || io_status.status < 0 || !is_valid_handle(handle) {
        close_if_valid_handle(handle);
        return Err(integrity_error());
    }
    let file = unsafe { File::from_raw_handle(handle as _) };
    verify_read_handle(file, directory).map(|(file, _)| Some(file))
}

fn verify_file_bytes(file: &mut DeleteOnDropFile, expected: &[u8]) -> McpPlatformResult<()> {
    if file.metadata().map_err(|_| unavailable())?.len()
        != u64::try_from(expected.len()).map_err(|_| integrity_error())?
    {
        return Err(integrity_error());
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| unavailable())?;
    let mut observed = Vec::new();
    file.read_to_end(&mut observed).map_err(|_| unavailable())?;
    file.seek(SeekFrom::Start(0)).map_err(|_| unavailable())?;
    (observed == expected)
        .then_some(())
        .ok_or_else(integrity_error)
}

fn hex_digest(bytes: &[u8]) -> String {
    hex_encoded_digest(&Sha256::digest(bytes))
}

fn hex_encoded_digest(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CommittedVersionEvidence {
    descriptor_sha256: String,
    tree_sha256: String,
}

fn read_verified_file(file: &File) -> McpPlatformResult<Vec<u8>> {
    let mut file = file.try_clone().map_err(|_| unavailable())?;
    verify_read_handle(file.try_clone().map_err(|_| unavailable())?, false)?;
    let length = file.metadata().map_err(|_| unavailable())?.len();
    if length > MAX_DIRECTORY_BYTES {
        return Err(integrity_error());
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.read_to_end(&mut bytes).map_err(|_| unavailable())?;
    Ok(bytes)
}

fn read_relative_file(parent: &File, leaf: &str) -> McpPlatformResult<Option<Vec<u8>>> {
    let Some(file) = open_relative_for_read(parent, leaf, false)? else {
        return Ok(None);
    };
    let (file, _) = verify_read_handle(file, false)?;
    read_verified_file(&file).map(Some)
}

fn capture_staging_snapshot(
    staging: &File,
) -> McpPlatformResult<BTreeMap<String, CleanupStagingObject>> {
    let mut snapshot = BTreeMap::new();
    capture_staging_snapshot_directory(staging, &[], &mut snapshot)?;
    Ok(snapshot)
}

fn capture_session_staging_snapshot(
    session: &WriterSession,
) -> McpPlatformResult<BTreeMap<String, CleanupStagingObject>> {
    if session.directory_entries.len() != session.directory_entry_paths.len() {
        return Err(integrity_error());
    }
    let mut snapshot = BTreeMap::new();
    for (entry, path) in session
        .directory_entries
        .iter()
        .zip(&session.directory_entry_paths)
    {
        verify_relative_object(entry, entry.directory)?;
        let key = path.join("/");
        if key.is_empty()
            || snapshot
                .insert(
                    key,
                    CleanupStagingObject {
                        identity: entry.identity.durable_key(),
                        directory: entry.directory,
                    },
                )
                .is_some()
        {
            return Err(integrity_error());
        }
    }
    Ok(snapshot)
}

fn capture_staging_snapshot_directory(
    directory: &File,
    path: &[String],
    snapshot: &mut BTreeMap<String, CleanupStagingObject>,
) -> McpPlatformResult<()> {
    for name in enumerate_directory_names(directory)? {
        let mut child_path = path.to_vec();
        child_path.push(name.clone());
        let key = child_path.join("/");
        match open_relative_for_mutation(directory, &name, false) {
            Ok(Some(file)) => {
                snapshot.insert(
                    key,
                    CleanupStagingObject {
                        identity: FileIdentity::from_file(&file)?.durable_key(),
                        directory: false,
                    },
                );
            }
            Ok(None) => return Err(integrity_error()),
            Err(_) => {
                let child = open_relative_directory_for_mutation(directory, &name)?;
                snapshot.insert(
                    key,
                    CleanupStagingObject {
                        identity: FileIdentity::from_file(&child)?.durable_key(),
                        directory: true,
                    },
                );
                capture_staging_snapshot_directory(&child, &child_path, snapshot)?;
            }
        }
    }
    Ok(())
}

fn delete_authenticated_staging_directory(
    capability: &ManagedRootCapability,
    staging: &File,
    directory: File,
    path: &[String],
    snapshot: &BTreeMap<String, CleanupStagingObject>,
    pending: &CommittedCleanupPending,
) -> McpPlatformResult<()> {
    capability.revalidate_staging_delete_binding(staging, pending)?;
    for name in enumerate_directory_names(&directory)? {
        let mut child_path = path.to_vec();
        child_path.push(name.clone());
        let key = child_path.join("/");
        let expected = snapshot.get(&key).ok_or_else(integrity_error)?;
        if expected.directory {
            let child = open_relative_directory_for_mutation(&directory, &name)?;
            if FileIdentity::from_file(&child)?.durable_key() != expected.identity {
                return Err(integrity_error());
            }
            delete_authenticated_staging_directory(
                capability,
                staging,
                child,
                &child_path,
                snapshot,
                pending,
            )?;
        } else {
            let file = open_relative_for_mutation(&directory, &name, false)?
                .ok_or_else(integrity_error)?;
            if FileIdentity::from_file(&file)?.durable_key() != expected.identity {
                return Err(integrity_error());
            }
            capability.revalidate_staging_delete_binding(staging, pending)?;
            set_delete_on_close(&file, true)?;
            drop(file);
            #[cfg(test)]
            if take_test_failure(TestFailurePoint::PartialStagingTreeDisposition) {
                return Err(cleanup_error());
            }
        }
    }
    capability.revalidate_staging_delete_binding(staging, pending)?;
    set_delete_on_close(&directory, true)?;
    drop(directory);
    #[cfg(test)]
    if !path.is_empty()
        && take_test_failure(TestFailurePoint::RootRevalidationAfterStagingDisposition)
    {
        inject_test_failure(TestFailurePoint::RootRevalidation);
    }
    #[cfg(test)]
    if !path.is_empty() && take_test_failure(TestFailurePoint::PartialStagingTreeDisposition) {
        return Err(cleanup_error());
    }
    Ok(())
}

fn verify_pending_staging_tree(staging: &File, managed_mcp_id: &str) -> McpPlatformResult<()> {
    let staging_entries = enumerate_directory_names(&staging)?;
    if staging_entries.len() != 1
        || staging_entries
            .iter()
            .next()
            .is_none_or(|entry| entry != managed_mcp_id)
    {
        return Err(integrity_error());
    }
    let managed = open_relative_directory_for_mutation(staging, managed_mcp_id)?;
    let managed_entries = enumerate_directory_names(&managed)?;
    if managed_entries.len() != 1
        || managed_entries
            .iter()
            .next()
            .is_none_or(|entry| entry != "versions")
    {
        return Err(integrity_error());
    }
    let versions = open_relative_directory_for_mutation(&managed, "versions")?;
    if !enumerate_directory_names(&versions)?.is_empty() {
        return Err(integrity_error());
    }
    Ok(())
}

fn open_prepared_staging_target(
    staging: &File,
    pending: &CommittedCleanupPending,
) -> McpPlatformResult<File> {
    let staging_entries = enumerate_directory_names(staging)?;
    if staging_entries.len() != 1 || staging_entries.first() != Some(&pending.managed_mcp_id) {
        return Err(integrity_error());
    }
    let managed = open_relative_directory_for_mutation(staging, &pending.managed_mcp_id)?;
    let managed_entries = enumerate_directory_names(&managed)?;
    if managed_entries.len() != 1 || !managed_entries.contains("versions") {
        return Err(integrity_error());
    }
    let versions = open_relative_directory_for_mutation(&managed, "versions")?;
    let version_entries = enumerate_directory_names(&versions)?;
    if version_entries.len() != 1 || version_entries.first() != Some(&pending.version) {
        return Err(integrity_error());
    }
    open_relative_directory_for_mutation(&versions, &pending.version)
}

fn open_pending_committed_target(
    root: &File,
    managed_mcp_id: &str,
    version: &str,
) -> McpPlatformResult<Option<File>> {
    let Some(installations) = open_relative_for_mutation(root, "platform.installations", true)?
    else {
        return Ok(None);
    };
    let Some(managed) = open_relative_for_mutation(&installations, managed_mcp_id, true)? else {
        return Ok(None);
    };
    let Some(versions) = open_relative_for_mutation(&managed, "versions", true)? else {
        return Ok(None);
    };
    open_relative_for_mutation(&versions, version, true)
}

fn delete_verified_tree(directory: File) -> McpPlatformResult<()> {
    for name in enumerate_directory_names(&directory)? {
        match open_relative_for_mutation(&directory, &name, false) {
            Ok(Some(file)) => {
                set_delete_on_close(&file, true)?;
                drop(file);
            }
            Ok(None) => return Err(integrity_error()),
            Err(_) => {
                let child = open_relative_directory_for_mutation(&directory, &name)?;
                delete_verified_tree(child)?;
            }
        }
    }
    set_delete_on_close(&directory, true)
}

fn verify_committed_version_tree(
    target: &File,
    descriptor: &[u8],
    expected_version: Option<&str>,
) -> McpPlatformResult<()> {
    if let Some(expected_version) = expected_version {
        let value: serde_json::Value =
            serde_json::from_slice(descriptor).map_err(|_| integrity_error())?;
        if value.get("version").and_then(serde_json::Value::as_str) != Some(expected_version) {
            return Err(integrity_error());
        }
    }
    let commitment =
        read_relative_file(target, COMMITTED_VERSION_EVIDENCE)?.ok_or_else(integrity_error)?;
    let evidence: CommittedVersionEvidence =
        serde_json::from_slice(&commitment).map_err(|_| integrity_error())?;
    if !is_lower_hex_sha256(&evidence.descriptor_sha256)
        || !is_lower_hex_sha256(&evidence.tree_sha256)
        || evidence.descriptor_sha256 != hex_digest(descriptor)
        || evidence.tree_sha256 != committed_version_tree_digest(target)?
    {
        return Err(integrity_error());
    }
    Ok(())
}

fn anchored_version_from_tree(target: &File, version: &str) -> McpPlatformResult<AnchoredVersion> {
    let descriptor = read_relative_file(target, ".goose-runtime-descriptor.json")?
        .ok_or_else(integrity_error)?;
    verify_committed_version_tree(target, &descriptor, Some(version))?;
    let commitment =
        read_relative_file(target, COMMITTED_VERSION_EVIDENCE)?.ok_or_else(integrity_error)?;
    let evidence: CommittedVersionEvidence =
        serde_json::from_slice(&commitment).map_err(|_| integrity_error())?;
    Ok(AnchoredVersion {
        descriptor_sha256: evidence.descriptor_sha256,
        tree_sha256: evidence.tree_sha256,
    })
}

fn verify_anchored_version(
    target: &File,
    descriptor: &[u8],
    version: &str,
    anchor: &AnchoredVersion,
) -> McpPlatformResult<()> {
    verify_committed_version_tree(target, descriptor, Some(version))?;
    let actual = anchored_version_from_tree(target, version)?;
    if &actual != anchor || anchor.descriptor_sha256 != hex_digest(descriptor) {
        return Err(integrity_error());
    }
    Ok(())
}

fn committed_version_tree_digest(root: &File) -> McpPlatformResult<String> {
    let mut entries = Vec::new();
    collect_committed_version_tree(root, &mut Vec::new(), &mut entries)?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    for (segments, file_digest, size) in entries {
        for segment in segments {
            digest.update(segment.as_bytes());
            digest.update([0]);
        }
        digest.update(size.to_le_bytes());
        digest.update(file_digest.as_bytes());
    }
    Ok(hex_digest(&digest.finalize()))
}

fn collect_committed_version_tree(
    directory: &File,
    prefix: &mut Vec<String>,
    entries: &mut Vec<(Vec<String>, String, u64)>,
) -> McpPlatformResult<()> {
    for name in enumerate_directory_names(directory)? {
        if prefix.is_empty() && name == COMMITTED_VERSION_EVIDENCE {
            continue;
        }
        match open_relative_for_read(directory, &name, false) {
            Ok(Some(file)) => {
                let (file, _) = verify_read_handle(file, false)?;
                let bytes = read_verified_file(&file)?;
                let mut path = prefix.clone();
                path.push(name);
                entries.push((path, hex_digest(&bytes), bytes.len() as u64));
            }
            Ok(None) => return Err(integrity_error()),
            Err(_) => {
                let child =
                    open_relative_for_read(directory, &name, true)?.ok_or_else(integrity_error)?;
                let (child, _) = verify_read_handle(child, true)?;
                prefix.push(name);
                collect_committed_version_tree(&child, prefix, entries)?;
                prefix.pop();
            }
        }
    }
    Ok(())
}

fn reject_session(session: &mut WriterSession, error: McpPlatformError) -> McpPlatformResult<()> {
    if discard_uncommitted(session).is_err() {
        return Err(cleanup_error());
    }
    Err(error)
}

fn discard_uncommitted(session: &mut WriterSession) -> McpPlatformResult<()> {
    if session.recovery_required {
        return Err(cleanup_error());
    }
    discard_uncommitted_staging(session)?;
    discard_recovery_marker(session)?;
    Ok(())
}

fn discard_recovery_marker(session: &mut WriterSession) -> McpPlatformResult<()> {
    #[cfg(test)]
    if take_test_failure(TestFailurePoint::CleanupMarkerDisposition) {
        return Err(cleanup_error());
    }
    discard_session_object(&mut session.recovery_marker, false)
}

fn discard_uncommitted_staging(session: &mut WriterSession) -> McpPlatformResult<()> {
    session.expected_sha256 = None;
    session.directory_seal = None;
    while !session.directory_entries.is_empty() {
        if session.directory_entries.len() != session.directory_entry_paths.len() {
            return Err(cleanup_error());
        }
        let index = session
            .directory_entry_paths
            .iter()
            .enumerate()
            .max_by(|(left_index, left), (right_index, right)| {
                left.len()
                    .cmp(&right.len())
                    .then_with(|| left.cmp(right))
                    .then_with(|| left_index.cmp(right_index))
            })
            .map(|(index, _)| index)
            .ok_or_else(cleanup_error)?;
        let mut entry = session.directory_entries.swap_remove(index);
        let path = session.directory_entry_paths.swap_remove(index);
        if let Err(error) = verify_relative_object(&entry, entry.directory) {
            session.directory_entries.push(entry);
            session.directory_entry_paths.push(path);
            return Err(error);
        }
        #[cfg(test)]
        if take_test_failure(TestFailurePoint::CleanupDisposition) {
            session.directory_entries.push(entry);
            session.directory_entry_paths.push(path);
            return Err(cleanup_error());
        }
        if let Err(error) = set_delete_on_close(&entry.file, true) {
            session.directory_entries.push(entry);
            session.directory_entry_paths.push(path);
            return Err(error);
        }
        entry.file.delete_on_drop = false;
        #[cfg(test)]
        if take_test_failure(TestFailurePoint::CleanupAfterDescendantDisposition) {
            return Err(cleanup_error());
        }
    }
    session.directory_entry_paths.clear();
    session.directory_manifest = None;
    discard_session_object(&mut session.payload, false)?;
    discard_session_object(&mut session.staging, true)?;
    Ok(())
}

impl Drop for WriterSession {
    fn drop(&mut self) {
        if self.recovery_required {
            return;
        }
        if discard_uncommitted(self).is_err() {
            // Drop cannot return a cleanup error. Preserve the marker and all
            // remaining objects so a subsequent preflight sees the residual
            // and fails closed instead of treating teardown as successful.
            self.retain_for_authenticated_recovery();
        }
    }
}

fn discard_session_object(
    slot: &mut Option<SessionObject>,
    expected_directory: bool,
) -> McpPlatformResult<()> {
    let object = match slot.as_ref() {
        Some(object) => object,
        None => return Ok(()),
    };
    match object {
        SessionObject::Pending(pending) => {
            if pending.directory != expected_directory {
                return Err(cleanup_error());
            }
            #[cfg(test)]
            if take_test_failure(TestFailurePoint::CleanupDisposition) {
                return Err(cleanup_error());
            }
            set_delete_on_close(pending.file.as_ref().ok_or_else(cleanup_error)?, true)?;
        }
        SessionObject::Ready(ready) => {
            verify_relative_object(ready, expected_directory)?;
            #[cfg(test)]
            if take_test_failure(TestFailurePoint::CleanupDisposition) {
                return Err(cleanup_error());
            }
            set_delete_on_close(&ready.file, true)?;
        }
    }
    // Do not release the only handle until the kernel accepted its deletion.
    // On failure the session retains the handle for a synchronous retry or Drop.
    let _ = slot.take();
    Ok(())
}

fn set_delete_on_close(file: &File, delete_file: bool) -> McpPlatformResult<()> {
    let mut disposition = FileDispositionInformation { delete_file: 1 };
    disposition.delete_file = u8::from(delete_file);
    let mut io_status = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle() as HANDLE,
            &mut io_status,
            (&mut disposition as *mut FileDispositionInformation).cast(),
            std::mem::size_of::<FileDispositionInformation>() as u32,
            FILE_DISPOSITION_INFORMATION,
        )
    };
    if status < 0 || io_status.status < 0 {
        return Err(cleanup_error());
    }
    Ok(())
}

fn create_relative(
    parent: &File,
    leaf: &str,
    directory: bool,
    desired_access: u32,
    destination: &mut Option<SessionObject>,
) -> McpPlatformResult<()> {
    if destination.is_some() {
        return Err(integrity_error());
    }
    let mut name = validated_leaf_name(leaf)?;
    let mut unicode = UnicodeString {
        length: ((name.len() - 1) * 2)
            .try_into()
            .map_err(|_| unavailable())?,
        maximum_length: (name.len() * 2).try_into().map_err(|_| unavailable())?,
        buffer: name.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent.as_raw_handle() as HANDLE,
        object_name: &mut unicode,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &mut attributes,
            &mut io_status,
            std::ptr::null_mut(),
            if directory {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            },
            // A write/delete handle opened before this point cannot be
            // revoked on Windows.  Every staged object is therefore born
            // with no write/delete sharing and retained until promotion.
            FILE_SHARE_READ,
            FILE_CREATE,
            FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_REPARSE_POINT
                | if directory {
                    FILE_DIRECTORY_FILE
                } else {
                    FILE_NON_DIRECTORY_FILE
                },
            std::ptr::null_mut(),
            0,
        )
    };
    if !create_succeeded(status, &io_status, handle) {
        close_if_valid_handle(handle);
        return Err(integrity_error());
    }
    let file = unsafe { File::from_raw_handle(handle as _) };
    // The pending object becomes session-owned before its first identity or
    // metadata read. Every subsequent error reaches session cleanup.
    *destination = Some(SessionObject::Pending(PendingSessionObject {
        file: Some(file),
        delete_on_drop: true,
        directory,
        desired_access,
        share_access: FILE_SHARE_READ,
    }));
    set_pending_delete_on_close(destination)?;
    finalize_pending_object(destination)
}

fn set_pending_delete_on_close(destination: &Option<SessionObject>) -> McpPlatformResult<()> {
    match destination.as_ref() {
        Some(SessionObject::Pending(pending)) => {
            set_delete_on_close(pending.file.as_ref().ok_or_else(cleanup_error)?, true)
        }
        _ => Err(integrity_error()),
    }
}

fn finalize_pending_object(destination: &mut Option<SessionObject>) -> McpPlatformResult<()> {
    let (identity, directory, desired_access, share_access) = match destination.as_ref() {
        Some(SessionObject::Pending(pending)) => {
            #[cfg(test)]
            if take_test_failure(TestFailurePoint::InitialIdentity) {
                return Err(unavailable());
            }
            let file = pending.file.as_ref().ok_or_else(cleanup_error)?;
            let identity = FileIdentity::from_file(file)?;
            set_delete_on_close(file, false)?;
            (
                identity,
                pending.directory,
                pending.desired_access,
                pending.share_access,
            )
        }
        _ => return Err(integrity_error()),
    };
    let mut pending = match destination.take() {
        Some(SessionObject::Pending(pending)) => pending,
        _ => return Err(integrity_error()),
    };
    *destination = Some(SessionObject::Ready(OwnedRelativeObject {
        file: DeleteOnDropFile {
            file: pending.file.take().ok_or_else(cleanup_error)?,
            delete_on_drop: true,
        },
        identity,
        directory,
        desired_access,
        share_access,
    }));
    Ok(())
}

fn ready_session_object(
    object: Option<&SessionObject>,
    expected_directory: bool,
) -> McpPlatformResult<&OwnedRelativeObject> {
    match object {
        Some(SessionObject::Ready(object)) if object.directory == expected_directory => Ok(object),
        _ => Err(integrity_error()),
    }
}

fn ready_session_object_mut(
    object: Option<&mut SessionObject>,
    expected_directory: bool,
) -> McpPlatformResult<&mut OwnedRelativeObject> {
    match object {
        Some(SessionObject::Ready(object)) if object.directory == expected_directory => Ok(object),
        _ => Err(integrity_error()),
    }
}

fn verify_session_object(
    object: &SessionObject,
    expected_directory: bool,
) -> McpPlatformResult<()> {
    verify_relative_object(
        ready_session_object(Some(object), expected_directory)?,
        expected_directory,
    )
}

fn create_succeeded(status: i32, io_status: &IoStatusBlock, handle: HANDLE) -> bool {
    status >= 0
        && io_status.status >= 0
        && io_status.information == FILE_CREATED
        && is_valid_handle(handle)
}

fn is_valid_handle(handle: HANDLE) -> bool {
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

fn close_if_valid_handle(handle: HANDLE) {
    if is_valid_handle(handle) {
        unsafe {
            CloseHandle(handle);
        }
    }
}

fn verify_relative_object(
    object: &OwnedRelativeObject,
    expected_directory: bool,
) -> McpPlatformResult<()> {
    if object.directory != expected_directory
        || FileIdentity::from_file(&object.file)? != object.identity
    {
        return Err(integrity_error());
    }
    let attributes = file_attributes(&object.file)?;
    let expected_kind = if expected_directory {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        0
    };
    if attributes
        & (FILE_ATTRIBUTE_DIRECTORY
            | FILE_ATTRIBUTE_HIDDEN
            | FILE_ATTRIBUTE_SYSTEM
            | FILE_ATTRIBUTE_REPARSE_POINT)
        != expected_kind
    {
        return Err(integrity_error());
    }
    if FileIdentity::link_count(&object.file)? != 1 {
        return Err(integrity_error());
    }
    if has_named_streams(&object.file)? {
        return Err(integrity_error());
    }
    Ok(())
}

fn has_named_streams(file: &File) -> McpPlatformResult<bool> {
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut io_status = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let status = unsafe {
        NtQueryInformationFile(
            file.as_raw_handle() as HANDLE,
            &mut io_status,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            FILE_STREAM_INFORMATION,
        )
    };
    if status < 0 || io_status.status < 0 {
        return Err(unavailable());
    }

    if io_status.information == 0 {
        return Ok(false);
    }

    has_named_streams_in_information(&buffer, io_status.information)
}

fn has_named_streams_in_information(buffer: &[u8], information: usize) -> McpPlatformResult<bool> {
    if information == 0 {
        return Ok(false);
    }
    if information > buffer.len() {
        return Err(unavailable());
    }

    let mut offset = 0_usize;
    let mut named_streams = false;
    loop {
        if offset.checked_add(24).is_none_or(|end| end > information) {
            return Err(unavailable());
        }
        let entry = &buffer[offset..];
        let next = u32::from_ne_bytes(entry[0..4].try_into().map_err(|_| unavailable())?) as usize;
        let name_len =
            u32::from_ne_bytes(entry[4..8].try_into().map_err(|_| unavailable())?) as usize;
        let name_end = offset
            .checked_add(24)
            .and_then(|start| start.checked_add(name_len))
            .filter(|end| *end <= information)
            .ok_or_else(unavailable)?;
        if name_len % 2 != 0 {
            return Err(unavailable());
        }
        let name: Vec<u16> = buffer[offset + 24..name_end]
            .chunks_exact(2)
            .map(|unit| u16::from_ne_bytes([unit[0], unit[1]]))
            .collect();
        let name = String::from_utf16(&name).map_err(|_| unavailable())?;
        if !name.is_empty() && name != "::$DATA" {
            named_streams = true;
        }

        let entry_end = if next == 0 {
            if name_end != information {
                return Err(unavailable());
            }
            information
        } else {
            if next % 8 != 0 {
                return Err(unavailable());
            }
            let entry_end = offset.checked_add(next).ok_or_else(unavailable)?;
            if entry_end >= information {
                return Err(unavailable());
            }
            entry_end
        };
        if entry_end < name_end {
            return Err(unavailable());
        }
        if next == 0 {
            return Ok(named_streams);
        }
        offset = entry_end;
    }
}

fn validated_leaf_name(value: &str) -> McpPlatformResult<Vec<u16>> {
    if value.is_empty()
        || value.len() > 64
        || value.ends_with(['.', ' '])
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || is_windows_device_name(value)
    {
        return Err(integrity_error());
    }
    let mut encoded: Vec<u16> = value.encode_utf16().collect();
    encoded.push(0);
    Ok(encoded)
}

fn is_windows_device_name(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .trim_end_matches(['.', ' '])
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;

    use super::*;
    use crate::mcp_platform::storage_domain::windows_authority::test_root_binding;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_WRITE,
    };

    fn test_root_handle(path: &std::path::Path) -> File {
        OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .expect("temporary managed root handle")
    }

    #[derive(Debug, PartialEq, Eq)]
    enum FreshRootPreflightStage {
        Complete,
        AcquireWriterRoot,
        RootLineage,
        WriterRootRevalidation,
        Attributes,
        LinkCount,
        NamedStreams,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct FreshRootPreflightDiagnostic {
        stage: FreshRootPreflightStage,
        attributes: Option<u32>,
        link_count: Option<u32>,
        named_streams: Option<bool>,
    }

    fn fresh_root_preflight_diagnostic(
        binding: &RootBinding,
    ) -> Result<FreshRootPreflightDiagnostic, FreshRootPreflightDiagnostic> {
        let writer_root =
            acquire_writer_root(binding).map_err(|_| FreshRootPreflightDiagnostic {
                stage: FreshRootPreflightStage::AcquireWriterRoot,
                attributes: None,
                link_count: None,
                named_streams: None,
            })?;
        relative_object_exists(&writer_root, "platform.installations")
            .and_then(|exists| verify_managed_root_lineage(binding, exists))
            .map_err(|_| FreshRootPreflightDiagnostic {
                stage: FreshRootPreflightStage::RootLineage,
                attributes: None,
                link_count: None,
                named_streams: None,
            })?;
        revalidate_writer_root(binding, &writer_root).map_err(|_| {
            FreshRootPreflightDiagnostic {
                stage: FreshRootPreflightStage::WriterRootRevalidation,
                attributes: None,
                link_count: None,
                named_streams: None,
            }
        })?;
        let attributes =
            file_attributes(&writer_root).map_err(|_| FreshRootPreflightDiagnostic {
                stage: FreshRootPreflightStage::Attributes,
                attributes: None,
                link_count: None,
                named_streams: None,
            })?;
        if attributes
            & (FILE_ATTRIBUTE_DIRECTORY
                | FILE_ATTRIBUTE_HIDDEN
                | FILE_ATTRIBUTE_SYSTEM
                | FILE_ATTRIBUTE_REPARSE_POINT)
            != FILE_ATTRIBUTE_DIRECTORY
        {
            return Err(FreshRootPreflightDiagnostic {
                stage: FreshRootPreflightStage::Attributes,
                attributes: Some(attributes),
                link_count: None,
                named_streams: None,
            });
        }
        let link_count =
            FileIdentity::link_count(&writer_root).map_err(|_| FreshRootPreflightDiagnostic {
                stage: FreshRootPreflightStage::LinkCount,
                attributes: Some(attributes),
                link_count: None,
                named_streams: None,
            })?;
        if link_count != 1 {
            return Err(FreshRootPreflightDiagnostic {
                stage: FreshRootPreflightStage::LinkCount,
                attributes: Some(attributes),
                link_count: Some(link_count),
                named_streams: None,
            });
        }
        let named_streams =
            has_named_streams(&writer_root).map_err(|_| FreshRootPreflightDiagnostic {
                stage: FreshRootPreflightStage::NamedStreams,
                attributes: Some(attributes),
                link_count: Some(link_count),
                named_streams: None,
            })?;
        if named_streams {
            return Err(FreshRootPreflightDiagnostic {
                stage: FreshRootPreflightStage::NamedStreams,
                attributes: Some(attributes),
                link_count: Some(link_count),
                named_streams: Some(named_streams),
            });
        }
        Ok(FreshRootPreflightDiagnostic {
            stage: FreshRootPreflightStage::Complete,
            attributes: Some(attributes),
            link_count: Some(link_count),
            named_streams: Some(named_streams),
        })
    }

    #[derive(Debug, PartialEq, Eq)]
    enum DirectorySealDiagnosticStage {
        CapabilityRootRevalidation,
        CapabilityRootAttributes,
        CapabilityRootLinkCount,
        CapabilityRootStreamParser,
        SealPreDirectorySession,
        DirectorySessionMarkerBytes,
        DirectorySessionEntryDigestReproducedComplete,
        DirectorySessionEntryDigestPosition,
        DirectorySessionEntryDigestMetadata,
        DirectorySessionEntryDigestLengthMismatch,
        DirectorySessionEntryDigestSeekToStart,
        DirectorySessionEntryDigestRead,
        DirectorySessionEntryDigestSeekAfterRead,
        DirectorySessionEntryDigestHashMismatch,
        DirectorySessionEnumeration,
        SessionState,
        ObjectWriteLock,
        ObjectIdentity,
        ObjectAttributes,
        ObjectLinkCount,
        ObjectStreamParser,
        SealPostRootRevalidation,
        SealPostRootAttributes,
        SealPostRootLinkCount,
        SealPostRootStreamParser,
        Complete,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum DirectorySealDiagnosticObject {
        Root,
        Staging,
        RecoveryMarker,
        MaterializedEntry,
    }

    #[derive(Debug)]
    struct DirectorySealDiagnostic {
        stage: DirectorySealDiagnosticStage,
        object: DirectorySealDiagnosticObject,
        relative_path: Vec<String>,
        attributes: Option<u32>,
        link_count: Option<u32>,
        named_streams: Option<bool>,
        expected_size: Option<u64>,
        expected_sha256: Option<String>,
        metadata_length: Option<u64>,
        position_before_diagnostic: Option<u64>,
        seek_to_start_succeeded: Option<bool>,
        read_length: Option<u64>,
        read_failed: Option<bool>,
        actual_sha256: Option<String>,
        expected_bytes_hex: Option<String>,
        observed_bytes_hex: Option<String>,
        seek_after_read_succeeded: Option<bool>,
        position_restored: Option<bool>,
        desired_access: Option<u32>,
        share_access: Option<u32>,
    }

    fn directory_seal_diagnostic(
        stage: DirectorySealDiagnosticStage,
        object: DirectorySealDiagnosticObject,
        relative_path: Vec<String>,
        attributes: Option<u32>,
        link_count: Option<u32>,
        named_streams: Option<bool>,
    ) -> DirectorySealDiagnostic {
        DirectorySealDiagnostic {
            stage,
            object,
            relative_path,
            attributes,
            link_count,
            named_streams,
            expected_size: None,
            expected_sha256: None,
            metadata_length: None,
            position_before_diagnostic: None,
            seek_to_start_succeeded: None,
            read_length: None,
            read_failed: None,
            actual_sha256: None,
            expected_bytes_hex: None,
            observed_bytes_hex: None,
            seek_after_read_succeeded: None,
            position_restored: None,
            desired_access: None,
            share_access: None,
        }
    }

    fn directory_entry_digest_diagnostic(
        entry: &mut OwnedRelativeObject,
        manifest_entry: &DirectoryPayloadManifestEntry,
        relative_path: Vec<String>,
    ) -> DirectorySealDiagnostic {
        let expected_fixture_bytes =
            minimal_seal_diagnostic_fixture_bytes(&relative_path, manifest_entry);
        let mut diagnostic = directory_seal_diagnostic(
            DirectorySealDiagnosticStage::DirectorySessionEntryDigestReproducedComplete,
            DirectorySealDiagnosticObject::MaterializedEntry,
            relative_path,
            None,
            None,
            None,
        );
        diagnostic.expected_size = Some(manifest_entry.size);
        diagnostic.expected_sha256 = Some(manifest_entry.sha256.clone());
        diagnostic.desired_access = Some(entry.desired_access);
        diagnostic.share_access = Some(entry.share_access);

        let position = match entry.file.stream_position() {
            Ok(position) => {
                diagnostic.position_before_diagnostic = Some(position);
                position
            }
            Err(_) => {
                diagnostic.stage =
                    DirectorySealDiagnosticStage::DirectorySessionEntryDigestPosition;
                return diagnostic;
            }
        };
        let metadata_length = match entry.file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => {
                diagnostic.stage =
                    DirectorySealDiagnosticStage::DirectorySessionEntryDigestMetadata;
                return diagnostic;
            }
        };
        diagnostic.metadata_length = Some(metadata_length);
        if metadata_length != manifest_entry.size {
            diagnostic.stage =
                DirectorySealDiagnosticStage::DirectorySessionEntryDigestLengthMismatch;
            return diagnostic;
        }
        if entry.file.seek(SeekFrom::Start(0)).is_err() {
            diagnostic.stage = DirectorySealDiagnosticStage::DirectorySessionEntryDigestSeekToStart;
            diagnostic.seek_to_start_succeeded = Some(false);
            return diagnostic;
        }
        diagnostic.seek_to_start_succeeded = Some(true);

        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 8192];
        let mut read_length = 0_u64;
        let mut read_failed = false;
        let mut observed_fixture_bytes = Vec::new();
        loop {
            match entry.file.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    read_length += read as u64;
                    digest.update(&buffer[..read]);
                    if expected_fixture_bytes.is_some()
                        && observed_fixture_bytes.len() < MAX_DIRECTORY_SEAL_DIAGNOSTIC_BYTES
                    {
                        let end = read.min(
                            MAX_DIRECTORY_SEAL_DIAGNOSTIC_BYTES - observed_fixture_bytes.len(),
                        );
                        observed_fixture_bytes.extend_from_slice(&buffer[..end]);
                    }
                }
                Err(_) => {
                    read_failed = true;
                    break;
                }
            }
        }
        diagnostic.read_length = Some(read_length);
        diagnostic.read_failed = Some(read_failed);

        if read_failed {
            diagnostic.stage = DirectorySealDiagnosticStage::DirectorySessionEntryDigestRead;
        } else {
            let actual_sha256 = hex_encoded_digest(&digest.finalize());
            diagnostic.actual_sha256 = Some(actual_sha256.clone());
            if entry.file.seek(SeekFrom::Start(0)).is_err() {
                diagnostic.stage =
                    DirectorySealDiagnosticStage::DirectorySessionEntryDigestSeekAfterRead;
                diagnostic.seek_after_read_succeeded = Some(false);
            } else if actual_sha256 != manifest_entry.sha256 {
                diagnostic.stage =
                    DirectorySealDiagnosticStage::DirectorySessionEntryDigestHashMismatch;
                diagnostic.seek_after_read_succeeded = Some(true);
                if let Some(expected_fixture_bytes) = expected_fixture_bytes {
                    diagnostic.expected_bytes_hex =
                        Some(directory_seal_diagnostic_bytes_hex(expected_fixture_bytes));
                    diagnostic.observed_bytes_hex =
                        Some(directory_seal_diagnostic_bytes_hex(&observed_fixture_bytes));
                }
            } else {
                diagnostic.seek_after_read_succeeded = Some(true);
            }
        }
        diagnostic.position_restored = Some(entry.file.seek(SeekFrom::Start(position)).is_ok());
        diagnostic
    }

    const MAX_DIRECTORY_SEAL_DIAGNOSTIC_BYTES: usize = 64;
    const MINIMAL_SEAL_DIAGNOSTIC_FIXTURE_PATH: &str = "seal-stage-diagnostic.bin";
    const MINIMAL_SEAL_DIAGNOSTIC_FIXTURE_BYTES: &[u8] = b"seal-stage-diagnostic";

    fn minimal_seal_diagnostic_fixture_bytes(
        relative_path: &[String],
        manifest_entry: &DirectoryPayloadManifestEntry,
    ) -> Option<&'static [u8]> {
        (relative_path.len() == 1
            && relative_path[0] == MINIMAL_SEAL_DIAGNOSTIC_FIXTURE_PATH
            && manifest_entry.size == MINIMAL_SEAL_DIAGNOSTIC_FIXTURE_BYTES.len() as u64
            && manifest_entry.sha256 == hex_digest(MINIMAL_SEAL_DIAGNOSTIC_FIXTURE_BYTES))
        .then_some(MINIMAL_SEAL_DIAGNOSTIC_FIXTURE_BYTES)
    }

    fn directory_seal_diagnostic_bytes_hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .take(MAX_DIRECTORY_SEAL_DIAGNOSTIC_BYTES)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn writer_root_seal_diagnostic(
        capability: &ManagedRootCapability,
        post_seal: bool,
    ) -> Option<DirectorySealDiagnostic> {
        let root_stage = |capability_stage, post_seal_stage| {
            if post_seal {
                post_seal_stage
            } else {
                capability_stage
            }
        };
        if revalidate_writer_root(&capability.binding, &capability.writer_root).is_err() {
            return Some(directory_seal_diagnostic(
                root_stage(
                    DirectorySealDiagnosticStage::CapabilityRootRevalidation,
                    DirectorySealDiagnosticStage::SealPostRootRevalidation,
                ),
                DirectorySealDiagnosticObject::Root,
                Vec::new(),
                None,
                None,
                None,
            ));
        }
        let attributes = match file_attributes(&capability.writer_root) {
            Ok(attributes) => attributes,
            Err(_) => {
                return Some(directory_seal_diagnostic(
                    root_stage(
                        DirectorySealDiagnosticStage::CapabilityRootAttributes,
                        DirectorySealDiagnosticStage::SealPostRootAttributes,
                    ),
                    DirectorySealDiagnosticObject::Root,
                    Vec::new(),
                    None,
                    None,
                    None,
                ));
            }
        };
        if attributes
            & (FILE_ATTRIBUTE_DIRECTORY
                | FILE_ATTRIBUTE_HIDDEN
                | FILE_ATTRIBUTE_SYSTEM
                | FILE_ATTRIBUTE_REPARSE_POINT)
            != FILE_ATTRIBUTE_DIRECTORY
        {
            return Some(directory_seal_diagnostic(
                root_stage(
                    DirectorySealDiagnosticStage::CapabilityRootAttributes,
                    DirectorySealDiagnosticStage::SealPostRootAttributes,
                ),
                DirectorySealDiagnosticObject::Root,
                Vec::new(),
                Some(attributes),
                None,
                None,
            ));
        }
        let link_count = match FileIdentity::link_count(&capability.writer_root) {
            Ok(link_count) => link_count,
            Err(_) => {
                return Some(directory_seal_diagnostic(
                    root_stage(
                        DirectorySealDiagnosticStage::CapabilityRootLinkCount,
                        DirectorySealDiagnosticStage::SealPostRootLinkCount,
                    ),
                    DirectorySealDiagnosticObject::Root,
                    Vec::new(),
                    Some(attributes),
                    None,
                    None,
                ));
            }
        };
        if link_count != 1 {
            return Some(directory_seal_diagnostic(
                root_stage(
                    DirectorySealDiagnosticStage::CapabilityRootLinkCount,
                    DirectorySealDiagnosticStage::SealPostRootLinkCount,
                ),
                DirectorySealDiagnosticObject::Root,
                Vec::new(),
                Some(attributes),
                Some(link_count),
                None,
            ));
        }
        match has_named_streams(&capability.writer_root) {
            Ok(false) => None,
            Ok(true) => Some(directory_seal_diagnostic(
                root_stage(
                    DirectorySealDiagnosticStage::CapabilityRootStreamParser,
                    DirectorySealDiagnosticStage::SealPostRootStreamParser,
                ),
                DirectorySealDiagnosticObject::Root,
                Vec::new(),
                Some(attributes),
                Some(link_count),
                Some(true),
            )),
            Err(_) => Some(directory_seal_diagnostic(
                root_stage(
                    DirectorySealDiagnosticStage::CapabilityRootStreamParser,
                    DirectorySealDiagnosticStage::SealPostRootStreamParser,
                ),
                DirectorySealDiagnosticObject::Root,
                Vec::new(),
                Some(attributes),
                Some(link_count),
                None,
            )),
        }
    }

    fn session_object_seal_diagnostic(
        object: &OwnedRelativeObject,
        expected_directory: bool,
        object_kind: DirectorySealDiagnosticObject,
        relative_path: Vec<String>,
    ) -> Option<DirectorySealDiagnostic> {
        if verify_session_write_lock(object, expected_directory).is_ok() {
            return None;
        }
        if verify_relative_object(object, expected_directory).is_ok() {
            return Some(directory_seal_diagnostic(
                DirectorySealDiagnosticStage::ObjectWriteLock,
                object_kind,
                relative_path,
                None,
                None,
                None,
            ));
        }
        if FileIdentity::from_file(&object.file)
            .map(|identity| identity != object.identity)
            .unwrap_or(true)
        {
            return Some(directory_seal_diagnostic(
                DirectorySealDiagnosticStage::ObjectIdentity,
                object_kind,
                relative_path,
                None,
                None,
                None,
            ));
        }
        let attributes = match file_attributes(&object.file) {
            Ok(attributes) => attributes,
            Err(_) => {
                return Some(directory_seal_diagnostic(
                    DirectorySealDiagnosticStage::ObjectAttributes,
                    object_kind,
                    relative_path,
                    None,
                    None,
                    None,
                ));
            }
        };
        let expected_attributes = if expected_directory {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            0
        };
        if attributes
            & (FILE_ATTRIBUTE_DIRECTORY
                | FILE_ATTRIBUTE_HIDDEN
                | FILE_ATTRIBUTE_SYSTEM
                | FILE_ATTRIBUTE_REPARSE_POINT)
            != expected_attributes
        {
            return Some(directory_seal_diagnostic(
                DirectorySealDiagnosticStage::ObjectAttributes,
                object_kind,
                relative_path,
                Some(attributes),
                None,
                None,
            ));
        }
        let link_count = match FileIdentity::link_count(&object.file) {
            Ok(link_count) => link_count,
            Err(_) => {
                return Some(directory_seal_diagnostic(
                    DirectorySealDiagnosticStage::ObjectLinkCount,
                    object_kind,
                    relative_path,
                    Some(attributes),
                    None,
                    None,
                ));
            }
        };
        if link_count != 1 {
            return Some(directory_seal_diagnostic(
                DirectorySealDiagnosticStage::ObjectLinkCount,
                object_kind,
                relative_path,
                Some(attributes),
                Some(link_count),
                None,
            ));
        }
        match has_named_streams(&object.file) {
            Ok(false) => Some(directory_seal_diagnostic(
                DirectorySealDiagnosticStage::ObjectWriteLock,
                object_kind,
                relative_path,
                Some(attributes),
                Some(link_count),
                Some(false),
            )),
            Ok(true) => Some(directory_seal_diagnostic(
                DirectorySealDiagnosticStage::ObjectStreamParser,
                object_kind,
                relative_path,
                Some(attributes),
                Some(link_count),
                Some(true),
            )),
            Err(_) => Some(directory_seal_diagnostic(
                DirectorySealDiagnosticStage::ObjectStreamParser,
                object_kind,
                relative_path,
                Some(attributes),
                Some(link_count),
                None,
            )),
        }
    }

    fn failed_directory_session_diagnostic(session: &mut WriterSession) -> DirectorySealDiagnostic {
        let (marker_bytes, manifest_entries) = match session.directory_manifest.as_ref() {
            Some(manifest) => (manifest_marker(manifest), manifest.entries.clone()),
            None => {
                return directory_seal_diagnostic(
                    DirectorySealDiagnosticStage::SessionState,
                    DirectorySealDiagnosticObject::Staging,
                    vec![STAGING_DIRECTORY.to_owned()],
                    None,
                    None,
                    None,
                );
            }
        };
        let marker = match ready_session_object_mut(session.recovery_marker.as_mut(), false) {
            Ok(marker) => marker,
            Err(_) => {
                return directory_seal_diagnostic(
                    DirectorySealDiagnosticStage::SessionState,
                    DirectorySealDiagnosticObject::RecoveryMarker,
                    vec![STAGING_SESSION_MARKER.to_owned()],
                    None,
                    None,
                    None,
                );
            }
        };
        if verify_relative_object(marker, false).is_err() {
            return session_object_seal_diagnostic(
                marker,
                false,
                DirectorySealDiagnosticObject::RecoveryMarker,
                vec![STAGING_SESSION_MARKER.to_owned()],
            )
            .unwrap_or_else(|| {
                directory_seal_diagnostic(
                    DirectorySealDiagnosticStage::SealPreDirectorySession,
                    DirectorySealDiagnosticObject::RecoveryMarker,
                    vec![STAGING_SESSION_MARKER.to_owned()],
                    None,
                    None,
                    None,
                )
            });
        }
        if verify_file_bytes(&mut marker.file, &marker_bytes).is_err() {
            return directory_seal_diagnostic(
                DirectorySealDiagnosticStage::DirectorySessionMarkerBytes,
                DirectorySealDiagnosticObject::RecoveryMarker,
                vec![STAGING_SESSION_MARKER.to_owned()],
                None,
                None,
                None,
            );
        }
        for (entry, path) in session
            .directory_entries
            .iter_mut()
            .zip(&session.directory_entry_paths)
        {
            if verify_relative_object(entry, entry.directory).is_err() {
                return session_object_seal_diagnostic(
                    entry,
                    entry.directory,
                    DirectorySealDiagnosticObject::MaterializedEntry,
                    path.clone(),
                )
                .unwrap_or_else(|| {
                    directory_seal_diagnostic(
                        DirectorySealDiagnosticStage::SealPreDirectorySession,
                        DirectorySealDiagnosticObject::MaterializedEntry,
                        path.clone(),
                        None,
                        None,
                        None,
                    )
                });
            }
            if !entry.directory {
                let manifest_entry = match manifest_entries
                    .iter()
                    .find(|manifest_entry| manifest_entry.relative_segments == *path)
                {
                    Some(manifest_entry) => manifest_entry,
                    None => {
                        return directory_seal_diagnostic(
                            DirectorySealDiagnosticStage::SealPreDirectorySession,
                            DirectorySealDiagnosticObject::MaterializedEntry,
                            path.clone(),
                            None,
                            None,
                            None,
                        );
                    }
                };
                if verify_file_digest(&mut entry.file, manifest_entry).is_err() {
                    return directory_entry_digest_diagnostic(entry, manifest_entry, path.clone());
                }
            }
        }
        if verify_directory_enumeration(session).is_err() {
            return directory_seal_diagnostic(
                DirectorySealDiagnosticStage::DirectorySessionEnumeration,
                DirectorySealDiagnosticObject::Staging,
                vec![STAGING_DIRECTORY.to_owned()],
                None,
                None,
                None,
            );
        }
        directory_seal_diagnostic(
            DirectorySealDiagnosticStage::SealPreDirectorySession,
            DirectorySealDiagnosticObject::Staging,
            vec![STAGING_DIRECTORY.to_owned()],
            None,
            None,
            None,
        )
    }

    fn pre_seal_directory_diagnostic(
        capability: &ManagedRootCapability,
    ) -> DirectorySealDiagnostic {
        if let Some(diagnostic) = writer_root_seal_diagnostic(capability, false) {
            return diagnostic;
        }
        let mut session = match capability.session.lock() {
            Ok(session) => session,
            Err(_) => {
                return directory_seal_diagnostic(
                    DirectorySealDiagnosticStage::SessionState,
                    DirectorySealDiagnosticObject::Root,
                    Vec::new(),
                    None,
                    None,
                    None,
                );
            }
        };
        if verify_directory_session(&mut session).is_err() {
            return failed_directory_session_diagnostic(&mut session);
        }
        let staging = match ready_session_object(session.staging.as_ref(), true) {
            Ok(staging) => staging,
            Err(_) => {
                return directory_seal_diagnostic(
                    DirectorySealDiagnosticStage::SessionState,
                    DirectorySealDiagnosticObject::Staging,
                    vec![STAGING_DIRECTORY.to_owned()],
                    None,
                    None,
                    None,
                );
            }
        };
        if let Some(diagnostic) = session_object_seal_diagnostic(
            staging,
            true,
            DirectorySealDiagnosticObject::Staging,
            vec![STAGING_DIRECTORY.to_owned()],
        ) {
            return diagnostic;
        }
        let marker = match ready_session_object(session.recovery_marker.as_ref(), false) {
            Ok(marker) => marker,
            Err(_) => {
                return directory_seal_diagnostic(
                    DirectorySealDiagnosticStage::SessionState,
                    DirectorySealDiagnosticObject::RecoveryMarker,
                    vec![STAGING_SESSION_MARKER.to_owned()],
                    None,
                    None,
                    None,
                );
            }
        };
        if let Some(diagnostic) = session_object_seal_diagnostic(
            marker,
            false,
            DirectorySealDiagnosticObject::RecoveryMarker,
            vec![STAGING_SESSION_MARKER.to_owned()],
        ) {
            return diagnostic;
        }
        for (entry, path) in session
            .directory_entries
            .iter()
            .zip(&session.directory_entry_paths)
        {
            if let Some(diagnostic) = session_object_seal_diagnostic(
                entry,
                entry.directory,
                DirectorySealDiagnosticObject::MaterializedEntry,
                path.clone(),
            ) {
                return diagnostic;
            }
        }
        drop(session);
        if let Some(diagnostic) = writer_root_seal_diagnostic(capability, true) {
            return diagnostic;
        }
        directory_seal_diagnostic(
            DirectorySealDiagnosticStage::Complete,
            DirectorySealDiagnosticObject::Root,
            Vec::new(),
            None,
            None,
            None,
        )
    }

    fn directory_manifest(entries: Vec<(Vec<&str>, &[u8])>) -> DirectoryPayloadManifest {
        DirectoryPayloadManifest {
            entries: entries
                .into_iter()
                .map(|(segments, bytes)| DirectoryPayloadManifestEntry {
                    relative_segments: segments.into_iter().map(str::to_owned).collect(),
                    size: bytes.len() as u64,
                    sha256: hex_digest(bytes),
                })
                .collect(),
        }
    }

    #[derive(Clone)]
    struct TestDirectoryPayload {
        relative_segments: Vec<String>,
        bytes: Vec<u8>,
    }

    fn managed_version_payloads(
        managed_mcp_id: &str,
        version: &str,
        executable: &[u8],
    ) -> (DirectoryPayloadManifest, Vec<TestDirectoryPayload>) {
        let descriptor = format!(r#"{{"version":"{version}"}}"#).into_bytes();
        let mut committed = vec![
            (".goose-runtime-descriptor.json".to_owned(), descriptor),
            ("server.exe".to_owned(), executable.to_vec()),
        ];
        committed.sort_by(|left, right| left.0.cmp(&right.0));
        let mut tree = Sha256::new();
        for (leaf, bytes) in &committed {
            tree.update(leaf.as_bytes());
            tree.update([0]);
            tree.update((bytes.len() as u64).to_le_bytes());
            tree.update(hex_digest(bytes).as_bytes());
        }
        let descriptor_sha256 = committed
            .iter()
            .find(|(leaf, _)| leaf == ".goose-runtime-descriptor.json")
            .map(|(_, bytes)| hex_digest(bytes))
            .expect("descriptor");
        let evidence = serde_json::to_vec(&serde_json::json!({
            "descriptor_sha256": descriptor_sha256,
            "tree_sha256": hex_digest(&tree.finalize()),
        }))
        .expect("evidence");
        committed.push((COMMITTED_VERSION_EVIDENCE.to_owned(), evidence));
        let payloads = committed
            .into_iter()
            .map(|(leaf, bytes)| TestDirectoryPayload {
                relative_segments: vec![
                    managed_mcp_id.to_owned(),
                    "versions".to_owned(),
                    version.to_owned(),
                    leaf,
                ],
                bytes,
            })
            .collect::<Vec<_>>();
        let manifest = DirectoryPayloadManifest {
            entries: payloads
                .iter()
                .map(|payload| DirectoryPayloadManifestEntry {
                    relative_segments: payload.relative_segments.clone(),
                    size: payload.bytes.len() as u64,
                    sha256: hex_digest(&payload.bytes),
                })
                .collect(),
        };
        (manifest, payloads)
    }

    fn copy_managed_version_tree(
        root: &std::path::Path,
        managed_mcp_id: &str,
        version: &str,
        payloads: &[TestDirectoryPayload],
    ) {
        let target = root
            .join("platform.installations")
            .join(managed_mcp_id)
            .join("versions")
            .join(version);
        std::fs::create_dir_all(&target).expect("attacker target tree");
        for payload in payloads {
            let leaf = payload
                .relative_segments
                .last()
                .expect("managed payload leaf");
            std::fs::write(target.join(leaf), &payload.bytes).expect("copy payload bytes");
        }
    }

    #[test]
    fn leaf_validation_rejects_paths_ads_and_windows_aliases() {
        for invalid in [
            "",
            ".",
            "..",
            "a/b",
            r"a\\b",
            "a:b",
            "a.",
            "a ",
            "CON",
            "CON.txt",
            "PRN.log",
            "COM1.log",
            "LPT9.data",
            "payload..",
        ] {
            assert!(validated_leaf_name(invalid).is_err(), "{invalid}");
            assert!(validated_read_leaf_name(invalid).is_err(), "{invalid}");
        }
        assert!(validated_leaf_name("goose-stage_1").is_ok());
        assert!(validated_read_leaf_name("active.json").is_ok());
    }

    fn stream_information_entry(name: &str) -> Vec<u8> {
        let name = name.encode_utf16().collect::<Vec<_>>();
        let mut entry = vec![0_u8; 24 + name.len() * 2];
        entry[4..8].copy_from_slice(&((name.len() * 2) as u32).to_ne_bytes());
        for (index, unit) in name.into_iter().enumerate() {
            entry[24 + index * 2..26 + index * 2].copy_from_slice(&unit.to_ne_bytes());
        }
        entry
    }

    #[test]
    fn named_stream_parsing_obeys_reported_information() {
        let default_stream = stream_information_entry("::$DATA");
        let mut buffer = vec![0xa5_u8; 64];
        assert!(!has_named_streams_in_information(&buffer, 0).expect("empty result"));

        buffer[..default_stream.len()].copy_from_slice(&default_stream);
        assert!(
            !has_named_streams_in_information(&buffer, default_stream.len())
                .expect("default stream")
        );
        let zero_name_stream = stream_information_entry("");
        assert!(
            !has_named_streams_in_information(&zero_name_stream, zero_name_stream.len())
                .expect("zero-name default stream")
        );
        assert!(has_named_streams_in_information(&buffer, default_stream.len() - 1).is_err());

        buffer[default_stream.len()] = 0xff;
        assert!(
            !has_named_streams_in_information(&buffer, default_stream.len())
                .expect("reported boundary ignores unused buffer")
        );
        assert!(has_named_streams_in_information(&buffer, default_stream.len() + 1).is_err());

        for named_stream in [
            ":DirectoryNamedAds:$DATA",
            ":NamedAds:$DATA",
            "::$INDEX_ALLOCATION",
        ] {
            let named_stream = stream_information_entry(named_stream);
            assert!(
                has_named_streams_in_information(&named_stream, named_stream.len())
                    .expect("named stream is rejected")
            );
        }
    }

    fn directory_information_buffer(names: &[&str]) -> Vec<u8> {
        let mut buffer = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let name = name.encode_utf16().collect::<Vec<_>>();
            let name_length = name.len() * 2;
            let record_length = 64 + name_length;
            let next = (index + 1 < names.len()).then(|| (record_length + 7) & !7);
            let offset = buffer.len();
            buffer.resize(offset + next.unwrap_or(record_length), 0);
            buffer[offset..offset + 4].copy_from_slice(&(next.unwrap_or(0) as u32).to_ne_bytes());
            buffer[offset + 60..offset + 64].copy_from_slice(&(name_length as u32).to_ne_bytes());
            for (index, unit) in name.into_iter().enumerate() {
                buffer[offset + 64 + index * 2..offset + 66 + index * 2]
                    .copy_from_slice(&unit.to_ne_bytes());
            }
        }
        buffer
    }

    fn parsed_directory_names(
        buffer: &[u8],
        information: usize,
    ) -> McpPlatformResult<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        let mut saw_current_directory = false;
        let mut saw_parent_directory = false;
        parse_directory_information_buffer(
            buffer,
            information,
            &mut names,
            &mut saw_current_directory,
            &mut saw_parent_directory,
        )?;
        Ok(names)
    }

    #[test]
    fn directory_information_parser_rejects_reported_information_past_the_buffer() {
        let buffer = directory_information_buffer(&["payload"]);
        assert!(parsed_directory_names(&buffer, buffer.len() + 1).is_err());
    }

    #[test]
    fn directory_information_parser_accepts_aligned_records_and_filters_pseudo_entries() {
        let current_and_real = directory_information_buffer(&[".", "payload"]);
        assert_eq!(
            u32::from_ne_bytes(current_and_real[0..4].try_into().unwrap()),
            72
        );
        assert!(current_and_real[66..72].iter().all(|byte| *byte == 0));
        assert_eq!(
            parsed_directory_names(&current_and_real, current_and_real.len())
                .expect("dot filtered"),
            BTreeSet::from(["payload".to_owned()])
        );

        let parent_and_real = directory_information_buffer(&["..", "artifact"]);
        assert_eq!(
            parsed_directory_names(&parent_and_real, parent_and_real.len())
                .expect("parent filtered"),
            BTreeSet::from(["artifact".to_owned()])
        );

        let pseudo_entries_and_real = directory_information_buffer(&[".", "..", "payload"]);
        assert_eq!(
            parsed_directory_names(&pseudo_entries_and_real, pseudo_entries_and_real.len())
                .expect("pseudo entries filtered"),
            BTreeSet::from(["payload".to_owned()])
        );
    }

    #[test]
    fn directory_information_parser_rejects_malformed_nonterminal_offsets() {
        let mut unaligned = directory_information_buffer(&["first", "last"]);
        unaligned[0..4].copy_from_slice(&68_u32.to_ne_bytes());
        assert!(parsed_directory_names(&unaligned, unaligned.len()).is_err());

        let mut name_crosses_entry = directory_information_buffer(&["first", "last"]);
        let next = u32::from_ne_bytes(name_crosses_entry[0..4].try_into().unwrap());
        name_crosses_entry[60..64].copy_from_slice(&(next - 64 + 2).to_ne_bytes());
        assert!(parsed_directory_names(&name_crosses_entry, name_crosses_entry.len()).is_err());

        let mut entry_ends_at_information = directory_information_buffer(&["first", "last"]);
        let information = entry_ends_at_information.len() as u32;
        entry_ends_at_information[0..4].copy_from_slice(&information.to_ne_bytes());
        assert!(parsed_directory_names(
            &entry_ends_at_information,
            entry_ends_at_information.len()
        )
        .is_err());
    }

    #[test]
    fn directory_information_parser_rejects_terminal_tail_bytes() {
        for tail in [0_u8, 0xa5] {
            let mut buffer = directory_information_buffer(&["payload"]);
            buffer.push(tail);
            assert!(parsed_directory_names(&buffer, buffer.len()).is_err());
        }
    }

    #[test]
    fn directory_information_parser_rejects_duplicate_pseudo_and_real_entries() {
        for pseudo_entry in [".", ".."] {
            let mut names = BTreeSet::new();
            let mut saw_current_directory = false;
            let mut saw_parent_directory = false;
            let buffer = directory_information_buffer(&[pseudo_entry]);
            parse_directory_information_buffer(
                &buffer,
                buffer.len(),
                &mut names,
                &mut saw_current_directory,
                &mut saw_parent_directory,
            )
            .expect("first pseudo entry");
            assert!(parse_directory_information_buffer(
                &buffer,
                buffer.len(),
                &mut names,
                &mut saw_current_directory,
                &mut saw_parent_directory,
            )
            .is_err());
        }

        let invalid = directory_information_buffer(&["invalid/name"]);
        assert!(parsed_directory_names(&invalid, invalid.len()).is_err());
        let duplicate = directory_information_buffer(&["payload", "payload"]);
        assert!(parsed_directory_names(&duplicate, duplicate.len()).is_err());
    }

    #[test]
    fn illegal_leaf_never_creates_an_entry_in_a_real_temporary_root() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let root = test_root_handle(temporary.path());
        for invalid in ["../escape", "payload:stream", r"a\\b", "NUL"] {
            let mut destination = None;
            assert!(create_relative(
                &root,
                invalid,
                false,
                FILE_READ_ATTRIBUTES,
                &mut destination,
            )
            .is_err());
            assert!(destination.is_none());
        }
        assert!(std::fs::read_dir(temporary.path())
            .expect("root entries")
            .next()
            .is_none());
    }

    #[test]
    fn writer_source_has_only_nt_handle_relative_mutation() {
        let source = include_str!("windows_managed_writer.rs");
        let preflight_source = include_str!("windows_storage_preflight.rs");
        let production_source = source
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production writer source");
        for forbidden in [
            concat!("Open", "Options"),
            concat!("tokio", "::fs"),
            concat!("Move", "FileEx"),
            concat!("Replace", "File"),
            concat!("rename", "("),
            concat!("remove", "_"),
        ] {
            assert!(
                !production_source.contains(forbidden),
                "forbidden fallback: {forbidden}"
            );
        }
        assert!(production_source.contains("NtCreateFile"));
        assert!(production_source.contains("OBJ_DONT_REPARSE"));
        assert!(production_source.contains("FILE_OPEN_REPARSE_POINT"));
        assert!(!production_source.contains("fn binding("));
        assert!(!production_source.contains("pub(super) fn binding"));
        let writer_root_open = &preflight_source[preflight_source
            .find("fn open_writer_directory_handle")
            .unwrap()
            ..preflight_source.find("fn open_handle").unwrap()];
        assert!(writer_root_open.contains(".share_mode(FILE_SHARE_READ)"));
        assert!(!writer_root_open.contains("FILE_SHARE_WRITE"));
    }

    #[test]
    fn root_binding_and_capability_do_not_export_root_handles_or_paths() {
        let preflight_source = include_str!("windows_storage_preflight.rs");
        let binding = &preflight_source[preflight_source
            .find("pub(super) struct RootBinding")
            .unwrap()
            ..preflight_source
                .find("struct ManagedStorageCapacityWitness")
                .unwrap()];
        for forbidden in [
            "pub(super) lexical",
            "pub(super) root_file",
            "pub(super) expected_final_path",
        ] {
            assert!(
                !binding.contains(forbidden),
                "RootBinding leaked: {forbidden}"
            );
        }

        let writer_source = include_str!("windows_managed_writer.rs");
        let capability = &writer_source[writer_source
            .find("pub(super) struct ManagedRootCapability")
            .unwrap()
            ..writer_source
                .find("impl std::fmt::Debug for ManagedRootCapability")
                .unwrap()];
        assert!(!capability.contains("pub(super) fn binding"));
        assert!(!capability.contains("AsRawHandle"));
    }

    #[test]
    fn nt_create_result_requires_nt_and_io_status_created_and_valid_handle() {
        let valid_handle = std::ptr::NonNull::<c_void>::dangling().as_ptr();
        let created = IoStatusBlock {
            status: 0,
            information: FILE_CREATED,
        };
        assert!(create_succeeded(0, &created, valid_handle));
        assert!(!create_succeeded(-1, &created, valid_handle));
        assert!(!create_succeeded(
            0,
            &IoStatusBlock {
                status: -1,
                information: FILE_CREATED
            },
            valid_handle
        ));
        assert!(!create_succeeded(
            0,
            &IoStatusBlock {
                status: 0,
                information: 0
            },
            valid_handle
        ));
        assert!(!create_succeeded(0, &created, std::ptr::null_mut()));
        assert!(!create_succeeded(0, &created, INVALID_HANDLE_VALUE));
    }

    #[test]
    fn real_temporary_root_creates_one_handle_relative_staging_payload() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("test root binding"),
        )
        .expect("capability");
        let digest = hex_digest(b"verified payload");

        capability.create_staging().expect("staging");
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());
        capability
            .write_verified_payload(b"verified payload", &digest)
            .expect("payload");
        capability.verify_regular_payload().expect("verification");

        assert_eq!(
            std::fs::read(temporary.path().join(STAGING_DIRECTORY).join(PAYLOAD_FILE))
                .expect("payload bytes"),
            b"verified payload"
        );

        capability.discard_uncommitted().expect("session cleanup");
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn fresh_temporary_root_preflight_reports_writer_safety_stage_before_capability_construction() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let binding = test_root_binding(test_root_handle(temporary.path()))
            .expect("fresh temporary root binding");
        let diagnostic = fresh_root_preflight_diagnostic(&binding)
            .unwrap_or_else(|diagnostic| panic!("fresh root preflight diagnostic: {diagnostic:?}"));

        assert_eq!(
            diagnostic.stage,
            FreshRootPreflightStage::Complete,
            "fresh root preflight diagnostic: {diagnostic:?}"
        );
        assert_eq!(
            diagnostic.attributes.map(|attributes| {
                attributes
                    & (FILE_ATTRIBUTE_DIRECTORY
                        | FILE_ATTRIBUTE_HIDDEN
                        | FILE_ATTRIBUTE_SYSTEM
                        | FILE_ATTRIBUTE_REPARSE_POINT)
            }),
            Some(FILE_ATTRIBUTE_DIRECTORY),
            "fresh root preflight diagnostic: {diagnostic:?}"
        );
        assert_eq!(
            diagnostic.link_count,
            Some(1),
            "fresh root preflight diagnostic: {diagnostic:?}"
        );
        assert_eq!(
            diagnostic.named_streams,
            Some(false),
            "fresh root preflight diagnostic: {diagnostic:?}"
        );

        ManagedRootCapability::from_verified_preflight(binding).unwrap_or_else(|_| {
            panic!("fresh root capability construction failed after {diagnostic:?}")
        });
    }

    #[test]
    fn streaming_payload_digest_hex_matches_payload_digest_and_seals_directory() {
        let bytes = b"streamed cache artifact";
        let mut streaming_digest = Sha256::new();
        for chunk in bytes.chunks(3) {
            streaming_digest.update(chunk);
        }
        assert_eq!(
            hex_encoded_digest(&streaming_digest.finalize()),
            hex_digest(bytes)
        );

        let temporary = tempfile::tempdir().expect("temporary root");
        let manifest = directory_manifest(vec![(vec!["payload"], bytes)]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec!["payload".to_owned()],
                bytes,
            }])
            .expect("materialize manifest bytes");
        capability
            .seal_staged_directory()
            .expect("streaming digest validates directory seal");
    }

    #[test]
    fn nested_directory_session_seals_after_enumerating_staging_and_descendants() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let bytes = b"nested directory artifact";
        let manifest = directory_manifest(vec![(vec!["nested", "artifact"], bytes)]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec!["nested".to_owned(), "artifact".to_owned()],
                bytes,
            }])
            .expect("materialize nested manifest bytes");
        capability
            .seal_staged_directory()
            .expect("enumerate staging and nested directory");

        assert_eq!(
            std::fs::read(
                temporary
                    .path()
                    .join(STAGING_DIRECTORY)
                    .join("nested")
                    .join("artifact")
            )
            .expect("nested staged bytes"),
            bytes
        );
        capability
            .discard_uncommitted()
            .expect("discard owned nested staging");
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn directory_enumeration_excludes_kernel_pseudo_entries_before_single_and_nested_seal() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let top_level_bytes = b"top-level directory artifact";
        let nested_bytes = b"nested directory artifact";
        let manifest = directory_manifest(vec![
            (vec!["top-level"], top_level_bytes),
            (vec!["nested", "artifact"], nested_bytes),
        ]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(&[
                DirectoryPayload {
                    relative_segments: vec!["top-level".to_owned()],
                    bytes: top_level_bytes,
                },
                DirectoryPayload {
                    relative_segments: vec!["nested".to_owned(), "artifact".to_owned()],
                    bytes: nested_bytes,
                },
            ])
            .expect("materialize single and nested manifest bytes");

        {
            let session = capability.session.lock().expect("session");
            let staging = ready_session_object(session.staging.as_ref(), true)
                .expect("owned staging directory");
            assert_eq!(
                enumerate_directory_names(&staging.file).expect("enumerate staging"),
                BTreeSet::from(["nested".to_owned(), "top-level".to_owned()])
            );
            let nested = session
                .directory_entries
                .iter()
                .zip(&session.directory_entry_paths)
                .find_map(|(entry, path)| {
                    (entry.directory
                        && path.len() == 1
                        && path.first().is_some_and(|segment| segment == "nested"))
                    .then_some(&entry.file)
                })
                .expect("owned nested directory");
            assert_eq!(
                enumerate_directory_names(nested).expect("enumerate nested directory"),
                BTreeSet::from(["artifact".to_owned()])
            );
        }

        capability
            .seal_staged_directory()
            .expect("seal after excluding kernel pseudo entries");
    }

    #[test]
    fn directory_seal_stage_diagnostic_reports_the_first_minimal_flow_failure() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let bytes = MINIMAL_SEAL_DIAGNOSTIC_FIXTURE_BYTES;
        let manifest =
            directory_manifest(vec![(vec![MINIMAL_SEAL_DIAGNOSTIC_FIXTURE_PATH], bytes)]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec![MINIMAL_SEAL_DIAGNOSTIC_FIXTURE_PATH.to_owned()],
                bytes,
            }])
            .expect("materialize manifest bytes");

        let diagnostic = pre_seal_directory_diagnostic(&capability);
        assert_eq!(
            diagnostic.stage,
            DirectorySealDiagnosticStage::Complete,
            "minimal directory seal preflight failed: {diagnostic:?}"
        );
        capability.seal_staged_directory().unwrap_or_else(|error| {
            panic!("minimal directory seal failed after diagnostic {diagnostic:?}: {error:?}")
        });
    }

    #[test]
    fn verified_directory_manifest_materializes_and_promotes_cache_without_path_mutation() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let bytes = b"cache artifact";
        let digest = hex_digest(bytes);
        let manifest = directory_manifest(vec![(vec![digest.as_str()], bytes)]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec![digest.clone()],
                bytes,
            }])
            .expect("materialize manifest bytes");
        capability
            .seal_staged_directory()
            .expect("enumerate and validate staging");
        capability
            .promote_staged_directory(RuntimeStorageCommittedLeaf::Cache)
            .expect("atomic no-replace promotion");

        assert_eq!(
            std::fs::read(temporary.path().join("platform.cache").join(&digest))
                .expect("committed bytes"),
            bytes
        );
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn cache_final_release_failure_rearms_descendants_before_retaining_the_root() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let bytes = b"cache artifact";
        let digest = hex_digest(bytes);
        let manifest = directory_manifest(vec![(
            vec!["layer-one", "layer-two", digest.as_str()],
            bytes,
        )]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec!["layer-one".to_owned(), "layer-two".to_owned(), digest],
                bytes,
            }])
            .expect("materialize manifest bytes");
        capability
            .seal_staged_directory()
            .expect("enumerate and validate staging");
        inject_test_failure(TestFailurePoint::FinalTreeRootReleaseDisposition);
        assert!(capability
            .promote_staged_directory(RuntimeStorageCommittedLeaf::Cache)
            .is_err());

        assert!(
            !temporary.path().join("platform.cache").exists(),
            "a failed final release must not leave an incomplete cache tree"
        );
    }

    #[test]
    fn committed_anchor_publish_error_is_reconciled_before_releasing_the_final_tree() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (manifest, payloads) =
            managed_version_payloads("managed_000001", "1.0.0", b"committed version");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("materialize version");
        capability.seal_staged_directory().expect("seal version");
        inject_test_failure(TestFailurePoint::PublishThenErrorAtCommitted);
        capability
            .promote_staged_managed_installation()
            .expect("persisted committed checkpoint must be reconciled");

        assert!(capability
            .managed_installation_version_is_verified("managed_000001", "1.0.0")
            .expect("verify committed version"));
        assert!(temporary
            .path()
            .join("platform.installations/managed_000001/versions/1.0.0")
            .is_dir());
    }

    #[test]
    fn managed_promotion_releases_the_final_tree_before_anchor_commit() {
        let _ = take_managed_promotion_steps();
        let temporary = tempfile::tempdir().expect("temporary root");
        let (manifest, payloads) =
            managed_version_payloads("managed_000004", "1.0.0", b"ordered promotion");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("materialize version");
        capability.seal_staged_directory().expect("seal version");
        capability
            .promote_staged_managed_installation()
            .expect("direct final managed promotion");

        assert_eq!(
            take_managed_promotion_steps(),
            [
                ManagedPromotionStep::DirectFinalTree,
                ManagedPromotionStep::PreparedAnchor,
                ManagedPromotionStep::FinalTreeReleased,
                ManagedPromotionStep::CommittedAnchor,
            ]
        );
    }

    #[test]
    fn managed_final_release_failure_preserves_prepared_recovery_before_drop() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (manifest, payloads) =
            managed_version_payloads("managed_000002", "1.0.0", b"rollback version");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("materialize version");
        capability.seal_staged_directory().expect("seal version");
        inject_test_failure(TestFailurePoint::FinalTreeReleaseDisposition);
        assert_eq!(
            capability
                .promote_staged_managed_installation()
                .expect_err("failed final release requires prepared recovery")
                .code(),
            McpPlatformErrorCode::RollbackIncomplete
        );

        let binding =
            test_root_binding(test_root_handle(temporary.path())).expect("anchor binding");
        let (_, _, anchor) = load_managed_anchor(&binding, false, true).expect("prepared anchor");
        assert!(
            !anchor.managed.contains_key("managed_000002"),
            "the failed final tree must not remain committed"
        );
        assert_eq!(
            anchor
                .cleanup_pending
                .expect("prepared recovery checkpoint")
                .phase,
            CleanupTransactionPhase::Prepared
        );
        assert!(
            !temporary
                .path()
                .join("platform.installations/managed_000002/versions/1.0.0")
                .exists(),
            "the rolled-back final tree must not survive as an incomplete committed version"
        );
        drop(capability);

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
        )
        .expect("restart capability");
        recovery
            .recover_committed_cleanup_pending()
            .expect("prepared evidence safely recovers after session end");
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn final_release_reaffirmation_failure_keeps_prepared_recovery_evidence_across_session_end() {
        for rollback_failure in [
            TestFailurePoint::PreparedRollbackPublish,
            TestFailurePoint::PreparedRollbackConfirmation,
        ] {
            let temporary = tempfile::tempdir().expect("temporary root");
            let (manifest, payloads) =
                managed_version_payloads("managed_000003", "1.0.0", b"rollback ambiguity");
            let capability = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
            )
            .expect("capability");
            capability
                .create_directory_staging(&manifest)
                .expect("directory staging");
            capability
                .materialize_verified_directory(
                    &payloads
                        .iter()
                        .map(|payload| DirectoryPayload {
                            relative_segments: payload.relative_segments.clone(),
                            bytes: &payload.bytes,
                        })
                        .collect::<Vec<_>>(),
                )
                .expect("materialize version");
            capability.seal_staged_directory().expect("seal version");
            inject_test_failure(TestFailurePoint::FinalTreeReleaseDisposition);
            inject_test_failure(rollback_failure);

            assert_eq!(
                capability
                    .promote_staged_managed_installation()
                    .expect_err("prepared recovery is required")
                    .code(),
                McpPlatformErrorCode::RollbackIncomplete
            );
            let final_path = temporary
                .path()
                .join("platform.installations/managed_000003/versions/1.0.0");
            assert!(
                !final_path.exists(),
                "no committed tree can retain a delete-on-close handle"
            );

            let binding =
                test_root_binding(test_root_handle(temporary.path())).expect("anchor binding");
            let (_, _, anchor) = load_managed_anchor(&binding, false, true).expect("anchor");
            assert_eq!(
                anchor
                    .cleanup_pending
                    .expect("prepared recovery checkpoint")
                    .phase,
                CleanupTransactionPhase::Prepared
            );
            assert!(anchor
                .managed
                .get("managed_000003")
                .expect("managed anchor")
                .versions
                .is_empty());
            drop(capability);

            let recovery = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
            )
            .expect("restart capability");
            recovery
                .recover_committed_cleanup_pending()
                .expect("prepared evidence safely recovers after session end");
            assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
            assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
        }
    }

    #[test]
    fn prepared_reaffirmation_never_restores_the_prior_anchor_after_ambiguous_readback() {
        for rollback_failure in [
            TestFailurePoint::PreparedRollbackPublish,
            TestFailurePoint::PreparedRollbackConfirmation,
        ] {
            let temporary = tempfile::tempdir().expect("temporary root");
            let (first_manifest, first_payloads) =
                managed_version_payloads("managed_000003", "1.0.0", b"first version");
            let first = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("first binding"),
            )
            .expect("first capability");
            first
                .create_directory_staging(&first_manifest)
                .expect("first staging");
            first
                .materialize_verified_directory(
                    &first_payloads
                        .iter()
                        .map(|payload| DirectoryPayload {
                            relative_segments: payload.relative_segments.clone(),
                            bytes: &payload.bytes,
                        })
                        .collect::<Vec<_>>(),
                )
                .expect("first materialize");
            first.seal_staged_directory().expect("first seal");
            first
                .promote_staged_managed_installation()
                .expect("first promotion");
            drop(first);

            let (second_manifest, second_payloads) =
                managed_version_payloads("managed_000003", "2.0.0", b"second version");
            let second = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("second binding"),
            )
            .expect("second capability");
            second
                .create_directory_staging(&second_manifest)
                .expect("second staging");
            second
                .materialize_verified_directory(
                    &second_payloads
                        .iter()
                        .map(|payload| DirectoryPayload {
                            relative_segments: payload.relative_segments.clone(),
                            bytes: &payload.bytes,
                        })
                        .collect::<Vec<_>>(),
                )
                .expect("second materialize");
            second.seal_staged_directory().expect("second seal");
            inject_test_failure(TestFailurePoint::FinalTreeReleaseDisposition);
            inject_test_failure(rollback_failure);
            assert_eq!(
                second
                    .promote_staged_managed_installation()
                    .expect_err("ambiguous prepared reaffirmation requires recovery")
                    .code(),
                McpPlatformErrorCode::RollbackIncomplete
            );

            let binding = test_root_binding(test_root_handle(temporary.path()))
                .expect("prepared anchor binding");
            let (_, _, anchor) =
                load_managed_anchor(&binding, false, true).expect("prepared anchor survives");
            let managed = anchor
                .managed
                .get("managed_000003")
                .expect("prior committed state remains anchored");
            assert!(managed.versions.contains_key("1.0.0"));
            assert!(!managed.versions.contains_key("2.0.0"));
            assert_eq!(
                anchor
                    .cleanup_pending
                    .expect("prepared recovery was not overwritten by the prior anchor")
                    .phase,
                CleanupTransactionPhase::Prepared
            );
            drop(second);

            let recovery = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
            )
            .expect("restart capability");
            recovery
                .recover_committed_cleanup_pending()
                .expect("restart recovers the authenticated prepared checkpoint");
            assert!(recovery
                .managed_installation_version_is_verified("managed_000003", "1.0.0")
                .expect("prior committed version remains usable"));
            assert!(!recovery
                .managed_installation_version_is_verified("managed_000003", "2.0.0")
                .expect("uncommitted version was not adopted"));
            drop(recovery);

            let retry = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("retry binding"),
            )
            .expect("retry capability");
            retry
                .create_directory_staging(&second_manifest)
                .expect("prepared recovery did not permanently lock the root");
            retry
                .materialize_verified_directory(
                    &second_payloads
                        .iter()
                        .map(|payload| DirectoryPayload {
                            relative_segments: payload.relative_segments.clone(),
                            bytes: &payload.bytes,
                        })
                        .collect::<Vec<_>>(),
                )
                .expect("retry materialize");
            retry.seal_staged_directory().expect("retry seal");
            retry
                .promote_staged_managed_installation()
                .expect("retry promotion");
        }
    }

    #[test]
    fn managed_installation_update_appends_a_new_version_without_replacing_the_old_one() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let first_bytes = b"first version";
        let (first_manifest, first_payloads) =
            managed_version_payloads("managed_000001", "1.0.0", first_bytes);
        let first = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        first
            .create_directory_staging(&first_manifest)
            .expect("first staging");
        first
            .materialize_verified_directory(
                &first_payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("first materialization");
        first.seal_staged_directory().expect("first seal");
        first
            .promote_staged_managed_installation()
            .expect("first promotion");
        let (_, _, anchor) = load_managed_anchor(
            &test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
            false,
            true,
        )
        .expect("normal cleanup leaves an authenticated anchor");
        assert!(
            anchor.cleanup_pending.is_none(),
            "successful cleanup must not leave a stale recovery checkpoint"
        );
        drop(first);

        let second_bytes = b"second version";
        let (second_manifest, second_payloads) =
            managed_version_payloads("managed_000001", "2.0.0", second_bytes);
        let second = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        second
            .create_directory_staging(&second_manifest)
            .expect("second staging");
        second
            .materialize_verified_directory(
                &second_payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("second materialization");
        second.seal_staged_directory().expect("second seal");
        inject_test_failure(TestFailurePoint::CleanupMarkerDisposition);
        assert_eq!(
            second
                .promote_staged_managed_installation()
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::RollbackIncomplete
        );
        let (_, _, anchor) = load_managed_anchor(
            &test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
            false,
            true,
        )
        .expect("committed cleanup checkpoint");
        assert_eq!(
            anchor
                .cleanup_pending
                .expect("pending after failed cleanup")
                .phase,
            CleanupTransactionPhase::CommittedMarkerDeleteAuthorized
        );
        second.abandon_uncommitted_for_crash_testing();
        drop(second);

        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());
        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path()))
                .expect("restart preflight binding"),
        )
        .expect("restart capability");
        recovery
            .recover_committed_cleanup_pending()
            .expect("authenticated committed cleanup retry");
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
        let (_, _, anchor) = load_managed_anchor(
            &test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
            false,
            true,
        )
        .expect("recovered anchor");
        assert!(anchor.cleanup_pending.is_none());
        drop(recovery);

        assert_eq!(
            std::fs::read(
                temporary
                    .path()
                    .join("platform.installations/managed_000001/versions/1.0.0/server.exe"),
            )
            .expect("first committed version"),
            first_bytes
        );
        assert_eq!(
            std::fs::read(
                temporary
                    .path()
                    .join("platform.installations/managed_000001/versions/2.0.0/server.exe"),
            )
            .expect("second committed version"),
            second_bytes
        );

        let conflict = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        conflict
            .create_directory_staging(&second_manifest)
            .expect("conflict staging");
        conflict
            .materialize_verified_directory(
                &second_payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("conflict materialization");
        conflict.seal_staged_directory().expect("conflict seal");
        assert!(conflict.promote_staged_managed_installation().is_err());
        conflict.discard_uncommitted().expect("conflict cleanup");
        assert_eq!(
            std::fs::read(
                temporary
                    .path()
                    .join("platform.installations/managed_000001/versions/2.0.0/server.exe"),
            )
            .expect("existing version preserved"),
            second_bytes
        );
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn committed_cleanup_crash_after_residue_removal_clears_checkpoint_on_restart() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (manifest, payloads) =
            managed_version_payloads("managed_000002", "1.0.0", b"crash-safe version");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .expect("materialize version");
        capability.seal_staged_directory().expect("seal version");
        inject_test_failure(TestFailurePoint::CleanupCheckpoint);
        assert!(capability.promote_staged_managed_installation().is_err());
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
        drop(capability);

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
        )
        .expect("restart capability");
        recovery
            .recover_committed_cleanup_pending()
            .expect("checkpoint completion after crash");
        let (_, _, anchor) = load_managed_anchor(
            &test_root_binding(test_root_handle(temporary.path())).expect("anchor binding"),
            false,
            true,
        )
        .expect("anchor");
        assert!(anchor.cleanup_pending.is_none());
        assert_eq!(
            std::fs::read(
                temporary
                    .path()
                    .join("platform.installations/managed_000002/versions/1.0.0/server.exe"),
            )
            .expect("committed version"),
            b"crash-safe version"
        );
    }

    #[test]
    fn committed_staging_partial_delete_resumes_only_the_signed_residue() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (first_manifest, first_payloads) =
            managed_version_payloads("managed_000009", "1.0.0", b"first version");
        let first = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("first binding"),
        )
        .expect("first capability");
        first.create_directory_staging(&first_manifest).unwrap();
        first
            .materialize_verified_directory(
                &first_payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        first.seal_staged_directory().unwrap();
        first.promote_staged_managed_installation().unwrap();
        drop(first);

        let (manifest, payloads) =
            managed_version_payloads("managed_000009", "2.0.0", b"second version");
        let update = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("update binding"),
        )
        .expect("update capability");
        update.create_directory_staging(&manifest).unwrap();
        update
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        update.seal_staged_directory().unwrap();
        inject_test_failure(TestFailurePoint::CleanupDisposition);
        assert_eq!(
            update
                .promote_staged_managed_installation()
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::RollbackIncomplete
        );
        update.abandon_uncommitted_for_crash_testing();
        drop(update);

        let partial = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("partial binding"),
        )
        .expect("partial recovery capability");
        inject_test_failure(TestFailurePoint::PartialStagingTreeDisposition);
        assert!(partial.recover_committed_cleanup_pending().is_err());
        assert!(
            temporary
                .path()
                .join(format!("{STAGING_DIRECTORY}/managed_000009"))
                .is_dir(),
            "the injected failure occurs after a controlled child was removed"
        );
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());
        drop(partial);

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("retry binding"),
        )
        .expect("retry recovery capability");
        recovery
            .recover_committed_cleanup_pending()
            .expect("signed partial staging residue resumes after restart");
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn legacy_cleanup_rejects_replayed_marker_and_clears_only_absent_residue() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (first_manifest, first_payloads) =
            managed_version_payloads("managed_000011", "1.0.0", b"first version");
        let first = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("first binding"),
        )
        .expect("first capability");
        first.create_directory_staging(&first_manifest).unwrap();
        first
            .materialize_verified_directory(
                &first_payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        first.seal_staged_directory().unwrap();
        first.promote_staged_managed_installation().unwrap();
        drop(first);

        let (manifest, payloads) =
            managed_version_payloads("managed_000011", "2.0.0", b"second version");
        let update = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("update binding"),
        )
        .expect("update capability");
        update.create_directory_staging(&manifest).unwrap();
        update
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        update.seal_staged_directory().unwrap();
        inject_test_failure(TestFailurePoint::CleanupDisposition);
        assert_eq!(
            update
                .promote_staged_managed_installation()
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::RollbackIncomplete
        );
        let binding =
            test_root_binding(test_root_handle(temporary.path())).expect("legacy binding");
        let (signer, checkpoint, mut anchor) =
            load_managed_anchor(&binding, false, true).expect("current cleanup checkpoint");
        let pending = anchor.cleanup_pending.as_mut().expect("pending cleanup");
        pending.marker_identity = None;
        pending.prepared_version_identity = None;
        pending.staging_identity = None;
        pending.staging_snapshot = None;
        anchor.format_version = 5;
        publish_managed_anchor(signer.as_ref(), checkpoint.as_ref(), &anchor)
            .expect("signed legacy cleanup checkpoint");
        let marker_path = temporary.path().join(STAGING_SESSION_MARKER);
        let marker_bytes = std::fs::read(&marker_path).expect("original marker bytes");
        update.abandon_uncommitted_for_crash_testing();
        drop(update);

        let staging = temporary.path().join(STAGING_DIRECTORY);
        std::fs::remove_file(&marker_path).expect("remove original marker");

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("missing marker binding"),
        )
        .expect("missing marker recovery capability");
        assert!(recovery.recover_committed_cleanup_pending().is_err());
        assert!(staging.is_dir(), "legacy staging residue is never disposed");
        assert!(
            !marker_path.exists(),
            "legacy recovery never recreates a marker"
        );
        let (_, _, anchor) = load_managed_anchor(
            &test_root_binding(test_root_handle(temporary.path())).expect("missing marker anchor"),
            false,
            true,
        )
        .expect("checkpoint remains without a legacy marker");
        assert_eq!(anchor.format_version, 5);
        assert!(anchor.cleanup_pending.is_some());
        drop(recovery);

        std::fs::remove_dir_all(&staging).expect("remove original staging residue");
        std::fs::create_dir(&staging).expect("replace staging with attacker directory");
        let attacker_file = staging.join("attacker-controlled");
        std::fs::write(&attacker_file, b"attacker-controlled residue")
            .expect("write attacker-controlled staging residue");

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path()))
                .expect("replaced staging missing marker binding"),
        )
        .expect("replaced staging missing marker recovery capability");
        assert!(recovery.recover_committed_cleanup_pending().is_err());
        assert!(
            staging.is_dir(),
            "replaced legacy staging is never disposed"
        );
        assert_eq!(
            std::fs::read(&attacker_file).expect("attacker-controlled staging residue"),
            b"attacker-controlled residue"
        );
        assert!(
            !marker_path.exists(),
            "missing marker prevents disposition of replaced staging"
        );
        let (_, _, anchor) = load_managed_anchor(
            &test_root_binding(test_root_handle(temporary.path()))
                .expect("replaced staging missing marker anchor"),
            false,
            true,
        )
        .expect("checkpoint remains after replaced staging recovery");
        assert_eq!(anchor.format_version, 5);
        assert!(anchor.cleanup_pending.is_some());
        drop(recovery);

        std::fs::remove_dir_all(&staging).expect("remove attacker-controlled staging residue");
        std::fs::write(&marker_path, &marker_bytes).expect("replay original marker bytes");

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
        )
        .expect("restart capability");
        assert!(recovery.recover_committed_cleanup_pending().is_err());
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker_bytes);
        let (_, _, anchor) = load_managed_anchor(
            &test_root_binding(test_root_handle(temporary.path())).expect("replayed binding"),
            false,
            true,
        )
        .expect("checkpoint remains after replay");
        assert!(anchor.cleanup_pending.is_some());
        drop(recovery);

        std::fs::remove_file(&marker_path).expect("remove replayed marker");
        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("cleanup binding"),
        )
        .expect("cleanup capability");
        recovery
            .recover_committed_cleanup_pending()
            .expect("absent legacy residue clears checkpoint");
        let (_, _, anchor) = load_managed_anchor(
            &test_root_binding(test_root_handle(temporary.path())).expect("anchor binding"),
            false,
            false,
        )
        .expect("cleared anchor");
        assert!(anchor.cleanup_pending.is_none());
        recovery
            .create_directory_staging(&manifest)
            .expect("cleared checkpoint no longer blocks a new installation");
    }

    #[test]
    fn committed_staging_revalidation_failure_stops_before_deletion() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (first_manifest, first_payloads) =
            managed_version_payloads("managed_000012", "1.0.0", b"first version");
        let first = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("first binding"),
        )
        .expect("first capability");
        first.create_directory_staging(&first_manifest).unwrap();
        first
            .materialize_verified_directory(
                &first_payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        first.seal_staged_directory().unwrap();
        first.promote_staged_managed_installation().unwrap();
        drop(first);

        let (manifest, payloads) =
            managed_version_payloads("managed_000012", "2.0.0", b"second version");
        let update = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("update binding"),
        )
        .expect("update capability");
        update.create_directory_staging(&manifest).unwrap();
        update
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        update.seal_staged_directory().unwrap();
        inject_test_failure(TestFailurePoint::CleanupDisposition);
        assert_eq!(
            update
                .promote_staged_managed_installation()
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::RollbackIncomplete
        );
        update.abandon_uncommitted_for_crash_testing();
        drop(update);

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
        )
        .expect("restart capability");
        inject_test_failure(TestFailurePoint::CleanupStagingBindingRevalidation);
        assert!(recovery.recover_committed_cleanup_pending().is_err());
        assert!(temporary.path().join(STAGING_DIRECTORY).is_dir());
        assert!(temporary
            .path()
            .join(format!("{STAGING_DIRECTORY}/managed_000012/versions"))
            .is_dir());
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());
    }

    #[test]
    fn committed_staging_root_revalidation_failure_after_disposition_preserves_remaining_residue() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (first_manifest, first_payloads) =
            managed_version_payloads("managed_000013", "1.0.0", b"first version");
        let first = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("first binding"),
        )
        .expect("first capability");
        first.create_directory_staging(&first_manifest).unwrap();
        first
            .materialize_verified_directory(
                &first_payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        first.seal_staged_directory().unwrap();
        first.promote_staged_managed_installation().unwrap();
        drop(first);

        let (manifest, payloads) =
            managed_version_payloads("managed_000013", "2.0.0", b"second version");
        let update = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("update binding"),
        )
        .expect("update capability");
        update.create_directory_staging(&manifest).unwrap();
        update
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        update.seal_staged_directory().unwrap();
        inject_test_failure(TestFailurePoint::CleanupDisposition);
        assert_eq!(
            update
                .promote_staged_managed_installation()
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::RollbackIncomplete
        );
        update.abandon_uncommitted_for_crash_testing();
        drop(update);

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
        )
        .expect("restart capability");
        inject_test_failure(TestFailurePoint::RootRevalidationAfterStagingDisposition);
        assert!(recovery.recover_committed_cleanup_pending().is_err());

        let managed = temporary
            .path()
            .join(format!("{STAGING_DIRECTORY}/managed_000013"));
        assert!(
            !managed.join("versions").exists(),
            "the test reaches a real staging disposition before the next root revalidation"
        );
        assert!(managed.is_dir(), "the current residue is not disposed");
        assert!(temporary.path().join(STAGING_DIRECTORY).is_dir());
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());
        let (_, _, anchor) = load_managed_anchor(
            &test_root_binding(test_root_handle(temporary.path()))
                .expect("failed recovery binding"),
            false,
            true,
        )
        .expect("checkpoint remains after root revalidation failure");
        assert_eq!(
            anchor.cleanup_pending.expect("pending cleanup").phase,
            CleanupTransactionPhase::CommittedStagingDeleteAuthorized
        );
        drop(recovery);

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("retry binding"),
        )
        .expect("retry recovery capability");
        recovery
            .recover_committed_cleanup_pending()
            .expect("remaining signed residue resumes safely");
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn committed_staging_partial_delete_never_deletes_a_replaced_descendant() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (first_manifest, first_payloads) =
            managed_version_payloads("managed_000010", "1.0.0", b"first version");
        let first = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("first binding"),
        )
        .expect("first capability");
        first.create_directory_staging(&first_manifest).unwrap();
        first
            .materialize_verified_directory(
                &first_payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        first.seal_staged_directory().unwrap();
        first.promote_staged_managed_installation().unwrap();
        drop(first);

        let (manifest, payloads) =
            managed_version_payloads("managed_000010", "2.0.0", b"second version");
        let update = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("update binding"),
        )
        .expect("update capability");
        update.create_directory_staging(&manifest).unwrap();
        update
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        update.seal_staged_directory().unwrap();
        inject_test_failure(TestFailurePoint::CleanupDisposition);
        assert_eq!(
            update
                .promote_staged_managed_installation()
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::RollbackIncomplete
        );
        update.abandon_uncommitted_for_crash_testing();
        drop(update);

        let partial = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("partial binding"),
        )
        .expect("partial recovery capability");
        inject_test_failure(TestFailurePoint::PartialStagingTreeDisposition);
        assert!(partial.recover_committed_cleanup_pending().is_err());
        drop(partial);

        let managed = temporary
            .path()
            .join(format!("{STAGING_DIRECTORY}/managed_000010"));
        std::fs::remove_dir(&managed).expect("removed empty original managed directory");
        std::fs::create_dir(&managed).expect("attacker replacement directory");
        std::fs::write(managed.join("attacker.txt"), b"preserve me").unwrap();

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("retry binding"),
        )
        .expect("retry recovery capability");
        assert!(recovery.recover_committed_cleanup_pending().is_err());
        assert_eq!(
            std::fs::read(managed.join("attacker.txt")).unwrap(),
            b"preserve me"
        );
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());
    }

    #[test]
    fn committed_cleanup_rejects_same_content_marker_and_target_replacements() {
        for replace_marker in [true, false] {
            let temporary = tempfile::tempdir().expect("temporary root");
            let (manifest, payloads) =
                managed_version_payloads("managed_000008", "1.0.0", b"committed identity");
            let capability = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
            )
            .expect("capability");
            capability
                .create_directory_staging(&manifest)
                .expect("directory staging");
            capability
                .materialize_verified_directory(
                    &payloads
                        .iter()
                        .map(|payload| DirectoryPayload {
                            relative_segments: payload.relative_segments.clone(),
                            bytes: &payload.bytes,
                        })
                        .collect::<Vec<_>>(),
                )
                .expect("materialize version");
            capability.seal_staged_directory().expect("seal version");
            inject_test_failure(TestFailurePoint::CleanupDisposition);
            assert_eq!(
                capability
                    .promote_staged_managed_installation()
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::RollbackIncomplete
            );
            capability.abandon_uncommitted_for_crash_testing();
            drop(capability);

            let marker_path = temporary.path().join(STAGING_SESSION_MARKER);
            let target = temporary
                .path()
                .join("platform.installations/managed_000008/versions/1.0.0");
            if replace_marker {
                let marker = std::fs::read(&marker_path).expect("original marker bytes");
                std::fs::remove_file(&marker_path).expect("replace marker identity");
                std::fs::write(&marker_path, marker).expect("copy marker bytes");
            } else {
                std::fs::remove_dir_all(&target).expect("replace committed target identity");
                copy_managed_version_tree(temporary.path(), "managed_000008", "1.0.0", &payloads);
            }

            let recovery = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
            )
            .expect("restart capability");
            assert!(
                recovery.recover_committed_cleanup_pending().is_err(),
                "same-content committed replacements must not authorize cleanup"
            );
            assert!(marker_path.exists(), "failed recovery preserves residue");
            assert!(
                target.exists(),
                "failed recovery preserves committed target"
            );
            let (_, _, anchor) = load_managed_anchor(
                &test_root_binding(test_root_handle(temporary.path())).expect("anchor binding"),
                false,
                true,
            )
            .expect("checkpoint remains");
            assert_eq!(
                anchor
                    .cleanup_pending
                    .expect("pending committed cleanup")
                    .phase,
                CleanupTransactionPhase::CommittedStagingDeleteAuthorized
            );
        }
    }

    #[test]
    fn prepared_rename_crash_aborts_only_the_authenticated_uncommitted_staging() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (manifest, payloads) =
            managed_version_payloads("managed_000003", "1.0.0", b"prepared version");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        capability.create_directory_staging(&manifest).unwrap();
        capability
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        capability.seal_staged_directory().unwrap();
        inject_test_failure(TestFailurePoint::PreparedRename);
        assert!(capability.promote_staged_managed_installation().is_err());
        capability.abandon_uncommitted_for_crash_testing();
        drop(capability);

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
        )
        .expect("restart capability");
        inject_test_failure(TestFailurePoint::CleanupMarkerDisposition);
        assert!(
            recovery.recover_committed_cleanup_pending().is_err(),
            "marker deletion failure leaves an authenticated prepared rollback checkpoint"
        );
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(temporary.path().join(STAGING_SESSION_MARKER).exists());
        drop(recovery);

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
        )
        .expect("restart capability");
        recovery
            .recover_committed_cleanup_pending()
            .expect("authenticated prepared crash is safely aborted");
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
        drop(recovery);

        let retry = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("retry binding"),
        )
        .expect("retry capability");
        retry.create_directory_staging(&manifest).unwrap();
        retry
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        retry.seal_staged_directory().unwrap();
        retry
            .promote_staged_managed_installation()
            .expect("a safely aborted prepared transaction may retry");
    }

    #[test]
    fn prepared_rename_after_rename_rejects_a_missing_transaction_marker() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (manifest, payloads) =
            managed_version_payloads("managed_000006", "1.0.0", b"renamed version");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        capability.create_directory_staging(&manifest).unwrap();
        capability
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        capability.seal_staged_directory().unwrap();
        inject_test_failure(TestFailurePoint::RenamedBeforeCommit);
        assert!(capability.promote_staged_managed_installation().is_err());
        capability.abandon_uncommitted_for_crash_testing();
        drop(capability);

        std::fs::remove_file(temporary.path().join(STAGING_SESSION_MARKER))
            .expect("delete transaction marker after rename");
        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
        )
        .expect("restart capability");
        assert!(
            recovery.recover_committed_cleanup_pending().is_err(),
            "a renamed tree without its original marker is never commit proof"
        );
    }

    #[test]
    fn prepared_rename_rejects_competing_same_content_tree_and_copied_marker() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let (manifest, payloads) =
            managed_version_payloads("managed_000007", "1.0.0", b"competing version");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        capability.create_directory_staging(&manifest).unwrap();
        capability
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        capability.seal_staged_directory().unwrap();
        inject_test_failure(TestFailurePoint::PreparedRename);
        assert!(capability.promote_staged_managed_installation().is_err());
        capability.abandon_uncommitted_for_crash_testing();
        drop(capability);

        let marker_path = temporary.path().join(STAGING_SESSION_MARKER);
        let copied_marker = std::fs::read(&marker_path).expect("original marker bytes");
        std::fs::remove_file(&marker_path).expect("replace marker identity");
        std::fs::write(&marker_path, copied_marker).expect("copy marker bytes");
        copy_managed_version_tree(temporary.path(), "managed_000007", "1.0.0", &payloads);

        let recovery = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
        )
        .expect("restart capability");
        assert!(
            recovery.recover_committed_cleanup_pending().is_err(),
            "same-content trees and copied marker bytes cannot prove this transaction renamed"
        );
    }

    #[test]
    fn prepared_rename_crash_rejects_tampered_marker_or_staging() {
        for tamper_marker in [false, true] {
            let temporary = tempfile::tempdir().expect("temporary root");
            let (manifest, payloads) =
                managed_version_payloads("managed_000004", "1.0.0", b"prepared version");
            let capability = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
            )
            .expect("capability");
            capability.create_directory_staging(&manifest).unwrap();
            capability
                .materialize_verified_directory(
                    &payloads
                        .iter()
                        .map(|payload| DirectoryPayload {
                            relative_segments: payload.relative_segments.clone(),
                            bytes: &payload.bytes,
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            capability.seal_staged_directory().unwrap();
            inject_test_failure(TestFailurePoint::PreparedRename);
            assert!(capability.promote_staged_managed_installation().is_err());
            capability.abandon_uncommitted_for_crash_testing();
            drop(capability);

            let tampered = if tamper_marker {
                temporary.path().join(STAGING_SESSION_MARKER)
            } else {
                temporary.path().join(format!(
                    "{STAGING_DIRECTORY}/managed_000004/versions/1.0.0/server.exe"
                ))
            };
            std::fs::write(tampered, b"attacker").expect("tamper authenticated residue");
            let recovery = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("restart binding"),
            )
            .expect("restart capability");
            assert_eq!(
                recovery
                    .recover_committed_cleanup_pending()
                    .expect_err("tampered prepared residue must never be adopted")
                    .code(),
                McpPlatformErrorCode::IntegrityError
            );
            assert!(recovery.create_staging().is_err());
        }
    }

    #[test]
    fn root_lineage_rejects_recreated_root_and_allows_a_true_fresh_root() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("managed-root");
        let moved = temporary.path().join("previous-managed-root");
        std::fs::create_dir(&root).expect("managed root");
        let (manifest, payloads) =
            managed_version_payloads("managed_000005", "1.0.0", b"lineage version");
        let original = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(&root)).expect("original binding"),
        )
        .expect("original capability");
        original.create_directory_staging(&manifest).unwrap();
        original
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        original.seal_staged_directory().unwrap();
        original.promote_staged_managed_installation().unwrap();
        drop(original);
        std::fs::rename(&root, &moved).expect("remove original root from configured path");
        std::fs::create_dir(&root).expect("recreate configured path");
        assert!(
            ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(&root)).expect("replacement binding"),
            )
            .is_err(),
            "a recreated root at the same configured location must not mint a new signer"
        );

        let fresh = tempfile::tempdir().expect("fresh root");
        let fresh_capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(fresh.path())).expect("fresh binding"),
        )
        .expect("fresh root has no lineage");
        fresh_capability
            .create_directory_staging(&manifest)
            .unwrap();
        fresh_capability
            .materialize_verified_directory(
                &payloads
                    .iter()
                    .map(|payload| DirectoryPayload {
                        relative_segments: payload.relative_segments.clone(),
                        bytes: &payload.bytes,
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        fresh_capability.seal_staged_directory().unwrap();
        fresh_capability
            .promote_staged_managed_installation()
            .expect("a root with no lineage and no prior managed evidence is usable");
    }

    #[test]
    fn managed_lineage_binding_distinguishes_volume_guid_when_serials_collide() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let mut first =
            test_root_binding(test_root_handle(temporary.path())).expect("first root binding");
        let mut replacement = test_root_binding(test_root_handle(temporary.path()))
            .expect("replacement root binding");
        first.set_volume_identity_for_testing("\\\\?\\Volume{first-guid}\\\u{0}00000007");
        replacement.set_volume_identity_for_testing("\\\\?\\Volume{second-guid}\\\u{0}00000007");

        assert_eq!(
            first.managed_installation_root_identity(),
            replacement.managed_installation_root_identity(),
            "the test models a serial/file-index collision"
        );
        assert_ne!(
            first.managed_installation_lineage_binding(),
            replacement.managed_installation_lineage_binding(),
            "the volume GUID remains part of the lineage account binding"
        );
    }

    #[test]
    fn managed_activation_replaces_only_a_sealed_existing_descriptor_and_rolls_back() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let first_descriptor = br#"{"version":"1.0.0"}"#;
        let mut tree = Sha256::new();
        tree.update(b".goose-runtime-descriptor.json");
        tree.update([0]);
        tree.update((first_descriptor.len() as u64).to_le_bytes());
        tree.update(hex_digest(first_descriptor).as_bytes());
        let commitment = serde_json::to_vec(&serde_json::json!({
            "descriptor_sha256": hex_digest(first_descriptor),
            "tree_sha256": hex_digest(&tree.finalize()),
        }))
        .unwrap();
        let first_manifest = directory_manifest(vec![
            (
                vec![
                    "managed_000001",
                    "versions",
                    "1.0.0",
                    ".goose-runtime-descriptor.json",
                ],
                first_descriptor,
            ),
            (
                vec![
                    "managed_000001",
                    "versions",
                    "1.0.0",
                    COMMITTED_VERSION_EVIDENCE,
                ],
                &commitment,
            ),
        ]);
        let first = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        first.create_directory_staging(&first_manifest).unwrap();
        first
            .materialize_verified_directory(&[
                DirectoryPayload {
                    relative_segments: vec![
                        "managed_000001".to_owned(),
                        "versions".to_owned(),
                        "1.0.0".to_owned(),
                        ".goose-runtime-descriptor.json".to_owned(),
                    ],
                    bytes: first_descriptor,
                },
                DirectoryPayload {
                    relative_segments: vec![
                        "managed_000001".to_owned(),
                        "versions".to_owned(),
                        "1.0.0".to_owned(),
                        COMMITTED_VERSION_EVIDENCE.to_owned(),
                    ],
                    bytes: &commitment,
                },
            ])
            .unwrap();
        first.seal_staged_directory().unwrap();
        first.promote_staged_managed_installation().unwrap();
        drop(first);

        let activate_first = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        activate_first.create_staging().unwrap();
        activate_first
            .write_verified_payload(first_descriptor, &hex_digest(first_descriptor))
            .unwrap();
        activate_first.verify_regular_payload().unwrap();
        activate_first
            .activate_verified_managed_installation(
                "managed_000001",
                "1.0.0",
                first_descriptor,
                &hex_digest(first_descriptor),
            )
            .unwrap();
        drop(activate_first);

        assert_eq!(
            std::fs::read(
                temporary
                    .path()
                    .join("platform.installations/managed_000001/active.json"),
            )
            .unwrap(),
            first_descriptor
        );

        let invalid = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        invalid.create_staging().unwrap();
        invalid
            .write_verified_payload(b"different", &hex_digest(b"different"))
            .unwrap();
        invalid.verify_regular_payload().unwrap();
        assert!(invalid
            .activate_verified_managed_installation(
                "managed_000001",
                "1.0.0",
                b"different",
                &hex_digest(b"different"),
            )
            .is_err());
        assert_eq!(
            std::fs::read(
                temporary
                    .path()
                    .join("platform.installations/managed_000001/active.json"),
            )
            .unwrap(),
            first_descriptor
        );

        drop(invalid);
        let tampered_descriptor = br#"{"version":"1.0.0","projection":"attacker"}"#;
        std::fs::write(
            temporary
                .path()
                .join("platform.installations/managed_000001/versions/1.0.0/.goose-runtime-descriptor.json"),
            tampered_descriptor,
        )
        .unwrap();
        let tampered = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        tampered.create_staging().unwrap();
        tampered
            .write_verified_payload(tampered_descriptor, &hex_digest(tampered_descriptor))
            .unwrap();
        tampered.verify_regular_payload().unwrap();
        assert!(tampered
            .activate_verified_managed_installation(
                "managed_000001",
                "1.0.0",
                tampered_descriptor,
                &hex_digest(tampered_descriptor),
            )
            .is_err());
        assert_eq!(
            std::fs::read(
                temporary
                    .path()
                    .join("platform.installations/managed_000001/active.json"),
            )
            .unwrap(),
            first_descriptor
        );
        drop(tampered);
        std::fs::write(
            temporary
                .path()
                .join("platform.installations/managed_000001/versions/1.0.0/.goose-runtime-descriptor.json"),
            first_descriptor,
        )
        .unwrap();
        let active_path = temporary
            .path()
            .join("platform.installations/managed_000001/active.json");
        std::fs::write(&active_path, br#"{"version":"1.0.0","tampered":true}"#)
            .expect("tamper active pointer");
        let pointer_tamper = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        pointer_tamper.create_staging().unwrap();
        pointer_tamper
            .write_verified_payload(first_descriptor, &hex_digest(first_descriptor))
            .unwrap();
        pointer_tamper.verify_regular_payload().unwrap();
        assert!(
            pointer_tamper
                .activate_verified_managed_installation(
                    "managed_000001",
                    "1.0.0",
                    first_descriptor,
                    &hex_digest(first_descriptor),
                )
                .is_err(),
            "activation must not overwrite a tampered active pointer"
        );
        drop(pointer_tamper);
        std::fs::write(&active_path, first_descriptor).expect("restore authenticated pointer");
        let clear = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        std::fs::remove_file(&active_path).expect("simulate external pointer deletion");
        assert!(
            clear
                .clear_managed_installation_activation("managed_000001")
                .is_err(),
            "a missing pointer without a prepared clear transition is tampering"
        );
        std::fs::write(&active_path, first_descriptor).expect("restore authenticated pointer");
        inject_test_failure(TestFailurePoint::ClearActivationFinalize);
        assert!(
            clear
                .clear_managed_installation_activation("managed_000001")
                .is_err(),
            "crash after pointer deletion leaves an authenticated transition"
        );
        assert!(!active_path.exists());
        assert!(
            clear
                .read_verified_managed_installation_activation("managed_000001")
                .is_err(),
            "readers fail closed while a clear transition is incomplete"
        );
        std::fs::write(&active_path, br#"{"version":"1.0.0","tampered":true}"#)
            .expect("tamper pending clear pointer");
        assert!(
            clear
                .clear_managed_installation_activation("managed_000001")
                .is_err(),
            "a pending clear must not accept a substituted pointer"
        );
        assert!(
            clear
                .activate_verified_managed_installation(
                    "managed_000001",
                    "1.0.0",
                    first_descriptor,
                    &hex_digest(first_descriptor),
                )
                .is_err(),
            "only the authenticated clear operation may complete a pending clear"
        );
        assert!(
            clear.create_directory_staging(&first_manifest).is_err(),
            "a pending active clear blocks every new installation transaction"
        );
        std::fs::write(&active_path, first_descriptor)
            .expect("restore authenticated pending pointer");
        clear
            .clear_managed_installation_activation("managed_000001")
            .expect("recover authenticated clear transition");
        assert!(!temporary
            .path()
            .join("platform.installations/managed_000001/active.json")
            .exists());
    }

    #[test]
    fn directory_manifest_rejects_paths_ads_duplicates_and_bounds_before_staging() {
        let bytes = b"payload";
        for manifest in [
            directory_manifest(vec![(vec!["bad:name"], bytes)]),
            directory_manifest(vec![(vec!["one"], bytes), (vec!["one"], bytes)]),
            directory_manifest(vec![(
                (0..MAX_DIRECTORY_DEPTH + 1).map(|_| "deep").collect(),
                bytes,
            )]),
        ] {
            let temporary = tempfile::tempdir().expect("temporary root");
            let capability = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
            )
            .expect("capability");
            assert!(capability.create_directory_staging(&manifest).is_err());
            assert!(std::fs::read_dir(temporary.path())
                .expect("root entries")
                .next()
                .is_none());
        }
    }

    #[test]
    fn directory_payload_preflight_rejects_invalid_input_before_materialization() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let bytes = b"cache artifact";
        let digest = hex_digest(bytes);
        let manifest = directory_manifest(vec![(vec![digest.as_str()], bytes)]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");

        assert!(capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec!["bad:name".to_owned()],
                bytes,
            }])
            .is_err());
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn directory_payload_preflight_bounds_depth_and_duplicate_paths_without_cloning_keys() {
        let bytes = b"payload";
        let duplicate = [
            DirectoryPayload {
                relative_segments: vec!["same".to_owned()],
                bytes,
            },
            DirectoryPayload {
                relative_segments: vec!["same".to_owned()],
                bytes,
            },
        ];
        let too_deep = [DirectoryPayload {
            relative_segments: (0..=MAX_DIRECTORY_DEPTH)
                .map(|_| "deep".to_owned())
                .collect(),
            bytes,
        }];

        assert!(validate_directory_payloads(&duplicate, 2).is_err());
        assert!(validate_directory_payloads(&too_deep, 1).is_err());
    }

    #[test]
    fn foreign_destination_is_never_replaced_and_promotion_failure_keeps_a_residual() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let bytes = b"cache artifact";
        let digest = hex_digest(bytes);
        let manifest = directory_manifest(vec![(vec![digest.as_str()], bytes)]);
        std::fs::create_dir(temporary.path().join("platform.cache")).expect("foreign cache");
        std::fs::write(
            temporary.path().join("platform.cache").join("foreign"),
            b"foreign",
        )
        .expect("foreign bytes");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec![digest.clone()],
                bytes,
            }])
            .expect("materialize");
        capability.seal_staged_directory().expect("seal");
        assert!(capability
            .promote_staged_directory(RuntimeStorageCommittedLeaf::Cache)
            .is_err());
        assert_eq!(
            std::fs::read(temporary.path().join("platform.cache").join("foreign"))
                .expect("foreign bytes"),
            b"foreign"
        );
        assert!(temporary.path().join(STAGING_DIRECTORY).is_dir());
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());
        capability
            .discard_uncommitted()
            .expect("residual cleanup stays session-owned");
    }

    #[test]
    fn staged_tree_refuses_foreign_writer_delete_reparse_and_hardlink_attempts() {
        for foreign_kind in ["reparse", "hardlink"] {
            let temporary = tempfile::tempdir().expect("temporary root");
            let bytes = b"cache artifact";
            let digest = hex_digest(bytes);
            let manifest = directory_manifest(vec![(vec![digest.as_str()], bytes)]);
            let capability = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
            )
            .expect("capability");
            capability
                .create_directory_staging(&manifest)
                .expect("directory staging");
            capability
                .materialize_verified_directory(&[DirectoryPayload {
                    relative_segments: vec![digest.clone()],
                    bytes,
                }])
                .expect("materialize");
            let foreign = temporary.path().join(STAGING_DIRECTORY).join("foreign");
            if foreign_kind == "reparse" {
                let target = tempfile::tempdir().expect("reparse target");
                assert!(std::os::windows::fs::symlink_file(
                    target.path().join("missing"),
                    &foreign
                )
                .is_err());
            } else {
                assert!(std::fs::hard_link(
                    temporary.path().join(STAGING_DIRECTORY).join(&digest),
                    &foreign,
                )
                .is_err());
            }
            assert!(OpenOptions::new()
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(temporary.path().join(STAGING_DIRECTORY).join(&digest))
                .is_err());
            assert!(
                std::fs::remove_file(temporary.path().join(STAGING_DIRECTORY).join(&digest))
                    .is_err()
            );
            assert!(std::fs::rename(
                temporary.path().join(STAGING_DIRECTORY),
                temporary.path().join("staging-foreign-rename"),
            )
            .is_err());
            capability
                .seal_staged_directory()
                .expect("seal locked tree");
            capability
                .promote_staged_directory(RuntimeStorageCommittedLeaf::Cache)
                .expect("safe promotion after denied foreign mutations");
        }
    }

    #[test]
    fn directory_promotion_requires_a_sealed_session_and_keeps_the_residual() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let bytes = b"cache artifact";
        let digest = hex_digest(bytes);
        let manifest = directory_manifest(vec![(vec![digest.as_str()], bytes)]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec![digest],
                bytes,
            }])
            .expect("materialize");
        assert!(capability
            .promote_staged_directory(RuntimeStorageCommittedLeaf::Cache)
            .is_err());
        assert!(temporary.path().join(STAGING_DIRECTORY).is_dir());
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());
        capability.discard_uncommitted().expect("owned cleanup");
    }

    #[test]
    fn directory_partial_write_and_cleanup_failure_leave_a_recovery_marker() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let bytes = b"cache artifact";
        let digest = hex_digest(bytes);
        let manifest = directory_manifest(vec![(vec![digest.as_str()], bytes)]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        inject_test_failure(TestFailurePoint::PartialWrite);
        inject_test_failure(TestFailurePoint::CleanupDisposition);

        assert!(capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec![digest],
                bytes,
            }])
            .is_err());
        assert!(temporary.path().join(STAGING_DIRECTORY).is_dir());
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());
        capability
            .discard_uncommitted()
            .expect("explicit retry cleanup");
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn nested_cleanup_failure_is_reported_and_drop_preserves_a_fail_closed_marker() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let bytes = b"nested cleanup artifact";
        let manifest = directory_manifest(vec![(vec!["nested", "payload"], bytes)]);
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        capability
            .create_directory_staging(&manifest)
            .expect("directory staging");
        capability
            .materialize_verified_directory(&[DirectoryPayload {
                relative_segments: vec!["nested".to_owned(), "payload".to_owned()],
                bytes,
            }])
            .expect("materialize nested payload");

        inject_test_failure(TestFailurePoint::CleanupAfterDescendantDisposition);
        assert_eq!(
            capability.discard_uncommitted().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityUnavailable,
            "callers receive the failed descendant cleanup"
        );
        assert!(
            !temporary
                .path()
                .join(STAGING_DIRECTORY)
                .join("nested")
                .join("payload")
                .exists(),
            "the descendant disposition completed before the injected failure"
        );
        assert!(temporary
            .path()
            .join(STAGING_DIRECTORY)
            .join("nested")
            .is_dir());
        inject_test_failure(TestFailurePoint::CleanupDisposition);
        drop(capability);

        assert!(
            temporary.path().join(STAGING_DIRECTORY).is_dir(),
            "Drop does not delete an ancestor after descendant cleanup fails"
        );
        assert!(
            temporary.path().join(STAGING_SESSION_MARKER).is_file(),
            "the durable marker makes the next writer fail closed"
        );
        let retry = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("retry binding"),
        )
        .expect("retry capability");
        assert!(retry.create_staging().is_err());
    }

    #[test]
    fn production_equivalent_preflight_and_writer_leases_create_staging_and_payload() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("writer lease with FILE_ADD_SUBDIRECTORY and FILE_ADD_FILE");
        let digest = hex_digest(b"lease payload");

        capability.create_staging().expect("staging");
        capability
            .write_verified_payload(b"lease payload", &digest)
            .expect("payload");

        assert_eq!(
            std::fs::read(temporary.path().join(STAGING_DIRECTORY).join(PAYLOAD_FILE))
                .expect("payload bytes"),
            b"lease payload"
        );
    }

    #[test]
    fn staging_payload_refuses_a_foreign_writer_before_verification() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        let digest = hex_digest(b"verified payload");

        capability.create_staging().expect("staging");
        capability
            .write_verified_payload(b"verified payload", &digest)
            .expect("payload");
        assert!(std::fs::write(
            temporary.path().join(STAGING_DIRECTORY).join(PAYLOAD_FILE),
            b"tampered payload",
        )
        .is_err());
        capability
            .verify_regular_payload()
            .expect("foreign writer was denied");
    }

    #[test]
    fn rejected_payload_digest_discards_the_uncommitted_staging_directory() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        capability.create_staging().expect("staging");
        assert!(capability
            .write_verified_payload(b"verified payload", &hex_digest(b"other payload"))
            .is_err());
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
    }

    #[test]
    fn initial_identity_failure_after_file_created_discards_the_pending_object() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        inject_test_failure(TestFailurePoint::InitialIdentity);
        assert!(capability.create_staging().is_err());
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(capability
            .session
            .lock()
            .expect("session")
            .staging
            .is_none());
    }

    #[test]
    fn partial_write_and_post_verification_failures_discard_payload_before_staging() {
        for failure in [
            TestFailurePoint::PartialWrite,
            TestFailurePoint::PostVerification,
        ] {
            let temporary = tempfile::tempdir().expect("temporary root");
            let capability = ManagedRootCapability::from_verified_preflight(
                test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
            )
            .expect("capability");
            let bytes = b"verified payload";
            let digest = hex_digest(bytes);

            capability.create_staging().expect("staging");
            inject_test_failure(failure);
            if failure == TestFailurePoint::PartialWrite {
                assert!(capability.write_verified_payload(bytes, &digest).is_err());
            } else {
                capability
                    .write_verified_payload(bytes, &digest)
                    .expect("payload");
                assert!(capability.verify_regular_payload().is_err());
            }
            assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        }
    }

    #[test]
    fn post_write_root_revalidation_drift_discards_the_uncommitted_session() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let foreign = temporary.path().join("foreign");
        std::fs::write(&foreign, b"foreign data").expect("foreign object");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        let bytes = b"verified payload";
        let digest = hex_digest(bytes);

        capability.create_staging().expect("staging");
        capability
            .write_verified_payload(bytes, &digest)
            .expect("payload");
        inject_test_failure(TestFailurePoint::RootRevalidation);
        assert!(capability.verify_regular_payload().is_err());
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert_eq!(
            std::fs::read(&foreign).expect("foreign object"),
            b"foreign data"
        );
    }

    #[test]
    fn disposition_failure_keeps_session_owned_handles_for_a_synchronous_retry() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");
        let bytes = b"verified payload";
        let digest = hex_digest(bytes);

        capability.create_staging().expect("staging");
        capability
            .write_verified_payload(bytes, &digest)
            .expect("payload");
        inject_test_failure(TestFailurePoint::PostVerification);
        inject_test_failure(TestFailurePoint::CleanupDisposition);
        assert!(capability.verify_regular_payload().is_err());
        {
            let session = capability.session.lock().expect("session");
            assert!(session.payload.is_some());
            assert!(session.staging.is_some());
            assert!(session.recovery_marker.is_some());
        }
        assert!(temporary.path().join(STAGING_DIRECTORY).is_dir());
        assert!(temporary.path().join(STAGING_SESSION_MARKER).is_file());

        capability
            .discard_uncommitted()
            .expect("retry cleanup retains the owned handle");
        assert!(!temporary.path().join(STAGING_DIRECTORY).exists());
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn foreign_staging_directory_is_not_adopted_or_removed() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let foreign_staging = temporary.path().join(STAGING_DIRECTORY);
        std::fs::create_dir(&foreign_staging).expect("foreign staging");
        std::fs::write(foreign_staging.join("foreign"), b"untrusted").expect("foreign data");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(temporary.path())).expect("preflight binding"),
        )
        .expect("capability");

        assert!(capability.create_staging().is_err());
        assert!(foreign_staging.is_dir());
        assert_eq!(
            std::fs::read(foreign_staging.join("foreign")).expect("foreign data"),
            b"untrusted"
        );
        assert!(!temporary.path().join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn root_drift_fails_before_a_new_root_is_written() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("managed-root");
        std::fs::create_dir(&root).expect("managed root");
        let binding = test_root_binding(test_root_handle(&root)).expect("test root binding");
        let moved = temporary.path().join("moved-root");
        std::fs::rename(&root, &moved).expect("move original root");
        std::fs::create_dir(&root).expect("replacement root");

        assert!(ManagedRootCapability::from_verified_preflight(binding).is_err());
        assert!(!root.join(STAGING_DIRECTORY).exists());
        assert!(!moved.join(STAGING_DIRECTORY).exists());
    }

    #[test]
    fn writer_lease_refuses_delete_sharing_so_root_swap_cannot_race_a_mutation() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("managed-root");
        let moved = temporary.path().join("moved-root");
        std::fs::create_dir(&root).expect("managed root");
        let capability = ManagedRootCapability::from_verified_preflight(
            test_root_binding(test_root_handle(&root)).expect("test root binding"),
        )
        .expect("capability");

        assert!(std::fs::rename(&root, &moved).is_err());
        assert!(!moved.exists());
        capability.create_staging().expect("safe staging creation");
        assert!(root.join(STAGING_DIRECTORY).is_dir());
    }

    #[test]
    fn writer_lease_rejects_a_preexisting_compatible_root_write_handle() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let root = temporary.path().join("managed-root");
        std::fs::create_dir(&root).expect("managed root");
        let binding = test_root_binding(test_root_handle(&root)).expect("test root binding");
        let _foreign_writer = OpenOptions::new()
            .access_mode(
                FILE_READ_ATTRIBUTES
                    | FILE_ADD_FILE
                    | FILE_ADD_SUBDIRECTORY
                    | FILE_TRAVERSE
                    | DELETE
                    | SYNCHRONIZE,
            )
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .expect("preexisting compatible root writer");

        let error = ManagedRootCapability::from_verified_preflight(binding)
            .expect_err("preexisting compatible root writer must block the writer lease");
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert!(!root.join(STAGING_DIRECTORY).exists());
        assert!(!root.join(STAGING_SESSION_MARKER).exists());
    }

    #[test]
    fn reparse_root_is_rejected_before_staging_creation() {
        let temporary = tempfile::tempdir().expect("temporary parent");
        let target = temporary.path().join("target");
        let link = temporary.path().join("managed-link");
        std::fs::create_dir(&target).expect("target root");
        if std::os::windows::fs::symlink_dir(&target, &link).is_err() {
            eprintln!("SKIP reparse-root coverage: symlink creation is unavailable");
            return;
        }

        let binding = test_root_binding(test_root_handle(&link)).expect("link binding");
        assert!(ManagedRootCapability::from_verified_preflight(binding).is_err());
        assert!(!target.join(STAGING_DIRECTORY).exists());
    }
}
