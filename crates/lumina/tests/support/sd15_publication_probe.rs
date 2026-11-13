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

const OWNER: &str = "lifecycle::task_resources::tests::sd15_probe::measure_installed_sd15_owner";
const IMPLEMENTATION: &str = "media::task_output::import_readers";
#[path = "../../../lumina-local-inference/src/lifecycle/task_resources/sd15_cases.rs"]
mod cases;

fn write_report(path: &Path, value: Value) -> Result<()> {
    let mut file = File::options().write(true).create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(&value)?)?;
    file.sync_all()?;
    Ok(())
}

fn sources() -> BTreeMap<&'static str, String> {
    macro_rules! paths {
        ($($path:literal),+ $(,)?) => { BTreeMap::from([$(($path,Sha256Digest::of_bytes(include_bytes!(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../", $path))).as_str().into())),+]) }
    }
    paths!(
        "crates/lumina/src/media/domain.rs",
        "crates/lumina/src/media/files.rs",
        "crates/lumina/src/media/formats.rs",
        "crates/lumina/src/media/store.rs",
        "crates/lumina/src/media/task_configuration.rs",
        "crates/lumina/src/media/task_output.rs",
        "crates/lumina/src/media/task_service.rs",
        "crates/lumina/src/session/session_manager.rs",
        "crates/lumina/tests/support/sd15_publication_probe.rs",
        "crates/lumina-local-inference/src/lifecycle/files.rs",
        "crates/lumina-local-inference/src/lifecycle/task_parameters.rs",
        "crates/lumina-local-inference/src/lifecycle/task_parameters/compile.rs",
        "crates/lumina-local-inference/src/lifecycle/task_parameters/models.json",
        "crates/lumina-local-inference/src/lifecycle/task_parameters/protocols.json",
        "crates/lumina-local-inference/src/lifecycle/task_parameters/inventory.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime/plan.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime/process.rs",
        "crates/lumina-local-inference/src/lifecycle/native_task_runtime/output.rs",
        "crates/lumina-local-inference/src/lifecycle/task_resources/sd15-cases.json",
        "crates/lumina-local-inference/src/lifecycle/task_resources/sd15_cases.rs",
        "crates/lumina-local-inference/src/lifecycle/task_resources/sd_conditions.rs",
        "crates/lumina-local-inference/src/lifecycle/task_resources/sd15_probe.rs"
    )
}

fn logical_bytes(path: &Path) -> Result<u64> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.file_attributes() & 0x400 == 0,
        "Measurement directory contains a reparse point"
    );
    if !meta.is_dir() {
        return Ok(meta.len());
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        total = total
            .checked_add(logical_bytes(&entry?.path())?)
            .context("Directory size overflow")?;
    }
    Ok(total)
}

#[tokio::test]
#[ignore = "Runs the installed SD1.5 owner and verifies an actual result on reopening its session"]
async fn measure_installed_sd15_publication() -> Result<()> {
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
            "sd15-image".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    let session = created.id.as_str();
    let call_id = "sd15-image-request";
    let storage = manager.storage();
    let pool = storage.pool().await?;
    let media = MediaResultStore::new(pool.clone());
    let config = TaskConfigurationStore::new(pool.clone());
    let root = media
        .register_output_root(&storage.media_output_directory())
        .await?;
    let app = Arc::new(InstallationApplication::new(
        &validation.join("m4-managed-generation-20261113-v2-r1"),
        std::sync::Arc::new(|| Ok(None)),
    ));
    storage.bind_native_installations(app.clone())?;
    let service = app.service().await?;
    service.wait_ready().await?;
    let catalog = NativeTaskCatalog::active(service.repository())?;
    let entry = catalog
        .list(NativeTaskIntent::TextToImage)
        .into_iter()
        .find(|entry| {
            entry.identity.installation_id == "acceptance-sd15"
                && entry.identity.version_id == "version:acceptance-sd15"
        })
        .context("Missing active managed SD1.5")?;
    let (case_id, case) = cases::selected()?;
    let request = GenerationTaskRequest {
        session_id: session.into(),
        job_id: crate::media::NativeTaskJobs::job_id(session, call_id)?,
        parameters: GenerationParameters::Image(Sd15ImageParameters {
            prompt: Some(case.prompt.clone()),
            negative_prompt: (case_id != 1).then(|| case.negative_prompt.clone()),
            seed: Some(GenerationSeed::Value { value: case.seed }),
            ..Default::default()
        }),
    };
    let plan = catalog.prepare(&entry.identity, serde_json::to_value(&request)?)?;
    let NativeTaskPlan::Generation(task) = &plan else {
        anyhow::bail!("Expected SD1.5")
    };
    let digest = plan.snapshot_digest()?;
    let association = MediaAssociation::ToolCall {
        tool_call_id: call_id.into(),
    };
    let before = logical_bytes(&directory)?;
    let mode = std::env::var("LUMINA_TASK_RESOURCE_VERIFY").unwrap_or_default();
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
        let mut input = json!({"identity":entry.identity,"parameters":{"prompt":case.prompt,"seed":{"kind":"value","value":case.seed}}});
        if case_id != 1 {
            input["parameters"]["negativePrompt"] = json!(case.negative_prompt);
        }
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
        ensure!(reply.is_error != Some(true), "Image tool failed: {reply:?}");
        let [status, link] = reply.content.as_slice() else {
            anyhow::bail!("Expected a completion and session image link")
        };
        let RawContent::Text(status) = &status.raw else {
            anyhow::bail!("Missing tool completion")
        };
        let status: Value = serde_json::from_str(&status.text)?;
        let id = status["resultIds"][0]
            .as_str()
            .context("No completed result")?;
        let RawContent::ResourceLink(link) = &link.raw else {
            anyhow::bail!("Missing native image link")
        };
        ensure!(
            status["status"] == "completed"
                && status["jobId"] == request.job_id
                && link.uri == format!("lumina-media://result/{id}")
                && link.mime_type.as_deref() == Some("image/png"),
            "Native tool returned an unexpected media result"
        );
        write_report(
            &run.join("tool-response.json"),
            json!({"tool":"native_tasks__run","request":input,"response":reply}),
        )?;
        let held = media.open_content(session, id).await?;
        ImportedTaskOutput::Media(vec![held.result().clone()])
    } else {
        config.save_generation(task).await?;
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
            json!({"implementation":IMPLEMENTATION,"ownerTest":OWNER,"sources":sources()}),
        )?;
        let path = run.join("output-root/native/output.png");
        let mut file = File::options().read(true).share_mode(1).open(path)?;
        let bytes = file.metadata()?.len();
        ensure!(
            (1..=32u64 << 20).contains(&bytes),
            "PNG outside probe limit"
        );
        let sha256 = files::digest(&mut file)?;
        file.rewind()?;
        let reader = OutputReader {
            name: "output.png".into(),
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
        anyhow::bail!("Expected exactly one PNG")
    };
    ensure!(
        matches!(
            result.spec,
            MediaSpec::Image {
                width: 512,
                height: 512
            }
        ) && result.mime == "image/png"
            && result.association == association,
        "Published image or message association differs"
    );
    let published = logical_bytes(&directory)?;
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
    let image = held.path().to_path_buf();
    let hash = files::digest(&mut File::open(&image)?)?;
    ensure!(
        result.sha256 == hash,
        "Published PNG hash differs after reopen"
    );
    drop(held);
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
            "snapshotDigest":digest,"publishedPath":image,"result":result,"reopenedVerified":true,
            "inputSha256":hash,"publishedSha256":result.sha256,"publishedBytes":result.bytes,
            "checkpoints":[{"directoryLogicalBytes":before},{"directoryLogicalBytes":published},{"directoryLogicalBytes":logical_bytes(&directory)?}]
        }),
    )
}
