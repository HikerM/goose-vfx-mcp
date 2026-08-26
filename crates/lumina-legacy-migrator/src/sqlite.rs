use anyhow::{bail, Context, Result};
use serde_json::Value as JsonValue;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection, Row};
use std::fs;
use std::path::{Path, PathBuf};

const LUMINA_SESSION_SCHEMA_VERSION: i32 = 16;

pub async fn backup_database(source: &Path, target: &Path) -> Result<()> {
    let parent = target
        .parent()
        .context("SQLite migration target has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temporary = sqlite_temporary_path(target);
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }

    let options = SqliteConnectOptions::new()
        .filename(source)
        .read_only(true)
        .create_if_missing(false);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .with_context(|| format!("failed to open legacy database {}", source.display()))?;

    sqlx::query("VACUUM INTO ?")
        .bind(temporary.to_string_lossy().as_ref())
        .execute(&mut connection)
        .await
        .with_context(|| format!("failed to snapshot legacy database {}", source.display()))?;
    connection.close().await?;
    fs::rename(&temporary, target)?;
    Ok(())
}

pub async fn import_session_database(source: &Path, target: &Path) -> Result<()> {
    let staging = sqlite_temporary_path(target);
    if staging.exists() {
        fs::remove_file(&staging)?;
    }

    let result = import_session_database_into(source, &staging).await;
    match result {
        Ok(()) => {
            fs::rename(&staging, target)?;
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&staging);
            Err(error)
        }
    }
}

async fn import_session_database_into(source: &Path, target: &Path) -> Result<()> {
    backup_database(source, target).await?;

    let options = SqliteConnectOptions::new()
        .filename(target)
        .read_only(false)
        .create_if_missing(false);
    let mut connection = SqliteConnection::connect_with(&options).await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut connection)
        .await?;
    if integrity != "ok" {
        bail!("legacy session database failed integrity_check: {integrity}");
    }

    let sessions_exist: i32 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sessions'",
    )
    .fetch_one(&mut connection)
    .await?;
    if sessions_exist == 0 {
        bail!("legacy session database has no sessions table");
    }

    let has_lumina_mode: i32 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='lumina_mode'",
    )
    .fetch_one(&mut connection)
    .await?;
    if has_lumina_mode == 0 {
        sqlx::query("ALTER TABLE sessions ADD COLUMN lumina_mode TEXT NOT NULL DEFAULT 'auto'")
            .execute(&mut connection)
            .await?;
    }

    let has_legacy_mode: i32 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='goose_mode'",
    )
    .fetch_one(&mut connection)
    .await?;
    if has_legacy_mode > 0 {
        sqlx::query("UPDATE sessions SET lumina_mode = goose_mode")
            .execute(&mut connection)
            .await?;
        sqlx::query("ALTER TABLE sessions DROP COLUMN goose_mode")
            .execute(&mut connection)
            .await?;
    }

    let recipe_rows = sqlx::query(
        "SELECT id, recipe_json FROM sessions WHERE recipe_json IS NOT NULL AND recipe_json <> ''",
    )
    .fetch_all(&mut connection)
    .await?;
    for row in recipe_rows {
        let session_id: String = row.try_get("id")?;
        let recipe_json: String = row.try_get("recipe_json")?;
        let mut recipe: JsonValue = serde_json::from_str(&recipe_json)
            .with_context(|| format!("session {session_id} contains invalid legacy recipe JSON"))?;
        crate::transform_legacy_json(&mut recipe);
        sqlx::query("UPDATE sessions SET recipe_json = ? WHERE id = ?")
            .bind(serde_json::to_string(&recipe)?)
            .bind(session_id)
            .execute(&mut connection)
            .await?;
    }

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY, applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP)",
    )
    .execute(&mut connection)
    .await?;
    let latest = sqlx::query("SELECT MAX(version) AS version FROM schema_version")
        .fetch_one(&mut connection)
        .await?
        .try_get::<Option<i32>, _>("version")?
        .unwrap_or(0);
    if latest > LUMINA_SESSION_SCHEMA_VERSION {
        bail!(
            "session database schema v{latest} is newer than Lumina v{LUMINA_SESSION_SCHEMA_VERSION}"
        );
    }
    if latest == LUMINA_SESSION_SCHEMA_VERSION - 1 {
        sqlx::query("INSERT OR IGNORE INTO schema_version(version) VALUES (?)")
            .bind(LUMINA_SESSION_SCHEMA_VERSION)
            .execute(&mut connection)
            .await?;
    }
    connection.close().await?;
    Ok(())
}

fn sqlite_temporary_path(target: &Path) -> PathBuf {
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("database.db");
    target.with_file_name(format!(
        ".{file_name}.lumina-sqlite-{}.tmp",
        std::process::id()
    ))
}
