use sqlx::{Sqlite, Transaction};

use crate::mcp_platform::error::McpPlatformResult;

use super::map_sqlx;

pub const CURRENT_SCHEMA_VERSION: i64 = 3;

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
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        mode TEXT NOT NULL CHECK(mode IN ('registration','runtime'))
    )"#,
    r#"CREATE TABLE health_observations (
        observation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
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
        managed_mcp_id TEXT NOT NULL REFERENCES managed_mcps(managed_mcp_id) ON DELETE RESTRICT,
        expected_revision INTEGER NOT NULL,
        previous_enabled INTEGER NOT NULL CHECK(previous_enabled IN (0,1)),
        desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),
        status TEXT NOT NULL CHECK(status IN ('started','config_committed','committed','recovery_required')),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    "CREATE UNIQUE INDEX projection_mutations_active ON projection_mutations(managed_mcp_id) WHERE status IN ('started','config_committed')",
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
