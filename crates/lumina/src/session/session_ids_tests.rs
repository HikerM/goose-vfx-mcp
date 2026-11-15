use super::*;
use crate::config::LuminaMode;
use crate::session::{SessionManager, SessionType};
use sqlx::sqlite::SqlitePoolOptions;
use std::path::PathBuf;

fn temporary_directory() -> Result<tempfile::TempDir> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/session-id-unit-tests");
    std::fs::create_dir_all(&base)?;
    Ok(tempfile::tempdir_in(base.canonicalize()?)?)
}

async fn memory_pool() -> Result<Pool<Sqlite>> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    sqlx::query("CREATE TABLE sessions(id TEXT PRIMARY KEY, parent_session_id TEXT)")
        .execute(&pool)
        .await?;
    Ok(pool)
}

async fn create(manager: &SessionManager) -> Result<String> {
    Ok(manager
        .create_session(
            PathBuf::new(),
            "ID allocation test".into(),
            SessionType::Hidden,
            LuminaMode::Auto,
        )
        .await?
        .id)
}

#[tokio::test]
async fn session_ids_do_not_reuse_deleted_maximum_or_all_deleted_after_reopen() -> Result<()> {
    let temp = temporary_directory()?;
    let manager = SessionManager::new(temp.path().to_path_buf());
    let first = create(&manager).await?;
    let second = create(&manager).await?;
    manager.delete_session(&second).await?;
    let third = create(&manager).await?;
    assert_ne!(third, second);
    manager.delete_session(&first).await?;
    manager.delete_session(&third).await?;
    manager.storage().pool().await?.close().await;
    drop(manager);
    let reopened = SessionManager::new(temp.path().to_path_buf());
    let fourth = create(&reopened).await?;
    assert!(![first, second, third].contains(&fourth));
    assert!(fourth
        .split_once('_')
        .is_some_and(|(day, suffix)| day.len() == 8 && suffix.parse::<i64>().is_ok()));
    reopened.storage().pool().await?.close().await;
    Ok(())
}

#[tokio::test]
async fn session_ids_seed_all_known_retained_references_during_upgrade() -> Result<()> {
    for &(table, column) in SOURCES {
        let pool = memory_pool().await?;
        if table != "sessions" {
            sqlx::query(&format!("CREATE TABLE {table} ({column} TEXT)"))
                .execute(&pool)
                .await?;
        }
        sqlx::query(&format!("INSERT INTO {table}({column}) VALUES(?)"))
            .bind("20261115_800")
            .execute(&pool)
            .await?;
        initialize(&pool).await?;
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let next = allocate(&mut tx, "20261115").await?;
        assert_eq!(
            next, "20261115_801",
            "Retained source {table}.{column} was omitted"
        );
        tx.commit().await?;
        pool.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn session_ids_upgrade_preserves_media_only_high_water_without_rescanning() -> Result<()> {
    let pool = memory_pool().await?;
    sqlx::query("CREATE TABLE media_results_v1(session_id TEXT)")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO media_results_v1 VALUES('20261115_9000')")
        .execute(&pool)
        .await?;
    initialize(&pool).await?;
    sqlx::query("DROP TABLE media_results_v1")
        .execute(&pool)
        .await?;
    initialize(&pool).await?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    assert_eq!(allocate(&mut tx, "20261115").await?, "20261115_9001");
    tx.commit().await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn session_ids_later_imports_advance_sequence_and_ignore_nonstandard_ids() -> Result<()> {
    let pool = memory_pool().await?;
    initialize(&pool).await?;
    for id in [
        "20261115_700",
        "20261115_2",
        "20261115X9999",
        "20261115_9999tail",
        "legacy-id",
        "20261116_12",
    ] {
        sqlx::query("INSERT INTO sessions(id) VALUES(?)")
            .bind(id)
            .execute(&pool)
            .await?;
    }
    sqlx::query("DELETE FROM sessions").execute(&pool).await?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    assert_eq!(allocate(&mut tx, "20261115").await?, "20261115_701");
    assert_eq!(allocate(&mut tx, "20261116").await?, "20261116_13");
    tx.commit().await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn session_ids_failed_session_insert_rolls_back_allocation() -> Result<()> {
    let temp = temporary_directory()?;
    let manager = SessionManager::new(temp.path().to_path_buf());
    let pool = manager.storage().pool().await?;
    let before: Vec<(String, i64)> =
        sqlx::query_as("SELECT day,high_water FROM session_id_sequences_v1 ORDER BY day")
            .fetch_all(pool)
            .await?;
    sqlx::query("CREATE TRIGGER reject_session_id_test BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT,'injected session insert failure'); END").execute(pool).await?;
    assert!(create(&manager).await.is_err());
    let after: Vec<(String, i64)> =
        sqlx::query_as("SELECT day,high_water FROM session_id_sequences_v1 ORDER BY day")
            .fetch_all(pool)
            .await?;
    assert_eq!(before, after);
    sqlx::query("DROP TRIGGER reject_session_id_test")
        .execute(pool)
        .await?;
    assert!(create(&manager).await?.ends_with("_1"));
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn session_ids_concurrent_creates_are_unique_across_managers() -> Result<()> {
    let temp = temporary_directory()?;
    let first = SessionManager::new(temp.path().to_path_buf());
    let second = SessionManager::new(temp.path().to_path_buf());
    let (a, b, c, d) = tokio::try_join!(
        create(&first),
        create(&second),
        create(&first),
        create(&second)
    )?;
    let unique = std::collections::BTreeSet::from([a, b, c, d]);
    assert_eq!(unique.len(), 4);
    first.storage().pool().await?.close().await;
    second.storage().pool().await?.close().await;
    Ok(())
}

#[tokio::test]
async fn session_ids_exhaustion_fails_without_mutating_high_water() -> Result<()> {
    let pool = memory_pool().await?;
    initialize(&pool).await?;
    sqlx::query("INSERT INTO sessions(id) VALUES('20261115_9223372036854775807')")
        .execute(&pool)
        .await?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    assert!(allocate(&mut tx, "20261115").await.is_err());
    tx.rollback().await?;
    let value: i64 =
        sqlx::query_scalar("SELECT high_water FROM session_id_sequences_v1 WHERE day='20261115'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(value, i64::MAX);
    pool.close().await;
    Ok(())
}
