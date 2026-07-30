use std::cell::RefCell;

use goose_verified_sources::{
    ChainLookup, Digest32, DocumentDigests, ErrorCode, ExistingSequence, LookupState, Root,
    VerificationContext, VerifiedEnvelope,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::{error_code_str, parse_doc_kind, DbError, VerifiedFactRecord};

pub(crate) enum AttemptDisposition<'a> {
    ParseRejected(ErrorCode),
    Successful {
        verified: &'a VerifiedEnvelope,
        verified_fact_id: Option<i64>,
        chain_error: Option<ErrorCode>,
        final_disposition: &'static str,
    },
}

pub(crate) struct PreparedAttempt {
    pub(crate) audit: AttemptAudit,
    pub(crate) verified: VerifiedEnvelope,
}

impl PreparedAttempt {
    pub(crate) fn verified(
        raw: &[u8],
        verified: VerifiedEnvelope,
        raw_limit: usize,
    ) -> Result<Self, DbError> {
        Ok(Self {
            audit: AttemptAudit::from_raw(raw, raw_limit)?,
            verified,
        })
    }
}

#[derive(Clone)]
pub(crate) struct AttemptAudit {
    pub(crate) raw_hash: [u8; 32],
    pub(crate) raw_total_len: u64,
    pub(crate) raw_truncated: bool,
    pub(crate) raw_evidence: Vec<u8>,
}

impl AttemptAudit {
    pub(crate) fn from_raw(raw: &[u8], raw_limit: usize) -> Result<Self, DbError> {
        let raw_hash = crate::hash_parts("evidence-db.raw", &[raw])?;
        let raw_truncated = raw.len() > raw_limit;
        let raw_evidence = raw[..raw.len().min(raw_limit)].to_vec();
        Ok(Self {
            raw_hash,
            raw_total_len: checked_usize_to_u64(raw.len())?,
            raw_truncated,
            raw_evidence,
        })
    }
}

pub(crate) struct JournalEvent {
    pub(crate) event_type: &'static str,
    pub(crate) attempt_id: Option<i64>,
    pub(crate) source_id: Option<Vec<u8>>,
    pub(crate) sequence: Option<u64>,
    pub(crate) verified_fact_id: Option<i64>,
    pub(crate) related_verified_fact_id: Option<i64>,
    pub(crate) error_code: Option<ErrorCode>,
    pub(crate) event_root: [u8; 32],
}

impl JournalEvent {
    pub(crate) fn verified(
        event_type: &'static str,
        attempt_id: i64,
        verified: &VerifiedEnvelope,
        verified_fact_id: i64,
        related_verified_fact_id: Option<i64>,
    ) -> Result<Self, DbError> {
        let source_bytes = verified.envelope.payload.source_id.as_bytes();
        let sequence = verified.envelope.payload.sequence;
        let mut related_buffer = [0u8; 8];
        if let Some(value) = related_verified_fact_id {
            related_buffer.copy_from_slice(&checked_i64_to_u64(value)?.to_be_bytes());
        }
        let event_root = crate::hash_parts(
            "evidence-db.journal-event",
            &[
                event_type.as_bytes(),
                source_bytes,
                &sequence.to_be_bytes(),
                verified.digests.document.as_bytes(),
                verified.digests.signature_set.as_bytes(),
                &related_buffer,
            ],
        )?;
        Ok(Self {
            event_type,
            attempt_id: Some(attempt_id),
            source_id: Some(source_bytes.to_vec()),
            sequence: Some(sequence),
            verified_fact_id: Some(verified_fact_id),
            related_verified_fact_id,
            error_code: None,
            event_root,
        })
    }

    pub(crate) fn rejected(
        event_type: &'static str,
        attempt_id: i64,
        raw_hash: [u8; 32],
        source_id: Option<Vec<u8>>,
        sequence: Option<u64>,
        error_code: Option<ErrorCode>,
    ) -> Result<Self, DbError> {
        let empty_source = Vec::new();
        let source_bytes = source_id.as_deref().unwrap_or(empty_source.as_slice());
        let sequence_bytes = sequence.unwrap_or(0).to_be_bytes();
        let error_bytes = error_code
            .map(error_code_str)
            .unwrap_or("none")
            .as_bytes()
            .to_vec();
        let event_root = crate::hash_parts(
            "evidence-db.journal-event",
            &[
                event_type.as_bytes(),
                source_bytes,
                &sequence_bytes,
                &raw_hash,
                &error_bytes,
            ],
        )?;
        Ok(Self {
            event_type,
            attempt_id: Some(attempt_id),
            source_id,
            sequence,
            verified_fact_id: None,
            related_verified_fact_id: None,
            error_code,
            event_root,
        })
    }
}

pub(crate) struct TxContext<'tx> {
    pub(crate) tx: &'tx Transaction<'tx>,
    latched_error: RefCell<Option<DbError>>,
}

impl<'tx> TxContext<'tx> {
    pub(crate) fn new(tx: &'tx Transaction<'tx>) -> Self {
        Self {
            tx,
            latched_error: RefCell::new(None),
        }
    }

    pub(crate) fn finish<T>(self, result: Result<T, DbError>) -> Result<T, DbError> {
        match self.latched_error.into_inner() {
            Some(error) => Err(error),
            None => result,
        }
    }

    fn latch<T>(&self, result: Result<T, DbError>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.record(error);
                None
            }
        }
    }

    fn record_closed(&self) {
        self.record(DbError::ClosedIndeterminate);
    }

    fn record(&self, error: DbError) {
        let mut slot = self.latched_error.borrow_mut();
        if slot.is_none() {
            *slot = Some(error);
        }
    }
}

impl ChainLookup for TxContext<'_> {
    fn lookup(&self, source_id: &str, sequence: u64) -> LookupState {
        let head = match self.latch(map_source_head_row_from_conn(self.tx, source_id.as_bytes())) {
            Some(head) => head,
            None => return LookupState::Fork,
        };
        if let Some(head) = head {
            match head.head_state.as_str() {
                "accepted" => {}
                "forked" => {
                    let head_sequence = match u64::try_from(head.head_sequence) {
                        Ok(value) => value,
                        Err(_) => {
                            self.record_closed();
                            return LookupState::Fork;
                        }
                    };
                    if sequence <= head_sequence {
                        return LookupState::Fork;
                    }
                }
                _ => {
                    self.record_closed();
                    return LookupState::Fork;
                }
            }
        }
        let slot = match self.latch(map_chain_slot_row_from_conn(
            self.tx,
            source_id.as_bytes(),
            sequence,
        )) {
            Some(slot) => slot,
            None => return LookupState::Fork,
        };
        match slot {
            None => LookupState::Absent,
            Some(slot) => match slot.slot_state.as_str() {
                "forked" => LookupState::Fork,
                "accepted" => {
                    let fact_id = match slot.head_verified_fact_id {
                        Some(value) => value,
                        None => {
                            self.record_closed();
                            return LookupState::Fork;
                        }
                    };
                    let fact = match self.latch(map_fact_row_from_conn(self.tx, fact_id)) {
                        Some(fact) => fact,
                        None => return LookupState::Fork,
                    };
                    match fact {
                        Some(fact) => LookupState::Verified(fact.to_resolved_document()),
                        None => {
                            self.record_closed();
                            LookupState::Fork
                        }
                    }
                }
                _ => {
                    self.record_closed();
                    LookupState::Fork
                }
            },
        }
    }
}

impl VerificationContext for TxContext<'_> {
    fn classify_existing(
        &self,
        source_id: &str,
        sequence: u64,
        document_digest: &Digest32,
        signature_set_digest: &Digest32,
    ) -> ExistingSequence {
        let head = match self.latch(map_source_head_row_from_conn(self.tx, source_id.as_bytes())) {
            Some(head) => head,
            None => return ExistingSequence::Fork,
        };
        if let Some(head) = head {
            match head.head_state.as_str() {
                "accepted" => {}
                "forked" => {
                    let head_sequence = match u64::try_from(head.head_sequence) {
                        Ok(value) => value,
                        Err(_) => {
                            self.record_closed();
                            return ExistingSequence::Fork;
                        }
                    };
                    if sequence <= head_sequence {
                        return ExistingSequence::Fork;
                    }
                }
                _ => {
                    self.record_closed();
                    return ExistingSequence::Fork;
                }
            }
        }
        let slot = match self.latch(map_chain_slot_row_from_conn(
            self.tx,
            source_id.as_bytes(),
            sequence,
        )) {
            Some(slot) => slot,
            None => return ExistingSequence::Fork,
        };
        let Some(slot) = slot else {
            return ExistingSequence::Absent;
        };
        match slot.slot_state.as_str() {
            "forked" => return ExistingSequence::Fork,
            "accepted" => {}
            _ => {
                self.record_closed();
                return ExistingSequence::Fork;
            }
        }
        let fact_id = match slot.head_verified_fact_id {
            Some(value) => value,
            None => {
                self.record_closed();
                return ExistingSequence::Fork;
            }
        };
        let head_fact = match self.latch(map_fact_row_from_conn(self.tx, fact_id)) {
            Some(head_fact) => head_fact,
            None => return ExistingSequence::Fork,
        };
        let Some(head_fact) = head_fact else {
            self.record_closed();
            return ExistingSequence::Fork;
        };
        if head_fact.digests.document != *document_digest {
            return ExistingSequence::Fork;
        }
        let sequence = match self.latch(as_i64(sequence)) {
            Some(sequence) => sequence,
            None => return ExistingSequence::Fork,
        };
        let duplicate = match self.latch(
            self.tx
                .query_row(
                    "SELECT fact_id FROM verified_facts WHERE source_id = ?1 AND sequence = ?2 AND document_digest = ?3 AND signature_set_digest = ?4",
                    params![
                        source_id.as_bytes(),
                        sequence,
                        document_digest.as_bytes().as_slice(),
                        signature_set_digest.as_bytes().as_slice()
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(DbError::from),
        ) {
            Some(duplicate) => duplicate,
            None => return ExistingSequence::Fork,
        };
        if duplicate.is_some() {
            ExistingSequence::Duplicate
        } else {
            ExistingSequence::EquivalentVariant
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ChainSlotRow {
    pub(crate) source_id: Vec<u8>,
    pub(crate) sequence: u64,
    pub(crate) slot_state: String,
    pub(crate) head_verified_fact_id: Option<i64>,
    pub(crate) original_verified_fact_id: Option<i64>,
    pub(crate) conflict_verified_fact_id: Option<i64>,
    pub(crate) decisive_attempt_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceHeadRow {
    pub(crate) source_id: Vec<u8>,
    pub(crate) head_state: String,
    pub(crate) head_sequence: i64,
    pub(crate) head_verified_fact_id: Option<i64>,
    pub(crate) forked_slot_sequence: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ProjectionReplay {
    pub(crate) event_type: String,
    pub(crate) attempt_id: Option<i64>,
    pub(crate) source_id: Option<Vec<u8>>,
    pub(crate) sequence: Option<u64>,
    pub(crate) verified_fact_id: Option<i64>,
    pub(crate) related_verified_fact_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct AttemptRow {
    pub(crate) attempt_id: i64,
    pub(crate) raw_hash: [u8; 32],
    pub(crate) raw_total_len: u64,
    pub(crate) raw_truncated: bool,
    pub(crate) raw_evidence: Vec<u8>,
    pub(crate) disposition: String,
    pub(crate) parse_error_code: Option<String>,
    pub(crate) chain_error_code: Option<String>,
    pub(crate) source_id: Option<Vec<u8>>,
    pub(crate) sequence: Option<u64>,
    pub(crate) doc_kind: Option<goose_verified_sources::DocKind>,
    pub(crate) parent_document_digest: Option<goose_verified_sources::Digest32>,
    pub(crate) canonical_payload: Option<Vec<u8>>,
    pub(crate) canonical_root: Option<Vec<u8>>,
    pub(crate) canonical_state: Option<Vec<u8>>,
    pub(crate) canonical_signatures: Option<Vec<u8>>,
    pub(crate) canonical_envelope: Option<Vec<u8>>,
    pub(crate) document_digest: Option<goose_verified_sources::Digest32>,
    pub(crate) root_digest: Option<goose_verified_sources::Digest32>,
    pub(crate) state_digest: Option<goose_verified_sources::Digest32>,
    pub(crate) signature_set_digest: Option<goose_verified_sources::Digest32>,
    pub(crate) signature_message_digest: Option<goose_verified_sources::Digest32>,
    pub(crate) verified_fact_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct JournalRow {
    pub(crate) journal_seq: u64,
    pub(crate) event_type: String,
    pub(crate) attempt_id: Option<i64>,
    pub(crate) source_id: Option<Vec<u8>>,
    pub(crate) sequence: Option<u64>,
    pub(crate) verified_fact_id: Option<i64>,
    pub(crate) related_verified_fact_id: Option<i64>,
    pub(crate) error_code: Option<String>,
    pub(crate) event_root: [u8; 32],
}

pub(crate) trait VerifiedFactExt {
    fn to_resolved_document(&self) -> goose_verified_sources::ResolvedDocument;
}

impl VerifiedFactExt for VerifiedFactRecord {
    fn to_resolved_document(&self) -> goose_verified_sources::ResolvedDocument {
        goose_verified_sources::ResolvedDocument {
            document_digest: self.digests.document,
            root_digest: self.digests.root,
            state_digest: self.digests.state,
            canonical_root: self.canonical_root.clone(),
            canonical_state: self.canonical_state.clone(),
        }
    }
}

pub(crate) fn map_chain_slot_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChainSlotRow> {
    Ok(ChainSlotRow {
        source_id: row.get(0)?,
        sequence: i64_to_u64(row.get::<_, i64>(1)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        slot_state: row.get(2)?,
        head_verified_fact_id: row.get(3)?,
        original_verified_fact_id: row.get(4)?,
        conflict_verified_fact_id: row.get(5)?,
        decisive_attempt_id: row.get(6)?,
    })
}

pub(crate) fn map_source_head_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceHeadRow> {
    Ok(SourceHeadRow {
        source_id: row.get(0)?,
        head_state: row.get(1)?,
        head_sequence: row.get(2)?,
        head_verified_fact_id: row.get(3)?,
        forked_slot_sequence: row
            .get::<_, Option<i64>>(4)?
            .map(i64_to_u64)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}

pub(crate) fn map_fact_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VerifiedFactRecord> {
    Ok(VerifiedFactRecord {
        fact_id: row.get(0)?,
        source_id: row.get(1)?,
        sequence: i64_to_u64(row.get::<_, i64>(2)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        doc_kind: parse_doc_kind(&row.get::<_, String>(3)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        parent_document_digest: row
            .get::<_, Option<Vec<u8>>>(4)?
            .map(vec_to_digest32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        raw_bytes: row.get(5)?,
        canonical_payload: row.get(6)?,
        canonical_root: row.get(7)?,
        canonical_state: row.get(8)?,
        canonical_signatures: row.get(9)?,
        canonical_envelope: row.get(10)?,
        digests: DocumentDigests {
            document: vec_to_digest32(row.get(11)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
            root: vec_to_digest32(row.get(12)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
            state: vec_to_digest32(row.get(13)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
            signature_set: vec_to_digest32(row.get(14)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            signature_message: vec_to_digest32(row.get(15)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
        },
    })
}

pub(crate) fn map_attempt_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttemptRow> {
    Ok(AttemptRow {
        attempt_id: row.get(0)?,
        raw_hash: vec_to_fixed_32(row.get(1)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        raw_total_len: checked_i64_to_u64(row.get::<_, i64>(2)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        raw_truncated: match row.get::<_, i64>(3)? {
            0 => false,
            1 => true,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
        raw_evidence: row.get(4)?,
        disposition: row.get(5)?,
        parse_error_code: row.get(6)?,
        chain_error_code: row.get(7)?,
        source_id: row.get(8)?,
        sequence: row
            .get::<_, Option<i64>>(9)?
            .map(checked_i64_to_u64)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        doc_kind: row
            .get::<_, Option<String>>(10)?
            .map(|value| parse_doc_kind(&value))
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        parent_document_digest: row
            .get::<_, Option<Vec<u8>>>(11)?
            .map(vec_to_digest32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        canonical_payload: row.get(12)?,
        canonical_root: row.get(13)?,
        canonical_state: row.get(14)?,
        canonical_signatures: row.get(15)?,
        canonical_envelope: row.get(16)?,
        document_digest: row
            .get::<_, Option<Vec<u8>>>(17)?
            .map(vec_to_digest32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        root_digest: row
            .get::<_, Option<Vec<u8>>>(18)?
            .map(vec_to_digest32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        state_digest: row
            .get::<_, Option<Vec<u8>>>(19)?
            .map(vec_to_digest32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        signature_set_digest: row
            .get::<_, Option<Vec<u8>>>(20)?
            .map(vec_to_digest32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        signature_message_digest: row
            .get::<_, Option<Vec<u8>>>(21)?
            .map(vec_to_digest32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        verified_fact_id: row.get(22)?,
    })
}

pub(crate) fn map_journal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JournalRow> {
    Ok(JournalRow {
        journal_seq: checked_i64_to_u64(row.get::<_, i64>(0)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        event_type: row.get(1)?,
        attempt_id: row.get(2)?,
        source_id: row.get(3)?,
        sequence: row
            .get::<_, Option<i64>>(4)?
            .map(checked_i64_to_u64)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        verified_fact_id: row.get(5)?,
        related_verified_fact_id: row.get(6)?,
        error_code: row.get(7)?,
        event_root: vec_to_fixed_32(row.get(8)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}

pub(crate) fn map_source_head_row_from_conn(
    conn: &Connection,
    source_id: &[u8],
) -> Result<Option<SourceHeadRow>, DbError> {
    conn.query_row("SELECT source_id, head_state, head_sequence, head_verified_fact_id, forked_slot_sequence FROM source_heads WHERE source_id = ?1", params![source_id], map_source_head_row).optional().map_err(DbError::from)
}

pub(crate) fn map_chain_slot_row_from_conn(
    conn: &Connection,
    source_id: &[u8],
    sequence: u64,
) -> Result<Option<ChainSlotRow>, DbError> {
    conn.query_row("SELECT source_id, sequence, slot_state, head_verified_fact_id, original_verified_fact_id, conflict_verified_fact_id, decisive_attempt_id FROM chain_slots WHERE source_id = ?1 AND sequence = ?2", params![source_id, as_i64(sequence)?], map_chain_slot_row).optional().map_err(DbError::from)
}

pub(crate) fn map_fact_row_from_conn(
    conn: &Connection,
    fact_id: i64,
) -> Result<Option<VerifiedFactRecord>, DbError> {
    conn.query_row("SELECT fact_id, source_id, sequence, doc_kind, parent_document_digest, raw_bytes, canonical_payload, canonical_root, canonical_state, canonical_signatures, canonical_envelope, document_digest, root_digest, state_digest, signature_set_digest, signature_message_digest FROM verified_facts WHERE fact_id = ?1", params![fact_id], map_fact_row).optional().map_err(DbError::from)
}

pub(crate) fn map_attempt_row_from_conn(
    conn: &Connection,
    attempt_id: i64,
) -> Result<Option<AttemptRow>, DbError> {
    conn.query_row(
        "SELECT attempt_id, raw_hash, raw_total_len, raw_truncated, raw_evidence, disposition, parse_error_code, chain_error_code, source_id, sequence, doc_kind, parent_document_digest, canonical_payload, canonical_root, canonical_state, canonical_signatures, canonical_envelope, document_digest, root_digest, state_digest, signature_set_digest, signature_message_digest, verified_fact_id
         FROM ingest_attempts
         WHERE attempt_id = ?1",
        params![attempt_id],
        map_attempt_row,
    )
    .optional()
    .map_err(DbError::from)
}

fn vec_to_digest32(bytes: Vec<u8>) -> Result<Digest32, DbError> {
    Ok(Digest32::new(vec_to_fixed_32(bytes)?))
}

fn i64_to_u64(value: i64) -> Result<u64, DbError> {
    u64::try_from(value).map_err(|_| DbError::ClosedIndeterminate)
}

pub(crate) fn checked_usize_to_u64(value: usize) -> Result<u64, DbError> {
    u64::try_from(value).map_err(|_| DbError::NumericOverflow)
}

pub(crate) fn checked_i64_to_u64(value: i64) -> Result<u64, DbError> {
    u64::try_from(value).map_err(|_| DbError::NumericOverflow)
}

fn vec_to_fixed_32(bytes: Vec<u8>) -> Result<[u8; 32], DbError> {
    if bytes.len() != 32 {
        return Err(DbError::ClosedIndeterminate);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub(crate) fn as_i64(value: u64) -> Result<i64, DbError> {
    i64::try_from(value).map_err(|_| DbError::NumericOverflow)
}

pub(crate) fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}
