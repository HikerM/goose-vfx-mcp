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
    payload_tier: ProjectReusePayloadTier,
    parent_asset_id: String,
    parent_asset_content_sha256: Sha256Digest,
    #[serde(default)]
    case_sha256: Option<Sha256Digest>,
}

fn validate_domain_patch(patch: &serde_json::Value) -> Result<()> {
    let patch = patch
        .as_object()
        .context("Project music patch must be an object")?;
    ensure!(
        patch.len() == 1,
        "Project music changes exactly one parameter"
    );
    match patch.iter().next().context("Missing project music patch")? {
        (field, value) if field == "prompt" => {
            let prompt = value
                .as_str()
                .context("Project music prompt must be text")?;
            ensure!(
                !(prompt.trim().is_empty()
                    || prompt.len() > 16_384
                    || prompt.contains(['\0', '|'])
                    || (prompt.starts_with("--") && !prompt.chars().any(char::is_whitespace))),
                "Project music prompt is outside the native single-prompt contract"
            );
        }
        (field, value) if field == "seed" => {
            ensure!(
                value
                    .as_u64()
                    .is_some_and(|seed| u32::try_from(seed).is_ok()),
                "Project music seed must be a u32"
            );
        }
        _ => anyhow::bail!("Project music domain changes only prompt or seed"),
    }
    Ok(())
}

fn validate_project_case(bytes: &[u8], request: &ProjectMusicReuseRequest) -> Result<Sha256Digest> {
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Project music case exceeds its byte limit"
    );
    let digest = Sha256Digest::of_bytes(bytes);
    ensure!(
        request.case_sha256.as_ref() == Some(&digest),
        "Project music case differs from its owner request digest"
    );
    let case: serde_json::Value = serde_json::from_slice(bytes)?;
    ensure!(
        case["schemaVersion"] == 2
            && case["patch"] == request.patch
            && case["parentAssetId"] == request.parent_asset_id
            && case["parentAssetContentSha256"] == request.parent_asset_content_sha256.as_str(),
        "Project music case differs from the owner parent or patch"
    );
    validate_domain_patch(&request.patch)?;
    Ok(digest)
}

fn prepare_project_music(
    run: &Path,
    entry: &InstalledTaskDescription,
    domain_measurement: bool,
) -> Result<(
    NativeTaskPlan,
    TaskPublicationContext,
    Sha256Digest,
    Option<Sha256Digest>,
)> {
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
        request.parent_snapshot.metadata.identity == entry.identity,
        "Project music parent differs from the installed task identity"
    );
    let case_sha256 = if domain_measurement {
        let file = std::fs::File::open(run.join("project-reuse-case.json"))?;
        ensure!(
            file.metadata()?.len() <= 1024 * 1024,
            "Project music case exceeds its byte limit"
        );
        let mut case_bytes = Vec::new();
        file.take(1024 * 1024 + 1).read_to_end(&mut case_bytes)?;
        Some(validate_project_case(&case_bytes, &request)?)
    } else {
        ensure!(
            request.case_sha256.is_none()
                && request.payload_tier == ProjectReusePayloadTier::Small
                && request.patch == json!({"seed":1})
                && request.parent_snapshot.parameters.prompt == PROMPT
                && request.parent_snapshot.parameters.seed == 42
                && request.parent_snapshot.metadata.identity == entry.identity
                && request.parent_asset_id == "0024354d-3a4a-83a4-9900-0ab7c1f96d1c"
                && request.parent_asset_content_sha256.as_str()
                    == "f69a343dfa9a14a69b2063d5100bb566d52cd6acfbf78741cc582f14c3d1c9ec",
            "Project music request differs from the retained-parent seed-change diagnostic"
        );
        None
    };
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
        TaskPublicationContext::ProjectParameterReuseV2 {
            implementation_sources_digest: request.implementation_sources_digest,
            payload_tier: request.payload_tier,
        },
        Sha256Digest::of_bytes(&bytes),
        case_sha256,
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
        "src/lifecycle/task_resources.rs",
        "src/lifecycle/task_resources/music_conditions.rs",
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

#[test]
fn project_domain_patch_accepts_native_text_and_full_seed_range_only() {
    for patch in [
        json!({"prompt":"钢琴与弦乐"}),
        json!({"prompt":"p".repeat(16_384)}),
        json!({"seed":0}),
        json!({"seed":u32::MAX}),
    ] {
        validate_domain_patch(&patch).unwrap();
    }
    for patch in [
        json!({}),
        json!({"prompt":"piano", "seed":1}),
        json!({"steps":8}),
        json!({"prompt":42}),
        json!({"prompt":" \t\n"}),
        json!({"prompt":"p".repeat(16_385)}),
        json!({"prompt":"猫".repeat(5462)}),
        json!({"prompt":"piano\0"}),
        json!({"prompt":"piano|strings"}),
        json!({"prompt":"--help"}),
        json!({"seed":-1}),
        json!({"seed":u64::from(u32::MAX) + 1}),
        json!({"seed":1.5}),
        json!({"seed":"1"}),
    ] {
        assert!(validate_domain_patch(&patch).is_err(), "{patch}");
    }
}

#[test]
fn project_domain_case_binds_schema_parent_patch_and_exact_file_bytes() {
    let NativeTaskPlan::StableAudioMusic(plan) = publication_plans().pop().unwrap() else {
        panic!("Expected synthetic music fixture")
    };
    let mut request = ProjectMusicReuseRequest {
        parent_snapshot: plan.snapshot().clone(),
        parent_snapshot_digest: plan.snapshot().digest().unwrap(),
        child_session_id: "synthetic-child".into(),
        child_job_id: "synthetic-project-domain".into(),
        patch: json!({"prompt":"Synthetic project melody"}),
        expected_child_snapshot_digest: Sha256Digest::of_bytes(b"synthetic expected snapshot"),
        implementation_sources_digest: Sha256Digest::of_bytes(b"synthetic host source"),
        payload_tier: ProjectReusePayloadTier::Small,
        parent_asset_id: "0024354d-3a4a-83a4-9900-0ab7c1f96d1c".into(),
        parent_asset_content_sha256: Sha256Digest::of_bytes(b"synthetic parent content"),
        case_sha256: None,
    };
    let case = json!({"schemaVersion":2,"patch":request.patch,
        "parentAssetId":request.parent_asset_id,
        "parentAssetContentSha256":request.parent_asset_content_sha256});
    let bytes = serde_json::to_vec(&case).unwrap();
    assert!(validate_project_case(&bytes, &request).is_err());
    request.case_sha256 = Some(Sha256Digest::of_bytes(&bytes));
    assert_eq!(
        validate_project_case(&bytes, &request).unwrap(),
        request.case_sha256.clone().unwrap()
    );
    let mut changed_bytes = bytes.clone();
    changed_bytes.push(b' ');
    assert!(validate_project_case(&changed_bytes, &request).is_err());
    for (field, value) in [
        ("schemaVersion", json!(1)),
        ("patch", json!({"seed":1})),
        ("parentAssetId", json!("another-parent")),
        (
            "parentAssetContentSha256",
            json!(Sha256Digest::of_bytes(b"another parent")),
        ),
    ] {
        let mut changed_case = case.clone();
        changed_case[field] = value;
        let changed_bytes = serde_json::to_vec(&changed_case).unwrap();
        request.case_sha256 = Some(Sha256Digest::of_bytes(&changed_bytes));
        assert!(
            validate_project_case(&changed_bytes, &request).is_err(),
            "{field}"
        );
    }
    let oversized = vec![b' '; 1024 * 1024 + 1];
    request.case_sha256 = Some(Sha256Digest::of_bytes(&oversized));
    assert!(validate_project_case(&oversized, &request)
        .unwrap_err()
        .to_string()
        .contains("exceeds its byte limit"));
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
    let (plan, publication, project_reuse_request_sha256, project_case_sha256) =
        if let Some(mode) = std::env::var_os("LUMINA_PROJECT_MUSIC_REUSE") {
            ensure!(
                (mode == "measurement" || mode == "domain_measurement")
                    && std::env::var_os("LUMINA_MUSIC_DOMAIN_CASE_FILE").is_none(),
                "Project music reuse requires its isolated measurement mode"
            );
            let (plan, publication, digest, case_sha256) =
                prepare_project_music(&run, &entry, mode == "domain_measurement")?;
            (plan, publication, Some(digest), case_sha256)
        } else {
            let request: StableAudioMusicRequest =
                serde_json::from_reader(std::fs::File::open(run.join("request.json"))?)?;
            (
                catalog.prepare(&entry.identity, serde_json::to_value(request)?)?,
                TaskPublicationContext::Session,
                None,
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
            || project_case_sha256.is_some()
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
            && snapshot.metadata.backend == "cpu"
            && snapshot.metadata.cpu_threads == 4
            && snapshot.metadata.requires_new_native_preparation
            && snapshot.metadata.binding.runtime
                == AudioTaskProfile::StableAudioSmallMusicCpu.runtime_build(),
        "Music request differs from reviewed diagnostic fixture"
    );
    if project_case_sha256.is_some() {
        validate_domain_patch(&json!({"prompt": &p.prompt}))?;
    }
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
            "projectReuseRequestSha256":project_reuse_request_sha256,"caseSha256":project_case_sha256,"publicationContext":publication}),
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
        "projectReuseRequestSha256":project_reuse_request_sha256,"caseSha256":project_case_sha256,"publicationContext":publication,
        "stdoutSha256":Sha256Digest::of_bytes(&output.stdout),"stderrSha256":Sha256Digest::of_bytes(&output.stderr)}),
    )?;
    Ok(())
}
