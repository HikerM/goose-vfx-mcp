//! Runs the measured application host while the finite native owner and speech engine are resident.

use super::super::{files, import_readers, ImportedTaskOutput, OutputReader};
use crate::media::{
    BeginMediaPublish, MediaAssociation, MediaKind, MediaResultStore, MediaSource, MediaSpec,
    PublishMediaRequest, TaskConfigurationStore,
};
use crate::session::session_manager::SessionStorage;
use anyhow::{ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::{
    AudioTaskMediaReference, InstallationApplication, NativeTaskCatalog, NativeTaskIntent,
    NativeTaskPlan, PocketTtsRequest, Sha256Digest,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

const IMPLEMENTATION: &str = "media::task_output::import_readers";
const NATIVE_NAME: &str = "output.wav";
const MAX_OUTPUT: u64 = 32 * 1024 * 1024;

fn validation_run() -> Result<PathBuf> {
    let allowed = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let supplied = PathBuf::from(
        std::env::var_os("LUMINA_TASK_RESOURCE_RUN_DIR").context("Missing profiling directory")?,
    );
    ensure!(
        supplied.is_absolute(),
        "Expected absolute profiling directory"
    );
    for path in supplied.ancestors() {
        ensure!(
            std::fs::symlink_metadata(path)?.file_attributes() & 0x400 == 0,
            "Profiling directory contains a reparse point"
        );
    }
    let run = supplied.canonicalize()?;
    ensure!(
        run.parent() == Some(allowed.as_path()),
        "Expected a fresh direct child of the profiling root"
    );
    Ok(run)
}

fn write_new(run: &Path, name: &str, value: &Value) -> Result<()> {
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(run.join(name))?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}

fn source_digests() -> BTreeMap<&'static str, String> {
    // Paths and contents are embedded at compile time. The controller independently
    // hashes each file in its checkout before launching the native process.
    macro_rules! sources {
        ($($path:literal),+ $(,)?) => {
            BTreeMap::from([$(
                ($path, Sha256Digest::of_bytes(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../", $path))).as_str().to_owned())
            ),+])
        };
    }
    sources!(
        "crates/lumina/src/media/avi_audio.rs",
        "crates/lumina/src/media/domain.rs",
        "crates/lumina/src/media/files.rs",
        "crates/lumina/src/media/formats.rs",
        "crates/lumina/src/media/store.rs",
        "crates/lumina/src/media/task_configuration.rs",
        "crates/lumina/src/media/task_output.rs",
        "crates/lumina/src/session/session_manager.rs",
        "crates/lumina/tests/support/task_resource_publication_probe.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/models.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/profiles.rs",
        "crates/lumina-local-inference/src/lifecycle/audio_task_parameters/services.rs",
        "crates/lumina-local-inference/src/lifecycle/files.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime/plan.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime/process.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime/output.rs",
        "crates/lumina-local-inference/src/lifecycle/task_resources/pocket_probe.rs",
    )
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut bytes = 0_u64;
    for entry in std::fs::read_dir(path)? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Unexpected reparse point in profiling output"
        );
        let size = if metadata.is_dir() {
            directory_bytes(&path)?
        } else {
            metadata.len()
        };
        bytes = bytes
            .checked_add(size)
            .context("Profiling directory size overflow")?;
    }
    Ok(bytes)
}

fn checkpoint(name: &str, started: Instant, directory: &Path) -> Result<Value> {
    Ok(
        json!({"name": name, "elapsedMicros": started.elapsed().as_micros(), "directoryLogicalBytes": directory_bytes(directory)?}),
    )
}

#[tokio::test]
#[ignore = "Measures real installed PocketTTS input, finite owner, import and restart in a single monitored process tree"]
async fn measure_installed_pocket_publication() -> Result<()> {
    let started = Instant::now();
    let run = validation_run()?;
    let validation = run.parent().context("No validation root")?;
    let directory = run.join("output-root/publication");
    std::fs::create_dir(&directory)?;
    let storage = SessionStorage::new(directory.clone());
    let pool = storage.pool().await?;
    let session_id = "resource-probe-session";
    sqlx::query("INSERT INTO sessions (id, working_dir) VALUES (?, ?)")
        .bind(session_id)
        .bind(directory.to_str().context("Invalid directory")?)
        .execute(pool)
        .await?;
    let media = MediaResultStore::new(pool.clone());
    let config = TaskConfigurationStore::new(pool.clone());
    let root = media
        .register_output_root(&storage.media_output_directory())
        .await?;
    let source_path = validation.join("asr-input/english-base.wav");
    let mut source = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(source_path)?;
    let expected_voice = "b29307e631d805d7079910c8c9c5ee14777549954b737e15a5770db7ac2da2e9";
    ensure!(
        files::digest(&mut source)? == expected_voice,
        "Unrecognized source voice"
    );
    source.rewind()?;
    let BeginMediaPublish::Staging(mut staged) = media
        .begin_publish(PublishMediaRequest {
            result_id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            job_id: "voice-fixture".into(),
            root_id: root.root_id.clone(),
            kind: MediaKind::Audio,
            association: MediaAssociation::Message {
                message_id: "sample".into(),
            },
            source: MediaSource::Mcp {
                server_id: "resource-probe".into(),
                tool_name: "import-voice-fixture".into(),
                tool_call_id: "voice-fixture".into(),
                input_refs: vec![],
            },
        })
        .await?
    else {
        anyhow::bail!("Reference already exists")
    };
    std::io::copy(&mut source, &mut staged)?;
    let voice = media.publish(staged).await?;
    let voice_content = media.open_content(session_id, &voice.result_id).await?;
    let MediaSpec::Audio {
        sample_rate,
        channels,
        sample_frames,
        ..
    } = &voice.spec
    else {
        anyhow::bail!("Expected decoded reference audio")
    };
    let model_data = validation.join("m4-managed-audio-install-20261113-r1");
    let app = InstallationApplication::new(&model_data, std::sync::Arc::new(|| Ok(None)));
    let service = app.service().await?;
    service.wait_ready().await?;
    let selected_version = std::env::var("LUMINA_TASK_RESOURCE_VERSION")?;
    let active = service
        .repository()
        .active_installations()?
        .into_iter()
        .find(|entry| entry.installation_id == "acceptance-pocket-tts")
        .context("No active PocketTTS version")?;
    let selected = service
        .repository()
        .installed_version(&selected_version)?
        .context("Requested PocketTTS version is not installed")?;
    ensure!(selected.manifest.installation_id == active.installation_id
            && selected.manifest.plan.runtime == lumina_providers::local_inference::lifecycle::AudioTaskProfile::PocketTtsEnglishCpu.runtime_build(),
            "Selected installation has a different model or runtime");
    if active.active_version_id != selected_version {
        service.repository().activate_installation_version(
            &active.installation_id,
            &active.active_version_id,
            &selected_version,
        )?;
    }
    let catalog = NativeTaskCatalog::active(service.repository())?;
    let entry = catalog
        .list(NativeTaskIntent::SpeechSynthesis)
        .into_iter()
        .find(|v| {
            v.identity.installation_id == "acceptance-pocket-tts"
                && v.identity.version_id == selected_version
        })
        .context("Missing PocketTTS installation")?;
    let text = std::env::var("LUMINA_TASK_RESOURCE_TEXT")?;
    let seed = std::env::var("LUMINA_TASK_RESOURCE_SEED")
        .unwrap_or_else(|_| "42".into())
        .parse::<u32>()?;
    let max_frames = std::env::var("LUMINA_TASK_RESOURCE_MAX_FRAMES")
        .unwrap_or_else(|_| "0".into())
        .parse::<u32>()?;
    ensure!(
        matches!(max_frames, 0 | 32),
        "Unregistered diagnostic frame cap"
    );
    let request = PocketTtsRequest {
        session_id: session_id.into(),
        job_id: "measure".into(),
        text,
        voice_reference: AudioTaskMediaReference {
            session_id: session_id.into(),
            result_id: voice.result_id.clone(),
            sha256: Sha256Digest::parse(expected_voice)?,
            size_bytes: voice.bytes,
            mime_type: voice.mime.clone(),
            sample_rate: *sample_rate,
            channels: u32::from(*channels),
            sample_frames: *sample_frames,
        },
        seed: Some(seed),
        temperature: None,
        max_frames: Some(max_frames),
        text_chunk_tokens: None,
        frames_after_eos: (max_frames > 0).then_some(max_frames as i32),
    };
    let plan = catalog.prepare(&entry.identity, serde_json::to_value(&request)?)?;
    let NativeTaskPlan::PocketTts(ref task) = plan else {
        anyhow::bail!("Expected PocketTTS task")
    };
    let digest = plan.snapshot_digest()?;
    if std::env::var("LUMINA_TASK_RESOURCE_VERIFY").as_deref() == Ok("production") {
        let engines = crate::media::configured_task_engines(plan.runtime())?;
        let outcome = crate::media::execute_prepared_task(
            &service,
            &engines,
            &storage,
            session_id,
            plan.clone(),
            MediaAssociation::Message {
                message_id: "verified-request".into(),
            },
        )
        .await?;
        let crate::media::PreparedTaskExecution::Completed(ImportedTaskOutput::Media(results)) =
            outcome
        else {
            anyhow::bail!("Measured task did not complete through the production admission and execution path: {outcome:?}");
        };
        let [result] = results.as_slice() else {
            anyhow::bail!("Expected one production audio result")
        };
        let mut changed = task.snapshot().parameters.text.clone();
        changed.push_str(" Unmeasured variation.");
        let mut variant = serde_json::to_value(&request)?;
        variant["text"] = json!(changed);
        let other = catalog.prepare(&entry.identity, variant)?;
        let assessment =
            lumina_providers::local_inference::lifecycle::assess_current_task_resources(
                &other, &root.path,
            )?;
        ensure!(matches!(assessment, lumina_providers::local_inference::lifecycle::TaskResourceAssessment::NeedsProfiling(_)), "Unmeasured text inherited admission");
        drop(voice_content);
        pool.close().await;
        let restarted = SessionStorage::new(directory);
        let reopened_pool = restarted.pool().await?;
        let content = MediaResultStore::new(reopened_pool.clone())
            .open_content(session_id, &result.result_id)
            .await?;
        ensure!(
            content.result() == result
                && TaskConfigurationStore::new(reopened_pool.clone())
                    .verify_result(result)
                    .await?
                    .generation_config_sha256()
                    == digest.as_str(),
            "Reopened content/configuration differs from production execution"
        );
        write_new(
            &run,
            "production.json",
            &json!({"snapshotDigest":digest,"result":result,"reopenedVerified":true,"unmeasuredTextRejected":true,"seed":seed,"maxFrames":max_frames}),
        )?;
        drop(content);
        reopened_pool.close().await;
        app.shutdown().await?;
        return Ok(());
    }
    let inputs = media.open_native_task_inputs(session_id, &plan).await?;
    config.save_pocket_tts(task).await?;
    write_new(
        &run,
        "snapshot.json",
        &serde_json::to_value(task.snapshot())?,
    )?;
    let owner = PathBuf::from(
        std::env::var_os("LUMINA_TASK_RESOURCE_OWNER").context("Missing owner probe")?,
    )
    .canonicalize()?;
    ensure!(
        owner.parent() == Some(validation.join("../debug/deps").canonicalize()?.as_path()),
        "Owner must come from the current test build"
    );
    let exact = "lifecycle::task_resources::tests::pocket_probe::measure_installed_pocket_owner";
    let output = std::process::Command::new(&owner)
        .args(["--exact", exact, "--ignored", "--nocapture"])
        .env("LUMINA_TASK_RESOURCE_VOICE_PATH", voice_content.path())
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure!(
        output.status.success() && stdout.contains(&format!("test {exact} ... ok")),
        "Owner failed: {stdout} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    write_new(
        &run,
        "driver.json",
        &json!({"implementation":IMPLEMENTATION,"ownerTest":exact,"ownerStdout":stdout,"sources":source_digests()}),
    )?;
    drop(inputs);
    drop(voice_content);
    let setup = checkpoint("existingSessionPoolOpened", started, &directory)?;
    let path = run.join("output-root/native").join(NATIVE_NAME);
    for path in path.ancestors().take_while(|p| *p != run) {
        ensure!(
            std::fs::symlink_metadata(path)?.file_attributes() & 0x400 == 0,
            "Native output contains a reparse point"
        );
    }
    let mut held = OpenOptions::new().read(true).share_mode(1).open(path)?;
    let bytes = held.metadata()?.len();
    ensure!(
        (1..=MAX_OUTPUT).contains(&bytes),
        "Native output outside profiling limit"
    );
    let hash = files::digest(&mut held)?;
    let reader = OutputReader {
        name: NATIVE_NAME.into(),
        file: held.try_clone()?,
        bytes,
        sha256: hash.clone(),
    };
    let collected = checkpoint("nativeFileHeldAndHashed", started, &directory)?;
    let outputs = import_readers(
        &config,
        &media,
        &plan,
        &digest,
        vec![reader],
        &root.root_id,
        MediaAssociation::Message {
            message_id: "resource-probe-only".into(),
        },
    )
    .await?;
    let ImportedTaskOutput::Media(results) = outputs else {
        anyhow::bail!("PocketTTS import returned a transcript");
    };
    let [result] = results.as_slice() else {
        anyhow::bail!("PocketTTS did not produce exactly one media result");
    };
    ensure!(
        result.sha256 == hash && result.bytes == bytes,
        "Published output differs from held source"
    );
    let imported = checkpoint("productionImportCompleted", started, &directory)?;
    drop(held);
    pool.close().await;
    let storage = SessionStorage::new(directory.clone());
    let pool = storage.pool().await?;
    let reopened = MediaResultStore::new(pool.clone())
        .open_content(&result.session_id, &result.result_id)
        .await?;
    ensure!(
        reopened.result() == result,
        "Reopened content differs from the imported result"
    );
    let record = TaskConfigurationStore::new(pool.clone())
        .verify_result(result)
        .await?;
    ensure!(
        record.generation_config_sha256() == digest.as_str(),
        "Reopened result refers to another configuration"
    );
    let verified = checkpoint("resultAndConfigurationReopened", started, &directory)?;
    drop(reopened);
    pool.close().await;
    let closed = checkpoint("reopenedPoolClosed", started, &directory)?;
    app.shutdown().await?;
    write_new(
        &run,
        "publication.json",
        &json!({
            "schemaVersion": 2, "implementation": IMPLEMENTATION, "snapshotDigest": digest,
            "implementationSourcesSha256": Sha256Digest::of_serializable(&source_digests())?,
            "rootId": root.root_id, "inputSha256": hash, "publishedSha256": result.sha256,
            "publishedBytes": result.bytes, "reopenedVerified": true, "result": result,
            "checkpoints": [setup, collected, imported, verified, closed],
        }),
    )
}
