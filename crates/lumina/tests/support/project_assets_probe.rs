use crate::config::LuminaMode;
use crate::media::{
    execute_prepared_task, ImportedTaskOutput, MediaAssociation, MediaResultStore,
    PreparedTaskExecution, TaskConfigurationStore,
};
use crate::project::{CreateWorkItem, ProjectAssetStore, ProjectService, RunCompletion, RunStatus};
use crate::session::{SessionManager, SessionType};
use anyhow::{ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::*;
use serde_json::{json, Value};
use std::io::Write;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

const PROMPT: &str = "A gentle solo piano melody, clear notes, calm instrumental music, no vocals.";
const OUTPUT_SHA: &str = "afa7d6699180ed7c1bd7fdd4524af6b4faf15b9e077f25d4b70dc5787693f2e5";

fn write_report(path: &Path, report: &Value) -> Result<()> {
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(report)?)?;
    file.sync_all()?;
    Ok(())
}

async fn run_probe(run: &Path) -> Result<Value> {
    let validation = run.parent().context("Missing validation directory")?;
    let engine = PathBuf::from(
        std::env::var_os("LUMINA_AUDIO_MUSIC_CPU_CLI")
            .context("LUMINA_AUDIO_MUSIC_CPU_CLI is required")?,
    );
    ensure!(engine.is_absolute(), "Music engine path must be absolute");
    let data = run.join("session-storage");
    let project_root = run.join("music-project");
    std::fs::create_dir(&project_root)?;
    let manager = Arc::new(SessionManager::new(data.clone()));
    let storage = Arc::clone(manager.storage());
    let projects = ProjectService::new(Arc::clone(&storage));
    let project = projects
        .open_project(&project_root, Some("Real music asset acceptance"))
        .await?;
    let work = projects
        .create_work_item(CreateWorkItem {
            project_id: project.id.clone(),
            title: "Generate a retained music asset".into(),
            objective: "Verify real generation and project archive survival".into(),
            acceptance_criteria: vec![
                "Retain exact music output and configuration after session deletion".into(),
            ],
        })
        .await?;
    let source = manager
        .create_session(
            project_root.clone(),
            "Real music generation".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    projects
        .attach_session(&project.id, &work.id, &source.id)
        .await?;
    let execution = projects.start_run(&work.id, Some(&source.id)).await?;
    let pool = storage.pool().await?.clone();
    let media = MediaResultStore::new(pool.clone());
    media.initialize().await?;
    let configurations = TaskConfigurationStore::new(pool.clone());
    configurations.initialize().await?;
    let assets = ProjectAssetStore::new(pool.clone());
    let app = Arc::new(InstallationApplication::new(
        &validation.join("m4-managed-audio-install-20261113-r1"),
        Arc::new(|| Ok(None)),
    ));
    storage.bind_native_installations(Arc::clone(&app))?;
    let installation = app.service().await?;
    installation.wait_ready().await?;
    let catalog = NativeTaskCatalog::active(installation.repository())?;
    let entry = catalog
        .list(NativeTaskIntent::TextToMusic)
        .into_iter()
        .find(|entry| {
            entry.identity.installation_id == "acceptance-stable-audio-small-music-cpu"
                && entry.identity.version_id == "acceptance-stable-audio-small-music-cpu-v2"
        })
        .context("Installed Stable Audio music acceptance version is missing")?;
    let job = format!("project-music-{}", uuid::Uuid::new_v4());
    let plan = catalog.prepare(
        &entry.identity,
        json!({
            "sessionId": source.id, "jobId": job, "prompt": PROMPT, "seed":42, "steps":8
        }),
    )?;
    ensure!(
        matches!(&plan, NativeTaskPlan::StableAudioMusic(_)),
        "Expected a music plan"
    );
    let snapshot_digest = plan.snapshot_digest()?;
    let mut engines = NativeTaskEngineRegistry::default();
    engines.register(plan.runtime(), &engine)?;
    let generation_started = Instant::now();
    let completed = execute_prepared_task(
        &installation,
        &engines,
        &storage,
        &source.id,
        plan,
        MediaAssociation::Message {
            message_id: "real-project-music-probe".into(),
        },
    )
    .await?;
    let result = match completed {
        PreparedTaskExecution::Completed(ImportedTaskOutput::Media(mut results)) => {
            ensure!(results.len() == 1, "Music must publish one output");
            results.remove(0)
        }
        other => anyhow::bail!(
            "Production resource admission or music execution did not complete: {other:?}"
        ),
    };
    let generation_millis = generation_started.elapsed().as_millis();
    ensure!(
        result.sha256 == OUTPUT_SHA && result.bytes == 705644,
        "Real music output differs from the recorded sample"
    );
    let mut content = media.open_content(&source.id, &result.result_id).await?;
    let published_path = content.path().to_path_buf();
    let mut wav = Vec::new();
    while (wav.len() as u64) < result.bytes {
        wav.extend_from_slice(&content.read_range(wav.len() as u64, 64 * 1024)?);
    }
    ensure!(
        Sha256Digest::of_bytes(&wav).as_str() == OUTPUT_SHA,
        "Published bytes fail independent SHA verification"
    );
    drop(content);
    let original_config = configurations
        .read(&source.id, &job)
        .await?
        .context("Executed task configuration is missing")?;
    ensure!(
        original_config.generation_config_sha256() == snapshot_digest.as_str(),
        "Executed snapshot digest changed"
    );
    let reader = manager
        .create_session(
            project_root.clone(),
            "New same-project reader".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    manager
        .update(&reader.id)
        .project_id(Some(project.id.clone()))
        .apply()
        .await?;
    let listing = assets.list(&reader.id, None, 20).await?;
    ensure!(
        listing.len() == 1 && listing[0].asset_id == result.result_id,
        "New project session cannot discover the music asset"
    );
    let archive = assets
        .read(&reader.id, &result.result_id)
        .await?
        .context("Project asset is missing")?;
    ensure!(
        archive.project_id == project.id
            && archive.source_session_id == source.id
            && archive.job_id == job
            && archive.work_item_id.as_deref() == Some(work.id.as_str())
            && archive.run_id.as_deref() == Some(execution.id.as_str()),
        "Project asset lost its original execution scope"
    );
    ensure!(
        archive.content["result"] == serde_json::to_value(&result)?,
        "Project asset media record differs"
    );
    ensure!(
        archive.content["configuration"]["snapshotSha256"] == snapshot_digest.as_str()
            && archive.content["configuration"]["snapshotJson"] == original_config.snapshot_json(),
        "Project asset configuration differs"
    );
    let checked: crate::media::task_configuration::ArchivedTaskConfiguration =
        serde_json::from_value(archive.content["configuration"].clone())?;
    ensure!(
        checked.validate(&source.id, &job)? == original_config,
        "Archived configuration did not validate against the executed request"
    );
    ensure!(
        media.read(&reader.id, &result.result_id).await?.is_none(),
        "Project discovery incorrectly widened session media access"
    );
    projects
        .finish_run(
            &execution.id,
            RunCompletion {
                status: RunStatus::Succeeded,
                error_code: None,
                error_message: None,
            },
        )
        .await?;
    manager.delete_session(&source.id).await?;
    ensure!(
        configurations.read(&source.id, &job).await?.is_none(),
        "Deleted session configuration remains in the session table"
    );
    ensure!(
        assets.read(&reader.id, &result.result_id).await? == Some(archive.clone()),
        "Deleting the source session changed its project asset"
    );
    storage.native_installations().shutdown().await?;
    pool.close().await;
    drop((
        assets,
        configurations,
        media,
        projects,
        pool,
        storage,
        manager,
        installation,
        app,
    ));

    let reopened_manager = Arc::new(SessionManager::new(data));
    let reopened_storage = Arc::clone(reopened_manager.storage());
    let reopened_pool = reopened_storage.pool().await?.clone();
    let reopened_assets = ProjectAssetStore::new(reopened_pool.clone());
    let reopened_media = MediaResultStore::new(reopened_pool.clone());
    let reopened_configurations = TaskConfigurationStore::new(reopened_pool.clone());
    ensure!(
        reopened_assets.read(&reader.id, &result.result_id).await? == Some(archive.clone()),
        "Reopening SQLite lost the immutable project archive"
    );
    ensure!(
        reopened_assets.list(&reader.id, None, 20).await?.len() == 1,
        "Reopening duplicated or lost the asset"
    );
    ensure!(
        reopened_configurations
            .read(&source.id, &job)
            .await?
            .is_none(),
        "Deleted source configuration returned after reopen"
    );
    ensure!(
        reopened_media
            .read(&reader.id, &result.result_id)
            .await?
            .is_none(),
        "Session media scope changed after reopen"
    );
    let foreign_root = run.join("foreign-project");
    std::fs::create_dir(&foreign_root)?;
    let reopened_projects = ProjectService::new(Arc::clone(&reopened_storage));
    let foreign_project = reopened_projects
        .open_project(&foreign_root, Some("Unrelated project"))
        .await?;
    let foreign = reopened_manager
        .create_session(
            foreign_root,
            "Foreign project reader".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    reopened_manager
        .update(&foreign.id)
        .project_id(Some(foreign_project.id))
        .apply()
        .await?;
    ensure!(
        reopened_assets
            .read(&foreign.id, &result.result_id)
            .await?
            .is_none()
            && reopened_assets
                .list(&foreign.id, None, 20)
                .await?
                .is_empty(),
        "Foreign project can discover this music asset"
    );
    ensure!(
        Sha256Digest::of_bytes(&std::fs::read(&published_path)?).as_str() == OUTPUT_SHA,
        "Audio bytes changed after delete/reopen"
    );
    reopened_pool.close().await;
    Ok(json!({
        "status":"PASS", "scope":"Production service real music generation and project persistence; not natural-language or desktop acceptance",
        "actualResult":result,"publishedPath":published_path,"verifiedWavSha256":OUTPUT_SHA,
        "projectId":project.id,"sourceSessionId":source.id,"readerSessionId":reader.id,
        "jobId":job,"workItemId":work.id,"runId":execution.id,
        "snapshotSha256":snapshot_digest,"archiveContentSha256":archive.content_sha256,
        "generationElapsedMillis":generation_millis,
        "sameProjectDiscovery":true,"sourceSessionDeleted":true,"sessionConfigurationRemoved":true,
        "archiveConfigurationVerified":true,"sqliteReopened":true,"crossProjectReadDenied":true,
        "originalMediaSessionIsolationPreserved":true,"realBudgetAdmission":true
    }))
}

#[tokio::test]
#[ignore = "Generates real installed music with production admission and verifies project persistence across session deletion and SQLite reopen"]
async fn installed_music_remains_project_asset_after_session_deletion_and_reopen() -> Result<()> {
    let started = Instant::now();
    let validation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let requested = PathBuf::from(
        std::env::var_os("LUMINA_PROJECT_ASSET_RUN_DIR")
            .context("LUMINA_PROJECT_ASSET_RUN_DIR is required")?,
    );
    ensure!(
        requested.is_absolute()
            && requested
                .parent()
                .context("Run directory needs a parent")?
                .canonicalize()?
                == validation,
        "Run must be a new direct child of the workspace validation directory"
    );
    std::fs::create_dir(&requested)
        .context("Project asset run directory must not already exist")?;
    let run = requested.canonicalize()?;
    ensure!(
        run.parent() == Some(validation.as_path())
            && run.metadata()?.file_attributes() & 0x400 == 0,
        "Unsafe project asset run directory"
    );
    let outcome = run_probe(&run).await;
    let mut report = match &outcome {
        Ok(report) => report.clone(),
        Err(error) => json!({"status":"FAILED","error":format!("{error:#}")}),
    };
    report["testElapsedMillis"] = json!(started.elapsed().as_millis());
    write_report(&run.join("result.json"), &report)?;
    outcome.map(|_| ())
}
