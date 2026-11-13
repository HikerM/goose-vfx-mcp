#![cfg(windows)]

use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
#[ignore = "Reads real pinned ModelScope GGUF metadata for a configured external model"]
async fn plan_configured_external_generation_profile_from_real_repository() -> Result<()> {
    let name = std::env::var("LUMINA_GENERATION_ACCEPTANCE_PROFILE")?;
    let profile = GenerationProfile::installation_profile(&name)
        .context("Missing external generation profile")?;
    ensure!(
        profile
            .parameter_schema_id()
            .starts_with("generation-profile-"),
        "Expected an external profile"
    );
    let directory = tempfile::tempdir()?;
    let application = InstallationApplication::new(directory.path(), Arc::new(|| Ok(None)));
    let plan = application.prepare_generation_install_plan(profile).await;
    application.shutdown().await?;
    let plan = plan?;
    ensure!(
        plan.runtime == profile.runtime_build()
            && plan.artifacts.len() == profile.recorded_assets().len(),
        "Prepared plan does not bind all external model components"
    );
    println!(
        "{}",
        serde_json::json!({"installationName":name, "identity": profile.parameter_schema_id(),
        "capabilities":plan.target_capabilities, "artifacts":plan.artifacts, "runtime":plan.runtime, "hardware":plan.hardware})
    );
    Ok(())
}

fn cached_artifacts(plan: &InstallPlan, cache: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut files = BTreeMap::new();
    let mut distinct = BTreeSet::new();
    for artifact in plan.artifacts.values() {
        let snapshot = plan
            .snapshots
            .get(&artifact.snapshot_id)
            .context("Missing pinned snapshot")?;
        let filename = Path::new(artifact.path.as_str())
            .file_name()
            .context("Missing GGUF name")?;
        let path = cache
            .join(snapshot.repository.as_str().replace('/', "__"))
            .join(filename)
            .canonicalize()?;
        ensure!(
            path.starts_with(cache) && distinct.insert(path.clone()),
            "Cached artifact path is ambiguous"
        );
        ensure!(
            artifact.size_bytes == Some(path.metadata()?.len()),
            "Cached GGUF size differs from inventory"
        );
        files.insert(artifact.id.clone(), path);
    }
    Ok(files)
}

#[tokio::test]
#[ignore = "Uses actual pinned ModelScope inventories and existing validation GGUF files on D:"]
async fn install_from_observed_generation_assets_and_reopen_task_catalog() -> Result<()> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let name = std::env::var("LUMINA_GENERATION_ACCEPTANCE_PROFILE")?;
    let profile = GenerationProfile::installation_profile(&name)
        .with_context(|| format!("Unknown generation acceptance profile: {name}"))?;
    // Preserve the IDs of existing acceptance stores so they can be reopened.
    let label = match name.as_str() {
        "sd15-cpu" => "sd15",
        "minimax-h3-cuda" => "h3",
        other => other,
    };
    let installation_id = format!("acceptance-{label}");
    let data = PathBuf::from(std::env::var("LUMINA_GENERATION_ACCEPTANCE_DIR")?).canonicalize()?;
    ensure!(
        data.parent() == Some(base.as_path()),
        "Expected a dedicated generation validation directory"
    );
    let job_id = format!("job:{installation_id}");
    let version_id = format!("version:{installation_id}");
    let verify_recovery = std::env::var_os("LUMINA_GENERATION_VERIFY_TRANSFER_RESTART").is_some();
    let mut application = InstallationApplication::new(&data, Arc::new(|| Ok(None)));
    let execution = async {
        let store = InstallationStore::open(application.root())?;
        let repository = LifecycleRepository::open(&store.root().join("models.db"))?;
        if verify_recovery {
            ensure!(
                repository.read_job(&job_id)?.is_none(),
                "Transfer recovery verification requires a fresh installation"
            );
        }
        if repository.read_job(&job_id)?.is_none() {
            let plan = application.prepare_generation_install_plan(profile).await?;
            if verify_recovery {
                ensure!(
                    plan.rules_version == "modelscope-diffusion-install-only-v2",
                    "Expected the v2 transfer promotion plan"
                );
            }
            let files = cached_artifacts(&plan, &base.join("assets").canonicalize()?)?;
            let acceptance = JobAcceptance {
                id: job_id.clone(),
                idempotency_key: installation_id.clone(),
                plan_id: plan.id.clone(),
                plan_digest: plan.digest()?,
            };
            let input = InstallationInput::new(
                &store,
                installation_id.clone(),
                version_id.clone(),
                InstallationSource::LocalCache { files },
            )?;
            repository.enqueue_installation(
                &acceptance,
                &input,
                chrono::Utc::now().timestamp(),
                fs2::available_space(store.root())?,
            )?;
        }
        drop(repository);
        if verify_recovery {
            let service = application.service().await?;
            service.wait_ready().await?;
            tokio::time::timeout(Duration::from_secs(600), async {
                loop {
                    let job = service
                        .repository()
                        .read_job(&job_id)?
                        .context("Missing recovery job")?;
                    ensure!(
                        matches!(job.status, JobStatus::Queued | JobStatus::Running),
                        "No partial download before job reached {:?}",
                        job.status
                    );
                    if service
                        .repository()
                        .file_transfers(&job_id)?
                        .iter()
                        .any(|f| {
                            f.checkpoint.completed_bytes > 0
                                && f.checkpoint.completed_bytes < f.expected_bytes
                        })
                    {
                        break Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            })
            .await??;
            application.shutdown().await?;
            let stopped = service
                .repository()
                .read_job(&job_id)?
                .context("Missing stopped job")?;
            let checkpoint = service
                .repository()
                .file_transfers(&job_id)?
                .into_iter()
                .find(|f| {
                    f.checkpoint.completed_bytes > 0
                        && f.checkpoint.completed_bytes < f.expected_bytes
                })
                .context("Service stopped without preserving its partial checkpoint")?;
            ensure!(
                stopped.lease_owner.is_none(),
                "Worker lease retained after service shutdown"
            );
            println!(
                "{}",
                serde_json::json!({"stoppedJob": stopped, "partialTransfer": checkpoint})
            );
            drop(service);
            application = InstallationApplication::new(&data, Arc::new(|| Ok(None)));
            let recovered = application.service().await?;
            recovered.wait_ready().await?;
            let progress = recovered
                .repository()
                .file_transfers(&job_id)?
                .into_iter()
                .find(|f| f.artifact_id == checkpoint.artifact_id)
                .context("Reopened service lost the transfer")?;
            ensure!(
                progress.checkpoint.completed_bytes >= checkpoint.checkpoint.completed_bytes,
                "Reopened service discarded the persisted progress"
            );
        }
        let service = application.service().await?;
        service.wait_ready().await?;
        tokio::time::timeout(Duration::from_secs(3600), async {
            loop {
                let job = service
                    .repository()
                    .read_job(&job_id)?
                    .context("Missing generation installation job")?;
                match job.status {
                    JobStatus::Completed => return Ok::<_, anyhow::Error>(()),
                    JobStatus::Queued | JobStatus::Running => {
                        tokio::time::sleep(Duration::from_secs(1)).await
                    }
                    status => {
                        let failure = service.repository().latest_installation_failure(&job_id)?;
                        anyhow::bail!(
                            "Generation installation {job_id} ended with {status:?}: {failure:?}"
                        )
                    }
                }
            }
        })
        .await??;
        let version = service
            .repository()
            .installed_version(&version_id)?
            .context("Missing installed generation model")?;
        let expected: BTreeSet<_> = profile.recorded_assets().into_values().collect();
        let installed: BTreeSet<_> = version
            .manifest
            .installed_files
            .values()
            .map(|file| (file.computed_sha256.clone(), file.size_bytes))
            .collect();
        ensure!(
            installed == expected && expected.len() == version.manifest.installed_files.len(),
            "Installed content differs from the recorded model components"
        );
        verify_installation_directory(store.root(), &version.manifest, true)?;
        let catalog = NativeTaskCatalog::active(service.repository())?;
        let mut descriptions = Vec::new();
        for variant in profile.task_variants() {
            let desc = catalog
                .list(variant.intent())
                .into_iter()
                .find(|entry| {
                    entry.identity.installation_id == installation_id
                        && entry.identity.version_id == version_id
                        && entry.identity.task_variant == variant.task_variant()
                        && entry.identity.parameter_schema_id == variant.parameter_schema_id()
                })
                .context("Installed generation task is missing from active catalog")?;
            ensure!(
                desc.identity.manifest_digest == version.manifest_digest.as_str()
                    && catalog.describe(&desc.identity)? == desc,
                "Generation task does not describe its exact installed version"
            );
            descriptions.push(desc);
        }
        Ok::<_, anyhow::Error>(descriptions)
    }
    .await;
    application.shutdown().await?;
    let descriptions = execution?;
    drop(application);

    let reopened = InstallationApplication::new(&data, Arc::new(|| Ok(None)));
    let result = async {
        let service = reopened.service().await?;
        service.wait_ready().await?;
        let catalog = NativeTaskCatalog::active(service.repository())?;
        for desc in descriptions {
            ensure!(
                catalog.list(desc.intent).contains(&desc)
                    && catalog.describe(&desc.identity)? == desc,
                "Restart lost the installed generation task descriptor"
            );
            println!("{}", serde_json::to_string(&desc.identity)?);
        }
        Ok(())
    }
    .await;
    reopened.shutdown().await?;
    result
}
