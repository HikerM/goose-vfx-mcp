use super::*;
use anyhow::Context;
use serde_json::json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const PROMPT: &str = "A gentle solo piano melody, clear notes, calm instrumental music, no vocals.";

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectMusicReuseRequest {
    parent_snapshot: StableAudioMusicSnapshot,
    parent_snapshot_digest: Sha256Digest,
    child_session_id: String,
    child_job_id: String,
    patch: serde_json::Value,
    expected_child_snapshot_digest: Sha256Digest,
    implementation_sources_digest: Sha256Digest,
    parent_asset_id: String,
    parent_asset_content_sha256: Sha256Digest,
}

fn prepare_project_music(
    run: &Path,
    entry: &InstalledTaskDescription,
) -> Result<(NativeTaskPlan, TaskPublicationContext, Sha256Digest)> {
    let file = std::fs::File::open(run.join("project-reuse-owner.json"))?;
    ensure!(
        file.metadata()?.len() <= 1024 * 1024,
        "Project music owner request exceeds its byte limit"
    );
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Project music owner request exceeds its byte limit"
    );
    let request: ProjectMusicReuseRequest = serde_json::from_slice(&bytes)?;
    ensure!(
        request.patch == json!({"seed":1})
            && request.parent_snapshot.parameters.prompt == PROMPT
            && request.parent_snapshot.parameters.seed == 42
            && request.parent_snapshot.metadata.identity == entry.identity
            && request.parent_asset_id == "0024354d-3a4a-83a4-9900-0ab7c1f96d1c"
            && request.parent_asset_content_sha256.as_str()
                == "f69a343dfa9a14a69b2063d5100bb566d52cd6acfbf78741cc582f14c3d1c9ec",
        "Project music request differs from the retained-parent seed-change diagnostic"
    );
    let NativeTaskParameterContract::StableAudioMusic(descriptor) = &entry.descriptor else {
        anyhow::bail!("Installed project music contract is not music")
    };
    let scope = ProjectTaskCompilationScope::new(
        request.child_session_id.clone(),
        request.child_job_id.clone(),
        request.parent_snapshot.metadata.session_id.clone(),
        request.parent_snapshot.metadata.job_id.clone(),
        request.parent_snapshot_digest.clone(),
        vec![],
    )?;
    let plan: NativeTaskPlan = StableAudioMusicParameterService::new(descriptor.binding.clone())?
        .reuse_configuration_with_project_scope(
            &request.child_session_id,
            &request.parent_snapshot,
            &request.parent_snapshot_digest,
            &request.child_job_id,
            serde_json::from_value(request.patch)?,
            &scope,
        )?
        .into();
    ensure!(
        plan.snapshot_digest()? == request.expected_child_snapshot_digest
            && plan.input_references().is_empty(),
        "Reconstructed project music task differs from the authenticated host task"
    );
    Ok((
        plan,
        TaskPublicationContext::ProjectParameterReuseV1 {
            implementation_sources_digest: request.implementation_sources_digest,
        },
        Sha256Digest::of_bytes(&bytes),
    ))
}

fn write_report(path: &Path, value: serde_json::Value) -> Result<()> {
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(&value)?)?;
    file.sync_all()?;
    Ok(())
}

fn native_sources() -> std::collections::BTreeMap<&'static str, Sha256Digest> {
    macro_rules! sources {
        ($($path:literal),+ $(,)?) => { std::collections::BTreeMap::from([$((
            concat!("crates/lumina-local-inference/", $path),
            Sha256Digest::of_bytes(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $path)))
        )),+]) };
    }
    sources!(
        "src/lifecycle/files.rs",
        "src/lifecycle/task_resources/music_probe.rs",
        "src/lifecycle/audio_task_parameters/music_task_parameters.rs",
        "src/lifecycle/audio_task_parameters/reuse.rs",
        "src/lifecycle/project_task_scope.rs",
        "src/lifecycle/audio_task_parameters/stable_audio.json",
        "src/lifecycle/audio_task_parameters/models.rs",
        "src/lifecycle/native_task_runtime.rs",
        "src/lifecycle/native_task_runtime/plan.rs",
        "src/lifecycle/native_task_runtime/process.rs",
        "src/lifecycle/native_task_runtime/output.rs",
        "src/lifecycle/task_parameters.rs",
        "src/lifecycle/audio_task_parameters/profiles.rs",
        "src/lifecycle/audio_task_parameters/services.rs",
        "src/lifecycle/audio_task_parameters.rs",
        "src/lifecycle/audio_task_parameters/install.rs",
    )
}

#[tokio::test]
#[ignore = "Measures the finite owner of an installed music task in a monitored Windows Job"]
async fn measure_installed_music_owner() -> Result<()> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let run = PathBuf::from(
        std::env::var_os("LUMINA_TASK_RESOURCE_RUN_DIR").context("Missing run directory")?,
    )
    .canonicalize()?;
    ensure!(
        run.parent() == Some(base.as_path()),
        "Unexpected measurement directory"
    );
    let detached = run.join("output-root/native").canonicalize()?;
    let store = InstallationStore::open(
        &base.join("m4-managed-audio-install-20261113-r1/model-lifecycle"),
    )?;
    let repo = LifecycleRepository::open(&store.root().join("models.db"))?;
    let catalog = NativeTaskCatalog::active(&repo)?;
    let entry = catalog
        .list(NativeTaskIntent::TextToMusic)
        .into_iter()
        .find(|entry| {
            entry.identity.installation_id == "acceptance-stable-audio-small-music-cpu"
                && entry.identity.version_id == "acceptance-stable-audio-small-music-cpu-v2"
        })
        .context("Pinned music version is not active")?;
    let (plan, publication, project_reuse_request_sha256) = if let Some(mode) =
        std::env::var_os("LUMINA_PROJECT_MUSIC_REUSE")
    {
        ensure!(
            mode == "measurement" && std::env::var_os("LUMINA_MUSIC_DOMAIN_CASE_FILE").is_none(),
            "Project music reuse requires its isolated measurement mode"
        );
        let (plan, publication, digest) = prepare_project_music(&run, &entry)?;
        (plan, publication, Some(digest))
    } else {
        let request: StableAudioMusicRequest =
            serde_json::from_reader(std::fs::File::open(run.join("request.json"))?)?;
        (
            catalog.prepare(&entry.identity, serde_json::to_value(request)?)?,
            TaskPublicationContext::Session,
            None,
        )
    };
    let NativeTaskPlan::StableAudioMusic(task) = &plan else {
        anyhow::bail!("Expected music generation")
    };
    let snapshot = task.snapshot();
    let p = &snapshot.parameters;
    let domain_case = std::env::var_os("LUMINA_MUSIC_DOMAIN_CASE_FILE")
        .map(|path| -> Result<_> {
            let path = PathBuf::from(path).canonicalize()?;
            ensure!(
                path == run.join("domain-case.json"),
                "Unexpected music case path"
            );
            let bytes = std::fs::read(path)?;
            ensure!(bytes.len() <= 65536, "Music case exceeds byte limit");
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            ensure!(
                value["parameters"] == serde_json::to_value(p)? && p.prompt.len() <= 1024,
                "Music case differs from compiled parameters or candidate byte bound"
            );
            Ok(Sha256Digest::of_bytes(&bytes))
        })
        .transpose()?;
    ensure!(
        (domain_case.is_some()
            || (p.prompt == PROMPT
                && p.seed
                    == if project_reuse_request_sha256.is_some() {
                        1
                    } else {
                        42
                    }))
            && p.negative_prompt.is_none()
            && p.duration_seconds == 4.
            && p.steps == 8
            && p.guidance_scale == 1.
            && p.apg_scale == 1.
            && p.sampler == StableAudioSampler::Pingpong
            && p.duration_padding_seconds == 6.
            && p.truncate_to_duration
            && p.chunked_decode
            && snapshot.metadata.binding.runtime
                == AudioTaskProfile::StableAudioSmallMusicCpu.runtime_build(),
        "Music request differs from reviewed diagnostic fixture"
    );
    let engine = PathBuf::from(
        std::env::var_os("LUMINA_AUDIO_MUSIC_CPU_CLI")
            .context("Pinned music engine is required")?,
    );
    let mut engines = NativeTaskEngineRegistry::default();
    engines.register(plan.runtime(), &engine)?;
    let _runtime = crate::InferenceRuntime::get_or_init()?;
    let now = chrono::Utc::now().timestamp();
    let hardware =
        crate::lifecycle::resources::probe_initialized_hardware_resources(&detached, now)?;
    let key = TaskResourceKey::from_plan_with_publication(&plan, &publication)?;
    // The test-only limits bound this first measurement. Production still requires a reviewed record.
    let bound = TaskResourceProfile {
        key: key.clone(),
        host: hardware
            .devices
            .iter()
            .find(|d| d.kind == "cpu")
            .context("CPU missing")?
            .into(),
        budget: TaskResourceBudget {
            host_ram_bytes: 16 << 30,
            gpu_memory: vec![],
            temporary_output_bytes: 128 << 20,
            publication_copy_bytes: 32 << 20,
            publication_scratch_bytes: 32 << 20,
            output_byte_limit: 32 << 20,
            timeout_seconds: 900,
        },
        evidence_sha256: Sha256Digest::of_bytes(b"test-only music measurement receipt"),
    };
    let TaskResourceAssessment::Admitted(receipt) =
        assess_with_profiles_and_publication(&plan, &hardware, now, &[bound], &publication)?
    else {
        anyhow::bail!("Actual capacity is below the provisional music measurement bound")
    };
    let digest = plan.snapshot_digest()?;
    write_report(
        &run.join("fixture.json"),
        json!({"key":key,"hardware":hardware,"snapshot":snapshot,"snapshotDigest":digest,"domainCaseSha256":domain_case,
            "projectReuseRequestSha256":project_reuse_request_sha256,"publicationContext":publication}),
    )?;
    let native =
        NativeTaskRuntime::execute(&repo, &store, &engines, plan, vec![], receipt, &detached).await;
    if let Err(error) = &native {
        if let Some(failure) = error.downcast_ref::<NativeTaskFailure>() {
            write_report(
                &run.join("native-failure.json"),
                json!({"reason":failure.reason,"exitCode":failure.exit_code,
                "stdout":String::from_utf8_lossy(&failure.stdout),"stderr":String::from_utf8_lossy(&failure.stderr)}),
            )?;
        }
    }
    let output = native?;
    let [file] = output.files() else {
        anyhow::bail!("Expected one WAV")
    };
    ensure!(
        file.name() == "output.wav" && output.snapshot_digest() == &digest,
        "Unexpected native output"
    );
    let mut held = std::fs::File::options()
        .create_new(true)
        .write(true)
        .open(detached.join("output.wav"))?;
    ensure!(
        std::io::copy(&mut file.try_clone_reader()?, &mut held)? == file.size_bytes(),
        "Incomplete WAV handoff"
    );
    held.sync_all()?;
    write_report(
        &run.join("owner.json"),
        json!({"snapshotDigest":digest,"outputSha256":file.sha256(),"outputBytes":file.size_bytes(),"sources":native_sources(),"domainCaseSha256":domain_case,
        "projectReuseRequestSha256":project_reuse_request_sha256,"publicationContext":publication,
        "stdoutSha256":Sha256Digest::of_bytes(&output.stdout),"stderrSha256":Sha256Digest::of_bytes(&output.stderr)}),
    )?;
    Ok(())
}
