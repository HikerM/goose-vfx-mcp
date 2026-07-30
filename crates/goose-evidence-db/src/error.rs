use goose_verified_sources::ErrorCode;
use thiserror::Error;

use crate::anchor::AnchorStoreError;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("unsupported_platform")]
    UnsupportedPlatform,
    #[error("unsupported_filesystem")]
    UnsupportedFilesystem,
    #[error("closed_indeterminate")]
    ClosedIndeterminate,
    #[error("invalid_path_binding")]
    InvalidPathBinding,
    #[error("path_reparse_rejected")]
    PathReparseRejected,
    #[error("hardlink_rejected")]
    HardlinkRejected,
    #[error("schema_fence_mismatch")]
    SchemaFenceMismatch,
    #[error("legacy_schema_rejected")]
    LegacySchemaRejected,
    #[error("pending_checkpoint")]
    PendingCheckpoint,
    #[error("rebuild_refused")]
    RebuildRefused,
    #[error("numeric_overflow")]
    NumericOverflow,
    #[error("verified_source:{0}")]
    VerifiedSource(ErrorCode),
    #[error("anchor_store:{0}")]
    AnchorStore(#[from] AnchorStoreError),
    #[error("sqlite:{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io:{0}")]
    Io(#[from] std::io::Error),
}

impl From<ErrorCode> for DbError {
    fn from(value: ErrorCode) -> Self {
        Self::VerifiedSource(value)
    }
}
