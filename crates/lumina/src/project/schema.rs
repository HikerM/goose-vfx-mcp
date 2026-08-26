use anyhow::Result;
use sqlx::{Sqlite, Transaction};

pub(crate) async fn create_project_tables_in_tx(tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
    for statement in PROJECT_SCHEMA {
        sqlx::query(statement).execute(&mut **tx).await?;
    }
    Ok(())
}

const PROJECT_SCHEMA: &[&str] = &[
    r#"
    CREATE TABLE IF NOT EXISTS projects (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        root_path TEXT NOT NULL,
        canonical_root TEXT NOT NULL COLLATE NOCASE UNIQUE,
        profile_json TEXT NOT NULL DEFAULT '{}',
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        last_opened_at_ms INTEGER NOT NULL,
        archived_at_ms INTEGER
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS project_roots (
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        canonical_path TEXT NOT NULL COLLATE NOCASE,
        is_primary INTEGER NOT NULL DEFAULT 0,
        permissions_json TEXT NOT NULL DEFAULT '{}',
        PRIMARY KEY (project_id, canonical_path)
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS work_items (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        title TEXT NOT NULL,
        objective TEXT NOT NULL,
        acceptance_criteria_json TEXT NOT NULL DEFAULT '[]',
        status TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        completed_at_ms INTEGER
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS work_item_sessions (
        work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        linked_at_ms INTEGER NOT NULL,
        PRIMARY KEY (work_item_id, session_id),
        UNIQUE (session_id)
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS project_runs (
        id TEXT PRIMARY KEY,
        work_item_id TEXT NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
        session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
        status TEXT NOT NULL,
        phase TEXT NOT NULL,
        started_at_ms INTEGER,
        updated_at_ms INTEGER NOT NULL,
        finished_at_ms INTEGER,
        error_code TEXT,
        error_message TEXT
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS run_steps (
        id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL REFERENCES project_runs(id) ON DELETE CASCADE,
        sequence INTEGER NOT NULL,
        kind TEXT NOT NULL,
        title TEXT NOT NULL,
        status TEXT NOT NULL,
        detail_json TEXT NOT NULL DEFAULT '{}',
        started_at_ms INTEGER,
        finished_at_ms INTEGER,
        UNIQUE (run_id, sequence)
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS workspace_checkpoints (
        id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL REFERENCES project_runs(id) ON DELETE CASCADE,
        sequence INTEGER NOT NULL,
        phase TEXT NOT NULL,
        state_json TEXT NOT NULL,
        safe_to_resume INTEGER NOT NULL DEFAULT 0,
        created_at_ms INTEGER NOT NULL,
        UNIQUE (run_id, sequence)
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS change_sets (
        id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL REFERENCES project_runs(id) ON DELETE CASCADE UNIQUE,
        base_revision TEXT,
        status TEXT NOT NULL,
        summary TEXT NOT NULL DEFAULT '',
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS change_files (
        change_set_id TEXT NOT NULL REFERENCES change_sets(id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        change_kind TEXT NOT NULL,
        before_digest TEXT,
        after_digest TEXT,
        patch TEXT,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (change_set_id, path)
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS validation_runs (
        id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL REFERENCES project_runs(id) ON DELETE CASCADE,
        profile TEXT NOT NULL,
        status TEXT NOT NULL,
        command_json TEXT NOT NULL,
        summary_json TEXT NOT NULL DEFAULT '{}',
        started_at_ms INTEGER,
        finished_at_ms INTEGER
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS project_artifacts (
        id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL REFERENCES project_runs(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        title TEXT NOT NULL,
        uri TEXT,
        content_json TEXT NOT NULL DEFAULT '{}',
        created_at_ms INTEGER NOT NULL
    )
    "#,
    "CREATE INDEX IF NOT EXISTS idx_projects_last_opened ON projects(last_opened_at_ms DESC)",
    "CREATE INDEX IF NOT EXISTS idx_work_items_project ON work_items(project_id, updated_at_ms DESC)",
    "CREATE INDEX IF NOT EXISTS idx_project_runs_work_item ON project_runs(work_item_id, updated_at_ms DESC)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_project_runs_one_active_per_work_item ON project_runs(work_item_id) WHERE status = 'running'",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_project_runs_one_active_per_session ON project_runs(session_id) WHERE session_id IS NOT NULL AND status = 'running'",
    "CREATE INDEX IF NOT EXISTS idx_run_steps_run ON run_steps(run_id, sequence)",
    "CREATE INDEX IF NOT EXISTS idx_checkpoints_run ON workspace_checkpoints(run_id, sequence DESC)",
];
