use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqliteRow, Pool, Row, Sqlite, Transaction};

const MAX_CONTENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SUMMARY_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAssetRecord {
    pub asset_id: String,
    pub project_id: String,
    pub source_session_id: String,
    pub job_id: String,
    pub work_item_id: Option<String>,
    pub run_id: Option<String>,
    pub created_at_ms: i64,
    pub content: Value,
    pub content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAssetSummary {
    pub asset_id: String,
    pub project_id: String,
    pub source_session_id: String,
    pub job_id: String,
    pub work_item_id: Option<String>,
    pub run_id: Option<String>,
    pub created_at_ms: i64,
    pub content_sha256: String,
    pub kind: String,
    pub output: Value,
}

#[derive(Clone)]
pub struct ProjectAssetStore {
    pool: Pool<Sqlite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    project_id: Option<String>,
    work_item_id: Option<String>,
    run_id: Option<String>,
    call_id: Option<String>,
}

fn identifier(value: &str, max: usize) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control),
        "Invalid project asset identifier"
    );
    Ok(())
}

fn asset_identifier(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.to_string() == value,
        "Invalid project asset UUID"
    );
    Ok(())
}

fn canonical_content(content: &Value) -> Result<(String, String)> {
    let mut content = content.clone();
    content.sort_all_objects();
    let json = serde_json::to_string(&content)?;
    ensure!(
        json.len() <= MAX_CONTENT_BYTES,
        "Project asset content exceeds its byte limit"
    );
    let digest = Sha256::digest(json.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((json, digest))
}

fn content_summary(content: &Value) -> Result<Value> {
    let kind = content
        .get("kind")
        .and_then(Value::as_str)
        .context("Project asset kind is missing")?;
    let (source, fields): (&Value, &[&str]) = match kind {
        "media" => (
            &content["result"],
            &["resultId", "mime", "fileName", "bytes", "sha256", "spec"],
        ),
        "transcript" => (&content["transcript"], &["bytes", "sha256"]),
        _ => bail!("Unsupported project asset kind"),
    };
    let source = source
        .as_object()
        .context("Project asset output is missing")?;
    let mut output = serde_json::Map::new();
    for field in fields {
        output.insert(
            (*field).into(),
            source
                .get(*field)
                .context("Project asset summary field is missing")?
                .clone(),
        );
    }
    ensure!(
        output["bytes"].as_u64().is_some() && output["sha256"].as_str().is_some(),
        "Invalid project asset output summary"
    );
    if kind == "media" {
        asset_identifier(
            output["resultId"]
                .as_str()
                .context("Invalid media result ID")?,
        )?;
        ensure!(
            output["mime"].is_string()
                && output["fileName"].is_string()
                && output["spec"].is_object(),
            "Invalid media result summary"
        );
    }
    Ok(serde_json::json!({"kind":kind,"output":output}))
}

fn summary_from_row(row: &SqliteRow) -> Result<ProjectAssetSummary> {
    let payload: String = row.try_get("summary_json")?;
    ensure!(
        payload.len() <= MAX_SUMMARY_BYTES,
        "Project asset summary exceeds its byte limit"
    );
    let summary: Value = serde_json::from_str(&payload)?;
    let (canonical, digest) = canonical_content(&summary)?;
    ensure!(
        canonical == payload && digest == row.try_get::<String, _>("summary_sha256")?,
        "Project asset summary integrity mismatch"
    );
    let kind = summary["kind"]
        .as_str()
        .context("Invalid archived asset kind")?;
    ensure!(
        matches!(kind, "media" | "transcript"),
        "Invalid archived asset kind"
    );
    let source_key = if kind == "media" {
        "result"
    } else {
        "transcript"
    };
    let mut source = serde_json::Map::new();
    source.insert("kind".into(), Value::String(kind.into()));
    source.insert(source_key.into(), summary["output"].clone());
    ensure!(
        content_summary(&Value::Object(source))? == summary,
        "Invalid archived output summary fields"
    );
    let record = ProjectAssetSummary {
        asset_id: row.try_get("asset_id")?,
        project_id: row.try_get("project_id")?,
        source_session_id: row.try_get("source_session_id")?,
        job_id: row.try_get("job_id")?,
        work_item_id: row.try_get("work_item_id")?,
        run_id: row.try_get("run_id")?,
        created_at_ms: row.try_get("created_at_ms")?,
        content_sha256: row.try_get("content_sha256")?,
        kind: kind.into(),
        output: summary["output"].clone(),
    };
    asset_identifier(&record.asset_id)?;
    identifier(&record.project_id, 96)?;
    identifier(&record.source_session_id, 96)?;
    identifier(&record.job_id, 96)?;
    ensure!(
        record.content_sha256.len() == 64
            && record
                .content_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Invalid archived content digest"
    );
    ensure!(
        record.run_id.is_none() || record.work_item_id.is_some(),
        "Invalid archived run scope"
    );
    if record.kind == "media" {
        ensure!(
            record.output["resultId"].as_str() == Some(record.asset_id.as_str()),
            "Project asset result identity differs"
        );
    }
    Ok(record)
}

fn scope_from_row(row: &SqliteRow) -> Result<Scope> {
    let scope = Scope {
        project_id: row.try_get("project_id")?,
        work_item_id: row.try_get("work_item_id")?,
        run_id: row.try_get("run_id")?,
        call_id: row.try_get("call_id")?,
    };
    ensure!(
        scope.project_id.is_some() || (scope.work_item_id.is_none() && scope.run_id.is_none()),
        "Invalid frozen project scope"
    );
    ensure!(
        scope.run_id.is_none() || scope.work_item_id.is_some(),
        "Invalid frozen run scope"
    );
    Ok(scope)
}

fn asset_from_row(row: &SqliteRow) -> Result<ProjectAssetRecord> {
    let payload: String = row.try_get("content_json")?;
    ensure!(
        payload.len() <= MAX_CONTENT_BYTES,
        "Project asset content exceeds its byte limit"
    );
    let content: Value = serde_json::from_str(&payload)?;
    let (canonical, digest) = canonical_content(&content)?;
    let content_sha256: String = row.try_get("content_sha256")?;
    ensure!(
        payload == canonical && digest == content_sha256,
        "Project asset content integrity mismatch"
    );
    let summary = summary_from_row(row)?;
    ensure!(
        content_summary(&content)?
            == serde_json::json!({"kind":summary.kind,"output":summary.output}),
        "Project asset summary differs from archived content"
    );
    let record = ProjectAssetRecord {
        asset_id: row.try_get("asset_id")?,
        project_id: row.try_get("project_id")?,
        source_session_id: row.try_get("source_session_id")?,
        job_id: row.try_get("job_id")?,
        work_item_id: row.try_get("work_item_id")?,
        run_id: row.try_get("run_id")?,
        created_at_ms: row.try_get("created_at_ms")?,
        content,
        content_sha256,
    };
    asset_identifier(&record.asset_id)?;
    identifier(&record.project_id, 96)?;
    identifier(&record.source_session_id, 96)?;
    identifier(&record.job_id, 96)?;
    ensure!(
        record.run_id.is_none() || record.work_item_id.is_some(),
        "Invalid archived run scope"
    );
    Ok(record)
}

impl ProjectAssetStore {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    pub async fn initialize(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS project_execution_scopes_v1 (
            source_session_id TEXT NOT NULL, scope_kind TEXT NOT NULL CHECK(scope_kind IN ('call','job')),
            scope_id TEXT NOT NULL, call_id TEXT, project_id TEXT, work_item_id TEXT, run_id TEXT,
            PRIMARY KEY(source_session_id,scope_kind,scope_id),
            CHECK(project_id IS NOT NULL OR (work_item_id IS NULL AND run_id IS NULL)),
            CHECK(run_id IS NULL OR work_item_id IS NOT NULL))")
            .execute(&mut *tx).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS project_assets_v1 (
            asset_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, source_session_id TEXT NOT NULL,
            job_id TEXT NOT NULL, call_id TEXT, work_item_id TEXT, run_id TEXT, created_at_ms INTEGER NOT NULL,
            content_json TEXT NOT NULL CHECK(length(CAST(content_json AS BLOB)) <= 33554432),
            content_sha256 TEXT NOT NULL CHECK(length(content_sha256)=64),
            summary_json TEXT NOT NULL CHECK(length(CAST(summary_json AS BLOB)) <= 8192),
            summary_sha256 TEXT NOT NULL CHECK(length(summary_sha256)=64),
            CHECK(run_id IS NULL OR work_item_id IS NOT NULL))")
            .execute(&mut *tx).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS project_assets_project_v1 ON project_assets_v1(project_id,asset_id)")
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn session_project(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
    ) -> Result<Option<String>> {
        identifier(session, 96)?;
        sqlx::query_scalar("SELECT project_id FROM sessions WHERE id=?")
            .bind(session)
            .fetch_optional(&mut **tx)
            .await?
            .context("Project asset session not found")
    }

    async fn current_scope(tx: &mut Transaction<'_, Sqlite>, session: &str) -> Result<Scope> {
        let project = Self::session_project(tx, session).await?;
        let mut scope = Scope {
            project_id: None,
            work_item_id: None,
            run_id: None,
            call_id: None,
        };
        let Some(project) = project.filter(|id| id.starts_with("project_")) else {
            return Ok(scope);
        };
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id=?)")
            .bind(&project)
            .fetch_one(&mut **tx)
            .await?;
        ensure!(exists, "Project asset session refers to a missing project");
        if let Some(work) = sqlx::query("SELECT s.work_item_id AS id,w.project_id FROM work_item_sessions s LEFT JOIN work_items w ON w.id=s.work_item_id WHERE s.session_id=?")
            .bind(session).fetch_optional(&mut **tx).await? {
            ensure!(work.try_get::<String,_>("project_id")? == project, "Session work item belongs to a different project");
            scope.work_item_id = Some(work.try_get("id")?);
        }
        if let Some(run) = sqlx::query(
            "SELECT id,work_item_id FROM project_runs WHERE session_id=? AND status='running'",
        )
        .bind(session)
        .fetch_optional(&mut **tx)
        .await?
        {
            let work: String = run.try_get("work_item_id")?;
            ensure!(
                scope.work_item_id.as_ref() == Some(&work),
                "Session run does not match its work item"
            );
            scope.run_id = Some(run.try_get("id")?);
        }
        scope.project_id = Some(project);
        Ok(scope)
    }

    async fn scope(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
        kind: &str,
        id: &str,
    ) -> Result<Option<Scope>> {
        sqlx::query("SELECT project_id,work_item_id,run_id,call_id FROM project_execution_scopes_v1 WHERE source_session_id=? AND scope_kind=? AND scope_id=?")
            .bind(session).bind(kind).bind(id).fetch_optional(&mut **tx).await?
            .as_ref().map(scope_from_row).transpose()
    }

    async fn insert_scope(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
        kind: &str,
        id: &str,
        scope: &Scope,
    ) -> Result<()> {
        sqlx::query("INSERT INTO project_execution_scopes_v1(source_session_id,scope_kind,scope_id,call_id,project_id,work_item_id,run_id) VALUES(?,?,?,?,?,?,?)")
            .bind(session).bind(kind).bind(id).bind(&scope.call_id).bind(&scope.project_id)
            .bind(&scope.work_item_id).bind(&scope.run_id).execute(&mut **tx).await?;
        Ok(())
    }

    async fn freeze_call_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
        call: &str,
    ) -> Result<Scope> {
        if let Some(scope) = Self::scope(tx, session, "call", call).await? {
            ensure!(
                scope.call_id.as_deref() == Some(call),
                "Frozen call identity differs"
            );
            return Ok(scope);
        }
        let mut scope = Self::current_scope(tx, session).await?;
        scope.call_id = Some(call.into());
        Self::insert_scope(tx, session, "call", call, &scope).await?;
        Ok(scope)
    }

    pub async fn freeze_call(&self, session: &str, call: &str) -> Result<()> {
        identifier(session, 96)?;
        identifier(call, 1024)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::freeze_call_in_tx(&mut tx, session, call).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn freeze_call_for_project(
        &self,
        session: &str,
        call: &str,
        expected_project: Option<&str>,
    ) -> Result<()> {
        identifier(session, 96)?;
        identifier(call, 1024)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let current = Self::session_project(&mut tx, session).await?;
        ensure!(
            current.as_deref() == expected_project,
            "Session project changed since reply context was loaded"
        );
        if let Some(scope) = Self::scope(&mut tx, session, "call", call).await? {
            let expected_database_project =
                expected_project.filter(|id| id.starts_with("project_"));
            ensure!(
                scope.project_id.as_deref() == expected_database_project,
                "Tool call belongs to a different frozen project"
            );
        }
        Self::freeze_call_in_tx(&mut tx, session, call).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn freeze_job(&self, session: &str, job: &str, call: Option<&str>) -> Result<()> {
        identifier(session, 96)?;
        identifier(job, 96)?;
        if let Some(call) = call {
            identifier(call, 1024)?;
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(scope) = Self::scope(&mut tx, session, "job", job).await? {
            if let Some(call) = call {
                ensure!(
                    scope.call_id.as_deref() == Some(call),
                    "Job belongs to a different frozen call"
                );
            }
        } else {
            let scope = match call {
                Some(call) => Self::freeze_call_in_tx(&mut tx, session, call).await?,
                None => Self::current_scope(&mut tx, session).await?,
            };
            Self::insert_scope(&mut tx, session, "job", job, &scope).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn has_project_scope_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
        job: &str,
        call: Option<&str>,
    ) -> Result<bool> {
        identifier(session, 96)?;
        identifier(job, 96)?;
        if let Some(call) = call {
            identifier(call, 1024)?;
        }
        let (kind, id) = call.map_or(("job", job), |call| ("call", call));
        Ok(Self::scope(tx, session, kind, id)
            .await?
            .is_some_and(|scope| scope.project_id.is_some()))
    }

    pub async fn record_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
        job: &str,
        call: Option<&str>,
        asset_id: &str,
        content: &Value,
    ) -> Result<()> {
        identifier(session, 96)?;
        identifier(job, 96)?;
        asset_identifier(asset_id)?;
        if let Some(call) = call {
            identifier(call, 1024)?;
        }
        let (kind, id) = call.map_or(("job", job), |call| ("call", call));
        let Some(scope) = Self::scope(tx, session, kind, id).await? else {
            return Ok(());
        };
        if let Some(call) = call {
            ensure!(
                scope.call_id.as_deref() == Some(call),
                "Frozen call identity differs"
            );
        }
        let Some(project) = &scope.project_id else {
            return Ok(());
        };
        let (payload, digest) = canonical_content(content)?;
        let summary = content_summary(content)?;
        if summary["kind"] == "media" {
            ensure!(
                summary["output"]["resultId"].as_str() == Some(asset_id),
                "Project asset result identity differs"
            );
        }
        let (summary_json, summary_digest) = canonical_content(&summary)?;
        ensure!(
            summary_json.len() <= MAX_SUMMARY_BYTES,
            "Project asset summary exceeds its byte limit"
        );
        sqlx::query("INSERT INTO project_assets_v1(asset_id,project_id,source_session_id,job_id,call_id,work_item_id,run_id,created_at_ms,content_json,content_sha256,summary_json,summary_sha256) VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(asset_id) DO NOTHING")
            .bind(asset_id).bind(project).bind(session).bind(job).bind(&scope.call_id).bind(&scope.work_item_id)
            .bind(&scope.run_id).bind(chrono::Utc::now().timestamp_millis()).bind(&payload).bind(&digest)
            .bind(summary_json).bind(summary_digest)
            .execute(&mut **tx).await?;
        let row = sqlx::query("SELECT * FROM project_assets_v1 WHERE asset_id=?")
            .bind(asset_id)
            .fetch_one(&mut **tx)
            .await?;
        let stored = asset_from_row(&row)?;
        ensure!(
            row.try_get::<Option<String>, _>("call_id")? == scope.call_id,
            "Project asset belongs to a different call"
        );
        ensure!(
            stored.project_id == *project
                && stored.source_session_id == session
                && stored.job_id == job
                && stored.work_item_id == scope.work_item_id
                && stored.run_id == scope.run_id
                && stored.content == *content
                && stored.content_sha256 == digest,
            "Project asset conflicts with immutable archive"
        );
        Ok(())
    }

    pub async fn list(
        &self,
        session: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ProjectAssetSummary>> {
        ensure!(
            (1..=100).contains(&limit),
            "Project asset page size must be between 1 and 100"
        );
        if let Some(after) = after {
            asset_identifier(after)?;
        }
        let mut tx = self.pool.begin().await?;
        let scope = Self::current_scope(&mut tx, session).await?;
        let Some(project) = scope.project_id else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query("SELECT asset_id,project_id,source_session_id,job_id,work_item_id,run_id,created_at_ms,content_sha256,summary_json,summary_sha256 FROM project_assets_v1 WHERE project_id=? AND asset_id>? ORDER BY asset_id LIMIT ?")
            .bind(project).bind(after.unwrap_or("")).bind(limit).fetch_all(&mut *tx).await?;
        let result = rows
            .iter()
            .map(summary_from_row)
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn read(&self, session: &str, asset_id: &str) -> Result<Option<ProjectAssetRecord>> {
        asset_identifier(asset_id)?;
        let mut tx = self.pool.begin().await?;
        let scope = Self::current_scope(&mut tx, session).await?;
        let Some(project) = scope.project_id else {
            return Ok(None);
        };
        let row = sqlx::query("SELECT * FROM project_assets_v1 WHERE project_id=? AND asset_id=?")
            .bind(project)
            .bind(asset_id)
            .fetch_optional(&mut *tx)
            .await?;
        let result = row.as_ref().map(asset_from_row).transpose()?;
        tx.commit().await?;
        Ok(result)
    }
    pub(crate) async fn project_for_frozen_job_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
        job: &str,
    ) -> Result<String> {
        identifier(job, 96)?;
        let current = Self::current_scope(tx, session).await?;
        let frozen = Self::scope(tx, session, "job", job)
            .await?
            .context("Project task has no frozen execution scope")?;
        let project = frozen
            .project_id
            .context("Task was not accepted in a project")?;
        ensure!(
            current.project_id.as_deref() == Some(project.as_str()),
            "Task project differs from the current session project"
        );
        Ok(project)
    }

    pub(crate) async fn read_for_frozen_job_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
        job: &str,
        asset_id: &str,
    ) -> Result<Option<ProjectAssetRecord>> {
        asset_identifier(asset_id)?;
        let project = Self::project_for_frozen_job_in_tx(tx, session, job).await?;
        let row = sqlx::query("SELECT * FROM project_assets_v1 WHERE project_id=? AND asset_id=?")
            .bind(project)
            .bind(asset_id)
            .fetch_optional(&mut **tx)
            .await?;
        row.as_ref().map(asset_from_row).transpose()
    }

    /// Applies the caller's remaining raw UTF-8 byte budget before returning JSON
    /// from SQLite. The returned count includes the original encoded archive.
    pub(crate) async fn read_for_frozen_job_bounded_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        session: &str,
        job: &str,
        asset_id: &str,
        max_bytes: usize,
    ) -> Result<Option<(ProjectAssetRecord, usize)>> {
        asset_identifier(asset_id)?;
        let project = Self::project_for_frozen_job_in_tx(tx, session, job).await?;
        let limit = max_bytes.min(MAX_CONTENT_BYTES);
        ensure!(
            limit > 0,
            "Project reuse payload has no remaining project payload byte budget"
        );
        let row = sqlx::query(
            "SELECT asset_id,project_id,source_session_id,job_id,work_item_id,run_id,created_at_ms,
                length(CAST(content_json AS BLOB)) AS content_bytes,
                CASE WHEN length(CAST(content_json AS BLOB))<=? THEN content_json END AS content_json,
                content_sha256,
                length(CAST(summary_json AS BLOB)) AS summary_bytes,
                CASE WHEN length(CAST(summary_json AS BLOB))<=? THEN summary_json END AS summary_json,
                summary_sha256
             FROM project_assets_v1 WHERE project_id=? AND asset_id=?",
        )
        .bind(i64::try_from(limit)?)
        .bind(i64::try_from(MAX_SUMMARY_BYTES)?)
        .bind(project)
        .bind(asset_id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let bytes = usize::try_from(row.try_get::<i64, _>("content_bytes")?)?;
        ensure!(bytes <= limit,
            "Project reuse payload exceeds its byte bound: {bytes} bytes requested, {limit} bytes remaining");
        let summary_bytes = usize::try_from(row.try_get::<i64, _>("summary_bytes")?)?;
        ensure!(
            summary_bytes <= MAX_SUMMARY_BYTES,
            "Project reuse payload summary exceeds its {MAX_SUMMARY_BYTES}-byte bound"
        );
        Ok(Some((asset_from_row(&row)?, bytes)))
    }
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "assets_bounded_tests.rs"]
mod bounded_tests;
