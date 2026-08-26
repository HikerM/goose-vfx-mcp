use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lumina_verified_sources::{
    ChainLookup, Digest32, DocKind, Document, DocumentDigests, Envelope, ExistingSequence,
    LookupState, Root, SignatureEntry, State, VerificationContext, VerifiedEnvelope,
};
use rusqlite::{params, Connection};

use crate::anchor::{AnchorStore, AnchorStoreError};
use crate::{
    checked_frame_len, doc_kind_str, hash_parts, read_anchor_sidecar, recreate_projection_schema,
    write_anchor_sidecar, zero_anchor, AnchorHeadRecord, AnchorState, ChainSlotRecord, DbError,
    EvidenceDb, HeadRecord, IngestDisposition, MemoryAnchorStore, TxContext,
    MAX_ANCHOR_SIDECAR_BYTES,
};

#[test]
fn api_boundary_compiles_and_opens() {
    let (mut db, path) = test_db("api-boundary");
    let _ = db.lookup_head("missing");
    let _ = db.lookup_chain("missing", 0);
    let _ = db.recover_anchor();
    drop(db);
    let _ = fs::remove_file(path);
}

#[test]
fn parse_rejection_is_audit_only_without_source_guess() {
    let (mut db, _) = test_db("parse-reject");
    let outcome = db
        .ingest_raw(br#"{"payload":{"doc_kind":"bootstrap","parent_document_digest":null},"signatures":[]}"#)
        .unwrap();
    assert!(matches!(
        outcome.disposition,
        IngestDisposition::ParseRejected { .. }
    ));
    let row = db
        .conn
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT source_id, verified_fact_id FROM ingest_attempts WHERE attempt_id = ?1",
            params![outcome.attempt_id],
            |row| {
                Ok((
                    row.get::<_, Option<Vec<u8>>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, None);
    assert_eq!(row.1, None);
}

#[test]
fn equivalent_variant_does_not_advance_head() {
    let (mut db, _) = test_db("equivalent");
    let bootstrap = test_verified("variants/source", 0, None, DocKind::Bootstrap, 1, 1);
    let bootstrap_digest = bootstrap.digests.document;
    db.ingest_verified_for_tests(&bootstrap.raw_bytes.clone(), bootstrap)
        .unwrap();

    let first = test_verified(
        "variants/source",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        2,
        1,
    );
    let second = test_verified(
        "variants/source",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        2,
        2,
    );
    let accepted = db
        .ingest_verified_for_tests(&first.raw_bytes.clone(), first.clone())
        .unwrap();
    let equivalent = db
        .ingest_verified_for_tests(&second.raw_bytes.clone(), second.clone())
        .unwrap();
    assert!(matches!(
        accepted.disposition,
        IngestDisposition::Accepted { .. }
    ));
    assert!(matches!(
        equivalent.disposition,
        IngestDisposition::EquivalentVariant { .. }
    ));
    let head = db.lookup_head("variants/source").unwrap();
    assert!(
        matches!(head, HeadRecord::Accepted(ref fact) if fact.digests.signature_set == first.digests.signature_set)
    );
}

#[test]
fn fork_is_irreversible_and_children_fail_closed() {
    let (mut db, _) = test_db("forked");
    let bootstrap = test_verified("fork/source", 0, None, DocKind::Bootstrap, 10, 1);
    let bootstrap_digest = bootstrap.digests.document;
    db.ingest_verified_for_tests(&bootstrap.raw_bytes.clone(), bootstrap)
        .unwrap();

    let first = test_verified(
        "fork/source",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        11,
        1,
    );
    let conflict = test_verified(
        "fork/source",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        12,
        2,
    );
    db.ingest_verified_for_tests(&first.raw_bytes.clone(), first.clone())
        .unwrap();
    let forked = db
        .ingest_verified_for_tests(&conflict.raw_bytes.clone(), conflict.clone())
        .unwrap();
    assert!(matches!(
        forked.disposition,
        IngestDisposition::Forked { .. }
    ));

    let child = test_verified(
        "fork/source",
        2,
        Some(first.digests.document),
        DocKind::Snapshot,
        13,
        1,
    );
    let child_outcome = db
        .ingest_verified_for_tests(&child.raw_bytes.clone(), child)
        .unwrap();
    assert!(matches!(
        child_outcome.disposition,
        IngestDisposition::ChainRejected {
            error: lumina_verified_sources::ErrorCode::LookupFork
        }
    ));
    assert!(matches!(
        db.lookup_head("fork/source").unwrap(),
        HeadRecord::Fork(_)
    ));
}

#[test]
fn first_fork_decisive_fields_remain_immutable_after_later_conflicts() {
    let (mut db, _) = test_db("forked-immutable");
    let bootstrap = test_verified("fork/immutable", 0, None, DocKind::Bootstrap, 30, 1);
    let bootstrap_digest = bootstrap.digests.document;
    db.ingest_verified_for_tests(&bootstrap.raw_bytes.clone(), bootstrap)
        .unwrap();

    let accepted = test_verified(
        "fork/immutable",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        31,
        1,
    );
    let first_conflict = test_verified(
        "fork/immutable",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        32,
        2,
    );
    let later_conflict = test_verified(
        "fork/immutable",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        33,
        3,
    );

    let accepted_outcome = db
        .ingest_verified_for_tests(&accepted.raw_bytes.clone(), accepted.clone())
        .unwrap();
    let first_fork = db
        .ingest_verified_for_tests(&first_conflict.raw_bytes.clone(), first_conflict.clone())
        .unwrap();
    let later_fork = db
        .ingest_verified_for_tests(&later_conflict.raw_bytes.clone(), later_conflict.clone())
        .unwrap();

    let accepted_fact_id = match accepted_outcome.disposition {
        IngestDisposition::Accepted { fact_id } => fact_id,
        _ => panic!("expected accepted"),
    };
    let first_conflict_fact_id = match &first_fork.disposition {
        IngestDisposition::Forked {
            original_fact_id,
            conflict_fact_id,
        } => {
            assert_eq!(*original_fact_id, accepted_fact_id);
            *conflict_fact_id
        }
        _ => panic!("expected first fork"),
    };
    let later_conflict_fact_id = match &later_fork.disposition {
        IngestDisposition::Forked {
            original_fact_id,
            conflict_fact_id,
        } => {
            assert_eq!(*original_fact_id, accepted_fact_id);
            *conflict_fact_id
        }
        _ => panic!("expected later fork"),
    };

    let conn = db.conn.as_ref().unwrap();
    let slot = conn
        .query_row(
            "SELECT slot_state, original_verified_fact_id, conflict_verified_fact_id, decisive_attempt_id
             FROM chain_slots
             WHERE source_id = ?1 AND sequence = 1",
            params![b"fork/immutable".to_vec()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(slot.0, "forked");
    assert_eq!(slot.1, Some(accepted_fact_id));
    assert_eq!(slot.2, Some(first_conflict_fact_id));
    assert_eq!(slot.3, Some(first_fork.attempt_id));

    let head = conn
        .query_row(
            "SELECT head_state, head_sequence, head_verified_fact_id, forked_slot_sequence
             FROM source_heads
             WHERE source_id = ?1",
            params![b"fork/immutable".to_vec()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(head.0, "forked");
    assert_eq!(head.1, 1);
    assert_eq!(head.2, None);
    assert_eq!(head.3, Some(1));

    let fork_event_count = conn
        .query_row(
            "SELECT COUNT(*) FROM journal_events
             WHERE event_type = 'forked' AND source_id = ?1 AND sequence = 1",
            params![b"fork/immutable".to_vec()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(fork_event_count, 2);

    let latest_fork_event = conn
        .query_row(
            "SELECT verified_fact_id, related_verified_fact_id
             FROM journal_events
             WHERE event_type = 'forked' AND source_id = ?1 AND sequence = 1
             ORDER BY journal_seq DESC
             LIMIT 1",
            params![b"fork/immutable".to_vec()],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .unwrap();
    assert_eq!(latest_fork_event.0, Some(later_conflict_fact_id));
    assert_eq!(latest_fork_event.1, Some(accepted_fact_id));

    let slot_update = conn.execute(
        "UPDATE chain_slots
         SET conflict_verified_fact_id = ?3, decisive_attempt_id = ?4
         WHERE source_id = ?1 AND sequence = ?2",
        params![
            b"fork/immutable".to_vec(),
            1i64,
            later_conflict_fact_id,
            later_fork.attempt_id
        ],
    );
    assert!(slot_update
        .unwrap_err()
        .to_string()
        .contains("chain_slots_fork_immutable"));

    let head_update = conn.execute(
        "UPDATE source_heads
         SET forked_slot_sequence = 0
         WHERE source_id = ?1",
        params![b"fork/immutable".to_vec()],
    );
    assert!(head_update
        .unwrap_err()
        .to_string()
        .contains("source_heads_fork_immutable"));
}

#[test]
fn source_id_blob_and_fact_roots_use_raw_ascii_bytes() {
    let (mut db, _) = test_db("blob-source");
    let verified = test_verified("blob/source", 0, None, DocKind::Bootstrap, 21, 1);
    let outcome = db
        .ingest_verified_for_tests(&verified.raw_bytes.clone(), verified.clone())
        .unwrap();
    let fact_id = match outcome.disposition {
        IngestDisposition::Accepted { fact_id } => fact_id,
        _ => panic!("expected accepted"),
    };
    let stored = db
        .conn
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT source_id, canonical_root FROM verified_facts WHERE fact_id = ?1",
            params![fact_id],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .unwrap();
    assert_eq!(stored.0, b"blob/source".to_vec());
    assert_eq!(stored.1, verified.canonical_root);
}

#[test]
fn checkpoint_has_no_self_reference_and_pending_blocks_writes() {
    let (mut db, _) = test_db("checkpoint");
    let verified = test_verified("anchor/source", 0, None, DocKind::Bootstrap, 31, 1);
    db.ingest_verified_for_tests(&verified.raw_bytes.clone(), verified)
        .unwrap();
    let before = db.max_journal_sequence(db.conn.as_ref().unwrap()).unwrap();
    let checkpoint = db.checkpoint_once().unwrap();
    assert!(checkpoint.created_pending);
    assert_eq!(checkpoint.anchor.freeze_journal_sequence, before);
    assert!(matches!(
        db.ingest_raw(br#"{}"#),
        Err(DbError::PendingCheckpoint)
    ));
}

#[test]
fn schema_fence_rejects_legacy_file_and_pending_rebuild_is_refused() {
    let (mut db, path) = test_db("schema-fence");
    db.checkpoint_once().unwrap();
    assert!(matches!(db.rebuild(), Err(DbError::RebuildRefused)));
    drop(db);

    let legacy_path = path.parent().unwrap().join("legacy").join("evidence.db");
    fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
    let legacy = Connection::open(&legacy_path).unwrap();
    legacy
        .execute("CREATE TABLE legacy_table (value INTEGER)", [])
        .unwrap();
    drop(legacy);
    let result = EvidenceDb::open(&legacy_path, Box::new(MemoryAnchorStore::default()));
    assert!(matches!(result, Err(DbError::LegacySchemaRejected)));
}

#[test]
fn fresh_create_close_and_reopen_succeeds() {
    let (db, path) = test_db("fresh-reopen");
    drop(db);

    let reopened = EvidenceDb::open(&path, Box::new(MemoryAnchorStore::default()));
    assert!(reopened.is_ok());
}

#[test]
fn open_fails_closed_when_projection_tables_drift_from_schema_valid_state() {
    let (db, path) = test_db("open-projection-drift");
    drop(db);

    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "INSERT INTO source_heads (
             source_id, head_state, head_sequence, head_verified_fact_id, forked_slot_sequence
         ) VALUES (?1, 'forked', 0, NULL, 0)",
        params![b"drift/source".to_vec()],
    )
    .unwrap();
    drop(conn);

    let reopened = EvidenceDb::open(&path, Box::new(MemoryAnchorStore::default()));
    assert!(matches!(reopened, Err(DbError::ClosedIndeterminate)));
}

#[test]
fn open_fails_closed_when_attempt_verified_fact_fk_drifts() {
    let (mut db, path) = test_db("attempt-fact-drift");
    let verified = test_verified("attempt/source", 0, None, DocKind::Bootstrap, 51, 1);
    let outcome = db
        .ingest_verified_for_tests(&verified.raw_bytes.clone(), verified)
        .unwrap();
    drop(db);

    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    insert_attempt_with_missing_verified_fact(&conn, outcome.attempt_id);
    drop(conn);

    let reopened = EvidenceDb::open(&path, Box::new(MemoryAnchorStore::default()));
    assert!(matches!(reopened, Err(DbError::ClosedIndeterminate)));
}

#[test]
fn open_fails_closed_when_non_accepted_journal_event_mismatches_attempt() {
    let (mut db, path) = test_db("journal-mismatch-open");
    let outcome = db
        .ingest_raw(br#"{"payload":{"doc_kind":"bootstrap","parent_document_digest":null},"signatures":[]}"#)
        .unwrap();
    drop(db);

    let conn = Connection::open(&path).unwrap();
    insert_mismatched_chain_rejected_event(&conn, outcome.attempt_id);
    drop(conn);

    let reopened = EvidenceDb::open(&path, Box::new(MemoryAnchorStore::default()));
    assert!(matches!(reopened, Err(DbError::ClosedIndeterminate)));
}

#[test]
fn rebuild_fails_closed_when_non_accepted_journal_event_mismatches_attempt() {
    let (mut db, _) = test_db("journal-mismatch-rebuild");
    let outcome = db
        .ingest_raw(br#"{"payload":{"doc_kind":"bootstrap","parent_document_digest":null},"signatures":[]}"#)
        .unwrap();

    insert_mismatched_chain_rejected_event(db.conn.as_ref().unwrap(), outcome.attempt_id);

    assert!(matches!(db.rebuild(), Err(DbError::ClosedIndeterminate)));
}

#[test]
fn open_closes_when_verified_fact_raw_is_only_a_synthetic_test_fixture() {
    let (mut db, path) = test_db("open-synthetic-raw");
    let verified = test_verified("open/source", 0, None, DocKind::Bootstrap, 40, 1);
    db.ingest_verified_for_tests(&verified.raw_bytes.clone(), verified)
        .unwrap();
    drop(db);

    let reopened = EvidenceDb::open(&path, Box::new(MemoryAnchorStore::default()));
    assert!(matches!(reopened, Err(DbError::ClosedIndeterminate)));
}

#[test]
fn rebuild_closes_when_verified_fact_raw_is_only_a_synthetic_test_fixture() {
    let (mut db, _) = test_db("rebuild-synthetic-raw");
    let verified = test_verified("rebuild/source", 0, None, DocKind::Bootstrap, 41, 1);
    db.ingest_verified_for_tests(&verified.raw_bytes.clone(), verified)
        .unwrap();

    recreate_projection_schema(db.conn.as_ref().unwrap()).unwrap();

    assert!(matches!(db.rebuild(), Err(DbError::ClosedIndeterminate)));
}

#[test]
fn open_closes_when_synthetic_forked_history_is_replayed() {
    let (mut db, path) = test_db("open-forked-synthetic");
    let bootstrap = test_verified("forked/synthetic", 0, None, DocKind::Bootstrap, 45, 1);
    let bootstrap_digest = bootstrap.digests.document;
    db.ingest_verified_for_tests(&bootstrap.raw_bytes.clone(), bootstrap)
        .unwrap();
    let accepted = test_verified(
        "forked/synthetic",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        46,
        1,
    );
    let conflict = test_verified(
        "forked/synthetic",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        47,
        2,
    );
    db.ingest_verified_for_tests(&accepted.raw_bytes.clone(), accepted)
        .unwrap();
    db.ingest_verified_for_tests(&conflict.raw_bytes.clone(), conflict)
        .unwrap();
    drop(db);

    let reopened = EvidenceDb::open(&path, Box::new(MemoryAnchorStore::default()));
    assert!(matches!(reopened, Err(DbError::ClosedIndeterminate)));
}

#[test]
fn rebuild_closes_when_synthetic_forked_history_is_replayed() {
    let (mut db, _) = test_db("rebuild-forked-synthetic");
    let bootstrap = test_verified("forked/synthetic", 0, None, DocKind::Bootstrap, 48, 1);
    let bootstrap_digest = bootstrap.digests.document;
    db.ingest_verified_for_tests(&bootstrap.raw_bytes.clone(), bootstrap)
        .unwrap();
    let accepted = test_verified(
        "forked/synthetic",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        49,
        1,
    );
    let conflict = test_verified(
        "forked/synthetic",
        1,
        Some(bootstrap_digest),
        DocKind::Snapshot,
        50,
        2,
    );
    db.ingest_verified_for_tests(&accepted.raw_bytes.clone(), accepted)
        .unwrap();
    db.ingest_verified_for_tests(&conflict.raw_bytes.clone(), conflict)
        .unwrap();

    assert!(matches!(db.rebuild(), Err(DbError::ClosedIndeterminate)));
}

#[test]
fn open_closes_when_synthetic_parse_rejected_history_is_replayed() {
    let (db, path) = test_db("open-parse-rejected-synthetic");
    insert_synthetic_parse_rejected_history(
        db.conn.as_ref().unwrap(),
        b"not-json",
        "missing_field",
    );
    drop(db);

    let reopened = EvidenceDb::open(&path, Box::new(MemoryAnchorStore::default()));
    assert!(matches!(reopened, Err(DbError::ClosedIndeterminate)));
}

#[test]
fn rebuild_closes_when_synthetic_parse_rejected_history_is_replayed() {
    let (mut db, _) = test_db("rebuild-parse-rejected-synthetic");
    insert_synthetic_parse_rejected_history(
        db.conn.as_ref().unwrap(),
        b"not-json",
        "missing_field",
    );

    assert!(matches!(db.rebuild(), Err(DbError::ClosedIndeterminate)));
}

#[test]
fn open_closes_when_synthetic_chain_rejected_history_is_replayed() {
    let (db, path) = test_db("open-chain-rejected-synthetic");
    let verified = test_verified("chain/synthetic", 0, None, DocKind::Bootstrap, 43, 1);
    insert_synthetic_chain_rejected_history(
        db.conn.as_ref().unwrap(),
        &verified,
        "signature_quorum_not_met",
    );
    drop(db);

    let reopened = EvidenceDb::open(&path, Box::new(MemoryAnchorStore::default()));
    assert!(matches!(reopened, Err(DbError::ClosedIndeterminate)));
}

#[test]
fn rebuild_closes_when_synthetic_chain_rejected_history_is_replayed() {
    let (mut db, _) = test_db("rebuild-chain-rejected-synthetic");
    let verified = test_verified("chain/synthetic", 0, None, DocKind::Bootstrap, 44, 1);
    insert_synthetic_chain_rejected_history(
        db.conn.as_ref().unwrap(),
        &verified,
        "signature_quorum_not_met",
    );

    assert!(matches!(db.rebuild(), Err(DbError::ClosedIndeterminate)));
}

#[test]
fn tx_context_latches_projection_decode_errors_instead_of_returning_absent() {
    let mut conn = malformed_projection_conn();

    {
        let tx = conn.transaction().unwrap();
        let context = TxContext::new(&tx);
        assert!(matches!(
            context.lookup("decode/source", 0),
            LookupState::Fork
        ));
        assert!(matches!(
            context.finish(Ok(())),
            Err(DbError::ClosedIndeterminate)
        ));
    }

    {
        let tx = conn.transaction().unwrap();
        let context = TxContext::new(&tx);
        let digest = Digest32::new([9u8; 32]);
        assert!(matches!(
            context.classify_existing("decode/source", 0, &digest, &digest),
            ExistingSequence::Fork
        ));
        assert!(matches!(
            context.finish(Ok(())),
            Err(DbError::ClosedIndeterminate)
        ));
    }
}

#[test]
fn rebuild_closes_when_persisted_source_id_drift_is_non_utf8() {
    let (mut db, _) = test_db("rebuild-non-utf8");
    let verified = test_verified("rebuild/source", 0, None, DocKind::Bootstrap, 42, 1);
    let fact = crate::VerifiedFactRecord {
        fact_id: 1,
        source_id: vec![0xffu8, 0xfeu8],
        sequence: verified.envelope.payload.sequence,
        doc_kind: verified.envelope.payload.doc_kind,
        parent_document_digest: verified.envelope.payload.parent_document_digest,
        raw_bytes: verified.raw_bytes.clone(),
        canonical_payload: verified.canonical_payload.clone(),
        canonical_root: verified.canonical_root.clone(),
        canonical_state: verified.canonical_state.clone(),
        canonical_signatures: verified.canonical_signatures.clone(),
        canonical_envelope: verified.canonical_envelope.clone(),
        digests: verified.digests.clone(),
    };

    assert!(matches!(
        db.replayed_fact_matches_for_tests(&fact, &verified),
        Err(DbError::ClosedIndeterminate)
    ));
}

#[test]
fn rebuild_closes_when_external_anchor_head_drifts_after_replay() {
    let zero = zero_anchor();
    let drifted = AnchorHeadRecord {
        state: AnchorState::Stable,
        checkpoint_sequence: 7,
        freeze_journal_sequence: 7,
        root_hash: [9u8; 32],
        prepare_token: None,
    };
    let root = std::env::temp_dir().join(format!(
        "lumina-evidence-db-anchor-drift-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("evidence.db");
    let store =
        ScriptedAnchorStore::new(vec![Some(zero.clone()), Some(zero.clone()), Some(drifted)]);
    let mut db = EvidenceDb::open(&path, Box::new(store)).unwrap();

    assert!(matches!(db.rebuild(), Err(DbError::ClosedIndeterminate)));
}

#[test]
fn oversized_framing_lengths_are_rejected() {
    let oversized = (u32::MAX as usize).checked_add(1).unwrap();
    assert!(matches!(
        checked_frame_len(oversized),
        Err(DbError::NumericOverflow)
    ));
}

#[test]
fn numeric_conversion_overflow_returns_error() {
    assert!(matches!(
        crate::checked_i64_to_u64(-1),
        Err(DbError::NumericOverflow)
    ));

    let verified = test_verified("overflow/source", 0, None, DocKind::Bootstrap, 61, 1);
    assert!(matches!(
        crate::JournalEvent::verified("forked", 7, &verified, 11, Some(-1)),
        Err(DbError::NumericOverflow)
    ));

    assert_eq!(
        crate::checked_usize_to_u64(verified.raw_bytes.len()).unwrap(),
        u64::try_from(verified.raw_bytes.len()).unwrap()
    );
}

#[test]
fn anchor_sidecar_appends_records_without_using_anchor_tmp_path() {
    let root = std::env::temp_dir().join(format!(
        "lumina-evidence-db-anchor-sidecar-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let anchor_path = root.join("evidence.db.anchor");
    let temp_path = root.join("evidence.db.anchor.tmp");
    let sibling = root.join("anchor-source.bin");
    fs::write(&sibling, b"anchor").unwrap();
    fs::hard_link(&sibling, &temp_path).unwrap();

    let first = zero_anchor();
    let second = AnchorHeadRecord {
        state: AnchorState::Pending,
        checkpoint_sequence: 1,
        freeze_journal_sequence: 3,
        root_hash: [5u8; 32],
        prepare_token: Some([7u8; 32]),
    };
    write_anchor_sidecar(&anchor_path, &first).unwrap();
    write_anchor_sidecar(&anchor_path, &second).unwrap();

    assert_eq!(read_anchor_sidecar(&anchor_path).unwrap(), second);
    assert_eq!(fs::metadata(&anchor_path).unwrap().len(), (87 * 2) as u64);
    assert!(temp_path.exists());
}

#[test]
fn anchor_sidecar_rejects_partial_last_record() {
    let root = std::env::temp_dir().join(format!(
        "lumina-evidence-db-anchor-sidecar-partial-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let anchor_path = root.join("evidence.db.anchor");
    write_anchor_sidecar(&anchor_path, &zero_anchor()).unwrap();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&anchor_path)
        .unwrap();
    use std::io::Write as _;
    file.write_all(&[0x7f]).unwrap();
    file.sync_all().unwrap();

    assert!(matches!(
        read_anchor_sidecar(&anchor_path),
        Err(DbError::ClosedIndeterminate)
    ));
}

#[test]
fn anchor_sidecar_rejects_oversized_files() {
    let root = std::env::temp_dir().join(format!(
        "lumina-evidence-db-anchor-sidecar-oversize-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let anchor_path = root.join("evidence.db.anchor");
    let file = fs::File::create(&anchor_path).unwrap();
    file.set_len(MAX_ANCHOR_SIDECAR_BYTES + 87).unwrap();
    file.sync_all().unwrap();

    assert!(matches!(
        read_anchor_sidecar(&anchor_path),
        Err(DbError::ClosedIndeterminate)
    ));
}

fn test_db(name: &str) -> (EvidenceDb, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "lumina-evidence-db-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("evidence.db");
    let db = EvidenceDb::open(&path, Box::new(MemoryAnchorStore::default())).unwrap();
    (db, path)
}

fn test_verified(
    source_id: &str,
    sequence: u64,
    parent_document_digest: Option<Digest32>,
    doc_kind: DocKind,
    document_seed: u8,
    signature_seed: u8,
) -> VerifiedEnvelope {
    let state = State {
        releases: Vec::new(),
    };
    let canonical_state = state.to_canonical_json_bytes().unwrap();
    let canonical_root = b"{\"keys\":[],\"quorum\":0}".to_vec();
    let canonical_payload = format!(
        "payload:{}:{}:{}:{}",
        doc_kind_str(doc_kind),
        source_id,
        sequence,
        document_seed
    )
    .into_bytes();
    let signature_value = "A".repeat(86);
    let canonical_signatures = format!(
        "[{{\"kid\":\"sig-{}\",\"sig_b64u\":\"{}\"}}]",
        signature_seed, signature_value
    )
    .into_bytes();
    let canonical_envelope = [
        canonical_payload.as_slice(),
        canonical_signatures.as_slice(),
        canonical_state.as_slice(),
    ]
    .concat();
    let document_digest = hash_parts("test.document", &[&[document_seed]]).unwrap();
    let signature_set =
        hash_parts("test.signature-set", &[&[document_seed], &[signature_seed]]).unwrap();
    let signature_message = hash_parts("test.signature-message", &[&document_digest]).unwrap();
    let raw_bytes = format!(
        "raw:{}:{}:{}:{}",
        source_id, sequence, document_seed, signature_seed
    )
    .into_bytes();
    VerifiedEnvelope {
        raw_bytes,
        envelope: Envelope {
            payload: Document {
                doc_kind,
                parent_document_digest,
                prior_root: None,
                root: Root {
                    keys: Vec::new(),
                    quorum: 0,
                },
                sequence,
                source_id: source_id.to_string(),
                state,
                wire_version: "v2".to_string(),
            },
            signatures: vec![SignatureEntry {
                kid: format!("sig-{}", signature_seed),
                sig_b64u: signature_value,
            }],
        },
        canonical_payload,
        canonical_root: canonical_root.clone(),
        canonical_state,
        canonical_signatures,
        canonical_envelope,
        digests: DocumentDigests {
            document: Digest32::new(document_digest),
            root: Digest32::new(hash_parts("test.root", &[&canonical_root]).unwrap()),
            state: Digest32::new(hash_parts("test.state", &[b"{\"releases\":[]}"]).unwrap()),
            signature_set: Digest32::new(signature_set),
            signature_message: Digest32::new(signature_message),
        },
    }
}

fn insert_attempt_with_missing_verified_fact(conn: &Connection, attempt_id: i64) {
    conn.execute(
        "INSERT INTO ingest_attempts (
             raw_hash, raw_total_len, raw_truncated, raw_evidence, disposition,
             parse_error_code, chain_error_code, source_id, sequence, doc_kind,
             parent_document_digest, canonical_payload, canonical_root,
             canonical_state, canonical_signatures, canonical_envelope,
             document_digest, root_digest, state_digest, signature_set_digest,
             signature_message_digest, verified_fact_id
         )
         SELECT raw_hash, raw_total_len, raw_truncated, raw_evidence, disposition,
                parse_error_code, chain_error_code, source_id, sequence, doc_kind,
                parent_document_digest, canonical_payload, canonical_root,
                canonical_state, canonical_signatures, canonical_envelope,
                document_digest, root_digest, state_digest, signature_set_digest,
                signature_message_digest, verified_fact_id + 1000
         FROM ingest_attempts
         WHERE attempt_id = ?1",
        params![attempt_id],
    )
    .unwrap();
}

fn insert_synthetic_parse_rejected_history(conn: &Connection, raw: &[u8], parse_error_code: &str) {
    let raw_hash = hash_parts("evidence-db.raw", &[raw]).unwrap();
    conn.execute(
        "INSERT INTO ingest_attempts (
             raw_hash, raw_total_len, raw_truncated, raw_evidence, disposition,
             parse_error_code, chain_error_code, source_id, sequence, doc_kind,
             parent_document_digest, canonical_payload, canonical_root,
             canonical_state, canonical_signatures, canonical_envelope,
             document_digest, root_digest, state_digest, signature_set_digest,
             signature_message_digest, verified_fact_id
         ) VALUES (?1, ?2, 0, ?3, 'parse_rejected', ?4, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
        params![
            raw_hash.as_slice(),
            i64::try_from(raw.len()).unwrap(),
            raw,
            parse_error_code
        ],
    )
    .unwrap();
    let attempt_id = conn.last_insert_rowid();
    let event_root = hash_parts(
        "evidence-db.journal-event",
        &[
            b"parse_rejected",
            &[],
            &0u64.to_be_bytes(),
            raw_hash.as_slice(),
            parse_error_code.as_bytes(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO journal_events (
             event_type, attempt_id, source_id, sequence, verified_fact_id,
             related_verified_fact_id, error_code, event_root
         ) VALUES ('parse_rejected', ?1, NULL, NULL, NULL, NULL, ?2, ?3)",
        params![attempt_id, parse_error_code, event_root.as_slice()],
    )
    .unwrap();
}

fn insert_synthetic_chain_rejected_history(
    conn: &Connection,
    verified: &VerifiedEnvelope,
    chain_error_code: &str,
) {
    let raw_hash = hash_parts("evidence-db.raw", &[verified.raw_bytes.as_slice()]).unwrap();
    conn.execute(
        "INSERT INTO ingest_attempts (
             raw_hash, raw_total_len, raw_truncated, raw_evidence, disposition,
             parse_error_code, chain_error_code, source_id, sequence, doc_kind,
             parent_document_digest, canonical_payload, canonical_root,
             canonical_state, canonical_signatures, canonical_envelope,
             document_digest, root_digest, state_digest, signature_set_digest,
             signature_message_digest, verified_fact_id
         ) VALUES (?1, ?2, 0, ?3, 'chain_rejected', NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, NULL)",
        params![
            raw_hash.as_slice(),
            i64::try_from(verified.raw_bytes.len()).unwrap(),
            &verified.raw_bytes,
            chain_error_code,
            verified.envelope.payload.source_id.as_bytes(),
            i64::try_from(verified.envelope.payload.sequence).unwrap(),
            doc_kind_str(verified.envelope.payload.doc_kind),
            verified
                .envelope
                .payload
                .parent_document_digest
                .map(|value| value.as_bytes().to_vec()),
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
    )
    .unwrap();
    let attempt_id = conn.last_insert_rowid();
    let event_root = hash_parts(
        "evidence-db.journal-event",
        &[
            b"chain_rejected",
            verified.envelope.payload.source_id.as_bytes(),
            &verified.envelope.payload.sequence.to_be_bytes(),
            raw_hash.as_slice(),
            chain_error_code.as_bytes(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO journal_events (
             event_type, attempt_id, source_id, sequence, verified_fact_id,
             related_verified_fact_id, error_code, event_root
         ) VALUES ('chain_rejected', ?1, ?2, ?3, NULL, NULL, ?4, ?5)",
        params![
            attempt_id,
            verified.envelope.payload.source_id.as_bytes(),
            i64::try_from(verified.envelope.payload.sequence).unwrap(),
            chain_error_code,
            event_root.as_slice()
        ],
    )
    .unwrap();
}

fn insert_mismatched_chain_rejected_event(conn: &Connection, attempt_id: i64) {
    let raw_hash = conn
        .query_row(
            "SELECT raw_hash FROM ingest_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .unwrap();
    let source_id = b"journal/mismatch".to_vec();
    let sequence = 0u64;
    let error_code = "missing_parent";
    let event_root = hash_parts(
        "evidence-db.journal-event",
        &[
            b"chain_rejected",
            source_id.as_slice(),
            &sequence.to_be_bytes(),
            raw_hash.as_slice(),
            error_code.as_bytes(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO journal_events (
             event_type, attempt_id, source_id, sequence, verified_fact_id,
             related_verified_fact_id, error_code, event_root
         ) VALUES ('chain_rejected', ?1, ?2, ?3, NULL, NULL, ?4, ?5)",
        params![
            attempt_id,
            source_id,
            0i64,
            error_code,
            event_root.as_slice()
        ],
    )
    .unwrap();
}

#[derive(Clone)]
struct ScriptedAnchorStore {
    remaining: Arc<Mutex<Vec<Option<AnchorHeadRecord>>>>,
    current: Arc<Mutex<Option<AnchorHeadRecord>>>,
}

impl ScriptedAnchorStore {
    fn new(sequence: Vec<Option<AnchorHeadRecord>>) -> Self {
        let current = sequence.last().cloned().unwrap_or(None);
        Self {
            remaining: Arc::new(Mutex::new(sequence)),
            current: Arc::new(Mutex::new(current)),
        }
    }
}

impl AnchorStore for ScriptedAnchorStore {
    fn load_head(&self) -> Result<Option<AnchorHeadRecord>, AnchorStoreError> {
        let mut remaining = self
            .remaining
            .lock()
            .map_err(|_| AnchorStoreError::Unavailable)?;
        if let Some(next) = remaining.first().cloned() {
            remaining.remove(0);
            *self
                .current
                .lock()
                .map_err(|_| AnchorStoreError::Unavailable)? = next.clone();
            return Ok(next);
        }
        Ok(self
            .current
            .lock()
            .map_err(|_| AnchorStoreError::Unavailable)?
            .clone())
    }

    fn compare_and_swap_head(
        &self,
        expected: Option<&AnchorHeadRecord>,
        replacement: &AnchorHeadRecord,
    ) -> Result<bool, AnchorStoreError> {
        let mut current = self
            .current
            .lock()
            .map_err(|_| AnchorStoreError::Unavailable)?;
        if current.as_ref() != expected {
            return Ok(false);
        }
        *current = Some(replacement.clone());
        Ok(true)
    }
}

fn malformed_projection_conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE source_heads (
             source_id BLOB PRIMARY KEY,
             head_state TEXT NOT NULL,
             head_sequence INTEGER NOT NULL,
             head_verified_fact_id INTEGER,
             forked_slot_sequence INTEGER
         );
         CREATE TABLE chain_slots (
             source_id BLOB NOT NULL,
             sequence INTEGER NOT NULL,
             slot_state TEXT NOT NULL,
             head_verified_fact_id INTEGER,
             original_verified_fact_id INTEGER,
             conflict_verified_fact_id INTEGER,
             decisive_attempt_id INTEGER,
             PRIMARY KEY (source_id, sequence)
         );
         CREATE TABLE verified_facts (
             fact_id INTEGER PRIMARY KEY,
             source_id BLOB NOT NULL,
             sequence INTEGER NOT NULL,
             doc_kind TEXT NOT NULL,
             parent_document_digest BLOB,
             raw_bytes BLOB NOT NULL,
             canonical_payload BLOB NOT NULL,
             canonical_root BLOB NOT NULL,
             canonical_state BLOB NOT NULL,
             canonical_signatures BLOB NOT NULL,
             canonical_envelope BLOB NOT NULL,
             document_digest BLOB NOT NULL,
             root_digest BLOB NOT NULL,
             state_digest BLOB NOT NULL,
             signature_set_digest BLOB NOT NULL,
             signature_message_digest BLOB NOT NULL
         );",
    )
    .unwrap();

    conn.execute(
        "INSERT INTO source_heads (
             source_id, head_state, head_sequence, head_verified_fact_id, forked_slot_sequence
         ) VALUES (?1, 'accepted', 0, 1, NULL)",
        params![b"decode/source".to_vec()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chain_slots (
             source_id, sequence, slot_state, head_verified_fact_id,
             original_verified_fact_id, conflict_verified_fact_id, decisive_attempt_id
         ) VALUES (?1, 0, 'accepted', 1, NULL, NULL, NULL)",
        params![b"decode/source".to_vec()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO verified_facts (
             fact_id, source_id, sequence, doc_kind, parent_document_digest, raw_bytes,
             canonical_payload, canonical_root, canonical_state, canonical_signatures,
             canonical_envelope, document_digest, root_digest, state_digest,
             signature_set_digest, signature_message_digest
         ) VALUES (?1, ?2, 0, 'bootstrap', NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            1i64,
            b"decode/source".to_vec(),
            b"raw".to_vec(),
            b"payload".to_vec(),
            b"root".to_vec(),
            b"state".to_vec(),
            b"signatures".to_vec(),
            b"envelope".to_vec(),
            vec![1u8; 31],
            vec![2u8; 32],
            vec![3u8; 32],
            vec![4u8; 32],
            vec![5u8; 32],
        ],
    )
    .unwrap();

    conn
}
