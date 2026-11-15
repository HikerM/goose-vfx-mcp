use super::*;
use crate::config::LuminaMode;
use crate::project::ProjectService;
use crate::session::{SessionManager, SessionType};
use serde_json::json;
use std::sync::Arc;

const ASSET: &str = "00000000-0000-4000-8000-000000000101";
const MISSING: &str = "00000000-0000-4000-8000-000000000102";
const JOB: &str = "bounded-read";

struct Fixture {
    manager: SessionManager,
    assets: ProjectAssetStore,
    source: String,
    peer: String,
    other: String,
    other_project: String,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let manager = SessionManager::new(directory.path().join("data"));
        let projects = ProjectService::new(Arc::clone(manager.storage()));
        let mut project_ids = Vec::new();
        for name in ["first", "second"] {
            let path = directory.path().join(name);
            std::fs::create_dir_all(&path)?;
            project_ids.push(projects.open_project(&path, Some(name)).await?.id);
        }
        let mut sessions = Vec::new();
        for project in [&project_ids[0], &project_ids[0], &project_ids[1]] {
            let session = manager
                .create_session(
                    directory.path().to_path_buf(),
                    "Bounded project asset test".into(),
                    SessionType::Hidden,
                    LuminaMode::Auto,
                )
                .await?;
            manager
                .update(&session.id)
                .project_id(Some(project.clone()))
                .apply()
                .await?;
            sessions.push(session.id);
        }
        let assets = ProjectAssetStore::new(manager.storage().pool().await?.clone());
        assets.initialize().await?;
        for session in &sessions {
            assets.freeze_job(session, JOB, None).await?;
        }
        let mut tx = assets.pool.begin().await?;
        ProjectAssetStore::record_in_tx(&mut tx, &sessions[0], JOB, None, ASSET, &json!({
            "kind":"media",
            "result":{"resultId":ASSET,"mime":"audio/wav","fileName":"result.wav","bytes":48,
                "sha256":"a".repeat(64),"spec":{"kind":"audio","sampleRate":16000,"channels":1,"sampleFrames":2}},
            "configuration":{"text":"中文 🎵 quote: \" and slash: \\"}
        })).await?;
        tx.commit().await?;
        Ok(Self {
            manager,
            assets,
            source: sessions[0].clone(),
            peer: sessions[1].clone(),
            other: sessions[2].clone(),
            other_project: project_ids[1].clone(),
            _directory: directory,
        })
    }

    async fn raw_payload(&self) -> Result<String> {
        Ok(
            sqlx::query_scalar("SELECT content_json FROM project_assets_v1 WHERE asset_id=?")
                .bind(ASSET)
                .fetch_one(&self.assets.pool)
                .await?,
        )
    }

    async fn read(
        &self,
        session: &str,
        asset: &str,
        bytes: usize,
    ) -> Result<Option<(ProjectAssetRecord, usize)>> {
        let mut tx = self.assets.pool.begin().await?;
        let result = ProjectAssetStore::read_for_frozen_job_bounded_in_tx(
            &mut tx, session, JOB, asset, bytes,
        )
        .await;
        tx.rollback().await?;
        result
    }
}

#[tokio::test]
async fn bounded_project_asset_read_uses_exact_utf8_bytes_and_preserves_record() -> Result<()> {
    let f = Fixture::new().await?;
    let payload = f.raw_payload().await?;
    assert!(payload.len() > payload.chars().count());
    let (record, bytes) = f.read(&f.peer, ASSET, payload.len()).await?.unwrap();
    assert_eq!(bytes, payload.len());
    assert_eq!(record, f.assets.read(&f.peer, ASSET).await?.unwrap());
    assert_eq!(record.source_session_id, f.source);
    let error = f.read(&f.peer, ASSET, payload.len() - 1).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("Project reuse payload exceeds its byte bound"));
    assert_eq!(
        f.read(&f.peer, ASSET, payload.chars().count())
            .await
            .unwrap_err()
            .to_string(),
        format!(
            "Project reuse payload exceeds its byte bound: {} bytes requested, {} bytes remaining",
            payload.len(),
            payload.chars().count()
        )
    );
    assert_eq!(
        f.read(&f.peer, ASSET, usize::MAX).await?.unwrap().1,
        payload.len()
    );
    f.assets.pool.close().await;
    Ok(())
}

#[tokio::test]
async fn bounded_project_asset_read_rejects_oversize_before_parsing_invalid_json() -> Result<()> {
    let f = Fixture::new().await?;
    let invalid = "!".repeat(1024);
    sqlx::query("UPDATE project_assets_v1 SET content_json=? WHERE asset_id=?")
        .bind(&invalid)
        .bind(ASSET)
        .execute(&f.assets.pool)
        .await?;
    let error = f.read(&f.peer, ASSET, 1023).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Project reuse payload exceeds its byte bound: 1024 bytes requested, 1023 bytes remaining"
    );
    let parse_error = f.read(&f.peer, ASSET, 1024).await.unwrap_err();
    assert!(parse_error.to_string().contains("expected value"));
    f.assets.pool.close().await;
    Ok(())
}

#[tokio::test]
async fn bounded_project_asset_read_preserves_missing_and_project_isolation() -> Result<()> {
    let f = Fixture::new().await?;
    assert!(f.read(&f.peer, MISSING, 4096).await?.is_none());
    assert!(f.read(&f.other, ASSET, 4096).await?.is_none());
    assert!(f
        .read(&f.peer, ASSET, 0)
        .await
        .unwrap_err()
        .to_string()
        .contains("no remaining project payload byte budget"));
    f.manager
        .update(&f.peer)
        .project_id(Some(f.other_project.clone()))
        .apply()
        .await?;
    assert!(f
        .read(&f.peer, ASSET, 4096)
        .await
        .unwrap_err()
        .to_string()
        .contains("Task project differs from the current session project"));
    let mut tx = f.assets.pool.begin().await?;
    assert!(ProjectAssetStore::read_for_frozen_job_bounded_in_tx(
        &mut tx,
        &f.source,
        "not-frozen",
        ASSET,
        4096
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("no frozen execution scope"));
    tx.rollback().await?;
    f.assets.pool.close().await;
    Ok(())
}

#[tokio::test]
async fn bounded_project_asset_read_caps_summary_before_json_validation() -> Result<()> {
    let f = Fixture::new().await?;
    let mut tx = f.assets.pool.begin().await?;
    sqlx::query("PRAGMA ignore_check_constraints=ON")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE project_assets_v1 SET summary_json=? WHERE asset_id=?")
        .bind("!".repeat(MAX_SUMMARY_BYTES + 1))
        .bind(ASSET)
        .execute(&mut *tx)
        .await?;
    let error = ProjectAssetStore::read_for_frozen_job_bounded_in_tx(
        &mut tx,
        &f.peer,
        JOB,
        ASSET,
        MAX_CONTENT_BYTES,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("Project reuse payload summary exceeds its {MAX_SUMMARY_BYTES}-byte bound")
    );
    sqlx::query("PRAGMA ignore_check_constraints=OFF")
        .execute(&mut *tx)
        .await?;
    tx.rollback().await?;
    f.assets.pool.close().await;
    Ok(())
}
