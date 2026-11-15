use anyhow::{ensure, Context, Result};
use lumina_providers::local_inference::lifecycle::{
    assess_current_task_resources_with_publication, InstallationService, NativeTaskEngineRegistry,
    NativeTaskPlan, NativeTaskRuntime, TaskProfilingRequirement, TaskPublicationContext,
    TaskResourceAssessment, TaskResourceRejection,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::{
    import_native_task_output, ImportedTaskOutput, MediaAssociation, MediaResultStore,
    PreparedProjectTask, TaskConfigurationStore,
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
    execute_task_controlled(
        installation,
        engines,
        storage,
        authenticated_session_id,
        TaskExecutionPlan::Session(plan),
        association,
        cancel,
    )
    .await
}

pub(crate) async fn execute_project_prepared_task_controlled(
    installation: &InstallationService,
    engines: &NativeTaskEngineRegistry,
    storage: &SessionStorage,
    authenticated_session_id: &str,
    task: PreparedProjectTask,
    association: MediaAssociation,
    cancel: CancellationToken,
) -> Result<PreparedTaskExecution> {
    execute_task_controlled(
        installation,
        engines,
        storage,
        authenticated_session_id,
        TaskExecutionPlan::Project(task),
        association,
        cancel,
    )
    .await
}

enum TaskExecutionPlan {
    Session(NativeTaskPlan),
    Project(PreparedProjectTask),
}

async fn execute_task_controlled(
    installation: &InstallationService,
    engines: &NativeTaskEngineRegistry,
    storage: &SessionStorage,
    authenticated_session_id: &str,
    execution: TaskExecutionPlan,
    association: MediaAssociation,
    cancel: CancellationToken,
) -> Result<PreparedTaskExecution> {
    let (plan, project_task) = match execution {
        TaskExecutionPlan::Session(plan) => (plan, None),
        TaskExecutionPlan::Project(task) => (task.plan().clone(), Some(task)),
    };
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
    let recorded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM media_task_configurations_v1 WHERE session_id=? AND job_id=?)
            OR EXISTS(SELECT 1 FROM native_task_calls_v1 WHERE session_id=? AND job_id=?)",
    )
    .bind(authenticated_session_id)
    .bind(plan.job_id())
    .bind(authenticated_session_id)
    .bind(plan.job_id())
    .fetch_one(pool)
    .await?;
    if !recorded {
        crate::project::ProjectAssetStore::new(pool.clone())
            .freeze_job(authenticated_session_id, plan.job_id(), None)
            .await?;
    }

    let media = MediaResultStore::new(pool.clone());
    let root = media
        .register_output_root(&storage.media_output_directory())
        .await?;
    let root_path = root.path.clone();
    let publication = if project_task.is_some() {
        super::task_publication::project_reuse_context()?
    } else {
        TaskPublicationContext::Session
    };
    let (plan, assessment) = tokio::task::spawn_blocking(move || -> Result<_> {
        let assessment =
            assess_current_task_resources_with_publication(&plan, &root_path, &publication)?;
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
    let inputs = match &project_task {
        Some(task) => {
            media
                .open_project_task_inputs(storage, authenticated_session_id, task)
                .await?
        }
        None => {
            media
                .open_native_task_inputs(authenticated_session_id, &plan)
                .await?
        }
    };
    let configurations = TaskConfigurationStore::new(pool.clone());
    if let Some(task) = &project_task {
        configurations.save_project_task(task).await?;
    } else {
        match &plan {
            NativeTaskPlan::Generation(task) => configurations.save_generation(task).await?,
            NativeTaskPlan::PocketTts(task) => configurations.save_pocket_tts(task).await?,
            NativeTaskPlan::Qwen3Asr(task) => configurations.save_qwen3_asr(task).await?,
            NativeTaskPlan::StableAudioMusic(task) => configurations.save_music(task).await?,
        };
    }
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
#[path = "task_service_tests.rs"]
mod tests;
