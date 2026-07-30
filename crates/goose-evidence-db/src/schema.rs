use goose_verified_sources::{frame, DocKind, ErrorCode};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::{AnchorHeadRecord, AnchorState, DbError, ANCHOR_ROW_ID, SCHEMA_PARAMS, SCHEMA_VERSION};

pub(crate) fn configure_connection(conn: &mut Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA wal_autocheckpoint = 0;",
    )?;
    Ok(())
}

pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), DbError> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if count == 0 {
        create_schema(conn)?;
        return Ok(());
    }
    verify_schema(conn)
}

pub(crate) fn verify_schema(conn: &Connection) -> Result<(), DbError> {
    let expected_manifest_hash = schema_manifest_hash()?;
    let expected_params_hash = schema_params_hash()?;
    let expected_catalog_hash = schema_catalog_hash()?;
    let has_meta = conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_meta'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if has_meta.is_none() {
        return Err(DbError::LegacySchemaRejected);
    }
    let (manifest_hash, params_hash, catalog_hash): (Vec<u8>, Vec<u8>, Vec<u8>) = conn.query_row(
        "SELECT manifest_hash, params_hash, catalog_hash
         FROM schema_meta
         WHERE singleton_id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if manifest_hash != expected_manifest_hash.to_vec()
        || params_hash != expected_params_hash.to_vec()
        || catalog_hash != expected_catalog_hash.to_vec()
        || actual_catalog_hash(conn)?.to_vec() != expected_catalog_hash.to_vec()
    {
        return Err(DbError::SchemaFenceMismatch);
    }
    Ok(())
}

pub(crate) fn recreate_projection_schema(conn: &Connection) -> Result<(), DbError> {
    for name in [
        "chain_slots_no_delete",
        "chain_slots_monotonic",
        "source_heads_no_delete",
        "source_heads_monotonic",
    ] {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {name}"))?;
    }
    for name in ["source_heads", "chain_slots"] {
        conn.execute_batch(&format!("DROP TABLE IF EXISTS {name}"))?;
    }
    for object in schema_objects().iter().filter(|object| {
        matches!(
            object.name,
            "chain_slots"
                | "source_heads"
                | "chain_slots_no_delete"
                | "chain_slots_monotonic"
                | "source_heads_no_delete"
                | "source_heads_monotonic"
        )
    }) {
        conn.execute_batch(object.sql)?;
    }
    Ok(())
}

fn create_schema(conn: &Connection) -> Result<(), DbError> {
    for object in schema_objects() {
        conn.execute_batch(object.sql)?;
    }
    conn.execute(
        "INSERT INTO schema_meta (singleton_id, schema_version, manifest_hash, params_hash, catalog_hash)
         VALUES (1, ?1, ?2, ?3, ?4)",
        params![
            SCHEMA_VERSION,
            schema_manifest_hash()?.as_slice(),
            schema_params_hash()?.as_slice(),
            schema_catalog_hash()?.as_slice(),
        ],
    )?;
    verify_schema(conn)
}

pub(crate) fn ensure_anchor_row(conn: &Connection) -> Result<(), DbError> {
    let count = conn.query_row("SELECT COUNT(*) FROM anchor_state", [], |row| {
        row.get::<_, i64>(0)
    })?;
    if count == 0 {
        let zero = zero_anchor();
        conn.execute(
            "INSERT INTO anchor_state (
                 singleton_id,
                 state,
                 checkpoint_sequence,
                 freeze_journal_sequence,
                 root_hash,
                 prepare_token
             ) VALUES (?1, 'stable', 0, 0, ?2, NULL)",
            params![ANCHOR_ROW_ID, zero.root_hash.as_slice()],
        )?;
    }
    Ok(())
}

pub(crate) fn load_anchor_state_from_conn(conn: &Connection) -> Result<AnchorHeadRecord, DbError> {
    conn.query_row(
        "SELECT state, checkpoint_sequence, freeze_journal_sequence, root_hash, prepare_token
         FROM anchor_state
         WHERE singleton_id = ?1",
        params![ANCHOR_ROW_ID],
        |row| {
            let root = row.get::<_, Vec<u8>>(3)?;
            let token = row.get::<_, Option<Vec<u8>>>(4)?;
            Ok(AnchorHeadRecord {
                state: parse_anchor_state(&row.get::<_, String>(0)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                checkpoint_sequence: u64::try_from(row.get::<_, i64>(1)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                freeze_journal_sequence: u64::try_from(row.get::<_, i64>(2)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                root_hash: vec_to_fixed_32(root).map_err(|_| rusqlite::Error::InvalidQuery)?,
                prepare_token: token
                    .map(vec_to_fixed_32)
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
            })
        },
    )
    .map_err(DbError::from)
}

pub(crate) fn hash_parts(label: &str, parts: &[&[u8]]) -> Result<[u8; 32], DbError> {
    let mut encoded = Vec::new();
    for part in parts {
        let len = checked_frame_len(part.len())?;
        encoded.extend_from_slice(&len.to_be_bytes());
        encoded.extend_from_slice(part);
    }
    Ok(Sha256::digest(frame(label, &encoded)).into())
}

pub(crate) fn checked_frame_len(len: usize) -> Result<u32, DbError> {
    u32::try_from(len).map_err(|_| DbError::NumericOverflow)
}

pub(crate) fn zero_anchor() -> AnchorHeadRecord {
    AnchorHeadRecord {
        state: AnchorState::Stable,
        checkpoint_sequence: 0,
        freeze_journal_sequence: 0,
        root_hash: [0u8; 32],
        prepare_token: None,
    }
}

pub(crate) fn anchor_state_str(state: AnchorState) -> &'static str {
    match state {
        AnchorState::Stable => "stable",
        AnchorState::Pending => "pending",
    }
}

pub(crate) fn parse_anchor_state(value: &str) -> Result<AnchorState, DbError> {
    match value {
        "stable" => Ok(AnchorState::Stable),
        "pending" => Ok(AnchorState::Pending),
        _ => Err(DbError::ClosedIndeterminate),
    }
}

pub(crate) fn doc_kind_str(doc_kind: DocKind) -> &'static str {
    match doc_kind {
        DocKind::Bootstrap => "bootstrap",
        DocKind::Snapshot => "snapshot",
        DocKind::Rotation => "rotation",
        DocKind::Revocation => "revocation",
    }
}

pub(crate) fn parse_doc_kind(value: &str) -> Result<DocKind, DbError> {
    match value {
        "bootstrap" => Ok(DocKind::Bootstrap),
        "snapshot" => Ok(DocKind::Snapshot),
        "rotation" => Ok(DocKind::Rotation),
        "revocation" => Ok(DocKind::Revocation),
        _ => Err(DbError::ClosedIndeterminate),
    }
}

pub(crate) fn error_code_str(value: ErrorCode) -> &'static str {
    match value {
        ErrorCode::InvalidUtf8 => "invalid_utf8",
        ErrorCode::Utf8BomNotAllowed => "utf8_bom_not_allowed",
        ErrorCode::TrailingBytes => "trailing_bytes",
        ErrorCode::UnexpectedEnd => "unexpected_end",
        ErrorCode::UnexpectedToken => "unexpected_token",
        ErrorCode::MaxBytesExceeded => "max_bytes_exceeded",
        ErrorCode::MaxDepthExceeded => "max_depth_exceeded",
        ErrorCode::MaxFieldsExceeded => "max_fields_exceeded",
        ErrorCode::MaxArrayItemsExceeded => "max_array_items_exceeded",
        ErrorCode::MaxStringBytesExceeded => "max_string_bytes_exceeded",
        ErrorCode::DuplicateKey => "duplicate_key",
        ErrorCode::BooleanNotAllowed => "boolean_not_allowed",
        ErrorCode::InvalidNumber => "invalid_number",
        ErrorCode::NumberOutOfRange => "number_out_of_range",
        ErrorCode::WrongType => "wrong_type",
        ErrorCode::MissingField => "missing_field",
        ErrorCode::UnknownField => "unknown_field",
        ErrorCode::InvalidAscii => "invalid_ascii",
        ErrorCode::InvalidDocKind => "invalid_doc_kind",
        ErrorCode::InvalidWireVersion => "invalid_wire_version",
        ErrorCode::InvalidParentDigest => "invalid_parent_digest",
        ErrorCode::InvalidSequence => "invalid_sequence",
        ErrorCode::InvalidKid => "invalid_kid",
        ErrorCode::InvalidBase64 => "invalid_base64",
        ErrorCode::InvalidSpki => "invalid_spki",
        ErrorCode::InvalidSignature => "invalid_signature",
        ErrorCode::InvalidQuorum => "invalid_quorum",
        ErrorCode::TooManyRootKeys => "too_many_root_keys",
        ErrorCode::TooManySignatures => "too_many_signatures",
        ErrorCode::UnsortedArray => "unsorted_array",
        ErrorCode::DuplicateArrayEntry => "duplicate_array_entry",
        ErrorCode::InvalidSourceId => "invalid_source_id",
        ErrorCode::InvalidReleaseId => "invalid_release_id",
        ErrorCode::InvalidMcpId => "invalid_mcp_id",
        ErrorCode::InvalidVersion => "invalid_version",
        ErrorCode::UnknownSignatureKey => "unknown_signature_key",
        ErrorCode::SignatureQuorumNotMet => "signature_quorum_not_met",
        ErrorCode::BootstrapConflict => "bootstrap_conflict",
        ErrorCode::MissingParent => "missing_parent",
        ErrorCode::LookupFork => "lookup_fork",
        ErrorCode::SequenceFork => "sequence_fork",
        ErrorCode::ParentDigestMismatch => "parent_digest_mismatch",
        ErrorCode::TransitionViolation => "transition_violation",
    }
}

pub(crate) fn parse_error_code(value: &str) -> Result<ErrorCode, DbError> {
    match value {
        "invalid_utf8" => Ok(ErrorCode::InvalidUtf8),
        "utf8_bom_not_allowed" => Ok(ErrorCode::Utf8BomNotAllowed),
        "trailing_bytes" => Ok(ErrorCode::TrailingBytes),
        "unexpected_end" => Ok(ErrorCode::UnexpectedEnd),
        "unexpected_token" => Ok(ErrorCode::UnexpectedToken),
        "max_bytes_exceeded" => Ok(ErrorCode::MaxBytesExceeded),
        "max_depth_exceeded" => Ok(ErrorCode::MaxDepthExceeded),
        "max_fields_exceeded" => Ok(ErrorCode::MaxFieldsExceeded),
        "max_array_items_exceeded" => Ok(ErrorCode::MaxArrayItemsExceeded),
        "max_string_bytes_exceeded" => Ok(ErrorCode::MaxStringBytesExceeded),
        "duplicate_key" => Ok(ErrorCode::DuplicateKey),
        "boolean_not_allowed" => Ok(ErrorCode::BooleanNotAllowed),
        "invalid_number" => Ok(ErrorCode::InvalidNumber),
        "number_out_of_range" => Ok(ErrorCode::NumberOutOfRange),
        "wrong_type" => Ok(ErrorCode::WrongType),
        "missing_field" => Ok(ErrorCode::MissingField),
        "unknown_field" => Ok(ErrorCode::UnknownField),
        "invalid_ascii" => Ok(ErrorCode::InvalidAscii),
        "invalid_doc_kind" => Ok(ErrorCode::InvalidDocKind),
        "invalid_wire_version" => Ok(ErrorCode::InvalidWireVersion),
        "invalid_parent_digest" => Ok(ErrorCode::InvalidParentDigest),
        "invalid_sequence" => Ok(ErrorCode::InvalidSequence),
        "invalid_kid" => Ok(ErrorCode::InvalidKid),
        "invalid_base64" => Ok(ErrorCode::InvalidBase64),
        "invalid_spki" => Ok(ErrorCode::InvalidSpki),
        "invalid_signature" => Ok(ErrorCode::InvalidSignature),
        "invalid_quorum" => Ok(ErrorCode::InvalidQuorum),
        "too_many_root_keys" => Ok(ErrorCode::TooManyRootKeys),
        "too_many_signatures" => Ok(ErrorCode::TooManySignatures),
        "unsorted_array" => Ok(ErrorCode::UnsortedArray),
        "duplicate_array_entry" => Ok(ErrorCode::DuplicateArrayEntry),
        "invalid_source_id" => Ok(ErrorCode::InvalidSourceId),
        "invalid_release_id" => Ok(ErrorCode::InvalidReleaseId),
        "invalid_mcp_id" => Ok(ErrorCode::InvalidMcpId),
        "invalid_version" => Ok(ErrorCode::InvalidVersion),
        "unknown_signature_key" => Ok(ErrorCode::UnknownSignatureKey),
        "signature_quorum_not_met" => Ok(ErrorCode::SignatureQuorumNotMet),
        "bootstrap_conflict" => Ok(ErrorCode::BootstrapConflict),
        "missing_parent" => Ok(ErrorCode::MissingParent),
        "lookup_fork" => Ok(ErrorCode::LookupFork),
        "sequence_fork" => Ok(ErrorCode::SequenceFork),
        "parent_digest_mismatch" => Ok(ErrorCode::ParentDigestMismatch),
        "transition_violation" => Ok(ErrorCode::TransitionViolation),
        _ => Err(DbError::ClosedIndeterminate),
    }
}

struct SchemaObject {
    kind: &'static str,
    name: &'static str,
    table_name: &'static str,
    sql: &'static str,
}

impl SchemaObject {
    const fn table(name: &'static str, sql: &'static str) -> Self {
        Self {
            kind: "table",
            name,
            table_name: name,
            sql,
        }
    }

    const fn index(name: &'static str, table_name: &'static str, sql: &'static str) -> Self {
        Self {
            kind: "index",
            name,
            table_name,
            sql,
        }
    }

    const fn trigger(name: &'static str, table_name: &'static str, sql: &'static str) -> Self {
        Self {
            kind: "trigger",
            name,
            table_name,
            sql,
        }
    }
}

fn schema_objects() -> &'static [SchemaObject] {
    &[
        SchemaObject::table("schema_meta", "CREATE TABLE schema_meta (singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1), schema_version TEXT NOT NULL, manifest_hash BLOB NOT NULL CHECK (length(manifest_hash) = 32), params_hash BLOB NOT NULL CHECK (length(params_hash) = 32), catalog_hash BLOB NOT NULL CHECK (length(catalog_hash) = 32))"),
        SchemaObject::table("anchor_state", "CREATE TABLE anchor_state (singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1), state TEXT NOT NULL CHECK (state IN ('stable', 'pending')), checkpoint_sequence INTEGER NOT NULL CHECK (checkpoint_sequence >= 0), freeze_journal_sequence INTEGER NOT NULL CHECK (freeze_journal_sequence >= 0), root_hash BLOB NOT NULL CHECK (length(root_hash) = 32), prepare_token BLOB CHECK (prepare_token IS NULL OR length(prepare_token) = 32))"),
        SchemaObject::table("verified_facts", "CREATE TABLE verified_facts (fact_id INTEGER PRIMARY KEY AUTOINCREMENT, source_id BLOB NOT NULL, sequence INTEGER NOT NULL CHECK (sequence >= 0), doc_kind TEXT NOT NULL, parent_document_digest BLOB, raw_bytes BLOB NOT NULL, canonical_payload BLOB NOT NULL, canonical_root BLOB NOT NULL, canonical_state BLOB NOT NULL, canonical_signatures BLOB NOT NULL, canonical_envelope BLOB NOT NULL, document_digest BLOB NOT NULL CHECK (length(document_digest) = 32), root_digest BLOB NOT NULL CHECK (length(root_digest) = 32), state_digest BLOB NOT NULL CHECK (length(state_digest) = 32), signature_set_digest BLOB NOT NULL CHECK (length(signature_set_digest) = 32), signature_message_digest BLOB NOT NULL CHECK (length(signature_message_digest) = 32))"),
        SchemaObject::table("ingest_attempts", "CREATE TABLE ingest_attempts (attempt_id INTEGER PRIMARY KEY AUTOINCREMENT, raw_hash BLOB NOT NULL CHECK (length(raw_hash) = 32), raw_total_len INTEGER NOT NULL CHECK (raw_total_len >= 0), raw_truncated INTEGER NOT NULL CHECK (raw_truncated IN (0, 1)), raw_evidence BLOB NOT NULL, disposition TEXT NOT NULL CHECK (disposition IN ('parse_rejected', 'accepted', 'duplicate', 'equivalent_variant', 'forked', 'chain_rejected')), parse_error_code TEXT, chain_error_code TEXT, source_id BLOB, sequence INTEGER CHECK (sequence IS NULL OR sequence >= 0), doc_kind TEXT CHECK (doc_kind IS NULL OR doc_kind IN ('bootstrap', 'snapshot', 'rotation', 'revocation')), parent_document_digest BLOB CHECK (parent_document_digest IS NULL OR length(parent_document_digest) = 32), canonical_payload BLOB, canonical_root BLOB, canonical_state BLOB, canonical_signatures BLOB, canonical_envelope BLOB, document_digest BLOB CHECK (document_digest IS NULL OR length(document_digest) = 32), root_digest BLOB CHECK (root_digest IS NULL OR length(root_digest) = 32), state_digest BLOB CHECK (state_digest IS NULL OR length(state_digest) = 32), signature_set_digest BLOB CHECK (signature_set_digest IS NULL OR length(signature_set_digest) = 32), signature_message_digest BLOB CHECK (signature_message_digest IS NULL OR length(signature_message_digest) = 32), verified_fact_id INTEGER, FOREIGN KEY (verified_fact_id) REFERENCES verified_facts(fact_id), CHECK ((disposition = 'parse_rejected' AND parse_error_code IS NOT NULL AND chain_error_code IS NULL AND source_id IS NULL AND sequence IS NULL AND doc_kind IS NULL AND parent_document_digest IS NULL AND canonical_payload IS NULL AND canonical_root IS NULL AND canonical_state IS NULL AND canonical_signatures IS NULL AND canonical_envelope IS NULL AND document_digest IS NULL AND root_digest IS NULL AND state_digest IS NULL AND signature_set_digest IS NULL AND signature_message_digest IS NULL AND verified_fact_id IS NULL) OR (disposition IN ('accepted', 'duplicate', 'equivalent_variant') AND parse_error_code IS NULL AND chain_error_code IS NULL AND source_id IS NOT NULL AND sequence IS NOT NULL AND doc_kind IS NOT NULL AND canonical_payload IS NOT NULL AND canonical_root IS NOT NULL AND canonical_state IS NOT NULL AND canonical_signatures IS NOT NULL AND canonical_envelope IS NOT NULL AND document_digest IS NOT NULL AND root_digest IS NOT NULL AND state_digest IS NOT NULL AND signature_set_digest IS NOT NULL AND signature_message_digest IS NOT NULL AND verified_fact_id IS NOT NULL) OR (disposition = 'forked' AND parse_error_code IS NULL AND chain_error_code = 'sequence_fork' AND source_id IS NOT NULL AND sequence IS NOT NULL AND doc_kind IS NOT NULL AND canonical_payload IS NOT NULL AND canonical_root IS NOT NULL AND canonical_state IS NOT NULL AND canonical_signatures IS NOT NULL AND canonical_envelope IS NOT NULL AND document_digest IS NOT NULL AND root_digest IS NOT NULL AND state_digest IS NOT NULL AND signature_set_digest IS NOT NULL AND signature_message_digest IS NOT NULL AND verified_fact_id IS NOT NULL) OR (disposition = 'chain_rejected' AND parse_error_code IS NULL AND chain_error_code IS NOT NULL AND chain_error_code != 'sequence_fork' AND source_id IS NOT NULL AND sequence IS NOT NULL AND doc_kind IS NOT NULL AND canonical_payload IS NOT NULL AND canonical_root IS NOT NULL AND canonical_state IS NOT NULL AND canonical_signatures IS NOT NULL AND canonical_envelope IS NOT NULL AND document_digest IS NOT NULL AND root_digest IS NOT NULL AND state_digest IS NOT NULL AND signature_set_digest IS NOT NULL AND signature_message_digest IS NOT NULL AND verified_fact_id IS NULL)))"),
        SchemaObject::table("journal_events", "CREATE TABLE journal_events (journal_seq INTEGER PRIMARY KEY AUTOINCREMENT, event_type TEXT NOT NULL CHECK (event_type IN ('accepted', 'duplicate', 'equivalent_variant', 'forked', 'parse_rejected', 'chain_rejected', 'checkpoint_prepare', 'checkpoint_pending', 'checkpoint_finalize')), attempt_id INTEGER, source_id BLOB, sequence INTEGER CHECK (sequence IS NULL OR sequence >= 0), verified_fact_id INTEGER, related_verified_fact_id INTEGER, error_code TEXT, event_root BLOB NOT NULL CHECK (length(event_root) = 32), FOREIGN KEY (attempt_id) REFERENCES ingest_attempts(attempt_id), FOREIGN KEY (verified_fact_id) REFERENCES verified_facts(fact_id), FOREIGN KEY (related_verified_fact_id) REFERENCES verified_facts(fact_id), CHECK ((event_type IN ('accepted', 'duplicate', 'equivalent_variant') AND attempt_id IS NOT NULL AND source_id IS NOT NULL AND sequence IS NOT NULL AND verified_fact_id IS NOT NULL AND related_verified_fact_id IS NULL AND error_code IS NULL) OR (event_type = 'forked' AND attempt_id IS NOT NULL AND source_id IS NOT NULL AND sequence IS NOT NULL AND verified_fact_id IS NOT NULL AND related_verified_fact_id IS NOT NULL AND error_code IS NULL) OR (event_type = 'parse_rejected' AND attempt_id IS NOT NULL AND source_id IS NULL AND sequence IS NULL AND verified_fact_id IS NULL AND related_verified_fact_id IS NULL AND error_code IS NOT NULL) OR (event_type = 'chain_rejected' AND attempt_id IS NOT NULL AND source_id IS NOT NULL AND sequence IS NOT NULL AND verified_fact_id IS NULL AND related_verified_fact_id IS NULL AND error_code IS NOT NULL) OR (event_type IN ('checkpoint_prepare', 'checkpoint_pending', 'checkpoint_finalize') AND attempt_id IS NULL AND source_id IS NULL AND sequence IS NOT NULL AND verified_fact_id IS NULL AND related_verified_fact_id IS NULL AND error_code IS NULL)))"),
        SchemaObject::table("chain_slots", "CREATE TABLE chain_slots (source_id BLOB NOT NULL, sequence INTEGER NOT NULL CHECK (sequence >= 0), slot_state TEXT NOT NULL CHECK (slot_state IN ('accepted', 'forked')), head_verified_fact_id INTEGER, original_verified_fact_id INTEGER, conflict_verified_fact_id INTEGER, decisive_attempt_id INTEGER, PRIMARY KEY (source_id, sequence), FOREIGN KEY (head_verified_fact_id) REFERENCES verified_facts(fact_id), FOREIGN KEY (original_verified_fact_id) REFERENCES verified_facts(fact_id), FOREIGN KEY (conflict_verified_fact_id) REFERENCES verified_facts(fact_id), FOREIGN KEY (decisive_attempt_id) REFERENCES ingest_attempts(attempt_id), CHECK ((slot_state = 'accepted' AND head_verified_fact_id IS NOT NULL AND original_verified_fact_id IS NULL AND conflict_verified_fact_id IS NULL AND decisive_attempt_id IS NULL) OR (slot_state = 'forked' AND head_verified_fact_id IS NULL AND original_verified_fact_id IS NOT NULL AND conflict_verified_fact_id IS NOT NULL AND decisive_attempt_id IS NOT NULL)))"),
        SchemaObject::table("source_heads", "CREATE TABLE source_heads (source_id BLOB PRIMARY KEY, head_state TEXT NOT NULL CHECK (head_state IN ('accepted', 'forked')), head_sequence INTEGER NOT NULL CHECK (head_sequence >= 0), head_verified_fact_id INTEGER, forked_slot_sequence INTEGER, FOREIGN KEY (head_verified_fact_id) REFERENCES verified_facts(fact_id), CHECK ((head_state = 'accepted' AND head_verified_fact_id IS NOT NULL AND forked_slot_sequence IS NULL) OR (head_state = 'forked' AND head_verified_fact_id IS NULL AND forked_slot_sequence IS NOT NULL)))"),
        SchemaObject::index("verified_facts_identity_idx", "verified_facts", "CREATE INDEX verified_facts_identity_idx ON verified_facts (source_id, sequence, document_digest, signature_set_digest)"),
        SchemaObject::trigger("schema_meta_no_update", "schema_meta", "CREATE TRIGGER schema_meta_no_update BEFORE UPDATE ON schema_meta BEGIN SELECT RAISE(ABORT, 'schema_meta_immutable'); END"),
        SchemaObject::trigger("schema_meta_no_delete", "schema_meta", "CREATE TRIGGER schema_meta_no_delete BEFORE DELETE ON schema_meta BEGIN SELECT RAISE(ABORT, 'schema_meta_immutable'); END"),
        SchemaObject::trigger("ingest_attempts_no_update", "ingest_attempts", "CREATE TRIGGER ingest_attempts_no_update BEFORE UPDATE ON ingest_attempts BEGIN SELECT RAISE(ABORT, 'ingest_attempts_immutable'); END"),
        SchemaObject::trigger("ingest_attempts_no_delete", "ingest_attempts", "CREATE TRIGGER ingest_attempts_no_delete BEFORE DELETE ON ingest_attempts BEGIN SELECT RAISE(ABORT, 'ingest_attempts_immutable'); END"),
        SchemaObject::trigger("verified_facts_no_update", "verified_facts", "CREATE TRIGGER verified_facts_no_update BEFORE UPDATE ON verified_facts BEGIN SELECT RAISE(ABORT, 'verified_facts_immutable'); END"),
        SchemaObject::trigger("verified_facts_no_delete", "verified_facts", "CREATE TRIGGER verified_facts_no_delete BEFORE DELETE ON verified_facts BEGIN SELECT RAISE(ABORT, 'verified_facts_immutable'); END"),
        SchemaObject::trigger("journal_events_no_update", "journal_events", "CREATE TRIGGER journal_events_no_update BEFORE UPDATE ON journal_events BEGIN SELECT RAISE(ABORT, 'journal_events_immutable'); END"),
        SchemaObject::trigger("journal_events_no_delete", "journal_events", "CREATE TRIGGER journal_events_no_delete BEFORE DELETE ON journal_events BEGIN SELECT RAISE(ABORT, 'journal_events_immutable'); END"),
        SchemaObject::trigger("chain_slots_no_delete", "chain_slots", "CREATE TRIGGER chain_slots_no_delete BEFORE DELETE ON chain_slots BEGIN SELECT RAISE(ABORT, 'chain_slots_no_delete'); END"),
        SchemaObject::trigger("chain_slots_monotonic", "chain_slots", "CREATE TRIGGER chain_slots_monotonic BEFORE UPDATE ON chain_slots BEGIN SELECT CASE WHEN OLD.slot_state = 'forked' AND (NEW.slot_state != 'forked' OR OLD.head_verified_fact_id IS NOT NEW.head_verified_fact_id OR OLD.original_verified_fact_id IS NOT NEW.original_verified_fact_id OR OLD.conflict_verified_fact_id IS NOT NEW.conflict_verified_fact_id OR OLD.decisive_attempt_id IS NOT NEW.decisive_attempt_id) THEN RAISE(ABORT, 'chain_slots_fork_immutable') WHEN OLD.slot_state = 'accepted' AND NEW.slot_state = 'accepted' AND OLD.head_verified_fact_id != NEW.head_verified_fact_id THEN RAISE(ABORT, 'chain_slots_head_change') END; END"),
        SchemaObject::trigger("source_heads_no_delete", "source_heads", "CREATE TRIGGER source_heads_no_delete BEFORE DELETE ON source_heads BEGIN SELECT RAISE(ABORT, 'source_heads_no_delete'); END"),
        SchemaObject::trigger("source_heads_monotonic", "source_heads", "CREATE TRIGGER source_heads_monotonic BEFORE UPDATE ON source_heads BEGIN SELECT CASE WHEN NEW.head_sequence < OLD.head_sequence THEN RAISE(ABORT, 'source_heads_rewind') WHEN OLD.head_state = 'forked' AND (NEW.head_state != 'forked' OR NEW.head_sequence != OLD.head_sequence OR OLD.head_verified_fact_id IS NOT NEW.head_verified_fact_id OR NEW.forked_slot_sequence != OLD.forked_slot_sequence) THEN RAISE(ABORT, 'source_heads_fork_immutable') WHEN OLD.head_state = 'accepted' AND NEW.head_state = 'accepted' AND NEW.head_sequence = OLD.head_sequence AND NEW.head_verified_fact_id != OLD.head_verified_fact_id THEN RAISE(ABORT, 'source_heads_head_change') END; END"),
    ]
}

fn schema_manifest_hash() -> Result<[u8; 32], DbError> {
    let mut parts = vec![SCHEMA_VERSION.as_bytes(), SCHEMA_PARAMS.as_bytes()];
    for object in schema_objects() {
        parts.push(object.kind.as_bytes());
        parts.push(object.name.as_bytes());
        parts.push(object.table_name.as_bytes());
        parts.push(object.sql.as_bytes());
    }
    hash_parts("evidence-db.schema-manifest", &parts)
}

fn schema_params_hash() -> Result<[u8; 32], DbError> {
    hash_parts("evidence-db.schema-params", &[SCHEMA_PARAMS.as_bytes()])
}

fn schema_catalog_hash() -> Result<[u8; 32], DbError> {
    let projection = schema_catalog_projection_bytes()?;
    hash_parts("evidence-db.sqlite-schema", &[&projection])
}

fn actual_catalog_hash(conn: &Connection) -> Result<[u8; 32], DbError> {
    let tuples = load_catalog_tuples(conn)?;
    let projection = catalog_projection_bytes(&tuples)?;
    hash_parts("evidence-db.sqlite-schema", &[&projection])
}

fn schema_catalog_projection_bytes() -> Result<Vec<u8>, DbError> {
    let conn = Connection::open_in_memory()?;
    for object in schema_objects() {
        conn.execute_batch(object.sql)?;
    }
    let tuples = load_catalog_tuples(&conn)?;
    catalog_projection_bytes(&tuples)
}

fn load_catalog_tuples(
    conn: &Connection,
) -> Result<Vec<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)>, DbError> {
    let mut statement = conn.prepare(
        "SELECT type, name, tbl_name, sql
         FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%' AND sql IS NOT NULL",
    )?;
    let mut rows = statement.query([])?;
    let mut tuples = Vec::new();
    while let Some(row) = rows.next()? {
        tuples.push((
            row.get::<_, String>(0)?.into_bytes(),
            row.get::<_, String>(1)?.into_bytes(),
            row.get::<_, String>(2)?.into_bytes(),
            row.get::<_, String>(3)?.into_bytes(),
        ));
    }
    Ok(tuples)
}

fn catalog_projection_bytes(
    tuples: &[(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)],
) -> Result<Vec<u8>, DbError> {
    let mut tuples = tuples.to_vec();
    tuples.sort_unstable();
    let mut out = Vec::new();
    for (kind, name, table_name, sql) in tuples {
        write_len_prefixed(&mut out, kind)?;
        write_len_prefixed(&mut out, name)?;
        write_len_prefixed(&mut out, table_name)?;
        write_len_prefixed(&mut out, sql)?;
    }
    Ok(out)
}

fn write_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), DbError> {
    let len = checked_frame_len(bytes.len())?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn vec_to_fixed_32(bytes: Vec<u8>) -> Result<[u8; 32], DbError> {
    if bytes.len() != 32 {
        return Err(DbError::ClosedIndeterminate);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}
