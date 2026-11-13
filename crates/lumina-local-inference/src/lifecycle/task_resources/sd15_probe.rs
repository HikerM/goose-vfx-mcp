use super::*;
use anyhow::Context;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

#[path = "sd15_cases.rs"]
mod cases;

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
        "src/lifecycle/task_resources/sd15_probe.rs",
        "src/lifecycle/task_resources/sd15_cases.rs",
        "src/lifecycle/task_resources/sd15-cases.json",
        "src/lifecycle/task_resources/sd_conditions.rs",
        "src/lifecycle/native_task_runtime.rs",
        "src/lifecycle/native_task_runtime/plan.rs",
        "src/lifecycle/native_task_runtime/process.rs",
        "src/lifecycle/native_task_runtime/output.rs",
        "src/lifecycle/task_parameters.rs",
        "src/lifecycle/task_parameters/compile.rs",
        "src/lifecycle/task_parameters/models.json",
        "src/lifecycle/task_parameters/protocols.json",
        "src/lifecycle/task_parameters/inventory.rs",
    )
}

#[tokio::test]
#[ignore = "Measures the finite owner of an installed SD1.5 task in a monitored Windows Job"]
async fn measure_installed_sd15_owner() -> Result<()> {
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
        &base.join("m4-managed-generation-20261113-v2-r1/model-lifecycle"),
    )?;
    let repo = LifecycleRepository::open(&store.root().join("models.db"))?;
    let catalog = NativeTaskCatalog::active(&repo)?;
    let entry = catalog
        .list(NativeTaskIntent::TextToImage)
        .into_iter()
        .find(|entry| {
            entry.identity.installation_id == "acceptance-sd15"
                && entry.identity.version_id == "version:acceptance-sd15"
        })
        .context("Pinned SD1.5 version is not active")?;
    let request: GenerationTaskRequest =
        serde_json::from_reader(std::fs::File::open(run.join("request.json"))?)?;
    let plan = catalog.prepare(&entry.identity, serde_json::to_value(request)?)?;
    let NativeTaskPlan::Generation(task) = &plan else {
        anyhow::bail!("Expected image generation")
    };
    let snapshot = task.snapshot();
    let EffectiveGenerationParameters::Image(p) = &snapshot.parameters else {
        anyhow::bail!("Expected an image workload")
    };
    let (_, case) = cases::selected()?;
    ensure!(
        snapshot.profile == GenerationProfile::Sd15CpuTextToImage
            && p.width == 512
            && p.height == 512
            && p.batch_count == 1
            && p.steps == 20
            && p.seed == case.seed
            && p.cfg_scale == 7.0
            && p.sampler == GenerationSampler::EulerA
            && p.scheduler == GenerationScheduler::Discrete
            && p.prompt == case.prompt
            && p.negative_prompt == case.negative_prompt,
        "Input does not match the preregistered CPU image workload"
    );
    let engine = PathBuf::from(
        std::env::var_os("LUMINA_DIFFUSION_CPU_CLI").context("Pinned image engine is required")?,
    );
    let mut engines = NativeTaskEngineRegistry::default();
    engines.register(plan.runtime(), &engine)?;
    let _runtime = crate::InferenceRuntime::get_or_init()?;
    let now = chrono::Utc::now().timestamp();
    let hardware =
        crate::lifecycle::resources::probe_initialized_hardware_resources(&detached, now)?;
    let key = TaskResourceKey::from_plan(&plan)?;
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
            temporary_output_bytes: 32 << 20,
            publication_copy_bytes: 32 << 20,
            publication_scratch_bytes: 32 << 20,
            output_byte_limit: 32 << 20,
            timeout_seconds: 900,
        },
        evidence_sha256: Sha256Digest::of_bytes(b"test-only SD1.5 measurement receipt"),
    };
    let TaskResourceAssessment::Admitted(receipt) =
        assess_with_profiles(&plan, &hardware, now, &[bound])?
    else {
        anyhow::bail!("Actual capacity is below the provisional SD1.5 measurement bound")
    };
    let digest = plan.snapshot_digest()?;
    write_report(
        &run.join("fixture.json"),
        json!({"key":key,"hardware":hardware,"snapshot":snapshot,"snapshotDigest":digest}),
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
        anyhow::bail!("Expected one PNG")
    };
    ensure!(
        file.name() == "output.png" && output.snapshot_digest() == &digest,
        "Unexpected native output"
    );
    let mut held = std::fs::File::options()
        .create_new(true)
        .write(true)
        .open(detached.join("output.png"))?;
    ensure!(
        std::io::copy(&mut file.try_clone_reader()?, &mut held)? == file.size_bytes(),
        "Incomplete PNG handoff"
    );
    held.sync_all()?;
    write_report(
        &run.join("owner.json"),
        json!({"snapshotDigest":digest,"outputSha256":file.sha256(),"outputBytes":file.size_bytes(),"sources":native_sources(),
        "stdoutSha256":Sha256Digest::of_bytes(&output.stdout),"stderrSha256":Sha256Digest::of_bytes(&output.stderr)}),
    )?;
    Ok(())
}
