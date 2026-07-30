use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use super::encoding::sha256_domain;
use super::error::{FoundationWriteRecordKind, FoundationWriteResolution, V2Error};
use super::schema::{
    actual_catalog_hash, apply_catalog, catalog_hash as public_catalog_hash, enforce_foreign_keys,
    manifest_hash as public_manifest_hash, params_hash as public_params_hash,
    require_current_catalog, V2_SCHEMA_PARAMS, V2_SCHEMA_VERSION,
};
use super::{
    AnchorIdentity, AnchorInstanceId, CandidateMembership, CheckpointEvent, CheckpointEventDigest,
    CheckpointEventKind, ContentEvent, ContentEventDigest, ContentRootDigest, GenesisRecord, Head,
    HeadCore, HeadDigest, HeadLifecycle, IntentRecord, IntentState, MemberDigest,
    OperationContentMembership, OperationId, PlanBindingDigest, V2Result,
};
use crate::DbError;

const STORE_SINGLETON_ID: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendedContent {
    pub event: ContentEvent,
    pub digest: ContentEventDigest,
    pub allocator_watermark: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedIntentPageEntry {
    pub rowid: u64,
    pub record: IntentRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCheckpoint {
    pub operation_id: OperationId,
    pub operation_generation: u64,
    pub stable_generation: u64,
    pub visible_frontier: u64,
    pub allocator_watermark: u64,
    pub head: Head,
    pub head_digest: HeadDigest,
    pub checkpoint_event: CheckpointEvent,
    pub membership: CandidateMembership,
    pub content_events: Vec<ContentEvent>,
}

#[derive(Debug)]
pub struct V2Store {
    db_path: PathBuf,
    conn: Option<Connection>,
    #[cfg(test)]
    test_write_fault: Option<TestWriteFault>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreSchemaObject {
    kind: &'static str,
    name: &'static str,
    table_name: &'static str,
    sql: &'static str,
}

impl StoreSchemaObject {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredAnchorState {
    head: Head,
    head_digest: HeadDigest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredCheckpointEvent {
    digest: CheckpointEventDigest,
    head: Head,
    event: CheckpointEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredOperation {
    row_operation_id: OperationId,
    head: Head,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredMembershipRow {
    row: OperationContentMembership,
    membership_row_digest: MemberDigest,
    candidate_membership_digest: MemberDigest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StoreAnchorView {
    Genesis,
    Stable(StoredAnchorState),
    RecoveryRequired { phase: CheckpointEventKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreOpenMode {
    Create,
    Existing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestWriteFault {
    CommitUnknown,
    RollbackUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoundationWriteFinalize {
    Commit,
    Rollback,
}

pub(crate) fn install_v2_store_catalog(conn: &Connection) -> V2Result<()> {
    for object in store_schema_objects() {
        conn.execute_batch(object.sql)?;
    }
    Ok(())
}

impl V2Store {
    pub fn create_new(path: impl AsRef<Path>) -> V2Result<Self> {
        let (db_path, conn) = open_verified_store_connection(path.as_ref(), StoreOpenMode::Create)?;
        configure_store_connection(&conn)?;
        let schema_count = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        if schema_count != 0 {
            return Err(V2Error::DatabaseAlreadyInitialized);
        }
        apply_catalog(&conn)?;
        install_v2_store_catalog(&conn)?;
        seed_schema_meta(&conn)?;
        seed_allocator_state(&conn)?;
        verify_store_catalog(&conn)?;
        Ok(Self {
            db_path,
            conn: Some(conn),
            #[cfg(test)]
            test_write_fault: None,
        })
    }

    pub fn open_existing(path: impl AsRef<Path>) -> V2Result<Self> {
        let (db_path, conn) =
            open_verified_store_connection(path.as_ref(), StoreOpenMode::Existing)?;
        configure_store_connection(&conn)?;
        verify_store_catalog(&conn)?;
        if let StoreAnchorView::RecoveryRequired { phase } = validate_store_state(&conn)? {
            return Err(V2Error::RecoveryRequired {
                phase: phase_str(phase),
            });
        }
        Ok(Self {
            db_path,
            conn: Some(conn),
            #[cfg(test)]
            test_write_fault: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.db_path
    }

    fn connection(&self) -> V2Result<&Connection> {
        self.conn
            .as_ref()
            .ok_or(V2Error::StoreUnavailableAfterConnectionLoss)
    }

    fn take_connection(&mut self) -> V2Result<Connection> {
        self.conn
            .take()
            .ok_or(V2Error::StoreUnavailableAfterConnectionLoss)
    }

    fn run_write_operation<T, Body, CommitUnknown, RollbackUnknown>(
        &mut self,
        body: Body,
        commit_unknown: CommitUnknown,
        rollback_unknown: RollbackUnknown,
    ) -> V2Result<T>
    where
        Body: FnOnce(&Connection) -> V2Result<T>,
        CommitUnknown: FnOnce(&Path) -> V2Error,
        RollbackUnknown: FnOnce(&Path) -> V2Error,
    {
        let db_path = self.db_path.clone();
        let mut conn = self.take_connection()?;
        let mut commit_unknown = Some(commit_unknown);
        let mut rollback_unknown = Some(rollback_unknown);

        if let Err(error) = conn.execute_batch("BEGIN IMMEDIATE") {
            self.conn = Some(conn);
            return Err(V2Error::Sqlite(error));
        }

        let test_write_fault = take_test_write_fault(self);
        let body_result = match (body(&conn), test_write_fault) {
            (Ok(_), Some(TestWriteFault::RollbackUnknown)) => {
                Err(V2Error::AuthorityInvariantViolation {
                    context: "forced_rollback_unknown_for_test",
                })
            }
            (result, _) => result,
        };

        match body_result {
            Ok(value) => {
                let commit_result = conn.execute_batch("COMMIT");
                if commit_result.is_ok()
                    && matches!(test_write_fault, Some(TestWriteFault::CommitUnknown))
                {
                    drop(conn);
                    self.conn = None;
                    return Err(commit_unknown.take().unwrap()(db_path.as_path()));
                }
                match commit_result {
                    Ok(()) => {
                        self.conn = Some(conn);
                        Ok(value)
                    }
                    Err(_) => {
                        drop(conn);
                        self.conn = None;
                        Err(commit_unknown.take().unwrap()(db_path.as_path()))
                    }
                }
            }
            Err(error) => {
                let rollback_result = conn.execute_batch("ROLLBACK");
                if rollback_result.is_ok()
                    && matches!(test_write_fault, Some(TestWriteFault::RollbackUnknown))
                {
                    drop(conn);
                    self.conn = None;
                    return Err(rollback_unknown.take().unwrap()(db_path.as_path()));
                }
                match rollback_result {
                    Ok(()) => {
                        self.conn = Some(conn);
                        Err(error)
                    }
                    Err(_) => {
                        drop(conn);
                        self.conn = None;
                        Err(rollback_unknown.take().unwrap()(db_path.as_path()))
                    }
                }
            }
        }
    }

    pub fn save_foundation_genesis_record(&mut self, record: &GenesisRecord) -> V2Result<()> {
        record.validate()?;
        let commit_record = record.clone();
        let rollback_record = record.clone();
        self.run_write_operation(
            |conn| {
                ensure_foundation_anchor_instance(conn, record.anchor_instance_id)?;
                conn.execute(
                    "INSERT INTO v2_genesis (
                        singleton_id,
                        anchor_instance_id,
                        protocol_version,
                        canonical_genesis_bytes,
                        canonical_genesis_len,
                        genesis_digest,
                        stable_generation,
                        content_generation,
                        work_generation,
                        stable_root,
                        content_root,
                        candidate_root,
                        operation_id,
                        predecessor_head_digest,
                        predecessor_candidate_freeze,
                        plan_binding_digest
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                    params![
                        STORE_SINGLETON_ID,
                        record.anchor_instance_id.as_bytes().to_vec(),
                        record.protocol_version.as_str(),
                        record.canonical_genesis_bytes.clone(),
                        to_i64(record.canonical_genesis_len)?,
                        record.genesis_digest.as_bytes().to_vec(),
                        to_i64(record.stable_generation)?,
                        to_i64(record.content_generation)?,
                        to_i64(record.work_generation)?,
                        record.stable_root.as_bytes().to_vec(),
                        record.content_root.as_bytes().to_vec(),
                        record.candidate_root.map(|digest| digest.as_bytes().to_vec()),
                        record.operation_id.map(|operation_id| operation_id.as_bytes().to_vec()),
                        record
                            .predecessor_head_digest
                            .map(|digest| digest.as_bytes().to_vec()),
                        record.predecessor_candidate_freeze.map(to_i64).transpose()?,
                        record
                            .plan_binding_digest
                            .map(|digest| digest.as_bytes().to_vec()),
                    ],
                )?;
                Ok(())
            },
            move |path| {
                foundation_write_unknown_error(
                    path,
                    FoundationWriteRecordKind::Genesis,
                    FoundationWriteFinalize::Commit,
                    FoundationWriteExpectedRecord::Genesis(&commit_record),
                )
            },
            move |path| {
                foundation_write_unknown_error(
                    path,
                    FoundationWriteRecordKind::Genesis,
                    FoundationWriteFinalize::Rollback,
                    FoundationWriteExpectedRecord::Genesis(&rollback_record),
                )
            },
        )
    }

    pub fn load_foundation_genesis_record(&self) -> V2Result<Option<GenesisRecord>> {
        let conn = self.connection()?;
        let record = load_foundation_genesis_record_row(conn)?;
        let Some(record) = record else {
            return Ok(None);
        };

        validate_foundation_anchor_instance(conn, record.anchor_instance_id)?;
        Ok(Some(record))
    }

    pub fn save_foundation_intent_record(&mut self, record: &IntentRecord) -> V2Result<()> {
        record.validate()?;
        let commit_record = record.clone();
        let rollback_record = record.clone();
        self.run_write_operation(
            |conn| {
                validate_foundation_intent_authority(conn, record.anchor_identity.anchor_instance_id)?;
                conn.execute(
                    "INSERT INTO v2_intents (
                        successor_head_digest,
                        protocol_version,
                        canonical_successor_bytes,
                        canonical_successor_len,
                        successor_event_digest,
                        event_value_offset,
                        event_value_len,
                        prev_head_digest,
                        intent_generation,
                        frame_sequence,
                        predecessor_head_digest,
                        predecessor_candidate_freeze,
                        plan_binding_digest,
                        intent_state,
                        stable_root,
                        content_root
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                    params![
                        record.successor_head_digest.as_bytes().to_vec(),
                        record.protocol_version.as_str(),
                        record.canonical_successor_bytes.clone(),
                        to_i64(record.canonical_successor_len)?,
                        record.successor_event_digest.as_bytes().to_vec(),
                        to_i64(record.event_value_offset)?,
                        to_i64(record.event_value_len)?,
                        record.prev_head_digest.as_bytes().to_vec(),
                        to_i64(record.intent_generation)?,
                        to_i64(record.frame_sequence)?,
                        record.predecessor_head_digest.as_bytes().to_vec(),
                        to_i64(record.plan_binding_witness.predecessor_candidate_freeze)?,
                        record.plan_binding_digest.as_bytes().to_vec(),
                        record.state.as_sql(),
                        record.stable_root.as_bytes().to_vec(),
                        record.content_root.as_bytes().to_vec(),
                    ],
                )?;
                Ok(())
            },
            move |path| {
                foundation_write_unknown_error(
                    path,
                    FoundationWriteRecordKind::Intent,
                    FoundationWriteFinalize::Commit,
                    FoundationWriteExpectedRecord::Intent(&commit_record),
                )
            },
            move |path| {
                foundation_write_unknown_error(
                    path,
                    FoundationWriteRecordKind::Intent,
                    FoundationWriteFinalize::Rollback,
                    FoundationWriteExpectedRecord::Intent(&rollback_record),
                )
            },
        )
    }

    pub fn load_foundation_intent_record(
        &self,
        successor_head_digest: HeadDigest,
    ) -> V2Result<Option<IntentRecord>> {
        let conn = self.connection()?;
        let record = load_foundation_intent_record_row(conn, successor_head_digest)?;
        let Some(record) = record else {
            return Ok(None);
        };

        record.validate()?;
        validate_foundation_intent_authority(conn, record.anchor_identity.anchor_instance_id)?;
        Ok(Some(record))
    }

    pub fn persisted_intent_upper_rowid(&self) -> V2Result<u64> {
        let conn = self.connection()?;
        let upper = conn.query_row("SELECT MAX(rowid) FROM v2_intents", [], |row| {
            row.get::<_, Option<i64>>(0)
        })?;
        match upper {
            Some(rowid) => from_i64(rowid, "v2_intents.rowid"),
            None => Ok(0),
        }
    }

    pub fn load_persisted_intent_page(
        &self,
        after_rowid_exclusive: u64,
        upper_rowid_inclusive: u64,
        limit: u64,
    ) -> V2Result<Vec<PersistedIntentPageEntry>> {
        if limit == 0 || after_rowid_exclusive >= upper_rowid_inclusive {
            return Ok(Vec::new());
        }
        if limit > 256 {
            return Err(V2Error::InvalidPersistedIntentLimit { limit });
        }

        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT
                rowid,
                successor_head_digest,
                protocol_version,
                canonical_successor_bytes,
                canonical_successor_len,
                successor_event_digest,
                event_value_offset,
                event_value_len,
                prev_head_digest,
                intent_generation,
                frame_sequence,
                predecessor_head_digest,
                predecessor_candidate_freeze,
                plan_binding_digest,
                intent_state,
                stable_root,
                content_root
             FROM v2_intents
             WHERE rowid > ?1
               AND rowid <= ?2
             ORDER BY rowid
             LIMIT ?3",
        )?;
        let mut rows = statement.query(params![
            to_i64(after_rowid_exclusive)?,
            to_i64(upper_rowid_inclusive)?,
            to_i64(limit)?,
        ])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let rowid = from_i64(row.get::<_, i64>(0)?, "v2_intents.rowid")?;
            let record = foundation_intent_record_from_row_with_offset(row, 1).map_err(
                |error| match error {
                    rusqlite::Error::InvalidQuery => V2Error::AuthorityInvariantViolation {
                        context: "foundation_intent_row_invalid",
                    },
                    other => V2Error::Sqlite(other),
                },
            )?;
            record.validate()?;
            validate_foundation_intent_authority(conn, record.anchor_identity.anchor_instance_id)?;
            out.push(PersistedIntentPageEntry { rowid, record });
        }
        Ok(out)
    }

    pub fn append_content(&mut self, canonical_bytes: &[u8]) -> V2Result<AppendedContent> {
        if canonical_bytes.is_empty() {
            return Err(V2Error::EmptyContentPayload);
        }
        let payload_len =
            u64::try_from(canonical_bytes.len()).map_err(|_| V2Error::NumericOverflow)?;
        let _ = u32::try_from(canonical_bytes.len())
            .map_err(|_| V2Error::InvalidContentPayloadLength { len: payload_len })?;
        self.run_write_operation(
            |conn| {
                match validate_store_state(conn)? {
                    StoreAnchorView::RecoveryRequired { phase } => {
                        return Err(V2Error::RecoveryRequired {
                            phase: phase_str(phase),
                        });
                    }
                    StoreAnchorView::Genesis | StoreAnchorView::Stable(_) => {}
                }

                let next_content_seq = load_next_content_seq(conn)?;
                let max_content_seq = load_max_content_seq(conn)?;
                if let Some(max_content_seq) = max_content_seq {
                    if next_content_seq <= max_content_seq {
                        return Err(V2Error::ContentAllocatorRewind {
                            next_content_seq,
                            max_content_seq,
                        });
                    }
                }

                let event = ContentEvent {
                    content_seq: next_content_seq,
                    payload: canonical_bytes.to_vec(),
                };
                let digest = event.digest()?;

                let duplicate_digest = conn
                    .query_row(
                        "SELECT 1 FROM content_events WHERE content_event_digest = ?1",
                        params![digest.as_bytes().to_vec()],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                if duplicate_digest.is_some() {
                    return Err(V2Error::DuplicateContentEventDigest);
                }

                conn.execute(
                    "INSERT INTO content_events (content_seq, content_event_digest, content_payload)
                     VALUES (?1, ?2, ?3)",
                    params![
                        to_i64(event.content_seq)?,
                        digest.as_bytes().to_vec(),
                        event.payload.clone(),
                    ],
                )?;

                let updated_next = next_content_seq
                    .checked_add(1)
                    .ok_or(V2Error::NumericOverflow)?;
                conn.execute(
                    "UPDATE v2_store_state
                     SET next_content_seq = ?2
                     WHERE singleton_id = ?1",
                    params![STORE_SINGLETON_ID, to_i64(updated_next)?],
                )?;

                Ok(AppendedContent {
                    event,
                    digest,
                    allocator_watermark: updated_next - 1,
                })
            },
            |_| V2Error::AppendContentCommitUnknown,
            |_| V2Error::AppendContentRollbackUnknown,
        )
    }

    pub fn prepare(&mut self, identity: AnchorIdentity) -> V2Result<PreparedCheckpoint> {
        self.run_write_operation(
            |conn| {
                let anchor_view = validate_store_state(conn)?;
                let (stable_generation, stable_root, prev_head_digest) = match anchor_view {
                    StoreAnchorView::Genesis => (0, ContentRootDigest::new([0u8; 32]), None),
                    StoreAnchorView::Stable(anchor) => {
                        if anchor.head.core.identity != identity {
                            return Err(V2Error::AnchorIdentityMismatch);
                        }
                        (
                            anchor.head.core.stable_generation,
                            anchor.head.core.content_root,
                            Some(anchor.head_digest),
                        )
                    }
                    StoreAnchorView::RecoveryRequired { phase } => {
                        return Err(V2Error::RecoveryRequired {
                            phase: phase_str(phase),
                        });
                    }
                };

                let allocator_watermark = allocator_watermark(conn)?;
                let visible_frontier = load_max_content_seq(conn)?.unwrap_or(0);
                if visible_frontier <= stable_generation {
                    return Err(V2Error::NoEligibleContent {
                        stable_generation,
                        visible_frontier,
                    });
                }
                if visible_frontier > allocator_watermark {
                    return Err(V2Error::AuthorityInvariantViolation {
                        context: "visible_frontier_exceeds_allocator_watermark",
                    });
                }

                let operation_generation = next_operation_generation(conn)?;
                let operation_id = derive_operation_id(
                    identity,
                    operation_generation,
                    stable_generation,
                    visible_frontier,
                    allocator_watermark,
                    prev_head_digest,
                )?;

                let (content_events, membership_rows) =
                    load_eligible_content_snapshot(conn, operation_id, stable_generation, visible_frontier)?;
                validate_membership_window(&membership_rows, stable_generation, visible_frontier)?;

                let membership = CandidateMembership::new(operation_id, membership_rows)?;
                let candidate_membership_digest = membership.candidate_membership_digest()?;
                let candidate_root = membership.content_candidate_root(&content_events)?;
                let head_core = HeadCore {
                    identity,
                    lifecycle: HeadLifecycle::InFlight,
                    phase: CheckpointEventKind::Prepare,
                    stable_generation,
                    content_generation: visible_frontier,
                    operation_generation,
                    content_root: stable_root,
                    content_freeze: allocator_watermark,
                    candidate_root: Some(candidate_root),
                    candidate_freeze: Some(allocator_watermark),
                    candidate_membership_digest: Some(candidate_membership_digest),
                    operation_id: Some(operation_id),
                };
                let head = Head::new(head_core.clone(), prev_head_digest)?;
                let checkpoint_event = head.checkpoint_event()?;
                let checkpoint_event_digest = checkpoint_event.digest()?;
                let head_digest = head.digest()?;

                conn.execute(
                    "INSERT INTO checkpoint_operations (
                        operation_id,
                        anchor_instance_id,
                        path_binding,
                        key_epoch,
                        phase,
                        stable_generation,
                        content_generation,
                        operation_generation,
                        content_root,
                        content_freeze,
                        candidate_root,
                        candidate_freeze,
                        candidate_membership_digest,
                        head_digest,
                        prev_head_digest
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        operation_id.as_bytes().to_vec(),
                        identity.anchor_instance_id.as_bytes().to_vec(),
                        identity.path_binding.as_bytes().to_vec(),
                        to_i64(identity.key_epoch)?,
                        phase_str(CheckpointEventKind::Prepare),
                        to_i64(stable_generation)?,
                        to_i64(visible_frontier)?,
                        to_i64(operation_generation)?,
                        stable_root.as_bytes().to_vec(),
                        to_i64(allocator_watermark)?,
                        candidate_root.as_bytes().to_vec(),
                        to_i64(allocator_watermark)?,
                        candidate_membership_digest.as_bytes().to_vec(),
                        head_digest.as_bytes().to_vec(),
                        prev_head_digest.map(|digest| digest.as_bytes().to_vec()),
                    ],
                )?;

                for row in &membership.rows {
                    conn.execute(
                        "INSERT INTO operation_content_membership (
                            operation_id,
                            content_seq,
                            content_event_digest,
                            membership_row_digest,
                            candidate_membership_digest
                        ) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            row.operation_id.as_bytes().to_vec(),
                            to_i64(row.content_seq)?,
                            row.content_event_digest.as_bytes().to_vec(),
                            row.membership_row_digest()?.as_bytes().to_vec(),
                            candidate_membership_digest.as_bytes().to_vec(),
                        ],
                    )?;
                }

                conn.execute(
                    "INSERT INTO checkpoint_events (
                        checkpoint_event_digest,
                        event_kind,
                        anchor_instance_id,
                        path_binding,
                        key_epoch,
                        lifecycle,
                        phase,
                        stable_generation,
                        content_generation,
                        operation_generation,
                        content_root,
                        content_freeze,
                        candidate_root,
                        candidate_freeze,
                        candidate_membership_digest,
                        operation_id,
                        head_digest,
                        prev_head_digest
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                    params![
                        checkpoint_event_digest.as_bytes().to_vec(),
                        phase_str(CheckpointEventKind::Prepare),
                        identity.anchor_instance_id.as_bytes().to_vec(),
                        identity.path_binding.as_bytes().to_vec(),
                        to_i64(identity.key_epoch)?,
                        lifecycle_str(HeadLifecycle::InFlight),
                        phase_str(CheckpointEventKind::Prepare),
                        to_i64(stable_generation)?,
                        to_i64(visible_frontier)?,
                        to_i64(operation_generation)?,
                        stable_root.as_bytes().to_vec(),
                        to_i64(allocator_watermark)?,
                        candidate_root.as_bytes().to_vec(),
                        to_i64(allocator_watermark)?,
                        candidate_membership_digest.as_bytes().to_vec(),
                        operation_id.as_bytes().to_vec(),
                        head_digest.as_bytes().to_vec(),
                        prev_head_digest.map(|digest| digest.as_bytes().to_vec()),
                    ],
                )?;

                let updated_rows = conn.execute(
                    "UPDATE v2_anchor_state
                     SET anchor_instance_id = ?2,
                         path_binding = ?3,
                         key_epoch = ?4,
                         lifecycle = ?5,
                         phase = ?6,
                         stable_generation = ?7,
                         content_generation = ?8,
                         operation_generation = ?9,
                         content_root = ?10,
                         content_freeze = ?11,
                         candidate_root = ?12,
                         candidate_freeze = ?13,
                         candidate_membership_digest = ?14,
                         operation_id = ?15,
                         terminal_checkpoint_digest = ?16,
                         prev_head_digest = ?17,
                         head_digest = ?18
                     WHERE singleton_id = ?1",
                    params![
                        STORE_SINGLETON_ID,
                        identity.anchor_instance_id.as_bytes().to_vec(),
                        identity.path_binding.as_bytes().to_vec(),
                        to_i64(identity.key_epoch)?,
                        lifecycle_str(HeadLifecycle::InFlight),
                        phase_str(CheckpointEventKind::Prepare),
                        to_i64(stable_generation)?,
                        to_i64(visible_frontier)?,
                        to_i64(operation_generation)?,
                        stable_root.as_bytes().to_vec(),
                        to_i64(allocator_watermark)?,
                        candidate_root.as_bytes().to_vec(),
                        to_i64(allocator_watermark)?,
                        candidate_membership_digest.as_bytes().to_vec(),
                        operation_id.as_bytes().to_vec(),
                        checkpoint_event_digest.as_bytes().to_vec(),
                        prev_head_digest.map(|digest| digest.as_bytes().to_vec()),
                        head_digest.as_bytes().to_vec(),
                    ],
                )?;
                if updated_rows == 0 {
                    conn.execute(
                        "INSERT INTO v2_anchor_state (
                            singleton_id,
                            anchor_instance_id,
                            path_binding,
                            key_epoch,
                            lifecycle,
                            phase,
                            stable_generation,
                            content_generation,
                            operation_generation,
                            content_root,
                            content_freeze,
                            candidate_root,
                            candidate_freeze,
                            candidate_membership_digest,
                            operation_id,
                            terminal_checkpoint_digest,
                            prev_head_digest,
                            head_digest
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                        params![
                            STORE_SINGLETON_ID,
                            identity.anchor_instance_id.as_bytes().to_vec(),
                            identity.path_binding.as_bytes().to_vec(),
                            to_i64(identity.key_epoch)?,
                            lifecycle_str(HeadLifecycle::InFlight),
                            phase_str(CheckpointEventKind::Prepare),
                            to_i64(stable_generation)?,
                            to_i64(visible_frontier)?,
                            to_i64(operation_generation)?,
                            stable_root.as_bytes().to_vec(),
                            to_i64(allocator_watermark)?,
                            candidate_root.as_bytes().to_vec(),
                            to_i64(allocator_watermark)?,
                            candidate_membership_digest.as_bytes().to_vec(),
                            operation_id.as_bytes().to_vec(),
                            checkpoint_event_digest.as_bytes().to_vec(),
                            prev_head_digest.map(|digest| digest.as_bytes().to_vec()),
                            head_digest.as_bytes().to_vec(),
                        ],
                    )?;
                }

                Ok(PreparedCheckpoint {
                    operation_id,
                    operation_generation,
                    stable_generation,
                    visible_frontier,
                    allocator_watermark,
                    head,
                    head_digest,
                    checkpoint_event,
                    membership,
                    content_events,
                })
            },
            |_| V2Error::PrepareCommitUnknown,
            |_| V2Error::PrepareRollbackUnknown,
        )
    }

    #[cfg(test)]
    pub(crate) fn force_connection_loss_for_test(&mut self) {
        self.conn = None;
    }

    #[cfg(test)]
    pub(crate) fn inject_commit_unknown_for_test(&mut self) {
        self.test_write_fault = Some(TestWriteFault::CommitUnknown);
    }

    #[cfg(test)]
    pub(crate) fn inject_rollback_unknown_for_test(&mut self) {
        self.test_write_fault = Some(TestWriteFault::RollbackUnknown);
    }
}

enum FoundationWriteExpectedRecord<'a> {
    Genesis(&'a GenesisRecord),
    Intent(&'a IntentRecord),
}

#[cfg(test)]
fn take_test_write_fault(store: &mut V2Store) -> Option<TestWriteFault> {
    store.test_write_fault.take()
}

#[cfg(not(test))]
fn take_test_write_fault(_store: &mut V2Store) -> Option<TestWriteFault> {
    None
}

fn foundation_write_unknown_error(
    path: &Path,
    kind: FoundationWriteRecordKind,
    finalize: FoundationWriteFinalize,
    expected: FoundationWriteExpectedRecord<'_>,
) -> V2Error {
    let resolution = classify_foundation_write_receipt(path, expected);
    match finalize {
        FoundationWriteFinalize::Commit => {
            V2Error::FoundationWriteCommitUnknown { kind, resolution }
        }
        FoundationWriteFinalize::Rollback => {
            V2Error::FoundationWriteRollbackUnknown { kind, resolution }
        }
    }
}

fn classify_foundation_write_receipt(
    path: &Path,
    expected: FoundationWriteExpectedRecord<'_>,
) -> FoundationWriteResolution {
    let Ok(conn) = open_receipt_probe_connection(path) else {
        return FoundationWriteResolution::Corrupt;
    };

    match expected {
        FoundationWriteExpectedRecord::Genesis(record) => {
            classify_foundation_genesis_receipt_with_connection(&conn, record)
        }
        FoundationWriteExpectedRecord::Intent(record) => {
            classify_foundation_intent_receipt_with_connection(&conn, record)
        }
    }
}

fn open_receipt_probe_connection(path: &Path) -> V2Result<Connection> {
    let (_, conn) = open_verified_store_connection(path, StoreOpenMode::Existing)?;
    configure_store_connection(&conn)?;
    verify_store_catalog(&conn)?;
    Ok(conn)
}

fn classify_foundation_genesis_receipt_with_connection(
    conn: &Connection,
    expected: &GenesisRecord,
) -> FoundationWriteResolution {
    let anchor = match load_foundation_anchor_instance(conn) {
        Ok(anchor) => anchor,
        Err(_) => return FoundationWriteResolution::Corrupt,
    };
    let genesis = match load_foundation_genesis_record_row(conn) {
        Ok(record) => record,
        Err(_) => return FoundationWriteResolution::Corrupt,
    };

    match (anchor, genesis) {
        (None, None) => FoundationWriteResolution::Missing,
        (Some(anchor_instance_id), Some(record)) => {
            if anchor_instance_id != expected.anchor_instance_id
                || record.anchor_instance_id != anchor_instance_id
                || validate_foundation_anchor_instance(conn, record.anchor_instance_id).is_err()
                || record != *expected
            {
                FoundationWriteResolution::Corrupt
            } else {
                FoundationWriteResolution::Persisted
            }
        }
        _ => FoundationWriteResolution::Corrupt,
    }
}

fn classify_foundation_intent_receipt_with_connection(
    conn: &Connection,
    expected: &IntentRecord,
) -> FoundationWriteResolution {
    if validate_foundation_intent_authority(conn, expected.anchor_identity.anchor_instance_id)
        .is_err()
    {
        return FoundationWriteResolution::Corrupt;
    }

    let record = match load_foundation_intent_record_row(conn, expected.successor_head_digest) {
        Ok(record) => record,
        Err(_) => return FoundationWriteResolution::Corrupt,
    };
    let Some(record) = record else {
        return FoundationWriteResolution::Missing;
    };

    if record.validate().is_err()
        || validate_foundation_intent_authority(conn, record.anchor_identity.anchor_instance_id)
            .is_err()
        || record != *expected
    {
        FoundationWriteResolution::Corrupt
    } else {
        FoundationWriteResolution::Persisted
    }
}

#[cfg(test)]
pub(crate) fn classify_foundation_genesis_receipt_for_test(
    path: &Path,
    expected: &GenesisRecord,
) -> FoundationWriteResolution {
    classify_foundation_write_receipt(path, FoundationWriteExpectedRecord::Genesis(expected))
}

#[cfg(test)]
pub(crate) fn classify_foundation_intent_receipt_for_test(
    path: &Path,
    expected: &IntentRecord,
) -> FoundationWriteResolution {
    classify_foundation_write_receipt(path, FoundationWriteExpectedRecord::Intent(expected))
}

fn store_schema_objects() -> &'static [StoreSchemaObject] {
    &[
        StoreSchemaObject::table(
            "schema_meta",
            "CREATE TABLE schema_meta (singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1), schema_version TEXT NOT NULL, schema_params TEXT NOT NULL, manifest_hash BLOB NOT NULL CHECK (length(manifest_hash) = 32), params_hash BLOB NOT NULL CHECK (length(params_hash) = 32), catalog_hash BLOB NOT NULL CHECK (length(catalog_hash) = 32))",
        ),
        StoreSchemaObject::table(
            "v2_store_state",
            "CREATE TABLE v2_store_state (singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1), next_content_seq INTEGER NOT NULL CHECK (next_content_seq >= 1))",
        ),
        StoreSchemaObject::trigger(
            "schema_meta_no_update",
            "schema_meta",
            "CREATE TRIGGER schema_meta_no_update BEFORE UPDATE ON schema_meta BEGIN SELECT RAISE(ABORT, 'schema_meta_immutable'); END",
        ),
        StoreSchemaObject::trigger(
            "schema_meta_no_delete",
            "schema_meta",
            "CREATE TRIGGER schema_meta_no_delete BEFORE DELETE ON schema_meta BEGIN SELECT RAISE(ABORT, 'schema_meta_immutable'); END",
        ),
        StoreSchemaObject::trigger(
            "v2_store_state_no_delete",
            "v2_store_state",
            "CREATE TRIGGER v2_store_state_no_delete BEFORE DELETE ON v2_store_state BEGIN SELECT RAISE(ABORT, 'v2_store_state_no_delete'); END",
        ),
        StoreSchemaObject::trigger(
            "v2_store_state_monotonic",
            "v2_store_state",
            "CREATE TRIGGER v2_store_state_monotonic BEFORE UPDATE ON v2_store_state BEGIN SELECT CASE WHEN OLD.singleton_id != NEW.singleton_id THEN RAISE(ABORT, 'v2_store_state_identity_immutable') WHEN NEW.next_content_seq < OLD.next_content_seq THEN RAISE(ABORT, 'v2_store_state_rewind') END; END",
        ),
    ]
}

pub(crate) fn configure_store_connection(conn: &Connection) -> V2Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA wal_autocheckpoint = 0;",
    )?;
    enforce_foreign_keys(conn)
}

fn seed_schema_meta(conn: &Connection) -> V2Result<()> {
    conn.execute(
        "INSERT INTO schema_meta (
            singleton_id,
            schema_version,
            schema_params,
            manifest_hash,
            params_hash,
            catalog_hash
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            STORE_SINGLETON_ID,
            V2_SCHEMA_VERSION,
            V2_SCHEMA_PARAMS,
            public_manifest_hash()?.to_vec(),
            public_params_hash()?.to_vec(),
            public_catalog_hash()?.to_vec(),
        ],
    )?;
    Ok(())
}

fn seed_allocator_state(conn: &Connection) -> V2Result<()> {
    conn.execute(
        "INSERT INTO v2_store_state (singleton_id, next_content_seq)
         VALUES (?1, 1)",
        params![STORE_SINGLETON_ID],
    )?;
    Ok(())
}

fn verify_store_catalog(conn: &Connection) -> V2Result<()> {
    let expected_catalog_hash = expected_store_catalog_hash()?;
    let actual_catalog_hash = actual_catalog_hash(conn)?;
    if actual_catalog_hash != expected_catalog_hash {
        let version = read_schema_meta_text(conn, "schema_version")?;
        let params = read_schema_meta_text(conn, "schema_params")?;
        match (version.as_deref(), params.as_deref()) {
            (None, None) => return Err(V2Error::LegacyCatalogRejected),
            (version, params) => require_current_catalog(version, params)?,
        }
        return Err(V2Error::SchemaFenceMismatch);
    }

    let Some((schema_version, schema_params, manifest_hash, params_hash, catalog_hash)) = conn
        .query_row(
            "SELECT schema_version, schema_params, manifest_hash, params_hash, catalog_hash
             FROM schema_meta
             WHERE singleton_id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()?
    else {
        return Err(V2Error::MissingSchemaMeta);
    };

    require_current_catalog(Some(&schema_version), Some(&schema_params))?;
    if manifest_hash != public_manifest_hash()?.to_vec()
        || params_hash != public_params_hash()?.to_vec()
        || catalog_hash != public_catalog_hash()?.to_vec()
    {
        return Err(V2Error::SchemaFenceMismatch);
    }
    Ok(())
}

fn validate_store_state(conn: &Connection) -> V2Result<StoreAnchorView> {
    validate_content_allocator(conn)?;
    validate_checkpoint_history(conn)?;

    let anchor_state = load_anchor_state(conn)?;
    let max_operation_generation = load_max_operation_generation(conn)?;
    let max_event_generation = load_max_event_generation(conn)?;
    match anchor_state {
        None => {
            if max_operation_generation.is_some() || max_event_generation.is_some() {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "checkpoint_history_without_anchor_state",
                });
            }
            Ok(StoreAnchorView::Genesis)
        }
        Some(anchor_state) => {
            if Some(anchor_state.head.core.operation_generation) != max_operation_generation
                || Some(anchor_state.head.core.operation_generation) != max_event_generation
            {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "latest_generation_not_reflected_by_anchor_state",
                });
            }

            let event = load_checkpoint_event_by_digest(
                conn,
                anchor_state.head.terminal_checkpoint_digest,
            )?;
            if event.digest != anchor_state.head.terminal_checkpoint_digest
                || event.head != anchor_state.head
                || event.event != anchor_state.head.checkpoint_event()?
            {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "anchor_state_event_mismatch",
                });
            }

            let operation =
                load_operation_by_generation(conn, anchor_state.head.core.operation_generation)?;
            if operation.head != anchor_state.head {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "anchor_state_operation_mismatch",
                });
            }

            match anchor_state.head.core.lifecycle {
                HeadLifecycle::Stable => Ok(StoreAnchorView::Stable(anchor_state)),
                HeadLifecycle::InFlight => Ok(StoreAnchorView::RecoveryRequired {
                    phase: anchor_state.head.core.phase,
                }),
            }
        }
    }
}

fn ensure_foundation_anchor_instance(
    conn: &Connection,
    anchor_instance_id: AnchorInstanceId,
) -> V2Result<()> {
    match load_foundation_anchor_instance(conn)? {
        Some(existing) if existing == anchor_instance_id => Ok(()),
        Some(_) => Err(V2Error::AuthorityInvariantViolation {
            context: "foundation_anchor_instance_mismatch",
        }),
        None => {
            conn.execute(
                "INSERT INTO v2_anchor_instance (singleton_id, anchor_instance_id)
                 VALUES (?1, ?2)",
                params![STORE_SINGLETON_ID, anchor_instance_id.as_bytes().to_vec()],
            )?;
            Ok(())
        }
    }
}

fn validate_foundation_anchor_instance(
    conn: &Connection,
    anchor_instance_id: AnchorInstanceId,
) -> V2Result<()> {
    match load_foundation_anchor_instance(conn)? {
        Some(existing) if existing == anchor_instance_id => Ok(()),
        Some(_) => Err(V2Error::AuthorityInvariantViolation {
            context: "foundation_anchor_instance_mismatch",
        }),
        None => Err(V2Error::AuthorityInvariantViolation {
            context: "foundation_anchor_instance_missing",
        }),
    }
}

fn validate_foundation_intent_authority(
    conn: &Connection,
    anchor_instance_id: AnchorInstanceId,
) -> V2Result<()> {
    validate_foundation_anchor_instance(conn, anchor_instance_id)?;
    let genesis =
        load_foundation_genesis_record_row(conn)?.ok_or(V2Error::AuthorityInvariantViolation {
            context: "foundation_genesis_missing",
        })?;
    if genesis.anchor_instance_id != anchor_instance_id {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "foundation_genesis_anchor_instance_mismatch",
        });
    }
    Ok(())
}

fn load_foundation_anchor_instance(conn: &Connection) -> V2Result<Option<AnchorInstanceId>> {
    match conn
        .query_row(
            "SELECT anchor_instance_id
             FROM v2_anchor_instance
             WHERE singleton_id = 1",
            [],
            |row| {
                read_fixed32(row.get::<_, Vec<u8>>(0)?).map_err(|_| rusqlite::Error::InvalidQuery)
            },
        )
        .optional()
    {
        Ok(anchor) => Ok(anchor),
        Err(rusqlite::Error::InvalidQuery) => Err(V2Error::AuthorityInvariantViolation {
            context: "foundation_anchor_instance_row_invalid",
        }),
        Err(error) => Err(V2Error::Sqlite(error)),
    }
}

fn load_foundation_genesis_record_row(conn: &Connection) -> V2Result<Option<GenesisRecord>> {
    let record = match conn
        .query_row(
            "SELECT
                anchor_instance_id,
                protocol_version,
                canonical_genesis_bytes,
                canonical_genesis_len,
                genesis_digest,
                stable_generation,
                content_generation,
                work_generation,
                stable_root,
                content_root,
                candidate_root,
                operation_id,
                predecessor_head_digest,
                predecessor_candidate_freeze,
                plan_binding_digest
             FROM v2_genesis
             WHERE singleton_id = 1",
            [],
            foundation_genesis_record_from_row,
        )
        .optional()
    {
        Ok(record) => record,
        Err(rusqlite::Error::InvalidQuery) => {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "foundation_genesis_row_invalid",
            });
        }
        Err(error) => return Err(V2Error::Sqlite(error)),
    };
    let Some(record) = record else {
        return Ok(None);
    };

    record.validate()?;
    Ok(Some(record))
}

fn load_foundation_intent_record_row(
    conn: &Connection,
    successor_head_digest: HeadDigest,
) -> V2Result<Option<IntentRecord>> {
    let record = match conn
        .query_row(
            "SELECT
                successor_head_digest,
                protocol_version,
                canonical_successor_bytes,
                canonical_successor_len,
                successor_event_digest,
                event_value_offset,
                event_value_len,
                prev_head_digest,
                intent_generation,
                frame_sequence,
                predecessor_head_digest,
                predecessor_candidate_freeze,
                plan_binding_digest,
                intent_state,
                stable_root,
                content_root
             FROM v2_intents
             WHERE successor_head_digest = ?1",
            params![successor_head_digest.as_bytes().to_vec()],
            foundation_intent_record_from_row,
        )
        .optional()
    {
        Ok(record) => record,
        Err(rusqlite::Error::InvalidQuery) => {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "foundation_intent_row_invalid",
            });
        }
        Err(error) => return Err(V2Error::Sqlite(error)),
    };
    Ok(record)
}

fn foundation_genesis_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GenesisRecord> {
    Ok(GenesisRecord {
        anchor_instance_id: read_fixed32(row.get::<_, Vec<u8>>(0)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        protocol_version: row.get::<_, String>(1)?,
        canonical_genesis_bytes: row.get::<_, Vec<u8>>(2)?,
        canonical_genesis_len: from_i64(row.get::<_, i64>(3)?, "v2_genesis.canonical_genesis_len")
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        genesis_digest: read_fixed32(row.get::<_, Vec<u8>>(4)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        stable_generation: from_i64(row.get::<_, i64>(5)?, "v2_genesis.stable_generation")
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        content_generation: from_i64(row.get::<_, i64>(6)?, "v2_genesis.content_generation")
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        work_generation: from_i64(row.get::<_, i64>(7)?, "v2_genesis.work_generation")
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        stable_root: read_fixed32(row.get::<_, Vec<u8>>(8)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        content_root: read_fixed32(row.get::<_, Vec<u8>>(9)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        candidate_root: row
            .get::<_, Option<Vec<u8>>>(10)?
            .map(read_fixed32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        operation_id: row
            .get::<_, Option<Vec<u8>>>(11)?
            .map(read_fixed32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        predecessor_head_digest: row
            .get::<_, Option<Vec<u8>>>(12)?
            .map(read_fixed32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        predecessor_candidate_freeze: row
            .get::<_, Option<i64>>(13)?
            .map(|value| from_i64(value, "v2_genesis.predecessor_candidate_freeze"))
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        plan_binding_digest: row
            .get::<_, Option<Vec<u8>>>(14)?
            .map(read_fixed32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}

fn foundation_intent_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IntentRecord> {
    foundation_intent_record_from_row_with_offset(row, 0)
}

fn foundation_intent_record_from_row_with_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<IntentRecord> {
    let state = match row.get::<_, String>(offset + 13)?.as_str() {
        "authorized" => IntentState::Authorized,
        "applied" => IntentState::Applied,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let successor_head_digest: HeadDigest =
        read_fixed32(row.get::<_, Vec<u8>>(offset)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let protocol_version = row.get::<_, String>(offset + 1)?;
    let canonical_successor_bytes = row.get::<_, Vec<u8>>(offset + 2)?;
    let canonical_successor_len = from_i64(
        row.get::<_, i64>(offset + 3)?,
        "v2_intents.canonical_successor_len",
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let successor_event_digest: super::SuccessorEventDigest =
        read_fixed32(row.get::<_, Vec<u8>>(offset + 4)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let event_value_offset = from_i64(
        row.get::<_, i64>(offset + 5)?,
        "v2_intents.event_value_offset",
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let event_value_len = from_i64(row.get::<_, i64>(offset + 6)?, "v2_intents.event_value_len")
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let prev_head_digest: HeadDigest = read_fixed32(row.get::<_, Vec<u8>>(offset + 7)?)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let intent_generation = from_i64(
        row.get::<_, i64>(offset + 8)?,
        "v2_intents.intent_generation",
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let frame_sequence = from_i64(row.get::<_, i64>(offset + 9)?, "v2_intents.frame_sequence")
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let predecessor_head_digest: HeadDigest = read_fixed32(row.get::<_, Vec<u8>>(offset + 10)?)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let predecessor_candidate_freeze = row
        .get::<_, Option<i64>>(offset + 11)?
        .ok_or(rusqlite::Error::InvalidQuery)
        .and_then(|value| {
            from_i64(value, "v2_intents.predecessor_candidate_freeze")
                .map_err(|_| rusqlite::Error::InvalidQuery)
        })?;
    let plan_binding_digest: PlanBindingDigest = row
        .get::<_, Option<Vec<u8>>>(offset + 12)?
        .ok_or(rusqlite::Error::InvalidQuery)
        .and_then(|bytes| read_fixed32(bytes).map_err(|_| rusqlite::Error::InvalidQuery))?;
    let stable_root: ContentRootDigest = read_fixed32(row.get::<_, Vec<u8>>(offset + 14)?)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let content_root: ContentRootDigest = read_fixed32(row.get::<_, Vec<u8>>(offset + 15)?)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;

    let successor = super::SuccessorHead::from_canonical_bytes(&canonical_successor_bytes)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let expected = IntentRecord::new(&successor).map_err(|_| rusqlite::Error::InvalidQuery)?;

    if expected.plan_binding_witness.predecessor_candidate_freeze != predecessor_candidate_freeze {
        return Err(rusqlite::Error::InvalidQuery);
    }

    Ok(IntentRecord {
        protocol_version,
        canonical_successor_bytes,
        canonical_successor_len,
        successor_head_digest,
        successor_event_digest,
        event_value_offset,
        event_value_len,
        anchor_identity: expected.anchor_identity,
        prev_head_digest,
        intent_generation,
        frame_sequence,
        intent_id: expected.intent_id,
        predecessor_head_digest,
        plan_binding_witness: expected.plan_binding_witness,
        plan_binding_digest,
        state,
        stable_root,
        content_root,
    })
}

fn validate_content_allocator(conn: &Connection) -> V2Result<()> {
    let next_content_seq = load_next_content_seq(conn)?;
    let mut statement = conn.prepare(
        "SELECT content_seq, content_event_digest, content_payload
         FROM content_events
         ORDER BY content_seq",
    )?;
    let mut rows = statement.query([])?;
    let mut last_seq = None::<u64>;
    while let Some(row) = rows.next()? {
        let content_seq = from_i64(row.get::<_, i64>(0)?, "content_events.content_seq")?;
        let stored_digest = ContentEventDigest::from_slice(&row.get::<_, Vec<u8>>(1)?)?;
        if let Some(previous) = last_seq {
            if content_seq <= previous {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "content_seq_not_strictly_increasing",
                });
            }
        }
        let event = ContentEvent {
            content_seq,
            payload: row.get::<_, Vec<u8>>(2)?,
        };
        if event.digest()? != stored_digest {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "content_event_digest_mismatch",
            });
        }
        last_seq = Some(content_seq);
    }
    if let Some(max_content_seq) = last_seq {
        if next_content_seq <= max_content_seq {
            return Err(V2Error::ContentAllocatorRewind {
                next_content_seq,
                max_content_seq,
            });
        }
    }
    Ok(())
}

fn validate_checkpoint_history(conn: &Connection) -> V2Result<()> {
    validate_checkpoint_event_history(conn)?;
    let allocator_watermark = allocator_watermark(conn)?;
    let mut previous = None::<super::CheckpointOperation>;
    for operation in load_checkpoint_operations(conn)? {
        validate_checkpoint_operation(conn, &operation, allocator_watermark)?;
        let checkpoint_operation = super::CheckpointOperation {
            operation_id: operation.row_operation_id,
            operation_generation: operation.head.core.operation_generation,
            phase: operation.head.core.phase,
        };
        if let Some(previous) = previous {
            previous.validate_next_operation(&checkpoint_operation)?;
        }
        previous = Some(checkpoint_operation);
    }
    Ok(())
}

fn validate_checkpoint_event_history(conn: &Connection) -> V2Result<()> {
    let mut previous = None::<Head>;
    for event in load_checkpoint_event_history(conn)? {
        validate_head_history_successor(previous.as_ref(), &event.head)?;
        previous = Some(event.head);
    }
    Ok(())
}

fn validate_checkpoint_operation(
    conn: &Connection,
    operation: &StoredOperation,
    allocator_watermark: u64,
) -> V2Result<()> {
    validate_operation_head_event(conn, operation)?;
    match operation.head.core.phase {
        CheckpointEventKind::Prepare | CheckpointEventKind::Pending => {
            validate_inflight_operation(conn, operation, allocator_watermark)
        }
        CheckpointEventKind::Finalize | CheckpointEventKind::Abort => {
            validate_terminal_operation(conn, operation)
        }
    }
}

fn validate_operation_head_event(conn: &Connection, operation: &StoredOperation) -> V2Result<()> {
    let event = load_checkpoint_event_by_head_digest(conn, operation.head.digest()?)?;
    if event.digest != operation.head.terminal_checkpoint_digest
        || event.head != operation.head
        || event.event != operation.head.checkpoint_event()?
    {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "operation_checkpoint_event_mismatch",
        });
    }
    Ok(())
}

fn validate_inflight_operation(
    conn: &Connection,
    operation: &StoredOperation,
    allocator_watermark: u64,
) -> V2Result<()> {
    let core = &operation.head.core;
    if core.content_generation > allocator_watermark {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_operation_exceeds_allocator_watermark",
        });
    }
    if core.content_freeze > allocator_watermark {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_operation_exceeds_allocator_watermark",
        });
    }
    if core.content_freeze != allocator_watermark {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "inflight_content_freeze_not_current_allocator",
        });
    }
    let candidate_freeze = core
        .candidate_freeze
        .ok_or(V2Error::AuthorityInvariantViolation {
            context: "prepare_operation_missing_candidate_freeze",
        })?;
    if candidate_freeze > allocator_watermark {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_candidate_freeze_exceeds_allocator_watermark",
        });
    }
    if candidate_freeze != allocator_watermark {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "inflight_candidate_freeze_not_current_allocator",
        });
    }
    if core.content_generation > core.content_freeze || core.content_generation > candidate_freeze {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_generation_exceeds_freeze_witness",
        });
    }
    if candidate_freeze != core.content_freeze {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_freeze_witness_mismatch",
        });
    }
    let expected_operation_id = derive_operation_id(
        core.identity,
        core.operation_generation,
        core.stable_generation,
        core.content_generation,
        core.content_freeze,
        operation.head.prev_head_digest,
    )?;
    if expected_operation_id != operation.row_operation_id {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_operation_id_mismatch",
        });
    }
    validate_prepare_prev_head_witness(conn, operation)?;

    let candidate_membership_digest =
        core.candidate_membership_digest
            .ok_or(V2Error::AuthorityInvariantViolation {
                context: "prepare_operation_missing_membership_digest",
            })?;
    let candidate_root = core
        .candidate_root
        .ok_or(V2Error::AuthorityInvariantViolation {
            context: "prepare_operation_missing_candidate_root",
        })?;

    let stored_rows = load_membership_rows(conn, operation.row_operation_id)?;
    if stored_rows.is_empty() {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_operation_missing_membership_rows",
        });
    }
    let mut membership_rows = Vec::with_capacity(stored_rows.len());
    let mut content_events = Vec::with_capacity(stored_rows.len());
    for stored_row in stored_rows {
        if stored_row.row.membership_row_digest()? != stored_row.membership_row_digest {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "membership_row_digest_mismatch",
            });
        }
        if stored_row.candidate_membership_digest != candidate_membership_digest {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "membership_candidate_digest_mismatch",
            });
        }
        if stored_row.row.content_seq <= core.stable_generation
            || stored_row.row.content_seq > core.content_generation
        {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "membership_row_outside_prepare_window",
            });
        }
        let content_event = load_content_event(conn, stored_row.row.content_seq)?;
        if content_event.digest()? != stored_row.row.content_event_digest {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "membership_content_digest_mismatch",
            });
        }
        membership_rows.push(stored_row.row);
        content_events.push(content_event);
    }
    validate_membership_window(
        &membership_rows,
        core.stable_generation,
        core.content_generation,
    )?;

    let membership = CandidateMembership::new(operation.row_operation_id, membership_rows)?;
    if membership.candidate_membership_digest()? != candidate_membership_digest {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_operation_membership_digest_mismatch",
        });
    }
    if membership.content_candidate_root(&content_events)? != candidate_root {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_operation_candidate_root_mismatch",
        });
    }
    Ok(())
}

fn validate_prepare_prev_head_witness(
    conn: &Connection,
    operation: &StoredOperation,
) -> V2Result<()> {
    let core = &operation.head.core;
    match operation.head.prev_head_digest {
        Some(prev_head_digest) => {
            let prev_head = load_head_by_digest(conn, prev_head_digest)?;
            if prev_head.core.lifecycle != HeadLifecycle::Stable
                || prev_head.core.identity != core.identity
                || prev_head.core.stable_generation != core.stable_generation
                || prev_head.core.content_root != core.content_root
                || prev_head.core.operation_generation >= core.operation_generation
            {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "prepare_prev_head_witness_mismatch",
                });
            }
        }
        None => {
            if core.stable_generation != 0 || core.content_root != zero_root_digest() {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "prepare_genesis_witness_mismatch",
                });
            }
        }
    }
    Ok(())
}

fn validate_terminal_operation(conn: &Connection, operation: &StoredOperation) -> V2Result<()> {
    let Some(prev_head_digest) = operation.head.prev_head_digest else {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "terminal_operation_missing_prev_head",
        });
    };
    let prev_head = load_head_by_digest(conn, prev_head_digest)?;
    let Some(prev_operation_id) = prev_head.core.operation_id else {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "terminal_operation_prev_head_mismatch",
        });
    };
    if operation.row_operation_id != prev_operation_id {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "terminal_operation_row_operation_id_mismatch",
        });
    }
    validate_terminal_head_successor(&prev_head, &operation.head)
}

fn validate_membership_window(
    rows: &[OperationContentMembership],
    stable_generation: u64,
    content_generation: u64,
) -> V2Result<()> {
    let mut expected = stable_generation
        .checked_add(1)
        .ok_or(V2Error::NumericOverflow)?;
    for row in rows {
        if row.content_seq < expected {
            return Err(V2Error::DuplicateContentSeq {
                content_seq: row.content_seq,
            });
        }
        if row.content_seq > content_generation {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "membership_row_outside_prepare_window",
            });
        }
        if row.content_seq != expected {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "membership_window_gap",
            });
        }
        expected = expected.checked_add(1).ok_or(V2Error::NumericOverflow)?;
    }
    if expected != content_generation.saturating_add(1) {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "membership_window_missing_tail",
        });
    }
    Ok(())
}

fn validate_head_history_successor(previous: Option<&Head>, current: &Head) -> V2Result<()> {
    match previous {
        None => {
            if current.prev_head_digest.is_some() {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "checkpoint_history_first_head_has_prev",
                });
            }
            if current.core.phase != CheckpointEventKind::Prepare {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "checkpoint_history_first_head_must_be_prepare",
                });
            }
            validate_prepare_prev_head_witness_head(current)
        }
        Some(previous) => {
            let expected_prev_digest = previous.digest()?;
            if current.prev_head_digest != Some(expected_prev_digest) {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "checkpoint_history_prev_head_gap",
                });
            }
            match previous.core.lifecycle {
                HeadLifecycle::Stable => validate_prepare_head_successor(previous, current),
                HeadLifecycle::InFlight => match current.core.phase {
                    CheckpointEventKind::Pending => {
                        validate_pending_head_successor(previous, current)
                    }
                    CheckpointEventKind::Finalize | CheckpointEventKind::Abort => {
                        validate_terminal_head_successor(previous, current)
                    }
                    CheckpointEventKind::Prepare => Err(V2Error::AuthorityInvariantViolation {
                        context: "inflight_head_not_followed_by_pending_or_terminal",
                    }),
                },
            }
        }
    }
}

fn validate_prepare_prev_head_witness_head(head: &Head) -> V2Result<()> {
    if head.core.stable_generation != 0 || head.core.content_root != zero_root_digest() {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_genesis_witness_mismatch",
        });
    }
    Ok(())
}

fn validate_prepare_head_successor(previous: &Head, current: &Head) -> V2Result<()> {
    if current.core.phase != CheckpointEventKind::Prepare
        || current.core.lifecycle != HeadLifecycle::InFlight
        || previous.core.identity != current.core.identity
        || current.core.stable_generation != previous.core.stable_generation
        || current.core.content_root != previous.core.content_root
        || current.core.operation_generation <= previous.core.operation_generation
    {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "prepare_prev_head_witness_mismatch",
        });
    }
    Ok(())
}

fn validate_pending_head_successor(previous: &Head, current: &Head) -> V2Result<()> {
    let Some(previous_operation_id) = previous.core.operation_id else {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "pending_operation_prev_head_mismatch",
        });
    };
    if previous.core.lifecycle != HeadLifecycle::InFlight
        || current.core.lifecycle != HeadLifecycle::InFlight
        || current.core.phase != CheckpointEventKind::Pending
        || previous.core.identity != current.core.identity
        || current.core.stable_generation != previous.core.stable_generation
        || current.core.content_root != previous.core.content_root
        || current.core.operation_generation != previous.core.operation_generation
        || current.core.operation_id != Some(previous_operation_id)
    {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "pending_operation_prev_head_mismatch",
        });
    }
    Ok(())
}

fn validate_terminal_head_successor(previous: &Head, current: &Head) -> V2Result<()> {
    if previous.core.lifecycle != HeadLifecycle::InFlight
        || current.core.lifecycle != HeadLifecycle::Stable
        || previous.core.identity != current.core.identity
        || current.core.operation_generation != previous.core.operation_generation
    {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "terminal_operation_prev_head_mismatch",
        });
    }
    match current.core.phase {
        CheckpointEventKind::Finalize => {
            let Some(candidate_root) = previous.core.candidate_root else {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "finalize_head_projection_mismatch",
                });
            };
            let Some(candidate_freeze) = previous.core.candidate_freeze else {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "finalize_head_projection_mismatch",
                });
            };
            if current.core.stable_generation != previous.core.content_generation
                || current.core.content_generation != previous.core.content_generation
                || current.core.content_root != candidate_root
                || current.core.content_freeze != candidate_freeze
            {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "finalize_head_projection_mismatch",
                });
            }
        }
        CheckpointEventKind::Abort => {
            if current.core.stable_generation != previous.core.stable_generation
                || current.core.content_generation != previous.core.stable_generation
                || current.core.content_root != previous.core.content_root
                || current.core.content_freeze != previous.core.content_freeze
            {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "abort_head_projection_mismatch",
                });
            }
        }
        CheckpointEventKind::Prepare | CheckpointEventKind::Pending => {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "terminal_operation_prev_head_mismatch",
            });
        }
    }
    Ok(())
}

fn load_checkpoint_operations(conn: &Connection) -> V2Result<Vec<StoredOperation>> {
    let mut statement = conn.prepare(
        "SELECT
            operation_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            head_digest,
            prev_head_digest
         FROM checkpoint_operations
         ORDER BY operation_generation",
    )?;
    let mut rows = statement.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(match stored_operation_from_row(row) {
            Ok(operation) => operation,
            Err(rusqlite::Error::InvalidQuery) => {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "checkpoint_operation_row_invalid",
                });
            }
            Err(error) => return Err(V2Error::Sqlite(error)),
        });
    }
    Ok(out)
}

fn load_checkpoint_event_history(conn: &Connection) -> V2Result<Vec<StoredCheckpointEvent>> {
    let mut statement = conn.prepare(
        "SELECT
            checkpoint_event_digest,
            event_kind,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            head_digest,
            prev_head_digest
         FROM checkpoint_events
         ORDER BY rowid",
    )?;
    let mut rows = statement.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(match stored_checkpoint_event_from_row(row) {
            Ok(event) => event,
            Err(rusqlite::Error::InvalidQuery) => {
                return Err(V2Error::AuthorityInvariantViolation {
                    context: "checkpoint_event_row_invalid",
                });
            }
            Err(error) => return Err(V2Error::Sqlite(error)),
        });
    }
    Ok(out)
}

fn load_anchor_state(conn: &Connection) -> V2Result<Option<StoredAnchorState>> {
    match conn
        .query_row(
            "SELECT
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            terminal_checkpoint_digest,
            prev_head_digest,
            head_digest
         FROM v2_anchor_state
         WHERE singleton_id = 1",
            [],
            |row| {
                let head = Head {
                    core: head_core_from_columns(
                        AnchorIdentity {
                            anchor_instance_id: read_fixed32(row.get::<_, Vec<u8>>(0)?)
                                .map_err(|_| rusqlite::Error::InvalidQuery)?,
                            path_binding: read_fixed32(row.get::<_, Vec<u8>>(1)?)
                                .map_err(|_| rusqlite::Error::InvalidQuery)?,
                            key_epoch: from_i64(row.get::<_, i64>(2)?, "v2_anchor_state.key_epoch")
                                .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        },
                        parse_lifecycle(&row.get::<_, String>(3)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        parse_phase(&row.get::<_, String>(4)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        from_i64(row.get::<_, i64>(5)?, "v2_anchor_state.stable_generation")
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        from_i64(row.get::<_, i64>(6)?, "v2_anchor_state.content_generation")
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        from_i64(
                            row.get::<_, i64>(7)?,
                            "v2_anchor_state.operation_generation",
                        )
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        read_fixed32(row.get::<_, Vec<u8>>(8)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        from_i64(row.get::<_, i64>(9)?, "v2_anchor_state.content_freeze")
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        row.get::<_, Option<Vec<u8>>>(10)?
                            .map(read_fixed32)
                            .transpose()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        row.get::<_, Option<i64>>(11)?
                            .map(|value| from_i64(value, "v2_anchor_state.candidate_freeze"))
                            .transpose()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        row.get::<_, Option<Vec<u8>>>(12)?
                            .map(read_fixed32)
                            .transpose()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        row.get::<_, Option<Vec<u8>>>(13)?
                            .map(read_fixed32)
                            .transpose()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    )
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    terminal_checkpoint_digest: read_fixed32(row.get::<_, Vec<u8>>(14)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    prev_head_digest: row
                        .get::<_, Option<Vec<u8>>>(15)?
                        .map(read_fixed32)
                        .transpose()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                };
                head.validate().map_err(|_| rusqlite::Error::InvalidQuery)?;
                let head_digest = head.digest().map_err(|_| rusqlite::Error::InvalidQuery)?;
                let stored_head_digest: HeadDigest = read_fixed32(row.get::<_, Vec<u8>>(16)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                if head_digest != stored_head_digest {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                Ok(StoredAnchorState { head, head_digest })
            },
        )
        .optional()
    {
        Ok(value) => Ok(value),
        Err(rusqlite::Error::InvalidQuery) => Err(V2Error::AuthorityInvariantViolation {
            context: "anchor_state_row_invalid",
        }),
        Err(error) => Err(V2Error::Sqlite(error)),
    }
}

fn load_checkpoint_event_by_digest(
    conn: &Connection,
    digest: CheckpointEventDigest,
) -> V2Result<StoredCheckpointEvent> {
    let event = match conn
        .query_row(
            "SELECT
            checkpoint_event_digest,
            event_kind,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            head_digest,
            prev_head_digest
         FROM checkpoint_events
         WHERE checkpoint_event_digest = ?1",
            params![digest.as_bytes().to_vec()],
            stored_checkpoint_event_from_row,
        )
        .optional()
    {
        Ok(event) => event,
        Err(rusqlite::Error::InvalidQuery) => {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "checkpoint_event_row_invalid",
            });
        }
        Err(error) => return Err(V2Error::Sqlite(error)),
    };
    event.ok_or(V2Error::AuthorityInvariantViolation {
        context: "missing_checkpoint_event_by_digest",
    })
}

fn load_checkpoint_event_by_head_digest(
    conn: &Connection,
    head_digest: HeadDigest,
) -> V2Result<StoredCheckpointEvent> {
    let event = match conn
        .query_row(
            "SELECT
            checkpoint_event_digest,
            event_kind,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            head_digest,
            prev_head_digest
         FROM checkpoint_events
         WHERE head_digest = ?1",
            params![head_digest.as_bytes().to_vec()],
            stored_checkpoint_event_from_row,
        )
        .optional()
    {
        Ok(event) => event,
        Err(rusqlite::Error::InvalidQuery) => {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "checkpoint_event_row_invalid",
            });
        }
        Err(error) => return Err(V2Error::Sqlite(error)),
    };
    event.ok_or(V2Error::AuthorityInvariantViolation {
        context: "missing_checkpoint_event_by_head_digest",
    })
}

fn load_operation_by_generation(
    conn: &Connection,
    operation_generation: u64,
) -> V2Result<StoredOperation> {
    let operation = match conn
        .query_row(
            "SELECT
            operation_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            head_digest,
            prev_head_digest
         FROM checkpoint_operations
         WHERE operation_generation = ?1",
            params![to_i64(operation_generation)?],
            stored_operation_from_row,
        )
        .optional()
    {
        Ok(operation) => operation,
        Err(rusqlite::Error::InvalidQuery) => {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "checkpoint_operation_row_invalid",
            });
        }
        Err(error) => return Err(V2Error::Sqlite(error)),
    };
    operation.ok_or(V2Error::AuthorityInvariantViolation {
        context: "missing_operation_by_generation",
    })
}

fn load_head_by_digest(conn: &Connection, head_digest: HeadDigest) -> V2Result<Head> {
    Ok(load_checkpoint_event_by_head_digest(conn, head_digest)?.head)
}

fn load_membership_rows(
    conn: &Connection,
    operation_id: OperationId,
) -> V2Result<Vec<StoredMembershipRow>> {
    let mut statement = conn.prepare(
        "SELECT
            operation_id,
            content_seq,
            content_event_digest,
            membership_row_digest,
            candidate_membership_digest
         FROM operation_content_membership
         WHERE operation_id = ?1
         ORDER BY content_seq",
    )?;
    let mut rows = statement.query(params![operation_id.as_bytes().to_vec()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(StoredMembershipRow {
            row: OperationContentMembership {
                operation_id: read_fixed32(row.get::<_, Vec<u8>>(0)?)?,
                content_seq: from_i64(
                    row.get::<_, i64>(1)?,
                    "operation_content_membership.content_seq",
                )?,
                content_event_digest: read_fixed32(row.get::<_, Vec<u8>>(2)?)?,
            },
            membership_row_digest: read_fixed32(row.get::<_, Vec<u8>>(3)?)?,
            candidate_membership_digest: read_fixed32(row.get::<_, Vec<u8>>(4)?)?,
        });
    }
    Ok(out)
}

fn load_content_event(conn: &Connection, content_seq: u64) -> V2Result<ContentEvent> {
    let Some((stored_digest, payload)) = conn
        .query_row(
            "SELECT content_event_digest, content_payload
             FROM content_events
             WHERE content_seq = ?1",
            params![to_i64(content_seq)?],
            |row| {
                Ok((
                    read_fixed32(row.get::<_, Vec<u8>>(0)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    row.get::<_, Vec<u8>>(1)?,
                ))
            },
        )
        .optional()?
    else {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "missing_content_event_projection",
        });
    };
    let event = ContentEvent {
        content_seq,
        payload,
    };
    if event.digest()? != stored_digest {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "content_event_projection_digest_mismatch",
        });
    }
    Ok(event)
}

fn load_eligible_content_snapshot(
    conn: &Connection,
    operation_id: OperationId,
    stable_generation: u64,
    visible_frontier: u64,
) -> V2Result<(Vec<ContentEvent>, Vec<OperationContentMembership>)> {
    let mut statement = conn.prepare(
        "SELECT ce.content_seq, ce.content_event_digest, ce.content_payload
         FROM content_events ce
         WHERE ce.content_seq > ?1
           AND ce.content_seq <= ?2
           AND NOT EXISTS (
               SELECT 1
               FROM operation_content_membership m
               WHERE m.content_seq = ce.content_seq
           )
         ORDER BY ce.content_seq",
    )?;
    let mut rows = statement.query(params![
        to_i64(stable_generation)?,
        to_i64(visible_frontier)?
    ])?;
    let mut content_events = Vec::new();
    let mut membership_rows = Vec::new();
    while let Some(row) = rows.next()? {
        let content_seq = from_i64(row.get::<_, i64>(0)?, "content_events.content_seq")?;
        let stored_digest: ContentEventDigest = read_fixed32(row.get::<_, Vec<u8>>(1)?)?;
        let event = ContentEvent {
            content_seq,
            payload: row.get::<_, Vec<u8>>(2)?,
        };
        let digest = event.digest()?;
        if digest != stored_digest {
            return Err(V2Error::AuthorityInvariantViolation {
                context: "eligible_content_digest_mismatch",
            });
        }
        content_events.push(event.clone());
        membership_rows.push(OperationContentMembership {
            operation_id,
            content_seq,
            content_event_digest: digest,
        });
    }
    Ok((content_events, membership_rows))
}

fn load_next_content_seq(conn: &Connection) -> V2Result<u64> {
    let value = conn
        .query_row(
            "SELECT next_content_seq
         FROM v2_store_state
         WHERE singleton_id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(value) = value else {
        return Err(V2Error::AuthorityInvariantViolation {
            context: "missing_v2_store_state",
        });
    };
    from_i64(value, "v2_store_state.next_content_seq")
}

fn load_max_content_seq(conn: &Connection) -> V2Result<Option<u64>> {
    let value = conn.query_row("SELECT MAX(content_seq) FROM content_events", [], |row| {
        row.get::<_, Option<i64>>(0)
    })?;
    value
        .map(|value| from_i64(value, "content_events.max_content_seq"))
        .transpose()
}

fn allocator_watermark(conn: &Connection) -> V2Result<u64> {
    load_next_content_seq(conn)?
        .checked_sub(1)
        .ok_or(V2Error::AuthorityInvariantViolation {
            context: "allocator_next_content_seq_underflow",
        })
}

fn load_max_operation_generation(conn: &Connection) -> V2Result<Option<u64>> {
    let value = conn.query_row(
        "SELECT MAX(operation_generation) FROM checkpoint_operations",
        [],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    value
        .map(|value| from_i64(value, "checkpoint_operations.max_operation_generation"))
        .transpose()
}

fn load_max_event_generation(conn: &Connection) -> V2Result<Option<u64>> {
    let value = conn.query_row(
        "SELECT MAX(operation_generation) FROM checkpoint_events",
        [],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    value
        .map(|value| from_i64(value, "checkpoint_events.max_operation_generation"))
        .transpose()
}

fn next_operation_generation(conn: &Connection) -> V2Result<u64> {
    let current = load_max_operation_generation(conn)?.unwrap_or(0);
    current.checked_add(1).ok_or(V2Error::NumericOverflow)
}

pub(crate) fn derive_operation_id(
    identity: AnchorIdentity,
    operation_generation: u64,
    stable_generation: u64,
    visible_frontier: u64,
    allocator_watermark: u64,
    prev_head_digest: Option<HeadDigest>,
) -> V2Result<OperationId> {
    let mut witness = Vec::with_capacity(1 + 32);
    match prev_head_digest {
        Some(digest) => {
            witness.push(1);
            witness.extend_from_slice(digest.as_bytes());
        }
        None => witness.push(0),
    }
    Ok(OperationId::new(sha256_domain(
        "goose.evidence-db.v2.operation-id",
        &[
            &identity.to_canonical_prefix_bytes(),
            &operation_generation.to_be_bytes(),
            &stable_generation.to_be_bytes(),
            &visible_frontier.to_be_bytes(),
            &allocator_watermark.to_be_bytes(),
            &witness,
        ],
    )?))
}

fn expected_store_catalog_hash() -> V2Result<[u8; 32]> {
    let conn = Connection::open_in_memory()?;
    apply_catalog(&conn)?;
    install_v2_store_catalog(&conn)?;
    actual_catalog_hash(&conn)
}

fn read_schema_meta_text(conn: &Connection, column: &str) -> V2Result<Option<String>> {
    let has_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_meta'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if has_table.is_none() {
        return Ok(None);
    }
    let has_column = conn
        .query_row(
            "SELECT 1
             FROM pragma_table_info('schema_meta')
             WHERE name = ?1",
            params![column],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if has_column.is_none() {
        return Ok(None);
    }
    let sql = format!("SELECT {column} FROM schema_meta WHERE singleton_id = 1");
    conn.query_row(&sql, [], |row| row.get::<_, String>(0))
        .optional()
        .map_err(V2Error::from)
}

fn stored_checkpoint_event_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredCheckpointEvent> {
    let digest: CheckpointEventDigest =
        read_fixed32(row.get::<_, Vec<u8>>(0)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let head_core = head_core_from_columns(
        AnchorIdentity {
            anchor_instance_id: read_fixed32(row.get::<_, Vec<u8>>(2)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            path_binding: read_fixed32(row.get::<_, Vec<u8>>(3)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            key_epoch: from_i64(row.get::<_, i64>(4)?, "checkpoint_events.key_epoch")
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
        },
        parse_lifecycle(&row.get::<_, String>(5)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        parse_phase(&row.get::<_, String>(6)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        from_i64(row.get::<_, i64>(7)?, "checkpoint_events.stable_generation")
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        from_i64(
            row.get::<_, i64>(8)?,
            "checkpoint_events.content_generation",
        )
        .map_err(|_| rusqlite::Error::InvalidQuery)?,
        from_i64(
            row.get::<_, i64>(9)?,
            "checkpoint_events.operation_generation",
        )
        .map_err(|_| rusqlite::Error::InvalidQuery)?,
        read_fixed32(row.get::<_, Vec<u8>>(10)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        from_i64(row.get::<_, i64>(11)?, "checkpoint_events.content_freeze")
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, Option<Vec<u8>>>(12)?
            .map(read_fixed32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, Option<i64>>(13)?
            .map(|value| from_i64(value, "checkpoint_events.candidate_freeze"))
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, Option<Vec<u8>>>(14)?
            .map(read_fixed32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, Option<Vec<u8>>>(15)?
            .map(read_fixed32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let prev_head_digest = row
        .get::<_, Option<Vec<u8>>>(17)?
        .map(read_fixed32)
        .transpose()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let event = CheckpointEvent::new(
        parse_phase(&row.get::<_, String>(1)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        prev_head_digest,
        head_core,
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let head = Head::new(event.head_core.clone(), prev_head_digest)
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    if event.digest().map_err(|_| rusqlite::Error::InvalidQuery)? != digest
        || head.terminal_checkpoint_digest != digest
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let stored_head_digest: HeadDigest =
        read_fixed32(row.get::<_, Vec<u8>>(16)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if head.digest().map_err(|_| rusqlite::Error::InvalidQuery)? != stored_head_digest {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(StoredCheckpointEvent {
        digest,
        head,
        event,
    })
}

fn stored_operation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredOperation> {
    let phase =
        parse_phase(&row.get::<_, String>(4)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let lifecycle = operation_lifecycle(phase);
    let row_operation_id: OperationId =
        read_fixed32(row.get::<_, Vec<u8>>(0)?).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let core = head_core_from_columns(
        AnchorIdentity {
            anchor_instance_id: read_fixed32(row.get::<_, Vec<u8>>(1)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            path_binding: read_fixed32(row.get::<_, Vec<u8>>(2)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            key_epoch: from_i64(row.get::<_, i64>(3)?, "checkpoint_operations.key_epoch")
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
        },
        lifecycle,
        phase,
        from_i64(
            row.get::<_, i64>(5)?,
            "checkpoint_operations.stable_generation",
        )
        .map_err(|_| rusqlite::Error::InvalidQuery)?,
        from_i64(
            row.get::<_, i64>(6)?,
            "checkpoint_operations.content_generation",
        )
        .map_err(|_| rusqlite::Error::InvalidQuery)?,
        from_i64(
            row.get::<_, i64>(7)?,
            "checkpoint_operations.operation_generation",
        )
        .map_err(|_| rusqlite::Error::InvalidQuery)?,
        read_fixed32(row.get::<_, Vec<u8>>(8)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
        from_i64(
            row.get::<_, i64>(9)?,
            "checkpoint_operations.content_freeze",
        )
        .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, Option<Vec<u8>>>(10)?
            .map(read_fixed32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, Option<i64>>(11)?
            .map(|value| from_i64(value, "checkpoint_operations.candidate_freeze"))
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, Option<Vec<u8>>>(12)?
            .map(read_fixed32)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        match phase {
            CheckpointEventKind::Prepare | CheckpointEventKind::Pending => Some(row_operation_id),
            CheckpointEventKind::Finalize | CheckpointEventKind::Abort => None,
        },
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let Some(head_digest_bytes) = row.get::<_, Option<Vec<u8>>>(13)? else {
        return Err(rusqlite::Error::InvalidQuery);
    };
    let prev_head_digest = row
        .get::<_, Option<Vec<u8>>>(14)?
        .map(read_fixed32)
        .transpose()
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let head = Head::new(core, prev_head_digest).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let stored_head_digest: HeadDigest =
        read_fixed32(head_digest_bytes).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if head.digest().map_err(|_| rusqlite::Error::InvalidQuery)? != stored_head_digest {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(StoredOperation {
        row_operation_id,
        head,
    })
}

fn head_core_from_columns(
    identity: AnchorIdentity,
    lifecycle: HeadLifecycle,
    phase: CheckpointEventKind,
    stable_generation: u64,
    content_generation: u64,
    operation_generation: u64,
    content_root: ContentRootDigest,
    content_freeze: u64,
    candidate_root: Option<ContentRootDigest>,
    candidate_freeze: Option<u64>,
    candidate_membership_digest: Option<MemberDigest>,
    operation_id: Option<OperationId>,
) -> V2Result<HeadCore> {
    let head_core = HeadCore {
        identity,
        lifecycle,
        phase,
        stable_generation,
        content_generation,
        operation_generation,
        content_root,
        content_freeze,
        candidate_root,
        candidate_freeze,
        candidate_membership_digest,
        operation_id,
    };
    head_core.validate()?;
    Ok(head_core)
}

fn operation_lifecycle(phase: CheckpointEventKind) -> HeadLifecycle {
    match phase {
        CheckpointEventKind::Prepare | CheckpointEventKind::Pending => HeadLifecycle::InFlight,
        CheckpointEventKind::Finalize | CheckpointEventKind::Abort => HeadLifecycle::Stable,
    }
}

fn lifecycle_str(value: HeadLifecycle) -> &'static str {
    match value {
        HeadLifecycle::Stable => "stable",
        HeadLifecycle::InFlight => "in_flight",
    }
}

fn phase_str(value: CheckpointEventKind) -> &'static str {
    match value {
        CheckpointEventKind::Prepare => "prepare",
        CheckpointEventKind::Pending => "pending",
        CheckpointEventKind::Finalize => "finalize",
        CheckpointEventKind::Abort => "abort",
    }
}

fn parse_lifecycle(value: &str) -> V2Result<HeadLifecycle> {
    match value {
        "stable" => Ok(HeadLifecycle::Stable),
        "in_flight" => Ok(HeadLifecycle::InFlight),
        _ => Err(V2Error::AuthorityInvariantViolation {
            context: "invalid_lifecycle_projection",
        }),
    }
}

fn parse_phase(value: &str) -> V2Result<CheckpointEventKind> {
    match value {
        "prepare" => Ok(CheckpointEventKind::Prepare),
        "pending" => Ok(CheckpointEventKind::Pending),
        "finalize" => Ok(CheckpointEventKind::Finalize),
        "abort" => Ok(CheckpointEventKind::Abort),
        _ => Err(V2Error::AuthorityInvariantViolation {
            context: "invalid_phase_projection",
        }),
    }
}

fn open_verified_store_connection(
    path: &Path,
    mode: StoreOpenMode,
) -> V2Result<(PathBuf, Connection)> {
    let mut authority = crate::path_guard::capture_db_authority(path).map_err(map_db_error)?;
    if mode == StoreOpenMode::Existing && !crate::path_guard::db_authority_exists(&authority) {
        return Err(V2Error::DatabaseNotFound);
    }
    if mode == StoreOpenMode::Create && !crate::path_guard::db_authority_exists(&authority) {
        authority = crate::path_guard::materialize_db_leaf_authority(&authority.db_path)
            .map_err(map_db_error)?;
    }
    let conn = Connection::open_with_flags(&authority.db_path, open_flags_for(mode))?;
    match validate_opened_store_connection_authority(&conn, &authority) {
        Ok(()) => Ok((authority.db_path, conn)),
        Err(error) => {
            let _ = conn.close();
            Err(error)
        }
    }
}

fn open_flags_for(mode: StoreOpenMode) -> OpenFlags {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if mode == StoreOpenMode::Create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    flags
}

fn validate_opened_store_connection_authority(
    conn: &Connection,
    authority: &crate::path_guard::ValidatedDbAuthority,
) -> V2Result<()> {
    let handle = sqlite_main_db_handle(conn)?;
    crate::path_guard::validate_opened_db_handle_authority(handle, authority).map_err(map_db_error)
}

fn sqlite_main_db_handle(conn: &Connection) -> V2Result<*mut std::ffi::c_void> {
    #[cfg(windows)]
    {
        let sqlite = unsafe { conn.handle() };
        if sqlite.is_null() {
            return Err(V2Error::SqliteHandleUnavailable {
                context: "sqlite_connection_handle_null",
            });
        }
        let mut handle = std::ptr::null_mut::<std::ffi::c_void>();
        let result = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                sqlite,
                std::ptr::null(),
                rusqlite::ffi::SQLITE_FCNTL_WIN32_GET_HANDLE,
                (&mut handle as *mut *mut std::ffi::c_void).cast(),
            )
        };
        return sqlite_main_db_handle_from_result(result, handle);
    }
    #[cfg(not(windows))]
    {
        let _ = conn;
        Err(V2Error::UnsupportedPlatform)
    }
}

fn sqlite_main_db_handle_from_result(
    result: i32,
    handle: *mut std::ffi::c_void,
) -> V2Result<*mut std::ffi::c_void> {
    if result == rusqlite::ffi::SQLITE_OK && !handle.is_null() {
        return Ok(handle);
    }
    let context = if result == rusqlite::ffi::SQLITE_NOTFOUND {
        "sqlite_main_handle_file_control_unsupported"
    } else if result != rusqlite::ffi::SQLITE_OK {
        "sqlite_main_handle_file_control_failed"
    } else {
        "sqlite_main_handle_unavailable"
    };
    Err(V2Error::SqliteHandleUnavailable { context })
}

#[cfg(test)]
pub(crate) fn sqlite_main_db_handle_outcome_for_test(
    result: i32,
    handle_is_null: bool,
) -> V2Result<()> {
    let handle = if handle_is_null {
        std::ptr::null_mut()
    } else {
        1_usize as *mut std::ffi::c_void
    };
    sqlite_main_db_handle_from_result(result, handle).map(|_| ())
}

#[cfg(test)]
pub(crate) fn validate_membership_window_for_test(
    rows: &[OperationContentMembership],
    stable_generation: u64,
    content_generation: u64,
) -> V2Result<()> {
    validate_membership_window(rows, stable_generation, content_generation)
}

#[cfg(test)]
pub(crate) fn validate_head_history_successor_for_test(
    previous: Option<&Head>,
    current: &Head,
) -> V2Result<()> {
    validate_head_history_successor(previous, current)
}

fn zero_root_digest() -> ContentRootDigest {
    ContentRootDigest::new([0u8; 32])
}

fn map_db_error(error: DbError) -> V2Error {
    match error {
        DbError::UnsupportedPlatform => V2Error::UnsupportedPlatform,
        DbError::UnsupportedFilesystem => V2Error::UnsupportedFilesystem,
        DbError::InvalidPathBinding => V2Error::InvalidPathBinding,
        DbError::PathReparseRejected => V2Error::PathReparseRejected,
        DbError::HardlinkRejected => V2Error::HardlinkRejected,
        DbError::NumericOverflow => V2Error::NumericOverflow,
        DbError::Sqlite(error) => V2Error::Sqlite(error),
        DbError::Io(error) => V2Error::Io(error),
        _ => V2Error::AuthorityInvariantViolation {
            context: "path_validation_rejected",
        },
    }
}

fn to_i64(value: u64) -> V2Result<i64> {
    i64::try_from(value).map_err(|_| V2Error::NumericOverflow)
}

fn from_i64(value: i64, context: &'static str) -> V2Result<u64> {
    u64::try_from(value).map_err(|_| V2Error::AuthorityInvariantViolation { context })
}

fn read_fixed32<T>(bytes: Vec<u8>) -> V2Result<T>
where
    T: Fixed32Type,
{
    T::from_slice(&bytes)
}

trait Fixed32Type: Sized {
    fn from_slice(bytes: &[u8]) -> V2Result<Self>;
}

impl Fixed32Type for super::AnchorInstanceId {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for super::PathBindingDigest {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for super::PlanBindingDigest {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for OperationId {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for ContentRootDigest {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for super::GenesisDigest {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for MemberDigest {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for CheckpointEventDigest {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for HeadDigest {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for ContentEventDigest {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

impl Fixed32Type for super::SuccessorEventDigest {
    fn from_slice(bytes: &[u8]) -> V2Result<Self> {
        Self::from_slice(bytes)
    }
}

#[cfg(test)]
mod fixed32_type_tests {
    use super::read_fixed32;

    #[test]
    fn foundation_digest_types_support_read_fixed32() {
        let genesis_digest: super::super::GenesisDigest = read_fixed32(vec![1u8; 32]).unwrap();
        let successor_event_digest: super::super::SuccessorEventDigest =
            read_fixed32(vec![2u8; 32]).unwrap();
        let plan_binding_digest: super::super::PlanBindingDigest =
            read_fixed32(vec![3u8; 32]).unwrap();

        assert_eq!(genesis_digest.as_bytes(), &[1u8; 32]);
        assert_eq!(successor_event_digest.as_bytes(), &[2u8; 32]);
        assert_eq!(plan_binding_digest.as_bytes(), &[3u8; 32]);
    }
}
