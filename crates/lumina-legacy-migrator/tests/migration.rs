use lumina_legacy_migrator::{migrate, MigrationRequest, MigrationStatus, StorageLayout};
use serde_json::Value as JsonValue;
use serde_yaml::Value as YamlValue;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection, Row};
use std::fs;
use tempfile::tempdir;

#[tokio::test]
async fn imports_config_desktop_and_session_data_without_mutating_the_source() {
    let temp = tempdir().unwrap();
    let source = StorageLayout::from_root("legacy-fixture", temp.path().join("source"));
    let target = StorageLayout::from_root("lumina-fixture", temp.path().join("target"));

    fs::create_dir_all(&source.config).unwrap();
    fs::create_dir_all(&source.desktop).unwrap();
    fs::create_dir_all(source.data.join("sessions")).unwrap();
    fs::write(
        source.data.join("schedule.json"),
        r#"{"launch":"goose","callback":"goose://recipe/run","settings":{"goose_mode":"auto"}}"#,
    )
    .unwrap();
    fs::write(
        source.config.join("config.yaml"),
        "goose_provider: openai\ngoose_model: test-model\nGOOSE_MODE: auto\n",
    )
    .unwrap();
    fs::write(
        source.desktop.join("settings.json"),
        r#"{"externalGoosed":{"enabled":true,"url":"http://127.0.0.1:3000"}}"#,
    )
    .unwrap();

    let source_database = source.data.join("sessions/sessions.db");
    let options = SqliteConnectOptions::new()
        .filename(&source_database)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    sqlx::query(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, goose_mode TEXT NOT NULL DEFAULT 'auto', recipe_json TEXT)",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO sessions(id, goose_mode, recipe_json) VALUES ('session-1', 'chat', '{"settings":{"goose_provider":"openai","goose_model":"test-model"}}')"#,
    )
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE schema_version (version INTEGER PRIMARY KEY)")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO schema_version(version) VALUES (15)")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    let report = migrate(MigrationRequest {
        sources: vec![source.clone()],
        target: target.clone(),
        dry_run: false,
        migrate_secrets: false,
    })
    .await
    .unwrap();

    assert_eq!(report.status, MigrationStatus::Complete);
    assert!(source.config.join("config.yaml").is_file());
    let migrated_config: YamlValue =
        serde_yaml::from_str(&fs::read_to_string(target.config.join("config.yaml")).unwrap())
            .unwrap();
    assert_eq!(migrated_config["lumina_provider"], "openai");
    assert_eq!(migrated_config["lumina_model"], "test-model");
    assert_eq!(migrated_config["LUMINA_MODE"], "auto");

    let desktop: JsonValue =
        serde_json::from_str(&fs::read_to_string(target.desktop.join("settings.json")).unwrap())
            .unwrap();
    assert!(desktop.get("externalGoosed").is_none());
    assert_eq!(desktop["externalLuminad"]["enabled"], true);

    let schedule: JsonValue =
        serde_json::from_str(&fs::read_to_string(target.data.join("schedule.json")).unwrap())
            .unwrap();
    assert_eq!(schedule["launch"], "lumina");
    assert_eq!(schedule["callback"], "lumina://recipe/run");
    assert_eq!(schedule["settings"]["lumina_mode"], "auto");

    let target_database = target.data.join("sessions/sessions.db");
    let options = SqliteConnectOptions::new()
        .filename(&target_database)
        .create_if_missing(false);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    let row = sqlx::query("SELECT lumina_mode FROM sessions WHERE id = 'session-1'")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("lumina_mode"), "chat");
    let legacy_columns: i32 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='goose_mode'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(legacy_columns, 0);
    let recipe_json: String =
        sqlx::query_scalar("SELECT recipe_json FROM sessions WHERE id = 'session-1'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let recipe: JsonValue = serde_json::from_str(&recipe_json).unwrap();
    assert_eq!(recipe["settings"]["lumina_provider"], "openai");
    assert!(recipe["settings"].get("goose_provider").is_none());
    let version: i32 = sqlx::query_scalar("SELECT MAX(version) FROM schema_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 16);
}

#[tokio::test]
async fn dry_run_does_not_create_the_lumina_tree() {
    let temp = tempdir().unwrap();
    let source = StorageLayout::from_root("legacy-fixture", temp.path().join("source"));
    let target_root = temp.path().join("target");
    let target = StorageLayout::from_root("lumina-fixture", &target_root);
    fs::create_dir_all(&source.config).unwrap();
    fs::write(
        source.config.join("config.yaml"),
        "goose_model: test-model\n",
    )
    .unwrap();

    let report = migrate(MigrationRequest {
        sources: vec![source],
        target,
        dry_run: true,
        migrate_secrets: false,
    })
    .await
    .unwrap();

    assert_eq!(report.status, MigrationStatus::DryRun);
    assert!(!target_root.exists());
}
