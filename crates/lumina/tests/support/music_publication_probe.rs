use super::super::{files, import_readers, ImportedTaskOutput, OutputReader};
use crate::media::{MediaAssociation, MediaResultStore, MediaSpec, TaskConfigurationStore};
use crate::session::session_manager::SessionStorage;
use anyhow::{ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Seek, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const OWNER: &str = "lifecycle::task_resources::tests::music_probe::measure_installed_music_owner";
const IMPLEMENTATION: &str = "media::task_output::import_readers";
const PROMPT: &str = "A gentle solo piano melody, clear notes, calm instrumental music, no vocals.";

pub(super) fn write_report(path: &Path, value: Value) -> Result<()> {
    let mut file = File::options().write(true).create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(&value)?)?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn sources() -> BTreeMap<&'static str, String> {
    macro_rules! paths {
        ($($path:literal),+ $(,)?) => { BTreeMap::from([$(($path,Sha256Digest::of_bytes(include_bytes!(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../", $path))).as_str().into())),+]) }
    }
    paths!(
        "crates/lumina/src/media/domain.rs",
        "crates/lumina/src/media/files.rs",
        "crates/lumina/src/media/formats.rs",
        "crates/lumina/src/media/store.rs",
        "crates/lumina/src/media/project_assets.rs",
        "crates/lumina/src/project/assets.rs",
        "crates/lumina/src/media/task_configuration.rs",
        "crates/lumina/src/media/project_task_reuse.rs",
        "crates/lumina/src/media/task_output.rs",
        "crates/lumina/src/media/task_service.rs",
        "crates/lumina/src/session/session_manager.rs",
        "crates/lumina/tests/support/music_publication_probe.rs",
        "crates/lumina-local-inference/src/lifecycle/files.rs",
        "crates/lumina-local-inference/src/lifecycle/task_parameters.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/music_task_parameters.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/reuse.rs",
        "crates/lumina-local-inference/src/lifecycle/project_task_scope.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/models.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/services.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/profiles.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime/plan.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime/process.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime/output.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/stable_audio.json",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/install.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters.rs",
        "crates/lumina-local-inference/src/lifecycle/task_resources/music_probe.rs"
    )
}

pub(super) fn logical_bytes(path: &Path) -> Result<u64> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("Inspect measurement entry {}", path.display()))?;
    ensure!(
        meta.file_attributes() & 0x400 == 0,
        "Measurement directory contains a reparse point"
    );
    if !meta.is_dir() {
        return Ok(meta.len());
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("Enumerate measurement directory {}", path.display()))?
    {
        total = total
            .checked_add(logical_bytes(&entry?.path())?)
            .context("Directory size overflow")?;
    }
    Ok(total)
}

#[tokio::test]
#[ignore = "Runs the installed music owner and verifies an actual result on reopening its session"]
async fn measure_installed_music_publication() -> Result<()> {
    let validation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let run = PathBuf::from(
        std::env::var_os("LUMINA_TASK_RESOURCE_RUN_DIR")
            .context("Missing measurement directory")?,
    )
    .canonicalize()?;
    ensure!(
        run.parent() == Some(validation.as_path())
            && run.metadata()?.file_attributes() & 0x400 == 0,
        "Unexpected run directory"
    );
    let directory = run.join("output-root/publication");
    std::fs::create_dir(&directory)?;
    let domain_case = std::env::var_os("LUMINA_MUSIC_DOMAIN_CASE_FILE")
        .map(|path| -> Result<_> {
            let path = PathBuf::from(path).canonicalize()?;
            ensure!(
                path == run.join("domain-case.json"),
                "Unexpected music case path"
            );
            let bytes = std::fs::read(path)?;
            ensure!(bytes.len() <= 65536, "Music case exceeds byte limit");
            let value: Value = serde_json::from_slice(&bytes)?;
            Ok((value, Sha256Digest::of_bytes(&bytes)))
        })
        .transpose()?;
    let domain_case_sha = domain_case.as_ref().map(|(_, digest)| digest.clone());
    use crate::agents::platform_extensions::{PlatformExtensionContext, PLATFORM_EXTENSIONS};
    use crate::agents::ToolCallContext;
    use crate::config::LuminaMode;
    use crate::session::{SessionManager, SessionType};
    use rmcp::model::RawContent;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    let manager = Arc::new(SessionManager::new(directory.clone()));
    let created = manager
        .create_session(
            directory.clone(),
            "stable-music".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    let session = created.id.as_str();
    let call_id = "stable-music-request";
    let storage = manager.storage();
    let pool = storage.pool().await?;
    let project = if domain_case.is_some() {
        let projects = crate::project::ProjectService::new(Arc::clone(storage));
        let project = projects
            .open_project(&directory, Some("Music resource domain"))
            .await?;
        manager
            .update(session)
            .project_id(Some(project.id.clone()))
            .apply()
            .await?;
        Some(project)
    } else {
        None
    };
    let media = MediaResultStore::new(pool.clone());
    let config = TaskConfigurationStore::new(pool.clone());
    let root = media
        .register_output_root(&storage.media_output_directory())
        .await?;
    let app = Arc::new(InstallationApplication::new(
        &validation.join("m4-managed-audio-install-20261113-r1"),
        std::sync::Arc::new(|| Ok(None)),
    ));
    storage.bind_native_installations(app.clone())?;
    let service = app.service().await?;
    service.wait_ready().await?;
    let catalog = NativeTaskCatalog::active(service.repository())?;
    let entry = catalog
        .list(NativeTaskIntent::TextToMusic)
        .into_iter()
        .find(|entry| {
            entry.identity.installation_id == "acceptance-stable-audio-small-music-cpu"
                && entry.identity.version_id == "acceptance-stable-audio-small-music-cpu-v2"
        })
        .context("Missing active managed music")?;
    let mode = std::env::var("LUMINA_TASK_RESOURCE_VERIFY").unwrap_or_default();
    ensure!(
        domain_case.is_none() || mode == "measurement",
        "Domain cases require isolated measurement mode"
    );
    let parameters = domain_case.as_ref().map_or_else(
        || json!({"prompt":PROMPT,"seed":42,"steps": if mode=="needs_profiling" {9} else {8}}),
        |(value, _)| value["parameters"].clone(),
    );
    let mut request_json = parameters.clone();
    request_json["sessionId"] = json!(session);
    request_json["jobId"] = json!(crate::media::NativeTaskJobs::job_id(session, call_id)?);
    let request: StableAudioMusicRequest = serde_json::from_value(request_json)?;
    let plan = catalog.prepare(&entry.identity, serde_json::to_value(&request)?)?;
    let NativeTaskPlan::StableAudioMusic(task) = &plan else {
        anyhow::bail!("Expected music")
    };
    let digest = plan.snapshot_digest()?;
    if domain_case.is_some() {
        let p = &task.snapshot().parameters;
        ensure!(
            serde_json::to_value(p)? == parameters
                && p.prompt.len() <= 1024
                && p.negative_prompt.is_none()
                && p.duration_seconds == 4.
                && p.steps == 8
                && p.guidance_scale == 1.
                && p.apg_scale == 1.
                && p.sampler == StableAudioSampler::Pingpong
                && p.duration_padding_seconds == 6.
                && p.truncate_to_duration
                && p.chunked_decode,
            "Music domain changes an unmeasured effective parameter"
        );
        crate::project::ProjectAssetStore::new(pool.clone())
            .freeze_job(session, &request.job_id, None)
            .await?;
    }
    let association = MediaAssociation::ToolCall {
        tool_call_id: call_id.into(),
    };
    let before = logical_bytes(&directory)?;
    let production = matches!(mode.as_str(), "production" | "needs_profiling");
    let output = if production {
        let def = &PLATFORM_EXTENSIONS["native_tasks"];
        let client = (def.client_factory)(PlatformExtensionContext {
            extension_manager: None,
            session_manager: manager.clone(),
            session: Some(Arc::new(created.clone())),
            use_login_shell_path: false,
        });
        let ctx = ToolCallContext::new(session.into(), None, Some(call_id.into()));
        let input = json!({"identity":entry.identity,"parameters":parameters});
        let reply = client
            .call_tool(
                &ctx,
                "run",
                input.as_object().cloned(),
                CancellationToken::new(),
            )
            .await?;
        if mode == "needs_profiling" {
            ensure!(
                reply.is_error == Some(true) && reply.content.len() == 1,
                "Expected deferred generation"
            );
            let RawContent::Text(status) = &reply.content[0].raw else {
                anyhow::bail!("Unexpected result for an unmeasured workload")
            };
            let status: Value = serde_json::from_str(&status.text)?;
            ensure!(
                status["status"] == "needs_profiling" && status["jobId"] == request.job_id,
                "Wrong deferral: {status}"
            );
            write_report(
                &run.join("deferred.json"),
                json!({"request":input, "status":status, "snapshotDigest":digest}),
            )?;
            pool.close().await;
            app.shutdown().await?;
            return Ok(());
        }
        ensure!(reply.is_error != Some(true), "Music tool failed: {reply:?}");
        let [status, link] = reply.content.as_slice() else {
            anyhow::bail!("Expected a completion and session audio link")
        };
        let RawContent::Text(status) = &status.raw else {
            anyhow::bail!("Missing tool completion")
        };
        let status: Value = serde_json::from_str(&status.text)?;
        let id = status["resultIds"][0]
            .as_str()
            .context("No completed result")?;
        let RawContent::ResourceLink(link) = &link.raw else {
            anyhow::bail!("Missing native audio link")
        };
        ensure!(
            status["status"] == "completed"
                && status["jobId"] == request.job_id
                && link.uri == format!("lumina-media://result/{id}")
                && link.mime_type.as_deref() == Some("audio/wav"),
            "Native tool returned an unexpected media result"
        );
        let replay = client
            .call_tool(
                &ctx,
                "run",
                input.as_object().cloned(),
                CancellationToken::new(),
            )
            .await?;
        ensure!(
            serde_json::to_value(&replay)? == serde_json::to_value(&reply)?,
            "Repeated music call did not recover the original result"
        );
        write_report(
            &run.join("tool-response.json"),
            json!({"tool":"native_tasks__run","request":input,"response":reply,
                "replay":replay,"sameCallReplayed":true}),
        )?;
        let held = media.open_content(session, id).await?;
        ImportedTaskOutput::Media(vec![held.result().clone()])
    } else {
        config.save_music(task).await?;
        write_report(&run.join("request.json"), serde_json::to_value(&request)?)?;
        write_report(
            &run.join("snapshot.json"),
            serde_json::to_value(task.snapshot())?,
        )?;
        let owner = PathBuf::from(
            std::env::var_os("LUMINA_TASK_RESOURCE_OWNER").context("Missing owner probe")?,
        )
        .canonicalize()?;
        ensure!(
            owner.parent() == Some(validation.join("../debug/deps").canonicalize()?.as_path()),
            "Unexpected probe location"
        );
        let child = std::process::Command::new(owner)
            .args(["--exact", OWNER, "--ignored", "--nocapture"])
            .output()?;
        let stdout = String::from_utf8_lossy(&child.stdout);
        ensure!(
            child.status.success()
                && stdout
                    .lines()
                    .any(|line| line == format!("test {OWNER} ... ok")),
            "Owner failed: {stdout} {}",
            String::from_utf8_lossy(&child.stderr)
        );
        write_report(
            &run.join("driver.json"),
            json!({"implementation":IMPLEMENTATION,"ownerTest":OWNER,"sources":sources(),"domainCaseSha256":domain_case_sha}),
        )?;
        let path = run.join("output-root/native/output.wav");
        let mut file = File::options().read(true).share_mode(1).open(path)?;
        let bytes = file.metadata()?.len();
        ensure!(
            (1..=32u64 << 20).contains(&bytes),
            "WAV outside probe limit"
        );
        let sha256 = files::digest(&mut file)?;
        file.rewind()?;
        let reader = OutputReader {
            name: "output.wav".into(),
            bytes,
            sha256,
            file,
        };
        import_readers(
            &config,
            &media,
            &plan,
            &digest,
            vec![reader],
            &root.root_id,
            association.clone(),
        )
        .await?
    };
    let ImportedTaskOutput::Media(results) = output else {
        anyhow::bail!("Expected media")
    };
    let [result] = results.as_slice() else {
        anyhow::bail!("Expected exactly one WAV")
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
            && result.association == association,
        "Published audio or tool association differs"
    );
    let published = logical_bytes(&directory)?;
    let project_archive = if let Some(project) = &project {
        let reader = manager
            .create_session(
                directory.clone(),
                "Music archive reader".into(),
                SessionType::User,
                LuminaMode::Auto,
            )
            .await?;
        manager
            .update(&reader.id)
            .project_id(Some(project.id.clone()))
            .apply()
            .await?;
        let assets = crate::project::ProjectAssetStore::new(pool.clone());
        let archived = assets
            .read(&reader.id, &result.result_id)
            .await?
            .context("Missing project music archive")?;
        let archived_configuration: crate::media::task_configuration::ArchivedTaskConfiguration =
            serde_json::from_value(archived.content["configuration"].clone())?;
        let original = config
            .read(session, &request.job_id)
            .await?
            .context("Missing music configuration")?;
        ensure!(
            archived.project_id == project.id
                && archived.source_session_id == session
                && archived.job_id == request.job_id
                && archived.content["result"] == serde_json::to_value(result)?
                && archived_configuration.validate(session, &request.job_id)? == original
                && media.read(&reader.id, &result.result_id).await?.is_none(),
            "Project music archive scope or configuration differs"
        );
        Some((reader.id, archived))
    } else {
        None
    };
    pool.close().await;
    let reopened_storage = SessionStorage::new(directory.clone());
    let reopened_pool = reopened_storage.pool().await?;
    let held = MediaResultStore::new(reopened_pool.clone())
        .open_content(session, &result.result_id)
        .await?;
    let record = TaskConfigurationStore::new(reopened_pool.clone())
        .verify_result(result)
        .await?;
    ensure!(
        held.result() == result && record.generation_config_sha256() == digest.as_str(),
        "Reopened result or configuration changed"
    );
    let wav = held.path().to_path_buf();
    let hash = files::digest(&mut File::open(&wav)?)?;
    ensure!(
        result.sha256 == hash
            && (domain_case.is_some()
                || hash == "afa7d6699180ed7c1bd7fdd4524af6b4faf15b9e077f25d4b70dc5787693f2e5"),
        "Published WAV hash differs after reopen"
    );
    drop(held);
    if let Some((reader, archived)) = &project_archive {
        ensure!(
            crate::project::ProjectAssetStore::new(reopened_pool.clone())
                .read(reader, &result.result_id)
                .await?
                .as_ref()
                == Some(archived),
            "Reopened project archive changed"
        );
        let native_hash =
            files::digest(&mut File::open(run.join("output-root/native/output.wav"))?)?;
        let owner: Value = serde_json::from_reader(File::open(run.join("owner.json"))?)?;
        ensure!(
            native_hash == hash && owner["outputSha256"] == hash,
            "Native and published music bytes differ"
        );
    }
    reopened_pool.close().await;
    app.shutdown().await?;
    write_report(
        &run.join(if production {
            "production.json"
        } else {
            "publication.json"
        }),
        json!({
            "schemaVersion":2,"implementation":IMPLEMENTATION,"implementationSourcesSha256":Sha256Digest::of_serializable(&sources())?,
            "snapshotDigest":digest,"publishedPath":wav,"result":result,"reopenedVerified":true,
            "inputSha256":hash,"publishedSha256":result.sha256,"publishedBytes":result.bytes,
            "domainCaseSha256":domain_case_sha,
            "projectArchive":{"verified":project_archive.is_some(),"reopenedVerified":project_archive.is_some()},
            "checkpoints":[{"directoryLogicalBytes":before},{"directoryLogicalBytes":published},{"directoryLogicalBytes":logical_bytes(&directory)?}]
        }),
    )
}
