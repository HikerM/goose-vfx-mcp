use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteRow;
use sqlx::{Column, Row, Sqlite, Transaction};

use super::{decode, integrity_error, map_sqlx};
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::fenced_projection_sink::{fenced_command_binding, fenced_receipt_binding};
use crate::mcp_platform::repository::PlanTarget;
use crate::verified_source_catalog::dao::ensure_source_schema_metadata_tx;

pub const CURRENT_SCHEMA_VERSION: i64 = 47;
pub const HTTPS_MANIFEST_CLAIM_LEASE_MS: i64 = 5 * 60 * 1000;

const V44_HTTPS_MANIFEST_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE https_manifest_provisions (
        provision_id TEXT PRIMARY KEY, actor TEXT NOT NULL, transport_session_binding TEXT NOT NULL,
        requested_url_id TEXT NOT NULL, final_url_id TEXT NOT NULL,
        raw_digest TEXT NOT NULL CHECK(length(raw_digest)=64 AND raw_digest NOT GLOB '*[^0-9a-f]*'),
        parsed_digest TEXT NOT NULL CHECK(length(parsed_digest)=64 AND parsed_digest NOT GLOB '*[^0-9a-f]*'),
        redirect_chain_digest TEXT NOT NULL CHECK(length(redirect_chain_digest)=64 AND redirect_chain_digest NOT GLOB '*[^0-9a-f]*'),
        frozen_bytes BLOB NOT NULL, token_hash TEXT NOT NULL UNIQUE CHECK(length(token_hash)=64 AND token_hash NOT GLOB '*[^0-9a-f]*'),
        expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms>0), status TEXT NOT NULL CHECK(status IN ('saved','claimed','consumed','rejected')),
        created_at_ms INTEGER NOT NULL, claimed_at_ms INTEGER, terminal_at_ms INTEGER,
        CHECK(status='saved' OR claimed_at_ms IS NOT NULL), CHECK(status IN ('consumed','rejected') OR terminal_at_ms IS NULL)
    ) STRICT"#,
    "CREATE INDEX https_manifest_provisions_actor_expiry ON https_manifest_provisions(actor, expires_at_ms)",
    "CREATE INDEX https_manifest_provisions_status_expiry ON https_manifest_provisions(status, expires_at_ms)",
    "CREATE TRIGGER https_manifest_provisions_immutable BEFORE UPDATE OF actor,transport_session_binding,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,frozen_bytes,token_hash,expires_at_ms,created_at_ms ON https_manifest_provisions BEGIN SELECT RAISE(ABORT,'HTTPS manifest provision immutable fields cannot change'); END",
    "CREATE TRIGGER https_manifest_provisions_status_transition BEFORE UPDATE OF status ON https_manifest_provisions WHEN NOT ((OLD.status='saved' AND NEW.status='claimed') OR (OLD.status='claimed' AND NEW.status IN ('consumed','rejected')) OR OLD.status=NEW.status) BEGIN SELECT RAISE(ABORT,'invalid HTTPS manifest provision status transition'); END",
];

const V44_HTTPS_MANIFEST_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "https_manifest_provisions",
        create_sql: V44_HTTPS_MANIFEST_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "https_manifest_provisions_actor_expiry",
        create_sql: V44_HTTPS_MANIFEST_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "https_manifest_provisions_status_expiry",
        create_sql: V44_HTTPS_MANIFEST_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "https_manifest_provisions_immutable",
        create_sql: V44_HTTPS_MANIFEST_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "https_manifest_provisions_status_transition",
        create_sql: V44_HTTPS_MANIFEST_STATEMENTS[4],
    },
];

pub async fn apply_v44(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V44_HTTPS_MANIFEST_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    validate_schema_contracts(tx, V44_HTTPS_MANIFEST_SCHEMA_CONTRACTS).await
}

const V45_HTTPS_MANIFEST_STATEMENTS: &[&str] = &[
    "ALTER TABLE https_manifest_provisions ADD COLUMN claim_lease_until_ms INTEGER CHECK(claim_lease_until_ms IS NULL OR claim_lease_until_ms > 0)",
    "CREATE INDEX https_manifest_provisions_claim_lease ON https_manifest_provisions(status, claim_lease_until_ms)",
];

const V45_HTTPS_MANIFEST_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[SchemaObjectContract {
    object_type: "index",
    name: "https_manifest_provisions_claim_lease",
    create_sql: V45_HTTPS_MANIFEST_STATEMENTS[1],
}];

pub async fn apply_v45(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V45_HTTPS_MANIFEST_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    validate_schema_contracts(tx, V45_HTTPS_MANIFEST_SCHEMA_CONTRACTS).await
}

const V46_HTTPS_MANIFEST_STATEMENTS: &[&str] = &[
    "ALTER TABLE https_manifest_provisions ADD COLUMN dns_evidence_digest TEXT CHECK(dns_evidence_digest IS NULL OR (length(dns_evidence_digest)=64 AND dns_evidence_digest NOT GLOB '*[^0-9a-f]*'))",
    "CREATE TRIGGER https_manifest_provisions_dns_evidence_required BEFORE INSERT ON https_manifest_provisions WHEN NEW.dns_evidence_digest IS NULL BEGIN SELECT RAISE(ABORT,'HTTPS manifest provision DNS evidence digest is required'); END",
    "CREATE TRIGGER https_manifest_provisions_dns_evidence_immutable BEFORE UPDATE OF dns_evidence_digest ON https_manifest_provisions BEGIN SELECT RAISE(ABORT,'HTTPS manifest provision immutable fields cannot change'); END",
];

const V46_HTTPS_MANIFEST_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "trigger",
        name: "https_manifest_provisions_dns_evidence_required",
        create_sql: V46_HTTPS_MANIFEST_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "https_manifest_provisions_dns_evidence_immutable",
        create_sql: V46_HTTPS_MANIFEST_STATEMENTS[2],
    },
];

pub async fn apply_v46(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V46_HTTPS_MANIFEST_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    validate_schema_contracts(tx, V46_HTTPS_MANIFEST_SCHEMA_CONTRACTS).await
}

const V47_HTTPS_MANIFEST_STATEMENTS: &[&str] = &[
    "ALTER TABLE https_manifest_provisions ADD COLUMN requested_url TEXT",
    "DROP TRIGGER https_manifest_provisions_immutable",
    "CREATE TRIGGER https_manifest_provisions_immutable BEFORE UPDATE OF actor,transport_session_binding,requested_url,requested_url_id,final_url_id,raw_digest,parsed_digest,redirect_chain_digest,frozen_bytes,token_hash,expires_at_ms,created_at_ms ON https_manifest_provisions BEGIN SELECT RAISE(ABORT,'HTTPS manifest provision immutable fields cannot change'); END",
];

const V47_HTTPS_MANIFEST_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[SchemaObjectContract {
    object_type: "trigger",
    name: "https_manifest_provisions_immutable",
    create_sql: V47_HTTPS_MANIFEST_STATEMENTS[2],
}];

pub async fn apply_v47(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V47_HTTPS_MANIFEST_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    validate_schema_contracts(tx, V47_HTTPS_MANIFEST_SCHEMA_CONTRACTS).await?;
    let requested_url = sqlx::query("PRAGMA table_info('https_manifest_provisions')")
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .find(|row| row.get::<String, _>("name") == "requested_url")
        .ok_or_else(integrity_error)?;
    if requested_url.get::<String, _>("type").to_ascii_uppercase() != "TEXT"
        || requested_url.get::<i64, _>("notnull") != 0
        || requested_url.get::<i64, _>("pk") != 0
    {
        return Err(integrity_error());
    }
    Ok(())
}

const PROJECTION_MUTATIONS_LEGACY_CREATE_TABLE_SQL: &str = r#"CREATE TABLE projection_mutations (
        mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        managed_mcp_id TEXT NOT NULL,
        expected_revision INTEGER NOT NULL,
        previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
        desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
        status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required','resolved')),
        writer_runtime_id TEXT,
        writer_sink_id TEXT,
        writer_anchor_instance_id TEXT,
        writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
        writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
        writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
        writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
        writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#;

const PROJECTION_MUTATIONS_WITNESS_V2_CREATE_TABLE_SQL: &str = r#"CREATE TABLE projection_mutations (
        mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        managed_mcp_id TEXT NOT NULL,
        expected_revision INTEGER NOT NULL,
        previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
        desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
        status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required','resolved')),
        writer_runtime_id TEXT,
        writer_sink_id TEXT,
        writer_anchor_instance_id TEXT,
        writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
        writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
        writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
        writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
        writer_witness_version INTEGER CHECK(writer_witness_version IS NULL OR writer_witness_version = 2),
        writer_provider_id TEXT CHECK(writer_provider_id IS NULL OR writer_provider_id IN ('system-keyring','in-memory-test')),
        writer_binding_domain TEXT,
        writer_observed_state_digest TEXT CHECK(writer_observed_state_digest IS NULL OR length(writer_observed_state_digest) = 64),
        writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#;

const PROJECTION_MUTATIONS_ACTIVE_INDEX_SQL: &str = "CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id) WHERE status IN ('started','config_committed','recovery_required')";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectionMutationsTableShape {
    LegacyV14,
    WitnessV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectionMutationsColumnContract {
    name: &'static str,
    declared_type: &'static str,
    not_null: bool,
    default_value: Option<&'static str>,
    primary_key_ordinal: i64,
    definition_sql: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectionMutationsIndexContract {
    name: &'static str,
    create_sql: &'static str,
    origin: &'static str,
    predicate_sql: Option<&'static str>,
    key_columns: &'static [ProjectionMutationsIndexKeyColumnContract],
    unique: bool,
    partial: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectionMutationsIndexKeyColumnContract {
    name: Option<&'static str>,
    collation: &'static str,
    descending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionMutationsColumnInfo {
    cid: i64,
    name: String,
    declared_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key_ordinal: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionMutationsIndexInfo {
    name: String,
    unique: bool,
    origin: String,
    partial: bool,
    key_columns: Vec<ProjectionMutationsIndexKeyColumnInfo>,
    create_sql: Option<String>,
    predicate_sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionMutationsIndexKeyColumnInfo {
    seqno: i64,
    name: Option<String>,
    collation: String,
    descending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionMutationsIndexInfoRow {
    seqno: i64,
    name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SchemaObjectContract {
    object_type: &'static str,
    name: &'static str,
    create_sql: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteInspectionV26SchemaState {
    Absent,
    Complete,
    PartialOrInvalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionMutationsIndexXInfoRow {
    seqno: i64,
    name: Option<String>,
    collation: Option<String>,
    descending: Option<bool>,
    key: Option<bool>,
}

const PROJECTION_MUTATIONS_LEGACY_COLUMNS: &[ProjectionMutationsColumnContract] = &[
    ProjectionMutationsColumnContract {
        name: "mutation_id",
        declared_type: "INTEGER",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 1,
        definition_sql: "mutation_id INTEGER PRIMARY KEY AUTOINCREMENT",
    },
    ProjectionMutationsColumnContract {
        name: "managed_mcp_id",
        declared_type: "TEXT",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "managed_mcp_id TEXT NOT NULL",
    },
    ProjectionMutationsColumnContract {
        name: "expected_revision",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "expected_revision INTEGER NOT NULL",
    },
    ProjectionMutationsColumnContract {
        name: "previous_enabled",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1))",
    },
    ProjectionMutationsColumnContract {
        name: "desired_enabled",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1))",
    },
    ProjectionMutationsColumnContract {
        name: "status",
        declared_type: "TEXT",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required','resolved'))",
    },
    ProjectionMutationsColumnContract {
        name: "writer_runtime_id",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_runtime_id TEXT",
    },
    ProjectionMutationsColumnContract {
        name: "writer_sink_id",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_sink_id TEXT",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_instance_id",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_instance_id TEXT",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_path_binding",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_key_epoch",
        declared_type: "INTEGER",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_sequence",
        declared_type: "INTEGER",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_root",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_commitment",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64)",
    },
    ProjectionMutationsColumnContract {
        name: "created_at_ms",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "created_at_ms INTEGER NOT NULL",
    },
    ProjectionMutationsColumnContract {
        name: "updated_at_ms",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "updated_at_ms INTEGER NOT NULL",
    },
];

const PROJECTION_MUTATIONS_WITNESS_V2_COLUMNS: &[ProjectionMutationsColumnContract] = &[
    ProjectionMutationsColumnContract {
        name: "mutation_id",
        declared_type: "INTEGER",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 1,
        definition_sql: "mutation_id INTEGER PRIMARY KEY AUTOINCREMENT",
    },
    ProjectionMutationsColumnContract {
        name: "managed_mcp_id",
        declared_type: "TEXT",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "managed_mcp_id TEXT NOT NULL",
    },
    ProjectionMutationsColumnContract {
        name: "expected_revision",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "expected_revision INTEGER NOT NULL",
    },
    ProjectionMutationsColumnContract {
        name: "previous_enabled",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1))",
    },
    ProjectionMutationsColumnContract {
        name: "desired_enabled",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1))",
    },
    ProjectionMutationsColumnContract {
        name: "status",
        declared_type: "TEXT",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required','resolved'))",
    },
    ProjectionMutationsColumnContract {
        name: "writer_runtime_id",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_runtime_id TEXT",
    },
    ProjectionMutationsColumnContract {
        name: "writer_sink_id",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_sink_id TEXT",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_instance_id",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_instance_id TEXT",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_path_binding",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_key_epoch",
        declared_type: "INTEGER",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_sequence",
        declared_type: "INTEGER",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_anchor_root",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_witness_version",
        declared_type: "INTEGER",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_witness_version INTEGER CHECK(writer_witness_version IS NULL OR writer_witness_version = 2)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_provider_id",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_provider_id TEXT CHECK(writer_provider_id IS NULL OR writer_provider_id IN ('system-keyring','in-memory-test'))",
    },
    ProjectionMutationsColumnContract {
        name: "writer_binding_domain",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_binding_domain TEXT",
    },
    ProjectionMutationsColumnContract {
        name: "writer_observed_state_digest",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_observed_state_digest TEXT CHECK(writer_observed_state_digest IS NULL OR length(writer_observed_state_digest) = 64)",
    },
    ProjectionMutationsColumnContract {
        name: "writer_commitment",
        declared_type: "TEXT",
        not_null: false,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64)",
    },
    ProjectionMutationsColumnContract {
        name: "created_at_ms",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "created_at_ms INTEGER NOT NULL",
    },
    ProjectionMutationsColumnContract {
        name: "updated_at_ms",
        declared_type: "INTEGER",
        not_null: true,
        default_value: None,
        primary_key_ordinal: 0,
        definition_sql: "updated_at_ms INTEGER NOT NULL",
    },
];

const PROJECTION_MUTATIONS_ACTIVE_INDEX_CONTRACT: ProjectionMutationsIndexContract =
    ProjectionMutationsIndexContract {
        name: "projection_mutations_active",
        create_sql: PROJECTION_MUTATIONS_ACTIVE_INDEX_SQL,
        origin: "c",
        predicate_sql: Some("status IN ('started','config_committed','recovery_required')"),
        key_columns: &[ProjectionMutationsIndexKeyColumnContract {
            name: Some("managed_mcp_id"),
            collation: "BINARY",
            descending: false,
        }],
        unique: true,
        partial: true,
    };

fn projection_mutations_schema_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "projection_mutations schema is malformed or only partially migrated",
    )
}

fn normalize_schema_sql(sql: &str) -> String {
    let mut tokens = Vec::new();
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '\'' || ch == '"' {
            let quote = ch;
            let mut token = String::new();
            token.push(ch);
            while let Some(next) = chars.next() {
                token.push(next);
                if next == quote {
                    if chars.peek().copied() == Some(quote) {
                        token.push(chars.next().expect("peeked quote must exist"));
                        continue;
                    }
                    break;
                }
            }
            tokens.push(token.to_ascii_lowercase());
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            let mut token = String::new();
            token.push(ch);
            while let Some(next) = chars.peek().copied() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    token.push(chars.next().expect("peeked token char must exist"));
                } else {
                    break;
                }
            }
            tokens.push(token.to_ascii_lowercase());
            continue;
        }
        if matches!(ch, '<' | '>' | '!' | '=') && chars.peek().copied() == Some('=') {
            let mut token = String::new();
            token.push(ch);
            token.push(chars.next().expect("peeked operator suffix must exist"));
            tokens.push(token);
            continue;
        }
        tokens.push(ch.to_string());
    }
    tokens.join(" ")
}

fn split_top_level_sql_items(sql: &str) -> McpPlatformResult<Vec<String>> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut depth = 0_i32;
    let mut quote = None;
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        current.push(ch);
        if let Some(active_quote) = quote {
            if ch == active_quote {
                if chars.peek().copied() == Some(active_quote) {
                    current.push(chars.next().expect("peeked escaped quote must exist"));
                    continue;
                }
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return Err(projection_mutations_schema_error());
                }
            }
            ',' if depth == 0 => {
                current.pop();
                items.push(current.trim().to_string());
                current.clear();
            }
            _ => {}
        }
    }
    if quote.is_some() || depth != 0 {
        return Err(projection_mutations_schema_error());
    }
    let tail = current.trim();
    if !tail.is_empty() {
        items.push(tail.to_string());
    }
    Ok(items)
}

fn normalize_table_items(sql: &str) -> McpPlatformResult<Vec<String>> {
    let open = sql
        .find('(')
        .ok_or_else(projection_mutations_schema_error)?;
    let close = sql
        .rfind(')')
        .ok_or_else(projection_mutations_schema_error)?;
    if close <= open {
        return Err(projection_mutations_schema_error());
    }
    split_top_level_sql_items(&sql[open + 1..close]).map(|items| {
        items
            .into_iter()
            .map(|item| normalize_schema_sql(&item))
            .collect()
    })
}

fn normalize_expected_projection_items(
    contract: &[ProjectionMutationsColumnContract],
) -> Vec<String> {
    contract
        .iter()
        .map(|column| normalize_schema_sql(column.definition_sql))
        .collect()
}

fn sqlite_identifier_literal(identifier: &str) -> String {
    identifier.replace('\'', "''")
}

fn row_has_column(row: &SqliteRow, name: &str) -> bool {
    row.columns().iter().any(|column| column.name() == name)
}

fn row_try_get_optional_string(row: &SqliteRow, name: &str) -> McpPlatformResult<Option<String>> {
    if !row_has_column(row, name) {
        return Ok(None);
    }
    row.try_get(name).map_err(map_sqlx)
}

fn row_try_get_optional_i64(row: &SqliteRow, name: &str) -> McpPlatformResult<Option<i64>> {
    if !row_has_column(row, name) {
        return Ok(None);
    }
    row.try_get(name).map_err(map_sqlx)
}

async fn current_schema_version(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<i64> {
    sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(version), 0) FROM schema_version")
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)
}

async fn schema_object_sql(
    tx: &mut Transaction<'_, Sqlite>,
    object_type: &str,
    name: &str,
) -> McpPlatformResult<Option<String>> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT sql FROM sqlite_master WHERE type = ? AND name = ?",
    )
    .bind(object_type)
    .bind(name)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx)
    .map(|value| value.flatten())
}

async fn schema_object_names(
    tx: &mut Transaction<'_, Sqlite>,
    object_type: &str,
) -> McpPlatformResult<Vec<String>> {
    sqlx::query_scalar::<_, String>("SELECT name FROM sqlite_master WHERE type = ?")
        .bind(object_type)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)
}

async fn validate_schema_contracts(
    tx: &mut Transaction<'_, Sqlite>,
    contracts: &[SchemaObjectContract],
) -> McpPlatformResult<()> {
    for contract in contracts {
        let actual = schema_object_sql(tx, contract.object_type, contract.name)
            .await?
            .ok_or_else(integrity_error)?;
        if normalize_schema_sql(&actual) != normalize_schema_sql(contract.create_sql) {
            return Err(integrity_error());
        }
    }
    Ok(())
}

pub(crate) async fn validate_remote_inspection_v25_schema(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    validate_schema_contracts(tx, V25_REMOTE_INSPECTION_SCHEMA_CONTRACTS).await
}

fn is_remote_inspection_v26_managed_object_name(name: &str, expected_names: &[&str]) -> bool {
    expected_names.iter().any(|expected| *expected == name)
        || (name.starts_with("mcp_intake_remote_inspection_")
            && (name.contains("_v26") || name.ends_with("_b26")))
}

async fn validate_remote_inspection_v26_schema_contracts_and_managed_set(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    validate_schema_contracts(tx, V26_REMOTE_INSPECTION_SCHEMA_CONTRACTS).await?;
    for object_type in ["table", "index", "trigger"] {
        let mut expected = V26_REMOTE_INSPECTION_SCHEMA_CONTRACTS
            .iter()
            .filter(|contract| contract.object_type == object_type)
            .map(|contract| contract.name)
            .collect::<Vec<_>>();
        let mut actual = schema_object_names(tx, object_type)
            .await?
            .into_iter()
            .filter(|name| is_remote_inspection_v26_managed_object_name(name, &expected))
            .collect::<Vec<_>>();
        expected.sort_unstable();
        actual.sort();
        if actual.len() != expected.len()
            || actual
                .iter()
                .zip(expected.iter())
                .any(|(actual_name, expected_name)| actual_name != expected_name)
        {
            return Err(integrity_error());
        }
    }
    Ok(())
}

pub(crate) async fn validate_remote_inspection_v26_schema(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    validate_remote_inspection_v26_schema_contracts_and_managed_set(tx).await
}

pub(crate) async fn validate_governed_source_refresh_schema(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    let version = current_schema_version(tx).await?;
    if version >= 29 {
        validate_schema_contracts(tx, V29_GOVERNED_SOURCE_REFRESH_SCHEMA_CONTRACTS).await?;
    }
    if version >= 30 {
        validate_schema_contracts(tx, V30_GOVERNED_SOURCE_REFRESH_SCHEMA_CONTRACTS).await?;
    }
    if version >= 31 {
        validate_schema_contracts(tx, V31_SOURCE_PROVISIONING_SCHEMA_CONTRACTS).await?;
    }
    if version == 32 {
        validate_schema_contracts(tx, V32_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS).await?;
    }
    if version == 33 {
        validate_schema_contracts(tx, V33_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS).await?;
    }
    if version == 34 {
        validate_schema_contracts(tx, V34_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS).await?;
    }
    if version == 35 {
        validate_schema_contracts(tx, V35_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS).await?;
    }
    if version == 36 {
        validate_schema_contracts(tx, V36_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS).await?;
    }
    if version == 37 {
        for contract in V36_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS {
            if contract.name != "mcp_profile_lifecycle_impact_v36_migration_dispositions" {
                validate_schema_contracts(tx, std::slice::from_ref(contract)).await?;
            }
        }
        validate_schema_contracts(tx, V37_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS).await?;
    }
    if version >= 38 {
        for contract in V36_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS {
            if contract.name != "mcp_profile_lifecycle_impact_v36_migration_dispositions" {
                validate_schema_contracts(tx, std::slice::from_ref(contract)).await?;
            }
        }
        validate_schema_contracts(tx, V38_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS).await?;
    }
    if version >= 40 {
        validate_effect_fence_state(tx).await?;
    } else if version >= 39 {
        validate_effect_fence_state_v39(tx).await?;
    }
    if version >= 43 {
        validate_fenced_effect_authority_v43_state(tx).await?;
    } else if version >= 42 {
        validate_fenced_effect_receipt_state(tx).await?;
    }
    Ok(())
}

fn infer_index_origin(name: &str, create_sql: Option<&str>) -> String {
    if let Some(sql) = create_sql {
        if sql.trim().is_empty() {
            return String::new();
        }
        return "c".to_string();
    }
    if name.starts_with("sqlite_autoindex_") {
        return "auto".to_string();
    }
    String::new()
}

fn normalized_partial_index_predicate(create_sql: &str) -> Option<String> {
    let normalized = normalize_schema_sql(create_sql);
    normalized
        .split_once(" where ")
        .map(|(_, predicate)| predicate.trim().to_string())
}

async fn projection_mutations_columns(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<Vec<ProjectionMutationsColumnInfo>> {
    sqlx::query("PRAGMA table_info('projection_mutations')")
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(|row| {
            Ok(ProjectionMutationsColumnInfo {
                cid: row.try_get("cid").map_err(map_sqlx)?,
                name: row.try_get("name").map_err(map_sqlx)?,
                declared_type: row.try_get::<String, _>("type").map_err(map_sqlx)?,
                not_null: row.try_get::<i64, _>("notnull").map_err(map_sqlx)? != 0,
                default_value: row.try_get("dflt_value").map_err(map_sqlx)?,
                primary_key_ordinal: row.try_get("pk").map_err(map_sqlx)?,
            })
        })
        .collect()
}

async fn projection_mutations_create_sql(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='projection_mutations'",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .ok_or_else(projection_mutations_schema_error)
}

async fn projection_mutations_index_key_columns(
    tx: &mut Transaction<'_, Sqlite>,
    name: &str,
) -> McpPlatformResult<Vec<ProjectionMutationsIndexKeyColumnInfo>> {
    let escaped_name = sqlite_identifier_literal(name);
    let info_rows = sqlx::query(&format!("PRAGMA index_info('{escaped_name}')"))
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    let info_rows = info_rows
        .into_iter()
        .map(|row| {
            Ok(ProjectionMutationsIndexInfoRow {
                seqno: row.try_get("seqno").map_err(map_sqlx)?,
                name: row.try_get("name").map_err(map_sqlx)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let xinfo_rows = match sqlx::query(&format!("PRAGMA index_xinfo('{escaped_name}')"))
        .fetch_all(&mut **tx)
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            let message = error.to_string().to_ascii_lowercase();
            if message.contains("index_xinfo") && message.contains("no such") {
                Vec::new()
            } else {
                return Err(map_sqlx(error));
            }
        }
    };
    let xinfo_rows = xinfo_rows
        .into_iter()
        .map(|row| {
            Ok(ProjectionMutationsIndexXInfoRow {
                seqno: row.try_get("seqno").map_err(map_sqlx)?,
                name: row.try_get("name").map_err(map_sqlx)?,
                collation: row_try_get_optional_string(&row, "coll")?,
                descending: row_try_get_optional_i64(&row, "desc")?.map(|value| value != 0),
                key: row_try_get_optional_i64(&row, "key")?.map(|value| value != 0),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut sorted_info_rows = info_rows;
    sorted_info_rows.sort_by_key(|row| row.seqno);
    let mut expected_key_rows = sorted_info_rows
        .iter()
        .map(|row| (row.seqno, row.name.as_deref()))
        .collect::<Vec<_>>();
    expected_key_rows.sort_unstable_by_key(|(seqno, _)| *seqno);

    let mut indexed_key_rows = xinfo_rows
        .iter()
        .filter_map(|row| {
            row.key
                .filter(|key| *key)
                .map(|_| (row.seqno, row.name.as_deref()))
        })
        .collect::<Vec<_>>();
    indexed_key_rows.sort_unstable_by_key(|(seqno, _)| *seqno);
    if !indexed_key_rows.is_empty() && indexed_key_rows != expected_key_rows {
        return Err(projection_mutations_schema_error());
    }

    Ok(sorted_info_rows
        .into_iter()
        .map(|row| {
            let xinfo = xinfo_rows
                .iter()
                .find(|candidate| {
                    candidate.seqno == row.seqno
                        && (candidate.name.is_none() || candidate.name == row.name)
                })
                .or_else(|| {
                    xinfo_rows
                        .iter()
                        .find(|candidate| candidate.seqno == row.seqno)
                });
            ProjectionMutationsIndexKeyColumnInfo {
                seqno: row.seqno,
                name: row.name,
                collation: xinfo
                    .and_then(|row| row.collation.clone())
                    .unwrap_or_else(|| "BINARY".to_string()),
                descending: xinfo.and_then(|row| row.descending).unwrap_or(false),
            }
        })
        .collect())
}

async fn projection_mutations_indexes(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<Vec<ProjectionMutationsIndexInfo>> {
    let index_rows = sqlx::query("PRAGMA index_list('projection_mutations')")
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    let mut indexes = Vec::with_capacity(index_rows.len());
    for index_row in index_rows {
        let name: String = index_row.try_get("name").map_err(map_sqlx)?;
        let create_sql = sqlx::query_scalar::<_, Option<String>>(
            "SELECT sql FROM sqlite_master WHERE type='index' AND tbl_name='projection_mutations' AND name = ?",
        )
        .bind(&name)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx)?
        .flatten();
        let origin = row_try_get_optional_string(&index_row, "origin")?
            .unwrap_or_else(|| infer_index_origin(&name, create_sql.as_deref()));
        let predicate_sql = create_sql
            .as_deref()
            .and_then(normalized_partial_index_predicate);
        let partial = row_try_get_optional_i64(&index_row, "partial")?
            .map(|value| value != 0)
            .unwrap_or_else(|| predicate_sql.is_some());
        indexes.push(ProjectionMutationsIndexInfo {
            name: name.clone(),
            unique: index_row.try_get::<i64, _>("unique").map_err(map_sqlx)? != 0,
            origin,
            partial,
            key_columns: projection_mutations_index_key_columns(tx, &name).await?,
            create_sql,
            predicate_sql,
        });
    }
    indexes.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(indexes)
}

fn projection_mutations_index_matches_contract(
    actual: &ProjectionMutationsIndexInfo,
    expected: &ProjectionMutationsIndexContract,
) -> bool {
    actual.name == expected.name
        && actual.unique == expected.unique
        && actual.partial == expected.partial
        && actual.origin.eq_ignore_ascii_case(expected.origin)
        && actual.key_columns.len() == expected.key_columns.len()
        && actual
            .key_columns
            .iter()
            .zip(expected.key_columns)
            .all(|(actual, expected)| {
                actual.name.as_deref() == expected.name
                    && actual.collation.eq_ignore_ascii_case(expected.collation)
                    && actual.descending == expected.descending
            })
        && actual.create_sql.as_deref().map(normalize_schema_sql)
            == Some(normalize_schema_sql(expected.create_sql))
        && actual.predicate_sql == expected.predicate_sql.map(normalize_schema_sql)
}

fn projection_mutations_indexes_match_contract(
    indexes: &[ProjectionMutationsIndexInfo],
    require_exact_set: bool,
) -> bool {
    let expected = [PROJECTION_MUTATIONS_ACTIVE_INDEX_CONTRACT];
    expected.iter().all(|contract| {
        indexes
            .iter()
            .find(|index| index.name == contract.name)
            .is_some_and(|index| projection_mutations_index_matches_contract(index, contract))
    }) && (!require_exact_set
        || (indexes.len() == expected.len()
            && indexes
                .iter()
                .all(|index| expected.iter().any(|contract| contract.name == index.name))))
}

fn projection_mutations_matches_contract(
    columns: &[ProjectionMutationsColumnInfo],
    table_items: &[String],
    indexes: &[ProjectionMutationsIndexInfo],
    expected_columns: &[ProjectionMutationsColumnContract],
    require_exact_index_set: bool,
) -> bool {
    let column_contract_matches = columns.len() == expected_columns.len()
        && columns
            .iter()
            .zip(expected_columns)
            .all(|(actual, expected)| {
                actual.cid >= 0
                    && actual.name == expected.name
                    && actual.declared_type == expected.declared_type
                    && actual.not_null == expected.not_null
                    && actual.default_value.as_deref().map(normalize_schema_sql)
                        == expected.default_value.map(normalize_schema_sql)
                    && actual.primary_key_ordinal == expected.primary_key_ordinal
            });
    let item_contract_matches =
        table_items == normalize_expected_projection_items(expected_columns);
    let index_contract_matches =
        projection_mutations_indexes_match_contract(indexes, require_exact_index_set);
    column_contract_matches && item_contract_matches && index_contract_matches
}

pub(crate) async fn classify_projection_mutations_schema(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<ProjectionMutationsTableShape> {
    let columns = projection_mutations_columns(tx).await?;
    let table_items = normalize_table_items(&projection_mutations_create_sql(tx).await?)?;
    let indexes = projection_mutations_indexes(tx).await?;
    if projection_mutations_matches_contract(
        &columns,
        &table_items,
        &indexes,
        PROJECTION_MUTATIONS_LEGACY_COLUMNS,
        false,
    ) {
        return Ok(ProjectionMutationsTableShape::LegacyV14);
    }
    if projection_mutations_matches_contract(
        &columns,
        &table_items,
        &indexes,
        PROJECTION_MUTATIONS_WITNESS_V2_COLUMNS,
        true,
    ) {
        return Ok(ProjectionMutationsTableShape::WitnessV2);
    }
    Err(projection_mutations_schema_error())
}

pub(crate) async fn validate_projection_mutations_schema(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    if classify_projection_mutations_schema(tx).await? != ProjectionMutationsTableShape::WitnessV2 {
        return Err(projection_mutations_schema_error());
    }
    match current_schema_version(tx).await? {
        version if version < 25 => {
            if inspect_remote_inspection_v26_schema_state(tx).await?
                != RemoteInspectionV26SchemaState::Absent
            {
                return Err(integrity_error());
            }
            Ok(())
        }
        25 => {
            validate_remote_inspection_v25_schema(tx).await?;
            match inspect_remote_inspection_v26_schema_state(tx).await? {
                RemoteInspectionV26SchemaState::Absent => Ok(()),
                RemoteInspectionV26SchemaState::Complete
                | RemoteInspectionV26SchemaState::PartialOrInvalid => Err(integrity_error()),
            }
        }
        version if should_validate_remote_inspection_v26_schema(version) => {
            validate_remote_inspection_v25_schema(tx).await?;
            validate_remote_inspection_v26_schema(tx).await
        }
        _ => Err(integrity_error()),
    }
}

fn should_validate_remote_inspection_v26_schema(version: i64) -> bool {
    (26..=CURRENT_SCHEMA_VERSION).contains(&version)
}

pub async fn apply_v1(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V1_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

const V1_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE manifest_blobs (
        manifest_digest TEXT PRIMARY KEY CHECK(length(manifest_digest) = 64),
        mcp_id TEXT NOT NULL,
        version TEXT NOT NULL,
        canonical_bytes BLOB NOT NULL,
        proof_json TEXT NOT NULL,
        trust_tier_json TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        UNIQUE(mcp_id, version)
    )"#,
    r#"CREATE TABLE managed_mcps (
        managed_mcp_id TEXT PRIMARY KEY,
        mcp_id TEXT NOT NULL,
        installation_scope TEXT NOT NULL,
        state_json TEXT NOT NULL,
        revision INTEGER NOT NULL DEFAULT 0 CHECK(revision >= 0),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        UNIQUE(mcp_id, installation_scope)
    )"#,
    r#"CREATE TABLE managed_versions (
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        version TEXT NOT NULL,
        manifest_digest TEXT NOT NULL REFERENCES manifest_blobs(manifest_digest) ON DELETE RESTRICT,
        installation_root TEXT,
        verified INTEGER NOT NULL CHECK(verified IN (0, 1)),
        active INTEGER NOT NULL CHECK(active IN (0, 1)),
        adapter_evidence_json TEXT,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(managed_mcp_id, version)
    )"#,
    r#"CREATE UNIQUE INDEX managed_versions_one_active
        ON managed_versions(managed_mcp_id) WHERE active = 1"#,
    r#"CREATE INDEX managed_versions_manifest_digest
        ON managed_versions(manifest_digest)"#,
    r#"CREATE TABLE connection_projections (
        managed_mcp_id TEXT PRIMARY KEY REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        link_key TEXT NOT NULL UNIQUE,
        projection_json TEXT NOT NULL,
        revision INTEGER NOT NULL DEFAULT 0 CHECK(revision >= 0),
        updated_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE install_plans (
        plan_id TEXT PRIMARY KEY,
        plan_digest TEXT NOT NULL CHECK(length(plan_digest) = 64),
        envelope_digest TEXT NOT NULL CHECK(length(envelope_digest) = 64),
        manifest_digest TEXT NOT NULL REFERENCES manifest_blobs(manifest_digest) ON DELETE RESTRICT,
        operation TEXT NOT NULL CHECK(operation IN ('register','install','update','repair','uninstall','health')),
        target_json TEXT NOT NULL,
        plan_json TEXT NOT NULL,
        policy_evidence_json TEXT NOT NULL,
        confirmation_evidence_json TEXT NOT NULL,
        expires_at_ms INTEGER NOT NULL,
        idempotency_key TEXT NOT NULL UNIQUE,
        actor TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        UNIQUE(plan_id, plan_digest)
    )"#,
    r#"CREATE INDEX install_plans_manifest_digest ON install_plans(manifest_digest)"#,
    r#"CREATE TABLE tasks (
        task_id TEXT PRIMARY KEY,
        plan_id TEXT NOT NULL,
        plan_digest TEXT NOT NULL,
        operation TEXT NOT NULL CHECK(operation IN ('register','install','update','repair','uninstall','health')),
        idempotency_key TEXT NOT NULL,
        status TEXT NOT NULL CHECK(status IN (
            'planned','awaiting_confirmation','queued','running','cancelling','verifying',
            'activating','rolling_back','succeeded','failed','cancelled','interrupted','recovery_required'
        )),
        actor TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        heartbeat_at_ms INTEGER,
        progress INTEGER NOT NULL DEFAULT 0 CHECK(progress BETWEEN 0 AND 100),
        step_cursor INTEGER NOT NULL DEFAULT 0 CHECK(step_cursor >= 0),
        adapter_evidence_json TEXT,
        redacted_error_json TEXT,
        rollback_status TEXT NOT NULL DEFAULT 'not_required' CHECK(rollback_status IN (
            'not_required','pending','in_progress','complete','incomplete'
        )),
        rollback_evidence_json TEXT,
        revision INTEGER NOT NULL DEFAULT 0 CHECK(revision >= 0),
        event_sequence INTEGER NOT NULL DEFAULT 0 CHECK(event_sequence >= 0),
        retry_idempotency_key TEXT,
        FOREIGN KEY(plan_id, plan_digest) REFERENCES install_plans(plan_id, plan_digest) ON DELETE RESTRICT,
        UNIQUE(operation, idempotency_key)
    )"#,
    r#"CREATE INDEX tasks_status_heartbeat ON tasks(status, heartbeat_at_ms)"#,
    r#"CREATE INDEX tasks_plan_id ON tasks(plan_id)"#,
    r#"CREATE TABLE task_steps (
        task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
        status TEXT NOT NULL CHECK(status IN ('not_started','started','committed')),
        idempotency_token TEXT NOT NULL,
        compensation_json TEXT NOT NULL,
        evidence_json TEXT,
        started_at_ms INTEGER,
        committed_at_ms INTEGER,
        PRIMARY KEY(task_id, ordinal),
        UNIQUE(task_id, idempotency_token)
    )"#,
    r#"CREATE TABLE audit_events (
        event_id INTEGER PRIMARY KEY AUTOINCREMENT,
        task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        sequence INTEGER NOT NULL CHECK(sequence > 0),
        event_type TEXT NOT NULL CHECK(event_type IN (
            'task_created','task_status_changed','step_started','step_committed',
            'recovery_interrupted','confirmation_recorded','cancellation_requested','retry_queued'
        )),
        actor TEXT NOT NULL,
        occurred_at_ms INTEGER NOT NULL,
        payload_json TEXT NOT NULL,
        redacted_error_json TEXT,
        UNIQUE(task_id, sequence)
    )"#,
    r#"CREATE INDEX audit_events_occurred_at ON audit_events(occurred_at_ms, event_id)"#,
    r#"CREATE INDEX audit_events_task_sequence ON audit_events(task_id, sequence)"#,
];

pub async fn apply_v2(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V2_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

const V2_STATEMENTS: &[&str] = &[
    "ALTER TABLE managed_mcps ADD COLUMN distribution_adapter TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE managed_mcps ADD COLUMN active_manifest_digest TEXT",
    "ALTER TABLE managed_mcps ADD COLUMN active_version TEXT",
    "ALTER TABLE managed_mcps ADD COLUMN owner_task_id TEXT REFERENCES tasks(task_id) ON DELETE RESTRICT",
    "ALTER TABLE connection_projections ADD COLUMN plan_id TEXT REFERENCES install_plans(plan_id) ON DELETE RESTRICT",
    "ALTER TABLE connection_projections ADD COLUMN manifest_digest TEXT REFERENCES manifest_blobs(manifest_digest) ON DELETE RESTRICT",
    "ALTER TABLE connection_projections ADD COLUMN owner_task_id TEXT REFERENCES tasks(task_id) ON DELETE RESTRICT",
    "ALTER TABLE connection_projections ADD COLUMN projection_digest TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE tasks ADD COLUMN owner_id TEXT",
    "ALTER TABLE tasks ADD COLUMN lease_expires_at_ms INTEGER",
    "ALTER TABLE tasks ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0)",
    "ALTER TABLE task_steps ADD COLUMN adapter_id TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE task_steps ADD COLUMN adapter_version TEXT NOT NULL DEFAULT ''",
    r#"CREATE TABLE task_retry_attempts (
        task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        attempt INTEGER NOT NULL CHECK(attempt > 0),
        idempotency_key TEXT NOT NULL,
        requested_from_status TEXT NOT NULL,
        actor TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(task_id, attempt),
        UNIQUE(task_id, idempotency_key)
    )"#,
    r#"CREATE TABLE task_step_history (
        task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        attempt INTEGER NOT NULL CHECK(attempt >= 0),
        ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
        status TEXT NOT NULL CHECK(status IN ('not_started','started','committed')),
        idempotency_token TEXT NOT NULL,
        compensation_json TEXT NOT NULL,
        evidence_json TEXT,
        started_at_ms INTEGER,
        committed_at_ms INTEGER,
        adapter_id TEXT NOT NULL,
        adapter_version TEXT NOT NULL,
        PRIMARY KEY(task_id, attempt, ordinal),
        UNIQUE(task_id, attempt, idempotency_token)
    )"#,
    r#"CREATE TABLE health_task_requests (
        task_id TEXT PRIMARY KEY REFERENCES tasks(task_id) ON DELETE RESTRICT,
        managed_mcp_id TEXT NOT NULL,
        mode TEXT NOT NULL CHECK(mode IN ('registration','runtime'))
    )"#,
    r#"CREATE TABLE health_observations (
        observation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        managed_mcp_id TEXT NOT NULL,
        task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        check_type TEXT NOT NULL,
        result_code TEXT NOT NULL CHECK(result_code IN (
            'healthy','unhealthy','blocked_auth','incompatible','timeout','cancelled'
        )),
        latency_ms INTEGER NOT NULL CHECK(latency_ms >= 0),
        capabilities_digest TEXT,
        tools_digest TEXT,
        checked_at_ms INTEGER NOT NULL,
        detail_json TEXT NOT NULL
    )"#,
    r#"CREATE TABLE projection_mutations (
        mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        managed_mcp_id TEXT NOT NULL,
        expected_revision INTEGER NOT NULL,
        previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
        desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
        status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required')),
        writer_runtime_id TEXT,
        writer_sink_id TEXT,
        writer_anchor_instance_id TEXT,
        writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
        writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
        writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
        writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
        writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    "CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id) WHERE status IN ('started','config_committed','recovery_required')",
    "CREATE INDEX health_observations_managed_checked ON health_observations(managed_mcp_id, checked_at_ms DESC, observation_id DESC)",
    "CREATE UNIQUE INDEX health_observations_task ON health_observations(task_id)",
    "CREATE INDEX tasks_claim ON tasks(status, lease_expires_at_ms, created_at_ms, task_id)",
    "CREATE INDEX task_retry_attempts_task ON task_retry_attempts(task_id, attempt)",
];

pub async fn apply_v3(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V3_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

const V3_STATEMENTS: &[&str] = &[
    "ALTER TABLE task_steps ADD COLUMN compensation_status TEXT NOT NULL DEFAULT 'pending' CHECK(compensation_status IN ('pending','started','committed'))",
    "ALTER TABLE task_steps ADD COLUMN compensation_started_at_ms INTEGER",
    "ALTER TABLE task_steps ADD COLUMN compensation_committed_at_ms INTEGER",
    "ALTER TABLE task_step_history ADD COLUMN compensation_status TEXT NOT NULL DEFAULT 'pending' CHECK(compensation_status IN ('pending','started','committed'))",
    "ALTER TABLE task_step_history ADD COLUMN compensation_started_at_ms INTEGER",
    "ALTER TABLE task_step_history ADD COLUMN compensation_committed_at_ms INTEGER",
];

pub async fn apply_v4(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V4_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    let rows = sqlx::query(
        r#"SELECT t.task_id, p.target_json
           FROM tasks t JOIN install_plans p ON p.plan_id = t.plan_id
           WHERE t.operation IN ('install','update','repair','uninstall')"#,
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    for row in rows {
        let target: PlanTarget =
            decode(&row.try_get::<String, _>("target_json").map_err(map_sqlx)?)?;
        if let Some(managed_mcp_id) = target.managed_mcp_id {
            sqlx::query(
                "INSERT INTO lifecycle_task_targets(task_id, managed_mcp_id) VALUES (?, ?)",
            )
            .bind(row.try_get::<String, _>("task_id").map_err(map_sqlx)?)
            .bind(managed_mcp_id)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        }
    }
    let active_rows = sqlx::query(
        r#"SELECT ltt.managed_mcp_id,t.task_id,t.operation
           FROM lifecycle_task_targets ltt JOIN tasks t ON t.task_id = ltt.task_id
           WHERE t.status NOT IN ('succeeded','failed','cancelled')
           ORDER BY t.created_at_ms,t.task_id"#,
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    for row in active_rows {
        sqlx::query(
            "INSERT INTO managed_lifecycle_leases(managed_mcp_id,task_id,operation,acquired_at_ms) VALUES (?,?,?,0)",
        )
        .bind(row.try_get::<String, _>("managed_mcp_id").map_err(map_sqlx)?)
        .bind(row.try_get::<String, _>("task_id").map_err(map_sqlx)?)
        .bind(row.try_get::<String, _>("operation").map_err(map_sqlx)?)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    }
    Ok(())
}

const V4_STATEMENTS: &[&str] = &[
    "ALTER TABLE health_task_requests RENAME TO health_task_requests_v3",
    r#"CREATE TABLE health_task_requests (
        task_id TEXT PRIMARY KEY REFERENCES tasks(task_id) ON DELETE RESTRICT,
        managed_mcp_id TEXT NOT NULL,
        mode TEXT NOT NULL CHECK(mode IN ('registration','runtime'))
    )"#,
    "INSERT INTO health_task_requests(task_id,managed_mcp_id,mode) SELECT task_id,managed_mcp_id,mode FROM health_task_requests_v3",
    "DROP TABLE health_task_requests_v3",
    "ALTER TABLE health_observations RENAME TO health_observations_v3",
    r#"CREATE TABLE health_observations (
        observation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        managed_mcp_id TEXT NOT NULL,
        task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        check_type TEXT NOT NULL,
        result_code TEXT NOT NULL CHECK(result_code IN (
            'healthy','unhealthy','blocked_auth','incompatible','timeout','cancelled'
        )),
        latency_ms INTEGER NOT NULL CHECK(latency_ms >= 0),
        capabilities_digest TEXT,
        tools_digest TEXT,
        checked_at_ms INTEGER NOT NULL,
        detail_json TEXT NOT NULL
    )"#,
    "INSERT INTO health_observations(observation_id,managed_mcp_id,task_id,check_type,result_code,latency_ms,capabilities_digest,tools_digest,checked_at_ms,detail_json) SELECT observation_id,managed_mcp_id,task_id,check_type,result_code,latency_ms,capabilities_digest,tools_digest,checked_at_ms,detail_json FROM health_observations_v3",
    "DROP TABLE health_observations_v3",
    "CREATE INDEX health_observations_managed_checked ON health_observations(managed_mcp_id, checked_at_ms DESC, observation_id DESC)",
    "CREATE UNIQUE INDEX health_observations_task ON health_observations(task_id)",
    "ALTER TABLE projection_mutations RENAME TO projection_mutations_v3",
    r#"CREATE TABLE projection_mutations (
        mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        managed_mcp_id TEXT NOT NULL,
        expected_revision INTEGER NOT NULL,
        previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
        desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
        status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required')),
        writer_runtime_id TEXT,
        writer_sink_id TEXT,
        writer_anchor_instance_id TEXT,
        writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
        writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
        writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
        writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
        writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    "INSERT INTO projection_mutations(mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,created_at_ms,updated_at_ms) SELECT mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,created_at_ms,updated_at_ms FROM projection_mutations_v3",
    "DROP TABLE projection_mutations_v3",
    "CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id) WHERE status IN ('started','config_committed','recovery_required')",
    "ALTER TABLE managed_versions ADD COLUMN artifact_digest TEXT CHECK(artifact_digest IS NULL OR length(artifact_digest) = 64)",
    "ALTER TABLE managed_versions ADD COLUMN verification_evidence_json TEXT",
    "ALTER TABLE managed_versions ADD COLUMN materialized_tree_digest TEXT CHECK(materialized_tree_digest IS NULL OR length(materialized_tree_digest) = 64)",
    "ALTER TABLE managed_versions ADD COLUMN activation_state TEXT NOT NULL DEFAULT 'inactive' CHECK(activation_state IN ('staged','inactive','active','retained','removal_pending'))",
    "ALTER TABLE tasks ADD COLUMN finalization_failures INTEGER NOT NULL DEFAULT 0 CHECK(finalization_failures >= 0)",
    r#"CREATE TABLE artifact_cache (
        artifact_digest TEXT PRIMARY KEY CHECK(length(artifact_digest) = 64),
        source_origin TEXT NOT NULL,
        size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
        adapter_id TEXT NOT NULL,
        adapter_version TEXT NOT NULL,
        platform_selector TEXT NOT NULL,
        verification_evidence_json TEXT NOT NULL,
        reference_count INTEGER NOT NULL DEFAULT 0 CHECK(reference_count >= 0),
        verified_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE artifact_claims (
        artifact_digest TEXT PRIMARY KEY CHECK(length(artifact_digest) = 64),
        owner_task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        status TEXT NOT NULL CHECK(status IN ('fetching','verified','released')),
        updated_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE installation_ownership (
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        version TEXT NOT NULL,
        relative_path TEXT NOT NULL,
        path_kind TEXT NOT NULL CHECK(path_kind IN ('file','directory')),
        expected_digest TEXT,
        owner_task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        remove_on_uninstall INTEGER NOT NULL CHECK(remove_on_uninstall IN (0,1)),
        PRIMARY KEY(managed_mcp_id, version, relative_path),
        FOREIGN KEY(managed_mcp_id, version) REFERENCES managed_versions(managed_mcp_id, version) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE activation_journal (
        activation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        managed_mcp_id TEXT NOT NULL,
        task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        previous_version TEXT,
        target_version TEXT,
        previous_state_json TEXT NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('started','pointer_committed','health_committed','cleanup_committed','rolled_back','recovery_required')),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    "CREATE UNIQUE INDEX activation_journal_pending ON activation_journal(managed_mcp_id) WHERE status IN ('started','pointer_committed')",
    "CREATE INDEX installation_ownership_owner ON installation_ownership(owner_task_id)",
    "CREATE INDEX artifact_cache_reference ON artifact_cache(reference_count, verified_at_ms)",
    r#"CREATE TABLE uninstall_journal (
        task_id TEXT PRIMARY KEY REFERENCES tasks(task_id) ON DELETE RESTRICT,
        managed_mcp_id TEXT NOT NULL,
        version TEXT NOT NULL,
        previous_state_json TEXT NOT NULL,
        artifact_digest TEXT,
        status TEXT NOT NULL CHECK(status IN ('started','quarantined','committed','cancelled','recovery_required')),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE lifecycle_task_targets (
        task_id TEXT PRIMARY KEY REFERENCES tasks(task_id) ON DELETE RESTRICT,
        managed_mcp_id TEXT NOT NULL
    )"#,
    "CREATE INDEX lifecycle_task_targets_managed ON lifecycle_task_targets(managed_mcp_id, task_id)",
    r#"CREATE TABLE managed_lifecycle_leases (
        managed_mcp_id TEXT PRIMARY KEY,
        task_id TEXT NOT NULL UNIQUE REFERENCES tasks(task_id) ON DELETE RESTRICT,
        operation TEXT NOT NULL CHECK(operation IN ('install','update','repair','uninstall')),
        acquired_at_ms INTEGER NOT NULL
    )"#,
];

pub async fn apply_v5(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    sqlx::query("ALTER TABLE managed_versions ADD COLUMN supply_chain_evidence_json TEXT")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    Ok(())
}

pub async fn apply_v6(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V6_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v7(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    let rows = sqlx::query(
        "SELECT managed_mcp_id, projection_json, projection_digest FROM connection_projections",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    for row in rows {
        let projection_digest: String = row.try_get("projection_digest").map_err(map_sqlx)?;
        if !projection_digest.is_empty() {
            continue;
        }
        let projection_json: String = row.try_get("projection_json").map_err(map_sqlx)?;
        let Ok(projection) = crate::mcp_platform::decode_projection_config(&projection_json) else {
            continue;
        };
        let canonical_json = crate::mcp_platform::encode_projection_config(&projection)?;
        let canonical_digest = crate::mcp_platform::projection_config_digest(&projection)?;
        sqlx::query(
            "UPDATE connection_projections SET projection_json = ?, projection_digest = ? WHERE managed_mcp_id = ? AND projection_digest = ''",
        )
        .bind(canonical_json)
        .bind(canonical_digest)
        .bind(row.try_get::<String, _>("managed_mcp_id").map_err(map_sqlx)?)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v8(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V8_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }

    Ok(())
}

pub async fn apply_v9(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V9_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v10(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V10_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v11(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V11_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v12(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V12_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v13(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V13_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v14(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V14_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v15(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    let shape = classify_projection_mutations_schema(tx).await?;
    sqlx::query("DROP INDEX IF EXISTS projection_mutations_active")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    sqlx::query("ALTER TABLE projection_mutations RENAME TO projection_mutations_v14")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    sqlx::query(PROJECTION_MUTATIONS_WITNESS_V2_CREATE_TABLE_SQL)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    match shape {
        ProjectionMutationsTableShape::LegacyV14 => {
            sqlx::query(
                r#"INSERT INTO projection_mutations(
                    mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                    writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                    writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_witness_version,
                    writer_provider_id,writer_binding_domain,writer_observed_state_digest,writer_commitment,
                    created_at_ms,updated_at_ms
                )
                SELECT mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                       writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                       writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,NULL,NULL,NULL,NULL,
                       writer_commitment,created_at_ms,updated_at_ms
                FROM projection_mutations_v14"#,
            )
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        }
        ProjectionMutationsTableShape::WitnessV2 => {
            sqlx::query(
                r#"INSERT INTO projection_mutations(
                    mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                    writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                    writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_witness_version,
                    writer_provider_id,writer_binding_domain,writer_observed_state_digest,writer_commitment,
                    created_at_ms,updated_at_ms
                )
                SELECT mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
                       writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
                       writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_witness_version,
                       writer_provider_id,writer_binding_domain,writer_observed_state_digest,writer_commitment,
                       created_at_ms,updated_at_ms
                FROM projection_mutations_v14"#,
            )
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        }
    }
    sqlx::query("DROP TABLE projection_mutations_v14")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    sqlx::query(PROJECTION_MUTATIONS_ACTIVE_INDEX_SQL)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    Ok(())
}

pub async fn apply_v16(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V16_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v17(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V17_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v18(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V18_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v19(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V19_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v20(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V20_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v21(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V21_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    ensure_source_schema_metadata_tx(tx).await.map_err(|_| {
        McpPlatformError::new(
            McpPlatformErrorCode::IntegrityError,
            "source schema compatibility fence materialization failed",
        )
    })?;
    Ok(())
}

pub async fn apply_v22(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V22_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v23(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V23_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v24(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V24_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v25(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V25_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v26(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    materialize_remote_inspection_v26_schema(tx).await
}

pub async fn apply_v27(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V27_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v28(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V28_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v29(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V29_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v30(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V30_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v31(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V31_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v32(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V32_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v33(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V33_PREPARE_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    for statement in &V33_STATEMENTS[..2] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }

    let legacy_rows = sqlx::query(
        "SELECT impact.impact_digest,impact.plan_id,impact.plan_digest,impact.managed_mcp_id,impact.actor_id,impact.operation,impact.expires_at_ms,impact.managed_revision,impact.manifest_evidence_digest,impact.projection_evidence_digest,impact.state,impact.confirmed_at_ms,impact.consumed_at_ms,impact.created_at_ms,impact.updated_at_ms,plan.expires_at_ms AS parent_expires_at_ms FROM mcp_profile_lifecycle_impacts_v32_legacy impact LEFT JOIN mcp_profile_apply_plans plan ON plan.plan_id=impact.plan_id",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;

    for row in legacy_rows {
        let actor_id: String = row.try_get("actor_id").map_err(map_sqlx)?;
        let impact_digest: String = row.try_get("impact_digest").map_err(map_sqlx)?;
        let sanitized_actor_id = profile_lifecycle_actor_digest(&actor_id);
        let parent_expires_at_ms = row
            .try_get::<Option<i64>, _>("parent_expires_at_ms")
            .map_err(map_sqlx)?;
        let expires_at_ms = row.try_get::<i64, _>("expires_at_ms").map_err(map_sqlx)?;
        let status = if parent_expires_at_ms.is_some_and(|parent| expires_at_ms <= parent) {
            "sanitized"
        } else {
            "discarded_parent_expiry"
        };
        sqlx::query(
            "INSERT INTO mcp_profile_lifecycle_impact_migration_audits(impact_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES (?,?,?,?,0)",
        )
        .bind(&impact_digest)
        .bind(profile_lifecycle_legacy_actor_digest(&actor_id))
        .bind(&sanitized_actor_id)
        .bind(status)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
        if status == "sanitized" {
            sqlx::query(
                "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            )
            .bind(impact_digest)
            .bind(row.try_get::<String, _>("plan_id").map_err(map_sqlx)?)
            .bind(row.try_get::<String, _>("plan_digest").map_err(map_sqlx)?)
            .bind(row.try_get::<String, _>("managed_mcp_id").map_err(map_sqlx)?)
            .bind(sanitized_actor_id)
            .bind(row.try_get::<String, _>("operation").map_err(map_sqlx)?)
            .bind(expires_at_ms)
            .bind(row.try_get::<i64, _>("managed_revision").map_err(map_sqlx)?)
            .bind(row.try_get::<String, _>("manifest_evidence_digest").map_err(map_sqlx)?)
            .bind(row.try_get::<String, _>("projection_evidence_digest").map_err(map_sqlx)?)
            .bind(row.try_get::<String, _>("state").map_err(map_sqlx)?)
            .bind(row.try_get::<Option<i64>, _>("confirmed_at_ms").map_err(map_sqlx)?)
            .bind(row.try_get::<Option<i64>, _>("consumed_at_ms").map_err(map_sqlx)?)
            .bind(row.try_get::<i64, _>("created_at_ms").map_err(map_sqlx)?)
            .bind(row.try_get::<i64, _>("updated_at_ms").map_err(map_sqlx)?)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        }
    }

    sqlx::query("DROP TABLE mcp_profile_lifecycle_impacts_v32_legacy")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    for statement in &V33_STATEMENTS[2..] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v34(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V34_PREPARE_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    for statement in &V34_STATEMENTS[..3] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }

    let valid_v33_row = "typeof(impact.impact_digest)='text' AND length(impact.impact_digest)=64 AND impact.impact_digest NOT GLOB '*[^0-9a-f]*' AND typeof(impact.plan_digest)='text' AND length(impact.plan_digest)=64 AND impact.plan_digest NOT GLOB '*[^0-9a-f]*' AND typeof(impact.actor_id)='text' AND length(impact.actor_id)=64 AND impact.actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(impact.manifest_evidence_digest)='text' AND length(impact.manifest_evidence_digest)=64 AND impact.manifest_evidence_digest NOT GLOB '*[^0-9a-f]*' AND typeof(impact.projection_evidence_digest)='text' AND length(impact.projection_evidence_digest)=64 AND impact.projection_evidence_digest NOT GLOB '*[^0-9a-f]*' AND impact.operation='archive' AND impact.managed_revision>=0 AND impact.expires_at_ms>=impact.created_at_ms AND impact.updated_at_ms>=impact.created_at_ms AND ((impact.state='planned' AND impact.confirmed_at_ms IS NULL AND impact.consumed_at_ms IS NULL) OR (impact.state='confirmed' AND impact.confirmed_at_ms>=impact.created_at_ms AND impact.confirmed_at_ms<=impact.expires_at_ms AND impact.consumed_at_ms IS NULL AND impact.updated_at_ms>=impact.confirmed_at_ms) OR (impact.state='consumed' AND impact.confirmed_at_ms>=impact.created_at_ms AND impact.confirmed_at_ms<=impact.expires_at_ms AND impact.consumed_at_ms>=impact.confirmed_at_ms AND impact.consumed_at_ms<=impact.expires_at_ms AND impact.updated_at_ms>=impact.consumed_at_ms)) AND plan.plan_digest=impact.plan_digest AND plan.expires_at_ms>=impact.expires_at_ms AND EXISTS (SELECT 1 FROM managed_mcps WHERE managed_mcp_id=impact.managed_mcp_id)";
    let copy_valid_rows = format!(
        "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) SELECT impact.impact_digest,impact.plan_id,impact.plan_digest,impact.managed_mcp_id,impact.actor_id,impact.operation,impact.expires_at_ms,impact.managed_revision,impact.manifest_evidence_digest,impact.projection_evidence_digest,impact.state,impact.confirmed_at_ms,impact.consumed_at_ms,impact.created_at_ms,impact.updated_at_ms FROM mcp_profile_lifecycle_impacts_v33_legacy impact JOIN mcp_profile_apply_plans plan ON plan.plan_id=impact.plan_id WHERE {valid_v33_row}"
    );
    sqlx::query(&copy_valid_rows)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;

    let invalid_rows = format!(
        "SELECT CAST(impact_digest AS BLOB) AS impact_bytes,CAST(actor_id AS BLOB) AS actor_bytes FROM mcp_profile_lifecycle_impacts_v33_legacy impact LEFT JOIN mcp_profile_apply_plans plan ON plan.plan_id=impact.plan_id WHERE NOT ({valid_v33_row}) OR plan.plan_id IS NULL"
    );
    for row in sqlx::query(&invalid_rows)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?
    {
        let impact_bytes = row
            .try_get::<Vec<u8>, _>("impact_bytes")
            .map_err(map_sqlx)?;
        let actor_bytes = row.try_get::<Vec<u8>, _>("actor_bytes").map_err(map_sqlx)?;
        sqlx::query("INSERT OR IGNORE INTO mcp_profile_lifecycle_impact_v34_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES (?,?,?,?,0)")
            .bind(profile_lifecycle_v34_migration_digest(b"impact", &impact_bytes))
            .bind(profile_lifecycle_v34_migration_digest(b"legacy-actor", &actor_bytes))
            .bind(profile_lifecycle_v34_migration_digest(b"sanitized-actor", &actor_bytes))
            .bind("discarded_integrity")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }

    sqlx::query("INSERT INTO mcp_profile_lifecycle_impact_migration_audits(impact_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) SELECT impact_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms FROM mcp_profile_lifecycle_impact_migration_audits_v33_legacy WHERE typeof(impact_digest)='text' AND length(impact_digest)=64 AND impact_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*'")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    for statement in [
        "DROP TABLE mcp_profile_lifecycle_impacts_v33_legacy",
        "DROP TABLE mcp_profile_lifecycle_impact_migration_audits_v33_legacy",
    ] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    for statement in &V34_STATEMENTS[3..] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v35(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V35_PREPARE_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    for statement in &V35_STATEMENTS[..2] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }

    let now_ms = "CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)";
    let valid_v34_row = format!(
        "typeof(impact.impact_digest)='text' AND length(impact.impact_digest)=64 AND impact.impact_digest NOT GLOB '*[^0-9a-f]*' AND typeof(impact.plan_digest)='text' AND length(impact.plan_digest)=64 AND impact.plan_digest NOT GLOB '*[^0-9a-f]*' AND typeof(impact.actor_id)='text' AND length(impact.actor_id)=64 AND impact.actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(impact.manifest_evidence_digest)='text' AND length(impact.manifest_evidence_digest)=64 AND impact.manifest_evidence_digest NOT GLOB '*[^0-9a-f]*' AND typeof(impact.projection_evidence_digest)='text' AND length(impact.projection_evidence_digest)=64 AND impact.projection_evidence_digest NOT GLOB '*[^0-9a-f]*' AND impact.operation='archive' AND impact.managed_revision>=0 AND impact.expires_at_ms>=impact.created_at_ms AND impact.updated_at_ms>=impact.created_at_ms AND ((impact.state='planned' AND impact.confirmed_at_ms IS NULL AND impact.consumed_at_ms IS NULL) OR (impact.state='confirmed' AND impact.confirmed_at_ms>=impact.created_at_ms AND impact.confirmed_at_ms<=impact.expires_at_ms AND impact.consumed_at_ms IS NULL AND impact.updated_at_ms>=impact.confirmed_at_ms) OR (impact.state='consumed' AND impact.confirmed_at_ms>=impact.created_at_ms AND impact.confirmed_at_ms<=impact.expires_at_ms AND impact.consumed_at_ms>=impact.confirmed_at_ms AND impact.consumed_at_ms<=impact.expires_at_ms AND impact.updated_at_ms>=impact.consumed_at_ms)) AND plan.plan_digest=impact.plan_digest AND plan.expires_at_ms>=impact.expires_at_ms AND impact.expires_at_ms>{now_ms} AND plan.expires_at_ms>{now_ms} AND EXISTS (SELECT 1 FROM managed_mcps WHERE managed_mcp_id=impact.managed_mcp_id)"
    );
    let copy_valid_rows = format!(
        "INSERT INTO mcp_profile_lifecycle_impacts(impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,state,confirmed_at_ms,consumed_at_ms,created_at_ms,updated_at_ms) SELECT impact.impact_digest,impact.plan_id,impact.plan_digest,impact.managed_mcp_id,impact.actor_id,impact.operation,impact.expires_at_ms,impact.managed_revision,impact.manifest_evidence_digest,impact.projection_evidence_digest,impact.state,impact.confirmed_at_ms,impact.consumed_at_ms,impact.created_at_ms,impact.updated_at_ms FROM mcp_profile_lifecycle_impacts_v34_legacy impact JOIN mcp_profile_apply_plans plan ON plan.plan_id=impact.plan_id WHERE {valid_v34_row}"
    );
    sqlx::query(&copy_valid_rows)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;

    let invalid_rows = format!(
        "SELECT typeof(impact.impact_digest) AS impact_storage_type,CAST(impact.impact_digest AS BLOB) AS impact_bytes,typeof(impact.actor_id) AS actor_storage_type,CAST(impact.actor_id AS BLOB) AS actor_bytes,CASE WHEN impact.expires_at_ms<={now_ms} OR plan.expires_at_ms<={now_ms} THEN 'discarded_expired' ELSE 'discarded_integrity' END AS status FROM mcp_profile_lifecycle_impacts_v34_legacy impact LEFT JOIN mcp_profile_apply_plans plan ON plan.plan_id=impact.plan_id WHERE ({valid_v34_row}) IS NOT TRUE"
    );
    for row in sqlx::query(&invalid_rows)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?
    {
        let impact_storage_type = row
            .try_get::<String, _>("impact_storage_type")
            .map_err(map_sqlx)?;
        let impact_bytes = row
            .try_get::<Option<Vec<u8>>, _>("impact_bytes")
            .map_err(map_sqlx)?;
        let actor_storage_type = row
            .try_get::<String, _>("actor_storage_type")
            .map_err(map_sqlx)?;
        let actor_bytes = row
            .try_get::<Option<Vec<u8>>, _>("actor_bytes")
            .map_err(map_sqlx)?;
        let status = row.try_get::<String, _>("status").map_err(map_sqlx)?;
        sqlx::query("INSERT OR IGNORE INTO mcp_profile_lifecycle_impact_v35_migration_audits(audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms) VALUES (?,?,?,?,0)")
            .bind(profile_lifecycle_v35_legacy_value_digest(
                b"impact",
                &impact_storage_type,
                impact_bytes.as_deref(),
            ))
            .bind(profile_lifecycle_v35_legacy_value_digest(
                b"legacy-actor",
                &actor_storage_type,
                actor_bytes.as_deref(),
            ))
            .bind(profile_lifecycle_v35_legacy_value_digest(
                b"sanitized-actor",
                &actor_storage_type,
                actor_bytes.as_deref(),
            ))
            .bind(status)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }

    sqlx::query("DROP TABLE mcp_profile_lifecycle_impacts_v34_legacy")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    for statement in &V35_STATEMENTS[2..] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v36(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V36_PREPARE_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    for statement in V36_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }

    copy_v36_valid_audit_rows(
        tx,
        "mcp_profile_lifecycle_impact_migration_audits",
        "mcp_profile_lifecycle_impact_migration_audits_v35_legacy",
        "impact_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms",
        "typeof(impact_digest)='text' AND length(impact_digest)=64 AND impact_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status IN ('sanitized','discarded_parent_expiry') AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
    )
    .await?;
    copy_v36_valid_audit_rows(
        tx,
        "mcp_profile_lifecycle_impact_v34_migration_audits",
        "mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy",
        "audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms",
        "typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status='discarded_integrity' AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
    )
    .await?;
    copy_v36_valid_audit_rows(
        tx,
        "mcp_profile_lifecycle_impact_v35_migration_audits",
        "mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy",
        "audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms",
        "typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status IN ('discarded_integrity','discarded_expired') AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
    )
    .await?;

    for (domain, table, predicate) in [
        (
            "profile_lifecycle_v33_audit",
            "mcp_profile_lifecycle_impact_migration_audits_v35_legacy",
            "typeof(impact_digest)='text' AND length(impact_digest)=64 AND impact_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status IN ('sanitized','discarded_parent_expiry') AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
        ),
        (
            "profile_lifecycle_v34_audit",
            "mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy",
            "typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status='discarded_integrity' AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
        ),
        (
            "profile_lifecycle_v35_audit",
            "mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy",
            "typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status IN ('discarded_integrity','discarded_expired') AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
        ),
    ] {
        record_v36_invalid_audit_dispositions(tx, domain, table, predicate).await?;
    }

    for statement in [
        "DROP TABLE mcp_profile_lifecycle_impact_migration_audits_v35_legacy",
        "DROP TABLE mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy",
        "DROP TABLE mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy",
    ] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

async fn copy_v36_valid_audit_rows(
    tx: &mut Transaction<'_, Sqlite>,
    destination: &str,
    source: &str,
    columns: &str,
    valid_row: &str,
) -> McpPlatformResult<()> {
    let statement = format!(
        "INSERT INTO {destination}({columns}) SELECT {columns} FROM {source} WHERE {valid_row}"
    );
    sqlx::query(&statement)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    Ok(())
}

async fn record_v36_invalid_audit_dispositions(
    tx: &mut Transaction<'_, Sqlite>,
    migration_domain: &str,
    source: &str,
    valid_row: &str,
) -> McpPlatformResult<()> {
    let statement = format!(
        "SELECT rowid, typeof(status) AS status_type, typeof(migrated_at_ms) AS migrated_at_type FROM {source} WHERE ({valid_row}) IS NOT TRUE"
    );
    for row in sqlx::query(&statement)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?
    {
        let source_rowid = row.try_get::<i64, _>("rowid").map_err(map_sqlx)?;
        let status_type = row.try_get::<String, _>("status_type").map_err(map_sqlx)?;
        let migrated_at_type = row
            .try_get::<String, _>("migrated_at_type")
            .map_err(map_sqlx)?;
        let legacy_row_type = format!("invalid_audit_status_{status_type}_time_{migrated_at_type}");
        sqlx::query("INSERT INTO mcp_profile_lifecycle_impact_v36_migration_dispositions(disposition_digest,migration_domain,source_rowid,legacy_row_type,status,migrated_at_ms) VALUES (?,?,?,?,?,0)")
            .bind(profile_lifecycle_v36_disposition_digest(
                migration_domain,
                source_rowid,
                &legacy_row_type,
            ))
            .bind(migration_domain)
            .bind(source_rowid)
            .bind(legacy_row_type)
            .bind("discarded_legacy_audit")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

fn profile_lifecycle_actor_digest(actor_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"goose.mcp-profile-lifecycle-impact-actor-v33\0");
    digest.update(actor_id.as_bytes());
    crate::utils::bytes_to_hex(digest.finalize())
}

fn profile_lifecycle_legacy_actor_digest(actor_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"goose.mcp-profile-lifecycle-impact-legacy-actor-v33\0");
    digest.update(actor_id.as_bytes());
    crate::utils::bytes_to_hex(digest.finalize())
}

fn profile_lifecycle_v34_migration_digest(domain: &[u8], value: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"goose.mcp-profile-lifecycle-impact-v34-migration\0");
    digest.update(domain);
    digest.update([0]);
    digest.update(value);
    crate::utils::bytes_to_hex(digest.finalize())
}

fn profile_lifecycle_v35_legacy_value_digest(
    domain: &[u8],
    storage_type: &str,
    value: Option<&[u8]>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"goose.mcp-profile-lifecycle-impact-v35-legacy-value\0");
    digest.update(domain);
    digest.update([0]);
    digest.update(storage_type.as_bytes());
    digest.update([0]);
    match value {
        Some(value) => {
            digest.update(b"value\0");
            digest.update(value);
        }
        None => digest.update(b"null\0"),
    }
    crate::utils::bytes_to_hex(digest.finalize())
}

fn profile_lifecycle_v36_disposition_digest(
    migration_domain: &str,
    source_rowid: i64,
    legacy_row_type: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"goose.mcp-profile-lifecycle-impact-v36-disposition\0");
    digest.update(migration_domain.as_bytes());
    digest.update([0]);
    digest.update(source_rowid.to_le_bytes());
    digest.update([0]);
    digest.update(legacy_row_type.as_bytes());
    crate::utils::bytes_to_hex(digest.finalize())
}

pub(crate) fn profile_lifecycle_v37_disposition_digest(
    migration_domain: &str,
    source_table: &str,
    source_rowid: i64,
    legacy_row_type: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"goose.mcp-profile-lifecycle-impact-v37-disposition\0");
    digest.update(migration_domain.as_bytes());
    digest.update([0]);
    digest.update(source_table.as_bytes());
    digest.update([0]);
    digest.update(source_rowid.to_le_bytes());
    digest.update([0]);
    digest.update(legacy_row_type.as_bytes());
    crate::utils::bytes_to_hex(digest.finalize())
}

fn profile_lifecycle_v37_source_table(migration_domain: &str) -> McpPlatformResult<&'static str> {
    match migration_domain {
        "profile_lifecycle_v33_audit" => {
            Ok("mcp_profile_lifecycle_impact_migration_audits_v35_legacy")
        }
        "profile_lifecycle_v34_audit" => {
            Ok("mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy")
        }
        "profile_lifecycle_v35_audit" => {
            Ok("mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy")
        }
        _ => Err(integrity_error()),
    }
}

async fn record_v37_invalid_audit_dispositions(
    tx: &mut Transaction<'_, Sqlite>,
    migration_domain: &str,
    source: &str,
    valid_row: &str,
) -> McpPlatformResult<()> {
    let source_table = profile_lifecycle_v37_source_table(migration_domain)?;
    let statement = format!(
        "SELECT rowid, typeof(status) AS status_type, typeof(migrated_at_ms) AS migrated_at_type FROM {source} WHERE ({valid_row}) IS NOT TRUE"
    );
    for row in sqlx::query(&statement)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?
    {
        let source_rowid = row.try_get::<i64, _>("rowid").map_err(map_sqlx)?;
        let status_type = row.try_get::<String, _>("status_type").map_err(map_sqlx)?;
        let migrated_at_type = row
            .try_get::<String, _>("migrated_at_type")
            .map_err(map_sqlx)?;
        let legacy_row_type = format!("invalid_audit_status_{status_type}_time_{migrated_at_type}");
        sqlx::query("INSERT INTO mcp_profile_lifecycle_impact_v36_migration_dispositions(disposition_digest,migration_domain,source_table,source_rowid,legacy_row_type,status,migrated_at_ms) VALUES (?,?,?,?,?,?,0)")
            .bind(profile_lifecycle_v37_disposition_digest(
                migration_domain,
                source_table,
                source_rowid,
                &legacy_row_type,
            ))
            .bind(migration_domain)
            .bind(source_table)
            .bind(source_rowid)
            .bind(legacy_row_type)
            .bind("discarded_legacy_audit")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v37_compatible_v36(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    for statement in V36_PREPARE_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    for statement in &V36_STATEMENTS[..3] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    sqlx::query(V37_DISPOSITION_STATEMENT)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;

    copy_v36_valid_audit_rows(
        tx,
        "mcp_profile_lifecycle_impact_migration_audits",
        "mcp_profile_lifecycle_impact_migration_audits_v35_legacy",
        "impact_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms",
        "typeof(impact_digest)='text' AND length(impact_digest)=64 AND impact_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status IN ('sanitized','discarded_parent_expiry') AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
    )
    .await?;
    copy_v36_valid_audit_rows(
        tx,
        "mcp_profile_lifecycle_impact_v34_migration_audits",
        "mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy",
        "audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms",
        "typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status='discarded_integrity' AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
    )
    .await?;
    copy_v36_valid_audit_rows(
        tx,
        "mcp_profile_lifecycle_impact_v35_migration_audits",
        "mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy",
        "audit_digest,legacy_actor_digest,sanitized_actor_id,status,migrated_at_ms",
        "typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status IN ('discarded_integrity','discarded_expired') AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
    )
    .await?;

    for (domain, table, predicate) in [
        (
            "profile_lifecycle_v33_audit",
            "mcp_profile_lifecycle_impact_migration_audits_v35_legacy",
            "typeof(impact_digest)='text' AND length(impact_digest)=64 AND impact_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status IN ('sanitized','discarded_parent_expiry') AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
        ),
        (
            "profile_lifecycle_v34_audit",
            "mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy",
            "typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status='discarded_integrity' AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
        ),
        (
            "profile_lifecycle_v35_audit",
            "mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy",
            "typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*' AND typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*' AND typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*' AND typeof(status)='text' AND status IN ('discarded_integrity','discarded_expired') AND typeof(migrated_at_ms)='integer' AND migrated_at_ms>=0",
        ),
    ] {
        record_v37_invalid_audit_dispositions(tx, domain, table, predicate).await?;
    }
    for statement in [
        "DROP TABLE mcp_profile_lifecycle_impact_migration_audits_v35_legacy",
        "DROP TABLE mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy",
        "DROP TABLE mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy",
    ] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v37(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    let disposition_sql = schema_object_sql(
        tx,
        "table",
        "mcp_profile_lifecycle_impact_v36_migration_dispositions",
    )
    .await?
    .ok_or_else(integrity_error)?;
    if normalize_schema_sql(&disposition_sql) == normalize_schema_sql(V36_STATEMENTS[3]) {
        sqlx::query("ALTER TABLE mcp_profile_lifecycle_impact_v36_migration_dispositions RENAME TO mcp_profile_lifecycle_impact_v36_migration_dispositions_v36_legacy")
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query(V37_DISPOSITION_STATEMENT)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        let invalid_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v36_migration_dispositions_v36_legacy WHERE typeof(disposition_digest)!='text' OR length(disposition_digest)!=64 OR disposition_digest GLOB '*[^0-9a-f]*' OR typeof(migration_domain)!='text' OR migration_domain NOT IN ('profile_lifecycle_v33_audit','profile_lifecycle_v34_audit','profile_lifecycle_v35_audit') OR typeof(source_rowid)!='integer' OR typeof(legacy_row_type)!='text' OR legacy_row_type NOT GLOB 'invalid_audit_status_*_time_*' OR typeof(status)!='text' OR status!='discarded_legacy_audit' OR typeof(migrated_at_ms)!='integer' OR migrated_at_ms<0")
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        if invalid_count != 0 {
            return Err(integrity_error());
        }
        for row in sqlx::query("SELECT migration_domain,source_rowid,legacy_row_type,status,migrated_at_ms FROM mcp_profile_lifecycle_impact_v36_migration_dispositions_v36_legacy")
            .fetch_all(&mut **tx)
            .await
            .map_err(map_sqlx)?
        {
            let migration_domain = row.try_get::<String, _>("migration_domain").map_err(map_sqlx)?;
            let source_table = profile_lifecycle_v37_source_table(&migration_domain)?;
            let source_rowid = row.try_get::<i64, _>("source_rowid").map_err(map_sqlx)?;
            let legacy_row_type = row.try_get::<String, _>("legacy_row_type").map_err(map_sqlx)?;
            let status = row.try_get::<String, _>("status").map_err(map_sqlx)?;
            let migrated_at_ms = row.try_get::<i64, _>("migrated_at_ms").map_err(map_sqlx)?;
            sqlx::query("INSERT INTO mcp_profile_lifecycle_impact_v36_migration_dispositions(disposition_digest,migration_domain,source_table,source_rowid,legacy_row_type,status,migrated_at_ms) VALUES (?,?,?,?,?,?,?)")
                .bind(profile_lifecycle_v37_disposition_digest(&migration_domain, source_table, source_rowid, &legacy_row_type))
                .bind(migration_domain)
                .bind(source_table)
                .bind(source_rowid)
                .bind(legacy_row_type)
                .bind(status)
                .bind(migrated_at_ms)
                .execute(&mut **tx)
                .await
                .map_err(map_sqlx)?;
        }
        sqlx::query(
            "DROP TABLE mcp_profile_lifecycle_impact_v36_migration_dispositions_v36_legacy",
        )
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    } else if normalize_schema_sql(&disposition_sql)
        != normalize_schema_sql(V37_DISPOSITION_STATEMENT)
    {
        return Err(integrity_error());
    }

    for statement in V37_ATTESTATION_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    let observed_record_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v35_migration_audits WHERE status='discarded_integrity'")
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    if observed_record_count > 0 {
        sqlx::query("INSERT INTO mcp_profile_lifecycle_audit_integrity_attestations(attestation_id,migration_domain,version_from,version_through,status,observed_record_count,created_at_ms) VALUES ('profile_lifecycle_v35_invalid_audit','profile_lifecycle_v35_invalid_audit',35,35,'unverifiable_legacy_aggregation',?,0)")
            .bind(observed_record_count)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v38(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    validate_schema_contracts(tx, V37_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS).await?;

    sqlx::query("ALTER TABLE mcp_profile_lifecycle_impact_v36_migration_dispositions RENAME TO mcp_profile_lifecycle_impact_v36_migration_dispositions_v37_legacy")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    sqlx::query(V38_DISPOSITION_STATEMENTS[0])
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    sqlx::query(V38_DISPOSITION_STATEMENTS[1])
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;

    for row in sqlx::query("SELECT disposition_digest,migration_domain,source_table,source_rowid,legacy_row_type,status,migrated_at_ms FROM mcp_profile_lifecycle_impact_v36_migration_dispositions_v37_legacy")
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?
    {
        let disposition_digest = row.try_get::<String, _>("disposition_digest").map_err(map_sqlx)?;
        let migration_domain = row.try_get::<String, _>("migration_domain").map_err(map_sqlx)?;
        let source_table = row.try_get::<String, _>("source_table").map_err(map_sqlx)?;
        let source_rowid = row.try_get::<i64, _>("source_rowid").map_err(map_sqlx)?;
        let legacy_row_type = row.try_get::<String, _>("legacy_row_type").map_err(map_sqlx)?;
        let status = row.try_get::<String, _>("status").map_err(map_sqlx)?;
        let migrated_at_ms = row.try_get::<i64, _>("migrated_at_ms").map_err(map_sqlx)?;
        if source_rowid < 1
            || migrated_at_ms < 0
            || status != "discarded_legacy_audit"
            || disposition_digest
                != profile_lifecycle_v37_disposition_digest(
                    &migration_domain,
                    &source_table,
                    source_rowid,
                    &legacy_row_type,
                )
        {
            return Err(integrity_error());
        }
        sqlx::query("INSERT INTO mcp_profile_lifecycle_impact_v36_migration_dispositions(disposition_digest,migration_domain,source_table,source_rowid,legacy_row_type,status,migrated_at_ms) VALUES (?,?,?,?,?,?,?)")
            .bind(&disposition_digest)
            .bind(&migration_domain)
            .bind(&source_table)
            .bind(source_rowid)
            .bind(&legacy_row_type)
            .bind(&status)
            .bind(migrated_at_ms)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
        sqlx::query("INSERT INTO mcp_profile_lifecycle_impact_v38_disposition_canonicals(disposition_digest,migration_domain,source_table,source_rowid,legacy_row_type,status,migrated_at_ms) VALUES (?,?,?,?,?,?,?)")
            .bind(disposition_digest)
            .bind(migration_domain)
            .bind(source_table)
            .bind(source_rowid)
            .bind(legacy_row_type)
            .bind(status)
            .bind(migrated_at_ms)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    sqlx::query("DROP TABLE mcp_profile_lifecycle_impact_v36_migration_dispositions_v37_legacy")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;

    for trigger in [
        "mcp_profile_lifecycle_audit_integrity_attestation_is_immutable",
        "mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_deleted",
        "mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_replaced",
    ] {
        sqlx::query(&format!("DROP TRIGGER {trigger}"))
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    sqlx::query("ALTER TABLE mcp_profile_lifecycle_audit_integrity_attestations RENAME TO mcp_profile_lifecycle_audit_integrity_attestations_v37_legacy")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    for statement in V38_ATTESTATION_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    let observed_record_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_profile_lifecycle_impact_v35_migration_audits WHERE status='discarded_integrity'")
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    for row in sqlx::query("SELECT attestation_id,migration_domain,version_from,version_through,status,observed_record_count,created_at_ms FROM mcp_profile_lifecycle_audit_integrity_attestations_v37_legacy")
        .fetch_all(&mut **tx)
        .await
        .map_err(map_sqlx)?
    {
        let attestation_id = row.try_get::<String, _>("attestation_id").map_err(map_sqlx)?;
        let migration_domain = row.try_get::<String, _>("migration_domain").map_err(map_sqlx)?;
        let version_from = row.try_get::<i64, _>("version_from").map_err(map_sqlx)?;
        let version_through = row.try_get::<i64, _>("version_through").map_err(map_sqlx)?;
        let status = row.try_get::<String, _>("status").map_err(map_sqlx)?;
        let count = row.try_get::<i64, _>("observed_record_count").map_err(map_sqlx)?;
        let created_at_ms = row.try_get::<i64, _>("created_at_ms").map_err(map_sqlx)?;
        if attestation_id != "profile_lifecycle_v35_invalid_audit"
            || migration_domain != attestation_id
            || version_from != 35
            || version_through != 35
            || status != "unverifiable_legacy_aggregation"
            || count < 1
            || count != observed_record_count
            || created_at_ms < 0
        {
            return Err(integrity_error());
        }
        sqlx::query("INSERT INTO mcp_profile_lifecycle_audit_integrity_attestations(attestation_id,migration_domain,version_from,version_through,status,observed_record_count,created_at_ms) VALUES (?,?,?,?,?,?,?)")
            .bind(&attestation_id).bind(&migration_domain).bind(version_from).bind(version_through).bind(&status).bind(count).bind(created_at_ms)
            .execute(&mut **tx).await.map_err(map_sqlx)?;
        sqlx::query("INSERT INTO mcp_profile_lifecycle_audit_integrity_attestation_canonicals(attestation_id,migration_domain,version_from,version_through,status,observed_record_count,created_at_ms) VALUES (?,?,?,?,?,?,?)")
            .bind(attestation_id).bind(migration_domain).bind(version_from).bind(version_through).bind(status).bind(count).bind(created_at_ms)
            .execute(&mut **tx).await.map_err(map_sqlx)?;
    }
    if (observed_record_count > 0)
        != (sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mcp_profile_lifecycle_audit_integrity_attestations",
        )
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx)?
            == 1)
    {
        return Err(integrity_error());
    }
    sqlx::query("DROP TABLE mcp_profile_lifecycle_audit_integrity_attestations_v37_legacy")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    for statement in V38_IMMUTABILITY_TRIGGERS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn apply_v39(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    validate_schema_contracts(tx, V38_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS).await?;
    for statement in V39_EFFECT_FENCE_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    sqlx::query(
        "INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) VALUES ('global',0,?,0)",
    )
    .bind(effect_fence_initial_digest())
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    validate_schema_contracts(tx, V39_EFFECT_FENCE_SCHEMA_CONTRACTS).await
}

pub async fn apply_v40(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    validate_effect_fence_state_v39(tx).await?;
    validate_v40_effect_rows(tx).await?;

    for trigger in [
        "effect_fences_are_append_only_update",
        "effect_fences_are_append_only_delete",
        "effect_fences_are_monotonic",
        "effect_grants_initial_state",
        "effect_grants_are_not_deleted",
        "effect_grants_transition_is_guarded",
        "effect_audit_is_immutable_update",
        "effect_audit_is_immutable_delete",
        "effect_audit_matches_grant",
    ] {
        sqlx::query(&format!("DROP TRIGGER {trigger}"))
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    sqlx::query("DROP INDEX effect_grants_status")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    sqlx::query("DROP INDEX effect_audit_effect_id")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    for table in ["effect_audit", "effect_grants", "effect_fences"] {
        sqlx::query(&format!("ALTER TABLE {table} RENAME TO {table}_v39_legacy"))
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    for statement in &V40_EFFECT_FENCE_STATEMENTS[..5] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    sqlx::query("INSERT INTO effect_fences(scope,epoch,canonical_digest,created_at_ms) SELECT scope,epoch,canonical_digest,created_at_ms FROM effect_fences_v39_legacy ORDER BY epoch")
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    sqlx::query("INSERT INTO effect_grants(grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms,abandon_reason) SELECT grant_id,effect_id,fence_scope,fence_epoch,target_digest,canonical_digest,nonce_hash,status,state_epoch,issued_at_ms,transitioned_at_ms,CASE WHEN status='abandoned' THEN ? ELSE NULL END FROM effect_grants_v39_legacy")
        .bind(V39_LEGACY_ABANDON_REASON)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    sqlx::query("INSERT INTO effect_audit(audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,abandon_reason) SELECT audit_id,grant_id,effect_id,state_epoch,event_kind,target_digest,canonical_digest,occurred_at_ms,CASE WHEN event_kind='abandoned' THEN ? ELSE NULL END FROM effect_audit_v39_legacy")
        .bind(V39_LEGACY_ABANDON_REASON)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx)?;
    for statement in &V40_EFFECT_FENCE_STATEMENTS[5..] {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    for table in [
        "effect_audit_v39_legacy",
        "effect_grants_v39_legacy",
        "effect_fences_v39_legacy",
    ] {
        sqlx::query(&format!("DROP TABLE {table}"))
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    validate_effect_fence_state(tx).await
}

/// V41 reserved the fenced lifecycle version without changing V40's durable
/// shape. Keeping the explicit step makes V42 upgrades auditable.
pub async fn apply_v41(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    validate_effect_fence_state(tx).await
}

pub async fn apply_v42(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    validate_effect_fence_state(tx).await?;
    let active_legacy_grants = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_grants WHERE status IN ('issued','consuming')",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if active_legacy_grants != 0 {
        return Err(integrity_error());
    }
    for statement in V42_FENCED_RECEIPT_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    sqlx::query(
        "INSERT INTO effect_fenced_legacy_terminal_grants(grant_id,effect_id,terminal_status,state_epoch,fence_epoch,target_digest,canonical_digest,transitioned_at_ms) SELECT grant_id,effect_id,status,state_epoch,fence_epoch,target_digest,canonical_digest,transitioned_at_ms FROM effect_grants WHERE status IN ('applied','unknown','abandoned')",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    validate_fenced_effect_receipt_state(tx).await
}

/// V43 binds every newly issued fenced command and receipt to the concrete
/// authority key. V42 active or unknown grants cannot be assigned a key safely,
/// so the migration refuses them before changing schema. Verified V42 terminal
/// bindings and receipts retain the explicit format-zero marker; they are
/// historical evidence only and are never reusable as V43 commands.
pub async fn apply_v43(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    validate_fenced_effect_receipt_state(tx).await?;
    let active = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_grants WHERE status IN ('issued','consuming','unknown')",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if active != 0 {
        return Err(integrity_error());
    }
    for statement in V43_FENCED_AUTHORITY_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    validate_fenced_effect_authority_v43_state(tx).await
}

pub(crate) async fn validate_fenced_effect_authority_v43_state(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    for contract in &V42_FENCED_RECEIPT_SCHEMA_CONTRACTS[3..] {
        validate_schema_contracts(tx, std::slice::from_ref(contract)).await?;
    }
    validate_schema_contracts(tx, V43_FENCED_AUTHORITY_SCHEMA_CONTRACTS).await?;
    validate_v43_fenced_table_shape(
        tx,
        "effect_fenced_sink_bindings",
        V42_FENCED_RECEIPT_STATEMENTS[0],
        &V43_FENCED_AUTHORITY_STATEMENTS[..3],
    )
    .await?;
    validate_v43_fenced_table_shape(
        tx,
        "effect_fenced_receipts",
        V42_FENCED_RECEIPT_STATEMENTS[1],
        &V43_FENCED_AUTHORITY_STATEMENTS[3..6],
    )
    .await?;
    let malformed = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_fenced_sink_bindings b LEFT JOIN effect_grants g ON g.grant_id=b.grant_id WHERE g.grant_id IS NULL OR g.effect_id IS NOT b.effect_id OR g.fence_epoch IS NOT b.fence_epoch OR g.target_digest IS NOT b.target_digest OR g.canonical_digest IS NOT b.canonical_digest UNION ALL SELECT COUNT(*) FROM effect_grants g WHERE g.status IN ('issued','consuming') AND NOT EXISTS(SELECT 1 FROM effect_fenced_sink_bindings b WHERE b.grant_id=g.grant_id AND b.effect_id=g.effect_id AND b.fence_epoch=g.fence_epoch AND b.target_digest=g.target_digest AND b.canonical_digest=g.canonical_digest) UNION ALL SELECT COUNT(*) FROM effect_fenced_receipts r LEFT JOIN effect_fenced_sink_bindings b ON b.grant_id=r.grant_id LEFT JOIN effect_grants g ON g.grant_id=r.grant_id WHERE b.grant_id IS NULL OR g.grant_id IS NULL OR g.status!='applied' OR r.effect_id IS NOT b.effect_id OR r.sink_identity IS NOT b.sink_identity OR r.sink_version IS NOT b.sink_version OR r.sink_authority IS NOT b.sink_authority OR r.authority_binding_format IS NOT b.authority_binding_format OR r.authority_key_epoch IS NOT b.authority_key_epoch OR r.authority_key_fingerprint IS NOT b.authority_key_fingerprint OR r.command_binding IS NOT b.command_binding OR r.fence_epoch IS NOT b.fence_epoch OR r.target_digest IS NOT b.target_digest UNION ALL SELECT COUNT(*) FROM effect_grants g JOIN effect_fenced_sink_bindings b ON b.grant_id=g.grant_id WHERE g.status='applied' AND NOT EXISTS(SELECT 1 FROM effect_fenced_receipts r WHERE r.grant_id=g.grant_id) UNION ALL SELECT COUNT(*) FROM effect_grants g JOIN effect_fenced_receipts r ON r.grant_id=g.grant_id WHERE g.status IN ('unknown','abandoned') UNION ALL SELECT COUNT(*) FROM effect_grants g WHERE g.status IN ('applied','unknown','abandoned') AND NOT EXISTS(SELECT 1 FROM effect_fenced_sink_bindings b WHERE b.grant_id=g.grant_id) AND NOT EXISTS(SELECT 1 FROM effect_fenced_legacy_terminal_grants l WHERE l.grant_id=g.grant_id AND l.effect_id=g.effect_id AND l.terminal_status=g.status AND l.state_epoch=g.state_epoch AND l.fence_epoch=g.fence_epoch AND l.target_digest=g.target_digest AND l.canonical_digest=g.canonical_digest AND l.transitioned_at_ms=g.transitioned_at_ms) UNION ALL SELECT COUNT(*) FROM effect_fenced_legacy_terminal_grants l LEFT JOIN effect_grants g ON g.grant_id=l.grant_id LEFT JOIN effect_fenced_sink_bindings b ON b.grant_id=l.grant_id LEFT JOIN effect_fenced_receipts r ON r.grant_id=l.grant_id WHERE g.grant_id IS NULL OR b.grant_id IS NOT NULL OR r.grant_id IS NOT NULL OR g.effect_id IS NOT l.effect_id OR g.status IS NOT l.terminal_status OR g.state_epoch IS NOT l.state_epoch OR g.fence_epoch IS NOT l.fence_epoch OR g.target_digest IS NOT l.target_digest OR g.canonical_digest IS NOT l.canonical_digest OR g.transitioned_at_ms IS NOT l.transitioned_at_ms UNION ALL SELECT COUNT(*) FROM effect_fenced_sink_bindings b JOIN effect_grants g ON g.grant_id=b.grant_id WHERE (b.authority_binding_format=1 AND (b.authority_key_epoch<=0 OR length(b.authority_key_fingerprint)!=64 OR b.authority_key_fingerprint GLOB '*[^0-9a-f]*')) OR (b.authority_binding_format=0 AND (b.authority_key_epoch!=0 OR b.authority_key_fingerprint!='' OR g.status NOT IN ('applied','abandoned'))) UNION ALL SELECT COUNT(*) FROM effect_fenced_receipts r WHERE r.authority_binding_format NOT IN (0,1) OR (r.authority_binding_format=1 AND (r.authority_key_epoch<=0 OR length(r.authority_key_fingerprint)!=64 OR r.authority_key_fingerprint GLOB '*[^0-9a-f]*')) OR (r.authority_binding_format=0 AND (r.authority_key_epoch!=0 OR r.authority_key_fingerprint!=''))",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .into_iter()
    .any(|count| count != 0);
    if malformed {
        return Err(integrity_error());
    }
    let bindings = sqlx::query(
        "SELECT grant_id,effect_id,sink_identity,sink_version,sink_authority,authority_key_epoch,authority_key_fingerprint,command_binding,repository_instance_id,repository_path_binding,repository_key_epoch,fence_epoch,target_digest,canonical_digest FROM effect_fenced_sink_bindings WHERE authority_binding_format=1",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    for binding in bindings {
        let expected = fenced_command_binding(
            &binding
                .try_get::<String, _>("repository_instance_id")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("repository_path_binding")
                .map_err(map_sqlx)?,
            binding.try_get("repository_key_epoch").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("sink_identity")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("sink_version")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("sink_authority")
                .map_err(map_sqlx)?,
            binding.try_get("authority_key_epoch").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("authority_key_fingerprint")
                .map_err(map_sqlx)?,
            &binding.try_get::<String, _>("grant_id").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("effect_id")
                .map_err(map_sqlx)?,
            binding.try_get("fence_epoch").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("target_digest")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("canonical_digest")
                .map_err(map_sqlx)?,
        );
        if binding
            .try_get::<String, _>("command_binding")
            .map_err(map_sqlx)?
            != expected
        {
            return Err(integrity_error());
        }
    }
    let receipts = sqlx::query(
        "SELECT receipt_id,grant_id,effect_id,sink_identity,sink_version,sink_authority,authority_key_epoch,authority_key_fingerprint,command_binding,receipt_binding,fence_epoch,target_digest FROM effect_fenced_receipts WHERE authority_binding_format=1",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    for receipt in receipts {
        let epoch: i64 = receipt.try_get("authority_key_epoch").map_err(map_sqlx)?;
        let expected = fenced_receipt_binding(
            &receipt
                .try_get::<String, _>("receipt_id")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("sink_identity")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("sink_version")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("sink_authority")
                .map_err(map_sqlx)?,
            u64::try_from(epoch).map_err(|_| integrity_error())?,
            &receipt
                .try_get::<String, _>("authority_key_fingerprint")
                .map_err(map_sqlx)?,
            &receipt.try_get::<String, _>("grant_id").map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("effect_id")
                .map_err(map_sqlx)?,
            receipt.try_get("fence_epoch").map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("target_digest")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("command_binding")
                .map_err(map_sqlx)?,
        );
        if receipt
            .try_get::<String, _>("receipt_binding")
            .map_err(map_sqlx)?
            != expected
        {
            return Err(integrity_error());
        }
    }
    let legacy_bindings = sqlx::query(
        "SELECT grant_id,effect_id,sink_identity,sink_version,sink_authority,command_binding,repository_instance_id,repository_path_binding,repository_key_epoch,fence_epoch,target_digest,canonical_digest FROM effect_fenced_sink_bindings WHERE authority_binding_format=0",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    for binding in legacy_bindings {
        let expected = fenced_command_binding_v42(
            &binding
                .try_get::<String, _>("repository_instance_id")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("repository_path_binding")
                .map_err(map_sqlx)?,
            binding.try_get("repository_key_epoch").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("sink_identity")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("sink_version")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("sink_authority")
                .map_err(map_sqlx)?,
            &binding.try_get::<String, _>("grant_id").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("effect_id")
                .map_err(map_sqlx)?,
            binding.try_get("fence_epoch").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("target_digest")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("canonical_digest")
                .map_err(map_sqlx)?,
        );
        if binding
            .try_get::<String, _>("command_binding")
            .map_err(map_sqlx)?
            != expected
        {
            return Err(integrity_error());
        }
    }
    let legacy_receipts = sqlx::query(
        "SELECT receipt_id,grant_id,effect_id,sink_identity,sink_version,sink_authority,command_binding,receipt_binding,fence_epoch,target_digest FROM effect_fenced_receipts WHERE authority_binding_format=0",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    for receipt in legacy_receipts {
        let expected = fenced_receipt_binding_v42(
            &receipt
                .try_get::<String, _>("receipt_id")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("sink_identity")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("sink_version")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("sink_authority")
                .map_err(map_sqlx)?,
            &receipt.try_get::<String, _>("grant_id").map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("effect_id")
                .map_err(map_sqlx)?,
            receipt.try_get("fence_epoch").map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("target_digest")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("command_binding")
                .map_err(map_sqlx)?,
        );
        if receipt
            .try_get::<String, _>("receipt_binding")
            .map_err(map_sqlx)?
            != expected
        {
            return Err(integrity_error());
        }
    }
    Ok(())
}

async fn validate_v43_fenced_table_shape(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    v42_definition: &str,
    v43_add_column_statements: &[&str],
) -> McpPlatformResult<()> {
    let actual = schema_object_sql(tx, "table", table)
        .await?
        .ok_or_else(integrity_error)?;
    let expected_items =
        expected_v43_fenced_table_items(v42_definition, v43_add_column_statements)?;
    if normalize_table_items(&actual)? != expected_items {
        return Err(integrity_error());
    }
    Ok(())
}

fn expected_v43_fenced_table_items(
    v42_definition: &str,
    v43_add_column_statements: &[&str],
) -> McpPlatformResult<Vec<String>> {
    let mut expected_definition = v42_definition.to_string();
    let close = expected_definition
        .rfind(')')
        .ok_or_else(projection_mutations_schema_error)?;
    let additions = v43_add_column_statements
        .iter()
        .map(|statement| {
            statement
                .split_once(" ADD COLUMN ")
                .map(|(_, definition)| definition)
                .ok_or_else(projection_mutations_schema_error)
        })
        .collect::<McpPlatformResult<Vec<_>>>()?;
    expected_definition.insert_str(close, &format!(", {}", additions.join(", ")));
    let expected_items = normalize_table_items(&expected_definition)?;
    Ok(expected_items)
}

fn fenced_binding_v42_digest(fields: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    crate::utils::bytes_to_hex(hasher.finalize())
}

fn fenced_command_binding_v42(
    repository_instance_id: &str,
    repository_path_binding: &str,
    repository_key_epoch: i64,
    sink_identity: &str,
    sink_version: &str,
    sink_authority: &str,
    grant_id: &str,
    effect_id: &str,
    fence_epoch: i64,
    target_digest: &str,
    canonical_digest: &str,
) -> String {
    fenced_binding_v42_digest(&[
        b"goose.mcp-platform.fenced-projection-command-v42",
        repository_instance_id.as_bytes(),
        repository_path_binding.as_bytes(),
        repository_key_epoch.to_string().as_bytes(),
        sink_identity.as_bytes(),
        sink_version.as_bytes(),
        sink_authority.as_bytes(),
        b"global",
        grant_id.as_bytes(),
        effect_id.as_bytes(),
        fence_epoch.to_string().as_bytes(),
        target_digest.as_bytes(),
        canonical_digest.as_bytes(),
    ])
}

fn fenced_receipt_binding_v42(
    receipt_id: &str,
    sink_identity: &str,
    sink_version: &str,
    sink_authority: &str,
    grant_id: &str,
    effect_id: &str,
    fence_epoch: i64,
    target_digest: &str,
    command_binding: &str,
) -> String {
    fenced_binding_v42_digest(&[
        b"goose.mcp-platform.fenced-projection-receipt-v42",
        receipt_id.as_bytes(),
        sink_identity.as_bytes(),
        sink_version.as_bytes(),
        sink_authority.as_bytes(),
        grant_id.as_bytes(),
        effect_id.as_bytes(),
        fence_epoch.to_string().as_bytes(),
        target_digest.as_bytes(),
        command_binding.as_bytes(),
    ])
}

pub(crate) async fn validate_fenced_effect_receipt_state(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    validate_schema_contracts(tx, V42_FENCED_RECEIPT_SCHEMA_CONTRACTS).await?;
    let malformed = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_fenced_sink_bindings b LEFT JOIN effect_grants g ON g.grant_id=b.grant_id WHERE g.grant_id IS NULL OR g.effect_id IS NOT b.effect_id OR g.fence_epoch IS NOT b.fence_epoch OR g.target_digest IS NOT b.target_digest OR g.canonical_digest IS NOT b.canonical_digest UNION ALL SELECT COUNT(*) FROM effect_grants g WHERE g.status IN ('issued','consuming') AND NOT EXISTS(SELECT 1 FROM effect_fenced_sink_bindings b WHERE b.grant_id=g.grant_id AND b.effect_id=g.effect_id AND b.fence_epoch=g.fence_epoch AND b.target_digest=g.target_digest AND b.canonical_digest=g.canonical_digest) UNION ALL SELECT COUNT(*) FROM effect_fenced_receipts r LEFT JOIN effect_fenced_sink_bindings b ON b.grant_id=r.grant_id LEFT JOIN effect_grants g ON g.grant_id=r.grant_id WHERE b.grant_id IS NULL OR g.grant_id IS NULL OR g.status!='applied' OR r.effect_id IS NOT b.effect_id OR r.sink_identity IS NOT b.sink_identity OR r.sink_version IS NOT b.sink_version OR r.sink_authority IS NOT b.sink_authority OR r.command_binding IS NOT b.command_binding OR r.fence_epoch IS NOT b.fence_epoch OR r.target_digest IS NOT b.target_digest UNION ALL SELECT COUNT(*) FROM effect_grants g JOIN effect_fenced_sink_bindings b ON b.grant_id=g.grant_id WHERE g.status='applied' AND NOT EXISTS(SELECT 1 FROM effect_fenced_receipts r WHERE r.grant_id=g.grant_id) UNION ALL SELECT COUNT(*) FROM effect_grants g JOIN effect_fenced_receipts r ON r.grant_id=g.grant_id WHERE g.status IN ('unknown','abandoned') UNION ALL SELECT COUNT(*) FROM effect_grants g WHERE g.status IN ('applied','unknown','abandoned') AND NOT EXISTS(SELECT 1 FROM effect_fenced_sink_bindings b WHERE b.grant_id=g.grant_id) AND NOT EXISTS(SELECT 1 FROM effect_fenced_legacy_terminal_grants l WHERE l.grant_id=g.grant_id AND l.effect_id=g.effect_id AND l.terminal_status=g.status AND l.state_epoch=g.state_epoch AND l.fence_epoch=g.fence_epoch AND l.target_digest=g.target_digest AND l.canonical_digest=g.canonical_digest AND l.transitioned_at_ms=g.transitioned_at_ms) UNION ALL SELECT COUNT(*) FROM effect_fenced_legacy_terminal_grants l LEFT JOIN effect_grants g ON g.grant_id=l.grant_id LEFT JOIN effect_fenced_sink_bindings b ON b.grant_id=l.grant_id LEFT JOIN effect_fenced_receipts r ON r.grant_id=l.grant_id WHERE g.grant_id IS NULL OR b.grant_id IS NOT NULL OR r.grant_id IS NOT NULL OR g.effect_id IS NOT l.effect_id OR g.status IS NOT l.terminal_status OR g.state_epoch IS NOT l.state_epoch OR g.fence_epoch IS NOT l.fence_epoch OR g.target_digest IS NOT l.target_digest OR g.canonical_digest IS NOT l.canonical_digest OR g.transitioned_at_ms IS NOT l.transitioned_at_ms",
    ).fetch_all(&mut **tx).await.map_err(map_sqlx)?.into_iter().any(|count| count != 0);
    if malformed {
        return Err(integrity_error());
    }
    let bindings = sqlx::query(
        "SELECT grant_id,effect_id,sink_identity,sink_version,sink_authority,command_binding,repository_instance_id,repository_path_binding,repository_key_epoch,fence_epoch,target_digest,canonical_digest FROM effect_fenced_sink_bindings",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    for binding in bindings {
        let expected = fenced_command_binding_v42(
            &binding
                .try_get::<String, _>("repository_instance_id")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("repository_path_binding")
                .map_err(map_sqlx)?,
            binding.try_get("repository_key_epoch").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("sink_identity")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("sink_version")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("sink_authority")
                .map_err(map_sqlx)?,
            &binding.try_get::<String, _>("grant_id").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("effect_id")
                .map_err(map_sqlx)?,
            binding.try_get("fence_epoch").map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("target_digest")
                .map_err(map_sqlx)?,
            &binding
                .try_get::<String, _>("canonical_digest")
                .map_err(map_sqlx)?,
        );
        if binding
            .try_get::<String, _>("command_binding")
            .map_err(map_sqlx)?
            != expected
        {
            return Err(integrity_error());
        }
    }
    let receipts = sqlx::query(
        "SELECT receipt_id,grant_id,effect_id,sink_identity,sink_version,sink_authority,command_binding,receipt_binding,fence_epoch,target_digest FROM effect_fenced_receipts",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    for receipt in receipts {
        let expected = fenced_receipt_binding_v42(
            &receipt
                .try_get::<String, _>("receipt_id")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("sink_identity")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("sink_version")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("sink_authority")
                .map_err(map_sqlx)?,
            &receipt.try_get::<String, _>("grant_id").map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("effect_id")
                .map_err(map_sqlx)?,
            receipt.try_get("fence_epoch").map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("target_digest")
                .map_err(map_sqlx)?,
            &receipt
                .try_get::<String, _>("command_binding")
                .map_err(map_sqlx)?,
        );
        if receipt
            .try_get::<String, _>("receipt_binding")
            .map_err(map_sqlx)?
            != expected
        {
            return Err(integrity_error());
        }
    }
    Ok(())
}

pub(crate) fn effect_fence_initial_digest() -> String {
    crate::utils::bytes_to_hex(Sha256::digest(
        b"goose.mcp-platform.effect-fence-v39/global/0",
    ))
}

pub(crate) async fn validate_effect_fence_state(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    validate_schema_contracts(tx, V40_EFFECT_FENCE_SCHEMA_CONTRACTS).await?;
    let initial_matches = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_fences WHERE scope='global' AND epoch=0 AND canonical_digest=? AND created_at_ms=0",
    )
    .bind(effect_fence_initial_digest())
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    let fence_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM effect_fences WHERE scope='global'")
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    let maximum_epoch =
        sqlx::query_scalar::<_, i64>("SELECT MAX(epoch) FROM effect_fences WHERE scope='global'")
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    if initial_matches != 1 || fence_count != maximum_epoch + 1 {
        return Err(integrity_error());
    }
    let inconsistent_grants = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_grants g LEFT JOIN effect_fences f ON f.scope=g.fence_scope AND f.epoch=g.fence_epoch WHERE f.scope IS NULL OR f.canonical_digest IS NOT g.canonical_digest OR NOT EXISTS(SELECT 1 FROM effect_audit a WHERE a.grant_id=g.grant_id AND a.state_epoch=0 AND a.event_kind='issued' AND a.effect_id=g.effect_id AND a.target_digest=g.target_digest AND a.canonical_digest=g.canonical_digest AND a.occurred_at_ms=g.issued_at_ms AND a.abandon_reason IS NULL) OR (SELECT COUNT(*) FROM effect_audit a WHERE a.grant_id=g.grant_id) != g.state_epoch+1 OR NOT EXISTS(SELECT 1 FROM effect_audit a WHERE a.grant_id=g.grant_id AND a.state_epoch=g.state_epoch AND a.event_kind=g.status AND a.effect_id=g.effect_id AND a.target_digest=g.target_digest AND a.canonical_digest=g.canonical_digest AND a.occurred_at_ms=g.transitioned_at_ms AND a.abandon_reason IS g.abandon_reason) OR g.state_epoch>2 OR (g.state_epoch=0 AND g.status!='issued') OR (g.state_epoch=1 AND g.status NOT IN ('consuming','abandoned')) OR (g.state_epoch=2 AND g.status NOT IN ('applied','unknown','abandoned')) OR ((g.state_epoch=2 OR (g.state_epoch=1 AND g.status='consuming')) AND NOT EXISTS(SELECT 1 FROM effect_audit a WHERE a.grant_id=g.grant_id AND a.state_epoch=1 AND a.event_kind='consuming' AND a.effect_id=g.effect_id AND a.target_digest=g.target_digest AND a.canonical_digest=g.canonical_digest AND a.occurred_at_ms>=g.issued_at_ms AND a.occurred_at_ms<=g.transitioned_at_ms))",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if inconsistent_grants != 0 {
        return Err(integrity_error());
    }
    Ok(())
}

async fn validate_effect_fence_state_v39(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    validate_schema_contracts(tx, V39_EFFECT_FENCE_SCHEMA_CONTRACTS).await?;
    validate_effect_fence_rows(tx).await
}

async fn validate_effect_fence_rows(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    let initial_matches = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_fences WHERE scope='global' AND epoch=0 AND canonical_digest=? AND created_at_ms=0",
    )
    .bind(effect_fence_initial_digest())
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    let fence_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM effect_fences WHERE scope='global'")
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    let maximum_epoch =
        sqlx::query_scalar::<_, i64>("SELECT MAX(epoch) FROM effect_fences WHERE scope='global'")
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    if initial_matches != 1 || fence_count != maximum_epoch + 1 {
        return Err(integrity_error());
    }
    let inconsistent_grants = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_grants g LEFT JOIN effect_fences f ON f.scope=g.fence_scope AND f.epoch=g.fence_epoch WHERE f.scope IS NULL OR f.canonical_digest IS NOT g.canonical_digest OR NOT EXISTS(SELECT 1 FROM effect_audit a WHERE a.grant_id=g.grant_id AND a.state_epoch=0 AND a.event_kind='issued' AND a.effect_id=g.effect_id AND a.target_digest=g.target_digest AND a.canonical_digest=g.canonical_digest AND a.occurred_at_ms=g.issued_at_ms) OR (SELECT COUNT(*) FROM effect_audit a WHERE a.grant_id=g.grant_id) != g.state_epoch+1 OR NOT EXISTS(SELECT 1 FROM effect_audit a WHERE a.grant_id=g.grant_id AND a.state_epoch=g.state_epoch AND a.event_kind=g.status AND a.effect_id=g.effect_id AND a.target_digest=g.target_digest AND a.canonical_digest=g.canonical_digest AND a.occurred_at_ms=g.transitioned_at_ms) OR g.state_epoch>2 OR (g.state_epoch=0 AND g.status!='issued') OR (g.state_epoch=1 AND g.status!='consuming') OR (g.state_epoch=2 AND g.status NOT IN ('applied','unknown','abandoned')) OR (g.state_epoch>=1 AND NOT EXISTS(SELECT 1 FROM effect_audit a WHERE a.grant_id=g.grant_id AND a.state_epoch=1 AND a.event_kind='consuming' AND a.effect_id=g.effect_id AND a.target_digest=g.target_digest AND a.canonical_digest=g.canonical_digest AND a.occurred_at_ms>=g.issued_at_ms AND a.occurred_at_ms<=g.transitioned_at_ms))",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx)?;
    if inconsistent_grants != 0 {
        return Err(integrity_error());
    }
    Ok(())
}

/// V39 did not record abandon reasons; V40 uses this marker only to preserve that fact,
/// not as a claim about a user- or sink-supplied reason.
pub(crate) const V39_LEGACY_ABANDON_REASON: &str = "legacy_v39_no_reason";

async fn validate_v40_effect_rows(tx: &mut Transaction<'_, Sqlite>) -> McpPlatformResult<()> {
    let malformed = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM effect_grants WHERE NOT (length(grant_id)=36 AND substr(grant_id,9,1)='-' AND substr(grant_id,14,1)='-' AND substr(grant_id,19,1)='-' AND substr(grant_id,24,1)='-' AND replace(grant_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(grant_id,'-',''))=32 AND length(effect_id)=36 AND substr(effect_id,9,1)='-' AND substr(effect_id,14,1)='-' AND substr(effect_id,19,1)='-' AND substr(effect_id,24,1)='-' AND replace(effect_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(effect_id,'-',''))=32 AND typeof(nonce_hash)='blob' AND length(nonce_hash)=32) UNION ALL SELECT COUNT(*) FROM effect_audit WHERE NOT (length(audit_id)=36 AND substr(audit_id,9,1)='-' AND substr(audit_id,14,1)='-' AND substr(audit_id,19,1)='-' AND substr(audit_id,24,1)='-' AND replace(audit_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(audit_id,'-',''))=32 AND length(effect_id)=36 AND substr(effect_id,9,1)='-' AND substr(effect_id,14,1)='-' AND substr(effect_id,19,1)='-' AND substr(effect_id,24,1)='-' AND replace(effect_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(effect_id,'-',''))=32)",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx)?
    .into_iter()
    .any(|count| count != 0);
    if malformed {
        return Err(integrity_error());
    }
    Ok(())
}

async fn materialize_remote_inspection_v26_schema(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<()> {
    for statement in V26_STATEMENTS {
        sqlx::query(statement)
            .execute(&mut **tx)
            .await
            .map_err(map_sqlx)?;
    }
    Ok(())
}

const V10_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE integrity_metadata (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        instance_id TEXT NOT NULL,
        path_binding TEXT NOT NULL CHECK(length(path_binding) = 64),
        key_epoch INTEGER NOT NULL CHECK(key_epoch > 0),
        sequence INTEGER NOT NULL CHECK(sequence >= 0),
        root TEXT NOT NULL CHECK(length(root) = 64),
        state_digest TEXT NOT NULL CHECK(length(state_digest) = 64),
        commit_mac TEXT NOT NULL CHECK(length(commit_mac) = 64),
        status TEXT NOT NULL CHECK(status IN ('active','recovery_required'))
    )"#,
    r#"CREATE TABLE integrity_commits (
        sequence INTEGER PRIMARY KEY CHECK(sequence >= 0),
        instance_id TEXT NOT NULL,
        key_epoch INTEGER NOT NULL CHECK(key_epoch > 0),
        parent_root TEXT NOT NULL,
        state_digest TEXT NOT NULL CHECK(length(state_digest) = 64),
        root TEXT NOT NULL UNIQUE CHECK(length(root) = 64),
        commit_mac TEXT NOT NULL CHECK(length(commit_mac) = 64)
    )"#,
];

const V11_STATEMENTS: &[&str] = &[
    "ALTER TABLE projection_mutations RENAME TO projection_mutations_v10",
    r#"CREATE TABLE projection_mutations (
        mutation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        managed_mcp_id TEXT NOT NULL,
        expected_revision INTEGER NOT NULL,
        previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
        desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
        status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required')),
        writer_runtime_id TEXT,
        writer_sink_id TEXT,
        writer_anchor_instance_id TEXT,
        writer_anchor_path_binding TEXT CHECK(writer_anchor_path_binding IS NULL OR length(writer_anchor_path_binding) = 64),
        writer_anchor_key_epoch INTEGER CHECK(writer_anchor_key_epoch IS NULL OR writer_anchor_key_epoch > 0),
        writer_anchor_sequence INTEGER CHECK(writer_anchor_sequence IS NULL OR writer_anchor_sequence >= 0),
        writer_anchor_root TEXT CHECK(writer_anchor_root IS NULL OR length(writer_anchor_root) = 64),
        writer_commitment TEXT CHECK(writer_commitment IS NULL OR length(writer_commitment) = 64),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    r#"INSERT INTO projection_mutations(
        mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
        created_at_ms,updated_at_ms
    )
    SELECT mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
           created_at_ms,updated_at_ms
    FROM projection_mutations_v10"#,
    "DROP TABLE projection_mutations_v10",
    "CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id) WHERE status IN ('started','config_committed','recovery_required')",
];

const V12_STATEMENTS: &[&str] = &[
    "DROP INDEX projection_mutations_active",
    "CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id) WHERE status IN ('started','config_committed')",
];

const V13_STATEMENTS: &[&str] = &[
    "DROP INDEX projection_mutations_active",
    "ALTER TABLE projection_mutations RENAME TO projection_mutations_v12",
    PROJECTION_MUTATIONS_LEGACY_CREATE_TABLE_SQL,
    r#"INSERT INTO projection_mutations(
        mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
        writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
        writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_commitment,
        created_at_ms,updated_at_ms
    )
    SELECT mutation_id,managed_mcp_id,expected_revision,previous_enabled,desired_enabled,status,
           writer_runtime_id,writer_sink_id,writer_anchor_instance_id,writer_anchor_path_binding,
           writer_anchor_key_epoch,writer_anchor_sequence,writer_anchor_root,writer_commitment,
           created_at_ms,updated_at_ms
    FROM projection_mutations_v12"#,
    "DROP TABLE projection_mutations_v12",
    PROJECTION_MUTATIONS_ACTIVE_INDEX_SQL,
];

const V14_STATEMENTS: &[&str] = &[
    "ALTER TABLE integrity_metadata RENAME TO integrity_metadata_v13",
    r#"CREATE TABLE integrity_metadata (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        instance_id TEXT NOT NULL,
        path_binding TEXT NOT NULL CHECK(length(path_binding) = 64),
        key_epoch INTEGER NOT NULL CHECK(key_epoch > 0),
        provider_id TEXT CHECK(provider_id IS NULL OR provider_id IN ('system-keyring','in-memory-test')),
        sequence INTEGER NOT NULL CHECK(sequence >= 0),
        root TEXT NOT NULL CHECK(length(root) = 64),
        state_digest TEXT NOT NULL CHECK(length(state_digest) = 64),
        commit_mac TEXT NOT NULL CHECK(length(commit_mac) = 64),
        status TEXT NOT NULL CHECK(status IN ('active','recovery_required'))
    )"#,
    r#"INSERT INTO integrity_metadata(
        singleton,instance_id,path_binding,key_epoch,provider_id,sequence,root,state_digest,commit_mac,status
    )
    SELECT singleton,instance_id,path_binding,key_epoch,NULL,sequence,root,state_digest,commit_mac,status
    FROM integrity_metadata_v13"#,
    "DROP TABLE integrity_metadata_v13",
    "ALTER TABLE integrity_commits RENAME TO integrity_commits_v13",
    r#"CREATE TABLE integrity_commits (
        sequence INTEGER PRIMARY KEY CHECK(sequence >= 0),
        instance_id TEXT NOT NULL,
        key_epoch INTEGER NOT NULL CHECK(key_epoch > 0),
        provider_id TEXT CHECK(provider_id IS NULL OR provider_id IN ('system-keyring','in-memory-test')),
        parent_root TEXT NOT NULL,
        state_digest TEXT NOT NULL CHECK(length(state_digest) = 64),
        root TEXT NOT NULL UNIQUE CHECK(length(root) = 64),
        commit_mac TEXT NOT NULL CHECK(length(commit_mac) = 64)
    )"#,
    r#"INSERT INTO integrity_commits(
        sequence,instance_id,key_epoch,provider_id,parent_root,state_digest,root,commit_mac
    )
    SELECT sequence,instance_id,key_epoch,NULL,parent_root,state_digest,root,commit_mac
    FROM integrity_commits_v13"#,
    "DROP TABLE integrity_commits_v13",
];

const V16_STATEMENTS: &[&str] = &[
    "ALTER TABLE mcp_profiles ADD COLUMN credential_references_json TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE mcp_profile_revisions ADD COLUMN credential_references_json TEXT NOT NULL DEFAULT '[]'",
];

const V17_STATEMENTS: &[&str] = &[r#"CREATE TABLE managed_credential_enrollments (
        managed_mcp_id TEXT PRIMARY KEY REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        manifest_digest TEXT NOT NULL REFERENCES manifest_blobs(manifest_digest) ON DELETE RESTRICT,
        auth_schema_id TEXT NOT NULL,
        credential_reference TEXT NOT NULL,
        reference_digest TEXT NOT NULL CHECK(length(reference_digest) = 64),
        authority_json TEXT NOT NULL,
        revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
        updated_at_ms INTEGER NOT NULL
    )"#];

const V18_STATEMENTS: &[&str] = &[
    "ALTER TABLE managed_credential_enrollments ADD COLUMN authority_evidence_digest TEXT CHECK(authority_evidence_digest IS NULL OR length(authority_evidence_digest) = 64)",
];

const V19_STATEMENTS: &[&str] = &[r#"CREATE TABLE mcp_profile_apply_confirmations (
        confirmation_hash TEXT PRIMARY KEY CHECK(length(confirmation_hash) = 64),
        plan_id TEXT NOT NULL REFERENCES mcp_profile_apply_plans(plan_id) ON DELETE RESTRICT,
        actor TEXT NOT NULL,
        expires_at_ms INTEGER NOT NULL,
        consumed_at_ms INTEGER,
        created_at_ms INTEGER NOT NULL
    )"#];

const V20_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE source_catalog_documents (
        source_id TEXT PRIMARY KEY,
        source_name TEXT NOT NULL,
        schema_version INTEGER NOT NULL CHECK(schema_version = 1),
        issued_at_ms INTEGER NOT NULL,
        source_status TEXT NOT NULL CHECK(source_status IN ('active','suspended','revoked')),
        raw_bytes BLOB NOT NULL,
        canonical_payload_bytes BLOB NOT NULL,
        canonical_envelope_bytes BLOB NOT NULL,
        raw_digest TEXT NOT NULL CHECK(length(raw_digest) = 64),
        canonical_digest TEXT NOT NULL CHECK(length(canonical_digest) = 64),
        signed_digest TEXT NOT NULL CHECK(length(signed_digest) = 64),
        binding_digest TEXT NOT NULL CHECK(length(binding_digest) = 64),
        document_digest TEXT NOT NULL CHECK(length(document_digest) = 64),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE source_root_policies (
        source_id TEXT PRIMARY KEY REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        current_quorum INTEGER NOT NULL CHECK(current_quorum > 0),
        previous_quorum INTEGER CHECK(previous_quorum IS NULL OR previous_quorum > 0),
        has_previous INTEGER NOT NULL CHECK(has_previous IN (0,1))
    )"#,
    r#"CREATE TABLE source_root_keys (
        source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        root_set TEXT NOT NULL CHECK(root_set IN ('current','previous')),
        kid TEXT NOT NULL,
        spki_der_b64u TEXT NOT NULL,
        key_order INTEGER NOT NULL CHECK(key_order >= 0),
        PRIMARY KEY(source_id, root_set, kid)
    )"#,
    r#"CREATE TABLE source_signatures (
        source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        kid TEXT NOT NULL,
        sig_b64u TEXT NOT NULL,
        signature_order INTEGER NOT NULL CHECK(signature_order >= 0),
        PRIMARY KEY(source_id, kid)
    )"#,
    r#"CREATE TABLE source_releases (
        source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        release_id TEXT NOT NULL,
        mcp_id TEXT NOT NULL,
        version TEXT NOT NULL,
        manifest_kind TEXT NOT NULL,
        platform TEXT NOT NULL,
        architecture TEXT NOT NULL,
        variant TEXT NOT NULL,
        release_status TEXT NOT NULL CHECK(release_status IN ('active','revoked','superseded')),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(source_id, release_id),
        UNIQUE(source_id, mcp_id, version, manifest_kind, platform, architecture, variant)
    )"#,
    r#"CREATE TABLE source_revocations (
        source_id TEXT NOT NULL,
        release_id TEXT NOT NULL,
        reason_code TEXT NOT NULL,
        revoked_at_ms INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(source_id, release_id),
        FOREIGN KEY(source_id, release_id) REFERENCES source_releases(source_id, release_id) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE source_operations (
        source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        operation_id TEXT NOT NULL,
        operation_kind TEXT NOT NULL CHECK(operation_kind IN ('ingest_snapshot','verify_snapshot','activate_source','revoke_release')),
        operation_status TEXT NOT NULL CHECK(operation_status IN ('pending','applied','failed','stopped')),
        last_error TEXT,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(source_id, operation_id)
    )"#,
    r#"CREATE TABLE source_stop_flags (
        source_id TEXT PRIMARY KEY REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        stop_required INTEGER NOT NULL CHECK(stop_required IN (0,1)),
        reason TEXT,
        updated_at_ms INTEGER NOT NULL
    )"#,
];

const V21_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE source_schema_fence (
        fence_id TEXT PRIMARY KEY,
        min_version INTEGER NOT NULL,
        max_version INTEGER NOT NULL,
        schema_hash TEXT NOT NULL CHECK(length(schema_hash) = 64)
    )"#,
    "CREATE INDEX source_releases_lookup ON source_releases(source_id, mcp_id, version)",
    "CREATE INDEX source_operations_status ON source_operations(source_id, operation_status)",
];

const V22_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_intake_configuration_refs (
        configuration_ref TEXT PRIMARY KEY,
        source_facet TEXT NOT NULL CHECK(source_facet IN (
            'catalog_planning','manual_https_candidate','approved_stdio_candidate','legacy_quarantine'
        )),
        redacted_descriptor_json TEXT NOT NULL,
        private_reference TEXT,
        descriptor_digest TEXT NOT NULL UNIQUE CHECK(length(descriptor_digest) = 64),
        created_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE mcp_intake_candidates (
        candidate_id TEXT PRIMARY KEY,
        submission_binding TEXT NOT NULL UNIQUE CHECK(length(submission_binding) = 64),
        source_facet TEXT NOT NULL CHECK(source_facet IN (
            'catalog_planning','manual_https_candidate','approved_stdio_candidate','legacy_quarantine'
        )),
        transport TEXT NOT NULL CHECK(transport IN ('streamable_http','stdio','catalog_reference','legacy')),
        redacted_configuration_ref TEXT NOT NULL
            REFERENCES mcp_intake_configuration_refs(configuration_ref) ON DELETE RESTRICT,
        descriptor_digest TEXT NOT NULL CHECK(length(descriptor_digest) = 64),
        created_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE mcp_intake_candidate_revisions (
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        seq INTEGER NOT NULL CHECK(seq > 0),
        lifecycle_state TEXT NOT NULL CHECK(lifecycle_state IN (
            'legacy_quarantine','submitted','awaiting_consent','consent_granted','approval_granted','binding_ready'
        )),
        gate_reason TEXT CHECK(gate_reason IS NULL OR gate_reason IN (
            'candidate_state_conflict','manifest_identity_conflict','manual_stdio_provider_unavailable',
            'remote_http_policy_unavailable','unsupported_provider','empty_provider','unsupported_transport'
        )),
        approval_binding TEXT CHECK(approval_binding IS NULL OR length(approval_binding) = 64),
        manifest_identity_binding TEXT CHECK(
            manifest_identity_binding IS NULL OR length(manifest_identity_binding) = 64
        ),
        registry_owned_payload_digest TEXT CHECK(
            registry_owned_payload_digest IS NULL OR length(registry_owned_payload_digest) = 64
        ),
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(candidate_id, seq)
    )"#,
    r#"CREATE TABLE mcp_intake_manifest_identity_claims (
        manifest_identity_binding TEXT PRIMARY KEY CHECK(length(manifest_identity_binding) = 64),
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        created_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE mcp_intake_consent_grants (
        candidate_id TEXT NOT NULL,
        seq INTEGER NOT NULL CHECK(seq > 0),
        consent_binding TEXT NOT NULL CHECK(length(consent_binding) = 64),
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(candidate_id, seq),
        FOREIGN KEY(candidate_id, seq)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_inspection_runs (
        candidate_id TEXT NOT NULL,
        seq INTEGER NOT NULL CHECK(seq > 0),
        inspection_state TEXT NOT NULL CHECK(inspection_state IN ('inspection_pending','inspection_recorded')),
        inspection_binding TEXT CHECK(inspection_binding IS NULL OR length(inspection_binding) = 64),
        result_ref TEXT,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(candidate_id, seq),
        FOREIGN KEY(candidate_id, seq)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_approval_events (
        candidate_id TEXT NOT NULL,
        seq INTEGER NOT NULL CHECK(seq > 0),
        approval_state TEXT NOT NULL CHECK(approval_state IN ('approval_granted')),
        approval_binding TEXT NOT NULL CHECK(length(approval_binding) = 64),
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(candidate_id, seq),
        FOREIGN KEY(candidate_id, seq)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_binding_events (
        candidate_id TEXT NOT NULL,
        seq INTEGER NOT NULL CHECK(seq > 0),
        binding_state TEXT NOT NULL CHECK(binding_state IN ('binding_ready')),
        approval_binding TEXT NOT NULL CHECK(length(approval_binding) = 64),
        manifest_identity_binding TEXT NOT NULL CHECK(length(manifest_identity_binding) = 64),
        registry_owned_payload_digest TEXT CHECK(
            registry_owned_payload_digest IS NULL OR length(registry_owned_payload_digest) = 64
        ),
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(candidate_id, seq),
        FOREIGN KEY(candidate_id, seq)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_projection_events (
        candidate_id TEXT NOT NULL,
        seq INTEGER NOT NULL CHECK(seq > 0),
        projection_state TEXT NOT NULL CHECK(projection_state IN ('projection_pending','projection_recorded')),
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(candidate_id, seq),
        FOREIGN KEY(candidate_id, seq)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
];

const V23_STATEMENTS: &[&str] = &[
    "CREATE INDEX mcp_intake_candidate_revision_lookup ON mcp_intake_candidate_revisions(candidate_id, seq DESC)",
    "CREATE INDEX mcp_intake_candidate_config_lookup ON mcp_intake_candidates(redacted_configuration_ref)",
    "CREATE INDEX mcp_intake_approval_lookup ON mcp_intake_approval_events(candidate_id, seq DESC)",
    "CREATE INDEX mcp_intake_binding_lookup ON mcp_intake_binding_events(candidate_id, seq DESC)",
    "CREATE INDEX mcp_intake_projection_lookup ON mcp_intake_projection_events(candidate_id, seq DESC)",
];

const V24_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_intake_inspection_consents (
        consent_id TEXT PRIMARY KEY,
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        candidate_lifecycle_state TEXT NOT NULL CHECK(candidate_lifecycle_state IN (
            'legacy_quarantine','submitted','awaiting_consent','consent_granted','approval_granted','binding_ready'
        )),
        source_facet TEXT NOT NULL CHECK(source_facet IN (
            'catalog_planning','manual_https_candidate','approved_stdio_candidate','legacy_quarantine'
        )),
        transport TEXT NOT NULL CHECK(transport IN ('streamable_http','stdio','catalog_reference','legacy')),
        purpose TEXT NOT NULL CHECK(purpose IN (
            'remote_candidate_boundary','approved_stdio_local_metadata'
        )),
        consent_binding TEXT NOT NULL UNIQUE CHECK(length(consent_binding) = 64),
        consent_lifecycle TEXT NOT NULL CHECK(consent_lifecycle IN ('consent_granted','consent_consumed')),
        policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
        expires_at_ms INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL,
        FOREIGN KEY(candidate_id, candidate_revision)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_inspection_snapshots (
        snapshot_id TEXT PRIMARY KEY,
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        source_facet TEXT NOT NULL CHECK(source_facet IN (
            'catalog_planning','manual_https_candidate','approved_stdio_candidate','legacy_quarantine'
        )),
        transport TEXT NOT NULL CHECK(transport IN ('streamable_http','stdio','catalog_reference','legacy')),
        purpose TEXT NOT NULL CHECK(purpose IN (
            'remote_candidate_boundary','approved_stdio_local_metadata'
        )),
        consent_id TEXT NOT NULL REFERENCES mcp_intake_inspection_consents(consent_id) ON DELETE RESTRICT,
        consent_binding TEXT NOT NULL CHECK(length(consent_binding) = 64),
        consent_lifecycle TEXT NOT NULL CHECK(consent_lifecycle IN ('consent_granted','consent_consumed')),
        policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
        inspection_state TEXT NOT NULL CHECK(inspection_state IN ('inspection_blocked','inspection_recorded')),
        observation_surface TEXT NOT NULL CHECK(observation_surface IN (
            'candidate_metadata','approved_stdio_local_metadata'
        )),
        operation_phase TEXT NOT NULL CHECK(operation_phase IN (
            'batch1_zero_egress','batch1_read_only_local_metadata'
        )),
        failure_family TEXT CHECK(failure_family IS NULL OR failure_family IN (
            'policy_drift','identity_conflict','candidate_state_conflict','stdio_disallowed',
            'transport_execution','transport_configuration'
        )),
        summary_code TEXT NOT NULL CHECK(summary_code IN (
            'network_execution_unavailable','local_metadata_only','policy_drift','identity_conflict',
            'candidate_state_conflict','stdio_disallowed','transport_configuration','classification_unavailable'
        )),
        conflict_codes_json TEXT NOT NULL,
        fact_codes_json TEXT NOT NULL,
        reusable INTEGER NOT NULL CHECK(reusable IN (0,1)),
        created_at_ms INTEGER NOT NULL,
        FOREIGN KEY(candidate_id, candidate_revision)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_inspection_consent_consumptions (
        consent_id TEXT PRIMARY KEY REFERENCES mcp_intake_inspection_consents(consent_id) ON DELETE RESTRICT,
        snapshot_id TEXT NOT NULL UNIQUE
            REFERENCES mcp_intake_inspection_snapshots(snapshot_id) ON DELETE RESTRICT,
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        created_at_ms INTEGER NOT NULL,
        FOREIGN KEY(candidate_id, candidate_revision)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    "CREATE INDEX mcp_intake_inspection_consent_lookup ON mcp_intake_inspection_consents(candidate_id, candidate_revision DESC)",
    "CREATE INDEX mcp_intake_inspection_snapshot_lookup ON mcp_intake_inspection_snapshots(candidate_id, candidate_revision DESC)",
];

const V25_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_intake_remote_inspection_consents (
        consent_id TEXT PRIMARY KEY,
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        candidate_lifecycle_state TEXT NOT NULL CHECK(candidate_lifecycle_state IN (
            'legacy_quarantine','submitted','awaiting_consent','consent_granted','approval_granted','binding_ready'
        )),
        source_facet TEXT NOT NULL CHECK(source_facet IN (
            'catalog_planning','manual_https_candidate','approved_stdio_candidate','legacy_quarantine'
        )),
        transport TEXT NOT NULL CHECK(transport IN ('streamable_http','catalog_reference')),
        purpose TEXT NOT NULL CHECK(purpose IN ('private_remote_check_v25')),
        policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
        expires_at_ms INTEGER NOT NULL,
        target_handle TEXT NOT NULL CHECK(length(target_handle) > 0 AND length(target_handle) <= 256),
        target_identity_binding TEXT NOT NULL CHECK(
            length(target_identity_binding) > 0 AND length(target_identity_binding) <= 256
        ),
        authority_provider_id TEXT NOT NULL CHECK(authority_provider_id IN (
            'sealed_authority','in_memory_test'
        )),
        provider_key_epoch INTEGER NOT NULL CHECK(provider_key_epoch > 0),
        generation INTEGER NOT NULL CHECK(generation > 0),
        created_at_ms INTEGER NOT NULL,
        CHECK(expires_at_ms > created_at_ms),
        FOREIGN KEY(candidate_id, candidate_revision)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_remote_inspection_attempts (
        attempt_id TEXT PRIMARY KEY,
        consent_id TEXT NOT NULL
            REFERENCES mcp_intake_remote_inspection_consents(consent_id) ON DELETE RESTRICT,
        attempt_state TEXT NOT NULL CHECK(attempt_state IN (
            'claimed','owner_started','owner_finished','finalized'
        )),
        claim_nonce TEXT NOT NULL CHECK(length(claim_nonce) > 0 AND length(claim_nonce) <= 256),
        claimed_at_ms INTEGER NOT NULL,
        claim_expires_at_ms INTEGER NOT NULL CHECK(claim_expires_at_ms > claimed_at_ms),
        owner_started_at_ms INTEGER,
        owner_finished_at_ms INTEGER,
        finalized_at_ms INTEGER,
        CHECK(owner_started_at_ms IS NULL OR claimed_at_ms <= owner_started_at_ms),
        CHECK(owner_started_at_ms IS NULL OR owner_started_at_ms < claim_expires_at_ms),
        CHECK(owner_finished_at_ms IS NULL OR owner_started_at_ms IS NOT NULL),
        CHECK(owner_finished_at_ms IS NULL OR owner_started_at_ms <= owner_finished_at_ms),
        CHECK(finalized_at_ms IS NULL OR claimed_at_ms <= finalized_at_ms),
        CHECK(finalized_at_ms IS NULL OR owner_finished_at_ms IS NULL OR owner_finished_at_ms <= finalized_at_ms),
        CHECK(finalized_at_ms IS NULL OR owner_started_at_ms IS NOT NULL OR claim_expires_at_ms <= finalized_at_ms),
        CHECK(
            (attempt_state = 'claimed'
                AND owner_started_at_ms IS NULL
                AND owner_finished_at_ms IS NULL
                AND finalized_at_ms IS NULL)
            OR (attempt_state = 'owner_started'
                AND owner_started_at_ms IS NOT NULL
                AND owner_finished_at_ms IS NULL
                AND finalized_at_ms IS NULL)
            OR (attempt_state = 'owner_finished'
                AND owner_started_at_ms IS NOT NULL
                AND owner_finished_at_ms IS NOT NULL
                AND finalized_at_ms IS NULL)
            OR (attempt_state = 'finalized'
                AND finalized_at_ms IS NOT NULL
                AND (
                    (owner_started_at_ms IS NULL
                        AND owner_finished_at_ms IS NULL
                        AND claim_expires_at_ms <= finalized_at_ms)
                    OR (owner_started_at_ms IS NOT NULL
                        AND owner_finished_at_ms IS NOT NULL
                        AND owner_finished_at_ms <= finalized_at_ms)
                ))
        )
    )"#,
    r#"CREATE TABLE mcp_intake_remote_inspection_snapshots (
        snapshot_id TEXT PRIMARY KEY,
        consent_id TEXT NOT NULL
            REFERENCES mcp_intake_remote_inspection_consents(consent_id) ON DELETE RESTRICT,
        attempt_id TEXT NOT NULL UNIQUE
            REFERENCES mcp_intake_remote_inspection_attempts(attempt_id) ON DELETE RESTRICT,
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        source_facet TEXT NOT NULL CHECK(source_facet IN (
            'catalog_planning','manual_https_candidate','approved_stdio_candidate','legacy_quarantine'
        )),
        transport TEXT NOT NULL CHECK(transport IN ('streamable_http','catalog_reference')),
        purpose TEXT NOT NULL CHECK(purpose IN ('private_remote_check_v25')),
        policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
        target_handle TEXT NOT NULL CHECK(length(target_handle) > 0 AND length(target_handle) <= 256),
        target_identity_binding TEXT NOT NULL CHECK(
            length(target_identity_binding) > 0 AND length(target_identity_binding) <= 256
        ),
        authority_provider_id TEXT NOT NULL CHECK(authority_provider_id IN (
            'sealed_authority','in_memory_test'
        )),
        provider_key_epoch INTEGER NOT NULL CHECK(provider_key_epoch > 0),
        generation INTEGER NOT NULL CHECK(generation > 0),
        safe_subcode TEXT NOT NULL CHECK(safe_subcode IN (
            'empty_target','control_character_rejected','invalid_url','https_required',
            'host_missing','host_must_contain_dot','ip_literal_rejected','userinfo_rejected',
            'fragment_rejected','query_rejected','port_not_allowed','active_attempt_exists',
            'claim_nonce_mismatch','invalid_attempt_transition','consent_expired',
            'consent_consumed','policy_revision_drift','snapshot_integrity_conflict',
            'classification_unavailable'
        )),
        safe_summary TEXT NOT NULL CHECK(safe_summary IN (
            'target_rejected','attempt_rejected','inspection_blocked','classification_unavailable'
        )),
        fail_closed INTEGER NOT NULL CHECK(fail_closed IN (0,1)),
        created_at_ms INTEGER NOT NULL,
        FOREIGN KEY(candidate_id, candidate_revision)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_remote_inspection_consumptions (
        consent_id TEXT PRIMARY KEY
            REFERENCES mcp_intake_remote_inspection_consents(consent_id) ON DELETE RESTRICT,
        attempt_id TEXT NOT NULL UNIQUE
            REFERENCES mcp_intake_remote_inspection_attempts(attempt_id) ON DELETE RESTRICT,
        snapshot_id TEXT NOT NULL UNIQUE
            REFERENCES mcp_intake_remote_inspection_snapshots(snapshot_id) ON DELETE RESTRICT,
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        created_at_ms INTEGER NOT NULL,
        FOREIGN KEY(candidate_id, candidate_revision)
            REFERENCES mcp_intake_candidate_revisions(candidate_id, seq) ON DELETE RESTRICT
    )"#,
    "CREATE INDEX mcp_intake_remote_inspection_consent_lookup ON mcp_intake_remote_inspection_consents(candidate_id, candidate_revision DESC)",
    "CREATE UNIQUE INDEX mcp_intake_remote_inspection_active_attempt ON mcp_intake_remote_inspection_attempts(consent_id) WHERE attempt_state IN ('claimed','owner_started','owner_finished')",
    "CREATE INDEX mcp_intake_remote_inspection_snapshot_lookup ON mcp_intake_remote_inspection_snapshots(candidate_id, candidate_revision DESC)",
];

const V26_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_intake_remote_inspection_scope_lineage_anchors_v26 (
        reservation_id TEXT PRIMARY KEY,
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        source_facet TEXT NOT NULL CHECK(source_facet = 'manual_https_candidate'),
        transport TEXT NOT NULL CHECK(transport = 'streamable_http'),
        purpose TEXT NOT NULL CHECK(purpose = 'private_remote_check_v26'),
        policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
        created_at_ms INTEGER NOT NULL,
        UNIQUE(candidate_id, candidate_revision, source_facet, transport, purpose, policy_revision)
    )"#,
    r#"CREATE TABLE mcp_intake_remote_inspection_reservations (
        reservation_id TEXT NOT NULL
            REFERENCES mcp_intake_remote_inspection_scope_lineage_anchors_v26(reservation_id) ON DELETE RESTRICT,
        generation INTEGER NOT NULL CHECK(generation > 0),
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        candidate_lifecycle_state TEXT NOT NULL CHECK(candidate_lifecycle_state IN (
            'submitted','awaiting_consent','consent_granted','approval_granted','binding_ready'
        )),
        source_facet TEXT NOT NULL CHECK(source_facet = 'manual_https_candidate'),
        transport TEXT NOT NULL CHECK(transport = 'streamable_http'),
        purpose TEXT NOT NULL CHECK(purpose = 'private_remote_check_v26'),
        policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
        intent_fingerprint TEXT NOT NULL CHECK(length(intent_fingerprint) > 0),
        intent_fingerprint_key_id TEXT NOT NULL CHECK(length(intent_fingerprint_key_id) > 0),
        state TEXT NOT NULL CHECK(state IN (
            'prebind_reserved','prebind_tombstoned_terminal','postbind_pending_commit',
            'reconcile_required','committed_claimable','postbind_blocked_terminal','claimed',
            'owner_started','owner_finished','consumed_finalized'
        )),
        consent_id TEXT,
        claim_attempt_id TEXT,
        no_live_proof_kind TEXT CHECK(no_live_proof_kind IS NULL OR no_live_proof_kind IN (
            'never_staged','tombstoned','definitive_no_live_postbind','unknown',
            'missing','unavailable','credential_lost'
        )),
        no_live_proof_id TEXT,
        no_live_proof_hmac TEXT,
        no_live_proof_key_epoch INTEGER,
        no_live_proof_observed_at_ms INTEGER,
        last_event_ordinal INTEGER NOT NULL CHECK(last_event_ordinal > 0),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(reservation_id, generation),
        UNIQUE(reservation_id, intent_fingerprint),
        CHECK(
            (no_live_proof_kind IS NULL AND no_live_proof_id IS NULL AND no_live_proof_hmac IS NULL
                AND no_live_proof_key_epoch IS NULL AND no_live_proof_observed_at_ms IS NULL)
            OR (no_live_proof_kind IS NOT NULL AND no_live_proof_id IS NOT NULL
                AND no_live_proof_hmac IS NOT NULL AND no_live_proof_key_epoch IS NOT NULL
                AND no_live_proof_observed_at_ms IS NOT NULL)
        )
    )"#,
    r#"CREATE TABLE mcp_intake_remote_inspection_reservation_events (
        reservation_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        event_ordinal INTEGER NOT NULL CHECK(event_ordinal > 0),
        event_type TEXT NOT NULL CHECK(event_type IN (
            'reserved','prebind_tombstoned','postbind_bound','authority_reconcile_required',
            'authority_committed','postbind_blocked_no_live','claim_expired','claimed',
            'owner_started','owner_finished','consumed_finalized'
        )),
        from_state TEXT NOT NULL,
        to_state TEXT NOT NULL,
        consent_id TEXT,
        claim_attempt_id TEXT,
        proof_kind TEXT,
        proof_id TEXT,
        proof_hmac TEXT,
        proof_key_epoch INTEGER,
        proof_observed_at_ms INTEGER,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(reservation_id, generation, event_ordinal),
        FOREIGN KEY(reservation_id, generation)
            REFERENCES mcp_intake_remote_inspection_reservations(reservation_id, generation)
            ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_remote_inspection_bindings_b26 (
        consent_id TEXT PRIMARY KEY
            REFERENCES mcp_intake_remote_inspection_consents(consent_id) ON DELETE RESTRICT,
        reservation_id TEXT NOT NULL,
        reservation_generation INTEGER NOT NULL CHECK(reservation_generation > 0),
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        source_facet TEXT NOT NULL CHECK(source_facet = 'manual_https_candidate'),
        transport TEXT NOT NULL CHECK(transport = 'streamable_http'),
        purpose TEXT NOT NULL CHECK(purpose = 'private_remote_check_v26'),
        policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
        intent_fingerprint TEXT NOT NULL CHECK(length(intent_fingerprint) > 0),
        intent_fingerprint_key_id TEXT NOT NULL CHECK(length(intent_fingerprint_key_id) > 0),
        target_handle TEXT NOT NULL CHECK(length(target_handle) > 0),
        target_identity_binding TEXT NOT NULL CHECK(length(target_identity_binding) > 0),
        authority_provider_id TEXT NOT NULL CHECK(authority_provider_id IN ('sealed_authority','in_memory_test')),
        authority_key_epoch INTEGER NOT NULL CHECK(authority_key_epoch > 0),
        authority_generation INTEGER NOT NULL CHECK(authority_generation > 0),
        authority_record_ref TEXT NOT NULL CHECK(length(authority_record_ref) > 0),
        created_at_ms INTEGER NOT NULL,
        FOREIGN KEY(reservation_id, reservation_generation)
            REFERENCES mcp_intake_remote_inspection_reservations(reservation_id, generation)
            ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE mcp_intake_remote_inspection_commit_events_v26 (
        consent_id TEXT NOT NULL
            REFERENCES mcp_intake_remote_inspection_consents(consent_id) ON DELETE RESTRICT,
        ordinal INTEGER NOT NULL CHECK(ordinal IN (1,2)),
        reservation_id TEXT NOT NULL,
        reservation_generation INTEGER NOT NULL CHECK(reservation_generation > 0),
        candidate_id TEXT NOT NULL REFERENCES mcp_intake_candidates(candidate_id) ON DELETE RESTRICT,
        candidate_revision INTEGER NOT NULL CHECK(candidate_revision > 0),
        source_facet TEXT NOT NULL CHECK(source_facet = 'manual_https_candidate'),
        transport TEXT NOT NULL CHECK(transport = 'streamable_http'),
        purpose TEXT NOT NULL CHECK(purpose = 'private_remote_check_v26'),
        policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
        intent_fingerprint TEXT NOT NULL CHECK(length(intent_fingerprint) > 0),
        intent_fingerprint_key_id TEXT NOT NULL CHECK(length(intent_fingerprint_key_id) > 0),
        target_handle TEXT NOT NULL CHECK(length(target_handle) > 0),
        target_identity_binding TEXT NOT NULL CHECK(length(target_identity_binding) > 0),
        authority_provider_id TEXT NOT NULL CHECK(authority_provider_id IN ('sealed_authority','in_memory_test')),
        authority_key_epoch INTEGER NOT NULL CHECK(authority_key_epoch > 0),
        authority_generation INTEGER NOT NULL CHECK(authority_generation > 0),
        authority_record_ref TEXT NOT NULL CHECK(length(authority_record_ref) > 0),
        commit_state TEXT NOT NULL CHECK(commit_state IN (
            'pending_authority_commit','committed','blocked_no_live'
        )),
        no_live_proof_kind TEXT CHECK(no_live_proof_kind IS NULL OR no_live_proof_kind IN (
            'never_staged','tombstoned','definitive_no_live_postbind','unknown',
            'missing','unavailable','credential_lost'
        )),
        no_live_proof_id TEXT,
        no_live_proof_hmac TEXT,
        no_live_proof_key_epoch INTEGER,
        no_live_proof_observed_at_ms INTEGER,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(consent_id, ordinal),
        FOREIGN KEY(reservation_id, reservation_generation)
            REFERENCES mcp_intake_remote_inspection_reservations(reservation_id, generation)
            ON DELETE RESTRICT,
        CHECK(
            ordinal = 1
            OR (ordinal = 2 AND commit_state IN ('committed','blocked_no_live'))
        )
    )"#,
    "CREATE INDEX mcp_intake_remote_inspection_reservation_lookup_v26 ON mcp_intake_remote_inspection_reservations(candidate_id, candidate_revision DESC)",
    "CREATE INDEX mcp_intake_remote_inspection_claim_lookup_v26 ON mcp_intake_remote_inspection_reservations(claim_attempt_id) WHERE claim_attempt_id IS NOT NULL",
    r#"CREATE TRIGGER mcp_intake_remote_inspection_reserved_event_v26
        AFTER INSERT ON mcp_intake_remote_inspection_reservations
        BEGIN
            INSERT INTO mcp_intake_remote_inspection_reservation_events(
                reservation_id,generation,event_ordinal,event_type,from_state,to_state,
                consent_id,claim_attempt_id,proof_kind,proof_id,proof_hmac,proof_key_epoch,
                proof_observed_at_ms,created_at_ms
            ) VALUES (
                NEW.reservation_id,NEW.generation,1,'reserved',NEW.state,NEW.state,
                NULL,NULL,NULL,NULL,NULL,NULL,NULL,NEW.created_at_ms
            );
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_commit_ord2_requires_ord1_v26
        BEFORE INSERT ON mcp_intake_remote_inspection_commit_events_v26
        WHEN NEW.ordinal = 2
        BEGIN
            SELECT CASE
                WHEN NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_commit_events_v26 prior
                    WHERE prior.consent_id = NEW.consent_id
                      AND prior.ordinal = 1
                      AND prior.reservation_id = NEW.reservation_id
                      AND prior.reservation_generation = NEW.reservation_generation
                      AND prior.candidate_id = NEW.candidate_id
                      AND prior.candidate_revision = NEW.candidate_revision
                      AND prior.source_facet = NEW.source_facet
                      AND prior.transport = NEW.transport
                      AND prior.purpose = NEW.purpose
                      AND prior.policy_revision = NEW.policy_revision
                      AND prior.intent_fingerprint = NEW.intent_fingerprint
                      AND prior.intent_fingerprint_key_id = NEW.intent_fingerprint_key_id
                      AND prior.target_handle = NEW.target_handle
                      AND prior.target_identity_binding = NEW.target_identity_binding
                      AND prior.authority_provider_id = NEW.authority_provider_id
                      AND prior.authority_key_epoch = NEW.authority_key_epoch
                      AND prior.authority_generation = NEW.authority_generation
                      AND prior.authority_record_ref = NEW.authority_record_ref
                      AND prior.commit_state = 'pending_authority_commit'
                ) THEN RAISE(ABORT, 'ri_v26_missing_pending_commit')
            END;
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_consents_v26_manual_only
        BEFORE INSERT ON mcp_intake_remote_inspection_consents
        BEGIN
            SELECT CASE
                WHEN NEW.source_facet <> 'manual_https_candidate'
                  OR NEW.transport <> 'streamable_http'
                  OR NEW.purpose <> 'private_remote_check_v25'
                THEN RAISE(ABORT, 'ri_v25_manual_only')
            END;
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_consents_v26_manual_only_update
        BEFORE UPDATE ON mcp_intake_remote_inspection_consents
        BEGIN
            SELECT CASE
                WHEN NEW.source_facet <> 'manual_https_candidate'
                  OR NEW.transport <> 'streamable_http'
                  OR NEW.purpose <> 'private_remote_check_v25'
                  OR OLD.source_facet <> NEW.source_facet
                  OR OLD.transport <> NEW.transport
                  OR OLD.purpose <> NEW.purpose
                THEN RAISE(ABORT, 'ri_v25_manual_only_update')
            END;
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_snapshots_v26_manual_only
        BEFORE INSERT ON mcp_intake_remote_inspection_snapshots
        BEGIN
            SELECT CASE
                WHEN NEW.source_facet <> 'manual_https_candidate'
                  OR NEW.transport <> 'streamable_http'
                  OR NEW.purpose <> 'private_remote_check_v25'
                THEN RAISE(ABORT, 'ri_v25_snapshot_manual_only')
            END;
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_snapshots_v26_manual_only_update
        BEFORE UPDATE ON mcp_intake_remote_inspection_snapshots
        BEGIN
            SELECT CASE
                WHEN NEW.source_facet <> 'manual_https_candidate'
                  OR NEW.transport <> 'streamable_http'
                  OR NEW.purpose <> 'private_remote_check_v25'
                  OR OLD.source_facet <> NEW.source_facet
                  OR OLD.transport <> NEW.transport
                  OR OLD.purpose <> NEW.purpose
                THEN RAISE(ABORT, 'ri_v25_snapshot_manual_only_update')
            END;
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_reservations_validate_insert_v26
        BEFORE INSERT ON mcp_intake_remote_inspection_reservations
        BEGIN
            SELECT CASE
                WHEN NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_scope_lineage_anchors_v26 anchor
                    WHERE anchor.reservation_id = NEW.reservation_id
                      AND anchor.candidate_id = NEW.candidate_id
                      AND anchor.candidate_revision = NEW.candidate_revision
                      AND anchor.source_facet = NEW.source_facet
                      AND anchor.transport = NEW.transport
                      AND anchor.purpose = NEW.purpose
                      AND anchor.policy_revision = NEW.policy_revision
                ) THEN RAISE(ABORT, 'ri_v26_anchor_mismatch')
            END;
            SELECT CASE
                WHEN NEW.state <> 'prebind_reserved'
                  OR NEW.last_event_ordinal <> 1
                  OR NEW.consent_id IS NOT NULL
                  OR NEW.claim_attempt_id IS NOT NULL
                THEN RAISE(ABORT, 'ri_v26_invalid_initial_state')
            END;
            SELECT CASE
                WHEN NEW.generation = 1
                 AND EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_reservations prior
                    WHERE prior.reservation_id = NEW.reservation_id
                ) THEN RAISE(ABORT, 'ri_v26_duplicate_generation_one')
            END;
            SELECT CASE
                WHEN NEW.generation > 1
                 AND NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_reservations prior
                    WHERE prior.reservation_id = NEW.reservation_id
                      AND prior.generation = NEW.generation - 1
                      AND prior.state = 'prebind_tombstoned_terminal'
                ) THEN RAISE(ABORT, 'ri_v26_generation_gap')
            END;
            SELECT CASE
                WHEN NEW.generation > 1
                 AND EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_reservations prior
                    WHERE prior.reservation_id = NEW.reservation_id
                      AND prior.intent_fingerprint = NEW.intent_fingerprint
                ) THEN RAISE(ABORT, 'ri_v26_old_credential_reuse')
            END;
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_bindings_validate_insert_v26
        BEFORE INSERT ON mcp_intake_remote_inspection_bindings_b26
        BEGIN
            SELECT CASE
                WHEN NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_reservations reservation
                    WHERE reservation.reservation_id = NEW.reservation_id
                      AND reservation.generation = NEW.reservation_generation
                      AND reservation.state = 'prebind_reserved'
                      AND reservation.consent_id IS NULL
                      AND reservation.claim_attempt_id IS NULL
                      AND reservation.candidate_id = NEW.candidate_id
                      AND reservation.candidate_revision = NEW.candidate_revision
                      AND reservation.source_facet = NEW.source_facet
                      AND reservation.transport = NEW.transport
                      AND reservation.purpose = NEW.purpose
                      AND reservation.policy_revision = NEW.policy_revision
                      AND reservation.intent_fingerprint = NEW.intent_fingerprint
                      AND reservation.intent_fingerprint_key_id = NEW.intent_fingerprint_key_id
                ) THEN RAISE(ABORT, 'ri_v26_binding_missing_reservation')
            END;
            SELECT CASE
                WHEN NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_consents consent
                    WHERE consent.consent_id = NEW.consent_id
                      AND consent.candidate_id = NEW.candidate_id
                      AND consent.candidate_revision = NEW.candidate_revision
                      AND consent.source_facet = 'manual_https_candidate'
                      AND consent.transport = 'streamable_http'
                      AND consent.purpose = 'private_remote_check_v25'
                      AND consent.policy_revision = NEW.policy_revision
                      AND consent.target_handle = NEW.target_handle
                      AND consent.target_identity_binding = NEW.target_identity_binding
                      AND consent.authority_provider_id = NEW.authority_provider_id
                      AND consent.provider_key_epoch = NEW.authority_key_epoch
                      AND consent.generation = NEW.authority_generation
                ) THEN RAISE(ABORT, 'ri_v26_binding_missing_v25_consent')
            END;
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_commit_events_validate_insert_v26
        BEFORE INSERT ON mcp_intake_remote_inspection_commit_events_v26
        BEGIN
            SELECT CASE
                WHEN NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_bindings_b26 binding
                    WHERE binding.consent_id = NEW.consent_id
                      AND binding.reservation_id = NEW.reservation_id
                      AND binding.reservation_generation = NEW.reservation_generation
                      AND binding.candidate_id = NEW.candidate_id
                      AND binding.candidate_revision = NEW.candidate_revision
                      AND binding.source_facet = NEW.source_facet
                      AND binding.transport = NEW.transport
                      AND binding.purpose = NEW.purpose
                      AND binding.policy_revision = NEW.policy_revision
                      AND binding.intent_fingerprint = NEW.intent_fingerprint
                      AND binding.intent_fingerprint_key_id = NEW.intent_fingerprint_key_id
                      AND binding.target_handle = NEW.target_handle
                      AND binding.target_identity_binding = NEW.target_identity_binding
                      AND binding.authority_provider_id = NEW.authority_provider_id
                      AND binding.authority_key_epoch = NEW.authority_key_epoch
                      AND binding.authority_generation = NEW.authority_generation
                      AND binding.authority_record_ref = NEW.authority_record_ref
                ) THEN RAISE(ABORT, 'ri_v26_commit_missing_binding')
            END;
            SELECT CASE
                WHEN NEW.ordinal = 1
                 AND (
                    NEW.commit_state <> 'pending_authority_commit'
                    OR NEW.no_live_proof_kind IS NOT NULL
                    OR NEW.no_live_proof_id IS NOT NULL
                    OR NEW.no_live_proof_hmac IS NOT NULL
                    OR NEW.no_live_proof_key_epoch IS NOT NULL
                    OR NEW.no_live_proof_observed_at_ms IS NOT NULL
                 ) THEN RAISE(ABORT, 'ri_v26_invalid_ord1')
            END;
            SELECT CASE
                WHEN NEW.ordinal = 2
                 AND NEW.commit_state = 'committed'
                 AND (
                    NEW.no_live_proof_kind IS NOT NULL
                    OR NEW.no_live_proof_id IS NOT NULL
                    OR NEW.no_live_proof_hmac IS NOT NULL
                    OR NEW.no_live_proof_key_epoch IS NOT NULL
                    OR NEW.no_live_proof_observed_at_ms IS NOT NULL
                 ) THEN RAISE(ABORT, 'ri_v26_committed_no_proof')
            END;
            SELECT CASE
                WHEN NEW.ordinal = 2
                 AND NEW.commit_state = 'blocked_no_live'
                 AND (
                    NEW.no_live_proof_kind <> 'definitive_no_live_postbind'
                    OR NEW.no_live_proof_id IS NULL
                    OR NEW.no_live_proof_hmac IS NULL
                    OR NEW.no_live_proof_key_epoch IS NULL
                    OR NEW.no_live_proof_observed_at_ms IS NULL
                 ) THEN RAISE(ABORT, 'ri_v26_blocked_proof_required')
            END;
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_reservation_events_immutable_v26
        BEFORE UPDATE ON mcp_intake_remote_inspection_reservation_events
        BEGIN
            SELECT RAISE(ABORT, 'ri_v26_reservation_event_immutable');
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_bindings_immutable_v26
        BEFORE UPDATE ON mcp_intake_remote_inspection_bindings_b26
        BEGIN
            SELECT RAISE(ABORT, 'ri_v26_binding_immutable');
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_commit_events_immutable_v26
        BEFORE UPDATE ON mcp_intake_remote_inspection_commit_events_v26
        BEGIN
            SELECT RAISE(ABORT, 'ri_v26_commit_event_immutable');
        END"#,
    r#"CREATE TRIGGER mcp_intake_remote_inspection_reservations_validate_update_v26
        BEFORE UPDATE ON mcp_intake_remote_inspection_reservations
        BEGIN
            SELECT CASE
                WHEN OLD.reservation_id <> NEW.reservation_id
                  OR OLD.generation <> NEW.generation
                  OR OLD.candidate_id <> NEW.candidate_id
                  OR OLD.candidate_revision <> NEW.candidate_revision
                  OR OLD.source_facet <> NEW.source_facet
                  OR OLD.transport <> NEW.transport
                  OR OLD.purpose <> NEW.purpose
                  OR OLD.policy_revision <> NEW.policy_revision
                  OR OLD.intent_fingerprint <> NEW.intent_fingerprint
                  OR OLD.intent_fingerprint_key_id <> NEW.intent_fingerprint_key_id
                THEN RAISE(ABORT, 'ri_v26_identity_mutation')
            END;
            SELECT CASE
                WHEN NEW.last_event_ordinal <> OLD.last_event_ordinal + 1
                THEN RAISE(ABORT, 'ri_v26_event_ordinal_gap')
            END;
            SELECT CASE
                WHEN NOT (
                    (OLD.state = 'prebind_reserved' AND NEW.state IN ('prebind_tombstoned_terminal', 'postbind_pending_commit'))
                    OR (OLD.state = 'postbind_pending_commit' AND NEW.state IN ('reconcile_required', 'committed_claimable', 'postbind_blocked_terminal'))
                    OR (OLD.state = 'reconcile_required' AND NEW.state IN ('committed_claimable', 'postbind_blocked_terminal'))
                    OR (
                        OLD.state = 'claimed'
                        AND NEW.state = 'committed_claimable'
                        AND OLD.consent_id = NEW.consent_id
                        AND OLD.claim_attempt_id IS NOT NULL
                        AND NEW.claim_attempt_id IS NULL
                        AND EXISTS (
                            SELECT 1
                            FROM mcp_intake_remote_inspection_attempts attempt
                            WHERE attempt.attempt_id = OLD.claim_attempt_id
                              AND attempt.consent_id = OLD.consent_id
                              AND attempt.attempt_state = 'finalized'
                              AND attempt.owner_started_at_ms IS NULL
                              AND attempt.owner_finished_at_ms IS NULL
                              AND attempt.finalized_at_ms IS NOT NULL
                              AND attempt.claim_expires_at_ms <= attempt.finalized_at_ms
                        )
                    )
                    OR (OLD.state = 'committed_claimable' AND NEW.state = 'claimed')
                    OR (OLD.state = 'claimed' AND NEW.state = 'owner_started')
                    OR (OLD.state = 'owner_started' AND NEW.state = 'owner_finished')
                    OR (OLD.state = 'owner_finished' AND NEW.state = 'consumed_finalized')
                ) THEN RAISE(ABORT, 'ri_v26_invalid_transition')
            END;
            SELECT CASE
                WHEN NEW.state = 'prebind_tombstoned_terminal'
                 AND (
                    NEW.consent_id IS NOT NULL
                    OR NEW.claim_attempt_id IS NOT NULL
                    OR NEW.no_live_proof_kind <> 'tombstoned'
                    OR NEW.no_live_proof_id IS NULL
                    OR NEW.no_live_proof_hmac IS NULL
                    OR NEW.no_live_proof_key_epoch IS NULL
                    OR NEW.no_live_proof_observed_at_ms IS NULL
                 ) THEN RAISE(ABORT, 'ri_v26_prebind_tombstone_proof_required')
            END;
            SELECT CASE
                WHEN NEW.state = 'postbind_pending_commit'
                 AND NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_bindings_b26 binding
                    WHERE binding.consent_id = NEW.consent_id
                      AND binding.reservation_id = NEW.reservation_id
                      AND binding.reservation_generation = NEW.generation
                 ) THEN RAISE(ABORT, 'ri_v26_missing_binding')
            END;
            SELECT CASE
                WHEN NEW.state = 'reconcile_required'
                 AND NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_commit_events_v26 event
                    WHERE event.consent_id = NEW.consent_id
                      AND event.ordinal = 1
                      AND event.reservation_id = NEW.reservation_id
                      AND event.reservation_generation = NEW.generation
                      AND event.commit_state = 'pending_authority_commit'
                 ) THEN RAISE(ABORT, 'ri_v26_missing_commit_ord1')
            END;
            SELECT CASE
                WHEN NEW.state = 'committed_claimable'
                 AND (
                    NEW.claim_attempt_id IS NOT NULL
                    OR NOT EXISTS (
                        SELECT 1
                        FROM mcp_intake_remote_inspection_commit_events_v26 event
                        WHERE event.consent_id = NEW.consent_id
                          AND event.ordinal = 2
                          AND event.reservation_id = NEW.reservation_id
                          AND event.reservation_generation = NEW.generation
                          AND event.commit_state = 'committed'
                    )
                 ) THEN RAISE(ABORT, 'ri_v26_missing_commit_ord2')
            END;
            SELECT CASE
                WHEN NEW.state = 'postbind_blocked_terminal'
                 AND (
                    NEW.no_live_proof_kind <> 'definitive_no_live_postbind'
                    OR NOT EXISTS (
                        SELECT 1
                        FROM mcp_intake_remote_inspection_commit_events_v26 event
                        WHERE event.consent_id = NEW.consent_id
                          AND event.ordinal = 2
                          AND event.reservation_id = NEW.reservation_id
                          AND event.reservation_generation = NEW.generation
                          AND event.commit_state = 'blocked_no_live'
                          AND event.no_live_proof_kind = NEW.no_live_proof_kind
                          AND event.no_live_proof_id = NEW.no_live_proof_id
                          AND event.no_live_proof_hmac = NEW.no_live_proof_hmac
                          AND event.no_live_proof_key_epoch = NEW.no_live_proof_key_epoch
                          AND event.no_live_proof_observed_at_ms = NEW.no_live_proof_observed_at_ms
                    )
                 ) THEN RAISE(ABORT, 'ri_v26_missing_blocked_proof')
            END;
            SELECT CASE
                WHEN NEW.state = 'claimed'
                 AND NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_attempts attempt
                    WHERE attempt.attempt_id = NEW.claim_attempt_id
                      AND attempt.consent_id = NEW.consent_id
                      AND attempt.attempt_state = 'claimed'
                 ) THEN RAISE(ABORT, 'ri_v26_claim_attempt_mismatch')
            END;
            SELECT CASE
                WHEN NEW.state = 'owner_started'
                 AND NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_attempts attempt
                    WHERE attempt.attempt_id = NEW.claim_attempt_id
                      AND attempt.consent_id = NEW.consent_id
                      AND attempt.attempt_state = 'owner_started'
                 ) THEN RAISE(ABORT, 'ri_v26_owner_started_mismatch')
            END;
            SELECT CASE
                WHEN NEW.state = 'owner_finished'
                 AND NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_attempts attempt
                    WHERE attempt.attempt_id = NEW.claim_attempt_id
                      AND attempt.consent_id = NEW.consent_id
                      AND attempt.attempt_state = 'owner_finished'
                 ) THEN RAISE(ABORT, 'ri_v26_owner_finished_mismatch')
            END;
            SELECT CASE
                WHEN NEW.state = 'consumed_finalized'
                 AND NOT EXISTS (
                    SELECT 1
                    FROM mcp_intake_remote_inspection_consumptions consumption
                    JOIN mcp_intake_remote_inspection_attempts attempt
                      ON attempt.attempt_id = consumption.attempt_id
                    WHERE consumption.consent_id = NEW.consent_id
                      AND consumption.attempt_id = NEW.claim_attempt_id
                      AND attempt.attempt_state = 'finalized'
                 ) THEN RAISE(ABORT, 'ri_v26_consumption_mismatch')
            END;
        END"#,
];

const V27_STATEMENTS: &[&str] = &[
    "ALTER TABLE manifest_blobs ADD COLUMN source_ref_json TEXT",
    "ALTER TABLE manifest_blobs ADD COLUMN import_kind TEXT CHECK(import_kind IS NULL OR import_kind IN ('local_persistence','verified_source_catalog','https_manifest_url','enterprise_directory'))",
    "ALTER TABLE manifest_blobs ADD COLUMN release_id TEXT",
    "ALTER TABLE manifest_blobs ADD COLUMN origin_provenance_json TEXT",
    "ALTER TABLE manifest_blobs ADD COLUMN update_channel_json TEXT",
];

const V28_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE governed_catalog_sources (
        source_id TEXT PRIMARY KEY,
        import_kind TEXT NOT NULL CHECK(import_kind IN ('local_persistence','verified_source_catalog','https_manifest_url','enterprise_directory')),
        source_ref_json TEXT NOT NULL,
        display_name TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE governed_catalog_documents (
        source_id TEXT NOT NULL REFERENCES governed_catalog_sources(source_id) ON DELETE CASCADE,
        document_id TEXT NOT NULL,
        document_digest TEXT NOT NULL CHECK(length(document_digest) = 64),
        document_kind TEXT NOT NULL CHECK(document_kind IN ('manifest','directory')),
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(source_id, document_id),
        UNIQUE(source_id, document_digest, document_kind)
    )"#,
    r#"CREATE TABLE governed_catalog_entries (
        source_id TEXT NOT NULL REFERENCES governed_catalog_sources(source_id) ON DELETE CASCADE,
        entry_id TEXT NOT NULL,
        document_id TEXT NOT NULL,
        manifest_digest TEXT NOT NULL REFERENCES manifest_blobs(manifest_digest) ON DELETE RESTRICT,
        mcp_id TEXT NOT NULL,
        version TEXT NOT NULL,
        proof_json TEXT NOT NULL,
        trust_tier_json TEXT NOT NULL,
        source_metadata_json TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(source_id, entry_id),
        UNIQUE(source_id, mcp_id, version),
        FOREIGN KEY(source_id, document_id)
            REFERENCES governed_catalog_documents(source_id, document_id) ON DELETE CASCADE
    )"#,
    "CREATE INDEX governed_catalog_entries_digest_lookup ON governed_catalog_entries(manifest_digest, source_id)",
];

const V29_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE governed_source_refresh_registrations (
        source_id TEXT PRIMARY KEY REFERENCES governed_catalog_sources(source_id) ON DELETE CASCADE,
        transport_kind TEXT NOT NULL CHECK(transport_kind IN ('verified_source_bundle_v1')),
        endpoint TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        last_attempted_at_ms INTEGER,
        last_refreshed_at_ms INTEGER,
        last_result TEXT NOT NULL CHECK(last_result IN ('idle','succeeded','failed')),
        last_document_digest TEXT CHECK(last_document_digest IS NULL OR length(last_document_digest) = 64),
        last_error_code TEXT
    )"#,
    r#"CREATE TABLE governed_source_refresh_audits (
        audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
        source_id TEXT NOT NULL REFERENCES governed_source_refresh_registrations(source_id) ON DELETE CASCADE,
        document_digest TEXT CHECK(document_digest IS NULL OR length(document_digest) = 64),
        result TEXT NOT NULL CHECK(result IN ('idle','succeeded','failed')),
        error_code TEXT,
        actor TEXT NOT NULL,
        correlation_id TEXT NOT NULL,
        occurred_at_ms INTEGER NOT NULL
    )"#,
    "CREATE INDEX governed_source_refresh_audits_source_time ON governed_source_refresh_audits(source_id, occurred_at_ms, audit_id)",
];

const V30_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE governed_source_refresh_trust_anchors (
        source_id TEXT PRIMARY KEY REFERENCES governed_source_refresh_registrations(source_id) ON DELETE CASCADE,
        verified_source_id TEXT NOT NULL,
        endpoint TEXT NOT NULL,
        root_digest TEXT NOT NULL CHECK(length(root_digest) = 64),
        anchor_document_digest TEXT NOT NULL CHECK(length(anchor_document_digest) = 64),
        anchor_document_bytes BLOB NOT NULL,
        created_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TRIGGER governed_source_refresh_anchor_endpoint_matches_registration
        BEFORE INSERT ON governed_source_refresh_trust_anchors
        FOR EACH ROW BEGIN
            SELECT CASE
                WHEN NOT EXISTS (
                    SELECT 1
                    FROM governed_source_refresh_registrations registration
                    WHERE registration.source_id = NEW.source_id
                      AND registration.endpoint = NEW.endpoint
                ) THEN RAISE(ABORT, 'governed_source_refresh_anchor_endpoint_mismatch')
            END;
        END"#,
    r#"CREATE TRIGGER governed_source_refresh_registration_endpoint_is_immutable
        BEFORE UPDATE OF endpoint ON governed_source_refresh_registrations
        FOR EACH ROW
        WHEN NEW.endpoint <> OLD.endpoint
         AND EXISTS (
            SELECT 1
            FROM governed_source_refresh_trust_anchors anchor
            WHERE anchor.source_id = OLD.source_id
         )
        BEGIN
            SELECT RAISE(ABORT, 'governed_source_refresh_endpoint_is_immutable');
        END"#,
];

const V31_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE governed_source_trust_pins (
        source_id TEXT PRIMARY KEY REFERENCES governed_catalog_sources(source_id) ON DELETE RESTRICT,
        verified_source_id TEXT NOT NULL UNIQUE REFERENCES source_catalog_documents(source_id) ON DELETE RESTRICT,
        trust_basis TEXT NOT NULL CHECK(trust_basis IN ('user_pin')),
        root_digest TEXT NOT NULL CHECK(length(root_digest) = 64),
        endpoint TEXT NOT NULL CHECK(length(endpoint) > 0),
        source_document_digest TEXT NOT NULL CHECK(length(source_document_digest) = 64),
        descriptor_digest TEXT NOT NULL CHECK(length(descriptor_digest) = 64),
        created_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE governed_source_provision_audits (
        provision_id TEXT PRIMARY KEY,
        source_id TEXT NOT NULL REFERENCES governed_source_trust_pins(source_id) ON DELETE RESTRICT,
        document_digest TEXT NOT NULL CHECK(length(document_digest) = 64),
        root_digest TEXT NOT NULL CHECK(length(root_digest) = 64),
        descriptor_digest TEXT NOT NULL CHECK(length(descriptor_digest) = 64),
        trust_basis TEXT NOT NULL CHECK(trust_basis IN ('user_pin')),
        actor TEXT NOT NULL,
        correlation_id TEXT NOT NULL,
        occurred_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TRIGGER governed_source_refresh_anchor_requires_user_pin
        BEFORE INSERT ON governed_source_refresh_trust_anchors
        FOR EACH ROW BEGIN
            SELECT CASE
                WHEN NOT EXISTS (
                    SELECT 1
                    FROM governed_source_trust_pins pin
                    WHERE pin.source_id = NEW.source_id
                      AND pin.verified_source_id = NEW.verified_source_id
                      AND pin.trust_basis = 'user_pin'
                      AND pin.root_digest = NEW.root_digest
                      AND pin.endpoint = NEW.endpoint
                      AND pin.source_document_digest = NEW.anchor_document_digest
                ) THEN RAISE(ABORT, 'governed_source_refresh_anchor_requires_user_pin')
            END;
        END"#,
    r#"CREATE TRIGGER governed_source_trust_pin_is_immutable
        BEFORE UPDATE ON governed_source_trust_pins
        FOR EACH ROW BEGIN
            SELECT RAISE(ABORT, 'governed_source_trust_pin_is_immutable');
        END"#,
    r#"CREATE TRIGGER governed_source_provision_audit_is_immutable
        BEFORE UPDATE ON governed_source_provision_audits
        FOR EACH ROW BEGIN
            SELECT RAISE(ABORT, 'governed_source_provision_audit_is_immutable');
        END"#,
];

const V32_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_profile_lifecycle_impacts (
        impact_digest TEXT PRIMARY KEY CHECK(length(impact_digest) = 64 AND impact_digest NOT GLOB '*[^0-9a-f]*'),
        plan_id TEXT NOT NULL REFERENCES mcp_profile_apply_plans(plan_id) ON DELETE RESTRICT,
        plan_digest TEXT NOT NULL CHECK(length(plan_digest) = 64 AND plan_digest NOT GLOB '*[^0-9a-f]*'),
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        actor_id TEXT NOT NULL CHECK(length(actor_id) > 0),
        operation TEXT NOT NULL CHECK(operation IN ('archive')),
        expires_at_ms INTEGER NOT NULL,
        managed_revision INTEGER NOT NULL CHECK(managed_revision >= 0),
        manifest_evidence_digest TEXT NOT NULL CHECK(length(manifest_evidence_digest) = 64 AND manifest_evidence_digest NOT GLOB '*[^0-9a-f]*'),
        projection_evidence_digest TEXT NOT NULL CHECK(length(projection_evidence_digest) = 64 AND projection_evidence_digest NOT GLOB '*[^0-9a-f]*'),
        state TEXT NOT NULL CHECK(state IN ('planned','confirmed','consumed')),
        confirmed_at_ms INTEGER,
        consumed_at_ms INTEGER,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        CHECK(expires_at_ms >= created_at_ms),
        CHECK(updated_at_ms >= created_at_ms),
        CHECK(
            (state = 'planned' AND confirmed_at_ms IS NULL AND consumed_at_ms IS NULL)
            OR (
                state = 'confirmed'
                AND confirmed_at_ms IS NOT NULL
                AND confirmed_at_ms >= created_at_ms
                AND confirmed_at_ms <= expires_at_ms
                AND consumed_at_ms IS NULL
                AND updated_at_ms >= confirmed_at_ms
            )
            OR (
                state = 'consumed'
                AND confirmed_at_ms IS NOT NULL
                AND confirmed_at_ms >= created_at_ms
                AND confirmed_at_ms <= expires_at_ms
                AND consumed_at_ms IS NOT NULL
                AND consumed_at_ms >= confirmed_at_ms
                AND consumed_at_ms <= expires_at_ms
                AND updated_at_ms >= consumed_at_ms
            )
        ),
        UNIQUE(plan_id, managed_mcp_id)
    )"#,
    "CREATE INDEX mcp_profile_lifecycle_impacts_plan_id ON mcp_profile_lifecycle_impacts(plan_id)",
    "CREATE INDEX mcp_profile_lifecycle_impacts_managed_mcp_id ON mcp_profile_lifecycle_impacts(managed_mcp_id)",
    "CREATE INDEX mcp_profile_lifecycle_impacts_expires_at_ms ON mcp_profile_lifecycle_impacts(expires_at_ms)",
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_initial_state_is_planned
        BEFORE INSERT ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN NEW.state <> 'planned'
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_initial_state_must_be_planned');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_state_transition_is_monotonic
        BEFORE UPDATE OF state ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN (OLD.state = 'planned' AND NEW.state NOT IN ('planned','confirmed'))
          OR (OLD.state = 'confirmed' AND NEW.state NOT IN ('confirmed','consumed'))
          OR (OLD.state = 'consumed' AND NEW.state <> 'consumed')
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_invalid_state_transition');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic
        BEFORE UPDATE OF updated_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN NEW.updated_at_ms < OLD.updated_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_updated_at_ms_regression');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_confirmation_is_immutable
        BEFORE UPDATE OF confirmed_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN OLD.confirmed_at_ms IS NOT NULL
         AND NEW.confirmed_at_ms <> OLD.confirmed_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_confirmation_is_immutable');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_consumption_is_immutable
        BEFORE UPDATE OF consumed_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN OLD.consumed_at_ms IS NOT NULL
         AND NEW.consumed_at_ms <> OLD.consumed_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_consumption_is_immutable');
        END"#,
];

const V33_PREPARE_STATEMENTS: &[&str] = &[
    "ALTER TABLE mcp_profile_lifecycle_impacts RENAME TO mcp_profile_lifecycle_impacts_v32_legacy",
    "DROP INDEX mcp_profile_lifecycle_impacts_plan_id",
    "DROP INDEX mcp_profile_lifecycle_impacts_managed_mcp_id",
    "DROP INDEX mcp_profile_lifecycle_impacts_expires_at_ms",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_initial_state_is_planned",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_state_transition_is_monotonic",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_confirmation_is_immutable",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_consumption_is_immutable",
];

const V33_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_profile_lifecycle_impacts (
        impact_digest TEXT PRIMARY KEY CHECK(length(impact_digest) = 64 AND impact_digest NOT GLOB '*[^0-9a-f]*'),
        plan_id TEXT NOT NULL REFERENCES mcp_profile_apply_plans(plan_id) ON DELETE RESTRICT,
        plan_digest TEXT NOT NULL CHECK(length(plan_digest) = 64 AND plan_digest NOT GLOB '*[^0-9a-f]*'),
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        actor_id TEXT NOT NULL CHECK(length(actor_id) = 64 AND actor_id NOT GLOB '*[^0-9a-f]*'),
        operation TEXT NOT NULL CHECK(operation IN ('archive')),
        expires_at_ms INTEGER NOT NULL,
        managed_revision INTEGER NOT NULL CHECK(managed_revision >= 0),
        manifest_evidence_digest TEXT NOT NULL CHECK(length(manifest_evidence_digest) = 64 AND manifest_evidence_digest NOT GLOB '*[^0-9a-f]*'),
        projection_evidence_digest TEXT NOT NULL CHECK(length(projection_evidence_digest) = 64 AND projection_evidence_digest NOT GLOB '*[^0-9a-f]*'),
        state TEXT NOT NULL CHECK(state IN ('planned','confirmed','consumed')),
        confirmed_at_ms INTEGER,
        consumed_at_ms INTEGER,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        CHECK(expires_at_ms >= created_at_ms),
        CHECK(updated_at_ms >= created_at_ms),
        CHECK(
            (state = 'planned' AND confirmed_at_ms IS NULL AND consumed_at_ms IS NULL)
            OR (
                state = 'confirmed'
                AND confirmed_at_ms IS NOT NULL
                AND confirmed_at_ms >= created_at_ms
                AND confirmed_at_ms <= expires_at_ms
                AND consumed_at_ms IS NULL
                AND updated_at_ms >= confirmed_at_ms
            )
            OR (
                state = 'consumed'
                AND confirmed_at_ms IS NOT NULL
                AND confirmed_at_ms >= created_at_ms
                AND confirmed_at_ms <= expires_at_ms
                AND consumed_at_ms IS NOT NULL
                AND consumed_at_ms >= confirmed_at_ms
                AND consumed_at_ms <= expires_at_ms
                AND updated_at_ms >= consumed_at_ms
            )
        ),
        UNIQUE(plan_id, managed_mcp_id)
    )"#,
    r#"CREATE TABLE mcp_profile_lifecycle_impact_migration_audits (
        impact_digest TEXT PRIMARY KEY CHECK(length(impact_digest) = 64 AND impact_digest NOT GLOB '*[^0-9a-f]*'),
        legacy_actor_digest TEXT NOT NULL CHECK(length(legacy_actor_digest) = 64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*'),
        sanitized_actor_id TEXT NOT NULL CHECK(length(sanitized_actor_id) = 64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*'),
        status TEXT NOT NULL CHECK(status IN ('sanitized','discarded_parent_expiry')),
        migrated_at_ms INTEGER NOT NULL
    )"#,
    "CREATE INDEX mcp_profile_lifecycle_impacts_plan_id ON mcp_profile_lifecycle_impacts(plan_id)",
    "CREATE INDEX mcp_profile_lifecycle_impacts_managed_mcp_id ON mcp_profile_lifecycle_impacts(managed_mcp_id)",
    "CREATE INDEX mcp_profile_lifecycle_impacts_expires_at_ms ON mcp_profile_lifecycle_impacts(expires_at_ms)",
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_initial_state_is_planned
        BEFORE INSERT ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN NEW.state <> 'planned'
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_initial_state_must_be_planned');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_parent_expiry_on_insert
        BEFORE INSERT ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN NOT EXISTS (
            SELECT 1 FROM mcp_profile_apply_plans
            WHERE plan_id = NEW.plan_id AND expires_at_ms >= NEW.expires_at_ms
        )
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_expiry_exceeds_parent_plan');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_state_transition_is_monotonic
        BEFORE UPDATE OF state ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN (OLD.state = 'planned' AND NEW.state NOT IN ('planned','confirmed'))
          OR (OLD.state = 'confirmed' AND NEW.state NOT IN ('confirmed','consumed'))
          OR (OLD.state = 'consumed' AND NEW.state <> 'consumed')
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_invalid_state_transition');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_parent_expiry_on_transition
        BEFORE UPDATE OF state,confirmed_at_ms,consumed_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN NOT EXISTS (
            SELECT 1 FROM mcp_profile_apply_plans
            WHERE plan_id = NEW.plan_id
              AND expires_at_ms >= NEW.expires_at_ms
              AND (NEW.confirmed_at_ms IS NULL OR expires_at_ms >= NEW.confirmed_at_ms)
              AND (NEW.consumed_at_ms IS NULL OR expires_at_ms >= NEW.consumed_at_ms)
        )
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_parent_plan_expired');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_actor_is_immutable
        BEFORE UPDATE OF actor_id ON mcp_profile_lifecycle_impacts
        FOR EACH ROW BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_actor_is_immutable');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_expiry_is_immutable
        BEFORE UPDATE OF expires_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_expiry_is_immutable');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_created_at_is_immutable
        BEFORE UPDATE OF created_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_created_at_is_immutable');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic
        BEFORE UPDATE OF updated_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN NEW.updated_at_ms < OLD.updated_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_updated_at_ms_regression');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_confirmation_is_immutable
        BEFORE UPDATE OF confirmed_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN OLD.confirmed_at_ms IS NOT NULL
         AND NEW.confirmed_at_ms <> OLD.confirmed_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_confirmation_is_immutable');
        END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_consumption_is_immutable
        BEFORE UPDATE OF consumed_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW
        WHEN OLD.consumed_at_ms IS NOT NULL
         AND NEW.consumed_at_ms <> OLD.consumed_at_ms
        BEGIN
            SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_consumption_is_immutable');
        END"#,
];

const V34_PREPARE_STATEMENTS: &[&str] = &[
    "ALTER TABLE mcp_profile_lifecycle_impacts RENAME TO mcp_profile_lifecycle_impacts_v33_legacy",
    "DROP INDEX mcp_profile_lifecycle_impacts_plan_id",
    "DROP INDEX mcp_profile_lifecycle_impacts_managed_mcp_id",
    "DROP INDEX mcp_profile_lifecycle_impacts_expires_at_ms",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_initial_state_is_planned",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_parent_expiry_on_insert",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_state_transition_is_monotonic",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_parent_expiry_on_transition",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_actor_is_immutable",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_expiry_is_immutable",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_created_at_is_immutable",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_confirmation_is_immutable",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_consumption_is_immutable",
    "ALTER TABLE mcp_profile_lifecycle_impact_migration_audits RENAME TO mcp_profile_lifecycle_impact_migration_audits_v33_legacy",
];

const V34_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_profile_lifecycle_impacts (
        impact_digest TEXT PRIMARY KEY CHECK(typeof(impact_digest)='text' AND length(impact_digest)=64 AND impact_digest NOT GLOB '*[^0-9a-f]*'),
        plan_id TEXT NOT NULL REFERENCES mcp_profile_apply_plans(plan_id) ON DELETE RESTRICT,
        plan_digest TEXT NOT NULL CHECK(typeof(plan_digest)='text' AND length(plan_digest)=64 AND plan_digest NOT GLOB '*[^0-9a-f]*'),
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        actor_id TEXT NOT NULL CHECK(typeof(actor_id)='text' AND length(actor_id)=64 AND actor_id NOT GLOB '*[^0-9a-f]*'),
        operation TEXT NOT NULL CHECK(operation IN ('archive')),
        expires_at_ms INTEGER NOT NULL,
        managed_revision INTEGER NOT NULL CHECK(managed_revision >= 0),
        manifest_evidence_digest TEXT NOT NULL CHECK(typeof(manifest_evidence_digest)='text' AND length(manifest_evidence_digest)=64 AND manifest_evidence_digest NOT GLOB '*[^0-9a-f]*'),
        projection_evidence_digest TEXT NOT NULL CHECK(typeof(projection_evidence_digest)='text' AND length(projection_evidence_digest)=64 AND projection_evidence_digest NOT GLOB '*[^0-9a-f]*'),
        state TEXT NOT NULL CHECK(state IN ('planned','confirmed','consumed')),
        confirmed_at_ms INTEGER,
        consumed_at_ms INTEGER,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        CHECK(expires_at_ms >= created_at_ms),
        CHECK(updated_at_ms >= created_at_ms),
        CHECK(
            (state = 'planned' AND confirmed_at_ms IS NULL AND consumed_at_ms IS NULL)
            OR (state = 'confirmed' AND confirmed_at_ms IS NOT NULL AND confirmed_at_ms >= created_at_ms AND confirmed_at_ms <= expires_at_ms AND consumed_at_ms IS NULL AND updated_at_ms >= confirmed_at_ms)
            OR (state = 'consumed' AND confirmed_at_ms IS NOT NULL AND confirmed_at_ms >= created_at_ms AND confirmed_at_ms <= expires_at_ms AND consumed_at_ms IS NOT NULL AND consumed_at_ms >= confirmed_at_ms AND consumed_at_ms <= expires_at_ms AND updated_at_ms >= consumed_at_ms)
        ),
        UNIQUE(plan_id, managed_mcp_id)
    )"#,
    r#"CREATE TABLE mcp_profile_lifecycle_impact_migration_audits (
        impact_digest TEXT PRIMARY KEY CHECK(typeof(impact_digest)='text' AND length(impact_digest)=64 AND impact_digest NOT GLOB '*[^0-9a-f]*'),
        legacy_actor_digest TEXT NOT NULL CHECK(typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*'),
        sanitized_actor_id TEXT NOT NULL CHECK(typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*'),
        status TEXT NOT NULL CHECK(status IN ('sanitized','discarded_parent_expiry')),
        migrated_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE mcp_profile_lifecycle_impact_v34_migration_audits (
        audit_digest TEXT PRIMARY KEY CHECK(typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*'),
        legacy_actor_digest TEXT NOT NULL CHECK(typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*'),
        sanitized_actor_id TEXT NOT NULL CHECK(typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*'),
        status TEXT NOT NULL CHECK(status IN ('discarded_integrity')),
        migrated_at_ms INTEGER NOT NULL
    )"#,
    "CREATE INDEX mcp_profile_lifecycle_impacts_plan_id ON mcp_profile_lifecycle_impacts(plan_id)",
    "CREATE INDEX mcp_profile_lifecycle_impacts_managed_mcp_id ON mcp_profile_lifecycle_impacts(managed_mcp_id)",
    "CREATE INDEX mcp_profile_lifecycle_impacts_expires_at_ms ON mcp_profile_lifecycle_impacts(expires_at_ms)",
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_initial_state_is_planned
        BEFORE INSERT ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN NEW.state <> 'planned'
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_initial_state_must_be_planned'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_insert
        BEFORE INSERT ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN NOT EXISTS (
            SELECT 1 FROM mcp_profile_apply_plans
            WHERE plan_id=NEW.plan_id AND plan_digest=NEW.plan_digest AND expires_at_ms>=NEW.expires_at_ms
        )
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_parent_commitment_mismatch'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_state_transition_is_monotonic
        BEFORE UPDATE OF state ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN (OLD.state='planned' AND NEW.state NOT IN ('planned','confirmed'))
          OR (OLD.state='confirmed' AND NEW.state NOT IN ('confirmed','consumed'))
          OR (OLD.state='consumed' AND NEW.state <> 'consumed')
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_invalid_state_transition'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_transition
        BEFORE UPDATE OF state,confirmed_at_ms,consumed_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN NOT EXISTS (
            SELECT 1 FROM mcp_profile_apply_plans
            WHERE plan_id=NEW.plan_id AND plan_digest=NEW.plan_digest AND expires_at_ms>=NEW.expires_at_ms
              AND (NEW.confirmed_at_ms IS NULL OR expires_at_ms>=NEW.confirmed_at_ms)
              AND (NEW.consumed_at_ms IS NULL OR expires_at_ms>=NEW.consumed_at_ms)
        )
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_parent_commitment_mismatch'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_commitments_are_immutable
        BEFORE UPDATE OF impact_digest,plan_id,plan_digest,managed_mcp_id,actor_id,operation,expires_at_ms,managed_revision,manifest_evidence_digest,projection_evidence_digest,created_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_commitments_are_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic
        BEFORE UPDATE OF updated_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN NEW.updated_at_ms < OLD.updated_at_ms
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_updated_at_ms_regression'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_confirmation_is_immutable
        BEFORE UPDATE OF confirmed_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN OLD.confirmed_at_ms IS NOT NULL AND NEW.confirmed_at_ms <> OLD.confirmed_at_ms
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_confirmation_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_consumption_is_immutable
        BEFORE UPDATE OF consumed_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN OLD.consumed_at_ms IS NOT NULL AND NEW.consumed_at_ms <> OLD.consumed_at_ms
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_consumption_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_apply_plans_commitments_are_immutable_after_impact
        BEFORE UPDATE OF plan_id,profile_id,profile_revision,plan_digest,snapshot_json,actor,expires_at_ms,idempotency_key,request_digest,created_at_ms ON mcp_profile_apply_plans
        FOR EACH ROW WHEN EXISTS (SELECT 1 FROM mcp_profile_lifecycle_impacts WHERE plan_id=OLD.plan_id)
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_apply_plans_commitments_are_immutable_after_impact'); END"#,
];

const V35_PREPARE_STATEMENTS: &[&str] = &[
    "ALTER TABLE mcp_profile_lifecycle_impacts RENAME TO mcp_profile_lifecycle_impacts_v34_legacy",
    "DROP INDEX mcp_profile_lifecycle_impacts_plan_id",
    "DROP INDEX mcp_profile_lifecycle_impacts_managed_mcp_id",
    "DROP INDEX mcp_profile_lifecycle_impacts_expires_at_ms",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_initial_state_is_planned",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_insert",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_state_transition_is_monotonic",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_transition",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_commitments_are_immutable",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_confirmation_is_immutable",
    "DROP TRIGGER mcp_profile_lifecycle_impacts_consumption_is_immutable",
    "DROP TRIGGER mcp_profile_apply_plans_commitments_are_immutable_after_impact",
];

const V35_STATEMENTS: &[&str] = &[
    V34_STATEMENTS[0],
    r#"CREATE TABLE mcp_profile_lifecycle_impact_v35_migration_audits (
        audit_digest TEXT PRIMARY KEY CHECK(typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*'),
        legacy_actor_digest TEXT NOT NULL CHECK(typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*'),
        sanitized_actor_id TEXT NOT NULL CHECK(typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*'),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status IN ('discarded_integrity','discarded_expired')),
        migrated_at_ms INTEGER NOT NULL CHECK(typeof(migrated_at_ms)='integer' AND migrated_at_ms >= 0)
    )"#,
    V34_STATEMENTS[3],
    V34_STATEMENTS[4],
    V34_STATEMENTS[5],
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_conflicting_insert_is_rejected
        BEFORE INSERT ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN EXISTS (
            SELECT 1 FROM mcp_profile_lifecycle_impacts
            WHERE impact_digest=NEW.impact_digest
               OR (plan_id=NEW.plan_id AND managed_mcp_id=NEW.managed_mcp_id)
        )
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_conflicting_insert_is_rejected'); END"#,
    V34_STATEMENTS[6],
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_insert
        BEFORE INSERT ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN NOT EXISTS (
            SELECT 1 FROM mcp_profile_apply_plans
            WHERE plan_id=NEW.plan_id AND plan_digest=NEW.plan_digest
              AND expires_at_ms>=NEW.expires_at_ms
              AND expires_at_ms>CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
        ) OR NEW.expires_at_ms<=CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_parent_commitment_mismatch'); END"#,
    V34_STATEMENTS[8],
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_parent_commitment_on_transition
        BEFORE UPDATE OF state,confirmed_at_ms,consumed_at_ms ON mcp_profile_lifecycle_impacts
        FOR EACH ROW WHEN NOT EXISTS (
            SELECT 1 FROM mcp_profile_apply_plans
            WHERE plan_id=NEW.plan_id AND plan_digest=NEW.plan_digest
              AND expires_at_ms>=NEW.expires_at_ms
              AND expires_at_ms>CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
              AND (NEW.confirmed_at_ms IS NULL OR expires_at_ms>=NEW.confirmed_at_ms)
              AND (NEW.consumed_at_ms IS NULL OR expires_at_ms>=NEW.consumed_at_ms)
        ) OR NEW.expires_at_ms<=CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_parent_commitment_mismatch'); END"#,
    V34_STATEMENTS[10],
    V34_STATEMENTS[11],
    V34_STATEMENTS[12],
    V34_STATEMENTS[13],
    V34_STATEMENTS[14],
    r#"CREATE TRIGGER mcp_profile_lifecycle_impacts_delete_is_rejected
        BEFORE DELETE ON mcp_profile_lifecycle_impacts
        FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_impacts_delete_is_rejected'); END"#,
    r#"CREATE TRIGGER mcp_profile_apply_plans_delete_is_rejected_after_impact
        BEFORE DELETE ON mcp_profile_apply_plans
        FOR EACH ROW WHEN EXISTS (SELECT 1 FROM mcp_profile_lifecycle_impacts WHERE plan_id=OLD.plan_id)
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_apply_plans_delete_is_rejected_after_impact'); END"#,
    r#"CREATE TRIGGER mcp_profile_apply_plans_conflicting_insert_is_rejected_after_impact
        BEFORE INSERT ON mcp_profile_apply_plans
        FOR EACH ROW WHEN EXISTS (
            SELECT 1 FROM mcp_profile_apply_plans plan
            JOIN mcp_profile_lifecycle_impacts impact ON impact.plan_id=plan.plan_id
            WHERE plan.plan_id=NEW.plan_id OR plan.idempotency_key=NEW.idempotency_key
        )
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_apply_plans_conflicting_insert_is_rejected_after_impact'); END"#,
];

const V36_PREPARE_STATEMENTS: &[&str] = &[
    "ALTER TABLE mcp_profile_lifecycle_impact_migration_audits RENAME TO mcp_profile_lifecycle_impact_migration_audits_v35_legacy",
    "ALTER TABLE mcp_profile_lifecycle_impact_v34_migration_audits RENAME TO mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy",
    "ALTER TABLE mcp_profile_lifecycle_impact_v35_migration_audits RENAME TO mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy",
];

const V36_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_profile_lifecycle_impact_migration_audits (
        impact_digest TEXT PRIMARY KEY CHECK(typeof(impact_digest)='text' AND length(impact_digest)=64 AND impact_digest NOT GLOB '*[^0-9a-f]*'),
        legacy_actor_digest TEXT NOT NULL CHECK(typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*'),
        sanitized_actor_id TEXT NOT NULL CHECK(typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*'),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status IN ('sanitized','discarded_parent_expiry')),
        migrated_at_ms BLOB NOT NULL CHECK(typeof(migrated_at_ms)='integer' AND migrated_at_ms >= 0)
    )"#,
    r#"CREATE TABLE mcp_profile_lifecycle_impact_v34_migration_audits (
        audit_digest TEXT PRIMARY KEY CHECK(typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*'),
        legacy_actor_digest TEXT NOT NULL CHECK(typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*'),
        sanitized_actor_id TEXT NOT NULL CHECK(typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*'),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status='discarded_integrity'),
        migrated_at_ms BLOB NOT NULL CHECK(typeof(migrated_at_ms)='integer' AND migrated_at_ms >= 0)
    )"#,
    r#"CREATE TABLE mcp_profile_lifecycle_impact_v35_migration_audits (
        audit_digest TEXT PRIMARY KEY CHECK(typeof(audit_digest)='text' AND length(audit_digest)=64 AND audit_digest NOT GLOB '*[^0-9a-f]*'),
        legacy_actor_digest TEXT NOT NULL CHECK(typeof(legacy_actor_digest)='text' AND length(legacy_actor_digest)=64 AND legacy_actor_digest NOT GLOB '*[^0-9a-f]*'),
        sanitized_actor_id TEXT NOT NULL CHECK(typeof(sanitized_actor_id)='text' AND length(sanitized_actor_id)=64 AND sanitized_actor_id NOT GLOB '*[^0-9a-f]*'),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status IN ('discarded_integrity','discarded_expired')),
        migrated_at_ms BLOB NOT NULL CHECK(typeof(migrated_at_ms)='integer' AND migrated_at_ms >= 0)
    )"#,
    r#"CREATE TABLE mcp_profile_lifecycle_impact_v36_migration_dispositions (
        disposition_digest TEXT PRIMARY KEY CHECK(typeof(disposition_digest)='text' AND length(disposition_digest)=64 AND disposition_digest NOT GLOB '*[^0-9a-f]*'),
        migration_domain TEXT NOT NULL CHECK(typeof(migration_domain)='text' AND migration_domain IN ('profile_lifecycle_v33_audit','profile_lifecycle_v34_audit','profile_lifecycle_v35_audit')),
        source_rowid BLOB NOT NULL CHECK(typeof(source_rowid)='integer' AND source_rowid >= 1),
        legacy_row_type TEXT NOT NULL CHECK(typeof(legacy_row_type)='text' AND legacy_row_type GLOB 'invalid_audit_status_*_time_*'),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status='discarded_legacy_audit'),
        migrated_at_ms BLOB NOT NULL CHECK(typeof(migrated_at_ms)='integer' AND migrated_at_ms >= 0),
        UNIQUE(migration_domain, source_rowid, legacy_row_type)
    )"#,
];

const V37_DISPOSITION_STATEMENT: &str = r#"CREATE TABLE mcp_profile_lifecycle_impact_v36_migration_dispositions (
        disposition_digest TEXT PRIMARY KEY CHECK(typeof(disposition_digest)='text' AND length(disposition_digest)=64 AND disposition_digest NOT GLOB '*[^0-9a-f]*'),
        migration_domain TEXT NOT NULL CHECK(typeof(migration_domain)='text' AND migration_domain IN ('profile_lifecycle_v33_audit','profile_lifecycle_v34_audit','profile_lifecycle_v35_audit')),
        source_table TEXT NOT NULL CHECK(typeof(source_table)='text' AND ((migration_domain='profile_lifecycle_v33_audit' AND source_table='mcp_profile_lifecycle_impact_migration_audits_v35_legacy') OR (migration_domain='profile_lifecycle_v34_audit' AND source_table='mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy') OR (migration_domain='profile_lifecycle_v35_audit' AND source_table='mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy'))),
        source_rowid BLOB NOT NULL CHECK(typeof(source_rowid)='integer'),
        legacy_row_type TEXT NOT NULL CHECK(typeof(legacy_row_type)='text' AND legacy_row_type GLOB 'invalid_audit_status_*_time_*'),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status='discarded_legacy_audit'),
        migrated_at_ms BLOB NOT NULL CHECK(typeof(migrated_at_ms)='integer' AND migrated_at_ms >= 0),
        UNIQUE(migration_domain, source_table, source_rowid, legacy_row_type)
    )"#;

const V37_ATTESTATION_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_profile_lifecycle_audit_integrity_attestations (
        attestation_id TEXT PRIMARY KEY CHECK(typeof(attestation_id)='text' AND attestation_id='profile_lifecycle_v35_invalid_audit'),
        migration_domain TEXT NOT NULL CHECK(typeof(migration_domain)='text' AND migration_domain='profile_lifecycle_v35_invalid_audit'),
        version_from INTEGER NOT NULL CHECK(typeof(version_from)='integer' AND version_from=35),
        version_through INTEGER NOT NULL CHECK(typeof(version_through)='integer' AND version_through=35),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status='unverifiable_legacy_aggregation'),
        observed_record_count INTEGER NOT NULL CHECK(typeof(observed_record_count)='integer' AND observed_record_count>=1),
        created_at_ms BLOB NOT NULL CHECK(typeof(created_at_ms)='integer' AND created_at_ms>=0)
    )"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_audit_integrity_attestation_is_immutable
        BEFORE UPDATE ON mcp_profile_lifecycle_audit_integrity_attestations
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_audit_integrity_attestation_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_deleted
        BEFORE DELETE ON mcp_profile_lifecycle_audit_integrity_attestations
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_deleted'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_replaced
        BEFORE INSERT ON mcp_profile_lifecycle_audit_integrity_attestations
        WHEN EXISTS (SELECT 1 FROM mcp_profile_lifecycle_audit_integrity_attestations WHERE attestation_id=NEW.attestation_id)
        BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_replaced'); END"#,
];

const V38_DISPOSITION_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_profile_lifecycle_impact_v36_migration_dispositions (
        disposition_digest TEXT PRIMARY KEY CHECK(typeof(disposition_digest)='text' AND length(disposition_digest)=64 AND disposition_digest NOT GLOB '*[^0-9a-f]*'),
        migration_domain TEXT NOT NULL CHECK(typeof(migration_domain)='text' AND migration_domain IN ('profile_lifecycle_v33_audit','profile_lifecycle_v34_audit','profile_lifecycle_v35_audit')),
        source_table TEXT NOT NULL CHECK(typeof(source_table)='text' AND ((migration_domain='profile_lifecycle_v33_audit' AND source_table='mcp_profile_lifecycle_impact_migration_audits_v35_legacy') OR (migration_domain='profile_lifecycle_v34_audit' AND source_table='mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy') OR (migration_domain='profile_lifecycle_v35_audit' AND source_table='mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy'))),
        source_rowid BLOB NOT NULL CHECK(typeof(source_rowid)='integer' AND source_rowid >= 1),
        legacy_row_type TEXT NOT NULL CHECK(typeof(legacy_row_type)='text' AND legacy_row_type GLOB 'invalid_audit_status_*_time_*'),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status='discarded_legacy_audit'),
        migrated_at_ms BLOB NOT NULL CHECK(typeof(migrated_at_ms)='integer' AND migrated_at_ms >= 0),
        UNIQUE(migration_domain, source_table, source_rowid, legacy_row_type)
    )"#,
    r#"CREATE TABLE mcp_profile_lifecycle_impact_v38_disposition_canonicals (
        disposition_digest TEXT PRIMARY KEY CHECK(typeof(disposition_digest)='text' AND length(disposition_digest)=64 AND disposition_digest NOT GLOB '*[^0-9a-f]*'),
        migration_domain TEXT NOT NULL CHECK(typeof(migration_domain)='text' AND migration_domain IN ('profile_lifecycle_v33_audit','profile_lifecycle_v34_audit','profile_lifecycle_v35_audit')),
        source_table TEXT NOT NULL CHECK(typeof(source_table)='text' AND ((migration_domain='profile_lifecycle_v33_audit' AND source_table='mcp_profile_lifecycle_impact_migration_audits_v35_legacy') OR (migration_domain='profile_lifecycle_v34_audit' AND source_table='mcp_profile_lifecycle_impact_v34_migration_audits_v35_legacy') OR (migration_domain='profile_lifecycle_v35_audit' AND source_table='mcp_profile_lifecycle_impact_v35_migration_audits_v35_legacy'))),
        source_rowid BLOB NOT NULL CHECK(typeof(source_rowid)='integer' AND source_rowid >= 1),
        legacy_row_type TEXT NOT NULL CHECK(typeof(legacy_row_type)='text' AND legacy_row_type GLOB 'invalid_audit_status_*_time_*'),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status='discarded_legacy_audit'),
        migrated_at_ms BLOB NOT NULL CHECK(typeof(migrated_at_ms)='integer' AND migrated_at_ms >= 0),
        UNIQUE(migration_domain, source_table, source_rowid, legacy_row_type)
    )"#,
];

const V38_ATTESTATION_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_profile_lifecycle_audit_integrity_attestations (
        attestation_id TEXT PRIMARY KEY CHECK(typeof(attestation_id)='text' AND attestation_id='profile_lifecycle_v35_invalid_audit'),
        migration_domain TEXT NOT NULL CHECK(typeof(migration_domain)='text' AND migration_domain='profile_lifecycle_v35_invalid_audit'),
        version_from BLOB NOT NULL CHECK(typeof(version_from)='integer' AND version_from=35),
        version_through BLOB NOT NULL CHECK(typeof(version_through)='integer' AND version_through=35),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status='unverifiable_legacy_aggregation'),
        observed_record_count BLOB NOT NULL CHECK(typeof(observed_record_count)='integer' AND observed_record_count>=1),
        created_at_ms BLOB NOT NULL CHECK(typeof(created_at_ms)='integer' AND created_at_ms>=0)
    )"#,
    r#"CREATE TABLE mcp_profile_lifecycle_audit_integrity_attestation_canonicals (
        attestation_id TEXT PRIMARY KEY CHECK(typeof(attestation_id)='text' AND attestation_id='profile_lifecycle_v35_invalid_audit'),
        migration_domain TEXT NOT NULL CHECK(typeof(migration_domain)='text' AND migration_domain='profile_lifecycle_v35_invalid_audit'),
        version_from BLOB NOT NULL CHECK(typeof(version_from)='integer' AND version_from=35),
        version_through BLOB NOT NULL CHECK(typeof(version_through)='integer' AND version_through=35),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status='unverifiable_legacy_aggregation'),
        observed_record_count BLOB NOT NULL CHECK(typeof(observed_record_count)='integer' AND observed_record_count>=1),
        created_at_ms BLOB NOT NULL CHECK(typeof(created_at_ms)='integer' AND created_at_ms>=0)
    )"#,
];

const V38_IMMUTABILITY_TRIGGERS: &[&str] = &[
    r#"CREATE TRIGGER mcp_profile_lifecycle_v38_disposition_cannot_be_inserted BEFORE INSERT ON mcp_profile_lifecycle_impact_v36_migration_dispositions BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_v38_disposition_is_migration_sealed'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_v38_disposition_cannot_be_updated BEFORE UPDATE ON mcp_profile_lifecycle_impact_v36_migration_dispositions BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_v38_disposition_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_v38_disposition_cannot_be_deleted BEFORE DELETE ON mcp_profile_lifecycle_impact_v36_migration_dispositions BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_v38_disposition_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_v38_disposition_canonical_cannot_be_inserted BEFORE INSERT ON mcp_profile_lifecycle_impact_v38_disposition_canonicals BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_v38_disposition_canonical_is_migration_sealed'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_v38_disposition_canonical_cannot_be_updated BEFORE UPDATE ON mcp_profile_lifecycle_impact_v38_disposition_canonicals BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_v38_disposition_canonical_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_v38_disposition_canonical_cannot_be_deleted BEFORE DELETE ON mcp_profile_lifecycle_impact_v38_disposition_canonicals BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_v38_disposition_canonical_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_inserted BEFORE INSERT ON mcp_profile_lifecycle_audit_integrity_attestations BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_audit_integrity_attestation_is_migration_sealed'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_updated BEFORE UPDATE ON mcp_profile_lifecycle_audit_integrity_attestations BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_audit_integrity_attestation_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_deleted BEFORE DELETE ON mcp_profile_lifecycle_audit_integrity_attestations BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_audit_integrity_attestation_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_audit_integrity_attestation_canonical_cannot_be_inserted BEFORE INSERT ON mcp_profile_lifecycle_audit_integrity_attestation_canonicals BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_audit_integrity_attestation_canonical_is_migration_sealed'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_audit_integrity_attestation_canonical_cannot_be_updated BEFORE UPDATE ON mcp_profile_lifecycle_audit_integrity_attestation_canonicals BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_audit_integrity_attestation_canonical_is_immutable'); END"#,
    r#"CREATE TRIGGER mcp_profile_lifecycle_audit_integrity_attestation_canonical_cannot_be_deleted BEFORE DELETE ON mcp_profile_lifecycle_audit_integrity_attestation_canonicals BEGIN SELECT RAISE(ABORT, 'mcp_profile_lifecycle_audit_integrity_attestation_canonical_is_immutable'); END"#,
];

const V29_GOVERNED_SOURCE_REFRESH_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "governed_source_refresh_registrations",
        create_sql: V29_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "governed_source_refresh_audits",
        create_sql: V29_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "governed_source_refresh_audits_source_time",
        create_sql: V29_STATEMENTS[2],
    },
];

const V30_GOVERNED_SOURCE_REFRESH_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "governed_source_refresh_trust_anchors",
        create_sql: V30_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "governed_source_refresh_anchor_endpoint_matches_registration",
        create_sql: V30_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "governed_source_refresh_registration_endpoint_is_immutable",
        create_sql: V30_STATEMENTS[2],
    },
];

const V31_SOURCE_PROVISIONING_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "governed_source_trust_pins",
        create_sql: V31_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "governed_source_provision_audits",
        create_sql: V31_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "governed_source_refresh_anchor_requires_user_pin",
        create_sql: V31_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "governed_source_trust_pin_is_immutable",
        create_sql: V31_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "governed_source_provision_audit_is_immutable",
        create_sql: V31_STATEMENTS[4],
    },
];

const V32_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impacts",
        create_sql: V32_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_plan_id",
        create_sql: V32_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_managed_mcp_id",
        create_sql: V32_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_expires_at_ms",
        create_sql: V32_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_initial_state_is_planned",
        create_sql: V32_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_state_transition_is_monotonic",
        create_sql: V32_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic",
        create_sql: V32_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_confirmation_is_immutable",
        create_sql: V32_STATEMENTS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_consumption_is_immutable",
        create_sql: V32_STATEMENTS[8],
    },
];

const V33_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impacts",
        create_sql: V33_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_migration_audits",
        create_sql: V33_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_plan_id",
        create_sql: V33_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_managed_mcp_id",
        create_sql: V33_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_expires_at_ms",
        create_sql: V33_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_initial_state_is_planned",
        create_sql: V33_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_parent_expiry_on_insert",
        create_sql: V33_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_state_transition_is_monotonic",
        create_sql: V33_STATEMENTS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_parent_expiry_on_transition",
        create_sql: V33_STATEMENTS[8],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_actor_is_immutable",
        create_sql: V33_STATEMENTS[9],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_expiry_is_immutable",
        create_sql: V33_STATEMENTS[10],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_created_at_is_immutable",
        create_sql: V33_STATEMENTS[11],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic",
        create_sql: V33_STATEMENTS[12],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_confirmation_is_immutable",
        create_sql: V33_STATEMENTS[13],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_consumption_is_immutable",
        create_sql: V33_STATEMENTS[14],
    },
];

const V34_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impacts",
        create_sql: V34_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_migration_audits",
        create_sql: V34_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_v34_migration_audits",
        create_sql: V34_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_plan_id",
        create_sql: V34_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_managed_mcp_id",
        create_sql: V34_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_expires_at_ms",
        create_sql: V34_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_initial_state_is_planned",
        create_sql: V34_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_parent_commitment_on_insert",
        create_sql: V34_STATEMENTS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_state_transition_is_monotonic",
        create_sql: V34_STATEMENTS[8],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_parent_commitment_on_transition",
        create_sql: V34_STATEMENTS[9],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_commitments_are_immutable",
        create_sql: V34_STATEMENTS[10],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic",
        create_sql: V34_STATEMENTS[11],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_confirmation_is_immutable",
        create_sql: V34_STATEMENTS[12],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_consumption_is_immutable",
        create_sql: V34_STATEMENTS[13],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_apply_plans_commitments_are_immutable_after_impact",
        create_sql: V34_STATEMENTS[14],
    },
];

const V35_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impacts",
        create_sql: V35_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_migration_audits",
        create_sql: V34_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_v34_migration_audits",
        create_sql: V34_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_v35_migration_audits",
        create_sql: V35_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_plan_id",
        create_sql: V35_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_managed_mcp_id",
        create_sql: V35_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_expires_at_ms",
        create_sql: V35_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_conflicting_insert_is_rejected",
        create_sql: V35_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_initial_state_is_planned",
        create_sql: V35_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_parent_commitment_on_insert",
        create_sql: V35_STATEMENTS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_state_transition_is_monotonic",
        create_sql: V35_STATEMENTS[8],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_parent_commitment_on_transition",
        create_sql: V35_STATEMENTS[9],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_commitments_are_immutable",
        create_sql: V35_STATEMENTS[10],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic",
        create_sql: V35_STATEMENTS[11],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_confirmation_is_immutable",
        create_sql: V35_STATEMENTS[12],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_consumption_is_immutable",
        create_sql: V35_STATEMENTS[13],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_apply_plans_commitments_are_immutable_after_impact",
        create_sql: V35_STATEMENTS[14],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_delete_is_rejected",
        create_sql: V35_STATEMENTS[15],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_apply_plans_delete_is_rejected_after_impact",
        create_sql: V35_STATEMENTS[16],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_apply_plans_conflicting_insert_is_rejected_after_impact",
        create_sql: V35_STATEMENTS[17],
    },
];

const V36_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impacts",
        create_sql: V35_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_migration_audits",
        create_sql: V36_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_v34_migration_audits",
        create_sql: V36_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_v35_migration_audits",
        create_sql: V36_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_v36_migration_dispositions",
        create_sql: V36_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_plan_id",
        create_sql: V35_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_managed_mcp_id",
        create_sql: V35_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_profile_lifecycle_impacts_expires_at_ms",
        create_sql: V35_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_conflicting_insert_is_rejected",
        create_sql: V35_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_initial_state_is_planned",
        create_sql: V35_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_parent_commitment_on_insert",
        create_sql: V35_STATEMENTS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_state_transition_is_monotonic",
        create_sql: V35_STATEMENTS[8],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_parent_commitment_on_transition",
        create_sql: V35_STATEMENTS[9],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_commitments_are_immutable",
        create_sql: V35_STATEMENTS[10],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_updated_at_ms_is_monotonic",
        create_sql: V35_STATEMENTS[11],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_confirmation_is_immutable",
        create_sql: V35_STATEMENTS[12],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_consumption_is_immutable",
        create_sql: V35_STATEMENTS[13],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_apply_plans_commitments_are_immutable_after_impact",
        create_sql: V35_STATEMENTS[14],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_impacts_delete_is_rejected",
        create_sql: V35_STATEMENTS[15],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_apply_plans_delete_is_rejected_after_impact",
        create_sql: V35_STATEMENTS[16],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_apply_plans_conflicting_insert_is_rejected_after_impact",
        create_sql: V35_STATEMENTS[17],
    },
];

const V37_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_v36_migration_dispositions",
        create_sql: V37_DISPOSITION_STATEMENT,
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_audit_integrity_attestations",
        create_sql: V37_ATTESTATION_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_is_immutable",
        create_sql: V37_ATTESTATION_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_deleted",
        create_sql: V37_ATTESTATION_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_replaced",
        create_sql: V37_ATTESTATION_STATEMENTS[3],
    },
];

const V38_PROFILE_LIFECYCLE_IMPACT_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_v36_migration_dispositions",
        create_sql: V38_DISPOSITION_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_impact_v38_disposition_canonicals",
        create_sql: V38_DISPOSITION_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_audit_integrity_attestations",
        create_sql: V38_ATTESTATION_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_canonicals",
        create_sql: V38_ATTESTATION_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_v38_disposition_cannot_be_inserted",
        create_sql: V38_IMMUTABILITY_TRIGGERS[0],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_v38_disposition_cannot_be_updated",
        create_sql: V38_IMMUTABILITY_TRIGGERS[1],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_v38_disposition_cannot_be_deleted",
        create_sql: V38_IMMUTABILITY_TRIGGERS[2],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_v38_disposition_canonical_cannot_be_inserted",
        create_sql: V38_IMMUTABILITY_TRIGGERS[3],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_v38_disposition_canonical_cannot_be_updated",
        create_sql: V38_IMMUTABILITY_TRIGGERS[4],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_v38_disposition_canonical_cannot_be_deleted",
        create_sql: V38_IMMUTABILITY_TRIGGERS[5],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_inserted",
        create_sql: V38_IMMUTABILITY_TRIGGERS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_updated",
        create_sql: V38_IMMUTABILITY_TRIGGERS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_cannot_be_deleted",
        create_sql: V38_IMMUTABILITY_TRIGGERS[8],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_canonical_cannot_be_inserted",
        create_sql: V38_IMMUTABILITY_TRIGGERS[9],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_canonical_cannot_be_updated",
        create_sql: V38_IMMUTABILITY_TRIGGERS[10],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_profile_lifecycle_audit_integrity_attestation_canonical_cannot_be_deleted",
        create_sql: V38_IMMUTABILITY_TRIGGERS[11],
    },
];

const V39_EFFECT_FENCE_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE effect_fences (
        scope TEXT NOT NULL CHECK(typeof(scope)='text' AND scope='global'),
        epoch INTEGER NOT NULL CHECK(typeof(epoch)='integer' AND epoch>=0),
        canonical_digest TEXT NOT NULL CHECK(typeof(canonical_digest)='text' AND length(canonical_digest)=64 AND canonical_digest NOT GLOB '*[^0-9a-f]*'),
        created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer' AND created_at_ms>=0),
        PRIMARY KEY(scope,epoch)
    ) STRICT"#,
    r#"CREATE TABLE effect_grants (
        grant_id TEXT PRIMARY KEY CHECK(typeof(grant_id)='text' AND length(grant_id)=36 AND grant_id NOT GLOB '*[^0-9a-f-]*' AND substr(grant_id,9,1)='-' AND substr(grant_id,14,1)='-' AND substr(grant_id,19,1)='-' AND substr(grant_id,24,1)='-'),
        effect_id TEXT NOT NULL UNIQUE CHECK(typeof(effect_id)='text' AND length(effect_id)=36 AND effect_id NOT GLOB '*[^0-9a-f-]*' AND substr(effect_id,9,1)='-' AND substr(effect_id,14,1)='-' AND substr(effect_id,19,1)='-' AND substr(effect_id,24,1)='-'),
        fence_scope TEXT NOT NULL CHECK(typeof(fence_scope)='text' AND fence_scope='global'),
        fence_epoch INTEGER NOT NULL CHECK(typeof(fence_epoch)='integer' AND fence_epoch>=0),
        target_digest TEXT NOT NULL CHECK(typeof(target_digest)='text' AND length(target_digest)=64 AND target_digest NOT GLOB '*[^0-9a-f]*'),
        canonical_digest TEXT NOT NULL CHECK(typeof(canonical_digest)='text' AND length(canonical_digest)=64 AND canonical_digest NOT GLOB '*[^0-9a-f]*'),
        nonce_hash BLOB NOT NULL CHECK(typeof(nonce_hash)='blob' AND length(nonce_hash)=32),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status IN ('issued','consuming','applied','unknown','abandoned')),
        state_epoch INTEGER NOT NULL CHECK(typeof(state_epoch)='integer' AND state_epoch>=0),
        issued_at_ms INTEGER NOT NULL CHECK(typeof(issued_at_ms)='integer' AND issued_at_ms>=0),
        transitioned_at_ms INTEGER NOT NULL CHECK(typeof(transitioned_at_ms)='integer' AND transitioned_at_ms>=issued_at_ms),
        FOREIGN KEY(fence_scope,fence_epoch) REFERENCES effect_fences(scope,epoch) ON DELETE RESTRICT,
        UNIQUE(fence_scope,fence_epoch)
    ) STRICT"#,
    r#"CREATE TABLE effect_audit (
        audit_id TEXT PRIMARY KEY CHECK(typeof(audit_id)='text' AND length(audit_id)=36 AND audit_id NOT GLOB '*[^0-9a-f-]*' AND substr(audit_id,9,1)='-' AND substr(audit_id,14,1)='-' AND substr(audit_id,19,1)='-' AND substr(audit_id,24,1)='-'),
        grant_id TEXT NOT NULL REFERENCES effect_grants(grant_id) ON DELETE RESTRICT,
        effect_id TEXT NOT NULL CHECK(typeof(effect_id)='text' AND length(effect_id)=36 AND effect_id NOT GLOB '*[^0-9a-f-]*' AND substr(effect_id,9,1)='-' AND substr(effect_id,14,1)='-' AND substr(effect_id,19,1)='-' AND substr(effect_id,24,1)='-'),
        state_epoch INTEGER NOT NULL CHECK(typeof(state_epoch)='integer' AND state_epoch>=0),
        event_kind TEXT NOT NULL CHECK(typeof(event_kind)='text' AND event_kind IN ('issued','consuming','applied','unknown','abandoned')),
        target_digest TEXT NOT NULL CHECK(typeof(target_digest)='text' AND length(target_digest)=64 AND target_digest NOT GLOB '*[^0-9a-f]*'),
        canonical_digest TEXT NOT NULL CHECK(typeof(canonical_digest)='text' AND length(canonical_digest)=64 AND canonical_digest NOT GLOB '*[^0-9a-f]*'),
        occurred_at_ms INTEGER NOT NULL CHECK(typeof(occurred_at_ms)='integer' AND occurred_at_ms>=0),
        UNIQUE(grant_id,state_epoch)
    ) STRICT"#,
    "CREATE INDEX effect_grants_status ON effect_grants(status, transitioned_at_ms)",
    "CREATE INDEX effect_audit_effect_id ON effect_audit(effect_id, occurred_at_ms)",
    r#"CREATE TRIGGER effect_fences_are_append_only_update BEFORE UPDATE ON effect_fences BEGIN SELECT RAISE(ABORT,'effect fence is immutable'); END"#,
    r#"CREATE TRIGGER effect_fences_are_append_only_delete BEFORE DELETE ON effect_fences BEGIN SELECT RAISE(ABORT,'effect fence is immutable'); END"#,
    r#"CREATE TRIGGER effect_fences_are_monotonic BEFORE INSERT ON effect_fences WHEN NEW.epoch != COALESCE((SELECT MAX(epoch)+1 FROM effect_fences WHERE scope=NEW.scope),0) BEGIN SELECT RAISE(ABORT,'effect fence epoch is not monotonic'); END"#,
    r#"CREATE TRIGGER effect_grants_initial_state BEFORE INSERT ON effect_grants WHEN NEW.status!='issued' OR NEW.state_epoch!=0 OR NEW.transitioned_at_ms!=NEW.issued_at_ms BEGIN SELECT RAISE(ABORT,'effect grant must be issued once'); END"#,
    r#"CREATE TRIGGER effect_grants_are_not_deleted BEFORE DELETE ON effect_grants BEGIN SELECT RAISE(ABORT,'effect grant is immutable'); END"#,
    r#"CREATE TRIGGER effect_grants_transition_is_guarded BEFORE UPDATE ON effect_grants WHEN OLD.grant_id!=NEW.grant_id OR OLD.effect_id!=NEW.effect_id OR OLD.fence_scope!=NEW.fence_scope OR OLD.fence_epoch!=NEW.fence_epoch OR OLD.target_digest!=NEW.target_digest OR OLD.canonical_digest!=NEW.canonical_digest OR OLD.nonce_hash!=NEW.nonce_hash OR OLD.issued_at_ms!=NEW.issued_at_ms OR NEW.state_epoch!=OLD.state_epoch+1 OR NEW.transitioned_at_ms<OLD.transitioned_at_ms OR NOT (OLD.status='issued' AND NEW.status='consuming' OR OLD.status='consuming' AND NEW.status IN ('applied','unknown','abandoned')) OR NOT EXISTS(SELECT 1 FROM effect_audit WHERE grant_id=OLD.grant_id AND state_epoch=NEW.state_epoch AND effect_id=OLD.effect_id AND event_kind=NEW.status AND target_digest=OLD.target_digest AND canonical_digest=OLD.canonical_digest AND occurred_at_ms=NEW.transitioned_at_ms) BEGIN SELECT RAISE(ABORT,'effect grant transition is not authorized'); END"#,
    r#"CREATE TRIGGER effect_audit_is_immutable_update BEFORE UPDATE ON effect_audit BEGIN SELECT RAISE(ABORT,'effect audit is immutable'); END"#,
    r#"CREATE TRIGGER effect_audit_is_immutable_delete BEFORE DELETE ON effect_audit BEGIN SELECT RAISE(ABORT,'effect audit is immutable'); END"#,
    r#"CREATE TRIGGER effect_audit_matches_grant BEFORE INSERT ON effect_audit WHEN (SELECT effect_id FROM effect_grants WHERE grant_id=NEW.grant_id)!=NEW.effect_id OR (SELECT target_digest FROM effect_grants WHERE grant_id=NEW.grant_id)!=NEW.target_digest OR (SELECT canonical_digest FROM effect_grants WHERE grant_id=NEW.grant_id)!=NEW.canonical_digest BEGIN SELECT RAISE(ABORT,'effect audit does not match grant'); END"#,
];

const V39_EFFECT_FENCE_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "effect_fences",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "effect_grants",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "effect_audit",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "effect_grants_status",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "effect_audit_effect_id",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fences_are_append_only_update",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fences_are_append_only_delete",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fences_are_monotonic",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_grants_initial_state",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[8],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_grants_are_not_deleted",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[9],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_grants_transition_is_guarded",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[10],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_audit_is_immutable_update",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[11],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_audit_is_immutable_delete",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[12],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_audit_matches_grant",
        create_sql: V39_EFFECT_FENCE_STATEMENTS[13],
    },
];

// SQL constraints protect stored shape and ordering; authorization is the anchored
// repository transaction whose integrity checkpoint covers this schema and its rows.
const V40_EFFECT_FENCE_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE effect_fences (
        scope TEXT NOT NULL CHECK(typeof(scope)='text' AND scope='global'),
        epoch INTEGER NOT NULL CHECK(typeof(epoch)='integer' AND epoch>=0),
        canonical_digest TEXT NOT NULL CHECK(typeof(canonical_digest)='text' AND length(canonical_digest)=64 AND canonical_digest NOT GLOB '*[^0-9a-f]*'),
        created_at_ms INTEGER NOT NULL CHECK(typeof(created_at_ms)='integer' AND created_at_ms>=0),
        PRIMARY KEY(scope,epoch)
    ) STRICT"#,
    r#"CREATE TABLE effect_grants (
        grant_id TEXT PRIMARY KEY CHECK(typeof(grant_id)='text' AND length(grant_id)=36 AND substr(grant_id,9,1)='-' AND substr(grant_id,14,1)='-' AND substr(grant_id,19,1)='-' AND substr(grant_id,24,1)='-' AND replace(grant_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(grant_id,'-',''))=32),
        effect_id TEXT NOT NULL UNIQUE CHECK(typeof(effect_id)='text' AND length(effect_id)=36 AND substr(effect_id,9,1)='-' AND substr(effect_id,14,1)='-' AND substr(effect_id,19,1)='-' AND substr(effect_id,24,1)='-' AND replace(effect_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(effect_id,'-',''))=32),
        fence_scope TEXT NOT NULL CHECK(typeof(fence_scope)='text' AND fence_scope='global'),
        fence_epoch INTEGER NOT NULL CHECK(typeof(fence_epoch)='integer' AND fence_epoch>=0),
        target_digest TEXT NOT NULL CHECK(typeof(target_digest)='text' AND length(target_digest)=64 AND target_digest NOT GLOB '*[^0-9a-f]*'),
        canonical_digest TEXT NOT NULL CHECK(typeof(canonical_digest)='text' AND length(canonical_digest)=64 AND canonical_digest NOT GLOB '*[^0-9a-f]*'),
        nonce_hash BLOB NOT NULL CHECK(typeof(nonce_hash)='blob' AND length(nonce_hash)=32),
        status TEXT NOT NULL CHECK(typeof(status)='text' AND status IN ('issued','consuming','applied','unknown','abandoned')),
        state_epoch INTEGER NOT NULL CHECK(typeof(state_epoch)='integer' AND state_epoch>=0),
        issued_at_ms INTEGER NOT NULL CHECK(typeof(issued_at_ms)='integer' AND issued_at_ms>=0),
        transitioned_at_ms INTEGER NOT NULL CHECK(typeof(transitioned_at_ms)='integer' AND transitioned_at_ms>=issued_at_ms),
        abandon_reason TEXT CHECK((status='abandoned' AND typeof(abandon_reason)='text' AND length(abandon_reason) BETWEEN 1 AND 256) OR (status!='abandoned' AND abandon_reason IS NULL)),
        FOREIGN KEY(fence_scope,fence_epoch) REFERENCES effect_fences(scope,epoch) ON DELETE RESTRICT,
        UNIQUE(fence_scope,fence_epoch)
    ) STRICT"#,
    r#"CREATE TABLE effect_audit (
        audit_id TEXT PRIMARY KEY CHECK(typeof(audit_id)='text' AND length(audit_id)=36 AND substr(audit_id,9,1)='-' AND substr(audit_id,14,1)='-' AND substr(audit_id,19,1)='-' AND substr(audit_id,24,1)='-' AND replace(audit_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(audit_id,'-',''))=32),
        grant_id TEXT NOT NULL REFERENCES effect_grants(grant_id) ON DELETE RESTRICT,
        effect_id TEXT NOT NULL CHECK(typeof(effect_id)='text' AND length(effect_id)=36 AND substr(effect_id,9,1)='-' AND substr(effect_id,14,1)='-' AND substr(effect_id,19,1)='-' AND substr(effect_id,24,1)='-' AND replace(effect_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(effect_id,'-',''))=32),
        state_epoch INTEGER NOT NULL CHECK(typeof(state_epoch)='integer' AND state_epoch>=0),
        event_kind TEXT NOT NULL CHECK(typeof(event_kind)='text' AND event_kind IN ('issued','consuming','applied','unknown','abandoned')),
        target_digest TEXT NOT NULL CHECK(typeof(target_digest)='text' AND length(target_digest)=64 AND target_digest NOT GLOB '*[^0-9a-f]*'),
        canonical_digest TEXT NOT NULL CHECK(typeof(canonical_digest)='text' AND length(canonical_digest)=64 AND canonical_digest NOT GLOB '*[^0-9a-f]*'),
        occurred_at_ms INTEGER NOT NULL CHECK(typeof(occurred_at_ms)='integer' AND occurred_at_ms>=0),
        abandon_reason TEXT CHECK((event_kind='abandoned' AND typeof(abandon_reason)='text' AND length(abandon_reason) BETWEEN 1 AND 256) OR (event_kind!='abandoned' AND abandon_reason IS NULL)),
        UNIQUE(grant_id,state_epoch)
    ) STRICT"#,
    "CREATE INDEX effect_grants_status ON effect_grants(status, transitioned_at_ms)",
    "CREATE INDEX effect_audit_effect_id ON effect_audit(effect_id, occurred_at_ms)",
    r#"CREATE TRIGGER effect_fences_are_append_only_update BEFORE UPDATE ON effect_fences BEGIN SELECT RAISE(ABORT,'effect fence is immutable'); END"#,
    r#"CREATE TRIGGER effect_fences_are_append_only_delete BEFORE DELETE ON effect_fences BEGIN SELECT RAISE(ABORT,'effect fence is immutable'); END"#,
    r#"CREATE TRIGGER effect_fences_are_monotonic BEFORE INSERT ON effect_fences WHEN NEW.epoch != COALESCE((SELECT MAX(epoch)+1 FROM effect_fences WHERE scope=NEW.scope),0) BEGIN SELECT RAISE(ABORT,'effect fence epoch is not monotonic'); END"#,
    r#"CREATE TRIGGER effect_grants_initial_state BEFORE INSERT ON effect_grants WHEN NEW.status!='issued' OR NEW.state_epoch!=0 OR NEW.transitioned_at_ms!=NEW.issued_at_ms OR NEW.abandon_reason IS NOT NULL BEGIN SELECT RAISE(ABORT,'effect grant must be issued once'); END"#,
    r#"CREATE TRIGGER effect_grants_are_not_deleted BEFORE DELETE ON effect_grants BEGIN SELECT RAISE(ABORT,'effect grant is immutable'); END"#,
    r#"CREATE TRIGGER effect_grants_transition_is_guarded BEFORE UPDATE ON effect_grants WHEN OLD.grant_id!=NEW.grant_id OR OLD.effect_id!=NEW.effect_id OR OLD.fence_scope!=NEW.fence_scope OR OLD.fence_epoch!=NEW.fence_epoch OR OLD.target_digest!=NEW.target_digest OR OLD.canonical_digest!=NEW.canonical_digest OR OLD.nonce_hash!=NEW.nonce_hash OR OLD.issued_at_ms!=NEW.issued_at_ms OR OLD.abandon_reason IS NOT NULL OR NEW.state_epoch!=OLD.state_epoch+1 OR NEW.transitioned_at_ms<OLD.transitioned_at_ms OR NOT (OLD.status='issued' AND NEW.status='consuming' OR OLD.status IN ('issued','consuming') AND NEW.status='abandoned' OR OLD.status='consuming' AND NEW.status IN ('applied','unknown')) OR NOT EXISTS(SELECT 1 FROM effect_audit WHERE grant_id=OLD.grant_id AND state_epoch=NEW.state_epoch AND effect_id=OLD.effect_id AND event_kind=NEW.status AND target_digest=OLD.target_digest AND canonical_digest=OLD.canonical_digest AND occurred_at_ms=NEW.transitioned_at_ms AND abandon_reason IS NEW.abandon_reason) BEGIN SELECT RAISE(ABORT,'effect grant transition is not authorized'); END"#,
    r#"CREATE TRIGGER effect_audit_is_immutable_update BEFORE UPDATE ON effect_audit BEGIN SELECT RAISE(ABORT,'effect audit is immutable'); END"#,
    r#"CREATE TRIGGER effect_audit_is_immutable_delete BEFORE DELETE ON effect_audit BEGIN SELECT RAISE(ABORT,'effect audit is immutable'); END"#,
    r#"CREATE TRIGGER effect_audit_matches_grant BEFORE INSERT ON effect_audit WHEN (SELECT effect_id FROM effect_grants WHERE grant_id=NEW.grant_id)!=NEW.effect_id OR (SELECT target_digest FROM effect_grants WHERE grant_id=NEW.grant_id)!=NEW.target_digest OR (SELECT canonical_digest FROM effect_grants WHERE grant_id=NEW.grant_id)!=NEW.canonical_digest BEGIN SELECT RAISE(ABORT,'effect audit does not match grant'); END"#,
];

const V40_EFFECT_FENCE_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "effect_fences",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "effect_grants",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "effect_audit",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "effect_grants_status",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "effect_audit_effect_id",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fences_are_append_only_update",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fences_are_append_only_delete",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fences_are_monotonic",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_grants_initial_state",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[8],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_grants_are_not_deleted",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[9],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_grants_transition_is_guarded",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[10],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_audit_is_immutable_update",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[11],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_audit_is_immutable_delete",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[12],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_audit_matches_grant",
        create_sql: V40_EFFECT_FENCE_STATEMENTS[13],
    },
];

const V42_FENCED_RECEIPT_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE effect_fenced_sink_bindings (
        grant_id TEXT PRIMARY KEY REFERENCES effect_grants(grant_id) ON DELETE RESTRICT,
        effect_id TEXT NOT NULL UNIQUE CHECK(typeof(effect_id)='text' AND length(effect_id)=36 AND substr(effect_id,9,1)='-' AND substr(effect_id,14,1)='-' AND substr(effect_id,19,1)='-' AND substr(effect_id,24,1)='-' AND replace(effect_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(effect_id,'-',''))=32),
        sink_identity TEXT NOT NULL CHECK(typeof(sink_identity)='text' AND length(sink_identity) BETWEEN 1 AND 128),
        sink_version TEXT NOT NULL CHECK(typeof(sink_version)='text' AND length(sink_version) BETWEEN 1 AND 128),
        sink_authority TEXT NOT NULL CHECK(typeof(sink_authority)='text' AND length(sink_authority) BETWEEN 1 AND 128),
        command_binding TEXT NOT NULL UNIQUE CHECK(typeof(command_binding)='text' AND length(command_binding)=64 AND command_binding NOT GLOB '*[^0-9a-f]*'),
        repository_instance_id TEXT NOT NULL CHECK(typeof(repository_instance_id)='text' AND length(repository_instance_id) BETWEEN 1 AND 128),
        repository_path_binding TEXT NOT NULL CHECK(typeof(repository_path_binding)='text' AND length(repository_path_binding)=64 AND repository_path_binding NOT GLOB '*[^0-9a-f]*'),
        repository_key_epoch INTEGER NOT NULL CHECK(typeof(repository_key_epoch)='integer' AND repository_key_epoch>0),
        fence_epoch INTEGER NOT NULL CHECK(typeof(fence_epoch)='integer' AND fence_epoch>0),
        target_digest TEXT NOT NULL CHECK(typeof(target_digest)='text' AND length(target_digest)=64 AND target_digest NOT GLOB '*[^0-9a-f]*'),
        canonical_digest TEXT NOT NULL CHECK(typeof(canonical_digest)='text' AND length(canonical_digest)=64 AND canonical_digest NOT GLOB '*[^0-9a-f]*')
    ) STRICT"#,
    r#"CREATE TABLE effect_fenced_receipts (
        receipt_id TEXT PRIMARY KEY CHECK(typeof(receipt_id)='text' AND length(receipt_id)=36 AND substr(receipt_id,9,1)='-' AND substr(receipt_id,14,1)='-' AND substr(receipt_id,19,1)='-' AND substr(receipt_id,24,1)='-' AND replace(receipt_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(receipt_id,'-',''))=32),
        grant_id TEXT NOT NULL UNIQUE REFERENCES effect_fenced_sink_bindings(grant_id) ON DELETE RESTRICT,
        effect_id TEXT NOT NULL UNIQUE CHECK(typeof(effect_id)='text' AND length(effect_id)=36 AND substr(effect_id,9,1)='-' AND substr(effect_id,14,1)='-' AND substr(effect_id,19,1)='-' AND substr(effect_id,24,1)='-' AND replace(effect_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(effect_id,'-',''))=32),
        sink_identity TEXT NOT NULL CHECK(typeof(sink_identity)='text' AND length(sink_identity) BETWEEN 1 AND 128),
        sink_version TEXT NOT NULL CHECK(typeof(sink_version)='text' AND length(sink_version) BETWEEN 1 AND 128),
        sink_authority TEXT NOT NULL CHECK(typeof(sink_authority)='text' AND length(sink_authority) BETWEEN 1 AND 128),
        command_binding TEXT NOT NULL CHECK(typeof(command_binding)='text' AND length(command_binding)=64 AND command_binding NOT GLOB '*[^0-9a-f]*'),
        receipt_binding TEXT NOT NULL UNIQUE CHECK(typeof(receipt_binding)='text' AND length(receipt_binding)=64 AND receipt_binding NOT GLOB '*[^0-9a-f]*'),
        outcome TEXT NOT NULL CHECK(typeof(outcome)='text' AND outcome='applied'),
        fence_epoch INTEGER NOT NULL CHECK(typeof(fence_epoch)='integer' AND fence_epoch>0),
        target_digest TEXT NOT NULL CHECK(typeof(target_digest)='text' AND length(target_digest)=64 AND target_digest NOT GLOB '*[^0-9a-f]*')
    ) STRICT"#,
    r#"CREATE TABLE effect_fenced_legacy_terminal_grants (
        grant_id TEXT PRIMARY KEY REFERENCES effect_grants(grant_id) ON DELETE RESTRICT,
        effect_id TEXT NOT NULL UNIQUE CHECK(typeof(effect_id)='text' AND length(effect_id)=36 AND substr(effect_id,9,1)='-' AND substr(effect_id,14,1)='-' AND substr(effect_id,19,1)='-' AND substr(effect_id,24,1)='-' AND replace(effect_id,'-','') NOT GLOB '*[^0-9a-f]*' AND length(replace(effect_id,'-',''))=32),
        terminal_status TEXT NOT NULL CHECK(typeof(terminal_status)='text' AND terminal_status IN ('applied','unknown','abandoned')),
        state_epoch INTEGER NOT NULL CHECK(typeof(state_epoch)='integer' AND state_epoch=2),
        fence_epoch INTEGER NOT NULL CHECK(typeof(fence_epoch)='integer' AND fence_epoch>0),
        target_digest TEXT NOT NULL CHECK(typeof(target_digest)='text' AND length(target_digest)=64 AND target_digest NOT GLOB '*[^0-9a-f]*'),
        canonical_digest TEXT NOT NULL CHECK(typeof(canonical_digest)='text' AND length(canonical_digest)=64 AND canonical_digest NOT GLOB '*[^0-9a-f]*'),
        transitioned_at_ms INTEGER NOT NULL CHECK(typeof(transitioned_at_ms)='integer' AND transitioned_at_ms>=0)
    ) STRICT"#,
    "CREATE INDEX effect_fenced_receipts_effect_id ON effect_fenced_receipts(effect_id)",
    r#"CREATE TRIGGER effect_fenced_sink_binding_matches_grant BEFORE INSERT ON effect_fenced_sink_bindings WHEN NOT EXISTS(SELECT 1 FROM effect_grants g WHERE g.grant_id=NEW.grant_id AND g.effect_id=NEW.effect_id AND g.fence_epoch=NEW.fence_epoch AND g.target_digest=NEW.target_digest AND g.canonical_digest=NEW.canonical_digest AND g.status='issued') BEGIN SELECT RAISE(ABORT,'fenced sink binding does not match issued grant'); END"#,
    r#"CREATE TRIGGER effect_grant_consuming_requires_fenced_binding BEFORE UPDATE ON effect_grants WHEN OLD.status='issued' AND NEW.status='consuming' AND NOT EXISTS(SELECT 1 FROM effect_fenced_sink_bindings b WHERE b.grant_id=OLD.grant_id AND b.effect_id=OLD.effect_id AND b.fence_epoch=OLD.fence_epoch AND b.target_digest=OLD.target_digest AND b.canonical_digest=OLD.canonical_digest) BEGIN SELECT RAISE(ABORT,'consuming grant requires fenced sink binding'); END"#,
    r#"CREATE TRIGGER effect_fenced_receipt_matches_applied_grant BEFORE INSERT ON effect_fenced_receipts WHEN NOT EXISTS(SELECT 1 FROM effect_fenced_sink_bindings b JOIN effect_grants g ON g.grant_id=b.grant_id WHERE b.grant_id=NEW.grant_id AND b.effect_id=NEW.effect_id AND b.sink_identity=NEW.sink_identity AND b.sink_version=NEW.sink_version AND b.sink_authority=NEW.sink_authority AND b.command_binding=NEW.command_binding AND b.fence_epoch=NEW.fence_epoch AND b.target_digest=NEW.target_digest AND g.status='applied') BEGIN SELECT RAISE(ABORT,'fenced receipt does not match applied grant'); END"#,
    r#"CREATE TRIGGER effect_fenced_legacy_terminal_matches_grant BEFORE INSERT ON effect_fenced_legacy_terminal_grants WHEN NOT EXISTS(SELECT 1 FROM effect_grants g WHERE g.grant_id=NEW.grant_id AND g.effect_id=NEW.effect_id AND g.status=NEW.terminal_status AND g.state_epoch=NEW.state_epoch AND g.fence_epoch=NEW.fence_epoch AND g.target_digest=NEW.target_digest AND g.canonical_digest=NEW.canonical_digest AND g.transitioned_at_ms=NEW.transitioned_at_ms AND g.status IN ('applied','unknown','abandoned')) BEGIN SELECT RAISE(ABORT,'legacy terminal does not match grant'); END"#,
    r#"CREATE TRIGGER effect_fenced_sink_bindings_are_immutable_update BEFORE UPDATE ON effect_fenced_sink_bindings BEGIN SELECT RAISE(ABORT,'fenced sink binding is immutable'); END"#,
    r#"CREATE TRIGGER effect_fenced_sink_bindings_are_immutable_delete BEFORE DELETE ON effect_fenced_sink_bindings BEGIN SELECT RAISE(ABORT,'fenced sink binding is immutable'); END"#,
    r#"CREATE TRIGGER effect_fenced_receipts_are_immutable_update BEFORE UPDATE ON effect_fenced_receipts BEGIN SELECT RAISE(ABORT,'fenced receipt is immutable'); END"#,
    r#"CREATE TRIGGER effect_fenced_receipts_are_immutable_delete BEFORE DELETE ON effect_fenced_receipts BEGIN SELECT RAISE(ABORT,'fenced receipt is immutable'); END"#,
    r#"CREATE TRIGGER effect_fenced_legacy_terminals_are_immutable_update BEFORE UPDATE ON effect_fenced_legacy_terminal_grants BEGIN SELECT RAISE(ABORT,'legacy terminal is immutable'); END"#,
    r#"CREATE TRIGGER effect_fenced_legacy_terminals_are_immutable_delete BEFORE DELETE ON effect_fenced_legacy_terminal_grants BEGIN SELECT RAISE(ABORT,'legacy terminal is immutable'); END"#,
];

const V43_FENCED_AUTHORITY_STATEMENTS: &[&str] = &[
    "ALTER TABLE effect_fenced_sink_bindings ADD COLUMN authority_binding_format INTEGER NOT NULL DEFAULT 0 CHECK(typeof(authority_binding_format)='integer' AND authority_binding_format IN (0,1))",
    "ALTER TABLE effect_fenced_sink_bindings ADD COLUMN authority_key_epoch INTEGER NOT NULL DEFAULT 0 CHECK(typeof(authority_key_epoch)='integer' AND authority_key_epoch>=0)",
    "ALTER TABLE effect_fenced_sink_bindings ADD COLUMN authority_key_fingerprint TEXT NOT NULL DEFAULT '' CHECK(typeof(authority_key_fingerprint)='text' AND (length(authority_key_fingerprint)=0 OR (length(authority_key_fingerprint)=64 AND authority_key_fingerprint NOT GLOB '*[^0-9a-f]*')))",
    "ALTER TABLE effect_fenced_receipts ADD COLUMN authority_binding_format INTEGER NOT NULL DEFAULT 0 CHECK(typeof(authority_binding_format)='integer' AND authority_binding_format IN (0,1))",
    "ALTER TABLE effect_fenced_receipts ADD COLUMN authority_key_epoch INTEGER NOT NULL DEFAULT 0 CHECK(typeof(authority_key_epoch)='integer' AND authority_key_epoch>=0)",
    "ALTER TABLE effect_fenced_receipts ADD COLUMN authority_key_fingerprint TEXT NOT NULL DEFAULT '' CHECK(typeof(authority_key_fingerprint)='text' AND (length(authority_key_fingerprint)=0 OR (length(authority_key_fingerprint)=64 AND authority_key_fingerprint NOT GLOB '*[^0-9a-f]*')))",
    r#"CREATE TRIGGER effect_fenced_v43_sink_authority_key BEFORE INSERT ON effect_fenced_sink_bindings WHEN NEW.authority_binding_format!=1 OR NEW.authority_key_epoch<=0 OR length(NEW.authority_key_fingerprint)!=64 OR NEW.authority_key_fingerprint GLOB '*[^0-9a-f]*' BEGIN SELECT RAISE(ABORT,'fenced sink authority binding is required'); END"#,
    r#"CREATE TRIGGER effect_fenced_v43_receipt_authority_key BEFORE INSERT ON effect_fenced_receipts WHEN NEW.authority_binding_format!=1 OR NEW.authority_key_epoch<=0 OR length(NEW.authority_key_fingerprint)!=64 OR NEW.authority_key_fingerprint GLOB '*[^0-9a-f]*' BEGIN SELECT RAISE(ABORT,'fenced receipt authority binding is required'); END"#,
];

const V43_FENCED_AUTHORITY_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_v43_sink_authority_key",
        create_sql: V43_FENCED_AUTHORITY_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_v43_receipt_authority_key",
        create_sql: V43_FENCED_AUTHORITY_STATEMENTS[7],
    },
];

#[cfg(test)]
mod tests {
    use super::{
        normalize_table_items, should_validate_remote_inspection_v26_schema,
        CURRENT_SCHEMA_VERSION, V42_FENCED_RECEIPT_STATEMENTS, V43_FENCED_AUTHORITY_STATEMENTS,
        V47_HTTPS_MANIFEST_STATEMENTS,
    };

    #[test]
    fn current_schema_uses_remote_inspection_v26_validation() {
        assert!(should_validate_remote_inspection_v26_schema(26));
        assert!(should_validate_remote_inspection_v26_schema(32));
        assert!(should_validate_remote_inspection_v26_schema(
            CURRENT_SCHEMA_VERSION
        ));
        assert!(!should_validate_remote_inspection_v26_schema(24));
        assert!(!should_validate_remote_inspection_v26_schema(25));
        assert!(!should_validate_remote_inspection_v26_schema(
            CURRENT_SCHEMA_VERSION + 1
        ));
    }

    #[test]
    fn v47_requested_url_contract_is_nullable_and_immutable_after_upgrade() {
        assert_eq!(
            V47_HTTPS_MANIFEST_STATEMENTS[0],
            "ALTER TABLE https_manifest_provisions ADD COLUMN requested_url TEXT"
        );
        assert!(V47_HTTPS_MANIFEST_STATEMENTS[1]
            .starts_with("DROP TRIGGER https_manifest_provisions_immutable"));
        assert!(V47_HTTPS_MANIFEST_STATEMENTS[2]
            .contains("UPDATE OF actor,transport_session_binding,requested_url,"));
    }

    #[test]
    fn v43_fenced_table_shape_accepts_fresh_sqlite_schema() {
        for (v42_definition, additions) in [
            (
                V42_FENCED_RECEIPT_STATEMENTS[0],
                &V43_FENCED_AUTHORITY_STATEMENTS[..3],
            ),
            (
                V42_FENCED_RECEIPT_STATEMENTS[1],
                &V43_FENCED_AUTHORITY_STATEMENTS[3..6],
            ),
        ] {
            let close = v42_definition
                .rfind(')')
                .expect("table has a closing parenthesis");
            let added_columns = additions
                .iter()
                .map(|statement| statement.split_once(" ADD COLUMN ").unwrap().1)
                .collect::<Vec<_>>();
            let mut fresh_sql = v42_definition.to_string();
            fresh_sql.insert_str(close, &format!(", {}", added_columns.join(", ")));
            assert_eq!(
                normalize_table_items(&fresh_sql).unwrap(),
                super::expected_v43_fenced_table_items(v42_definition, additions).unwrap()
            );
        }
    }

    #[test]
    fn v43_fenced_table_shape_rejects_constraint_mutation() {
        let v42_definition = V42_FENCED_RECEIPT_STATEMENTS[0];
        let additions = &V43_FENCED_AUTHORITY_STATEMENTS[..3];
        let close = v42_definition
            .rfind(')')
            .expect("table has a closing parenthesis");
        let added_columns = additions
            .iter()
            .map(|statement| statement.split_once(" ADD COLUMN ").unwrap().1)
            .collect::<Vec<_>>();
        let mut fresh_sql = v42_definition.to_string();
        fresh_sql.insert_str(close, &format!(", {}", added_columns.join(", ")));
        let mutated_sql = fresh_sql.replace("authority_key_epoch>=0", "authority_key_epoch>0");
        assert_ne!(
            super::expected_v43_fenced_table_items(v42_definition, additions).unwrap(),
            normalize_table_items(&mutated_sql).unwrap()
        );
    }
}

const V42_FENCED_RECEIPT_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "effect_fenced_sink_bindings",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "effect_fenced_receipts",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "effect_fenced_legacy_terminal_grants",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "effect_fenced_receipts_effect_id",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_sink_binding_matches_grant",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_grant_consuming_requires_fenced_binding",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_receipt_matches_applied_grant",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_legacy_terminal_matches_grant",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_sink_bindings_are_immutable_update",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[8],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_sink_bindings_are_immutable_delete",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[9],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_receipts_are_immutable_update",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[10],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_receipts_are_immutable_delete",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[11],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_legacy_terminals_are_immutable_update",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[12],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "effect_fenced_legacy_terminals_are_immutable_delete",
        create_sql: V42_FENCED_RECEIPT_STATEMENTS[13],
    },
];

const V25_REMOTE_INSPECTION_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_intake_remote_inspection_consents",
        create_sql: V25_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_intake_remote_inspection_attempts",
        create_sql: V25_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_intake_remote_inspection_snapshots",
        create_sql: V25_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_intake_remote_inspection_consumptions",
        create_sql: V25_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_intake_remote_inspection_consent_lookup",
        create_sql: V25_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_intake_remote_inspection_active_attempt",
        create_sql: V25_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_intake_remote_inspection_snapshot_lookup",
        create_sql: V25_STATEMENTS[6],
    },
];

const V26_REMOTE_INSPECTION_SCHEMA_CONTRACTS: &[SchemaObjectContract] = &[
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_intake_remote_inspection_scope_lineage_anchors_v26",
        create_sql: V26_STATEMENTS[0],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_intake_remote_inspection_reservations",
        create_sql: V26_STATEMENTS[1],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_intake_remote_inspection_reservation_events",
        create_sql: V26_STATEMENTS[2],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_intake_remote_inspection_bindings_b26",
        create_sql: V26_STATEMENTS[3],
    },
    SchemaObjectContract {
        object_type: "table",
        name: "mcp_intake_remote_inspection_commit_events_v26",
        create_sql: V26_STATEMENTS[4],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_intake_remote_inspection_reservation_lookup_v26",
        create_sql: V26_STATEMENTS[5],
    },
    SchemaObjectContract {
        object_type: "index",
        name: "mcp_intake_remote_inspection_claim_lookup_v26",
        create_sql: V26_STATEMENTS[6],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_reserved_event_v26",
        create_sql: V26_STATEMENTS[7],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_commit_ord2_requires_ord1_v26",
        create_sql: V26_STATEMENTS[8],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_consents_v26_manual_only",
        create_sql: V26_STATEMENTS[9],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_consents_v26_manual_only_update",
        create_sql: V26_STATEMENTS[10],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_snapshots_v26_manual_only",
        create_sql: V26_STATEMENTS[11],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_snapshots_v26_manual_only_update",
        create_sql: V26_STATEMENTS[12],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_reservations_validate_insert_v26",
        create_sql: V26_STATEMENTS[13],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_bindings_validate_insert_v26",
        create_sql: V26_STATEMENTS[14],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_commit_events_validate_insert_v26",
        create_sql: V26_STATEMENTS[15],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_reservation_events_immutable_v26",
        create_sql: V26_STATEMENTS[16],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_bindings_immutable_v26",
        create_sql: V26_STATEMENTS[17],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_commit_events_immutable_v26",
        create_sql: V26_STATEMENTS[18],
    },
    SchemaObjectContract {
        object_type: "trigger",
        name: "mcp_intake_remote_inspection_reservations_validate_update_v26",
        create_sql: V26_STATEMENTS[19],
    },
];

pub(crate) async fn inspect_remote_inspection_v26_schema_state(
    tx: &mut Transaction<'_, Sqlite>,
) -> McpPlatformResult<RemoteInspectionV26SchemaState> {
    let mut present = 0_usize;
    for object_type in ["table", "index", "trigger"] {
        let expected = V26_REMOTE_INSPECTION_SCHEMA_CONTRACTS
            .iter()
            .filter(|contract| contract.object_type == object_type)
            .map(|contract| contract.name)
            .collect::<Vec<_>>();
        present += schema_object_names(tx, object_type)
            .await?
            .into_iter()
            .filter(|name| is_remote_inspection_v26_managed_object_name(name, &expected))
            .count();
    }
    if present == 0 {
        return Ok(RemoteInspectionV26SchemaState::Absent);
    }
    if present != V26_REMOTE_INSPECTION_SCHEMA_CONTRACTS.len() {
        return Ok(RemoteInspectionV26SchemaState::PartialOrInvalid);
    }
    match validate_remote_inspection_v26_schema_contracts_and_managed_set(tx).await {
        Ok(()) => Ok(RemoteInspectionV26SchemaState::Complete),
        Err(_) => Ok(RemoteInspectionV26SchemaState::PartialOrInvalid),
    }
}

const V9_STATEMENTS: &[&str] = &[
    "ALTER TABLE managed_lifecycle_leases ADD COLUMN expires_at_ms INTEGER",
    "DROP TABLE task_step_history_compensation_bindings",
    "DROP TABLE task_step_compensation_bindings",
    "DROP TABLE projection_writer_bindings",
    "DROP TABLE legacy_projection_bindings",
    r#"CREATE TABLE managed_projection_integrity (
        managed_mcp_id TEXT PRIMARY KEY REFERENCES connection_projections(managed_mcp_id) ON DELETE RESTRICT,
        mac TEXT NOT NULL CHECK(length(mac)=64)
    )"#,
    r#"CREATE TABLE projection_writer_bindings (
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        plan_id TEXT NOT NULL REFERENCES install_plans(plan_id) ON DELETE RESTRICT,
        plan_digest TEXT NOT NULL CHECK(length(plan_digest)=64),
        link_key TEXT NOT NULL,
        manifest_digest TEXT NOT NULL CHECK(length(manifest_digest)=64),
        projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
        lifecycle_acquired_at_ms INTEGER NOT NULL,
        worker_owner_id TEXT NOT NULL,
        worker_lease_expires_at_ms INTEGER NOT NULL,
        step_ordinal INTEGER NOT NULL,
        step_token TEXT NOT NULL,
        mac TEXT NOT NULL CHECK(length(mac)=64),
        PRIMARY KEY(managed_mcp_id,task_id)
    )"#,
    r#"CREATE TABLE task_step_compensation_bindings (
        task_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL,
        mac TEXT NOT NULL CHECK(length(mac)=64),
        PRIMARY KEY(task_id,ordinal),
        FOREIGN KEY(task_id,ordinal) REFERENCES task_steps(task_id,ordinal) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE task_step_history_compensation_bindings (
        task_id TEXT NOT NULL,
        attempt INTEGER NOT NULL,
        ordinal INTEGER NOT NULL,
        mac TEXT NOT NULL CHECK(length(mac)=64),
        PRIMARY KEY(task_id,attempt,ordinal),
        FOREIGN KEY(task_id,attempt,ordinal) REFERENCES task_step_history(task_id,attempt,ordinal) ON DELETE RESTRICT
    )"#,
];

const V8_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE legacy_projection_bindings (
        managed_mcp_id TEXT PRIMARY KEY REFERENCES connection_projections(managed_mcp_id) ON DELETE RESTRICT,
        link_key TEXT NOT NULL,
        projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
        binding_digest TEXT NOT NULL CHECK(length(binding_digest)=64)
    )"#,
    r#"CREATE TABLE projection_writer_bindings (
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE RESTRICT,
        plan_id TEXT NOT NULL REFERENCES install_plans(plan_id) ON DELETE RESTRICT,
        link_key TEXT NOT NULL,
        manifest_digest TEXT NOT NULL CHECK(length(manifest_digest)=64),
        projection_digest TEXT NOT NULL CHECK(length(projection_digest)=64),
        PRIMARY KEY(managed_mcp_id,task_id)
    )"#,
    r#"CREATE TABLE task_step_compensation_bindings (
        task_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL,
        binding_digest TEXT NOT NULL CHECK(length(binding_digest)=64),
        PRIMARY KEY(task_id,ordinal),
        FOREIGN KEY(task_id,ordinal) REFERENCES task_steps(task_id,ordinal) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE task_step_history_compensation_bindings (
        task_id TEXT NOT NULL,
        attempt INTEGER NOT NULL,
        ordinal INTEGER NOT NULL,
        binding_digest TEXT NOT NULL CHECK(length(binding_digest)=64),
        PRIMARY KEY(task_id,attempt,ordinal),
        FOREIGN KEY(task_id,attempt,ordinal) REFERENCES task_step_history(task_id,attempt,ordinal) ON DELETE RESTRICT
    )"#,
];

const V6_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE mcp_profiles (
        profile_id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        description TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK(revision > 0),
        archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0,1)),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE mcp_profile_entries (
        profile_id TEXT NOT NULL REFERENCES mcp_profiles(profile_id) ON DELETE CASCADE,
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
        PRIMARY KEY(profile_id, managed_mcp_id),
        UNIQUE(profile_id, ordinal)
    )"#,
    r#"CREATE TABLE mcp_profile_revisions (
        profile_id TEXT NOT NULL REFERENCES mcp_profiles(profile_id) ON DELETE CASCADE,
        revision INTEGER NOT NULL CHECK(revision > 0),
        snapshot_json TEXT NOT NULL,
        actor TEXT NOT NULL,
        operation TEXT NOT NULL CHECK(operation IN ('create','update','restore','archive')),
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(profile_id, revision)
    )"#,
    r#"CREATE TABLE mcp_profile_idempotency (
        operation TEXT NOT NULL,
        idempotency_key TEXT NOT NULL,
        request_digest TEXT NOT NULL CHECK(length(request_digest) = 64),
        profile_id TEXT NOT NULL REFERENCES mcp_profiles(profile_id) ON DELETE CASCADE,
        resulting_revision INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(operation, idempotency_key)
    )"#,
    r#"CREATE TABLE mcp_profile_apply_plans (
        plan_id TEXT PRIMARY KEY,
        profile_id TEXT NOT NULL REFERENCES mcp_profiles(profile_id) ON DELETE RESTRICT,
        profile_revision INTEGER NOT NULL,
        plan_digest TEXT NOT NULL CHECK(length(plan_digest) = 64),
        snapshot_json TEXT NOT NULL,
        actor TEXT NOT NULL,
        expires_at_ms INTEGER NOT NULL,
        idempotency_key TEXT NOT NULL UNIQUE,
        request_digest TEXT NOT NULL CHECK(length(request_digest) = 64),
        created_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE mcp_profile_application_tokens (
        token_hash TEXT PRIMARY KEY CHECK(length(token_hash) = 64),
        application_id TEXT NOT NULL UNIQUE,
        plan_id TEXT NOT NULL REFERENCES mcp_profile_apply_plans(plan_id) ON DELETE RESTRICT,
        plan_digest TEXT NOT NULL CHECK(length(plan_digest) = 64),
        profile_id TEXT NOT NULL REFERENCES mcp_profiles(profile_id) ON DELETE RESTRICT,
        profile_revision INTEGER NOT NULL,
        actor TEXT NOT NULL,
        expires_at_ms INTEGER NOT NULL,
        consumed_at_ms INTEGER,
        created_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE mcp_profile_applications (
        application_id TEXT PRIMARY KEY,
        token_hash TEXT NOT NULL REFERENCES mcp_profile_application_tokens(token_hash) ON DELETE RESTRICT,
        plan_id TEXT NOT NULL REFERENCES mcp_profile_apply_plans(plan_id) ON DELETE RESTRICT,
        profile_id TEXT NOT NULL REFERENCES mcp_profiles(profile_id) ON DELETE RESTRICT,
        profile_revision INTEGER NOT NULL,
        plan_digest TEXT NOT NULL CHECK(length(plan_digest) = 64),
        actor TEXT NOT NULL,
        status TEXT NOT NULL CHECK(status IN ('confirmed','consumed','created','failed','rolled_back','recovery_required','deleted')),
        session_id TEXT,
        failure_code TEXT,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE mcp_profile_application_events (
        event_id INTEGER PRIMARY KEY AUTOINCREMENT,
        application_id TEXT NOT NULL REFERENCES mcp_profile_applications(application_id) ON DELETE RESTRICT,
        event_type TEXT NOT NULL CHECK(event_type IN ('token_created','token_consumed','session_created','application_failed','application_rolled_back','recovery_required','session_deleted')),
        actor TEXT NOT NULL,
        occurred_at_ms INTEGER NOT NULL,
        detail_code TEXT NOT NULL
    )"#,
    "CREATE INDEX mcp_profile_entries_ordinal ON mcp_profile_entries(profile_id, ordinal)",
    "CREATE INDEX mcp_profile_applications_session ON mcp_profile_applications(session_id)",
];
