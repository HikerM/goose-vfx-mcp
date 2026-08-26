use super::model::{
    ChangeSet, ChangeSetStatus, Project, ProjectProfile, ProjectSnapshot, Run, RunPhase, RunStatus,
    RunStep, RunStepStatus, WorkItem, WorkItemStatus, WorkspaceCheckpoint,
};
use crate::session::session_manager::SessionStorage;
use anyhow::{ensure, Context, Result};
use chrono::Utc;
use sqlx::{Row, SqlitePool};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CreateWorkItem {
    pub project_id: String,
    pub title: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RunCompletion {
    pub status: RunStatus,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone)]
pub struct ProjectService {
    storage: Arc<SessionStorage>,
}

impl ProjectService {
    pub fn new(storage: Arc<SessionStorage>) -> Self {
        Self { storage }
    }

    async fn pool(&self) -> Result<&SqlitePool> {
        self.storage.pool().await
    }

    pub async fn open_project(&self, root: &Path, name: Option<&str>) -> Result<Project> {
        ensure!(root.is_absolute(), "project root must be absolute");
        ensure!(root.is_dir(), "project root must be an existing directory");
        let canonical_root = root.canonicalize()?;
        let explicit_name = name.map(str::trim).filter(|name| !name.is_empty());
        let display_name = normalized_project_name(explicit_name, &canonical_root)?;
        let profile = detect_project_profile(&canonical_root);
        let profile_json = serde_json::to_string(&profile)?;
        let now = Utc::now().timestamp_millis();
        let id = format!("project_{}", Uuid::new_v4());
        let root_path = root.to_string_lossy().to_string();
        let canonical_path = canonical_root.to_string_lossy().to_string();
        let pool = self.pool().await?;
        let mut tx = pool.begin().await?;

        sqlx::query(
            r#"
            INSERT INTO projects (
                id, name, root_path, canonical_root, profile_json,
                created_at_ms, updated_at_ms, last_opened_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(canonical_root) DO UPDATE SET
                name = CASE WHEN ? THEN excluded.name ELSE projects.name END,
                root_path = excluded.root_path,
                profile_json = excluded.profile_json,
                updated_at_ms = excluded.updated_at_ms,
                last_opened_at_ms = excluded.last_opened_at_ms,
                archived_at_ms = NULL
            "#,
        )
        .bind(&id)
        .bind(&display_name)
        .bind(&root_path)
        .bind(&canonical_path)
        .bind(&profile_json)
        .bind(now)
        .bind(now)
        .bind(now)
        .bind(explicit_name.is_some())
        .execute(&mut *tx)
        .await?;

        let project_id: String =
            sqlx::query_scalar("SELECT id FROM projects WHERE canonical_root = ? COLLATE NOCASE")
                .bind(&canonical_path)
                .fetch_one(&mut *tx)
                .await?;

        sqlx::query(
            r#"
            INSERT INTO project_roots (
                project_id, path, canonical_path, is_primary, permissions_json
            ) VALUES (?, ?, ?, 1, '{"read":true,"write":true}')
            ON CONFLICT(project_id, canonical_path) DO UPDATE SET
                path = excluded.path,
                is_primary = 1
            "#,
        )
        .bind(&project_id)
        .bind(&root_path)
        .bind(&canonical_path)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.get_project(&project_id).await
    }

    pub async fn list_projects(&self, include_archived: bool) -> Result<Vec<Project>> {
        let pool = self.pool().await?;
        let rows = if include_archived {
            sqlx::query("SELECT * FROM projects ORDER BY last_opened_at_ms DESC")
                .fetch_all(pool)
                .await?
        } else {
            sqlx::query(
                "SELECT * FROM projects WHERE archived_at_ms IS NULL ORDER BY last_opened_at_ms DESC",
            )
            .fetch_all(pool)
            .await?
        };
        rows.iter().map(project_from_row).collect()
    }

    pub async fn get_project(&self, project_id: &str) -> Result<Project> {
        let row = sqlx::query("SELECT * FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_one(self.pool().await?)
            .await?;
        project_from_row(&row)
    }

    pub async fn snapshot(&self, project_id: &str) -> Result<ProjectSnapshot> {
        let project = self.get_project(project_id).await?;
        let work_items = self.list_work_items(project_id).await?;
        let runs = self.list_project_runs(project_id).await?;
        let run_steps = self.list_project_run_steps(project_id).await?;
        let checkpoints = self.list_project_checkpoints(project_id).await?;
        let interrupted_run_count = runs
            .iter()
            .filter(|run| run.status == RunStatus::Interrupted)
            .count();
        Ok(ProjectSnapshot {
            project,
            work_items,
            runs,
            run_steps,
            checkpoints,
            interrupted_run_count,
        })
    }

    pub async fn create_work_item(&self, input: CreateWorkItem) -> Result<WorkItem> {
        let title = input.title.trim();
        let objective = input.objective.trim();
        ensure!(!title.is_empty(), "work item title is required");
        ensure!(!objective.is_empty(), "work item objective is required");
        self.get_project(&input.project_id).await?;

        let id = format!("work_{}", Uuid::new_v4());
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO work_items (
                id, project_id, title, objective, acceptance_criteria_json,
                status, created_at_ms, updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(&input.project_id)
        .bind(title)
        .bind(objective)
        .bind(serde_json::to_string(&input.acceptance_criteria)?)
        .bind(WorkItemStatus::Draft.as_str())
        .bind(now)
        .bind(now)
        .execute(self.pool().await?)
        .await?;
        self.get_work_item(&id).await
    }

    pub async fn get_work_item(&self, work_item_id: &str) -> Result<WorkItem> {
        let row = sqlx::query("SELECT * FROM work_items WHERE id = ?")
            .bind(work_item_id)
            .fetch_one(self.pool().await?)
            .await?;
        work_item_from_row(&row)
    }

    pub async fn list_work_items(&self, project_id: &str) -> Result<Vec<WorkItem>> {
        let rows = sqlx::query(
            "SELECT * FROM work_items WHERE project_id = ? ORDER BY updated_at_ms DESC",
        )
        .bind(project_id)
        .fetch_all(self.pool().await?)
        .await?;
        rows.iter().map(work_item_from_row).collect()
    }

    pub async fn attach_session(
        &self,
        project_id: &str,
        work_item_id: &str,
        session_id: &str,
    ) -> Result<()> {
        let work_item = self.get_work_item(work_item_id).await?;
        ensure!(
            work_item.project_id == project_id,
            "work item does not belong to project"
        );
        let pool = self.pool().await?;
        let mut tx = pool.begin().await?;
        let updated = sqlx::query("UPDATE sessions SET project_id = ? WHERE id = ?")
            .bind(project_id)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        ensure!(updated.rows_affected() == 1, "session not found");
        sqlx::query(
            r#"
            INSERT INTO work_item_sessions (work_item_id, session_id, linked_at_ms)
            VALUES (?, ?, ?)
            ON CONFLICT(session_id) DO UPDATE SET
                work_item_id = excluded.work_item_id,
                linked_at_ms = excluded.linked_at_ms
            "#,
        )
        .bind(work_item_id)
        .bind(session_id)
        .bind(Utc::now().timestamp_millis())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn start_run(&self, work_item_id: &str, session_id: Option<&str>) -> Result<Run> {
        let work_item = self.get_work_item(work_item_id).await?;
        ensure!(
            !matches!(
                work_item.status,
                WorkItemStatus::Completed | WorkItemStatus::Cancelled
            ),
            "completed or cancelled work item cannot start a new run"
        );
        let active_run_id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM project_runs WHERE work_item_id = ? AND status = 'running' LIMIT 1",
        )
        .bind(work_item_id)
        .fetch_optional(self.pool().await?)
        .await?;
        ensure!(
            active_run_id.is_none(),
            "work item already has an active run"
        );
        let id = format!("run_{}", Uuid::new_v4());
        let now = Utc::now().timestamp_millis();
        let pool = self.pool().await?;
        let mut tx = pool.begin().await?;
        if let Some(session_id) = session_id {
            let updated = sqlx::query("UPDATE sessions SET project_id = ? WHERE id = ?")
                .bind(&work_item.project_id)
                .bind(session_id)
                .execute(&mut *tx)
                .await?;
            ensure!(updated.rows_affected() == 1, "session not found");
            sqlx::query(
                r#"
                INSERT INTO work_item_sessions (work_item_id, session_id, linked_at_ms)
                VALUES (?, ?, ?)
                ON CONFLICT(session_id) DO UPDATE SET
                    work_item_id = excluded.work_item_id,
                    linked_at_ms = excluded.linked_at_ms
                "#,
            )
            .bind(work_item_id)
            .bind(session_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            r#"
            INSERT INTO project_runs (
                id, work_item_id, session_id, status, phase,
                started_at_ms, updated_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(work_item_id)
        .bind(session_id)
        .bind(RunStatus::Running.as_str())
        .bind(RunPhase::Discovering.as_str())
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE work_items SET status = ?, updated_at_ms = ? WHERE id = ?")
            .bind(WorkItemStatus::Running.as_str())
            .bind(now)
            .bind(work_item_id)
            .execute(&mut *tx)
            .await?;
        for (index, (kind, title)) in [
            ("discover", "Discover project context"),
            ("plan", "Plan the implementation"),
            ("approve", "Confirm guarded actions"),
            ("execute", "Apply project changes"),
            ("validate", "Validate the result"),
            ("review", "Review and hand off"),
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query(
                r#"
                INSERT INTO run_steps (
                    id, run_id, sequence, kind, title, status, detail_json, started_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, '{}', ?)
                "#,
            )
            .bind(format!("step_{}", Uuid::new_v4()))
            .bind(&id)
            .bind(index as i64 + 1)
            .bind(kind)
            .bind(title)
            .bind(if index == 0 {
                RunStepStatus::Running.as_str()
            } else {
                RunStepStatus::Pending.as_str()
            })
            .bind(if index == 0 { Some(now) } else { None })
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.get_run(&id).await
    }

    pub async fn update_run_phase(&self, run_id: &str, phase: RunPhase) -> Result<Run> {
        let run = self.get_run(run_id).await?;
        ensure!(
            run.status == RunStatus::Running,
            "only running runs can change phase"
        );
        ensure!(
            phase_step_sequence(phase) >= phase_step_sequence(run.phase),
            "run phase cannot move backwards"
        );
        let now = Utc::now().timestamp_millis();
        let pool = self.pool().await?;
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE project_runs SET phase = ?, updated_at_ms = ? WHERE id = ?")
            .bind(phase.as_str())
            .bind(now)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        let active_sequence = phase_step_sequence(phase);
        sqlx::query(
            r#"
            UPDATE run_steps
            SET status = CASE
                    WHEN sequence < ? THEN 'completed'
                    WHEN sequence = ? THEN 'running'
                    ELSE 'pending'
                END,
                started_at_ms = CASE
                    WHEN sequence = ? THEN COALESCE(started_at_ms, ?)
                    ELSE started_at_ms
                END,
                finished_at_ms = CASE
                    WHEN sequence < ? THEN COALESCE(finished_at_ms, ?)
                    ELSE NULL
                END
            WHERE run_id = ?
            "#,
        )
        .bind(active_sequence)
        .bind(active_sequence)
        .bind(active_sequence)
        .bind(now)
        .bind(active_sequence)
        .bind(now)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
        let work_status = match phase {
            RunPhase::Validating => WorkItemStatus::Validating,
            RunPhase::Reviewing | RunPhase::Complete => WorkItemStatus::Review,
            _ => WorkItemStatus::Running,
        };
        sqlx::query("UPDATE work_items SET status = ?, updated_at_ms = ? WHERE id = ?")
            .bind(work_status.as_str())
            .bind(now)
            .bind(&run.work_item_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.create_checkpoint(
            run_id,
            phase,
            serde_json::json!({ "source": "lifecycle" }),
            matches!(
                phase,
                RunPhase::Planning
                    | RunPhase::AwaitingApproval
                    | RunPhase::Validating
                    | RunPhase::Reviewing
            ),
        )
        .await?;
        self.get_run(run_id).await
    }

    pub async fn finish_run(&self, run_id: &str, completion: RunCompletion) -> Result<Run> {
        ensure!(
            completion.status != RunStatus::Running,
            "run completion must be terminal"
        );
        ensure!(
            completion.status != RunStatus::Pending,
            "run completion must be terminal"
        );
        let run = self.get_run(run_id).await?;
        ensure!(
            matches!(run.status, RunStatus::Running | RunStatus::Pending),
            "run is already terminal"
        );
        let (work_status, phase) = match completion.status {
            RunStatus::Succeeded => (WorkItemStatus::Review, RunPhase::Reviewing),
            RunStatus::Failed => (WorkItemStatus::Failed, run.phase),
            RunStatus::Cancelled => (WorkItemStatus::Cancelled, run.phase),
            RunStatus::Interrupted => (WorkItemStatus::Interrupted, run.phase),
            RunStatus::Pending | RunStatus::Running => unreachable!(),
        };
        let now = Utc::now().timestamp_millis();
        let pool = self.pool().await?;
        let mut tx = pool.begin().await?;
        sqlx::query(
            r#"
            UPDATE project_runs
            SET status = ?, phase = ?, updated_at_ms = ?, finished_at_ms = ?,
                error_code = ?, error_message = ?
            WHERE id = ?
            "#,
        )
        .bind(completion.status.as_str())
        .bind(phase.as_str())
        .bind(now)
        .bind(now)
        .bind(completion.error_code)
        .bind(completion.error_message)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE work_items SET status = ?, updated_at_ms = ? WHERE id = ?")
            .bind(work_status.as_str())
            .bind(now)
            .bind(&run.work_item_id)
            .execute(&mut *tx)
            .await?;
        match completion.status {
            RunStatus::Succeeded => {
                sqlx::query(
                    "UPDATE run_steps SET status = 'completed', started_at_ms = COALESCE(started_at_ms, ?), finished_at_ms = COALESCE(finished_at_ms, ?) WHERE run_id = ?",
                )
                .bind(now)
                .bind(now)
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
            }
            RunStatus::Failed => {
                sqlx::query(
                    "UPDATE run_steps SET status = 'failed', finished_at_ms = ? WHERE run_id = ? AND status = 'running'",
                )
                .bind(now)
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
            }
            RunStatus::Cancelled | RunStatus::Interrupted => {
                sqlx::query(
                    "UPDATE run_steps SET status = 'skipped', finished_at_ms = ? WHERE run_id = ? AND status IN ('running', 'pending')",
                )
                .bind(now)
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
            }
            RunStatus::Pending | RunStatus::Running => unreachable!(),
        }
        tx.commit().await?;
        self.get_run(run_id).await
    }

    pub async fn complete_work_item(&self, work_item_id: &str) -> Result<WorkItem> {
        let work_item = self.get_work_item(work_item_id).await?;
        if work_item.status == WorkItemStatus::Completed {
            return Ok(work_item);
        }
        ensure!(
            work_item.status == WorkItemStatus::Review,
            "only a reviewed work item can be completed"
        );
        let active_run_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_runs WHERE work_item_id = ? AND status = 'running'",
        )
        .bind(work_item_id)
        .fetch_one(self.pool().await?)
        .await?;
        ensure!(active_run_count == 0, "work item still has an active run");
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "UPDATE work_items SET status = ?, updated_at_ms = ?, completed_at_ms = ? WHERE id = ?",
        )
        .bind(WorkItemStatus::Completed.as_str())
        .bind(now)
        .bind(now)
        .bind(work_item_id)
        .execute(self.pool().await?)
        .await?;
        self.get_work_item(work_item_id).await
    }

    pub async fn get_run(&self, run_id: &str) -> Result<Run> {
        let row = sqlx::query("SELECT * FROM project_runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(self.pool().await?)
            .await?;
        run_from_row(&row)
    }

    pub async fn running_run_for_session(&self, session_id: &str) -> Result<Option<Run>> {
        let row = sqlx::query(
            "SELECT * FROM project_runs WHERE session_id = ? AND status = 'running' ORDER BY started_at_ms DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(self.pool().await?)
        .await?;
        row.as_ref().map(run_from_row).transpose()
    }

    pub async fn work_item_for_session(&self, session_id: &str) -> Result<Option<WorkItem>> {
        let row = sqlx::query(
            r#"
            SELECT w.*
            FROM work_items w
            JOIN work_item_sessions s ON s.work_item_id = w.id
            WHERE s.session_id = ?
            LIMIT 1
            "#,
        )
        .bind(session_id)
        .fetch_optional(self.pool().await?)
        .await?;
        row.as_ref().map(work_item_from_row).transpose()
    }

    pub async fn list_project_runs(&self, project_id: &str) -> Result<Vec<Run>> {
        let rows = sqlx::query(
            r#"
            SELECT r.*
            FROM project_runs r
            JOIN work_items w ON w.id = r.work_item_id
            WHERE w.project_id = ?
            ORDER BY r.updated_at_ms DESC
            "#,
        )
        .bind(project_id)
        .fetch_all(self.pool().await?)
        .await?;
        rows.iter().map(run_from_row).collect()
    }

    pub async fn list_project_run_steps(&self, project_id: &str) -> Result<Vec<RunStep>> {
        let rows = sqlx::query(
            r#"
            SELECT s.*
            FROM run_steps s
            JOIN project_runs r ON r.id = s.run_id
            JOIN work_items w ON w.id = r.work_item_id
            WHERE w.project_id = ?
            ORDER BY r.updated_at_ms DESC, s.sequence ASC
            "#,
        )
        .bind(project_id)
        .fetch_all(self.pool().await?)
        .await?;
        rows.iter().map(run_step_from_row).collect()
    }

    pub async fn list_project_checkpoints(
        &self,
        project_id: &str,
    ) -> Result<Vec<WorkspaceCheckpoint>> {
        let rows = sqlx::query(
            r#"
            SELECT c.*
            FROM workspace_checkpoints c
            JOIN project_runs r ON r.id = c.run_id
            JOIN work_items w ON w.id = r.work_item_id
            WHERE w.project_id = ?
            ORDER BY c.created_at_ms DESC
            "#,
        )
        .bind(project_id)
        .fetch_all(self.pool().await?)
        .await?;
        rows.iter().map(checkpoint_from_row).collect()
    }

    pub async fn create_checkpoint(
        &self,
        run_id: &str,
        phase: RunPhase,
        state: serde_json::Value,
        safe_to_resume: bool,
    ) -> Result<WorkspaceCheckpoint> {
        self.get_run(run_id).await?;
        let pool = self.pool().await?;
        let mut tx = pool.begin().await?;
        let sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM workspace_checkpoints WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_one(&mut *tx)
        .await?;
        let id = format!("checkpoint_{}", Uuid::new_v4());
        let created_at_ms = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO workspace_checkpoints (
                id, run_id, sequence, phase, state_json, safe_to_resume, created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(run_id)
        .bind(sequence)
        .bind(phase.as_str())
        .bind(serde_json::to_string(&state)?)
        .bind(safe_to_resume)
        .bind(created_at_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(WorkspaceCheckpoint {
            id,
            run_id: run_id.to_string(),
            sequence,
            phase,
            state,
            safe_to_resume,
            created_at_ms,
        })
    }

    pub async fn ensure_change_set(
        &self,
        run_id: &str,
        base_revision: Option<&str>,
    ) -> Result<ChangeSet> {
        self.get_run(run_id).await?;
        let id = format!("changes_{}", Uuid::new_v4());
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO change_sets (
                id, run_id, base_revision, status, summary, created_at_ms, updated_at_ms
            ) VALUES (?, ?, ?, ?, '', ?, ?)
            ON CONFLICT(run_id) DO NOTHING
            "#,
        )
        .bind(&id)
        .bind(run_id)
        .bind(base_revision)
        .bind(ChangeSetStatus::Open.as_str())
        .bind(now)
        .bind(now)
        .execute(self.pool().await?)
        .await?;
        self.get_change_set_for_run(run_id).await
    }

    pub async fn get_change_set_for_run(&self, run_id: &str) -> Result<ChangeSet> {
        let row = sqlx::query("SELECT * FROM change_sets WHERE run_id = ?")
            .bind(run_id)
            .fetch_one(self.pool().await?)
            .await?;
        Ok(ChangeSet {
            id: row.try_get("id")?,
            run_id: row.try_get("run_id")?,
            base_revision: row.try_get("base_revision")?,
            status: ChangeSetStatus::parse(row.try_get("status")?)?,
            summary: row.try_get("summary")?,
            created_at_ms: row.try_get("created_at_ms")?,
            updated_at_ms: row.try_get("updated_at_ms")?,
        })
    }

    pub async fn interrupt_abandoned_runs(&self) -> Result<u64> {
        let now = Utc::now().timestamp_millis();
        let pool = self.pool().await?;
        let mut tx = pool.begin().await?;
        let result = sqlx::query(
            r#"
            UPDATE project_runs
            SET status = 'interrupted', updated_at_ms = ?, finished_at_ms = ?,
                error_code = 'process_restarted',
                error_message = 'Lumina restarted before this run reached a terminal state'
            WHERE status = 'running'
            "#,
        )
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            UPDATE work_items
            SET status = 'interrupted', updated_at_ms = ?
            WHERE id IN (
                SELECT work_item_id FROM project_runs WHERE status = 'interrupted'
            ) AND status IN ('running', 'validating')
            "#,
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            UPDATE run_steps
            SET status = 'skipped', finished_at_ms = ?
            WHERE run_id IN (
                SELECT id FROM project_runs WHERE status = 'interrupted'
            ) AND status IN ('running', 'pending')
            "#,
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }
}

fn normalized_project_name(name: Option<&str>, canonical_root: &Path) -> Result<String> {
    let candidate = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            canonical_root
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
        })
        .context("project name is required")?;
    ensure!(candidate.chars().count() <= 160, "project name is too long");
    Ok(candidate)
}

fn phase_step_sequence(phase: RunPhase) -> i64 {
    match phase {
        RunPhase::Discovering => 1,
        RunPhase::Planning => 2,
        RunPhase::AwaitingApproval => 3,
        RunPhase::Executing => 4,
        RunPhase::Validating => 5,
        RunPhase::Reviewing | RunPhase::Complete => 6,
    }
}

fn detect_project_profile(root: &Path) -> ProjectProfile {
    let candidates = [
        ("Cargo.toml", "Rust"),
        ("package.json", "JavaScript"),
        ("pnpm-workspace.yaml", "pnpm workspace"),
        ("pyproject.toml", "Python"),
        ("go.mod", "Go"),
        ("pom.xml", "Maven"),
        ("build.gradle.kts", "Gradle"),
    ];
    let mut manifests = Vec::new();
    let mut detected_stacks = Vec::new();
    for (manifest, stack) in candidates {
        if root.join(manifest).is_file() {
            manifests.push(manifest.to_string());
            detected_stacks.push(stack.to_string());
        }
    }
    ProjectProfile {
        manifests,
        detected_stacks,
        git_repository: root.join(".git").exists(),
    }
}

fn project_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Project> {
    Ok(Project {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        root_path: PathBuf::from(row.try_get::<String, _>("root_path")?),
        canonical_root: PathBuf::from(row.try_get::<String, _>("canonical_root")?),
        profile: serde_json::from_str(row.try_get("profile_json")?)?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        last_opened_at_ms: row.try_get("last_opened_at_ms")?,
        archived_at_ms: row.try_get("archived_at_ms")?,
    })
}

fn work_item_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<WorkItem> {
    Ok(WorkItem {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        title: row.try_get("title")?,
        objective: row.try_get("objective")?,
        acceptance_criteria: serde_json::from_str(row.try_get("acceptance_criteria_json")?)?,
        status: WorkItemStatus::parse(row.try_get("status")?)?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        completed_at_ms: row.try_get("completed_at_ms")?,
    })
}

fn run_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Run> {
    Ok(Run {
        id: row.try_get("id")?,
        work_item_id: row.try_get("work_item_id")?,
        session_id: row.try_get("session_id")?,
        status: RunStatus::parse(row.try_get("status")?)?,
        phase: RunPhase::parse(row.try_get("phase")?)?,
        started_at_ms: row.try_get("started_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        finished_at_ms: row.try_get("finished_at_ms")?,
        error_code: row.try_get("error_code")?,
        error_message: row.try_get("error_message")?,
    })
}

fn run_step_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<RunStep> {
    Ok(RunStep {
        id: row.try_get("id")?,
        run_id: row.try_get("run_id")?,
        sequence: row.try_get("sequence")?,
        kind: row.try_get("kind")?,
        title: row.try_get("title")?,
        status: RunStepStatus::parse(row.try_get("status")?)?,
        detail: serde_json::from_str(row.try_get("detail_json")?)?,
        started_at_ms: row.try_get("started_at_ms")?,
        finished_at_ms: row.try_get("finished_at_ms")?,
    })
}

fn checkpoint_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<WorkspaceCheckpoint> {
    Ok(WorkspaceCheckpoint {
        id: row.try_get("id")?,
        run_id: row.try_get("run_id")?,
        sequence: row.try_get("sequence")?,
        phase: RunPhase::parse(row.try_get("phase")?)?,
        state: serde_json::from_str(row.try_get("state_json")?)?,
        safe_to_resume: row.try_get("safe_to_resume")?,
        created_at_ms: row.try_get("created_at_ms")?,
    })
}
