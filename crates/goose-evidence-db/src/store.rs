use std::cmp::max;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};

use fs2::FileExt;
use goose_verified_sources::{
    parse_and_verify_envelope, verify_chain, ChainLookup, ChainVerification, Digest32, DocKind,
    Document, DocumentDigests, Envelope, ErrorCode, ExistingSequence, LookupState,
    ResolvedDocument, Root, SequenceDisposition, State, VerificationContext, VerifiedEnvelope,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::{
    anchor_state_str, as_i64, bool_to_i64, checked_i64_to_u64, checked_usize_to_u64,
    configure_connection, doc_kind_str, ensure_anchor_row, ensure_schema, error_code_str,
    hash_parts, load_anchor_state_from_conn, map_attempt_row, map_chain_slot_row_from_conn,
    map_fact_row, map_journal_row, map_source_head_row_from_conn, parse_anchor_state,
    parse_doc_kind, parse_error_code, read_anchor_sidecar, recreate_projection_schema,
    replace_file_atomic, validate_db_path, write_anchor_sidecar, AnchorHeadRecord, AnchorState,
    AttemptAudit, AttemptDisposition, AttemptRow, ChainForkRecord, ChainSlotRecord,
    CheckpointOutcome, DbError, EvidenceDb, HeadRecord, IngestDisposition, IngestOutcome,
    JournalEvent, JournalRow, PreparedAttempt, ProjectionReplay, SourceHeadRow, TxContext,
    ValidatedPaths, VerifiedFactExt, VerifiedFactRecord, ANCHOR_ROW_ID, RAW_EVIDENCE_LIMIT,
};

#[derive(Clone)]
struct ReplayVariant {
    document_digest: Digest32,
    signature_set_digest: Digest32,
    resolved: ResolvedDocument,
}

#[derive(Clone)]
enum ReplaySlot {
    Verified {
        head_fact_id: i64,
        variants: Vec<ReplayVariant>,
    },
    Forked {
        original_fact_id: i64,
        original_document_digest: Digest32,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplayHeadState {
    Accepted,
    Forked,
}

#[derive(Clone)]
struct ReplayHead {
    state: ReplayHeadState,
    head_sequence: u64,
    forked_slot_sequence: Option<u64>,
}

#[derive(Default)]
struct JournalReplayState {
    heads: HashMap<Vec<u8>, ReplayHead>,
    slots: HashMap<(Vec<u8>, u64), ReplaySlot>,
}

enum ReplayLookup {
    Absent,
    Accepted {
        document_digest: Digest32,
        signature_set_digest: Digest32,
    },
    Fork,
}

impl JournalReplayState {
    fn lookup(&self, source_id: &[u8], sequence: u64) -> ReplayLookup {
        if let Some(head) = self.heads.get(source_id) {
            if head.state == ReplayHeadState::Forked && sequence <= head.head_sequence {
                return ReplayLookup::Fork;
            }
        }
        match self.slots.get(&(source_id.to_vec(), sequence)) {
            Some(ReplaySlot::Verified { variants, .. }) => {
                let Some(first) = variants.first() else {
                    return ReplayLookup::Fork;
                };
                if variants
                    .iter()
                    .any(|variant| variant.document_digest != first.document_digest)
                {
                    return ReplayLookup::Fork;
                }
                ReplayLookup::Accepted {
                    document_digest: first.document_digest,
                    signature_set_digest: first.signature_set_digest,
                }
            }
            Some(ReplaySlot::Forked { .. }) => ReplayLookup::Fork,
            None => ReplayLookup::Absent,
        }
    }

    fn slot_key(source_id: &[u8], sequence: u64) -> (Vec<u8>, u64) {
        (source_id.to_vec(), sequence)
    }

    fn variant_from_verified(verified: &VerifiedEnvelope) -> ReplayVariant {
        ReplayVariant {
            document_digest: verified.digests.document,
            signature_set_digest: verified.digests.signature_set,
            resolved: ResolvedDocument {
                document_digest: verified.digests.document,
                root_digest: verified.digests.root,
                state_digest: verified.digests.state,
                canonical_root: verified.canonical_root.clone(),
                canonical_state: verified.canonical_state.clone(),
            },
        }
    }
}

impl ChainLookup for JournalReplayState {
    fn lookup(&self, source_id: &str, sequence: u64) -> LookupState {
        match JournalReplayState::lookup(self, source_id.as_bytes(), sequence) {
            ReplayLookup::Absent => LookupState::Absent,
            ReplayLookup::Fork => LookupState::Fork,
            ReplayLookup::Accepted { .. } => {
                let Some(slot) = self
                    .slots
                    .get(&Self::slot_key(source_id.as_bytes(), sequence))
                else {
                    return LookupState::Fork;
                };
                match slot {
                    ReplaySlot::Verified { variants, .. } => {
                        let Some(first) = variants.first() else {
                            return LookupState::Fork;
                        };
                        if variants
                            .iter()
                            .any(|variant| variant.document_digest != first.document_digest)
                        {
                            return LookupState::Fork;
                        }
                        LookupState::Verified(first.resolved.clone())
                    }
                    ReplaySlot::Forked { .. } => LookupState::Fork,
                }
            }
        }
    }
}

impl VerificationContext for JournalReplayState {
    fn classify_existing(
        &self,
        source_id: &str,
        sequence: u64,
        document_digest: &Digest32,
        signature_set_digest: &Digest32,
    ) -> ExistingSequence {
        if let Some(head) = self.heads.get(source_id.as_bytes()) {
            if head.state == ReplayHeadState::Forked && sequence <= head.head_sequence {
                return ExistingSequence::Fork;
            }
        }
        let Some(slot) = self
            .slots
            .get(&Self::slot_key(source_id.as_bytes(), sequence))
        else {
            return ExistingSequence::Absent;
        };
        match slot {
            ReplaySlot::Forked { .. } => ExistingSequence::Fork,
            ReplaySlot::Verified { variants, .. } => {
                let Some(first) = variants.first() else {
                    return ExistingSequence::Fork;
                };
                if first.document_digest != *document_digest {
                    return ExistingSequence::Fork;
                }
                if variants
                    .iter()
                    .any(|variant| variant.signature_set_digest == *signature_set_digest)
                {
                    ExistingSequence::Duplicate
                } else {
                    ExistingSequence::EquivalentVariant
                }
            }
        }
    }
}

impl EvidenceDb {
    pub fn open(
        path: impl AsRef<std::path::Path>,
        anchor_store: Box<dyn crate::anchor::AnchorStore>,
    ) -> Result<Self, DbError> {
        let validated = validate_db_path(path.as_ref())?;
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&validated.lock_path)?;
        lock_file.lock_exclusive()?;

        let mut conn = Connection::open(&validated.db_path)?;
        configure_connection(&mut conn)?;
        ensure_schema(&conn)?;
        ensure_anchor_row(&conn)?;

        let mut db = Self {
            db_path: validated.db_path,
            lock_path: validated.lock_path,
            anchor_path: validated.anchor_path,
            shadow_path: validated.shadow_path,
            lock_file,
            conn: Some(conn),
            anchor_store,
            raw_evidence_limit: RAW_EVIDENCE_LIMIT,
        };
        {
            let conn = db.conn()?;
            db.verify_attempt_consistency(conn)?;
            db.verify_journal_consistency(conn)?;
            db.verify_projection_consistency(conn)?;
        }
        db.verify_anchor_parity_on_open()?;
        Ok(db)
    }

    pub fn ingest_raw(&mut self, raw: &[u8]) -> Result<IngestOutcome, DbError> {
        self.ensure_writable()?;
        match parse_and_verify_envelope(raw) {
            Ok(verified) => self.ingest_verified_internal(raw, verified),
            Err(error) => self.record_parse_rejection(raw, error),
        }
    }

    pub fn lookup_chain(&self, source_id: &str, sequence: u64) -> Result<ChainSlotRecord, DbError> {
        let conn = self.conn()?;
        let source_bytes = source_id.as_bytes();
        if let Some(head) = self.load_source_head(conn, source_bytes)? {
            match head.head_state.as_str() {
                "accepted" => {}
                "forked" => {
                    let head_sequence = u64::try_from(head.head_sequence)
                        .map_err(|_| DbError::ClosedIndeterminate)?;
                    if sequence <= head_sequence {
                        return Ok(ChainSlotRecord::Fork(self.load_head_fork(conn, &head)?));
                    }
                }
                _ => return Err(DbError::ClosedIndeterminate),
            }
        }

        let Some(slot) = self.load_chain_slot(conn, source_bytes, sequence)? else {
            return Ok(ChainSlotRecord::Absent);
        };
        match slot.slot_state.as_str() {
            "forked" => Ok(ChainSlotRecord::Fork(self.load_fork_record(conn, &slot)?)),
            "accepted" => Ok(ChainSlotRecord::Accepted(
                self.load_fact_by_id(
                    conn,
                    slot.head_verified_fact_id
                        .ok_or(DbError::ClosedIndeterminate)?,
                )?,
            )),
            _ => Err(DbError::ClosedIndeterminate),
        }
    }

    pub fn lookup_head(&self, source_id: &str) -> Result<HeadRecord, DbError> {
        let conn = self.conn()?;
        let Some(head) = self.load_source_head(conn, source_id.as_bytes())? else {
            return Ok(HeadRecord::Absent);
        };
        match head.head_state.as_str() {
            "forked" => Ok(HeadRecord::Fork(self.load_head_fork(conn, &head)?)),
            "accepted" => Ok(HeadRecord::Accepted(
                self.load_fact_by_id(
                    conn,
                    head.head_verified_fact_id
                        .ok_or(DbError::ClosedIndeterminate)?,
                )?,
            )),
            _ => Err(DbError::ClosedIndeterminate),
        }
    }

    pub fn checkpoint_once(&mut self) -> Result<CheckpointOutcome, DbError> {
        self.ensure_writable()?;
        let current = self.load_anchor_state()?;
        if current.state == AnchorState::Pending {
            return Err(DbError::PendingCheckpoint);
        }

        let freeze = self.max_journal_sequence(self.conn()?)?;
        let next_sequence = current.checkpoint_sequence + 1;
        let root_hash = self.compute_checkpoint_root(&current.root_hash, freeze)?;
        let pending = AnchorHeadRecord {
            state: AnchorState::Pending,
            checkpoint_sequence: next_sequence,
            freeze_journal_sequence: freeze,
            root_hash,
            prepare_token: Some(self.compute_prepare_token(next_sequence, freeze, &root_hash)?),
        };

        {
            let tx = self
                .conn_mut()?
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            self.append_anchor_event(&tx, "checkpoint_prepare", &pending)?;
            self.append_anchor_event(&tx, "checkpoint_pending", &pending)?;
            self.store_anchor_state(&tx, &pending)?;
            tx.commit()?;
        }

        if !self
            .anchor_store
            .compare_and_swap_head(Some(&current), &pending)?
        {
            return Err(DbError::ClosedIndeterminate);
        }
        write_anchor_sidecar(&self.anchor_path, &pending)?;
        Ok(CheckpointOutcome {
            anchor: pending,
            created_pending: true,
        })
    }

    pub fn recover_anchor(&mut self) -> Result<AnchorHeadRecord, DbError> {
        let current = self.load_anchor_state()?;
        if current.state == AnchorState::Stable {
            return Ok(current);
        }

        let stable = AnchorHeadRecord {
            state: AnchorState::Stable,
            checkpoint_sequence: current.checkpoint_sequence,
            freeze_journal_sequence: current.freeze_journal_sequence,
            root_hash: current.root_hash,
            prepare_token: None,
        };
        {
            let tx = self
                .conn_mut()?
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            self.append_anchor_event(&tx, "checkpoint_finalize", &stable)?;
            self.store_anchor_state(&tx, &stable)?;
            tx.commit()?;
        }
        if !self
            .anchor_store
            .compare_and_swap_head(Some(&current), &stable)?
        {
            return Err(DbError::ClosedIndeterminate);
        }
        write_anchor_sidecar(&self.anchor_path, &stable)?;
        Ok(stable)
    }

    pub fn rebuild(&mut self) -> Result<(), DbError> {
        let anchor = self.load_anchor_state()?;
        if anchor.state == AnchorState::Pending {
            return Err(DbError::RebuildRefused);
        }
        let expected_store_head = match self.anchor_store.load_head()? {
            Some(head) if head == anchor => head,
            _ => return Err(DbError::ClosedIndeterminate),
        };

        self.conn()?
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        let original = self.conn.take().ok_or(DbError::ClosedIndeterminate)?;
        drop(original);

        fs::copy(&self.db_path, &self.shadow_path)?;
        if fs::read(&self.db_path)? != fs::read(&self.shadow_path)? {
            return Err(DbError::ClosedIndeterminate);
        }

        {
            let mut shadow = Connection::open(&self.shadow_path)?;
            configure_connection(&mut shadow)?;
            crate::verify_schema(&shadow)?;
            self.verify_attempt_consistency(&shadow)?;
            self.verify_journal_consistency(&shadow)?;
            self.rebuild_projection(&mut shadow)?;
        }

        replace_file_atomic(&self.db_path, &self.shadow_path)?;

        let mut reopened = Connection::open(&self.db_path)?;
        configure_connection(&mut reopened)?;
        crate::verify_schema(&reopened)?;
        self.verify_projection_consistency(&reopened)?;
        let reopened_anchor = load_anchor_state_from_conn(&reopened)?;
        let sidecar_anchor = read_anchor_sidecar(&self.anchor_path)?;
        if reopened_anchor != sidecar_anchor || reopened_anchor != anchor {
            return Err(DbError::ClosedIndeterminate);
        }
        match self.anchor_store.load_head()? {
            Some(head) if head == expected_store_head && head == anchor => {}
            _ => return Err(DbError::ClosedIndeterminate),
        }
        self.conn = Some(reopened);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn ingest_verified_for_tests(
        &mut self,
        raw: &[u8],
        verified: VerifiedEnvelope,
    ) -> Result<IngestOutcome, DbError> {
        self.ensure_writable()?;
        self.ingest_verified_internal(raw, verified)
    }

    #[cfg(test)]
    pub(crate) fn replayed_fact_matches_for_tests(
        &self,
        fact: &VerifiedFactRecord,
        verified: &VerifiedEnvelope,
    ) -> Result<(), DbError> {
        self.verify_replayed_fact(fact, verified)
    }

    pub(crate) fn ingest_verified_internal(
        &mut self,
        raw: &[u8],
        verified: VerifiedEnvelope,
    ) -> Result<IngestOutcome, DbError> {
        let prepared = PreparedAttempt::verified(raw, verified, self.raw_evidence_limit)?;
        let tx = self
            .conn_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let context = TxContext::new(&tx);
        let chain =
            context.finish(verify_chain(&context, &prepared.verified).map_err(DbError::from));
        let outcome = match chain {
            Ok(chain) => self.commit_verified_attempt(&tx, prepared, chain)?,
            Err(DbError::VerifiedSource(ErrorCode::SequenceFork)) => {
                self.commit_fork_attempt(&tx, prepared)?
            }
            Err(DbError::VerifiedSource(error)) => {
                self.commit_chain_rejection(&tx, prepared, error)?
            }
            Err(other) => return Err(other),
        };
        tx.commit()?;
        Ok(outcome)
    }

    fn commit_verified_attempt(
        &self,
        tx: &Transaction<'_>,
        prepared: PreparedAttempt,
        chain: ChainVerification,
    ) -> Result<IngestOutcome, DbError> {
        let fact_id = self.ensure_fact(tx, &prepared.verified)?;
        let attempt_id = self.insert_attempt(
            tx,
            &prepared.audit,
            AttemptDisposition::Successful {
                verified: &prepared.verified,
                verified_fact_id: Some(fact_id),
                chain_error: None,
                final_disposition: match chain.disposition {
                    SequenceDisposition::Accepted => "accepted",
                    SequenceDisposition::Duplicate => "duplicate",
                    SequenceDisposition::EquivalentVariant => "equivalent_variant",
                },
            },
        )?;

        match chain.disposition {
            SequenceDisposition::Accepted => {
                self.project_accepted(tx, &prepared.verified, fact_id)?;
                self.append_journal_event(
                    tx,
                    JournalEvent::verified(
                        "accepted",
                        attempt_id,
                        &prepared.verified,
                        fact_id,
                        None,
                    )?,
                )?;
                Ok(IngestOutcome {
                    attempt_id,
                    disposition: IngestDisposition::Accepted { fact_id },
                })
            }
            SequenceDisposition::Duplicate => {
                self.append_journal_event(
                    tx,
                    JournalEvent::verified(
                        "duplicate",
                        attempt_id,
                        &prepared.verified,
                        fact_id,
                        None,
                    )?,
                )?;
                Ok(IngestOutcome {
                    attempt_id,
                    disposition: IngestDisposition::Duplicate { fact_id },
                })
            }
            SequenceDisposition::EquivalentVariant => {
                self.append_journal_event(
                    tx,
                    JournalEvent::verified(
                        "equivalent_variant",
                        attempt_id,
                        &prepared.verified,
                        fact_id,
                        None,
                    )?,
                )?;
                Ok(IngestOutcome {
                    attempt_id,
                    disposition: IngestDisposition::EquivalentVariant { fact_id },
                })
            }
        }
    }

    fn commit_fork_attempt(
        &self,
        tx: &Transaction<'_>,
        prepared: PreparedAttempt,
    ) -> Result<IngestOutcome, DbError> {
        let fact_id = self.ensure_fact(tx, &prepared.verified)?;
        let source_id = prepared.verified.envelope.payload.source_id.as_bytes();
        let sequence = prepared.verified.envelope.payload.sequence;
        let existing_slot = self
            .load_chain_slot(tx, source_id, sequence)?
            .ok_or(DbError::ClosedIndeterminate)?;
        let original_fact_id = existing_slot
            .head_verified_fact_id
            .or(existing_slot.original_verified_fact_id)
            .ok_or(DbError::ClosedIndeterminate)?;
        let attempt_id = self.insert_attempt(
            tx,
            &prepared.audit,
            AttemptDisposition::Successful {
                verified: &prepared.verified,
                verified_fact_id: Some(fact_id),
                chain_error: Some(ErrorCode::SequenceFork),
                final_disposition: "forked",
            },
        )?;
        self.project_fork(
            tx,
            &prepared.verified,
            original_fact_id,
            fact_id,
            attempt_id,
        )?;
        self.append_journal_event(
            tx,
            JournalEvent::verified(
                "forked",
                attempt_id,
                &prepared.verified,
                fact_id,
                Some(original_fact_id),
            )?,
        )?;
        Ok(IngestOutcome {
            attempt_id,
            disposition: IngestDisposition::Forked {
                original_fact_id,
                conflict_fact_id: fact_id,
            },
        })
    }

    fn commit_chain_rejection(
        &self,
        tx: &Transaction<'_>,
        prepared: PreparedAttempt,
        error: ErrorCode,
    ) -> Result<IngestOutcome, DbError> {
        let attempt_id = self.insert_attempt(
            tx,
            &prepared.audit,
            AttemptDisposition::Successful {
                verified: &prepared.verified,
                verified_fact_id: None,
                chain_error: Some(error),
                final_disposition: "chain_rejected",
            },
        )?;
        self.append_journal_event(
            tx,
            JournalEvent::rejected(
                "chain_rejected",
                attempt_id,
                prepared.audit.raw_hash,
                Some(
                    prepared
                        .verified
                        .envelope
                        .payload
                        .source_id
                        .as_bytes()
                        .to_vec(),
                ),
                Some(prepared.verified.envelope.payload.sequence),
                Some(error),
            )?,
        )?;
        Ok(IngestOutcome {
            attempt_id,
            disposition: IngestDisposition::ChainRejected { error },
        })
    }

    fn record_parse_rejection(
        &mut self,
        raw: &[u8],
        error: ErrorCode,
    ) -> Result<IngestOutcome, DbError> {
        let audit = AttemptAudit::from_raw(raw, self.raw_evidence_limit)?;
        let tx = self
            .conn_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let attempt_id =
            self.insert_attempt(&tx, &audit, AttemptDisposition::ParseRejected(error))?;
        self.append_journal_event(
            &tx,
            JournalEvent::rejected(
                "parse_rejected",
                attempt_id,
                audit.raw_hash,
                None,
                None,
                Some(error),
            )?,
        )?;
        tx.commit()?;
        Ok(IngestOutcome {
            attempt_id,
            disposition: IngestDisposition::ParseRejected { error },
        })
    }

    pub(crate) fn ensure_fact(
        &self,
        tx: &Transaction<'_>,
        verified: &VerifiedEnvelope,
    ) -> Result<i64, DbError> {
        let source_id = verified.envelope.payload.source_id.as_bytes();
        let sequence = as_i64(verified.envelope.payload.sequence)?;
        let existing = tx
            .query_row(
                "SELECT fact_id FROM verified_facts WHERE source_id = ?1 AND sequence = ?2 AND document_digest = ?3 AND signature_set_digest = ?4",
                params![
                    source_id,
                    sequence,
                    verified.digests.document.as_bytes().as_slice(),
                    verified.digests.signature_set.as_bytes().as_slice()
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(fact_id) = existing {
            return Ok(fact_id);
        }
        tx.execute(
            "INSERT INTO verified_facts (
                 source_id, sequence, doc_kind, parent_document_digest, raw_bytes,
                 canonical_payload, canonical_root, canonical_state, canonical_signatures,
                 canonical_envelope, document_digest, root_digest, state_digest,
                 signature_set_digest, signature_message_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                source_id,
                sequence,
                doc_kind_str(verified.envelope.payload.doc_kind),
                verified
                    .envelope
                    .payload
                    .parent_document_digest
                    .map(|value| value.as_bytes().to_vec()),
                &verified.raw_bytes,
                &verified.canonical_payload,
                &verified.canonical_root,
                &verified.canonical_state,
                &verified.canonical_signatures,
                &verified.canonical_envelope,
                verified.digests.document.as_bytes().as_slice(),
                verified.digests.root.as_bytes().as_slice(),
                verified.digests.state.as_bytes().as_slice(),
                verified.digests.signature_set.as_bytes().as_slice(),
                verified.digests.signature_message.as_bytes().as_slice(),
            ],
        )?;
        Ok(tx.last_insert_rowid())
    }

    fn insert_attempt(
        &self,
        tx: &Transaction<'_>,
        audit: &AttemptAudit,
        disposition: AttemptDisposition<'_>,
    ) -> Result<i64, DbError> {
        match disposition {
            AttemptDisposition::ParseRejected(error) => {
                tx.execute(
                    "INSERT INTO ingest_attempts (
                         raw_hash, raw_total_len, raw_truncated, raw_evidence, disposition,
                         parse_error_code, chain_error_code, source_id, sequence, doc_kind,
                         parent_document_digest, canonical_payload, canonical_root,
                         canonical_state, canonical_signatures, canonical_envelope,
                         document_digest, root_digest, state_digest, signature_set_digest,
                         signature_message_digest, verified_fact_id
                     ) VALUES (?1, ?2, ?3, ?4, 'parse_rejected', ?5, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
                    params![
                        audit.raw_hash.as_slice(),
                        as_i64(audit.raw_total_len)?,
                        bool_to_i64(audit.raw_truncated),
                        &audit.raw_evidence,
                        error_code_str(error),
                    ],
                )?;
            }
            AttemptDisposition::Successful {
                verified,
                verified_fact_id,
                chain_error,
                final_disposition,
            } => {
                tx.execute(
                    "INSERT INTO ingest_attempts (
                         raw_hash, raw_total_len, raw_truncated, raw_evidence, disposition,
                         parse_error_code, chain_error_code, source_id, sequence, doc_kind,
                         parent_document_digest, canonical_payload, canonical_root,
                         canonical_state, canonical_signatures, canonical_envelope,
                         document_digest, root_digest, state_digest, signature_set_digest,
                         signature_message_digest, verified_fact_id
                     ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                    params![
                        audit.raw_hash.as_slice(),
                        as_i64(audit.raw_total_len)?,
                        bool_to_i64(audit.raw_truncated),
                        &audit.raw_evidence,
                        final_disposition,
                        chain_error.map(error_code_str),
                        verified.envelope.payload.source_id.as_bytes(),
                        as_i64(verified.envelope.payload.sequence)?,
                        doc_kind_str(verified.envelope.payload.doc_kind),
                        verified.envelope.payload.parent_document_digest.map(|value| value.as_bytes().to_vec()),
                        &verified.canonical_payload,
                        &verified.canonical_root,
                        &verified.canonical_state,
                        &verified.canonical_signatures,
                        &verified.canonical_envelope,
                        verified.digests.document.as_bytes().as_slice(),
                        verified.digests.root.as_bytes().as_slice(),
                        verified.digests.state.as_bytes().as_slice(),
                        verified.digests.signature_set.as_bytes().as_slice(),
                        verified.digests.signature_message.as_bytes().as_slice(),
                        verified_fact_id,
                    ],
                )?;
            }
        }
        Ok(tx.last_insert_rowid())
    }

    fn append_journal_event(
        &self,
        tx: &Transaction<'_>,
        event: JournalEvent,
    ) -> Result<u64, DbError> {
        tx.execute(
            "INSERT INTO journal_events (
                 event_type, attempt_id, source_id, sequence, verified_fact_id,
                 related_verified_fact_id, error_code, event_root
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                event.event_type,
                event.attempt_id,
                event.source_id,
                event.sequence.map(as_i64).transpose()?,
                event.verified_fact_id,
                event.related_verified_fact_id,
                event.error_code.map(error_code_str),
                event.event_root.as_slice(),
            ],
        )?;
        checked_i64_to_u64(tx.last_insert_rowid())
    }

    fn append_anchor_event(
        &self,
        tx: &Transaction<'_>,
        event_type: &'static str,
        anchor: &AnchorHeadRecord,
    ) -> Result<u64, DbError> {
        self.append_journal_event(
            tx,
            JournalEvent {
                event_type,
                attempt_id: None,
                source_id: None,
                sequence: Some(anchor.checkpoint_sequence),
                verified_fact_id: None,
                related_verified_fact_id: None,
                error_code: None,
                event_root: hash_parts(
                    "evidence-db.anchor-event",
                    &[
                        event_type.as_bytes(),
                        &anchor.checkpoint_sequence.to_be_bytes(),
                        &anchor.freeze_journal_sequence.to_be_bytes(),
                        &anchor.root_hash,
                        anchor.prepare_token.as_ref().map_or(&[][..], |value| value),
                    ],
                )?,
            },
        )
    }

    fn project_accepted(
        &self,
        tx: &Transaction<'_>,
        verified: &VerifiedEnvelope,
        fact_id: i64,
    ) -> Result<(), DbError> {
        let source_id = verified.envelope.payload.source_id.as_bytes();
        let sequence = as_i64(verified.envelope.payload.sequence)?;
        let existing = self.load_chain_slot(tx, source_id, verified.envelope.payload.sequence)?;
        match existing {
            None => {
                tx.execute(
                    "INSERT INTO chain_slots (
                         source_id, sequence, slot_state, head_verified_fact_id,
                         original_verified_fact_id, conflict_verified_fact_id, decisive_attempt_id
                     ) VALUES (?1, ?2, 'accepted', ?3, NULL, NULL, NULL)",
                    params![source_id, sequence, fact_id],
                )?;
            }
            Some(slot) if slot.slot_state == "accepted" => {}
            Some(_) => return Err(DbError::ClosedIndeterminate),
        }
        self.advance_source_head(tx, source_id, sequence, fact_id)?;
        Ok(())
    }

    fn project_fork(
        &self,
        tx: &Transaction<'_>,
        verified: &VerifiedEnvelope,
        original_fact_id: i64,
        conflict_fact_id: i64,
        decisive_attempt_id: i64,
    ) -> Result<(), DbError> {
        let source_id = verified.envelope.payload.source_id.as_bytes();
        let sequence = as_i64(verified.envelope.payload.sequence)?;
        let slot = self
            .load_chain_slot(tx, source_id, verified.envelope.payload.sequence)?
            .ok_or(DbError::ClosedIndeterminate)?;
        match slot.slot_state.as_str() {
            "accepted" => {
                if slot.head_verified_fact_id != Some(original_fact_id) {
                    return Err(DbError::ClosedIndeterminate);
                }
                let changed = tx.execute(
                    "UPDATE chain_slots
                     SET slot_state = 'forked',
                         head_verified_fact_id = NULL,
                         original_verified_fact_id = ?3,
                         conflict_verified_fact_id = ?4,
                         decisive_attempt_id = ?5
                     WHERE source_id = ?1 AND sequence = ?2 AND slot_state = 'accepted'",
                    params![
                        source_id,
                        sequence,
                        original_fact_id,
                        conflict_fact_id,
                        decisive_attempt_id
                    ],
                )?;
                if changed != 1 {
                    return Err(DbError::ClosedIndeterminate);
                }
                self.mark_source_forked(tx, source_id, sequence)?;
            }
            "forked" => {
                if slot.original_verified_fact_id != Some(original_fact_id)
                    || slot.conflict_verified_fact_id.is_none()
                    || slot.decisive_attempt_id.is_none()
                {
                    return Err(DbError::ClosedIndeterminate);
                }
                match self.load_source_head(tx, source_id)? {
                    Some(head)
                        if head.head_state == "forked"
                            && head.forked_slot_sequence == Some(sequence)
                            && head.head_verified_fact_id.is_none()
                            && head.head_sequence >= sequence => {}
                    _ => return Err(DbError::ClosedIndeterminate),
                }
            }
            _ => return Err(DbError::ClosedIndeterminate),
        }
        Ok(())
    }

    fn advance_source_head(
        &self,
        tx: &Transaction<'_>,
        source_id: &[u8],
        sequence: i64,
        fact_id: i64,
    ) -> Result<(), DbError> {
        match self.load_source_head(tx, source_id)? {
            None => {
                tx.execute(
                    "INSERT INTO source_heads (
                         source_id, head_state, head_sequence, head_verified_fact_id, forked_slot_sequence
                     ) VALUES (?1, 'accepted', ?2, ?3, NULL)",
                    params![source_id, sequence, fact_id],
                )?;
            }
            Some(head) if head.head_state == "accepted" && sequence > head.head_sequence => {
                tx.execute(
                    "UPDATE source_heads
                     SET head_sequence = ?2, head_verified_fact_id = ?3
                     WHERE source_id = ?1",
                    params![source_id, sequence, fact_id],
                )?;
            }
            Some(_) => {}
        }
        Ok(())
    }

    fn mark_source_forked(
        &self,
        tx: &Transaction<'_>,
        source_id: &[u8],
        sequence: i64,
    ) -> Result<(), DbError> {
        match self.load_source_head(tx, source_id)? {
            None => {
                tx.execute(
                    "INSERT INTO source_heads (
                         source_id, head_state, head_sequence, head_verified_fact_id, forked_slot_sequence
                     ) VALUES (?1, 'forked', ?2, NULL, ?2)",
                    params![source_id, sequence],
                )?;
            }
            Some(head) if head.head_state == "accepted" => {
                tx.execute(
                    "UPDATE source_heads
                     SET head_state = 'forked',
                         head_sequence = ?2,
                         head_verified_fact_id = NULL,
                         forked_slot_sequence = ?3
                     WHERE source_id = ?1",
                    params![source_id, max(head.head_sequence, sequence), sequence],
                )?;
            }
            Some(head) if head.head_state == "forked" => {
                if head.forked_slot_sequence != Some(sequence)
                    || head.head_verified_fact_id.is_some()
                    || head.head_sequence < sequence
                {
                    return Err(DbError::ClosedIndeterminate);
                }
            }
            Some(_) => return Err(DbError::ClosedIndeterminate),
        }
        Ok(())
    }

    pub(crate) fn compute_checkpoint_root(
        &self,
        previous_root: &[u8; 32],
        freeze: u64,
    ) -> Result<[u8; 32], DbError> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT journal_seq, event_root
             FROM journal_events
             WHERE journal_seq <= ?1
             ORDER BY journal_seq ASC",
        )?;
        let mut rows = statement.query(params![as_i64(freeze)?])?;
        let mut encoded_parts = vec![previous_root.to_vec(), freeze.to_be_bytes().to_vec()];
        while let Some(row) = rows.next()? {
            let journal_seq =
                u64::try_from(row.get::<_, i64>(0)?).map_err(|_| DbError::ClosedIndeterminate)?;
            let root = row.get::<_, Vec<u8>>(1)?;
            encoded_parts.push(journal_seq.to_be_bytes().to_vec());
            encoded_parts.push(root);
        }
        let refs = encoded_parts.iter().map(Vec::as_slice).collect::<Vec<_>>();
        hash_parts("evidence-db.checkpoint-root", &refs)
    }

    pub(crate) fn compute_prepare_token(
        &self,
        checkpoint_sequence: u64,
        freeze: u64,
        root_hash: &[u8; 32],
    ) -> Result<[u8; 32], DbError> {
        hash_parts(
            "evidence-db.prepare-token",
            &[
                self.db_path.to_string_lossy().as_bytes(),
                &checkpoint_sequence.to_be_bytes(),
                &freeze.to_be_bytes(),
                root_hash,
            ],
        )
    }

    pub(crate) fn max_journal_sequence(&self, conn: &Connection) -> Result<u64, DbError> {
        let value = conn.query_row(
            "SELECT COALESCE(MAX(journal_seq), 0) FROM journal_events",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        u64::try_from(value).map_err(|_| DbError::ClosedIndeterminate)
    }

    fn verify_attempt_consistency(&self, conn: &Connection) -> Result<(), DbError> {
        let mut statement = conn.prepare(
            "SELECT attempt_id, raw_hash, raw_total_len, raw_truncated, raw_evidence, disposition,
                    parse_error_code, chain_error_code, source_id, sequence, doc_kind,
                    parent_document_digest, canonical_payload, canonical_root, canonical_state,
                    canonical_signatures, canonical_envelope, document_digest, root_digest,
                    state_digest, signature_set_digest, signature_message_digest, verified_fact_id
             FROM ingest_attempts
             ORDER BY attempt_id ASC",
        )?;
        let mut rows = statement.query([])?;
        let mut fact_cache = HashMap::new();
        while let Some(row) = rows.next()? {
            let attempt = map_attempt_row(row).map_err(|_| DbError::ClosedIndeterminate)?;
            self.validate_attempt_row(conn, &attempt, &mut fact_cache)?;
        }
        Ok(())
    }

    fn validate_attempt_row(
        &self,
        conn: &Connection,
        attempt: &AttemptRow,
        fact_cache: &mut HashMap<i64, VerifiedFactRecord>,
    ) -> Result<(), DbError> {
        self.verify_attempt_raw_audit(attempt, None)?;
        match attempt.disposition.as_str() {
            "parse_rejected" => {
                let _ = parse_error_code(
                    attempt
                        .parse_error_code
                        .as_deref()
                        .ok_or(DbError::ClosedIndeterminate)?,
                )?;
                if attempt.verified_fact_id.is_some() {
                    return Err(DbError::ClosedIndeterminate);
                }
                if attempt.parse_error_code.is_none()
                    || attempt.chain_error_code.is_some()
                    || attempt.source_id.is_some()
                    || attempt.sequence.is_some()
                    || attempt.doc_kind.is_some()
                    || attempt.parent_document_digest.is_some()
                    || attempt.canonical_payload.is_some()
                    || attempt.canonical_root.is_some()
                    || attempt.canonical_state.is_some()
                    || attempt.canonical_signatures.is_some()
                    || attempt.canonical_envelope.is_some()
                    || attempt.document_digest.is_some()
                    || attempt.root_digest.is_some()
                    || attempt.state_digest.is_some()
                    || attempt.signature_set_digest.is_some()
                    || attempt.signature_message_digest.is_some()
                {
                    return Err(DbError::ClosedIndeterminate);
                }
            }
            "accepted" | "duplicate" | "equivalent_variant" | "forked" => {
                let fact_id = attempt
                    .verified_fact_id
                    .ok_or(DbError::ClosedIndeterminate)?;
                let fact = self.load_cached_fact(conn, fact_id, fact_cache)?;
                if attempt.parse_error_code.is_some() {
                    return Err(DbError::ClosedIndeterminate);
                }
                if attempt.disposition == "forked" {
                    if self.recorded_chain_error(attempt)? != ErrorCode::SequenceFork {
                        return Err(DbError::ClosedIndeterminate);
                    }
                } else if attempt.chain_error_code.is_some() {
                    return Err(DbError::ClosedIndeterminate);
                }
                self.verify_attempt_matches_fact(attempt, &fact)?;
            }
            "chain_rejected" => {
                let chain_error = self.recorded_chain_error(attempt)?;
                if attempt.verified_fact_id.is_some()
                    || attempt.parse_error_code.is_some()
                    || attempt.chain_error_code.is_none()
                    || chain_error == ErrorCode::SequenceFork
                    || attempt.source_id.is_none()
                    || attempt.sequence.is_none()
                    || attempt.doc_kind.is_none()
                    || attempt.canonical_payload.is_none()
                    || attempt.canonical_root.is_none()
                    || attempt.canonical_state.is_none()
                    || attempt.canonical_signatures.is_none()
                    || attempt.canonical_envelope.is_none()
                    || attempt.document_digest.is_none()
                    || attempt.root_digest.is_none()
                    || attempt.state_digest.is_none()
                    || attempt.signature_set_digest.is_none()
                    || attempt.signature_message_digest.is_none()
                {
                    return Err(DbError::ClosedIndeterminate);
                }
            }
            _ => return Err(DbError::ClosedIndeterminate),
        }
        Ok(())
    }

    fn verify_attempt_raw_audit(
        &self,
        attempt: &AttemptRow,
        full_raw: Option<&[u8]>,
    ) -> Result<(), DbError> {
        if attempt.raw_evidence.len() > self.raw_evidence_limit {
            return Err(DbError::ClosedIndeterminate);
        }
        let raw_evidence_len = checked_usize_to_u64(attempt.raw_evidence.len())?;
        if attempt.raw_truncated {
            if attempt.raw_total_len <= raw_evidence_len
                || attempt.raw_evidence.len() != self.raw_evidence_limit
            {
                return Err(DbError::ClosedIndeterminate);
            }
        } else if attempt.raw_total_len != raw_evidence_len
            || hash_parts("evidence-db.raw", &[attempt.raw_evidence.as_slice()])?
                != attempt.raw_hash
        {
            return Err(DbError::ClosedIndeterminate);
        }

        if let Some(raw) = full_raw {
            if hash_parts("evidence-db.raw", &[raw])? != attempt.raw_hash
                || attempt.raw_total_len != checked_usize_to_u64(raw.len())?
                || attempt.raw_truncated != (raw.len() > self.raw_evidence_limit)
                || attempt.raw_evidence != raw[..raw.len().min(self.raw_evidence_limit)].to_vec()
            {
                return Err(DbError::ClosedIndeterminate);
            }
        }
        Ok(())
    }

    fn require_complete_attempt_raw<'a>(
        &self,
        attempt: &'a AttemptRow,
    ) -> Result<&'a [u8], DbError> {
        if attempt.raw_truncated {
            return Err(DbError::ClosedIndeterminate);
        }
        Ok(attempt.raw_evidence.as_slice())
    }

    fn recorded_parse_error(&self, attempt: &AttemptRow) -> Result<ErrorCode, DbError> {
        parse_error_code(
            attempt
                .parse_error_code
                .as_deref()
                .ok_or(DbError::ClosedIndeterminate)?,
        )
    }

    fn recorded_chain_error(&self, attempt: &AttemptRow) -> Result<ErrorCode, DbError> {
        parse_error_code(
            attempt
                .chain_error_code
                .as_deref()
                .ok_or(DbError::ClosedIndeterminate)?,
        )
    }

    fn verify_attempt_matches_verified(
        &self,
        attempt: &AttemptRow,
        verified: &VerifiedEnvelope,
    ) -> Result<(), DbError> {
        self.verify_attempt_raw_audit(attempt, Some(&verified.raw_bytes))?;
        if attempt.source_id.as_deref() != Some(verified.envelope.payload.source_id.as_bytes())
            || attempt.sequence != Some(verified.envelope.payload.sequence)
            || attempt.doc_kind != Some(verified.envelope.payload.doc_kind)
            || attempt.parent_document_digest != verified.envelope.payload.parent_document_digest
            || attempt.canonical_payload.as_deref() != Some(verified.canonical_payload.as_slice())
            || attempt.canonical_root.as_deref() != Some(verified.canonical_root.as_slice())
            || attempt.canonical_state.as_deref() != Some(verified.canonical_state.as_slice())
            || attempt.canonical_signatures.as_deref()
                != Some(verified.canonical_signatures.as_slice())
            || attempt.canonical_envelope.as_deref() != Some(verified.canonical_envelope.as_slice())
            || attempt.document_digest != Some(verified.digests.document)
            || attempt.root_digest != Some(verified.digests.root)
            || attempt.state_digest != Some(verified.digests.state)
            || attempt.signature_set_digest != Some(verified.digests.signature_set)
            || attempt.signature_message_digest != Some(verified.digests.signature_message)
        {
            return Err(DbError::ClosedIndeterminate);
        }
        Ok(())
    }

    fn verify_attempt_matches_fact(
        &self,
        attempt: &AttemptRow,
        fact: &VerifiedFactRecord,
    ) -> Result<(), DbError> {
        self.verify_attempt_raw_audit(attempt, Some(&fact.raw_bytes))?;
        if attempt.source_id.as_deref() != Some(fact.source_id.as_slice())
            || attempt.sequence != Some(fact.sequence)
            || attempt.doc_kind.as_ref() != Some(&fact.doc_kind)
            || attempt.parent_document_digest.as_ref() != fact.parent_document_digest.as_ref()
            || attempt.canonical_payload.as_deref() != Some(fact.canonical_payload.as_slice())
            || attempt.canonical_root.as_deref() != Some(fact.canonical_root.as_slice())
            || attempt.canonical_state.as_deref() != Some(fact.canonical_state.as_slice())
            || attempt.canonical_signatures.as_deref() != Some(fact.canonical_signatures.as_slice())
            || attempt.canonical_envelope.as_deref() != Some(fact.canonical_envelope.as_slice())
            || attempt.document_digest.as_ref() != Some(&fact.digests.document)
            || attempt.root_digest.as_ref() != Some(&fact.digests.root)
            || attempt.state_digest.as_ref() != Some(&fact.digests.state)
            || attempt.signature_set_digest.as_ref() != Some(&fact.digests.signature_set)
            || attempt.signature_message_digest.as_ref() != Some(&fact.digests.signature_message)
        {
            return Err(DbError::ClosedIndeterminate);
        }
        Ok(())
    }

    fn verify_journal_consistency(&self, conn: &Connection) -> Result<(), DbError> {
        let mut events = Vec::new();
        {
            let mut statement = conn.prepare(
                "SELECT journal_seq, event_type, attempt_id, source_id, sequence, verified_fact_id,
                        related_verified_fact_id, error_code, event_root
                 FROM journal_events
                 ORDER BY journal_seq ASC",
            )?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                events.push(map_journal_row(row).map_err(|_| DbError::ClosedIndeterminate)?);
            }
        }

        let mut attempts = HashMap::new();
        let mut facts = HashMap::new();
        let mut replay = JournalReplayState::default();
        let mut last_seq = 0u64;
        for event in events {
            if event.journal_seq <= last_seq {
                return Err(DbError::ClosedIndeterminate);
            }
            last_seq = event.journal_seq;
            self.verify_journal_row(conn, &event, &mut attempts, &mut facts, &mut replay)?;
        }
        Ok(())
    }

    fn verify_journal_row(
        &self,
        conn: &Connection,
        event: &JournalRow,
        attempts: &mut HashMap<i64, AttemptRow>,
        facts: &mut HashMap<i64, VerifiedFactRecord>,
        replay: &mut JournalReplayState,
    ) -> Result<(), DbError> {
        match event.event_type.as_str() {
            "accepted" | "duplicate" | "equivalent_variant" | "forked" => {
                self.verify_verified_event(conn, event, attempts, facts, replay)
            }
            "parse_rejected" | "chain_rejected" => {
                self.verify_rejected_event(conn, event, attempts, replay)
            }
            "checkpoint_prepare" | "checkpoint_pending" | "checkpoint_finalize" => {
                self.verify_checkpoint_event(event)
            }
            _ => Err(DbError::ClosedIndeterminate),
        }
    }

    fn verify_verified_event(
        &self,
        conn: &Connection,
        event: &JournalRow,
        attempts: &mut HashMap<i64, AttemptRow>,
        facts: &mut HashMap<i64, VerifiedFactRecord>,
        replay: &mut JournalReplayState,
    ) -> Result<(), DbError> {
        let attempt = self.load_cached_attempt(conn, event.attempt_id, attempts)?;
        let fact = self.load_cached_fact(
            conn,
            event.verified_fact_id.ok_or(DbError::ClosedIndeterminate)?,
            facts,
        )?;
        if attempt.disposition != event.event_type
            || attempt.verified_fact_id != event.verified_fact_id
            || attempt.source_id != event.source_id
            || attempt.sequence != event.sequence
            || event.source_id.as_deref() != Some(fact.source_id.as_slice())
            || event.sequence != Some(fact.sequence)
            || event.error_code.is_some()
            || event.event_root
                != self.compute_verified_event_root(
                    &event.event_type,
                    &fact,
                    event.related_verified_fact_id,
                )?
        {
            return Err(DbError::ClosedIndeterminate);
        }

        let verified = self.reparse_verified_fact(&fact)?;
        self.verify_attempt_matches_verified(&attempt, &verified)?;

        match event.event_type.as_str() {
            "accepted" => {
                if event.related_verified_fact_id.is_some() {
                    return Err(DbError::ClosedIndeterminate);
                }
                let chain = verify_chain(replay, &verified).map_err(DbError::from)?;
                if chain.disposition != SequenceDisposition::Accepted {
                    return Err(DbError::ClosedIndeterminate);
                }
                self.replay_accepted_event(&fact, &verified, replay)
            }
            "duplicate" => {
                if event.related_verified_fact_id.is_some() {
                    return Err(DbError::ClosedIndeterminate);
                }
                let chain = verify_chain(replay, &verified).map_err(DbError::from)?;
                if chain.disposition != SequenceDisposition::Duplicate {
                    return Err(DbError::ClosedIndeterminate);
                }
                self.replay_duplicate_event(&verified, replay)
            }
            "equivalent_variant" => {
                if event.related_verified_fact_id.is_some() {
                    return Err(DbError::ClosedIndeterminate);
                }
                let chain = verify_chain(replay, &verified).map_err(DbError::from)?;
                if chain.disposition != SequenceDisposition::EquivalentVariant {
                    return Err(DbError::ClosedIndeterminate);
                }
                self.replay_equivalent_event(&verified, replay)
            }
            "forked" => {
                let related_fact = self.load_cached_fact(
                    conn,
                    event
                        .related_verified_fact_id
                        .ok_or(DbError::ClosedIndeterminate)?,
                    facts,
                )?;
                if related_fact.source_id != fact.source_id
                    || related_fact.sequence != fact.sequence
                    || related_fact.fact_id == fact.fact_id
                {
                    return Err(DbError::ClosedIndeterminate);
                }
                match verify_chain(replay, &verified) {
                    Err(ErrorCode::SequenceFork) => {
                        self.replay_fork_event(event, &verified, replay)
                    }
                    _ => Err(DbError::ClosedIndeterminate),
                }
            }
            _ => Err(DbError::ClosedIndeterminate),
        }
    }

    fn verify_rejected_event(
        &self,
        conn: &Connection,
        event: &JournalRow,
        attempts: &mut HashMap<i64, AttemptRow>,
        replay: &mut JournalReplayState,
    ) -> Result<(), DbError> {
        let attempt = self.load_cached_attempt(conn, event.attempt_id, attempts)?;
        match event.event_type.as_str() {
            "parse_rejected" => {
                let error = self.recorded_parse_error(&attempt)?;
                if attempt.disposition != "parse_rejected"
                    || event.error_code.as_deref() != Some(error_code_str(error))
                    || attempt.source_id.is_some()
                    || attempt.sequence.is_some()
                    || attempt.verified_fact_id.is_some()
                    || event.source_id.is_some()
                    || event.sequence.is_some()
                    || event.verified_fact_id.is_some()
                    || event.related_verified_fact_id.is_some()
                {
                    return Err(DbError::ClosedIndeterminate);
                }
                match parse_and_verify_envelope(self.require_complete_attempt_raw(&attempt)?) {
                    Err(actual) if actual == error => {}
                    _ => return Err(DbError::ClosedIndeterminate),
                }
            }
            "chain_rejected" => {
                let error = self.recorded_chain_error(&attempt)?;
                if attempt.disposition != "chain_rejected"
                    || event.error_code.as_deref() != Some(error_code_str(error))
                    || attempt.source_id != event.source_id
                    || attempt.sequence != event.sequence
                    || attempt.verified_fact_id.is_some()
                    || event.verified_fact_id.is_some()
                    || event.related_verified_fact_id.is_some()
                {
                    return Err(DbError::ClosedIndeterminate);
                }
                let verified =
                    parse_and_verify_envelope(self.require_complete_attempt_raw(&attempt)?)
                        .map_err(|_| DbError::ClosedIndeterminate)?;
                self.verify_attempt_matches_verified(&attempt, &verified)?;
                match verify_chain(replay, &verified) {
                    Err(actual) if actual == error => {}
                    _ => return Err(DbError::ClosedIndeterminate),
                }
            }
            _ => return Err(DbError::ClosedIndeterminate),
        }

        if event.event_root
            != self.compute_rejected_event_root(
                &event.event_type,
                &attempt.raw_hash,
                event.source_id.as_deref(),
                event.sequence,
                event.error_code.as_deref(),
            )?
        {
            return Err(DbError::ClosedIndeterminate);
        }
        Ok(())
    }

    fn verify_checkpoint_event(&self, event: &JournalRow) -> Result<(), DbError> {
        if event.attempt_id.is_some()
            || event.source_id.is_some()
            || event.sequence.is_none()
            || event.verified_fact_id.is_some()
            || event.related_verified_fact_id.is_some()
            || event.error_code.is_some()
        {
            return Err(DbError::ClosedIndeterminate);
        }
        Ok(())
    }

    fn replay_accepted_event(
        &self,
        fact: &VerifiedFactRecord,
        verified: &VerifiedEnvelope,
        replay: &mut JournalReplayState,
    ) -> Result<(), DbError> {
        let source_id = fact.source_id.as_slice();
        let sequence = fact.sequence;
        if matches!(
            JournalReplayState::lookup(replay, source_id, sequence),
            ReplayLookup::Fork | ReplayLookup::Accepted { .. }
        ) {
            return Err(DbError::ClosedIndeterminate);
        }
        if let Some(head) = replay.heads.get(source_id) {
            if head.state == ReplayHeadState::Forked
                || sequence
                    != head
                        .head_sequence
                        .checked_add(1)
                        .ok_or(DbError::ClosedIndeterminate)?
            {
                return Err(DbError::ClosedIndeterminate);
            }
        } else if sequence != 0 {
            return Err(DbError::ClosedIndeterminate);
        }

        replay.slots.insert(
            JournalReplayState::slot_key(source_id, sequence),
            ReplaySlot::Verified {
                head_fact_id: fact.fact_id,
                variants: vec![JournalReplayState::variant_from_verified(verified)],
            },
        );
        replay.heads.insert(
            fact.source_id.clone(),
            ReplayHead {
                state: ReplayHeadState::Accepted,
                head_sequence: sequence,
                forked_slot_sequence: None,
            },
        );
        Ok(())
    }

    fn replay_duplicate_event(
        &self,
        verified: &VerifiedEnvelope,
        replay: &mut JournalReplayState,
    ) -> Result<(), DbError> {
        match replay.slots.get(&JournalReplayState::slot_key(
            verified.envelope.payload.source_id.as_bytes(),
            verified.envelope.payload.sequence,
        )) {
            Some(ReplaySlot::Verified { variants, .. })
                if variants.iter().any(|variant| {
                    variant.document_digest == verified.digests.document
                        && variant.signature_set_digest == verified.digests.signature_set
                }) =>
            {
                Ok(())
            }
            _ => Err(DbError::ClosedIndeterminate),
        }
    }

    fn replay_equivalent_event(
        &self,
        verified: &VerifiedEnvelope,
        replay: &mut JournalReplayState,
    ) -> Result<(), DbError> {
        match replay.slots.get_mut(&JournalReplayState::slot_key(
            verified.envelope.payload.source_id.as_bytes(),
            verified.envelope.payload.sequence,
        )) {
            Some(ReplaySlot::Verified { variants, .. }) => {
                let Some(first) = variants.first() else {
                    return Err(DbError::ClosedIndeterminate);
                };
                if first.document_digest != verified.digests.document
                    || variants.iter().any(|variant| {
                        variant.signature_set_digest == verified.digests.signature_set
                    })
                {
                    return Err(DbError::ClosedIndeterminate);
                }
                variants.push(JournalReplayState::variant_from_verified(verified));
                Ok(())
            }
            _ => Err(DbError::ClosedIndeterminate),
        }
    }

    fn replay_fork_event(
        &self,
        event: &JournalRow,
        conflict_verified: &VerifiedEnvelope,
        replay: &mut JournalReplayState,
    ) -> Result<(), DbError> {
        let source_id = conflict_verified.envelope.payload.source_id.as_bytes();
        let sequence = conflict_verified.envelope.payload.sequence;
        let original_fact_id = event
            .related_verified_fact_id
            .ok_or(DbError::ClosedIndeterminate)?;
        let key = JournalReplayState::slot_key(source_id, sequence);
        match replay.slots.get(&key).cloned() {
            Some(ReplaySlot::Verified {
                head_fact_id,
                variants,
            }) => {
                let Some(first) = variants.first() else {
                    return Err(DbError::ClosedIndeterminate);
                };
                if head_fact_id != original_fact_id
                    || first.document_digest == conflict_verified.digests.document
                {
                    return Err(DbError::ClosedIndeterminate);
                }
                let Some(head) = replay.heads.get(source_id).cloned() else {
                    return Err(DbError::ClosedIndeterminate);
                };
                if head.state != ReplayHeadState::Accepted || sequence > head.head_sequence {
                    return Err(DbError::ClosedIndeterminate);
                }
                replay.slots.insert(
                    key,
                    ReplaySlot::Forked {
                        original_fact_id,
                        original_document_digest: first.document_digest,
                    },
                );
                replay.heads.insert(
                    source_id.to_vec(),
                    ReplayHead {
                        state: ReplayHeadState::Forked,
                        head_sequence: max(head.head_sequence, sequence),
                        forked_slot_sequence: Some(sequence),
                    },
                );
                Ok(())
            }
            Some(ReplaySlot::Forked {
                original_fact_id: current_original_fact_id,
                original_document_digest,
            }) => {
                let Some(head) = replay.heads.get(source_id) else {
                    return Err(DbError::ClosedIndeterminate);
                };
                if current_original_fact_id != original_fact_id
                    || original_document_digest == conflict_verified.digests.document
                    || head.state != ReplayHeadState::Forked
                    || head.forked_slot_sequence != Some(sequence)
                    || head.head_sequence < sequence
                {
                    return Err(DbError::ClosedIndeterminate);
                }
                Ok(())
            }
            None => Err(DbError::ClosedIndeterminate),
        }
    }

    fn load_cached_attempt(
        &self,
        conn: &Connection,
        attempt_id: Option<i64>,
        attempts: &mut HashMap<i64, AttemptRow>,
    ) -> Result<AttemptRow, DbError> {
        let attempt_id = attempt_id.ok_or(DbError::ClosedIndeterminate)?;
        if let Some(attempt) = attempts.get(&attempt_id) {
            return Ok(attempt.clone());
        }
        let mut statement = conn.prepare(
            "SELECT attempt_id, raw_hash, raw_total_len, raw_truncated, raw_evidence, disposition,
                    parse_error_code, chain_error_code, source_id, sequence, doc_kind,
                    parent_document_digest, canonical_payload, canonical_root, canonical_state,
                    canonical_signatures, canonical_envelope, document_digest, root_digest,
                    state_digest, signature_set_digest, signature_message_digest, verified_fact_id
             FROM ingest_attempts
             WHERE attempt_id = ?1",
        )?;
        let mut rows = statement.query(params![attempt_id])?;
        let Some(row) = rows.next()? else {
            return Err(DbError::ClosedIndeterminate);
        };
        let attempt = map_attempt_row(row).map_err(|_| DbError::ClosedIndeterminate)?;
        attempts.insert(attempt_id, attempt.clone());
        Ok(attempt)
    }

    fn load_cached_fact(
        &self,
        conn: &Connection,
        fact_id: i64,
        facts: &mut HashMap<i64, VerifiedFactRecord>,
    ) -> Result<VerifiedFactRecord, DbError> {
        if let Some(fact) = facts.get(&fact_id) {
            return Ok(fact.clone());
        }
        let fact = match self.load_fact_by_id(conn, fact_id) {
            Ok(fact) => fact,
            Err(DbError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => {
                return Err(DbError::ClosedIndeterminate);
            }
            Err(other) => return Err(other),
        };
        facts.insert(fact_id, fact.clone());
        Ok(fact)
    }

    fn compute_verified_event_root(
        &self,
        event_type: &str,
        fact: &VerifiedFactRecord,
        related_verified_fact_id: Option<i64>,
    ) -> Result<[u8; 32], DbError> {
        let mut related_buffer = [0u8; 8];
        if let Some(value) = related_verified_fact_id {
            related_buffer.copy_from_slice(&checked_i64_to_u64(value)?.to_be_bytes());
        }
        hash_parts(
            "evidence-db.journal-event",
            &[
                event_type.as_bytes(),
                fact.source_id.as_slice(),
                &fact.sequence.to_be_bytes(),
                fact.digests.document.as_bytes(),
                fact.digests.signature_set.as_bytes(),
                &related_buffer,
            ],
        )
    }

    fn compute_rejected_event_root(
        &self,
        event_type: &str,
        raw_hash: &[u8; 32],
        source_id: Option<&[u8]>,
        sequence: Option<u64>,
        error_code: Option<&str>,
    ) -> Result<[u8; 32], DbError> {
        let empty_source = Vec::new();
        let source_bytes = source_id.unwrap_or(empty_source.as_slice());
        let sequence_bytes = sequence.unwrap_or(0).to_be_bytes();
        let error_bytes = error_code.unwrap_or("none").as_bytes().to_vec();
        hash_parts(
            "evidence-db.journal-event",
            &[
                event_type.as_bytes(),
                source_bytes,
                &sequence_bytes,
                raw_hash,
                &error_bytes,
            ],
        )
    }

    fn rebuild_projection(&self, shadow: &mut Connection) -> Result<(), DbError> {
        let tx = shadow.transaction_with_behavior(TransactionBehavior::Immediate)?;
        recreate_projection_schema(&tx)?;
        let mut verified_cache = HashMap::new();

        let mut events = Vec::new();
        {
            let mut statement = tx.prepare(
                "SELECT event_type, attempt_id, source_id, sequence, verified_fact_id, related_verified_fact_id
                 FROM journal_events
                 ORDER BY journal_seq ASC",
            )?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                events.push(ProjectionReplay {
                    event_type: row.get(0)?,
                    attempt_id: row.get(1)?,
                    source_id: row.get(2)?,
                    sequence: row
                        .get::<_, Option<i64>>(3)?
                        .map(|value| u64::try_from(value).map_err(|_| DbError::ClosedIndeterminate))
                        .transpose()?,
                    verified_fact_id: row.get(4)?,
                    related_verified_fact_id: row.get(5)?,
                });
            }
        }

        for event in events {
            match event.event_type.as_str() {
                "accepted" => {
                    let fact_id = event.verified_fact_id.ok_or(DbError::ClosedIndeterminate)?;
                    let verified = self.load_rebuild_verified(&tx, fact_id, &mut verified_cache)?;
                    let fact = self.load_fact_by_id(&tx, fact_id)?;
                    self.project_accepted(&tx, &verified, fact.fact_id)?;
                }
                "forked" => {
                    let conflict_fact_id =
                        event.verified_fact_id.ok_or(DbError::ClosedIndeterminate)?;
                    let verified =
                        self.load_rebuild_verified(&tx, conflict_fact_id, &mut verified_cache)?;
                    let conflict = self.load_fact_by_id(&tx, conflict_fact_id)?;
                    self.project_fork(
                        &tx,
                        &verified,
                        event
                            .related_verified_fact_id
                            .ok_or(DbError::ClosedIndeterminate)?,
                        conflict.fact_id,
                        event.attempt_id.ok_or(DbError::ClosedIndeterminate)?,
                    )?;
                }
                _ => {}
            }
        }
        self.verify_projection_consistency(&tx)?;
        tx.commit()?;
        Ok(())
    }

    fn load_rebuild_verified(
        &self,
        conn: &Connection,
        fact_id: i64,
        verified_cache: &mut HashMap<i64, VerifiedEnvelope>,
    ) -> Result<VerifiedEnvelope, DbError> {
        if let Some(verified) = verified_cache.get(&fact_id) {
            return Ok(verified.clone());
        }
        let fact = self.load_fact_by_id(conn, fact_id)?;
        let verified = self.reparse_verified_fact(&fact)?;
        verified_cache.insert(fact_id, verified.clone());
        Ok(verified)
    }

    fn reparse_verified_fact(
        &self,
        fact: &VerifiedFactRecord,
    ) -> Result<VerifiedEnvelope, DbError> {
        let verified =
            parse_and_verify_envelope(&fact.raw_bytes).map_err(|_| DbError::ClosedIndeterminate)?;
        self.verify_replayed_fact(fact, &verified)?;
        Ok(verified)
    }

    fn verify_replayed_fact(
        &self,
        fact: &VerifiedFactRecord,
        verified: &VerifiedEnvelope,
    ) -> Result<(), DbError> {
        if verified.raw_bytes != fact.raw_bytes
            || verified.envelope.payload.source_id.as_bytes() != fact.source_id.as_slice()
            || verified.envelope.payload.sequence != fact.sequence
            || verified.envelope.payload.doc_kind != fact.doc_kind
            || verified.envelope.payload.parent_document_digest != fact.parent_document_digest
            || verified.canonical_payload != fact.canonical_payload
            || verified.canonical_root != fact.canonical_root
            || verified.canonical_state != fact.canonical_state
            || verified.canonical_signatures != fact.canonical_signatures
            || verified.canonical_envelope != fact.canonical_envelope
            || verified.digests.document != fact.digests.document
            || verified.digests.root != fact.digests.root
            || verified.digests.state != fact.digests.state
            || verified.digests.signature_set != fact.digests.signature_set
            || verified.digests.signature_message != fact.digests.signature_message
        {
            return Err(DbError::ClosedIndeterminate);
        }
        Ok(())
    }

    fn verify_anchor_parity_on_open(&mut self) -> Result<(), DbError> {
        let db_anchor = self.load_anchor_state()?;
        let sidecar = match read_anchor_sidecar(&self.anchor_path) {
            Ok(value) => value,
            Err(DbError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                if db_anchor == crate::zero_anchor() {
                    write_anchor_sidecar(&self.anchor_path, &db_anchor)?;
                    db_anchor.clone()
                } else {
                    return Err(DbError::ClosedIndeterminate);
                }
            }
            Err(other) => return Err(other),
        };
        if sidecar != db_anchor {
            return Err(DbError::ClosedIndeterminate);
        }
        match self.anchor_store.load_head()? {
            Some(store_anchor) if store_anchor == db_anchor => Ok(()),
            Some(_) => Err(DbError::ClosedIndeterminate),
            None if db_anchor == crate::zero_anchor() => {
                if self.anchor_store.compare_and_swap_head(None, &db_anchor)? {
                    Ok(())
                } else {
                    Err(DbError::ClosedIndeterminate)
                }
            }
            None => Err(DbError::ClosedIndeterminate),
        }
    }

    fn ensure_writable(&self) -> Result<(), DbError> {
        if self.load_anchor_state()?.state == AnchorState::Pending {
            return Err(DbError::PendingCheckpoint);
        }
        Ok(())
    }

    pub(crate) fn load_anchor_state(&self) -> Result<AnchorHeadRecord, DbError> {
        load_anchor_state_from_conn(self.conn()?)
    }

    fn store_anchor_state(
        &self,
        tx: &Transaction<'_>,
        anchor: &AnchorHeadRecord,
    ) -> Result<(), DbError> {
        tx.execute(
            "UPDATE anchor_state
             SET state = ?2,
                 checkpoint_sequence = ?3,
                 freeze_journal_sequence = ?4,
                 root_hash = ?5,
                 prepare_token = ?6
             WHERE singleton_id = ?1",
            params![
                ANCHOR_ROW_ID,
                anchor_state_str(anchor.state),
                as_i64(anchor.checkpoint_sequence)?,
                as_i64(anchor.freeze_journal_sequence)?,
                anchor.root_hash.as_slice(),
                anchor.prepare_token.map(|value| value.to_vec()),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn load_source_head(
        &self,
        conn: &Connection,
        source_id: &[u8],
    ) -> Result<Option<SourceHeadRow>, DbError> {
        map_source_head_row_from_conn(conn, source_id)
    }

    pub(crate) fn load_chain_slot(
        &self,
        conn: &Connection,
        source_id: &[u8],
        sequence: u64,
    ) -> Result<Option<crate::ChainSlotRow>, DbError> {
        map_chain_slot_row_from_conn(conn, source_id, sequence)
    }

    pub(crate) fn load_fact_by_id(
        &self,
        conn: &Connection,
        fact_id: i64,
    ) -> Result<VerifiedFactRecord, DbError> {
        conn.query_row(
            "SELECT fact_id, source_id, sequence, doc_kind, parent_document_digest, raw_bytes, canonical_payload, canonical_root, canonical_state, canonical_signatures, canonical_envelope, document_digest, root_digest, state_digest, signature_set_digest, signature_message_digest
             FROM verified_facts
             WHERE fact_id = ?1",
            params![fact_id],
            map_fact_row,
        )
        .map_err(DbError::from)
    }

    fn load_fork_record(
        &self,
        conn: &Connection,
        slot: &crate::ChainSlotRow,
    ) -> Result<ChainForkRecord, DbError> {
        Ok(ChainForkRecord {
            source_id: slot.source_id.clone(),
            sequence: slot.sequence,
            original: self.load_fact_by_id(
                conn,
                slot.original_verified_fact_id
                    .ok_or(DbError::ClosedIndeterminate)?,
            )?,
            conflict: self.load_fact_by_id(
                conn,
                slot.conflict_verified_fact_id
                    .ok_or(DbError::ClosedIndeterminate)?,
            )?,
            decisive_attempt_id: slot
                .decisive_attempt_id
                .ok_or(DbError::ClosedIndeterminate)?,
        })
    }

    fn load_head_fork(
        &self,
        conn: &Connection,
        head: &SourceHeadRow,
    ) -> Result<ChainForkRecord, DbError> {
        let slot = self
            .load_chain_slot(
                conn,
                &head.source_id,
                head.forked_slot_sequence
                    .ok_or(DbError::ClosedIndeterminate)?,
            )?
            .ok_or(DbError::ClosedIndeterminate)?;
        self.load_fork_record(conn, &slot)
    }

    fn verify_projected_fact(
        &self,
        conn: &Connection,
        source_id: &[u8],
        sequence: u64,
        fact_id: i64,
    ) -> Result<(), DbError> {
        let fact = self.load_fact_by_id(conn, fact_id)?;
        if fact.source_id != source_id || fact.sequence != sequence {
            return Err(DbError::ClosedIndeterminate);
        }
        Ok(())
    }

    fn verify_fork_attempt(
        &self,
        conn: &Connection,
        slot: &crate::ChainSlotRow,
        conflict_fact_id: i64,
        decisive_attempt_id: i64,
    ) -> Result<(), DbError> {
        let attempt = conn
            .query_row(
                "SELECT disposition, chain_error_code, source_id, sequence, verified_fact_id
                 FROM ingest_attempts
                 WHERE attempt_id = ?1",
                params![decisive_attempt_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(DbError::ClosedIndeterminate)?;
        if attempt.0 != "forked"
            || attempt.1.as_deref() != Some("sequence_fork")
            || attempt.2.as_deref() != Some(slot.source_id.as_slice())
            || attempt.3 != Some(as_i64(slot.sequence)?)
            || attempt.4 != Some(conflict_fact_id)
        {
            return Err(DbError::ClosedIndeterminate);
        }
        Ok(())
    }

    fn verify_chain_slot(
        &self,
        conn: &Connection,
        slot: &crate::ChainSlotRow,
    ) -> Result<(), DbError> {
        match slot.slot_state.as_str() {
            "accepted" => {
                let fact_id = slot
                    .head_verified_fact_id
                    .ok_or(DbError::ClosedIndeterminate)?;
                if slot.original_verified_fact_id.is_some()
                    || slot.conflict_verified_fact_id.is_some()
                    || slot.decisive_attempt_id.is_some()
                {
                    return Err(DbError::ClosedIndeterminate);
                }
                self.verify_projected_fact(conn, &slot.source_id, slot.sequence, fact_id)?;
            }
            "forked" => {
                if slot.head_verified_fact_id.is_some() {
                    return Err(DbError::ClosedIndeterminate);
                }
                let original_fact_id = slot
                    .original_verified_fact_id
                    .ok_or(DbError::ClosedIndeterminate)?;
                let conflict_fact_id = slot
                    .conflict_verified_fact_id
                    .ok_or(DbError::ClosedIndeterminate)?;
                let decisive_attempt_id = slot
                    .decisive_attempt_id
                    .ok_or(DbError::ClosedIndeterminate)?;
                if original_fact_id == conflict_fact_id {
                    return Err(DbError::ClosedIndeterminate);
                }
                self.verify_projected_fact(conn, &slot.source_id, slot.sequence, original_fact_id)?;
                self.verify_projected_fact(conn, &slot.source_id, slot.sequence, conflict_fact_id)?;
                self.verify_fork_attempt(conn, slot, conflict_fact_id, decisive_attempt_id)?;
            }
            _ => return Err(DbError::ClosedIndeterminate),
        }
        Ok(())
    }

    fn finalize_projection_source(
        &self,
        head: &SourceHeadRow,
        last_sequence: Option<u64>,
        saw_fork_sequence: Option<u64>,
    ) -> Result<(), DbError> {
        let last_sequence = last_sequence.ok_or(DbError::ClosedIndeterminate)?;
        let head_sequence =
            u64::try_from(head.head_sequence).map_err(|_| DbError::ClosedIndeterminate)?;
        if last_sequence != head_sequence {
            return Err(DbError::ClosedIndeterminate);
        }
        match head.head_state.as_str() {
            "accepted" => {
                if head.head_verified_fact_id.is_none()
                    || head.forked_slot_sequence.is_some()
                    || saw_fork_sequence.is_some()
                {
                    return Err(DbError::ClosedIndeterminate);
                }
            }
            "forked" => {
                if head.head_verified_fact_id.is_some()
                    || saw_fork_sequence != head.forked_slot_sequence
                {
                    return Err(DbError::ClosedIndeterminate);
                }
            }
            _ => return Err(DbError::ClosedIndeterminate),
        }
        Ok(())
    }

    fn verify_projection_consistency(&self, conn: &Connection) -> Result<(), DbError> {
        let mut statement = conn.prepare(
            "SELECT source_id, head_state, head_sequence, head_verified_fact_id, forked_slot_sequence
             FROM source_heads
             ORDER BY source_id ASC",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let head = map_source_head_row(row)?;
            match head.head_state.as_str() {
                "accepted" => {
                    let head_sequence = u64::try_from(head.head_sequence)
                        .map_err(|_| DbError::ClosedIndeterminate)?;
                    let head_fact_id = head
                        .head_verified_fact_id
                        .ok_or(DbError::ClosedIndeterminate)?;
                    if head.forked_slot_sequence.is_some() {
                        return Err(DbError::ClosedIndeterminate);
                    }
                    let slot = self
                        .load_chain_slot(conn, &head.source_id, head_sequence)?
                        .ok_or(DbError::ClosedIndeterminate)?;
                    if slot.slot_state != "accepted"
                        || slot.head_verified_fact_id != Some(head_fact_id)
                    {
                        return Err(DbError::ClosedIndeterminate);
                    }
                    self.verify_projected_fact(conn, &head.source_id, head_sequence, head_fact_id)?;
                }
                "forked" => {
                    if head.head_verified_fact_id.is_some() {
                        return Err(DbError::ClosedIndeterminate);
                    }
                    let forked_sequence = head
                        .forked_slot_sequence
                        .ok_or(DbError::ClosedIndeterminate)?;
                    let slot = self
                        .load_chain_slot(conn, &head.source_id, forked_sequence)?
                        .ok_or(DbError::ClosedIndeterminate)?;
                    if slot.slot_state != "forked" || slot.decisive_attempt_id.is_none() {
                        return Err(DbError::ClosedIndeterminate);
                    }
                    if head.head_sequence < as_i64(forked_sequence)? {
                        return Err(DbError::ClosedIndeterminate);
                    }
                    self.verify_chain_slot(conn, &slot)?;
                }
                _ => return Err(DbError::ClosedIndeterminate),
            }
        }

        let mut statement = conn.prepare(
            "SELECT source_id, sequence, slot_state, head_verified_fact_id, original_verified_fact_id, conflict_verified_fact_id, decisive_attempt_id
             FROM chain_slots
             ORDER BY source_id ASC, sequence ASC",
        )?;
        let mut rows = statement.query([])?;
        let mut current_source = None::<Vec<u8>>;
        let mut current_head = None::<SourceHeadRow>;
        let mut expected_sequence = 0u64;
        let mut last_sequence = None::<u64>;
        let mut saw_fork_sequence = None::<u64>;
        while let Some(row) = rows.next()? {
            let slot = map_chain_slot_row(row)?;
            if current_source.as_ref() != Some(&slot.source_id) {
                if let Some(head) = current_head.as_ref() {
                    self.finalize_projection_source(head, last_sequence, saw_fork_sequence)?;
                }
                current_head = Some(
                    self.load_source_head(conn, &slot.source_id)?
                        .ok_or(DbError::ClosedIndeterminate)?,
                );
                current_source = Some(slot.source_id.clone());
                expected_sequence = 0;
                last_sequence = None;
                saw_fork_sequence = None;
            }
            if slot.sequence != expected_sequence {
                return Err(DbError::ClosedIndeterminate);
            }
            self.verify_chain_slot(conn, &slot)?;

            let head = current_head.as_ref().ok_or(DbError::ClosedIndeterminate)?;
            let head_sequence =
                u64::try_from(head.head_sequence).map_err(|_| DbError::ClosedIndeterminate)?;
            if slot.sequence > head_sequence {
                return Err(DbError::ClosedIndeterminate);
            }
            match head.head_state.as_str() {
                "accepted" => {
                    if slot.slot_state != "accepted" {
                        return Err(DbError::ClosedIndeterminate);
                    }
                    if slot.sequence == head_sequence
                        && slot.head_verified_fact_id != head.head_verified_fact_id
                    {
                        return Err(DbError::ClosedIndeterminate);
                    }
                }
                "forked" => {
                    let forked_sequence = head
                        .forked_slot_sequence
                        .ok_or(DbError::ClosedIndeterminate)?;
                    if slot.slot_state == "forked" {
                        if saw_fork_sequence.replace(slot.sequence).is_some()
                            || slot.sequence != forked_sequence
                        {
                            return Err(DbError::ClosedIndeterminate);
                        }
                    } else if slot.sequence == forked_sequence {
                        return Err(DbError::ClosedIndeterminate);
                    }
                }
                _ => return Err(DbError::ClosedIndeterminate),
            }
            expected_sequence = expected_sequence
                .checked_add(1)
                .ok_or(DbError::ClosedIndeterminate)?;
            last_sequence = Some(slot.sequence);
        }
        if let Some(head) = current_head.as_ref() {
            self.finalize_projection_source(head, last_sequence, saw_fork_sequence)?;
        }
        Ok(())
    }

    pub(crate) fn conn(&self) -> Result<&Connection, DbError> {
        self.conn.as_ref().ok_or(DbError::ClosedIndeterminate)
    }

    pub(crate) fn conn_mut(&mut self) -> Result<&mut Connection, DbError> {
        self.conn.as_mut().ok_or(DbError::ClosedIndeterminate)
    }
}

impl Drop for EvidenceDb {
    fn drop(&mut self) {
        let _ = self.lock_file.unlock();
    }
}
