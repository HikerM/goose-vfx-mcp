use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::{
    managed_chat_status, run_managed_chat_validation, CapabilityStatus, InstallationStore,
    LifecycleRepository, ModelCapability, ModelStructure, Sha256Digest,
};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

fn path(name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name).with_context(|| format!("Set {name}"))?);
    ensure!(path.is_absolute(), "{name} must be absolute");
    Ok(path)
}

async fn install_recorded_dense_model(
    root: &std::path::Path,
    version: &str,
    weight: PathBuf,
) -> Result<()> {
    use lumina_local_inference::lifecycle::{
        InstallationApplication, InstallationInput, InstallationSource, JobAcceptance, JobStatus,
        ModelScopeRepository, RepositoryPath,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let data = root
        .parent()
        .context("Missing acceptance data root")?
        .canonicalize()?;
    ensure!(
        data.parent() == Some(base.as_path())
            && root.file_name() == Some(std::ffi::OsStr::new("model-lifecycle")),
        "Refusing to modify a store outside acceptance data"
    );
    let app = InstallationApplication::new(&data, Arc::new(|| Ok(None)));
    let result = async {
        let store = InstallationStore::open(app.root())?;
        let repo = LifecycleRepository::open(&store.root().join("models.db"))?;
        if let Some(existing) = repo.installed_version(version)? {
            ensure!(
                existing.manifest.installation_id == "acceptance-text-qwen35-4b",
                "Wrong existing installation"
            );
            return Ok(());
        }
        let job_id = "job:acceptance-text-qwen35-4b";
        if repo.read_job(job_id)?.is_none() {
            let assessment = app
                .prepare_text_plan(
                    ModelScopeRepository::parse("AtomicChat/Qwen3.5-4B-GGUF")?,
                    "3ba229851bf643577ee703364116751f3ac120b8",
                    RepositoryPath::parse("qwen35-4b-Q4_K_M.gguf")?,
                )
                .await?;
            ensure!(
                assessment.blocking_reasons.is_empty(),
                "Text planning blocked: {:?}",
                assessment.blocking_reasons
            );
            let plan = assessment.plan.context("Missing measured text plan")?;
            let id = plan
                .artifacts
                .keys()
                .next()
                .context("Missing weight")?
                .clone();
            let input = InstallationInput::new(
                &store,
                "acceptance-text-qwen35-4b".into(),
                version.into(),
                InstallationSource::LocalCache {
                    files: BTreeMap::from([(id, weight)]),
                },
            )?;
            repo.enqueue_installation(
                &JobAcceptance {
                    id: job_id.into(),
                    idempotency_key: job_id.into(),
                    plan_id: plan.id.clone(),
                    plan_digest: plan.digest()?,
                },
                &input,
                chrono::Utc::now().timestamp(),
                fs2::available_space(store.root())?,
            )?;
        }
        let service = app.service().await?;
        service.wait_ready().await?;
        tokio::time::timeout(Duration::from_secs(3600), async {
            loop {
                let state = repo
                    .read_job(job_id)?
                    .context("Missing text install job")?
                    .status;
                match state {
                    JobStatus::Completed => return Ok::<_, anyhow::Error>(()),
                    JobStatus::Queued | JobStatus::Running => {
                        tokio::time::sleep(Duration::from_secs(1)).await
                    }
                    _ => anyhow::bail!(
                        "Text installation ended with {state:?}: {:?}",
                        repo.latest_installation_failure(job_id)?
                    ),
                }
            }
        })
        .await??;
        Ok(())
    }
    .await;
    app.shutdown().await?;
    result
}

#[tokio::test]
#[ignore = "Requires an actual registered Dense or MoE installation and its authenticated native helper"]
async fn registered_model_runs_the_frozen_a41_suite_and_reopens_its_evidence() -> Result<()> {
    let root = path("LUMINA_CHAT_VALIDATION_STORE")?;
    let engine = path("LUMINA_CHAT_VALIDATION_ENGINE")?;
    let output = path("LUMINA_CHAT_VALIDATION_REPORT")?;
    let version = std::env::var("LUMINA_CHAT_VALIDATION_VERSION")?;
    let structure = match std::env::var("LUMINA_CHAT_VALIDATION_STRUCTURE")?.as_str() {
        "dense" => ModelStructure::Dense,
        "moe" => ModelStructure::Moe,
        _ => anyhow::bail!("Set LUMINA_CHAT_VALIDATION_STRUCTURE to dense or moe"),
    };
    ensure!(!output.exists(), "A41 acceptance output already exists");
    if let Some(weight) = std::env::var_os("LUMINA_CHAT_VALIDATION_INSTALL_CACHE") {
        install_recorded_dense_model(&root, &version, PathBuf::from(weight).canonicalize()?)
            .await?;
    }
    let store = InstallationStore::open(&root)?;
    let database = root.join("models.db");
    ensure!(
        database.is_file(),
        "No registered installation database exists"
    );
    let repository = LifecycleRepository::open(&database)?;
    let validation = run_managed_chat_validation(
        &repository,
        &store,
        &version,
        &engine,
        Duration::from_secs(180),
    )
    .await?;
    drop(repository);
    let reopened = LifecycleRepository::open(&database)?;
    let status = managed_chat_status(&reopened, &store, &version, &engine)?;
    let observed = serde_json::json!({
        "scope":"Actual registered CPU text capability; frozen single-turn content, multi-turn history and one-token limit.",
        "store":root, "engine":engine, "version_id":version,
        "validation":validation,"reopened_status":status
    });
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?;
    serde_json::to_writer_pretty(&mut file, &observed)?;
    file.flush()?;
    file.sync_all()?;
    ensure!(
        validation.structure == structure,
        "Wrong installed model structure"
    );
    ensure!(
        status.capability == ModelCapability::Text
            && status.status == CapabilityStatus::Ready
            && status.evidence.as_ref() == Some(&validation.evidence)
            && validation.evidence.status == CapabilityStatus::Ready
            && validation.completed == 10
            && validation.correct >= 8
            && validation.minimum == 8
            && validation.dialogs_completed == 3
            && validation.dialogs_passed == 3
            && validation.output_limit_passed
            && validation.failure.is_none(),
        "Real A41 capability validation failed; inspect the saved output"
    );
    let bytes = std::fs::read(root.join(&validation.evidence.evidence_path))?;
    ensure!(
        validation.evidence.output_digest == Some(Sha256Digest::of_bytes(&bytes)),
        "Persisted chat transcript differs from the evidence registered in SQLite"
    );
    let report: serde_json::Value = serde_json::from_slice(&bytes)?;
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/chat-a41-v1.json"))?;
    for (observed_name, expected_name) in [("single", "cases"), ("dialogs", "dialogs")] {
        let observed = report["cases"][observed_name]
            .as_array()
            .context("Missing model transcripts")?;
        let cases = expected[expected_name]
            .as_array()
            .context("Missing fixture cases")?;
        ensure!(
            observed.len() == cases.len(),
            "Incomplete chat transcript set"
        );
        for (transcript, case) in observed.iter().zip(cases) {
            ensure!(
                transcript["id"] == case["id"],
                "Transcript belongs to a different case"
            );
            let turns = transcript["turns"]
                .as_array()
                .context("Missing chat turns")?;
            let expected_turn_count = case["turns"].as_array().map_or(1, Vec::len);
            ensure!(turns.len() == expected_turn_count, "Incomplete real dialog");
            for turn in turns {
                ensure!(
                    turn["response"]["launch_digest"] == report["actual_launch_digest"]
                        && turn["response"]["usage"]["completion_tokens"]
                            .as_u64()
                            .is_some_and(|n| n > 0),
                    "Response lacks the actual launch identity or generated tokens"
                );
            }
        }
    }
    ensure!(
        report["cases"]["limit"]["response"]["usage"]["completion_tokens"] == 1,
        "The saved limit probe did not emit exactly one token"
    );
    Ok(())
}
