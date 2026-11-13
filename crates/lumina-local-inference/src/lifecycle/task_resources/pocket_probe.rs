use super::*;
use anyhow::Context;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

fn write_new(directory: &Path, name: &str, data: &[u8]) -> Result<()> {
    let mut file = std::fs::File::options()
        .create_new(true)
        .write(true)
        .open(directory.join(name))?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

fn owner_sources() -> std::collections::BTreeMap<&'static str, Sha256Digest> {
    macro_rules! sources {
        ($($path:literal),+ $(,)?) => {
            std::collections::BTreeMap::from([$(
                ($path, Sha256Digest::of_bytes(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $path))))
            ),+])
        };
    }
    sources!(
        "src/lifecycle/task_resources.rs",
        "src/lifecycle/task_resources/conditions.rs",
        "src/lifecycle/task_resources/pocket-tts-conditions.json",
        "src/lifecycle/files.rs",
        "src/lifecycle/task_resources/pocket_probe.rs",
        "src/lifecycle/native_task_runtime.rs",
        "src/lifecycle/native_task_runtime/plan.rs",
        "src/lifecycle/native_task_runtime/process.rs",
        "src/lifecycle/native_task_runtime/output.rs",
        "src/lifecycle/audio_task_parameters.rs",
        "src/lifecycle/audio_task_parameters/models.rs",
        "src/lifecycle/audio_task_parameters/profiles.rs",
        "src/lifecycle/audio_task_parameters/services.rs",
    )
}

#[tokio::test]
#[ignore = "Measures one pinned managed PocketTTS invocation in an independently monitored Windows Job"]
async fn measure_installed_pocket_owner() -> Result<()> {
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
        "Expected an isolated measurement directory"
    );
    let store_root = validation.join("m4-managed-audio-install-20261113-r1/model-lifecycle");
    let repo = LifecycleRepository::open(&store_root.join("models.db"))?;
    let store = InstallationStore::open(&store_root)?;
    let catalog = NativeTaskCatalog::active(&repo)?;
    let entry = catalog
        .list(NativeTaskIntent::SpeechSynthesis)
        .into_iter()
        .find(|v| v.identity.installation_id == "acceptance-pocket-tts")
        .context("Missing managed PocketTTS")?;
    let snapshot: PocketTtsSnapshot =
        serde_json::from_reader(std::fs::File::open(run.join("snapshot.json"))?)?;
    let p = &snapshot.parameters;
    let fixed_seed_probe = p.max_frames == 32
        && p.frames_after_eos == 32
        && ((p.text == "Please read the short note aloud."
            && matches!(p.seed, 0 | 2147483648 | u32::MAX))
            || matches!(
                p.text.as_str(),
                "I." | "Hello there."
                    | "Fresh blue rain falls now."
                    | "QQKWKWKKWQJXWZWJXKWWJXQZQZWZWK."
            ));
    ensure!(
        fixed_seed_probe
            || (p.seed == 42
                && p.max_frames == 0
                && p.frames_after_eos == -1
                && matches!(
            p.text.as_str(),
            "The red apple is on the wooden table."
                | "Please read the short note aloud."
                | "The morning train leaves at nine. We can walk to the station after breakfast."
        )),
        "Text is outside the preselected probe cases"
    );
    let plan = catalog.prepare(
        &entry.identity,
        serde_json::to_value(PocketTtsRequest {
            session_id: snapshot.metadata.session_id.clone(),
            job_id: snapshot.metadata.job_id.clone(),
            text: p.text.clone(),
            voice_reference: p.voice_reference.clone(),
            seed: Some(p.seed),
            temperature: None,
            max_frames: Some(p.max_frames),
            text_chunk_tokens: None,
            frames_after_eos: Some(p.frames_after_eos),
        })?,
    )?;
    ensure!(
        matches!(&plan, NativeTaskPlan::PocketTts(task) if task.snapshot() == &snapshot),
        "Catalog disagrees with the session plan"
    );
    let input = PathBuf::from(
        std::env::var_os("LUMINA_TASK_RESOURCE_VOICE_PATH")
            .context("Missing session voice reference")?,
    )
    .canonicalize()?;
    ensure!(
        input.starts_with(run.join("output-root/publication").canonicalize()?)
            && p.voice_reference.sha256.as_str()
                == "b29307e631d805d7079910c8c9c5ee14777549954b737e15a5770db7ac2da2e9",
        "Voice reference differs from fixed published input"
    );
    let voice = std::fs::File::open(&input)?;
    let input = NativeTaskInput::from_verified_media(
        NativeTaskInputReference::Audio(p.voice_reference.clone()),
        input,
        voice,
    );
    let engine = PathBuf::from(
        std::env::var_os("LUMINA_AUDIO_CPU_CLI").context("Missing pinned audio engine")?,
    );
    let mut engines = NativeTaskEngineRegistry::default();
    engines.register(plan.runtime(), &engine)?;
    let native_root = run.join("output-root/native").canonicalize()?;
    let _runtime = crate::InferenceRuntime::get_or_init()?;
    let now = chrono::Utc::now().timestamp();
    let observed =
        crate::lifecycle::resources::probe_initialized_hardware_resources(&native_root, now)?;
    let key = TaskResourceKey::from_plan(&plan)?;
    let host = observed
        .devices
        .iter()
        .find(|device| device.kind == "cpu")
        .context("Host CPU not enumerated")?
        .into();
    // This provisional envelope exists only in the ignored unit-test binary.
    let probe = TaskResourceProfile {
        key: key.clone(),
        host,
        budget: TaskResourceBudget {
            host_ram_bytes: 8 << 30,
            gpu_memory: vec![],
            temporary_output_bytes: 32 << 20,
            publication_copy_bytes: 32 << 20,
            publication_scratch_bytes: 32 << 20,
            output_byte_limit: 32 << 20,
            timeout_seconds: 240,
        },
        evidence_sha256: Sha256Digest::of_bytes(b"test-only bounded measurement"),
    };
    let TaskResourceAssessment::Admitted(receipt) =
        assess_with_profiles(&plan, &observed, now, &[probe])?
    else {
        anyhow::bail!("Insufficient actual capacity for bounded measurement");
    };
    let digest = plan.snapshot_digest()?;
    write_new(
        &run,
        "fixture.json",
        &serde_json::to_vec_pretty(
            &json!({"key":key, "hardware":observed, "snapshot":snapshot,"snapshotDigest":digest}),
        )?,
    )?;
    let output = NativeTaskRuntime::execute(
        &repo,
        &store,
        &engines,
        plan,
        vec![input],
        receipt,
        &native_root,
    )
    .await?;
    let [file] = output.files() else {
        anyhow::bail!("Expected one speech output")
    };
    ensure!(
        output.snapshot_digest() == &digest && file.name() == "output.wav",
        "Unexpected native result"
    );
    let mut export = std::fs::File::options()
        .create_new(true)
        .write(true)
        .open(native_root.join("output.wav"))?;
    ensure!(
        std::io::copy(&mut file.try_clone_reader()?, &mut export)? == file.size_bytes(),
        "Incomplete inter-process WAV handoff"
    );
    export.sync_all()?;
    write_new(
        &run,
        "owner.json",
        &serde_json::to_vec_pretty(
            &json!({"snapshotDigest":digest,"outputSha256":file.sha256(),"outputBytes":file.size_bytes(),"stdoutSha256":Sha256Digest::of_bytes(&output.stdout),"stderrSha256":Sha256Digest::of_bytes(&output.stderr),"implementationSources":owner_sources()}),
        )?,
    )?;
    Ok(())
}
