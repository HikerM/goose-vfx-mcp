use anyhow::{ensure, Context, Result};
use sqlx::{Pool, Sqlite, Transaction};

const SOURCES: &[(&str, &str)] = &[
    ("sessions", "id"),
    ("sessions", "parent_session_id"),
    ("messages", "session_id"),
    ("usage_ledger", "session_id"),
    ("thread_messages", "session_id"),
    ("work_item_sessions", "session_id"),
    ("project_runs", "session_id"),
    ("media_results_v1", "session_id"),
    ("media_task_configurations_v1", "session_id"),
    ("media_task_transcripts_v1", "session_id"),
    ("native_task_calls_v1", "session_id"),
    ("project_execution_scopes_v1", "source_session_id"),
    ("project_assets_v1", "source_session_id"),
    ("project_task_configurations_v1", "session_id"),
    ("project_task_authorizations_v1", "session_id"),
];

fn numeric_session_id(value: &str) -> String {
    format!(
        "length({value}) BETWEEN 10 AND 28
        AND substr({value},1,8) GLOB '[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]'
        AND substr({value},9,1)='_'
        AND substr({value},10) NOT GLOB '*[^0-9]*'
        AND (length(substr({value},10))<19 OR substr({value},10)<='9223372036854775807')"
    )
}

pub(super) async fn initialize(pool: &Pool<Sqlite>) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_id_sequences_v1')")
        .fetch_one(&mut *tx).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS session_id_sequences_v1 (
        day TEXT PRIMARY KEY CHECK(length(day)=8 AND day NOT GLOB '*[^0-9]*'),
        high_water INTEGER NOT NULL CHECK(typeof(high_water)='integer' AND high_water>=0))",
    )
    .execute(&mut *tx)
    .await?;
    if !exists {
        for &(table, column) in SOURCES {
            let available: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?) WHERE name=?)",
            )
            .bind(table)
            .bind(column)
            .fetch_one(&mut *tx)
            .await?;
            if !available {
                continue;
            }
            let statement = format!(
                "INSERT INTO session_id_sequences_v1(day,high_water)
                SELECT substr({column},1,8),MAX(CAST(substr({column},10) AS INTEGER))
                FROM {table} WHERE {} GROUP BY substr({column},1,8)
                ON CONFLICT(day) DO UPDATE SET high_water=MAX(high_water,excluded.high_water)",
                numeric_session_id(column),
            );
            sqlx::query(&statement).execute(&mut *tx).await?;
        }
    }
    // Imports retain their original IDs, so they must advance the allocator too.
    let trigger = format!(
        "CREATE TRIGGER IF NOT EXISTS session_id_high_water_insert_v1 AFTER INSERT ON sessions
        WHEN {} BEGIN
            INSERT INTO session_id_sequences_v1(day,high_water)
            VALUES(substr(NEW.id,1,8),CAST(substr(NEW.id,10) AS INTEGER))
            ON CONFLICT(day) DO UPDATE SET high_water=MAX(high_water,excluded.high_water);
        END",
        numeric_session_id("NEW.id"),
    );
    sqlx::query(&trigger).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub(super) async fn allocate(tx: &mut Transaction<'_, Sqlite>, day: &str) -> Result<String> {
    ensure!(
        day.len() == 8 && day.bytes().all(|byte| byte.is_ascii_digit()),
        "Invalid session ID day"
    );
    let previous: Option<i64> =
        sqlx::query_scalar("SELECT high_water FROM session_id_sequences_v1 WHERE day=?")
            .bind(day)
            .fetch_optional(&mut **tx)
            .await?;
    let next = previous
        .unwrap_or(0)
        .checked_add(1)
        .context("Session ID sequence exhausted")?;
    sqlx::query(
        "INSERT INTO session_id_sequences_v1(day,high_water) VALUES(?,?)
        ON CONFLICT(day) DO UPDATE SET high_water=excluded.high_water",
    )
    .bind(day)
    .bind(next)
    .execute(&mut **tx)
    .await?;
    Ok(format!("{day}_{next}"))
}

#[cfg(test)]
#[path = "session_ids_tests.rs"]
mod tests;
