use super::super::{files, import_readers, ImportedTaskOutput, OutputReader};
use super::music_probe::{logical_bytes, write_report};
use crate::config::LuminaMode;
use crate::media::project_task_reuse::{prepare_project_reused_task, ProjectTaskReuseRequest};
use crate::media::task_configuration::ArchivedTaskConfiguration;
use crate::media::{
    MediaAssociation, MediaResultStore, MediaSpec, TaskConfigurationSnapshot,
    TaskConfigurationStore,
};
use crate::project::ProjectAssetStore;
use crate::session::session_manager::SessionStorage;
use crate::session::{SessionManager, SessionType};
use anyhow::{ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Seek;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

const OWNER: &str = "lifecycle::task_resources::tests::music_probe::measure_installed_music_owner";
const PARENT_ASSET: &str = "0024354d-3a4a-83a4-9900-0ab7c1f96d1c";
const PARENT_CONTENT: &str = "f69a343dfa9a14a69b2063d5100bb566d52cd6acfbf78741cc582f14c3d1c9ec";
const PARENT_WAV: &str = "afa7d6699180ed7c1bd7fdd4524af6b4faf15b9e077f25d4b70dc5787693f2e5";

fn sources() -> BTreeMap<&'static str, String> {
    let mut sources = super::music_probe::sources();
    macro_rules! extra {
        ($($path:literal),+ $(,)?) => { $(sources.insert($path, Sha256Digest::of_bytes(include_bytes!(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../", $path))).as_str().into());)+ };
    }
    extra!(
        "crates/lumina/tests/support/project_music_reuse_probe.rs",
        "crates/lumina/src/media/task_publication.rs",
        "crates/lumina/src/media/task_inputs.rs",
        "crates/lumina/src/media/project_payload.rs",
        "Cargo.lock",
    );
    sources
}

#[tokio::test]
#[ignore = "Measures actual cross-session music reuse from a retained real project asset"]
async fn measure_project_music_reuse() -> Result<()> {
    ensure!(
        std::env::var("LUMINA_PROJECT_MUSIC_REUSE").as_deref() == Ok("measurement"),
        "Project music reuse requires explicit test measurement mode"
    );
    let validation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let run = PathBuf::from(
        std::env::var_os("LUMINA_TASK_RESOURCE_RUN_DIR")
            .context("Missing measurement directory")?,
    )
    .canonicalize()?;
    ensure!(
        run.parent() == Some(validation.as_path()),
        "Unexpected run directory"
    );
    let case_bytes = std::fs::read(run.join("project-reuse-case.json"))?;
    ensure!(
        case_bytes.len() <= 65536,
        "Project reuse fixture exceeds limit"
    );
    let case: Value = serde_json::from_slice(&case_bytes)?;
    let case_sha = Sha256Digest::of_bytes(&case_bytes);
    ensure!(
        case["schemaVersion"] == 1
            && case["parentAssetId"] == PARENT_ASSET
            && case["parentAssetContentSha256"] == PARENT_CONTENT
            && case["parentOutputSha256"] == PARENT_WAV
            && case["patch"] == json!({"seed":1}),
        "Unexpected real parent or patch"
    );
    let report_path = PathBuf::from(
        case["parentReportPath"]
            .as_str()
            .context("Missing parent report")?,
    )
    .canonicalize()?;
    ensure!(
        report_path
            == validation
                .join("m4-project-music-20261115-r1/result.json")
                .canonicalize()?,
        "Parent report is outside the pinned acceptance run"
    );
    let report_bytes = std::fs::read(&report_path)?;
    ensure!(
        Sha256Digest::of_bytes(&report_bytes).as_str() == case["parentReportSha256"],
        "Parent report changed"
    );
    let report: Value = serde_json::from_slice(&report_bytes)?;
    ensure!(
        report["status"] == "PASS"
            && report["realBudgetAdmission"] == true
            && report["sourceSessionDeleted"] == true
            && report["sqliteReopened"] == true
            && report["actualResult"]["resultId"] == PARENT_ASSET
            && report["archiveContentSha256"] == PARENT_CONTENT
            && report["verifiedWavSha256"] == PARENT_WAV
            && report["projectId"] == case["parentProjectId"]
            && report["sourceSessionId"] == case["parentSessionId"]
            && report["jobId"] == case["parentJobId"],
        "Parent source did not pass real generation and retention acceptance"
    );
    let directory = run.join("output-root/publication");
    let database = directory.join("sessions/sessions.db");
    ensure!(
        files::digest(&mut File::open(&database)?)? == case["backupDatabaseSha256"],
        "Prepared database differs from its frozen backup"
    );
    let before =
        logical_bytes(&directory).context("Project reuse initial directory measurement")?;
    let manager = Arc::new(SessionManager::new(directory.clone()));
    let storage = Arc::clone(manager.storage());
    let child = manager
        .create_session(
            directory.clone(),
            "Real project music reuse".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    let project_id = case["parentProjectId"]
        .as_str()
        .context("Missing parent project")?;
    let parent_session = case["parentSessionId"]
        .as_str()
        .context("Missing parent session")?;
    manager
        .update(&child.id)
        .project_id(Some(project_id.into()))
        .apply()
        .await?;
    let pool = storage.pool().await?.clone();
    let parent_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)")
            .bind(parent_session)
            .fetch_one(&pool)
            .await?;
    ensure!(!parent_exists, "Source session was not deleted");
    let assets = ProjectAssetStore::new(pool.clone());
    let parent = assets
        .read(&child.id, PARENT_ASSET)
        .await?
        .context("Real parent archive missing")?;
    ensure!(
        parent.content_sha256 == PARENT_CONTENT && parent.project_id == project_id,
        "Parent archive changed"
    );
    let parent_asset_bytes: i64 = sqlx::query_scalar(
        "SELECT length(CAST(content_json AS BLOB)) FROM project_assets_v1 WHERE asset_id=?",
    )
    .bind(PARENT_ASSET)
    .fetch_one(&pool)
    .await?;
    let archive: ArchivedTaskConfiguration =
        serde_json::from_value(parent.content["configuration"].clone())?;
    let parent_record = archive.validate(&parent.source_session_id, &parent.job_id)?;
    let TaskConfigurationSnapshot::StableAudioMusic(parent_snapshot) = parent_record.snapshot()
    else {
        anyhow::bail!("Parent is not a music task")
    };
    let media = MediaResultStore::new(pool.clone());
    let parent_content = media.open_content(parent_session, PARENT_ASSET).await?;
    ensure!(
        files::digest(&mut File::open(parent_content.path())?)? == PARENT_WAV,
        "Parent WAV changed"
    );
    let root = media
        .register_output_root(&storage.media_output_directory())
        .await?;
    let app = Arc::new(InstallationApplication::new(
        &validation.join("m4-managed-audio-install-20261113-r1"),
        Arc::new(|| Ok(None)),
    ));
    storage.bind_native_installations(Arc::clone(&app))?;
    let installation = app.service().await?;
    installation.wait_ready().await?;
    let job = format!("project-music-reuse-{}", uuid::Uuid::new_v4());
    let preparation_started = Instant::now();
    let prepared = prepare_project_reused_task(
        &installation,
        &storage,
        &child.id,
        ProjectTaskReuseRequest {
            identity: parent_record.identity().clone(),
            parent_asset_id: PARENT_ASSET.into(),
            new_job_id: job.clone(),
            patch: serde_json::from_value(case["patch"].clone())?,
        },
    )
    .await?;
    let inputs = media
        .open_project_task_inputs(&storage, &child.id, &prepared)
        .await?;
    ensure!(inputs.is_empty(), "Music sample unexpectedly has inputs");
    let plan = prepared.plan();
    let digest = plan.snapshot_digest()?;
    let NativeTaskPlan::StableAudioMusic(task) = plan else {
        anyhow::bail!("Reuse adapter changed type")
    };
    ensure!(
        task.snapshot().parameters.seed == 1 && parent_snapshot.parameters.seed == 42,
        "Seed patch was not applied"
    );
    let configuration = TaskConfigurationStore::new(pool.clone());
    configuration.save_project_task(&prepared).await?;
    let authorization_bytes = prepared.authorization().encode()?.len();
    let preparation_millis = preparation_started.elapsed().as_millis();
    let publication = crate::media::task_publication::project_reuse_context(&prepared)?;
    let TaskPublicationContext::ProjectParameterReuseV2 {
        implementation_sources_digest,
        payload_tier,
    } = &publication
    else {
        anyhow::bail!("Expected project publication context")
    };
    let owner_request = json!({"parentSnapshot":parent_snapshot,"parentSnapshotDigest":parent_record.generation_config_sha256(),
        "childSessionId":child.id,"childJobId":job,"patch":case["patch"],"expectedChildSnapshotDigest":digest,
        "implementationSourcesDigest":implementation_sources_digest,"payloadTier":payload_tier,"parentAssetId":PARENT_ASSET,"parentAssetContentSha256":PARENT_CONTENT});
    let request_path = run.join("project-reuse-owner.json");
    write_report(&request_path, owner_request)?;
    let request_sha = Sha256Digest::of_bytes(&std::fs::read(&request_path)?);
    write_report(
        &run.join("snapshot.json"),
        serde_json::to_value(task.snapshot())?,
    )?;
    write_report(
        &run.join("driver.json"),
        json!({"implementation":"media::task_output::import_readers",
        "ownerTest":OWNER,"sources":sources(),"projectReuseRequestSha256":request_sha,
        "publicationContext":publication,"caseSha256":case_sha,"parentAssetBytes":parent_asset_bytes,
        "authorizationBytes":authorization_bytes,"preparationMillis":preparation_millis}),
    )?;
    let owner =
        PathBuf::from(std::env::var_os("LUMINA_TASK_RESOURCE_OWNER").context("Missing owner")?)
            .canonicalize()?;
    ensure!(
        owner.parent() == Some(validation.join("../debug/deps").canonicalize()?.as_path()),
        "Unexpected owner location"
    );
    let executed = std::process::Command::new(owner)
        .args(["--exact", OWNER, "--ignored", "--nocapture"])
        .output()?;
    let stdout = String::from_utf8_lossy(&executed.stdout);
    ensure!(
        executed.status.success()
            && stdout
                .lines()
                .any(|line| line == format!("test {OWNER} ... ok")),
        "Project owner failed: {stdout} {}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let native_path = run.join("output-root/native/output.wav");
    let mut native = File::options()
        .read(true)
        .share_mode(1)
        .open(&native_path)?;
    let bytes = native.metadata()?.len();
    ensure!(bytes == 705644, "Expected the standard four-second WAV");
    let native_sha = files::digest(&mut native)?;
    native.rewind()?;
    let association = MediaAssociation::Message {
        message_id: "project-reuse-measurement".into(),
    };
    let output = import_readers(
        &configuration,
        &media,
        plan,
        &digest,
        vec![OutputReader {
            name: "output.wav".into(),
            bytes,
            sha256: native_sha.clone(),
            file: native,
        }],
        &root.root_id,
        association.clone(),
    )
    .await?;
    let ImportedTaskOutput::Media(results) = output else {
        anyhow::bail!("Missing media output")
    };
    let [result] = results.as_slice() else {
        anyhow::bail!("Expected one music result")
    };
    ensure!(
        matches!(
            result.spec,
            MediaSpec::Audio {
                sample_rate: 44100,
                channels: 2,
                sample_frames: 176400,
                ..
            }
        ) && result.mime == "audio/wav"
            && result.association == association
            && result.sha256 == native_sha
            && result.sha256 != PARENT_WAV,
        "Changed seed did not produce the expected new WAV"
    );
    let child_archive = assets
        .read(&child.id, &result.result_id)
        .await?
        .context("Child project asset missing")?;
    let output_archive_bytes: i64 = sqlx::query_scalar(
        "SELECT length(CAST(content_json AS BLOB)) FROM project_assets_v1 WHERE asset_id=?",
    )
    .bind(&result.result_id)
    .fetch_one(&pool)
    .await?;
    ensure!(
        child_archive.content["projectLineage"]["parentAssetId"] == PARENT_ASSET
            && child_archive.content["projectLineage"]["parentAssetContentSha256"]
                == PARENT_CONTENT
            && child_archive.content["projectLineage"]["patch"] == case["patch"],
        "Child lineage differs from the requested patch"
    );
    ensure!(
        assets.read(&child.id, PARENT_ASSET).await? == Some(parent.clone()),
        "Parent archive changed after publication"
    );
    let published =
        logical_bytes(&directory).context("Project reuse published directory measurement")?;
    pool.close().await;
    let reopened = SessionStorage::new(directory.clone());
    let reopened_pool = reopened.pool().await?.clone();
    let held = MediaResultStore::new(reopened_pool.clone())
        .open_content(&child.id, &result.result_id)
        .await?;
    let verified = TaskConfigurationStore::new(reopened_pool.clone())
        .verify_result(result)
        .await?;
    let published_path = held.path().to_path_buf();
    ensure!(
        held.result() == result
            && verified.generation_config_sha256() == digest.as_str()
            && files::digest(&mut File::open(&published_path)?)? == native_sha,
        "Reopened child output or configuration differs"
    );
    let reopened_assets = ProjectAssetStore::new(reopened_pool.clone());
    ensure!(
        reopened_assets.read(&child.id, &result.result_id).await? == Some(child_archive)
            && reopened_assets.read(&child.id, PARENT_ASSET).await? == Some(parent)
            && files::digest(&mut File::open(parent_content.path())?)? == PARENT_WAV,
        "Reopened project versions changed"
    );
    let parent_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)")
            .bind(parent_session)
            .fetch_one(&reopened_pool)
            .await?;
    ensure!(!parent_exists, "Reuse recreated the source session");
    drop((held, parent_content));
    reopened_pool.close().await;
    app.shutdown().await?;
    let after = logical_bytes(&directory).context("Project reuse final directory measurement")?;
    write_report(
        &run.join("publication.json"),
        json!({"schemaVersion":2,
        "implementation":"media::task_output::import_readers","implementationSourcesSha256":Sha256Digest::of_serializable(&sources())?,
        "sources":sources(),"projectReuseRequestSha256":request_sha,"publicationContext":publication,"caseSha256":case_sha,
        "snapshotDigest":digest,"publishedPath":published_path,"result":result,"reopenedVerified":true,
        "inputSha256":native_sha,"publishedSha256":result.sha256,"publishedBytes":result.bytes,
        "parentAssetBytes":parent_asset_bytes,"authorizationBytes":authorization_bytes,"preparationMillis":preparation_millis,"outputArchiveBytes":output_archive_bytes,
        "projectArchive":{"verified":true,"reopenedVerified":true,"parentUnchanged":true,"lineageVerified":true,"sourceSessionAbsent":true},
        "checkpoints":[{"directoryLogicalBytes":before},{"directoryLogicalBytes":published},{"directoryLogicalBytes":after}]}),
    )
}
