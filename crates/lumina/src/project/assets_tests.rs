use super::*;
use crate::config::LuminaMode;
use crate::project::{CreateWorkItem, ProjectService};
use crate::session::{SessionManager, SessionType};
use serde_json::json;
use std::sync::Arc;

const ASSET_A: &str = "00000000-0000-4000-8000-000000000001";
const ASSET_B: &str = "00000000-0000-4000-8000-000000000002";
const ASSET_C: &str = "00000000-0000-4000-8000-000000000003";

fn media_content(id: &str, configuration: Value) -> Value {
    json!({
        "kind":"media",
        "result":{"resultId":id,"mime":"audio/wav","fileName":"result.wav","bytes":48,
            "sha256":"a".repeat(64),"spec":{"kind":"audio","sampleRate":16000,"channels":1,"sampleFrames":2}},
        "configuration":configuration,
    })
}

struct Fixture {
    manager: SessionManager,
    projects: ProjectService,
    assets: ProjectAssetStore,
    project: String,
    other_project: String,
    session: String,
    peer: String,
    other: String,
    _root: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let manager = SessionManager::new(root.path().join("data"));
        let projects = ProjectService::new(Arc::clone(manager.storage()));
        let path = root.path().join("project");
        let other_path = root.path().join("other");
        std::fs::create_dir_all(&path)?;
        std::fs::create_dir_all(&other_path)?;
        let project = projects.open_project(&path, Some("First")).await?.id;
        let other_project = projects.open_project(&other_path, Some("Second")).await?.id;
        let mut sessions = Vec::new();
        for project_id in [&project, &project, &other_project] {
            let session = manager
                .create_session(
                    root.path().to_path_buf(),
                    "asset test".into(),
                    SessionType::Hidden,
                    LuminaMode::Auto,
                )
                .await?;
            manager
                .update(&session.id)
                .project_id(Some(project_id.clone()))
                .apply()
                .await?;
            sessions.push(session.id);
        }
        let assets = ProjectAssetStore::new(manager.storage().pool().await?.clone());
        assets.initialize().await?;
        Ok(Self {
            manager,
            projects,
            assets,
            project,
            other_project,
            session: sessions[0].clone(),
            peer: sessions[1].clone(),
            other: sessions[2].clone(),
            _root: root,
        })
    }

    async fn publish(
        &self,
        session: &str,
        job: &str,
        call: Option<&str>,
        asset: &str,
        content: Value,
    ) -> Result<()> {
        let content = if content.get("kind").is_some() {
            content
        } else {
            media_content(asset, content)
        };
        let mut tx = self.assets.pool.begin().await?;
        ProjectAssetStore::record_in_tx(&mut tx, session, job, call, asset, &content).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn close(&self) {
        self.assets.pool.close().await;
    }
}

#[tokio::test]
async fn project_assets_initialize_without_session_or_project_schema() -> Result<()> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    let assets = ProjectAssetStore::new(pool.clone());
    assets.initialize().await?;
    assets.initialize().await?;
    let mut tx = pool.begin().await?;
    assert!(
        !ProjectAssetStore::has_project_scope_in_tx(&mut tx, "old-session", "old-job", None)
            .await?
    );
    ProjectAssetStore::record_in_tx(
        &mut tx,
        "old-session",
        "old-job",
        None,
        ASSET_A,
        &json!({"result": "legacy"}),
    )
    .await?;
    tx.commit().await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_assets_v1")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_concurrent_call_and_job_freezes_share_project_without_snapshot_conflicts(
) -> Result<()> {
    let f = Fixture::new().await?;
    tokio::try_join!(
        f.assets.freeze_call(&f.session, "parallel-call"),
        f.assets
            .freeze_call_for_project(&f.session, "parallel-context-call", Some(&f.project)),
        f.assets.freeze_job(
            &f.session,
            "parallel-native-job",
            Some("parallel-native-call")
        ),
        f.assets.freeze_job(&f.session, "parallel-direct-job", None),
    )?;
    let projects: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT project_id FROM project_execution_scopes_v1 WHERE source_session_id=?",
    )
    .bind(&f.session)
    .fetch_all(&f.assets.pool)
    .await?;
    assert_eq!(projects.len(), 5);
    assert!(projects
        .iter()
        .all(|project| project.as_deref() == Some(f.project.as_str())));
    f.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_dispatch_rejects_changed_context_without_freezing_or_reassigning_call(
) -> Result<()> {
    let f = Fixture::new().await?;
    f.manager
        .update(&f.session)
        .project_id(Some(f.other_project.clone()))
        .apply()
        .await?;
    let error = f
        .assets
        .freeze_call_for_project(&f.session, "dispatch", Some(&f.project))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed since reply context"));
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM project_execution_scopes_v1 WHERE source_session_id=?",
    )
    .bind(&f.session)
    .fetch_one(&f.assets.pool)
    .await?;
    assert_eq!(count, 0);
    f.manager
        .update(&f.session)
        .project_id(Some(f.project.clone()))
        .apply()
        .await?;
    f.assets
        .freeze_call_for_project(&f.session, "dispatch", Some(&f.project))
        .await?;
    f.manager
        .update(&f.session)
        .project_id(Some(f.other_project.clone()))
        .apply()
        .await?;
    let error = f
        .assets
        .freeze_call_for_project(&f.session, "dispatch", Some(&f.other_project))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different frozen project"));
    assert!(f
        .assets
        .freeze_call_for_project(&f.session, "dispatch", Some(&f.project))
        .await
        .is_err());
    f.manager
        .update(&f.session)
        .project_id(Some(f.project.clone()))
        .apply()
        .await?;
    f.assets
        .freeze_call_for_project(&f.session, "dispatch", Some(&f.project))
        .await?;
    f.publish(&f.session, "job", Some("dispatch"), ASSET_A, Value::Null)
        .await?;
    assert_eq!(
        f.assets.read(&f.peer, ASSET_A).await?.unwrap().project_id,
        f.project
    );
    assert!(f.assets.read(&f.other, ASSET_A).await?.is_none());
    f.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_dispatch_compares_legacy_and_absent_context_exactly() -> Result<()> {
    let f = Fixture::new().await?;
    f.manager
        .update(&f.session)
        .project_id(None)
        .apply()
        .await?;
    assert!(f
        .assets
        .freeze_call_for_project(&f.session, "none", Some("legacy-a"))
        .await
        .is_err());
    f.assets
        .freeze_call_for_project(&f.session, "none", None)
        .await?;
    f.manager
        .update(&f.session)
        .project_id(Some("legacy-a".into()))
        .apply()
        .await?;
    assert!(f
        .assets
        .freeze_call_for_project(&f.session, "legacy", None)
        .await
        .is_err());
    assert!(f
        .assets
        .freeze_call_for_project(&f.session, "legacy", Some("legacy-b"))
        .await
        .is_err());
    f.assets
        .freeze_call_for_project(&f.session, "legacy", Some("legacy-a"))
        .await?;
    f.manager
        .update(&f.session)
        .project_id(Some(f.project.clone()))
        .apply()
        .await?;
    assert!(f
        .assets
        .freeze_call_for_project(&f.session, "none", Some(&f.project))
        .await
        .is_err());
    assert!(f
        .assets
        .freeze_call_for_project(&f.session, "legacy", Some(&f.project))
        .await
        .is_err());
    f.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_freeze_no_run_and_keep_original_project_after_session_move() -> Result<()> {
    let f = Fixture::new().await?;
    f.assets.freeze_call(&f.session, "call-a").await?;
    f.manager
        .update(&f.session)
        .project_id(Some(f.other_project.clone()))
        .apply()
        .await?;
    f.assets.freeze_call(&f.session, "call-a").await?;
    f.assets
        .freeze_job(&f.session, "job-a", Some("call-a"))
        .await?;
    f.assets.freeze_job(&f.session, "job-a", None).await?;
    let mut tx = f.assets.pool.begin().await?;
    assert!(ProjectAssetStore::has_project_scope_in_tx(&mut tx, &f.session, "job-a", None).await?);
    assert!(
        ProjectAssetStore::has_project_scope_in_tx(&mut tx, &f.session, "mcp-a", Some("call-a"))
            .await?
    );
    tx.commit().await?;
    f.publish(
        &f.session,
        "job-a",
        None,
        ASSET_A,
        json!({"snapshot_json":"{\"seed\":7}"}),
    )
    .await?;
    f.publish(
        &f.session,
        "mcp-a",
        Some("call-a"),
        ASSET_B,
        json!({"source":"mcp"}),
    )
    .await?;
    let asset = f.assets.read(&f.peer, ASSET_A).await?.unwrap();
    assert_eq!(asset.project_id, f.project);
    assert_eq!(asset.source_session_id, f.session);
    assert!(asset.work_item_id.is_none());
    assert!(asset.run_id.is_none());
    assert!(f.assets.read(&f.session, ASSET_A).await?.is_none());
    assert!(f.assets.read(&f.other, ASSET_B).await?.is_none());
    assert_eq!(f.assets.list(&f.peer, None, 10).await?.len(), 2);
    f.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_frozen_none_and_legacy_are_never_retroactively_attached() -> Result<()> {
    let f = Fixture::new().await?;
    f.manager
        .update(&f.session)
        .project_id(None)
        .apply()
        .await?;
    f.assets.freeze_call(&f.session, "none-call").await?;
    f.assets.freeze_job(&f.session, "none-job", None).await?;
    f.manager
        .update(&f.session)
        .project_id(Some("legacy-slug".into()))
        .apply()
        .await?;
    f.assets.freeze_job(&f.session, "legacy-job", None).await?;
    assert!(f.assets.list(&f.session, None, 10).await?.is_empty());
    assert!(f.assets.read(&f.session, ASSET_A).await?.is_none());
    f.manager
        .update(&f.session)
        .project_id(Some(f.project.clone()))
        .apply()
        .await?;
    f.assets
        .freeze_job(&f.session, "none-call-job", Some("none-call"))
        .await?;
    f.assets.freeze_job(&f.session, "none-job", None).await?;
    for (job, id) in [
        ("none-job", ASSET_A),
        ("legacy-job", ASSET_B),
        ("none-call-job", ASSET_C),
    ] {
        f.publish(&f.session, job, None, id, json!({"noProject":true}))
            .await?;
    }
    assert!(f.assets.list(&f.peer, None, 10).await?.is_empty());
    assert!(f
        .assets
        .freeze_call("missing-session", "call")
        .await
        .is_err());
    f.manager
        .update(&f.session)
        .project_id(Some(format!("project_{}", uuid::Uuid::new_v4())))
        .apply()
        .await?;
    assert!(f
        .assets
        .freeze_job(&f.session, "missing-project-job", None)
        .await
        .is_err());
    f.manager.delete_session(&f.session).await?;
    f.assets.freeze_job(&f.session, "none-job", None).await?;
    let mut tx = f.assets.pool.begin().await?;
    assert!(
        !ProjectAssetStore::has_project_scope_in_tx(&mut tx, &f.session, "none-job", None).await?
    );
    ProjectAssetStore::record_in_tx(
        &mut tx,
        &f.session,
        "none-job",
        None,
        ASSET_A,
        &media_content(ASSET_A, Value::Null),
    )
    .await?;
    tx.commit().await?;
    f.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_keep_run_and_snapshot_after_source_session_deletion_and_reopen(
) -> Result<()> {
    let f = Fixture::new().await?;
    let work = f
        .projects
        .create_work_item(CreateWorkItem {
            project_id: f.project.clone(),
            title: "Soundtrack".into(),
            objective: "Keep provenance".into(),
            acceptance_criteria: vec!["Recover after deletion".into()],
        })
        .await?;
    let run = f.projects.start_run(&work.id, Some(&f.session)).await?;
    f.assets.freeze_job(&f.session, "job", Some("call")).await?;
    let content = media_content(
        ASSET_A,
        json!({"snapshot_json":"{\"sessionId\":\"original\",\"seed\":9}","parent":{"resultId":ASSET_B,"sha256":"original-parent-sha"}}),
    );
    f.publish(&f.session, "job", None, ASSET_A, content.clone())
        .await?;
    let before = f.assets.read(&f.peer, ASSET_A).await?.unwrap();
    assert_eq!(before.work_item_id.as_deref(), Some(work.id.as_str()));
    assert_eq!(before.run_id.as_deref(), Some(run.id.as_str()));
    f.manager.delete_session(&f.session).await?;
    assert!(f.assets.read(&f.session, ASSET_A).await.is_err());
    assert_eq!(f.assets.read(&f.peer, ASSET_A).await?, Some(before.clone()));
    f.close().await;
    let reopened = SessionManager::new(f._root.path().join("data"));
    let assets = ProjectAssetStore::new(reopened.storage().pool().await?.clone());
    assets.initialize().await?;
    assert_eq!(assets.read(&f.peer, ASSET_A).await?, Some(before));
    assert_eq!(
        assets.read(&f.peer, ASSET_A).await?.unwrap().content,
        content
    );
    let summary = assets.list(&f.peer, None, 10).await?.remove(0);
    assert_eq!(summary.kind, "media");
    assert!(summary.output.get("configuration").is_none());
    assets.pool.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_reject_scope_content_and_call_conflicts_and_rollback() -> Result<()> {
    let f = Fixture::new().await?;
    f.assets.freeze_job(&f.session, "job", Some("call")).await?;
    assert!(f
        .assets
        .freeze_job(&f.session, "job", Some("other-call"))
        .await
        .is_err());
    f.assets.freeze_job(&f.session, "standalone", None).await?;
    assert!(f
        .assets
        .freeze_job(&f.session, "standalone", Some("call"))
        .await
        .is_err());
    f.publish(
        &f.session,
        "job",
        None,
        ASSET_A,
        json!({"z":1,"a":{"second":2,"first":1}}),
    )
    .await?;
    let original = f.assets.read(&f.peer, ASSET_A).await?.unwrap();
    f.publish(
        &f.session,
        "job",
        None,
        ASSET_A,
        json!({"a":{"first":1,"second":2},"z":1}),
    )
    .await?;
    assert_eq!(
        f.assets.read(&f.peer, ASSET_A).await?,
        Some(original.clone())
    );
    assert!(f
        .publish(&f.session, "job", None, ASSET_A, json!({"z":2}))
        .await
        .is_err());
    f.assets.freeze_job(&f.other, "job", None).await?;
    assert!(f
        .publish(&f.other, "job", None, ASSET_A, original.content.clone())
        .await
        .is_err());
    let mut tx = f.assets.pool.begin().await?;
    ProjectAssetStore::record_in_tx(
        &mut tx,
        &f.session,
        "job",
        None,
        ASSET_B,
        &media_content(ASSET_B, json!({"rollback":true})),
    )
    .await?;
    tx.rollback().await?;
    assert!(f.assets.read(&f.peer, ASSET_B).await?.is_none());
    assert_eq!(f.assets.read(&f.peer, ASSET_A).await?, Some(original));
    f.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_validate_pagination_bounds_and_archived_integrity() -> Result<()> {
    let f = Fixture::new().await?;
    f.assets.freeze_job(&f.session, "job", None).await?;
    for id in [ASSET_C, ASSET_A, ASSET_B] {
        f.publish(&f.session, "job", None, id, json!({"resultId":id}))
            .await?;
    }
    let first = f.assets.list(&f.peer, None, 2).await?;
    assert_eq!(
        first
            .iter()
            .map(|a| a.asset_id.as_str())
            .collect::<Vec<_>>(),
        vec![ASSET_A, ASSET_B]
    );
    let second = f.assets.list(&f.peer, Some(ASSET_B), 2).await?;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].asset_id, ASSET_C);
    assert!(f.assets.list(&f.peer, Some(ASSET_C), 2).await?.is_empty());
    assert!(f.assets.list(&f.peer, None, 0).await.is_err());
    assert!(f.assets.list(&f.peer, None, 101).await.is_err());
    assert!(f.assets.list(&f.peer, Some("invalid"), 10).await.is_err());
    assert!(f
        .publish(
            &f.session,
            "job",
            None,
            &uuid::Uuid::new_v4().to_string(),
            json!({"oversize":"x".repeat(MAX_CONTENT_BYTES)})
        )
        .await
        .is_err());
    sqlx::query("UPDATE project_assets_v1 SET content_json='{}' WHERE asset_id=?")
        .bind(ASSET_A)
        .execute(&f.assets.pool)
        .await?;
    assert!(f.assets.read(&f.peer, ASSET_A).await.is_err());
    assert_eq!(f.assets.list(&f.peer, None, 10).await?.len(), 3);
    sqlx::query("UPDATE project_assets_v1 SET summary_json='{}' WHERE asset_id=?")
        .bind(ASSET_A)
        .execute(&f.assets.pool)
        .await?;
    assert!(f.assets.list(&f.peer, None, 10).await.is_err());
    assert!(f.assets.read(&f.other, ASSET_A).await?.is_none());
    sqlx::query("UPDATE project_assets_v1 SET asset_id='invalid' WHERE asset_id=?")
        .bind(ASSET_B)
        .execute(&f.assets.pool)
        .await?;
    assert!(f.assets.list(&f.peer, Some(ASSET_C), 10).await.is_err());
    f.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_archive_maximum_transcript_without_exposing_text_in_summary() -> Result<()>
{
    let f = Fixture::new().await?;
    f.assets.freeze_job(&f.session, "asr-job", None).await?;
    let text = "\u{0001}".repeat(4 * 1024 * 1024);
    let content = json!({
        "kind":"transcript",
        "transcript":{"bytes":text.len(),"sha256":Sha256::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>(),"text":text},
        "configuration":{"snapshotJson":"x".repeat(256 * 1024)},
    });
    assert!(serde_json::to_vec(&content)?.len() > 24 * 1024 * 1024);
    f.publish(&f.session, "asr-job", None, ASSET_A, content.clone())
        .await?;
    let summaries = f.assets.list(&f.peer, None, 1).await?;
    assert_eq!(summaries[0].kind, "transcript");
    assert_eq!(summaries[0].output.as_object().unwrap().len(), 2);
    assert_eq!(summaries[0].output["bytes"], 4 * 1024 * 1024);
    assert!(serde_json::to_string(&summaries)?.len() < 2048);
    assert_eq!(
        f.assets.read(&f.peer, ASSET_A).await?.unwrap().content,
        content
    );
    assert!(f
        .publish(
            &f.session,
            "asr-job",
            None,
            ASSET_B,
            json!({"kind":"unknown"})
        )
        .await
        .is_err());
    f.close().await;
    Ok(())
}

#[tokio::test]
async fn project_assets_reject_inconsistent_session_task_and_run_associations() -> Result<()> {
    let f = Fixture::new().await?;
    let work = f
        .projects
        .create_work_item(CreateWorkItem {
            project_id: f.project.clone(),
            title: "Task".into(),
            objective: "Only this project".into(),
            acceptance_criteria: vec![],
        })
        .await?;
    f.projects.start_run(&work.id, Some(&f.session)).await?;
    f.manager
        .update(&f.session)
        .project_id(Some(f.other_project.clone()))
        .apply()
        .await?;
    assert!(f.assets.freeze_call(&f.session, "call").await.is_err());
    assert!(f.assets.list(&f.session, None, 10).await.is_err());
    assert!(f.assets.read(&f.session, ASSET_A).await.is_err());
    f.manager
        .update(&f.session)
        .project_id(Some(f.project.clone()))
        .apply()
        .await?;
    let next = f
        .projects
        .create_work_item(CreateWorkItem {
            project_id: f.project.clone(),
            title: "Next".into(),
            objective: "Other task".into(),
            acceptance_criteria: vec![],
        })
        .await?;
    f.projects
        .attach_session(&f.project, &next.id, &f.session)
        .await?;
    assert!(f.assets.freeze_call(&f.session, "call").await.is_err());
    f.close().await;
    Ok(())
}
