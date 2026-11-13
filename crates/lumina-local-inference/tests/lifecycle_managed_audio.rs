#![cfg(windows)]

use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::{
    AudioTaskProfile, AudioTaskProtocol, InstallationApplication, InstallationSubmission,
    JobStatus, NativeTaskCatalog, NativeTaskIntent,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
#[ignore = "Downloads pinned ModelScope weights into the specified persistent validation directory"]
async fn install_and_discover_managed_audio() -> Result<()> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let supplied = PathBuf::from(
        std::env::var_os("LUMINA_AUDIO_ACCEPTANCE_DIR")
            .context("Set LUMINA_AUDIO_ACCEPTANCE_DIR")?,
    );
    ensure!(
        supplied.is_absolute()
            && supplied
                .parent()
                .context("Missing parent")?
                .canonicalize()?
                == base,
        "Expected a dedicated directory below lifecycle-validation"
    );
    let directory = supplied.canonicalize()?;
    ensure!(
        directory.parent() == Some(base.as_path()),
        "Acceptance root resolves outside validation"
    );
    let name = std::env::var("LUMINA_AUDIO_ACCEPTANCE_PROFILE")?;
    let profile = match name.as_str() {
        "pocket-tts" => AudioTaskProfile::PocketTtsEnglishCpu,
        "qwen3-asr" => AudioTaskProfile::Qwen3Asr06bCpu,
        other => AudioTaskProfile::registered()
            .find(|p| p.installation_name() == other)
            .context("Unknown audio acceptance profile")?,
    };
    let intent = match profile.protocol() {
        AudioTaskProtocol::PocketTtsOffline => NativeTaskIntent::SpeechSynthesis,
        AudioTaskProtocol::Qwen3AsrOffline => NativeTaskIntent::Transcription,
    };
    let id = format!("acceptance-{name}");
    let id = id.as_str();
    let application = InstallationApplication::new(&directory, Arc::new(|| Ok(None)));
    let result = async {
        let service = application.service().await?;
        service.wait_ready().await?;
        let job = if let Ok(job_id) = std::env::var("LUMINA_AUDIO_ACCEPTANCE_JOB") {
            let job = service
                .repository()
                .read_job(&job_id)?
                .context("Unknown acceptance job")?;
            let intent = service
                .repository()
                .read_installation_input(&job.id)?
                .context("Missing durable installation input")?;
            ensure!(
                intent.input.installation_id == id,
                "Recovery job belongs to another installation"
            );
            job
        } else {
            let plan = application.prepare_audio_install_plan(profile).await?;
            let digest = plan.digest()?;
            service
                .submit(&InstallationSubmission {
                    idempotency_key: format!("{id}-{}", digest.as_str()),
                    plan_id: plan.id.clone(),
                    plan_digest: digest,
                    installation_id: id.into(),
                })
                .await?
        };
        tokio::time::timeout(Duration::from_secs(3600), async {
            loop {
                let current = service
                    .repository()
                    .read_job(&job.id)?
                    .context("Missing installation job")?;
                match current.status {
                    JobStatus::Completed => return Ok::<(), anyhow::Error>(()),
                    JobStatus::Queued | JobStatus::Running => {
                        tokio::time::sleep(Duration::from_secs(1)).await
                    }
                    status => anyhow::bail!("Installation job {} ended with {status:?}", job.id),
                }
            }
        })
        .await??;
        let repo = service.repository();
        let version_id = repo
            .read_installation_input(&job.id)?
            .context("Missing completed installation input")?
            .input
            .version_id;
        let catalog = NativeTaskCatalog::for_version(repo, id, &version_id)?;
        let descriptor = catalog
            .list(intent)
            .into_iter()
            .find(|entry| {
                entry.identity.installation_id == id && entry.identity.version_id == version_id
            })
            .context("Completed installation missing from its task catalog")?;
        let installed = service
            .repository()
            .installed_version(&descriptor.identity.version_id)?
            .context("Missing installed version")?;
        lumina_local_inference::lifecycle::verify_installation_directory(
            service.store().root(),
            &installed.manifest,
            true,
        )?;
        ensure!(
            descriptor.identity.manifest_digest == installed.manifest_digest.as_str()
                && catalog.describe(&descriptor.identity)? == descriptor,
            "Catalog does not resolve the installed manifest"
        );
        let active = repo
            .active_installations()?
            .into_iter()
            .find(|row| row.installation_id == id)
            .context("Missing active acceptance installation")?;
        if active.active_version_id != version_id {
            repo.activate_installation_version(id, &active.active_version_id, &version_id)?;
        }
        Ok::<_, anyhow::Error>(descriptor)
    }
    .await;
    application.shutdown().await?;
    let descriptor = result?;
    drop(application);
    let reopened = InstallationApplication::new(&directory, Arc::new(|| Ok(None)));
    let recovered = async {
        let service = reopened.service().await?;
        service.wait_ready().await?;
        let catalog = NativeTaskCatalog::active(service.repository())?;
        ensure!(
            catalog.list(intent).contains(&descriptor)
                && catalog.describe(&descriptor.identity)? == descriptor,
            "Restart lost the installed model task descriptor"
        );
        println!("{}", serde_json::to_string(&descriptor.identity)?);
        Ok(())
    }
    .await;
    reopened.shutdown().await?;
    recovered
}

#[test]
#[ignore = "Uses independently recorded ModelScope GGUF headers from two real audio installations"]
fn both_installed_weights_prepare_in_one_store() -> Result<()> {
    use lumina_local_inference::lifecycle::{
        install_from_local_cache, CacheInstallRequest, ComponentObservation, ComponentRole,
        FilePurpose, InstallationInput, InstallationSource, InstallationStore, JobAcceptance,
        LifecycleRepository, SourceGgufObservation,
    };

    let file = PathBuf::from(std::env::var("LUMINA_AUDIO_OBSERVATION_PAIR")?);
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RecordedWeight {
        observation: SourceGgufObservation,
        weight_path: PathBuf,
    }
    let inputs: [RecordedWeight; 2] = serde_json::from_reader(std::fs::File::open(file)?)?;
    let weights = [&inputs[0].observation, &inputs[1].observation];
    ensure!(
        weights[0].snapshot.content_digest()? == weights[1].snapshot.content_digest()?
            && weights[0].artifact.source_sha256 != weights[1].artifact.source_sha256,
        "Expected different weights from the same pinned ModelScope inventory"
    );
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/lifecycle-validation");
    for (first, second) in [(0, 1), (1, 0)] {
        let root = tempfile::tempdir_in(&base)?;
        let store = InstallationStore::open(&root.path().canonicalize()?)?;
        let db = store.root().join("models.db");
        let repo = LifecycleRepository::open(&db)?;
        let mut plans = Vec::new();
        for index in [first, second] {
            let selection = ComponentObservation {
                role: ComponentRole::Main,
                files: vec![weights[index].clone()],
            };
            let free = fs2::available_space(store.root())?;
            let now = || chrono::Utc::now().timestamp();
            let plan = repo.prepare_audio_install_plan(&selection, free, now())?;
            let request = CacheInstallRequest {
                job_id: format!("job-{index}"),
                installation_id: format!("speech-{index}"),
                version_id: format!("version-{index}"),
                source_files: std::collections::BTreeMap::from([(
                    weights[index].artifact.id.clone(),
                    inputs[index].weight_path.canonicalize()?,
                )]),
            };
            let input = InstallationInput::new(
                &store,
                request.installation_id.clone(),
                request.version_id.clone(),
                InstallationSource::LocalCache {
                    files: request.source_files.clone(),
                },
            )?;
            repo.enqueue_installation(
                &JobAcceptance {
                    id: request.job_id.clone(),
                    idempotency_key: request.job_id.clone(),
                    plan_id: plan.id.clone(),
                    plan_digest: plan.digest()?,
                },
                &input,
                now(),
                free,
            )?;
            install_from_local_cache(&repo, &store, &request, "audio-pair", now)?;
            plans.push(plan);
        }
        drop(repo);
        let reopened = LifecycleRepository::open(&db)?;
        let entries = NativeTaskCatalog::active(&reopened)?.list(NativeTaskIntent::SpeechSynthesis);
        ensure!(
            entries.len() == 2,
            "Restart must discover both speech models"
        );
        let mut specs = Vec::new();
        for (plan, index) in plans.iter().zip([first, second]) {
            let stored = reopened
                .read_plan(&plan.id)?
                .context("Missing audio plan")?;
            ensure!(&stored == plan, "Reopened plan changed");
            ensure!(
                entries
                    .iter()
                    .any(|e| e.identity.version_id == format!("version-{index}")
                        && e.identity.parameter_schema_id == stored.runtime.binding_version),
                "Installed model lost its parameter schema"
            );
            ensure!(
                stored.artifacts.get(&weights[index].artifact.id) == Some(&weights[index].artifact),
                "Wrong model weight"
            );
            let spec = stored
                .artifacts
                .values()
                .find(|a| matches!(a.purpose, FilePurpose::RuntimeDistributionConfig(_)))
                .context("Missing runtime distribution member")?;
            specs.push(spec.clone());
        }
        ensure!(
            specs[0] == specs[1]
                && plans[0].runtime.binding_version != plans[1].runtime.binding_version,
            "Models must share the spec and keep separate parameter schemas"
        );
        ensure!(
            reopened.blob_reference_count(
                specs[0]
                    .source_sha256
                    .as_ref()
                    .context("Missing spec digest")?
            )? == 2,
            "Both installed versions must reference the shared spec"
        );
        println!(
            "{}",
            serde_json::json!({"installedVersions":2,"sharedSpecReferences":2,"first":weights[first].artifact.path,"second":weights[second].artifact.path,"specId":specs[0].id,"parameterSchemas":[plans[0].runtime.binding_version,plans[1].runtime.binding_version]})
        );
    }
    Ok(())
}
