use super::super::{files, import_readers, ImportedTaskOutput, OutputReader};
use super::music_probe::{logical_bytes, write_report};
use crate::config::LuminaMode;
use crate::media::project_task_reuse::{prepare_project_reused_task, ProjectTaskReuseRequest};
use crate::media::task_configuration::ArchivedTaskConfiguration;
use crate::media::{
    MediaAssociation, MediaResultStore, MediaSpec, TaskConfigurationSnapshot,
    TaskConfigurationStore,
};
use crate::project::{ProjectAssetRecord, ProjectAssetStore};
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
        "crates/lumina/tests/support/project_music_metadata_fixture.rs",
        "crates/lumina/tests/support/project_music_reuse_production.rs",
        "crates/lumina/src/media/task_publication.rs",
        "crates/lumina/src/media/task_inputs.rs",
        "crates/lumina/src/media/project_payload.rs",
        "crates/lumina-local-inference/src/lifecycle/task_resources.rs",
        "crates/lumina-local-inference/src/lifecycle/task_resources/music_conditions.rs",
        "crates/lumina-local-inference/src/lifecycle/task_resources/music_probe.rs",
        "Cargo.lock",
    );
    sources
}

fn case_string<'a>(case: &'a Value, field: &str) -> Result<&'a str> {
    case[field]
        .as_str()
        .with_context(|| format!("Missing {field}"))
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Measurement input exceeds byte limit"
    );
    Ok(bytes)
}

fn contained_path(value: &Value, field: &str, root: &Path) -> Result<PathBuf> {
    let path = PathBuf::from(case_string(value, field)?);
    ensure!(path.is_absolute(), "{field} must be absolute");
    let path = path.canonicalize()?;
    ensure!(
        path.starts_with(root) && path != root,
        "{field} leaves source run"
    );
    Ok(path)
}

fn verify_export_source(source: &Value, exported: &Value) -> Result<()> {
    let exported = exported
        .as_object()
        .context("Proof has no exported source")?;
    for field in [
        "schemaVersion",
        "kind",
        "assetId",
        "assetContentSha256",
        "projectId",
        "sourceSessionId",
        "jobId",
        "snapshotSha256",
        "outputSha256",
        "outputBytes",
        "sourceSessionDeleted",
        "projectArchiveReopenedVerified",
        "databasePath",
        "publishedPath",
        "result",
    ] {
        ensure!(
            exported.get(field).is_some_and(|value| !value.is_null()),
            "Proof export is missing {field}"
        );
    }
    let mut descriptor = source
        .as_object()
        .context("Source is not an object")?
        .clone();
    for field in ["provenanceKind", "proofPath", "proofSha256"] {
        descriptor.remove(field);
    }
    ensure!(
        &descriptor == exported,
        "Source descriptor differs from proof export"
    );
    Ok(())
}

fn read_source_artifact(run: &Path, name: &str, expected_sha: &Value) -> Result<Value> {
    let expected_sha = expected_sha
        .as_str()
        .context("Missing source artifact digest")?;
    let path = run.join(name).canonicalize()?;
    ensure!(path.parent() == Some(run), "Source artifact leaves its run");
    let bytes = read_bounded(&path, 4 * 1024 * 1024)?;
    ensure!(
        Sha256Digest::of_bytes(&bytes).as_str() == expected_sha,
        "Source {name} digest differs from proof"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn validate_case(validation: &Path, case: &Value, domain: bool) -> Result<Option<Value>> {
    if domain {
        ensure!(case["schemaVersion"] == 2, "Expected domain case schema 2");
        let patch = case["patch"].as_object().context("Missing patch")?;
        ensure!(
            patch.len() == 1,
            "Domain measurement requires one patch field"
        );
        match patch.iter().next().context("Empty patch")? {
            (key, value) if key == "prompt" => {
                let prompt = value.as_str().context("Prompt must be text")?;
                ensure!(
                    !prompt.is_empty() && prompt.len() <= 16384,
                    "Prompt exceeds domain limit"
                );
            }
            (key, value) if key == "seed" => {
                ensure!(
                    value.as_u64().is_some_and(|seed| seed <= u32::MAX as u64),
                    "Seed must be u32"
                );
            }
            _ => anyhow::bail!("Unsupported domain patch"),
        }
    } else {
        ensure!(
            case["schemaVersion"] == 1 && case["patch"] == json!({"seed":1}),
            "Unexpected legacy case"
        );
    }
    let proof_kind = if domain {
        case_string(case, "parentProofKind")?
    } else {
        "normal_tool"
    };
    let report_path = PathBuf::from(case_string(case, "parentReportPath")?);
    ensure!(
        report_path.is_absolute(),
        "Parent report path must be absolute"
    );
    let report_path = report_path.canonicalize()?;
    if proof_kind == "normal_tool" {
        ensure!(
            report_path
                == validation
                    .join("m4-project-music-20261115-r1/result.json")
                    .canonicalize()?,
            "Parent report is outside the pinned acceptance run"
        );
    } else {
        ensure!(
            matches!(proof_kind, "project_measurement" | "api_metadata_fixture"),
            "Unsupported parent proof kind"
        );
        ensure!(
            report_path.parent().and_then(Path::parent) == Some(validation)
                && report_path
                    .file_name()
                    .is_some_and(|name| name == "source.json"),
            "Source must be a direct validation run source.json"
        );
    }
    let report_bytes = read_bounded(&report_path, 4 * 1024 * 1024)?;
    ensure!(
        Sha256Digest::of_bytes(&report_bytes).as_str() == case["parentReportSha256"],
        "Parent report changed"
    );
    let report: Value = serde_json::from_slice(&report_bytes)?;
    if proof_kind == "normal_tool" {
        ensure!(
            report_path
                == validation
                    .join("m4-project-music-20261115-r1/result.json")
                    .canonicalize()?,
            "Parent report is outside the pinned acceptance run"
        );
        ensure!(
            case["parentAssetId"] == PARENT_ASSET
                && case["parentAssetContentSha256"] == PARENT_CONTENT
                && case["parentOutputSha256"] == PARENT_WAV,
            "Unexpected pinned parent"
        );
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
        return Ok(None);
    }
    ensure!(
        matches!(proof_kind, "project_measurement" | "api_metadata_fixture"),
        "Unsupported parent proof kind"
    );
    let source_run = report_path.parent().context("Missing source run")?;
    ensure!(
        source_run.parent() == Some(validation)
            && report_path
                .file_name()
                .is_some_and(|name| name == "source.json"),
        "Source must be a direct validation run source.json"
    );
    ensure!(
        report["schemaVersion"] == 1
            && report["kind"] == "project_music_reuse_source"
            && report["provenanceKind"] == proof_kind
            && report["outputBytes"] == 705644
            && report["sourceSessionDeleted"] == true
            && report["projectArchiveReopenedVerified"] == true,
        "Source descriptor is incomplete"
    );
    for (field, case_field) in [
        ("assetId", "parentAssetId"),
        ("assetContentSha256", "parentAssetContentSha256"),
        ("projectId", "parentProjectId"),
        ("sourceSessionId", "parentSessionId"),
        ("jobId", "parentJobId"),
        ("outputSha256", "parentOutputSha256"),
    ] {
        ensure!(
            case_string(&report, field)? == case_string(case, case_field)?,
            "Source {field} differs from case"
        );
    }
    Sha256Digest::parse(case_string(&report, "snapshotSha256")?)?;
    contained_path(&report, "databasePath", source_run)?;
    let wav = contained_path(&report, "publishedPath", source_run)?;
    ensure!(
        files::digest(&mut File::open(wav)?)? == case["parentOutputSha256"],
        "Source WAV differs"
    );
    let proof_path = contained_path(&report, "proofPath", source_run)?;
    ensure!(
        proof_path.parent() == Some(source_run),
        "Proof must be in the source run"
    );
    let proof_bytes = read_bounded(&proof_path, 4 * 1024 * 1024)?;
    ensure!(
        Sha256Digest::of_bytes(&proof_bytes).as_str() == report["proofSha256"],
        "Source proof changed"
    );
    let proof: Value = serde_json::from_slice(&proof_bytes)?;
    if proof_kind == "project_measurement" {
        ensure!(
            proof_path
                .file_name()
                .is_some_and(|name| name == "result.json")
                && proof["status"] == "MEASURED_REVIEW_REQUIRED",
            "Invalid project measurement proof"
        );
        let publication = read_source_artifact(
            source_run,
            "publication.json",
            &proof["artifacts"]["publication.json"],
        )?;
        let measurement =
            read_source_artifact(source_run, "measurement.json", &proof["measurementSha256"])?;
        verify_export_source(&report, &publication["exportSource"])?;
        ensure!(
            measurement["key"]["kind"] == "music"
                && measurement["snapshotDigest"] == report["snapshotSha256"]
                && measurement["native"]["exitCode"] == 0
                && measurement["native"]["processTreeExited"] == true
                && measurement["publication"]["kind"] == "media"
                && measurement["publication"]["inputSha256"] == report["outputSha256"]
                && measurement["publication"]["publishedSha256"] == report["outputSha256"]
                && measurement["publication"]["publishedBytes"] == report["outputBytes"]
                && measurement["publication"]["reopenedVerified"] == true
                && publication["result"] == report["result"]
                && publication["snapshotDigest"] == report["snapshotSha256"]
                && publication["publishedSha256"] == report["outputSha256"]
                && publication["publishedBytes"] == report["outputBytes"]
                && publication["reopenedVerified"] == true,
            "Source differs from actual music measurement or publication"
        );
    } else {
        ensure!(
            proof_path
                .file_name()
                .is_some_and(|name| name == "fixture-result.json")
                && proof["status"] == "API_METADATA_FIXTURE_READY"
                && proof["generatedNewAudio"] == false,
            "Invalid API metadata fixture proof"
        );
        verify_export_source(&report, &proof["exportSource"])?;
    }
    Ok(Some(report))
}

#[test]
fn source_descriptor_binds_every_exported_field() {
    let exported = json!({"schemaVersion":1,"kind":"project_music_reuse_source",
        "assetId":"asset","assetContentSha256":"content","projectId":"project",
        "sourceSessionId":"session","jobId":"job","snapshotSha256":"snapshot",
        "outputSha256":"output","outputBytes":705644,"sourceSessionDeleted":true,
        "projectArchiveReopenedVerified":true,"databasePath":"database","publishedPath":"wav",
        "result":{"resultId":"asset","sha256":"output"}});
    let mut source = exported.clone();
    source["provenanceKind"] = json!("api_metadata_fixture");
    source["proofPath"] = json!("proof");
    source["proofSha256"] = json!("proof-digest");
    verify_export_source(&source, &exported).unwrap();
    for key in exported.as_object().unwrap().keys() {
        let mut changed = source.clone();
        changed[key] = Value::Null;
        assert!(verify_export_source(&changed, &exported).is_err(), "{key}");
        let mut missing_export = exported.clone();
        missing_export.as_object_mut().unwrap().remove(key);
        assert!(
            verify_export_source(&source, &missing_export).is_err(),
            "{key}"
        );
    }
    let mut extra = source.clone();
    extra["unproven"] = json!(true);
    assert!(verify_export_source(&extra, &exported).is_err());
    let mut changed_output = exported.clone();
    changed_output["result"]["sha256"] = json!("another-output");
    assert!(verify_export_source(&source, &changed_output).is_err());
}

#[path = "project_music_metadata_fixture.rs"]
mod metadata_fixture;

#[path = "project_music_reuse_production.rs"]
mod production;

async fn export_retained_source(
    directory: &Path,
    result: &crate::media::MediaResultRef,
    archive: &ProjectAssetRecord,
    parent: &ProjectAssetRecord,
) -> Result<Value> {
    let manager = SessionManager::new(directory.to_path_buf());
    let reader = manager
        .create_session(
            directory.to_path_buf(),
            "Retained project measurement reader".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    manager
        .update(&reader.id)
        .project_id(Some(archive.project_id.clone()))
        .apply()
        .await?;
    manager.delete_session(&result.session_id).await?;
    let pool = manager.storage().pool().await?.clone();
    let assets = ProjectAssetStore::new(pool.clone());
    ensure!(
        assets.read(&reader.id, &result.result_id).await? == Some(archive.clone()),
        "Deleting source changed child archive"
    );
    ensure!(
        assets.read(&reader.id, &parent.asset_id).await? == Some(parent.clone()),
        "Deleting source changed parent archive"
    );
    pool.close().await;
    let reopened = SessionStorage::new(directory.to_path_buf());
    let reopened_pool = reopened.pool().await?.clone();
    let assets = ProjectAssetStore::new(reopened_pool.clone());
    ensure!(
        assets.read(&reader.id, &result.result_id).await? == Some(archive.clone())
            && assets.read(&reader.id, &parent.asset_id).await? == Some(parent.clone()),
        "Reopened retained versions differ"
    );
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)")
        .bind(&result.session_id)
        .fetch_one(&reopened_pool)
        .await?;
    ensure!(!exists, "Export source session remains present");
    let media = MediaResultStore::new(reopened_pool.clone());
    let content = media
        .open_content(&result.session_id, &result.result_id)
        .await?;
    ensure!(
        content.result() == result
            && files::digest(&mut File::open(content.path())?)? == result.sha256,
        "Exported WAV changed"
    );
    let original = media
        .open_content(&parent.source_session_id, &parent.asset_id)
        .await?;
    ensure!(
        files::digest(&mut File::open(original.path())?)? == parent.content["result"]["sha256"],
        "Parent WAV changed during export"
    );
    let configuration: ArchivedTaskConfiguration =
        serde_json::from_value(archive.content["configuration"].clone())?;
    let record = configuration.validate(&result.session_id, &result.job_id)?;
    let export = json!({"schemaVersion":1,"kind":"project_music_reuse_source",
        "assetId":archive.asset_id,"assetContentSha256":archive.content_sha256,"projectId":archive.project_id,
        "sourceSessionId":result.session_id,"jobId":result.job_id,"snapshotSha256":record.generation_config_sha256(),
        "outputSha256":result.sha256,"outputBytes":result.bytes,"sourceSessionDeleted":true,"projectArchiveReopenedVerified":true,
        "databasePath":directory.join("sessions/sessions.db").canonicalize()?,"publishedPath":content.path(),"result":result});
    drop((content, original));
    reopened_pool.close().await;
    Ok(export)
}

#[tokio::test]
#[ignore = "Measures actual cross-session music reuse from a retained real project asset"]
async fn measure_project_music_reuse() -> Result<()> {
    let mode = std::env::var("LUMINA_PROJECT_MUSIC_REUSE")?;
    ensure!(
        matches!(mode.as_str(), "measurement" | "domain_measurement"),
        "Project music reuse requires explicit test measurement mode"
    );
    let domain = mode == "domain_measurement";
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
    let case_bytes = read_bounded(
        &run.join("project-reuse-case.json"),
        if domain { 1024 * 1024 } else { 65536 },
    )?;
    let case: Value = serde_json::from_slice(&case_bytes)?;
    let case_sha = Sha256Digest::of_bytes(&case_bytes);
    let source = validate_case(&validation, &case, domain)?;
    let parent_asset = case_string(&case, "parentAssetId")?;
    let parent_content_sha = case_string(&case, "parentAssetContentSha256")?;
    let parent_wav = case_string(&case, "parentOutputSha256")?;
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
        .read(&child.id, parent_asset)
        .await?
        .context("Real parent archive missing")?;
    ensure!(
        parent.content_sha256 == parent_content_sha && parent.project_id == project_id,
        "Parent archive changed"
    );
    let parent_asset_bytes: i64 = sqlx::query_scalar(
        "SELECT length(CAST(content_json AS BLOB)) FROM project_assets_v1 WHERE asset_id=?",
    )
    .bind(parent_asset)
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
    let parent_content = media.open_content(parent_session, parent_asset).await?;
    if let Some(source) = &source {
        ensure!(
            source["snapshotSha256"] == parent_record.generation_config_sha256(),
            "Source snapshot differs from archived configuration"
        );
        ensure!(
            source["result"] == parent.content["result"],
            "Source result differs from archived media"
        );
        let source_run = PathBuf::from(case_string(&case, "parentReportPath")?)
            .canonicalize()?
            .parent()
            .context("Missing source directory")?
            .to_path_buf();
        ensure!(
            parent_content.path().canonicalize()?
                == contained_path(source, "publishedPath", &source_run)?,
            "Archived source path differs from descriptor"
        );
    }
    ensure!(
        files::digest(&mut File::open(parent_content.path())?)? == parent_wav,
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
            parent_asset_id: parent_asset.into(),
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
    let mut expected_parameters = serde_json::to_value(&parent_snapshot.parameters)?;
    for (key, value) in case["patch"]
        .as_object()
        .context("Missing parameter patch")?
    {
        expected_parameters[key] = value.clone();
    }
    ensure!(
        serde_json::to_value(&task.snapshot().parameters)? == expected_parameters,
        "Child changed parameters outside the requested patch"
    );
    if !domain {
        ensure!(
            task.snapshot().parameters.seed == 1 && parent_snapshot.parameters.seed == 42,
            "Seed patch was not applied"
        );
    }
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
    let mut owner_request = json!({"parentSnapshot":parent_snapshot,"parentSnapshotDigest":parent_record.generation_config_sha256(),
        "childSessionId":child.id,"childJobId":job,"patch":case["patch"],"expectedChildSnapshotDigest":digest,
        "implementationSourcesDigest":implementation_sources_digest,"payloadTier":payload_tier,"parentAssetId":parent_asset,"parentAssetContentSha256":parent_content_sha});
    if domain {
        owner_request["caseSha256"] = json!(case_sha);
    }
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
            && (domain || result.sha256 != parent_wav),
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
        child_archive.content["projectLineage"]["parentAssetId"] == parent_asset
            && child_archive.content["projectLineage"]["parentAssetContentSha256"]
                == parent_content_sha
            && child_archive.content["projectLineage"]["patch"] == case["patch"],
        "Child lineage differs from the requested patch"
    );
    ensure!(
        assets.read(&child.id, parent_asset).await? == Some(parent.clone()),
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
        reopened_assets.read(&child.id, &result.result_id).await? == Some(child_archive.clone())
            && reopened_assets.read(&child.id, parent_asset).await? == Some(parent.clone())
            && files::digest(&mut File::open(parent_content.path())?)? == parent_wav,
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
    let export_source = if domain {
        Some(export_retained_source(&directory, result, &child_archive, &parent).await?)
    } else {
        None
    };
    app.shutdown().await?;
    let after = logical_bytes(&directory).context("Project reuse final directory measurement")?;
    write_report(
        &run.join("publication.json"),
        json!({"schemaVersion":2,
        "implementation":"media::task_output::import_readers","implementationSourcesSha256":Sha256Digest::of_serializable(&sources())?,
        "sources":sources(),"projectReuseRequestSha256":request_sha,"publicationContext":publication,"caseSha256":case_sha,
        "snapshotDigest":digest,"publishedPath":published_path,"result":result,"reopenedVerified":true,"exportSource":export_source,
        "inputSha256":native_sha,"publishedSha256":result.sha256,"publishedBytes":result.bytes,
        "parentAssetBytes":parent_asset_bytes,"authorizationBytes":authorization_bytes,"preparationMillis":preparation_millis,"outputArchiveBytes":output_archive_bytes,
        "projectArchive":{"verified":true,"reopenedVerified":true,"parentUnchanged":true,"lineageVerified":true,"sourceSessionAbsent":true},
        "checkpoints":[{"directoryLogicalBytes":before},{"directoryLogicalBytes":published},{"directoryLogicalBytes":after}]}),
    )
}
