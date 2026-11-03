use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::Sha256Digest;

pub const STORAGE_RESERVATION_RULES_VERSION: &str = "installation-storage-v1";
pub(crate) const PUBLICATION_STORAGE_RULES_VERSION: &str = "installation-storage-v2";
pub(crate) const TRANSFER_STORAGE_RULES_VERSION: &str = "installation-storage-v3";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageReservationState {
    NeedsAdmission,
    Held,
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageReconciliationPage {
    pub released: u32,
    pub next_after_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageReservation {
    pub rules_version: String,
    pub job_id: String,
    pub plan_digest: Sha256Digest,
    pub store_root: PathBuf,
    pub volume_id: Option<String>,
    pub peak_bytes: u64,
    pub reserved_bytes: u64,
    pub state: StorageReservationState,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl StorageReservation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(
                self.rules_version.as_str(),
                STORAGE_RESERVATION_RULES_VERSION
                    | PUBLICATION_STORAGE_RULES_VERSION
                    | TRANSFER_STORAGE_RULES_VERSION
            ) && !self.job_id.is_empty()
                && self.store_root.is_absolute()
                && self.peak_bytes > 0
                && self.peak_bytes <= i64::MAX as u64
                && self.created_at >= 0
                && self.updated_at >= self.created_at,
            "Invalid installation storage reservation"
        );
        ensure!(
            self.volume_id.as_ref().is_none_or(|id| !id.is_empty()),
            "Invalid storage volume identity"
        );
        ensure!(
            match self.state {
                StorageReservationState::NeedsAdmission =>
                    self.volume_id.is_none() && self.reserved_bytes == self.peak_bytes,
                StorageReservationState::Held =>
                    self.volume_id.is_some()
                        && self.reserved_bytes <= self.peak_bytes
                        && (self.rules_version != STORAGE_RESERVATION_RULES_VERSION
                            || self.reserved_bytes == self.peak_bytes),
                StorageReservationState::Released => self.reserved_bytes == 0,
            },
            "Storage reservation state mismatch"
        );
        Ok(())
    }
}

pub fn installation_storage_volume(root: &Path) -> Result<String> {
    ensure!(
        root.is_absolute() && root.is_dir(),
        "Storage volume requires an absolute directory"
    );
    #[cfg(windows)]
    {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        #[repr(C)]
        struct FileInformation {
            attributes: u32,
            creation_time: [u32; 2],
            access_time: [u32; 2],
            write_time: [u32; 2],
            volume_serial_number: u32,
            file_size_high: u32,
            file_size_low: u32,
            links: u32,
            file_index_high: u32,
            file_index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                handle: *mut std::ffi::c_void,
                information: *mut FileInformation,
            ) -> i32;
        }
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(7)
            .custom_flags(0x0220_0000)
            .open(root)?;
        let mut info = std::mem::MaybeUninit::<FileInformation>::uninit();
        let result =
            unsafe { GetFileInformationByHandle(directory.as_raw_handle(), info.as_mut_ptr()) };
        if result == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let info = unsafe { info.assume_init() };
        ensure!(
            info.attributes & 0x400 == 0,
            "Storage root is a reparse point"
        );
        Ok(format!("windows-volume:{:08x}", info.volume_serial_number))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!("unix-device:{}", std::fs::metadata(root)?.dev()))
    }
    #[cfg(not(any(windows, unix)))]
    anyhow::bail!("Storage volume identification is unsupported on this platform")
}
