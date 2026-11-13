use super::*;
use std::os::windows::{
    ffi::OsStrExt,
    fs::{MetadataExt, OpenOptionsExt},
    io::AsRawHandle,
};

/// Rename the verified handle into the content store without replacing an existing blob.
/// A committed staging journal already binds every transfer to its final content digest.
pub(super) fn verified_transfer<F: Fn() -> i64>(
    execution: &InstallationExecution<'_, F>,
    manifest: &ModelManifest,
    id: &str,
    transfer_root: &Path,
    relative: &RepositoryPath,
    destination: &Path,
) -> Result<()> {
    let source = checked_local_path(transfer_root, relative)?;
    let _source_tree = crate::lifecycle::files::pin_directory_tree(
        source.parent().context("Missing transfer parent")?,
    )?;
    let _target_tree = crate::lifecycle::files::pin_directory_tree(
        destination.parent().context("Missing blob parent")?,
    )?;
    let file = open_transfer(&source)?;
    let verified = super::super::files::verify_open_artifact(
        &manifest.plan,
        id,
        file.try_clone()?,
        relative,
        |_| execution.checkpoint().map(|_| ()),
    )?;
    let expected = &manifest.installed_files[id];
    ensure!(
        verified.sha256() == &expected.computed_sha256
            && verified.size_bytes() == expected.size_bytes,
        "Transfer differs from its journal"
    );
    execution.checkpoint()?;
    rename_noclobber(&file, destination)
}

fn open_transfer(source: &Path) -> Result<File> {
    // DELETE on this handle enables the rename; no other writer/deleter may open the source.
    let file = OpenOptions::new()
        .access_mode(0x8000_0000 | 0x0001_0000)
        .share_mode(1)
        .custom_flags(0x0020_0000)
        .open(source)?;
    ensure!(
        file.metadata()?.file_attributes() & 0x400 == 0,
        "Transfer cannot be a reparse point"
    );
    super::super::files::require_single_link_file(&file)?;
    Ok(file)
}

fn rename_noclobber(file: &File, destination: &Path) -> Result<()> {
    use std::ffi::c_void;
    #[repr(C)]
    struct RenameInfo {
        flags: u32,
        root_directory: *mut c_void,
        file_name_length: u32,
        file_name: [u16; 1],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileInformationByHandle(
            handle: *mut c_void,
            class: i32,
            information: *const c_void,
            length: u32,
        ) -> i32;
    }
    let name: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let offset = std::mem::offset_of!(RenameInfo, file_name);
    let name_bytes = name.len().checked_mul(2).context("Blob path too long")?;
    let length = u32::try_from(
        offset
            .checked_add(name_bytes)
            .context("Blob path too long")?,
    )?;
    let mut aligned = vec![0u64; (length as usize).div_ceil(8)];
    let info = aligned.as_mut_ptr().cast::<RenameInfo>();
    // FILE_RENAME_INFO has a variable-length UTF-16 tail. Zero flags prohibit replacement.
    unsafe {
        (*info).file_name_length = u32::try_from(name_bytes - 2)?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            info.cast::<u8>().add(offset).cast::<u16>(),
            name.len(),
        );
        if SetFileInformationByHandle(file.as_raw_handle(), 3, info.cast(), length) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_keeps_file_identity_and_preserves_an_existing_blob() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let source = parent.join("weight.part");
        let occupied = parent.join("existing-blob");
        let destination = parent.join("blob");
        std::fs::write(&source, b"verified-content").unwrap();
        std::fs::write(&occupied, b"previous").unwrap();
        let handle = open_transfer(&source).unwrap();
        assert!(OpenOptions::new().write(true).open(&source).is_err());
        assert!(rename_noclobber(&handle, &occupied).is_err());
        assert_eq!(std::fs::read(occupied).unwrap(), b"previous");
        rename_noclobber(&handle, &destination).unwrap();
        assert!(!source.exists());
        assert!(OpenOptions::new().write(true).open(&destination).is_err());
        drop(handle);
        assert_eq!(std::fs::read(destination).unwrap(), b"verified-content");
    }

    #[test]
    fn transfer_with_another_hard_link_cannot_be_promoted() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("weight.part");
        std::fs::write(&source, b"verified-content").unwrap();
        std::fs::hard_link(&source, temp.path().join("alias")).unwrap();
        assert!(open_transfer(&source).is_err());
        assert!(source.exists());
    }
}
