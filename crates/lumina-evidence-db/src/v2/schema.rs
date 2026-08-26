use rusqlite::Connection;

use super::encoding::{sha256_domain, write_len_prefixed};
use super::error::{V2Error, V2Result};

pub const V2_SCHEMA_VERSION: &str = "lumina-evidence-db/pre-a0.2-v2.3-foundation";
pub const V2_SCHEMA_PARAMS: &str =
    "anchor=v2.3;content=v2;genesis=v2.3;intent=v2.3;legacy-checkpoint=v2.2;sidecar=v2.2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaObject {
    pub kind: &'static str,
    pub name: &'static str,
    pub table_name: &'static str,
    pub sql: &'static str,
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

    const fn trigger(name: &'static str, table_name: &'static str, sql: &'static str) -> Self {
        Self {
            kind: "trigger",
            name,
            table_name,
            sql,
        }
    }
}

pub fn schema_objects() -> &'static [SchemaObject] {
    &[
        SchemaObject::table(
            "v2_anchor_instance",
            "CREATE TABLE v2_anchor_instance (singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1), anchor_instance_id BLOB NOT NULL UNIQUE CHECK (length(anchor_instance_id) = 32))",
        ),
        SchemaObject::table(
            "v2_genesis",
            "CREATE TABLE v2_genesis (singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1), anchor_instance_id BLOB NOT NULL UNIQUE CHECK (length(anchor_instance_id) = 32), protocol_version TEXT NOT NULL CHECK (length(protocol_version) > 0), canonical_genesis_bytes BLOB NOT NULL, canonical_genesis_len INTEGER NOT NULL CHECK (canonical_genesis_len > 0 AND canonical_genesis_len = length(canonical_genesis_bytes)), genesis_digest BLOB NOT NULL UNIQUE CHECK (length(genesis_digest) = 32), stable_generation INTEGER NOT NULL CHECK (stable_generation = 0), content_generation INTEGER NOT NULL CHECK (content_generation = 0), work_generation INTEGER NOT NULL CHECK (work_generation = 0), stable_root BLOB NOT NULL CHECK (length(stable_root) = 32 AND stable_root = X'0000000000000000000000000000000000000000000000000000000000000000'), content_root BLOB NOT NULL CHECK (length(content_root) = 32 AND content_root = X'0000000000000000000000000000000000000000000000000000000000000000'), candidate_root BLOB CHECK (candidate_root IS NULL), operation_id BLOB CHECK (operation_id IS NULL), predecessor_head_digest BLOB CHECK (predecessor_head_digest IS NULL), predecessor_candidate_freeze INTEGER CHECK (predecessor_candidate_freeze IS NULL), plan_binding_digest BLOB CHECK (plan_binding_digest IS NULL), FOREIGN KEY (anchor_instance_id) REFERENCES v2_anchor_instance(anchor_instance_id))",
        ),
        SchemaObject::table(
            "v2_intents",
            "CREATE TABLE v2_intents (successor_head_digest BLOB PRIMARY KEY CHECK (length(successor_head_digest) = 32), protocol_version TEXT NOT NULL CHECK (length(protocol_version) > 0), canonical_successor_bytes BLOB NOT NULL UNIQUE, canonical_successor_len INTEGER NOT NULL CHECK (canonical_successor_len > 0 AND canonical_successor_len = length(canonical_successor_bytes)), successor_event_digest BLOB NOT NULL CHECK (length(successor_event_digest) = 32), event_value_offset INTEGER NOT NULL CHECK (event_value_offset >= 0), event_value_len INTEGER NOT NULL CHECK (event_value_len >= 0), prev_head_digest BLOB NOT NULL UNIQUE CHECK (length(prev_head_digest) = 32), intent_generation INTEGER NOT NULL CHECK (intent_generation >= 1), frame_sequence INTEGER NOT NULL CHECK (frame_sequence >= 1), predecessor_head_digest BLOB NOT NULL CHECK (length(predecessor_head_digest) = 32), predecessor_candidate_freeze INTEGER CHECK (predecessor_candidate_freeze IS NULL OR predecessor_candidate_freeze >= 0), plan_binding_digest BLOB CHECK (plan_binding_digest IS NULL OR length(plan_binding_digest) = 32), intent_state TEXT NOT NULL CHECK (intent_state IN ('authorized', 'applied')), stable_root BLOB NOT NULL CHECK (length(stable_root) = 32), content_root BLOB NOT NULL CHECK (length(content_root) = 32), CHECK (event_value_offset <= canonical_successor_len), CHECK (event_value_len <= canonical_successor_len), CHECK (event_value_offset + event_value_len <= canonical_successor_len))",
        ),
        SchemaObject::table(
            "content_events",
            "CREATE TABLE content_events (content_seq INTEGER PRIMARY KEY CHECK (content_seq >= 0), content_event_digest BLOB NOT NULL UNIQUE CHECK (length(content_event_digest) = 32), content_payload BLOB NOT NULL, UNIQUE (content_seq, content_event_digest))",
        ),
        SchemaObject::table(
            "checkpoint_operations",
            "CREATE TABLE checkpoint_operations (operation_id BLOB PRIMARY KEY CHECK (length(operation_id) = 32), anchor_instance_id BLOB NOT NULL CHECK (length(anchor_instance_id) = 32), path_binding BLOB NOT NULL CHECK (length(path_binding) = 32), key_epoch INTEGER NOT NULL CHECK (key_epoch >= 0), phase TEXT NOT NULL CHECK (phase IN ('prepare', 'pending', 'finalize', 'abort')), stable_generation INTEGER NOT NULL CHECK (stable_generation >= 0), content_generation INTEGER NOT NULL CHECK (content_generation >= stable_generation), operation_generation INTEGER NOT NULL UNIQUE CHECK (operation_generation >= 0), content_root BLOB NOT NULL CHECK (length(content_root) = 32), content_freeze INTEGER NOT NULL CHECK (content_freeze >= 0), candidate_root BLOB CHECK (candidate_root IS NULL OR length(candidate_root) = 32), candidate_freeze INTEGER CHECK (candidate_freeze IS NULL OR candidate_freeze >= 0), candidate_membership_digest BLOB CHECK (candidate_membership_digest IS NULL OR length(candidate_membership_digest) = 32), head_digest BLOB CHECK (head_digest IS NULL OR length(head_digest) = 32), prev_head_digest BLOB CHECK (prev_head_digest IS NULL OR length(prev_head_digest) = 32), UNIQUE (operation_id, candidate_membership_digest), CHECK ((phase IN ('prepare', 'pending') AND content_generation >= stable_generation AND candidate_root IS NOT NULL AND candidate_freeze IS NOT NULL AND candidate_membership_digest IS NOT NULL) OR (phase IN ('finalize', 'abort') AND content_generation = stable_generation AND candidate_root IS NULL AND candidate_freeze IS NULL AND candidate_membership_digest IS NULL)), CHECK (candidate_freeze IS NULL OR candidate_freeze >= content_freeze))",
        ),
        SchemaObject::table(
            "operation_content_membership",
            "CREATE TABLE operation_content_membership (operation_id BLOB NOT NULL CHECK (length(operation_id) = 32), content_seq INTEGER NOT NULL CHECK (content_seq >= 0), content_event_digest BLOB NOT NULL CHECK (length(content_event_digest) = 32), membership_row_digest BLOB NOT NULL UNIQUE CHECK (length(membership_row_digest) = 32), candidate_membership_digest BLOB NOT NULL CHECK (length(candidate_membership_digest) = 32), PRIMARY KEY (operation_id, content_seq), UNIQUE (content_seq), UNIQUE (content_event_digest), FOREIGN KEY (operation_id) REFERENCES checkpoint_operations(operation_id), FOREIGN KEY (operation_id, candidate_membership_digest) REFERENCES checkpoint_operations(operation_id, candidate_membership_digest), FOREIGN KEY (content_seq, content_event_digest) REFERENCES content_events(content_seq, content_event_digest))",
        ),
        SchemaObject::table(
            "checkpoint_events",
            "CREATE TABLE checkpoint_events (checkpoint_event_digest BLOB PRIMARY KEY CHECK (length(checkpoint_event_digest) = 32), event_kind TEXT NOT NULL CHECK (event_kind IN ('prepare', 'pending', 'finalize', 'abort')), anchor_instance_id BLOB NOT NULL CHECK (length(anchor_instance_id) = 32), path_binding BLOB NOT NULL CHECK (length(path_binding) = 32), key_epoch INTEGER NOT NULL CHECK (key_epoch >= 0), lifecycle TEXT NOT NULL CHECK (lifecycle IN ('stable', 'in_flight')), phase TEXT NOT NULL CHECK (phase IN ('prepare', 'pending', 'finalize', 'abort')), stable_generation INTEGER NOT NULL CHECK (stable_generation >= 0), content_generation INTEGER NOT NULL CHECK (content_generation >= stable_generation), operation_generation INTEGER NOT NULL CHECK (operation_generation >= 0), content_root BLOB NOT NULL CHECK (length(content_root) = 32), content_freeze INTEGER NOT NULL CHECK (content_freeze >= 0), candidate_root BLOB CHECK (candidate_root IS NULL OR length(candidate_root) = 32), candidate_freeze INTEGER CHECK (candidate_freeze IS NULL OR candidate_freeze >= 0), candidate_membership_digest BLOB CHECK (candidate_membership_digest IS NULL OR length(candidate_membership_digest) = 32), operation_id BLOB CHECK (operation_id IS NULL OR length(operation_id) = 32), head_digest BLOB NOT NULL UNIQUE CHECK (length(head_digest) = 32), prev_head_digest BLOB CHECK (prev_head_digest IS NULL OR length(prev_head_digest) = 32), CHECK (event_kind = phase), CHECK ((lifecycle = 'in_flight' AND phase IN ('prepare', 'pending') AND content_generation >= stable_generation AND candidate_root IS NOT NULL AND candidate_freeze IS NOT NULL AND candidate_membership_digest IS NOT NULL AND operation_id IS NOT NULL) OR (lifecycle = 'stable' AND phase IN ('finalize', 'abort') AND content_generation = stable_generation AND candidate_root IS NULL AND candidate_freeze IS NULL AND candidate_membership_digest IS NULL AND operation_id IS NULL)), CHECK (candidate_freeze IS NULL OR candidate_freeze >= content_freeze))",
        ),
        SchemaObject::table(
            "v2_anchor_state",
            "CREATE TABLE v2_anchor_state (singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1), anchor_instance_id BLOB NOT NULL CHECK (length(anchor_instance_id) = 32), path_binding BLOB NOT NULL CHECK (length(path_binding) = 32), key_epoch INTEGER NOT NULL CHECK (key_epoch >= 0), lifecycle TEXT NOT NULL CHECK (lifecycle IN ('stable', 'in_flight')), phase TEXT NOT NULL CHECK (phase IN ('prepare', 'pending', 'finalize', 'abort')), stable_generation INTEGER NOT NULL CHECK (stable_generation >= 0), content_generation INTEGER NOT NULL CHECK (content_generation >= stable_generation), operation_generation INTEGER NOT NULL CHECK (operation_generation >= 0), content_root BLOB NOT NULL CHECK (length(content_root) = 32), content_freeze INTEGER NOT NULL CHECK (content_freeze >= 0), candidate_root BLOB CHECK (candidate_root IS NULL OR length(candidate_root) = 32), candidate_freeze INTEGER CHECK (candidate_freeze IS NULL OR candidate_freeze >= 0), candidate_membership_digest BLOB CHECK (candidate_membership_digest IS NULL OR length(candidate_membership_digest) = 32), operation_id BLOB CHECK (operation_id IS NULL OR length(operation_id) = 32), terminal_checkpoint_digest BLOB NOT NULL CHECK (length(terminal_checkpoint_digest) = 32), prev_head_digest BLOB CHECK (prev_head_digest IS NULL OR length(prev_head_digest) = 32), head_digest BLOB NOT NULL CHECK (length(head_digest) = 32), CHECK ((lifecycle = 'in_flight' AND phase IN ('prepare', 'pending') AND content_generation >= stable_generation AND candidate_root IS NOT NULL AND candidate_freeze IS NOT NULL AND candidate_membership_digest IS NOT NULL AND operation_id IS NOT NULL) OR (lifecycle = 'stable' AND phase IN ('finalize', 'abort') AND content_generation = stable_generation AND candidate_root IS NULL AND candidate_freeze IS NULL AND candidate_membership_digest IS NULL AND operation_id IS NULL)), CHECK (candidate_freeze IS NULL OR candidate_freeze >= content_freeze))",
        ),
        SchemaObject::trigger(
            "v2_anchor_instance_no_update",
            "v2_anchor_instance",
            "CREATE TRIGGER v2_anchor_instance_no_update BEFORE UPDATE ON v2_anchor_instance BEGIN SELECT RAISE(ABORT, 'v2_anchor_instance_immutable'); END",
        ),
        SchemaObject::trigger(
            "v2_anchor_instance_no_delete",
            "v2_anchor_instance",
            "CREATE TRIGGER v2_anchor_instance_no_delete BEFORE DELETE ON v2_anchor_instance BEGIN SELECT RAISE(ABORT, 'v2_anchor_instance_immutable'); END",
        ),
        SchemaObject::trigger(
            "v2_genesis_no_update",
            "v2_genesis",
            "CREATE TRIGGER v2_genesis_no_update BEFORE UPDATE ON v2_genesis BEGIN SELECT RAISE(ABORT, 'v2_genesis_immutable'); END",
        ),
        SchemaObject::trigger(
            "v2_genesis_no_delete",
            "v2_genesis",
            "CREATE TRIGGER v2_genesis_no_delete BEFORE DELETE ON v2_genesis BEGIN SELECT RAISE(ABORT, 'v2_genesis_immutable'); END",
        ),
        SchemaObject::trigger(
            "v2_intents_no_update",
            "v2_intents",
            "CREATE TRIGGER v2_intents_no_update BEFORE UPDATE ON v2_intents BEGIN SELECT RAISE(ABORT, 'v2_intents_immutable'); END",
        ),
        SchemaObject::trigger(
            "v2_intents_no_delete",
            "v2_intents",
            "CREATE TRIGGER v2_intents_no_delete BEFORE DELETE ON v2_intents BEGIN SELECT RAISE(ABORT, 'v2_intents_immutable'); END",
        ),
        SchemaObject::trigger(
            "content_events_no_update",
            "content_events",
            "CREATE TRIGGER content_events_no_update BEFORE UPDATE ON content_events BEGIN SELECT RAISE(ABORT, 'content_events_append_only'); END",
        ),
        SchemaObject::trigger(
            "content_events_no_delete",
            "content_events",
            "CREATE TRIGGER content_events_no_delete BEFORE DELETE ON content_events BEGIN SELECT RAISE(ABORT, 'content_events_append_only'); END",
        ),
        SchemaObject::trigger(
            "checkpoint_operations_no_delete",
            "checkpoint_operations",
            "CREATE TRIGGER checkpoint_operations_no_delete BEFORE DELETE ON checkpoint_operations BEGIN SELECT RAISE(ABORT, 'checkpoint_operations_no_delete'); END",
        ),
        SchemaObject::trigger(
            "checkpoint_operations_monotonic",
            "checkpoint_operations",
            "CREATE TRIGGER checkpoint_operations_monotonic BEFORE UPDATE ON checkpoint_operations BEGIN SELECT CASE WHEN OLD.phase IN ('finalize', 'abort') THEN RAISE(ABORT, 'checkpoint_operations_terminal_immutable') WHEN OLD.operation_id != NEW.operation_id OR OLD.anchor_instance_id != NEW.anchor_instance_id OR OLD.path_binding != NEW.path_binding OR OLD.key_epoch != NEW.key_epoch THEN RAISE(ABORT, 'checkpoint_operations_identity_immutable') WHEN NEW.stable_generation < OLD.stable_generation OR NEW.content_generation < OLD.content_generation OR NEW.operation_generation < OLD.operation_generation OR NEW.content_freeze < OLD.content_freeze OR (OLD.candidate_freeze IS NOT NULL AND NEW.candidate_freeze IS NULL) OR (OLD.candidate_freeze IS NOT NULL AND NEW.candidate_freeze < OLD.candidate_freeze) THEN RAISE(ABORT, 'checkpoint_operations_monotonic') WHEN OLD.phase = 'prepare' AND NEW.phase = 'finalize' THEN RAISE(ABORT, 'checkpoint_operations_phase_skip') WHEN OLD.phase = 'pending' AND NEW.phase = 'prepare' THEN RAISE(ABORT, 'checkpoint_operations_phase_rewind') END; END",
        ),
        SchemaObject::trigger(
            "operation_content_membership_no_update",
            "operation_content_membership",
            "CREATE TRIGGER operation_content_membership_no_update BEFORE UPDATE ON operation_content_membership BEGIN SELECT RAISE(ABORT, 'operation_content_membership_append_only'); END",
        ),
        SchemaObject::trigger(
            "operation_content_membership_no_delete",
            "operation_content_membership",
            "CREATE TRIGGER operation_content_membership_no_delete BEFORE DELETE ON operation_content_membership BEGIN SELECT RAISE(ABORT, 'operation_content_membership_append_only'); END",
        ),
        SchemaObject::trigger(
            "checkpoint_events_no_update",
            "checkpoint_events",
            "CREATE TRIGGER checkpoint_events_no_update BEFORE UPDATE ON checkpoint_events BEGIN SELECT RAISE(ABORT, 'checkpoint_events_immutable'); END",
        ),
        SchemaObject::trigger(
            "checkpoint_events_no_delete",
            "checkpoint_events",
            "CREATE TRIGGER checkpoint_events_no_delete BEFORE DELETE ON checkpoint_events BEGIN SELECT RAISE(ABORT, 'checkpoint_events_immutable'); END",
        ),
        SchemaObject::trigger(
            "v2_anchor_state_no_delete",
            "v2_anchor_state",
            "CREATE TRIGGER v2_anchor_state_no_delete BEFORE DELETE ON v2_anchor_state BEGIN SELECT RAISE(ABORT, 'v2_anchor_state_no_delete'); END",
        ),
        SchemaObject::trigger(
            "v2_anchor_state_monotonic",
            "v2_anchor_state",
            "CREATE TRIGGER v2_anchor_state_monotonic BEFORE UPDATE ON v2_anchor_state BEGIN SELECT CASE WHEN OLD.singleton_id != NEW.singleton_id OR OLD.anchor_instance_id != NEW.anchor_instance_id OR OLD.path_binding != NEW.path_binding OR OLD.key_epoch != NEW.key_epoch THEN RAISE(ABORT, 'v2_anchor_state_identity_immutable') WHEN NEW.stable_generation < OLD.stable_generation OR NEW.content_generation < OLD.content_generation OR NEW.operation_generation < OLD.operation_generation OR NEW.content_freeze < OLD.content_freeze OR (OLD.candidate_freeze IS NOT NULL AND NEW.candidate_freeze IS NULL) OR (OLD.candidate_freeze IS NOT NULL AND NEW.candidate_freeze < OLD.candidate_freeze) THEN RAISE(ABORT, 'v2_anchor_state_monotonic') WHEN OLD.lifecycle = 'in_flight' AND OLD.phase = 'pending' AND NEW.phase = 'prepare' THEN RAISE(ABORT, 'v2_anchor_state_phase_rewind') END; END",
        ),
    ]
}

pub fn enforce_foreign_keys(conn: &Connection) -> V2Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    let enabled = conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?;
    if enabled != 1 {
        return Err(V2Error::ForeignKeysDisabled);
    }
    Ok(())
}

pub fn apply_catalog(conn: &Connection) -> V2Result<()> {
    enforce_foreign_keys(conn)?;
    for object in schema_objects() {
        conn.execute_batch(object.sql)?;
    }
    Ok(())
}

pub fn require_current_catalog(
    found_version: Option<&str>,
    found_params: Option<&str>,
) -> V2Result<()> {
    match (found_version, found_params) {
        (Some(version), Some(params))
            if version == V2_SCHEMA_VERSION && params == V2_SCHEMA_PARAMS =>
        {
            Ok(())
        }
        (None, None) => Err(V2Error::LegacyCatalogRejected),
        (version, params) => Err(V2Error::OfflineMigrationRequired {
            found_version: version.map(ToOwned::to_owned),
            found_params: params.map(ToOwned::to_owned),
        }),
    }
}

pub fn manifest_hash() -> V2Result<[u8; 32]> {
    let mut parts = vec![V2_SCHEMA_VERSION.as_bytes(), V2_SCHEMA_PARAMS.as_bytes()];
    for object in schema_objects() {
        parts.push(object.kind.as_bytes());
        parts.push(object.name.as_bytes());
        parts.push(object.table_name.as_bytes());
        parts.push(object.sql.as_bytes());
    }
    sha256_domain("lumina.evidence-db.v2.schema-manifest", &parts)
}

pub fn params_hash() -> V2Result<[u8; 32]> {
    sha256_domain(
        "lumina.evidence-db.v2.schema-params",
        &[V2_SCHEMA_PARAMS.as_bytes()],
    )
}

pub fn catalog_projection_bytes() -> V2Result<Vec<u8>> {
    let mut tuples: Vec<_> = schema_objects()
        .iter()
        .map(|object| {
            (
                object.kind.as_bytes().to_vec(),
                object.name.as_bytes().to_vec(),
                object.table_name.as_bytes().to_vec(),
                object.sql.as_bytes().to_vec(),
            )
        })
        .collect();
    tuples.sort_unstable();
    let mut out = Vec::new();
    for (kind, name, table_name, sql) in tuples {
        write_len_prefixed(&mut out, &kind)?;
        write_len_prefixed(&mut out, &name)?;
        write_len_prefixed(&mut out, &table_name)?;
        write_len_prefixed(&mut out, &sql)?;
    }
    Ok(out)
}

pub fn catalog_hash() -> V2Result<[u8; 32]> {
    sha256_domain(
        "lumina.evidence-db.v2.sqlite-schema",
        &[&catalog_projection_bytes()?],
    )
}

pub fn actual_catalog_hash(conn: &Connection) -> V2Result<[u8; 32]> {
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
    tuples.sort_unstable();
    let mut out = Vec::new();
    for (kind, name, table_name, sql) in tuples {
        write_len_prefixed(&mut out, &kind)?;
        write_len_prefixed(&mut out, &name)?;
        write_len_prefixed(&mut out, &table_name)?;
        write_len_prefixed(&mut out, &sql)?;
    }
    sha256_domain("lumina.evidence-db.v2.sqlite-schema", &[&out])
}
