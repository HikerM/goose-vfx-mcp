use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use crate::{checked_usize_to_u64, AnchorHeadRecord, AnchorState, DbError};

const REPARSE_POINT_ATTRIBUTE: u32 = 0x0400;
const ANCHOR_RECORD_LEN: usize = 87;
pub(crate) const MAX_ANCHOR_SIDECAR_BYTES: u64 = 1_048_576u64 * ANCHOR_RECORD_LEN as u64;

#[cfg(windows)]
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;
#[cfg(windows)]
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
#[cfg(windows)]
const FILE_SHARE_DELETE: u32 = 0x0000_0004;
#[cfg(windows)]
const OPEN_EXISTING: u32 = 3;
#[cfg(windows)]
const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1_isize as *mut std::ffi::c_void;

#[cfg(windows)]
#[repr(C)]
struct FileTime {
    dw_low_date_time: u32,
    dw_high_date_time: u32,
}

#[cfg(windows)]
#[repr(C)]
struct ByHandleFileInformation {
    dw_file_attributes: u32,
    ft_creation_time: FileTime,
    ft_last_access_time: FileTime,
    ft_last_write_time: FileTime,
    dw_volume_serial_number: u32,
    n_file_size_high: u32,
    n_file_size_low: u32,
    n_number_of_links: u32,
    n_file_index_high: u32,
    n_file_index_low: u32,
}

#[cfg(windows)]
#[link(name = "Kernel32")]
extern "system" {
    fn CreateFileW(
        lp_file_name: *const u16,
        dw_desired_access: u32,
        dw_share_mode: u32,
        lp_security_attributes: *mut std::ffi::c_void,
        dw_creation_disposition: u32,
        dw_flags_and_attributes: u32,
        h_template_file: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn GetFileInformationByHandle(
        h_file: *mut std::ffi::c_void,
        lp_file_information: *mut ByHandleFileInformation,
    ) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

pub(crate) struct ValidatedPaths {
    pub(crate) db_path: PathBuf,
    pub(crate) lock_path: PathBuf,
    pub(crate) anchor_path: PathBuf,
    pub(crate) shadow_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedDbAuthority {
    pub(crate) db_path: PathBuf,
    ancestor_chain: Vec<PathIdentity>,
    leaf_identity: Option<PathIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathIdentity {
    volume_serial_number: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Directory,
    File,
}

pub(crate) fn ensure_windows() -> Result<(), DbError> {
    #[cfg(windows)]
    {
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Err(DbError::UnsupportedPlatform)
    }
}

pub(crate) fn validate_db_path(path: &Path) -> Result<ValidatedPaths, DbError> {
    ensure_windows()?;
    if !path.is_absolute() {
        return Err(DbError::InvalidPathBinding);
    }
    if path.file_name().and_then(|value| value.to_str()) != Some("evidence.db") {
        return Err(DbError::InvalidPathBinding);
    }
    match path.components().next() {
        Some(Component::Prefix(prefix))
            if matches!(
                prefix.kind(),
                std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
            ) => {}
        _ => return Err(DbError::UnsupportedFilesystem),
    }
    let parent = path.parent().ok_or(DbError::InvalidPathBinding)?;
    validate_existing_ancestors(parent)?;
    if path.exists() {
        validate_existing_leaf(path)?;
    }
    let lock_path = parent.join("evidence.db.lock");
    let anchor_path = parent.join("evidence.db.anchor");
    let shadow_path = parent.join("evidence.db.shadow");
    for sibling in [&lock_path, &anchor_path, &shadow_path] {
        if sibling.exists() {
            validate_existing_leaf(sibling)?;
        }
    }
    Ok(ValidatedPaths {
        db_path: path.to_path_buf(),
        lock_path,
        anchor_path,
        shadow_path,
    })
}

pub(crate) fn capture_db_authority(path: &Path) -> Result<ValidatedDbAuthority, DbError> {
    ensure_windows()?;
    if !path.is_absolute() {
        return Err(DbError::InvalidPathBinding);
    }
    if path.file_name().and_then(|value| value.to_str()) != Some("evidence.db") {
        return Err(DbError::InvalidPathBinding);
    }
    match path.components().next() {
        Some(Component::Prefix(prefix))
            if matches!(
                prefix.kind(),
                std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
            ) => {}
        _ => return Err(DbError::UnsupportedFilesystem),
    }
    let parent = path.parent().ok_or(DbError::InvalidPathBinding)?;
    let lock_path = parent.join("evidence.db.lock");
    let anchor_path = parent.join("evidence.db.anchor");
    let shadow_path = parent.join("evidence.db.shadow");
    let ancestor_chain = capture_existing_ancestor_chain(parent)?;
    let leaf_identity = if path.exists() {
        Some(capture_existing_path_identity(path, PathKind::File, true)?)
    } else {
        None
    };
    for sibling in [&lock_path, &anchor_path, &shadow_path] {
        if sibling.exists() {
            capture_existing_path_identity(sibling, PathKind::File, true)?;
        }
    }
    Ok(ValidatedDbAuthority {
        db_path: path.to_path_buf(),
        ancestor_chain,
        leaf_identity,
    })
}

pub(crate) fn db_authority_exists(authority: &ValidatedDbAuthority) -> bool {
    authority.leaf_identity.is_some()
}

pub(crate) fn materialize_db_leaf_authority(path: &Path) -> Result<ValidatedDbAuthority, DbError> {
    ensure_windows()?;
    let authority = capture_db_authority(path)?;
    if authority.leaf_identity.is_some() {
        return Ok(authority);
    }
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    let file = options.open(path)?;
    validate_open_leaf(&file)?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;

        let leaf_identity = capture_path_identity_from_handle(
            file.as_raw_handle() as *mut std::ffi::c_void,
            PathKind::File,
            true,
        )?;
        return Ok(ValidatedDbAuthority {
            db_path: authority.db_path,
            ancestor_chain: authority.ancestor_chain,
            leaf_identity: Some(leaf_identity),
        });
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Err(DbError::UnsupportedPlatform)
    }
}

pub(crate) fn validate_opened_db_authority(
    path: &Path,
    expected: &ValidatedDbAuthority,
) -> Result<(), DbError> {
    let actual = capture_db_authority(path)?;
    if actual.ancestor_chain != expected.ancestor_chain {
        return Err(DbError::InvalidPathBinding);
    }
    let Some(actual_leaf_identity) = actual.leaf_identity else {
        return Err(DbError::InvalidPathBinding);
    };
    if let Some(expected_leaf_identity) = expected.leaf_identity {
        if actual_leaf_identity != expected_leaf_identity {
            return Err(DbError::InvalidPathBinding);
        }
    }
    Ok(())
}

pub(crate) fn validate_opened_db_handle_authority(
    handle: *mut std::ffi::c_void,
    expected: &ValidatedDbAuthority,
) -> Result<(), DbError> {
    ensure_windows()?;
    let Some(expected_leaf_identity) = expected.leaf_identity else {
        return Err(DbError::InvalidPathBinding);
    };
    let actual_leaf_identity = capture_path_identity_from_handle(handle, PathKind::File, true)?;
    if actual_leaf_identity != expected_leaf_identity {
        return Err(DbError::InvalidPathBinding);
    }
    Ok(())
}

fn validate_existing_ancestors(path: &Path) -> Result<(), DbError> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        let metadata = fs::symlink_metadata(candidate)?;
        validate_metadata(&metadata, PathKind::Directory, false)?;
        current = candidate.parent();
    }
    Ok(())
}

pub(crate) fn validate_existing_leaf(path: &Path) -> Result<(), DbError> {
    let metadata = fs::symlink_metadata(path)?;
    validate_metadata(&metadata, PathKind::File, true)
}

fn validate_metadata(
    metadata: &fs::Metadata,
    expected_kind: PathKind,
    enforce_single_link: bool,
) -> Result<(), DbError> {
    match expected_kind {
        PathKind::Directory if !metadata.is_dir() => return Err(DbError::InvalidPathBinding),
        PathKind::File if !metadata.is_file() => return Err(DbError::InvalidPathBinding),
        PathKind::Directory | PathKind::File => {}
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        if metadata.file_attributes() & REPARSE_POINT_ATTRIBUTE != 0 {
            return Err(DbError::PathReparseRejected);
        }
        if enforce_single_link && metadata.number_of_links() > 1 {
            return Err(DbError::HardlinkRejected);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (metadata, expected_kind, enforce_single_link);
        Err(DbError::UnsupportedPlatform)
    }
}

pub(crate) fn write_anchor_sidecar(path: &Path, anchor: &AnchorHeadRecord) -> Result<(), DbError> {
    let parent = path.parent().ok_or(DbError::InvalidPathBinding)?;
    validate_existing_ancestors(parent)?;
    if path.exists() {
        validate_existing_leaf(path)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    validate_open_leaf(&file)?;
    validate_existing_leaf(path)?;
    file.write_all(&encode_anchor_record(anchor))?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn read_anchor_sidecar(path: &Path) -> Result<AnchorHeadRecord, DbError> {
    validate_existing_leaf(path)?;
    let mut file = File::open(path)?;
    validate_open_leaf(&file)?;
    let len = file
        .metadata()
        .map_err(|_| DbError::ClosedIndeterminate)?
        .len();
    let record_len = checked_usize_to_u64(ANCHOR_RECORD_LEN)?;
    if len == 0 || len > MAX_ANCHOR_SIDECAR_BYTES || len % record_len != 0 {
        return Err(DbError::ClosedIndeterminate);
    }
    file.seek(SeekFrom::Start(len - record_len))
        .map_err(|_| DbError::ClosedIndeterminate)?;
    let mut bytes = [0u8; ANCHOR_RECORD_LEN];
    file.read_exact(&mut bytes)
        .map_err(|_| DbError::ClosedIndeterminate)?;
    parse_anchor_record(&bytes)
}

fn encode_anchor_record(anchor: &AnchorHeadRecord) -> [u8; ANCHOR_RECORD_LEN] {
    let mut bytes = [0u8; ANCHOR_RECORD_LEN];
    bytes[..5].copy_from_slice(b"GEDB1");
    bytes[5] = match anchor.state {
        AnchorState::Stable => 0,
        AnchorState::Pending => 1,
    };
    bytes[6..14].copy_from_slice(&anchor.checkpoint_sequence.to_be_bytes());
    bytes[14..22].copy_from_slice(&anchor.freeze_journal_sequence.to_be_bytes());
    bytes[22..54].copy_from_slice(&anchor.root_hash);
    match anchor.prepare_token {
        Some(token) => {
            bytes[54] = 1;
            bytes[55..87].copy_from_slice(&token);
        }
        None => {
            bytes[54] = 0;
        }
    }
    bytes
}

fn parse_anchor_record(bytes: &[u8]) -> Result<AnchorHeadRecord, DbError> {
    if bytes.len() != ANCHOR_RECORD_LEN || &bytes[..5] != b"GEDB1" {
        return Err(DbError::ClosedIndeterminate);
    }
    let state = match bytes[5] {
        0 => AnchorState::Stable,
        1 => AnchorState::Pending,
        _ => return Err(DbError::ClosedIndeterminate),
    };
    let checkpoint_sequence = u64::from_be_bytes(bytes[6..14].try_into().unwrap());
    let freeze_journal_sequence = u64::from_be_bytes(bytes[14..22].try_into().unwrap());
    let mut root_hash = [0u8; 32];
    root_hash.copy_from_slice(&bytes[22..54]);
    let prepare_token = match bytes[54] {
        0 => None,
        1 => {
            let mut token = [0u8; 32];
            token.copy_from_slice(&bytes[55..87]);
            Some(token)
        }
        _ => return Err(DbError::ClosedIndeterminate),
    };
    Ok(AnchorHeadRecord {
        state,
        checkpoint_sequence,
        freeze_journal_sequence,
        root_hash,
        prepare_token,
    })
}

fn validate_open_leaf(file: &File) -> Result<(), DbError> {
    let metadata = file.metadata()?;
    validate_metadata(&metadata, PathKind::File, true)
}

fn validate_sibling_binding(target: &Path, sibling: &Path) -> Result<(), DbError> {
    let target_parent = target.parent().ok_or(DbError::InvalidPathBinding)?;
    let sibling_parent = sibling.parent().ok_or(DbError::InvalidPathBinding)?;
    if target_parent != sibling_parent {
        return Err(DbError::InvalidPathBinding);
    }
    validate_existing_ancestors(target_parent)?;
    if target.exists() {
        validate_existing_leaf(target)?;
    }
    if sibling.exists() {
        validate_existing_leaf(sibling)?;
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn replace_file_atomic(target: &Path, replacement: &Path) -> Result<(), DbError> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut c_void,
            reserved: *mut c_void,
        ) -> i32;
    }

    const REPLACEFILE_IGNORE_MERGE_ERRORS: u32 = 0x0000_0002;

    validate_sibling_binding(target, replacement)?;

    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement_wide = replacement
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    let result = unsafe {
        ReplaceFileW(
            target_wide.as_ptr(),
            replacement_wide.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(DbError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn replace_file_atomic(_target: &Path, _replacement: &Path) -> Result<(), DbError> {
    Err(DbError::UnsupportedPlatform)
}

fn capture_existing_ancestor_chain(path: &Path) -> Result<Vec<PathIdentity>, DbError> {
    let mut current = Some(path);
    let mut chain = Vec::new();
    while let Some(candidate) = current {
        chain.push(capture_existing_path_identity(
            candidate,
            PathKind::Directory,
            false,
        )?);
        current = candidate.parent();
    }
    Ok(chain)
}

#[cfg(windows)]
fn capture_existing_path_identity(
    path: &Path,
    expected_kind: PathKind,
    reject_hardlinks: bool,
) -> Result<PathIdentity, DbError> {
    use std::os::windows::ffi::OsStrExt;

    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | match expected_kind {
            PathKind::Directory => FILE_FLAG_BACKUP_SEMANTICS,
            PathKind::File => 0,
        };
    let wide_path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(DbError::Io(std::io::Error::last_os_error()));
    }
    let identity = capture_path_identity_from_handle(handle, expected_kind, reject_hardlinks);
    let close_result = unsafe { CloseHandle(handle) };
    if close_result == 0 {
        return Err(DbError::Io(std::io::Error::last_os_error()));
    }
    identity
}

#[cfg(not(windows))]
fn capture_existing_path_identity(
    _path: &Path,
    _expected_kind: PathKind,
    _reject_hardlinks: bool,
) -> Result<PathIdentity, DbError> {
    Err(DbError::UnsupportedPlatform)
}

#[cfg(windows)]
fn capture_path_identity_from_handle(
    handle: *mut std::ffi::c_void,
    expected_kind: PathKind,
    reject_hardlinks: bool,
) -> Result<PathIdentity, DbError> {
    let mut info = ByHandleFileInformation {
        dw_file_attributes: 0,
        ft_creation_time: FileTime {
            dw_low_date_time: 0,
            dw_high_date_time: 0,
        },
        ft_last_access_time: FileTime {
            dw_low_date_time: 0,
            dw_high_date_time: 0,
        },
        ft_last_write_time: FileTime {
            dw_low_date_time: 0,
            dw_high_date_time: 0,
        },
        dw_volume_serial_number: 0,
        n_file_size_high: 0,
        n_file_size_low: 0,
        n_number_of_links: 0,
        n_file_index_high: 0,
        n_file_index_low: 0,
    };
    let result = unsafe { GetFileInformationByHandle(handle, &mut info) };
    if result == 0 {
        return Err(DbError::Io(std::io::Error::last_os_error()));
    }
    if info.dw_file_attributes & REPARSE_POINT_ATTRIBUTE != 0 {
        return Err(DbError::PathReparseRejected);
    }
    let is_directory = info.dw_file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    match (expected_kind, is_directory) {
        (PathKind::Directory, false) | (PathKind::File, true) => {
            return Err(DbError::InvalidPathBinding);
        }
        (PathKind::Directory, true) | (PathKind::File, false) => {}
    }
    if reject_hardlinks && info.n_number_of_links > 1 {
        return Err(DbError::HardlinkRejected);
    }
    Ok(PathIdentity {
        volume_serial_number: info.dw_volume_serial_number,
        file_index_high: info.n_file_index_high,
        file_index_low: info.n_file_index_low,
    })
}
#[cfg(test)]
pub(crate) fn validate_db_leaf_identity_for_test(
    actual_leaf_identity: [u32; 3],
    expected_leaf_identity: Option<[u32; 3]>,
) -> Result<(), DbError> {
    let actual_leaf_identity = PathIdentity {
        volume_serial_number: actual_leaf_identity[0],
        file_index_high: actual_leaf_identity[1],
        file_index_low: actual_leaf_identity[2],
    };
    let expected_leaf_identity = expected_leaf_identity
        .map(|expected_leaf_identity| PathIdentity {
            volume_serial_number: expected_leaf_identity[0],
            file_index_high: expected_leaf_identity[1],
            file_index_low: expected_leaf_identity[2],
        })
        .ok_or(DbError::InvalidPathBinding)?;

    if actual_leaf_identity != expected_leaf_identity {
        return Err(DbError::InvalidPathBinding);
    }

    Ok(())
}
