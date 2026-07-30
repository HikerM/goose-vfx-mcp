pub mod anchor;
pub mod error;
mod path_guard;
mod projection;
mod schema;
mod store;
pub mod v2;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::path::PathBuf;

use anchor::AnchorStore;
use goose_verified_sources::{Digest32, DocKind, DocumentDigests};
use rusqlite::Connection;

pub use anchor::{AnchorHeadRecord, AnchorState, MemoryAnchorStore};
pub use error::DbError;
pub(crate) use path_guard::{
    read_anchor_sidecar, replace_file_atomic, validate_db_path, write_anchor_sidecar,
    ValidatedPaths, MAX_ANCHOR_SIDECAR_BYTES,
};
pub(crate) use projection::{
    as_i64, bool_to_i64, checked_i64_to_u64, checked_usize_to_u64, map_attempt_row,
    map_attempt_row_from_conn, map_chain_slot_row, map_chain_slot_row_from_conn, map_fact_row,
    map_fact_row_from_conn, map_journal_row, map_source_head_row, map_source_head_row_from_conn,
    AttemptAudit, AttemptDisposition, AttemptRow, ChainSlotRow, JournalEvent, JournalRow,
    PreparedAttempt, ProjectionReplay, SourceHeadRow, TxContext, VerifiedFactExt,
};
pub(crate) use schema::{
    anchor_state_str, checked_frame_len, configure_connection, doc_kind_str, ensure_anchor_row,
    ensure_schema, error_code_str, hash_parts, load_anchor_state_from_conn, parse_anchor_state,
    parse_doc_kind, parse_error_code, recreate_projection_schema, verify_schema, zero_anchor,
};

pub(crate) const RAW_EVIDENCE_LIMIT: usize = 4096;
pub(crate) const SCHEMA_VERSION: &str = "goose-evidence-db/pre-a0.2";
pub(crate) const SCHEMA_PARAMS: &str = "raw_limit=4096;journal=v2;anchor=v1";
pub(crate) const ANCHOR_ROW_ID: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestOutcome {
    pub attempt_id: i64,
    pub disposition: IngestDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestDisposition {
    ParseRejected {
        error: goose_verified_sources::ErrorCode,
    },
    ChainRejected {
        error: goose_verified_sources::ErrorCode,
    },
    Accepted {
        fact_id: i64,
    },
    Duplicate {
        fact_id: i64,
    },
    EquivalentVariant {
        fact_id: i64,
    },
    Forked {
        original_fact_id: i64,
        conflict_fact_id: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointOutcome {
    pub anchor: AnchorHeadRecord,
    pub created_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainSlotRecord {
    Absent,
    Accepted(VerifiedFactRecord),
    Fork(ChainForkRecord),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainForkRecord {
    pub source_id: Vec<u8>,
    pub sequence: u64,
    pub original: VerifiedFactRecord,
    pub conflict: VerifiedFactRecord,
    pub decisive_attempt_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadRecord {
    Absent,
    Accepted(VerifiedFactRecord),
    Fork(ChainForkRecord),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFactRecord {
    pub fact_id: i64,
    pub source_id: Vec<u8>,
    pub sequence: u64,
    pub doc_kind: DocKind,
    pub parent_document_digest: Option<Digest32>,
    pub raw_bytes: Vec<u8>,
    pub canonical_payload: Vec<u8>,
    pub canonical_root: Vec<u8>,
    pub canonical_state: Vec<u8>,
    pub canonical_signatures: Vec<u8>,
    pub canonical_envelope: Vec<u8>,
    pub digests: DocumentDigests,
}

pub struct EvidenceDb {
    pub(crate) db_path: PathBuf,
    pub(crate) lock_path: PathBuf,
    pub(crate) anchor_path: PathBuf,
    pub(crate) shadow_path: PathBuf,
    pub(crate) lock_file: File,
    pub(crate) conn: Option<Connection>,
    pub(crate) anchor_store: Box<dyn AnchorStore>,
    pub(crate) raw_evidence_limit: usize,
}
