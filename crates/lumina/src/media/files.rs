use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};

use super::{MediaAccessError, MediaAvailability};

#[derive(Debug)]
pub(crate) struct GuardedDirectory {
    pub path: PathBuf,
    pub identity: String,
    _parents: Vec<File>,
}

fn unsafe_path() -> MediaAccessError {
    MediaAccessError::new(
        MediaAvailability::UnsafePath,
        "Media path or filesystem identity is unsafe",
    )
}

#[cfg(windows)]
fn information(
    file: &File,
) -> Result<windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(MediaAccessError::io(std::io::Error::last_os_error()).into());
    }
    Ok(info)
}

#[cfg(windows)]
pub(crate) fn identity(file: &File, directory: bool) -> Result<String> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };
    let info = information(file)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || (!directory && info.nNumberOfLinks != 1)
    {
        return Err(unsafe_path().into());
    }
    Ok(format!(
        "win32:{:08x}:{:08x}{:08x}",
        info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
    ))
}

#[cfg(not(windows))]
pub(crate) fn identity(_: &File, _: bool) -> Result<String> {
    anyhow::bail!("Guarded media file access is not yet implemented for this platform")
}

impl GuardedDirectory {
    #[cfg(windows)]
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        use std::path::Prefix;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ,
        };
        let mut components = path.components();
        let drive = match components.next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive.to_ascii_uppercase(),
                _ => return Err(unsafe_path().into()),
            },
            _ => return Err(unsafe_path().into()),
        };
        ensure!(
            matches!(components.next(), Some(Component::RootDir)),
            "Media root must be absolute"
        );
        let mut current = PathBuf::from(format!("\\\\?\\{}:\\", char::from(drive)));
        let mut paths = vec![current.clone()];
        for part in components {
            let Component::Normal(name) = part else {
                return Err(unsafe_path().into());
            };
            let name = name.to_str().ok_or_else(unsafe_path)?;
            if name.is_empty() || name.ends_with(['.', ' ']) || name.contains([':', '/', '\\']) {
                return Err(unsafe_path().into());
            }
            current.push(name);
            paths.push(current.clone());
        }
        ensure!(
            paths.len() > 1,
            "A volume root is not a media output directory"
        );
        let mut parents = Vec::new();
        let mut last_identity = String::new();
        for (index, path) in paths.iter().enumerate() {
            if create && index > 0 {
                match std::fs::create_dir(path) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(error) => return Err(MediaAccessError::io(error).into()),
                }
            }
            // Retaining every ancestor without write/delete sharing prevents a
            // validated component from being renamed or turned into a junction.
            let file = OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
                .map_err(MediaAccessError::io)?;
            last_identity = identity(&file, true)?;
            parents.push(file);
        }
        Ok(Self {
            path: current,
            identity: last_identity,
            _parents: parents,
        })
    }

    #[cfg(not(windows))]
    pub fn open(_: &Path, _: bool) -> Result<Self> {
        anyhow::bail!("Guarded media output is not yet implemented for this platform")
    }

    pub fn child(&self, relative: &str, create: bool) -> Result<Self> {
        let relative = Path::new(relative);
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(unsafe_path().into());
        }
        Self::open(&self.path.join(relative), create)
    }

    pub fn open_file(&self, name: &str, writing: bool, create: bool) -> Result<File> {
        if name.is_empty()
            || name.contains(['/', '\\', ':'])
            || name.starts_with('.')
            || name.ends_with(['.', ' '])
        {
            return Err(unsafe_path().into());
        }
        let mut options = OpenOptions::new();
        options.read(true).write(writing).create_new(create);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            };
            if writing {
                // DELETE access is needed for an atomic rename through this
                // same handle, without reopening an attacker-replaceable path.
                options.access_mode(0x8000_0000 | 0x4000_0000 | 0x0001_0000);
            }
            options
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options
            .open(self.path.join(name))
            .map_err(MediaAccessError::io)?;
        identity(&file, false)?;
        Ok(file)
    }
}

#[cfg(windows)]
pub(crate) fn rename(file: &File, destination: &Path) -> Result<()> {
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };
    // The Win32 full-path rename wrapper requires a terminator even though
    // FileNameLength excludes it; padding alone does not guarantee a final NUL.
    let name: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let bytes = offset
        .checked_add(name.len().checked_mul(2).ok_or_else(unsafe_path)?)
        .ok_or_else(unsafe_path)?;
    let length = u32::try_from(bytes)?;
    let mut aligned = vec![0_u64; bytes.div_ceil(8)];
    let ptr = aligned.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*ptr).Anonymous.ReplaceIfExists = false;
        (*ptr).RootDirectory = std::ptr::null_mut();
        (*ptr).FileNameLength = u32::try_from((name.len() - 1) * 2)?;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*ptr).FileName.as_mut_ptr(), name.len());
        if SetFileInformationByHandle(file.as_raw_handle(), FileRenameInfo, ptr.cast(), length) == 0
        {
            return Err(MediaAccessError::io(std::io::Error::last_os_error()).into());
        }
    }
    file.sync_all().map_err(MediaAccessError::io)?;
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn rename(_: &File, _: &Path) -> Result<()> {
    anyhow::bail!("Guarded media publication is not yet implemented for this platform")
}

pub(crate) fn digest(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        digest.update(&bytes[..count]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(crate) fn request_digest(value: &impl serde::Serialize) -> Result<String> {
    Ok(Sha256::digest(serde_json::to_vec(value)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
