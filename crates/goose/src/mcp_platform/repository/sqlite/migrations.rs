use sqlx::{Row, Sqlite, Transaction};

use crate::mcp_platform::error::McpPlatformResult;
use crate::mcp_platform::repository::PlanTarget;

use super::{decode, map_sqlx};

pub const CURRENT_SCHEMA_VERSION: i64 = 4;

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
