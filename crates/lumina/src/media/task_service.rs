use anyhow::{ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::{
    assess_current_task_resources, InstallationService, NativeTaskEngineRegistry, NativeTaskPlan,
    NativeTaskRuntime, TaskProfilingRequirement, TaskResourceAssessment, TaskResourceRejection,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::{
    import_native_task_output, ImportedTaskOutput, MediaAssociation, MediaResultStore,
    TaskConfigurationStore,
};
use crate::session::session_manager::SessionStorage;

#[derive(Debug)]
pub enum PreparedTaskExecution {
    Completed(ImportedTaskOutput),
    NeedsProfiling(TaskProfilingRequirement),
    Unavailable(TaskResourceRejection),
}

pub(crate) fn configured_task_engines(
    runtime: &lumina_providers::local_inference::lifecycle::RuntimeBuild,
) -> Result<NativeTaskEngineRegistry> {
    let setting = NativeTaskEngineRegistry::configuration_key(runtime)?;
    let configured = std::env::var_os(setting).or_else(|| {
        (setting == "LUMINA_AUDIO_LEGACY_CPU_CLI")
            .then(|| std::env::var_os("LUMINA_AUDIO_CPU_CLI"))
            .flatten()
    });
    let Some(configured) = configured else {
        return Ok(NativeTaskEngineRegistry::default());
    };
    let path = std::path::PathBuf::from(configured);
    ensure!(
        path.is_absolute(),
        "Configured native engine path must be absolute"
    );
    let mut engines = NativeTaskEngineRegistry::default();
    engines.register(runtime, &path)?;
    Ok(engines)
}

/// Executes a compiled plan for a session already authenticated by the caller.
pub async fn execute_prepared_task(
    installation: &InstallationService,
    engines: &NativeTaskEngineRegistry,
    storage: &SessionStorage,
    authenticated_session_id: &str,
    plan: NativeTaskPlan,
    association: MediaAssociation,
) -> Result<PreparedTaskExecution> {
    execute_prepared_task_controlled(
        installation,
        engines,
        storage,
        authenticated_session_id,
        plan,
        association,
        CancellationToken::new(),
    )
    .await
}

pub(crate) async fn execute_prepared_task_controlled(
    installation: &InstallationService,
    engines: &NativeTaskEngineRegistry,
    storage: &SessionStorage,
    authenticated_session_id: &str,
    plan: NativeTaskPlan,
    association: MediaAssociation,
    cancel: CancellationToken,
) -> Result<PreparedTaskExecution> {
    ensure!(
        authenticated_session_id == plan.session_id(),
        "Task belongs to a different session"
    );
    tokio::select! {
        biased;
        _ = cancel.cancelled() => anyhow::bail!("Native task was cancelled"),
        ready = installation.wait_ready() => ready?,
    }
    let repository = Arc::clone(installation.repository());
    let plan = tokio::task::spawn_blocking(move || -> Result<_> {
        let version = repository
            .installed_version(plan.version_id())?
            .context("Task requires a registered installation version")?;
        plan.validate_manifest(&version)?;
        Ok(plan)
    })
    .await
    .context("Task installation verification worker failed")??;

    let pool = storage.pool().await?;
    let media = MediaResultStore::new(pool.clone());
    let root = media
        .register_output_root(&storage.media_output_directory())
        .await?;
    let root_path = root.path.clone();
    let (plan, assessment) = tokio::task::spawn_blocking(move || -> Result<_> {
        let assessment = assess_current_task_resources(&plan, &root_path)?;
        Ok((plan, assessment))
    })
    .await
    .context("Task resource assessment worker failed")??;
    ensure!(!cancel.is_cancelled(), "Native task was cancelled");
    let admission = match assessment {
        TaskResourceAssessment::NeedsProfiling(requirement) => {
            return Ok(PreparedTaskExecution::NeedsProfiling(requirement))
        }
        TaskResourceAssessment::Unavailable(reason) => {
            return Ok(PreparedTaskExecution::Unavailable(reason))
        }
        TaskResourceAssessment::Admitted(admission) => admission,
    };
    let inputs = media
        .open_native_task_inputs(authenticated_session_id, &plan)
        .await?;
    let configurations = TaskConfigurationStore::new(pool.clone());
    match &plan {
        NativeTaskPlan::Generation(task) => configurations.save_generation(task).await?,
        NativeTaskPlan::PocketTts(task) => configurations.save_pocket_tts(task).await?,
        NativeTaskPlan::Qwen3Asr(task) => configurations.save_qwen3_asr(task).await?,
    };
    let output = NativeTaskRuntime::execute_cancellable(
        installation.repository(),
        installation.store(),
        engines,
        plan.clone(),
        inputs,
        admission,
        &root.path,
        cancel.cancelled(),
    )
    .await?;
    ensure!(!cancel.is_cancelled(), "Native task was cancelled");
    let imported = import_native_task_output(
        &configurations,
        &media,
        &plan,
        output,
        &root.root_id,
        association,
    )
    .await?;
    Ok(PreparedTaskExecution::Completed(imported))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumina_providers::local_inference::lifecycle::*;
    use std::{path::PathBuf, time::Duration};

    fn prepared() -> NativeTaskPlan {
        let profile = GenerationProfile::Sd15CpuTextToImage;
        let binding = GenerationModelBinding {
            installation_id: "unregistered-installation".into(),
            version_id: "unregistered-version".into(),
            manifest_digest: Sha256Digest::of_bytes(b"fixture"),
            runtime: profile.runtime_build(),
            target_capabilities: profile.required_capabilities(),
            assets: profile
                .recorded_assets()
                .into_iter()
                .map(|(role, (sha256, size_bytes))| {
                    (
                        role,
                        GenerationAssetBinding {
                            artifact_id: "model".into(),
                            sha256,
                            size_bytes,
                        },
                    )
                })
                .collect(),
        };
        GenerationParameterService::new(profile, binding)
            .unwrap()
            .compile(GenerationTaskRequest {
                session_id: "session-a".into(),
                job_id: "job-a".into(),
                parameters: GenerationParameters::Image(Sd15ImageParameters {
                    prompt: Some("an apple".into()),
                    ..Default::default()
                }),
            })
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn rejects_foreign_session_and_absent_install_before_persisting_configuration() {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/lifecycle-validation/task-service-unit");
        std::fs::create_dir_all(&directory).unwrap();
        let tmp = tempfile::tempdir_in(directory.canonicalize().unwrap()).unwrap();
        let service = InstallationService::start(
            Arc::new(LifecycleRepository::open(&tmp.path().join("models.db")).unwrap()),
            Arc::new(InstallationStore::open(&tmp.path().join("models")).unwrap()),
            Arc::new(|| Ok(None)),
            Arc::new(|| 1000),
            Duration::from_millis(50),
        )
        .unwrap();
        let storage = SessionStorage::new(tmp.path().to_owned());
        let engines = NativeTaskEngineRegistry::default();
        let association = MediaAssociation::Message {
            message_id: "test".into(),
        };
        for (session, expected) in [
            ("session-b", "different session"),
            ("session-a", "registered installation version"),
        ] {
            // The ACP command may spawn this future on its multithreaded runtime.
            fn is_send<T: Send>(value: T) -> T {
                value
            }
            let error = is_send(execute_prepared_task(
                &service,
                &engines,
                &storage,
                session,
                prepared(),
                association.clone(),
            ))
            .await
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
        assert!(!storage.media_output_directory().exists());
        let configurations = TaskConfigurationStore::new(storage.pool().await.unwrap().clone());
        assert!(configurations
            .read("session-a", "job-a")
            .await
            .unwrap()
            .is_none());
        service.shutdown().await.unwrap();
    }
}
