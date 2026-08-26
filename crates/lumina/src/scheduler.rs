use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_cron_scheduler::{job::JobId, Job, JobScheduler as TokioJobScheduler};
use tokio_util::sync::CancellationToken;

use crate::agents::AgentEvent;
use crate::agents::{Agent, SessionConfig};
use crate::config::paths::Paths;
use crate::config::{resolve_extensions_for_new_session, Config};
use crate::conversation::message::Message;
use crate::conversation::Conversation;
use crate::providers::create;
use crate::recipe::build_recipe::build_recipe_from_template;
use crate::recipe::Recipe;
use crate::scheduler_trait::SchedulerTrait;
use crate::session::session_manager::ExtensionProvenanceError;
use crate::session::session_manager::SessionType;
use crate::session::{Session, SessionManager};

type RunningTasksMap = HashMap<String, CancellationToken>;
type JobsMap = HashMap<String, (JobId, ScheduledJob)>;

#[cfg(test)]
static ATOMIC_REPLACE_FAILURES: std::sync::LazyLock<std::sync::Mutex<HashSet<PathBuf>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

pub fn get_default_scheduler_storage_path() -> Result<PathBuf, io::Error> {
    let data_dir = Paths::data_dir();
    fs::create_dir_all(&data_dir)?;
    Ok(data_dir.join("schedule.json"))
}

pub fn get_default_scheduled_recipes_dir() -> Result<PathBuf, SchedulerError> {
    let data_dir = Paths::data_dir();
    let recipes_dir = data_dir.join("scheduled_recipes");
    fs::create_dir_all(&recipes_dir).map_err(SchedulerError::StorageError)?;
    Ok(recipes_dir)
}

#[derive(Debug)]
pub enum SchedulerError {
    JobIdExists(String),
    JobNotFound(String),
    StorageError(io::Error),
    RecipeLoadError(String),
    AgentSetupError(String),
    PersistError(String),
    CronParseError(String),
    SecurityRejected,
    RepositoryUnavailable,
    SchedulerInternalError(String),
    AnyhowError(anyhow::Error),
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchedulerError::JobIdExists(id) => write!(f, "Job ID '{}' already exists.", id),
            SchedulerError::JobNotFound(id) => write!(f, "Job ID '{}' not found.", id),
            SchedulerError::StorageError(e) => write!(f, "Storage error: {}", e),
            SchedulerError::RecipeLoadError(e) => write!(f, "Recipe load error: {}", e),
            SchedulerError::AgentSetupError(e) => write!(f, "Agent setup error: {}", e),
            SchedulerError::PersistError(e) => write!(f, "Failed to persist schedules: {}", e),
            SchedulerError::CronParseError(e) => write!(f, "Invalid cron string: {}", e),
            SchedulerError::SecurityRejected => write!(f, "Schedule security validation rejected."),
            SchedulerError::RepositoryUnavailable => {
                write!(f, "Schedule provenance repository unavailable.")
            }
            SchedulerError::SchedulerInternalError(e) => {
                write!(f, "Scheduler internal error: {}", e)
            }
            SchedulerError::AnyhowError(e) => write!(f, "Scheduler operation failed: {}", e),
        }
    }
}

impl std::error::Error for SchedulerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SchedulerError::StorageError(e) => Some(e),
            SchedulerError::AnyhowError(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<io::Error> for SchedulerError {
    fn from(err: io::Error) -> Self {
        SchedulerError::StorageError(err)
    }
}

impl From<serde_json::Error> for SchedulerError {
    fn from(err: serde_json::Error) -> Self {
        SchedulerError::PersistError(err.to_string())
    }
}

impl From<anyhow::Error> for SchedulerError {
    fn from(err: anyhow::Error) -> Self {
        SchedulerError::AnyhowError(err)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ScheduledJob {
    pub id: String,
    pub source: String,
    pub cron: String,
    pub last_run: Option<DateTime<Utc>>,
    #[serde(default)]
    pub currently_running: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub current_session_id: Option<String>,
    #[serde(default)]
    pub process_start_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub parameters: Vec<(String, String)>,
    /// Original directory of the recipe file before it was copied to scheduled_recipes/.
    /// Preserved so that relative paths (sub-recipes, template includes) resolve correctly
    /// against the source tree rather than the scheduler's internal storage directory.
    #[serde(default)]
    pub recipe_base_dir: Option<String>,
}

async fn persist_jobs(
    storage_path: &Path,
    jobs: &Arc<Mutex<JobsMap>>,
) -> Result<(), SchedulerError> {
    let jobs_guard = jobs.lock().await;
    let list: Vec<ScheduledJob> = jobs_guard.values().map(|(_, j)| j.clone()).collect();
    persist_job_list(storage_path, &list)
}

fn persist_job_list(storage_path: &Path, list: &[ScheduledJob]) -> Result<(), SchedulerError> {
    if let Some(parent) = storage_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(list)?;
    let parent = storage_path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(data.as_bytes())?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    #[cfg(test)]
    if should_fail_atomic_replace(storage_path) {
        return Err(SchedulerError::StorageError(io::Error::other(
            "injected atomic replace failure",
        )));
    }
    temporary
        .persist(storage_path)
        .map_err(|error| SchedulerError::StorageError(error.error))?;
    sync_parent_directory(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), SchedulerError> {
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), SchedulerError> {
    Ok(())
}

#[cfg(test)]
fn should_fail_atomic_replace(storage_path: &Path) -> bool {
    ATOMIC_REPLACE_FAILURES
        .lock()
        .expect("replace fault lock poisoned")
        .remove(storage_path)
}

#[cfg(test)]
fn fail_next_atomic_replace(storage_path: &Path) {
    ATOMIC_REPLACE_FAILURES
        .lock()
        .expect("replace fault lock poisoned")
        .insert(storage_path.to_path_buf());
}

fn clear_running_state(job: &mut ScheduledJob) -> bool {
    let changed = job.currently_running
        || job.current_session_id.is_some()
        || job.process_start_time.is_some();
    job.currently_running = false;
    job.current_session_id = None;
    job.process_start_time = None;
    changed
}

pub struct Scheduler {
    tokio_scheduler: TokioJobScheduler,
    jobs: Arc<Mutex<JobsMap>>,
    storage_path: PathBuf,
    running_tasks: Arc<Mutex<RunningTasksMap>>,
    session_manager: Arc<SessionManager>,
    mutation_lock: Arc<Mutex<()>>,
    reservations: Mutex<HashSet<String>>,
    pending_callback_cleanup: Arc<Mutex<HashMap<JobId, String>>>,
    degraded_schedules: Arc<Mutex<HashMap<String, String>>>,
    callback_generations: Arc<Mutex<HashMap<String, String>>>,
}

impl Scheduler {
    pub async fn new(
        storage_path: PathBuf,
        session_manager: Arc<SessionManager>,
    ) -> Result<Arc<Self>, SchedulerError> {
        let internal_scheduler = TokioJobScheduler::new()
            .await
            .map_err(|e| SchedulerError::SchedulerInternalError(e.to_string()))?;

        let jobs = Arc::new(Mutex::new(HashMap::new()));
        let running_tasks = Arc::new(Mutex::new(HashMap::new()));

        let arc_self = Arc::new(Self {
            tokio_scheduler: internal_scheduler,
            jobs,
            storage_path,
            running_tasks,
            session_manager,
            mutation_lock: Arc::new(Mutex::new(())),
            reservations: Mutex::new(HashSet::new()),
            pending_callback_cleanup: Arc::new(Mutex::new(HashMap::new())),
            degraded_schedules: Arc::new(Mutex::new(HashMap::new())),
            callback_generations: Arc::new(Mutex::new(HashMap::new())),
        });

        arc_self.load_jobs_from_storage().await?;
        arc_self
            .tokio_scheduler
            .start()
            .await
            .map_err(|e| SchedulerError::SchedulerInternalError(e.to_string()))?;

        Ok(arc_self)
    }

    fn create_cron_task(&self, job: ScheduledJob) -> Result<(Job, String), SchedulerError> {
        let job_for_task = job.clone();
        let callback_generation = uuid::Uuid::new_v4().to_string();
        let callback_generation_for_task = callback_generation.clone();
        let jobs_arc = self.jobs.clone();
        let storage_path = self.storage_path.clone();
        let running_tasks_arc = self.running_tasks.clone();
        let session_manager = self.session_manager.clone();
        let mutation_lock = self.mutation_lock.clone();
        let degraded_schedules = self.degraded_schedules.clone();
        let callback_generations = self.callback_generations.clone();

        let cron_parts: Vec<&str> = job.cron.split_whitespace().collect();
        let cron = match cron_parts.len() {
            5 => {
                tracing::warn!(
                    "Job '{}' has legacy 5-field cron '{}', converting to 6-field",
                    job.id,
                    job.cron
                );
                format!("0 {}", job.cron)
            }
            6 => job.cron.clone(),
            _ => {
                return Err(SchedulerError::CronParseError(format!(
                    "Invalid cron expression '{}': expected 5 or 6 fields, got {}",
                    job.cron,
                    cron_parts.len()
                )))
            }
        };

        let local_tz = Local::now().timezone();

        let task = Job::new_async_tz(&cron, local_tz, move |_uuid, _l| {
            tracing::info!("Cron task triggered for job '{}'", job_for_task.id);
            let task_job_id = job_for_task.id.clone();
            let current_jobs_arc = jobs_arc.clone();
            let local_storage_path = storage_path.clone();
            let running_tasks = running_tasks_arc.clone();
            let session_manager = session_manager.clone();
            let mutation_lock = mutation_lock.clone();
            let degraded_schedules = degraded_schedules.clone();
            let callback_generations = callback_generations.clone();
            let expected_generation = callback_generation_for_task.clone();

            Box::pin(async move {
                if callback_generations
                    .lock()
                    .await
                    .get(&task_job_id)
                    .is_none_or(|generation| generation != &expected_generation)
                {
                    return;
                }
                let job_to_verify = {
                    let jobs_guard = current_jobs_arc.lock().await;
                    jobs_guard.get(&task_job_id).map(|(_, job)| job.clone())
                };
                let Some(job_to_verify) = job_to_verify else {
                    return;
                };
                if let Err(error) =
                    verify_scheduled_job_recipe_provenance(session_manager.as_ref(), &job_to_verify)
                        .await
                {
                    tracing::error!(
                        job_id = %task_job_id,
                        error = %error,
                        "Rejected scheduled job before runtime state reservation"
                    );
                    return;
                }

                let _mutation = mutation_lock.lock().await;
                if callback_generations
                    .lock()
                    .await
                    .get(&task_job_id)
                    .is_none_or(|generation| generation != &expected_generation)
                {
                    return;
                }
                let current_time = Utc::now();
                let job_to_execute = {
                    let mut jobs_guard = current_jobs_arc.lock().await;
                    let Some((_, job)) = jobs_guard.get_mut(&task_job_id) else {
                        return;
                    };
                    if job.paused
                        || job.currently_running
                        || job.source != job_to_verify.source
                        || job.parameters != job_to_verify.parameters
                        || job.recipe_base_dir != job_to_verify.recipe_base_dir
                    {
                        return;
                    }
                    job.last_run = Some(current_time);
                    job.currently_running = true;
                    job.process_start_time = Some(current_time);
                    job.clone()
                };

                if let Err(error) = persist_jobs(&local_storage_path, &current_jobs_arc).await {
                    if let Some((_, job)) = current_jobs_arc.lock().await.get_mut(&task_job_id) {
                        clear_running_state(job);
                        job.last_run = job_to_verify.last_run.clone();
                    }
                    degraded_schedules
                        .lock()
                        .await
                        .insert(task_job_id.clone(), format!("runtime reservation persistence failed: {error}"));
                    tracing::error!(job_id = %task_job_id, %error, "Failed to persist scheduler runtime reservation");
                    return;
                }

                let cancel_token = CancellationToken::new();
                {
                    let mut tasks = running_tasks.lock().await;
                    tasks.insert(task_job_id.clone(), cancel_token.clone());
                }
                drop(_mutation);

                let result = execute_job(
                    job_to_execute,
                    current_jobs_arc.clone(),
                    task_job_id.clone(),
                    cancel_token.clone(),
                    session_manager,
                    mutation_lock.clone(),
                    local_storage_path.clone(),
                )
                .await;

                let _mutation = mutation_lock.lock().await;
                {
                    let mut tasks = running_tasks.lock().await;
                    tasks.remove(&task_job_id);
                }

                {
                    let mut jobs_guard = current_jobs_arc.lock().await;
                    if let Some((_, job)) = jobs_guard.get_mut(&task_job_id) {
                        job.currently_running = false;
                        job.current_session_id = None;
                        job.process_start_time = None;
                    }
                }

                if let Err(error) = persist_jobs(&local_storage_path, &current_jobs_arc).await {
                    degraded_schedules
                        .lock()
                        .await
                        .insert(task_job_id.clone(), format!("completion persistence failed: {error}"));
                    tracing::error!(job_id = %task_job_id, %error, "Failed to persist scheduler completion");
                } else {
                    degraded_schedules.lock().await.remove(&task_job_id);
                }

                match result {
                    Ok(_) => tracing::info!("Job '{}' completed", task_job_id),
                    Err(ref e) => {
                        tracing::error!("Job '{}' failed: {}", task_job_id, e);
                    }
                }
            })
        })
        .map_err(|e| SchedulerError::CronParseError(e.to_string()))?;
        Ok((task, callback_generation))
    }

    pub async fn add_scheduled_job(
        &self,
        original_job_spec: ScheduledJob,
        make_copy: bool,
    ) -> Result<(), SchedulerError> {
        let _mutation = self.mutation_lock.lock().await;
        self.retry_pending_callback_cleanup().await?;
        self.reserve_job_id(&original_job_spec.id).await?;
        self.add_reserved_scheduled_job(original_job_spec, make_copy)
            .await
    }

    pub async fn add_scheduled_job_from_content(
        &self,
        mut job: ScheduledJob,
        recipe_content: &[u8],
    ) -> Result<ScheduledJob, SchedulerError> {
        let _mutation = self.mutation_lock.lock().await;
        self.retry_pending_callback_cleanup().await?;
        let job_id = job.id.clone();
        self.reserve_job_id(&job_id).await?;
        let result = async {
            let recipes_dir = get_default_scheduled_recipes_dir()?;
            let destination = recipes_dir.join(format!("{job_id}.yaml"));
            let mut temporary = tempfile::NamedTempFile::new_in(&recipes_dir)
                .map_err(SchedulerError::StorageError)?;
            temporary
                .write_all(recipe_content)
                .map_err(SchedulerError::StorageError)?;
            temporary.flush().map_err(SchedulerError::StorageError)?;
            temporary
                .as_file()
                .sync_all()
                .map_err(SchedulerError::StorageError)?;
            job.source = temporary.path().to_string_lossy().into_owned();
            self.verify_job_recipe_provenance(&job).await?;
            temporary.persist_noclobber(&destination).map_err(|error| {
                if destination.exists() {
                    SchedulerError::JobIdExists(job_id.clone())
                } else {
                    SchedulerError::StorageError(error.error)
                }
            })?;
            sync_parent_directory(&recipes_dir)?;
            job.source = destination.to_string_lossy().into_owned();
            if let Err(error) = self.commit_reserved_job(job.clone()).await {
                return match fs::remove_file(&destination) {
                    Ok(()) => {
                        sync_parent_directory(&recipes_dir)?;
                        Err(error)
                    }
                    Err(cleanup_error) => Err(SchedulerError::SchedulerInternalError(format!(
                        "{error}; recipe rollback cleanup failed: {cleanup_error}"
                    ))),
                };
            }
            Ok(job)
        }
        .await;
        self.reservations.lock().await.remove(&job_id);
        result
    }

    async fn reserve_job_id(&self, id: &str) -> Result<(), SchedulerError> {
        let mut reservations = self.reservations.lock().await;
        if reservations.contains(id) || self.jobs.lock().await.contains_key(id) {
            return Err(SchedulerError::JobIdExists(id.to_string()));
        }
        reservations.insert(id.to_string());
        Ok(())
    }

    async fn add_reserved_scheduled_job(
        &self,
        mut stored_job: ScheduledJob,
        make_copy: bool,
    ) -> Result<(), SchedulerError> {
        let job_id = stored_job.id.clone();
        let result = async {
            self.verify_job_recipe_provenance(&stored_job).await?;
            let mut owned_recipe = None;
            if make_copy {
                let original_recipe_path =
                    Path::new(&stored_job.source).canonicalize().map_err(|e| {
                        SchedulerError::RecipeLoadError(format!(
                            "Recipe file not found: {}: {}",
                            stored_job.source, e
                        ))
                    })?;
                if !original_recipe_path.is_file() {
                    return Err(SchedulerError::RecipeLoadError(format!(
                        "Recipe file not found: {}",
                        stored_job.source
                    )));
                }
                let recipes_dir = get_default_scheduled_recipes_dir()?;
                let extension = original_recipe_path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("yaml");
                let destination = recipes_dir.join(format!("{job_id}.{extension}"));
                let mut source = fs::File::open(&original_recipe_path)?;
                let mut temporary = tempfile::NamedTempFile::new_in(&recipes_dir)?;
                io::copy(&mut source, &mut temporary)?;
                temporary.flush()?;
                temporary.as_file().sync_all()?;
                temporary.persist_noclobber(&destination).map_err(|error| {
                    if destination.exists() {
                        SchedulerError::JobIdExists(job_id.clone())
                    } else {
                        SchedulerError::StorageError(error.error)
                    }
                })?;
                sync_parent_directory(&recipes_dir)?;
                stored_job.recipe_base_dir = original_recipe_path
                    .parent()
                    .map(|path| path.to_string_lossy().into_owned());
                stored_job.source = destination.to_string_lossy().into_owned();
                stored_job.current_session_id = None;
                stored_job.process_start_time = None;
                owned_recipe = Some(destination);
            }
            if let Err(error) = self.commit_reserved_job(stored_job).await {
                if let Some(path) = owned_recipe {
                    if let Err(cleanup_error) = fs::remove_file(&path) {
                        return Err(SchedulerError::SchedulerInternalError(format!(
                            "{error}; recipe rollback cleanup failed: {cleanup_error}"
                        )));
                    }
                    if let Some(parent) = path.parent() {
                        sync_parent_directory(parent)?;
                    }
                }
                return Err(error);
            }
            Ok(())
        }
        .await;
        self.reservations.lock().await.remove(&job_id);
        result
    }

    async fn commit_reserved_job(&self, job: ScheduledJob) -> Result<(), SchedulerError> {
        let (cron_task, generation) = self.create_cron_task(job.clone())?;
        let job_uuid = self
            .tokio_scheduler
            .add(cron_task)
            .await
            .map_err(|error| SchedulerError::SchedulerInternalError(error.to_string()))?;
        let mut persisted = self
            .jobs
            .lock()
            .await
            .values()
            .map(|(_, current)| current.clone())
            .collect::<Vec<_>>();
        persisted.push(job.clone());
        if let Err(error) = persist_job_list(&self.storage_path, &persisted) {
            self.callback_generations.lock().await.remove(&job.id);
            if let Err(cleanup_error) = self.tokio_scheduler.remove(&job_uuid).await {
                self.pending_callback_cleanup
                    .lock()
                    .await
                    .insert(job_uuid, job.id.clone());
                self.degraded_schedules.lock().await.insert(
                    job.id.clone(),
                    format!("storage publication failed and callback cleanup is pending: {cleanup_error}"),
                );
                return Err(SchedulerError::SchedulerInternalError(format!(
                    "{error}; callback cleanup failed: {cleanup_error}"
                )));
            }
            return Err(error);
        }
        self.jobs
            .lock()
            .await
            .insert(job.id.clone(), (job_uuid, job.clone()));
        self.callback_generations
            .lock()
            .await
            .insert(job.id, generation);
        Ok(())
    }

    async fn retry_pending_callback_cleanup(&self) -> Result<(), SchedulerError> {
        let pending = self
            .pending_callback_cleanup
            .lock()
            .await
            .iter()
            .map(|(uuid, id)| (*uuid, id.clone()))
            .collect::<Vec<_>>();
        for (uuid, id) in pending {
            match self.tokio_scheduler.remove(&uuid).await {
                Ok(()) => {
                    self.pending_callback_cleanup.lock().await.remove(&uuid);
                    self.degraded_schedules.lock().await.remove(&id);
                }
                Err(error) => {
                    return Err(SchedulerError::SchedulerInternalError(format!(
                        "callback cleanup for schedule '{id}' remains pending: {error}"
                    )));
                }
            }
        }
        Ok(())
    }

    pub async fn schedule_recipe(
        &self,
        recipe_path: PathBuf,
        cron_schedule: Option<String>,
    ) -> Result<(), SchedulerError> {
        let recipe_path_str = recipe_path.to_string_lossy().to_string();

        let existing_job_id = {
            let jobs_guard = self.jobs.lock().await;
            jobs_guard
                .iter()
                .find(|(_, (_, job))| job.source == recipe_path_str)
                .map(|(id, _)| id.clone())
        };

        match cron_schedule {
            Some(cron) => {
                if let Some(job_id) = existing_job_id {
                    self.update_schedule(&job_id, cron).await
                } else {
                    let job_id = self.generate_unique_job_id(&recipe_path).await;
                    let job = ScheduledJob {
                        id: job_id,
                        source: recipe_path_str,
                        cron,
                        last_run: None,
                        currently_running: false,
                        paused: false,
                        current_session_id: None,
                        process_start_time: None,
                        parameters: vec![],
                        recipe_base_dir: None,
                    };
                    self.add_scheduled_job(job, false).await
                }
            }
            None => {
                if let Some(job_id) = existing_job_id {
                    self.remove_scheduled_job(&job_id, false).await
                } else {
                    Ok(())
                }
            }
        }
    }

    async fn generate_unique_job_id(&self, path: &Path) -> String {
        let base_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();

        let jobs_guard = self.jobs.lock().await;
        let mut id = base_id.clone();
        let mut counter = 1;

        while jobs_guard.contains_key(&id) {
            id = format!("{}_{}", base_id, counter);
            counter += 1;
        }

        id
    }

    async fn load_jobs_from_storage(self: &Arc<Self>) -> Result<(), SchedulerError> {
        if !self.storage_path.exists() {
            return Ok(());
        }
        let data = fs::read_to_string(&self.storage_path)?;
        if data.trim().is_empty() {
            return Ok(());
        }
        let mut list: Vec<ScheduledJob> = serde_json::from_str(&data)?;

        let id_counts = list.iter().fold(HashMap::new(), |mut counts, job| {
            *counts.entry(job.id.clone()).or_insert(0_usize) += 1;
            counts
        });
        let mut verified = vec![false; list.len()];
        for (index, job) in list.iter().enumerate() {
            if id_counts.get(&job.id).copied().unwrap_or_default() > 1 {
                self.degraded_schedules.lock().await.insert(
                    job.id.clone(),
                    "duplicate persisted schedule identifier".to_string(),
                );
                tracing::error!(
                    job_id = %job.id,
                    "Rejected all persisted schedules sharing a duplicate job ID"
                );
                continue;
            }
            if !Path::new(&job.source).exists() {
                self.degraded_schedules.lock().await.insert(
                    job.id.clone(),
                    "persisted recipe file is missing".to_string(),
                );
                continue;
            }
            match verify_scheduled_job_recipe_provenance(self.session_manager.as_ref(), job).await {
                Ok(()) => {
                    verified[index] = true;
                }
                Err(error) => {
                    self.degraded_schedules.lock().await.insert(
                        job.id.clone(),
                        format!("persisted schedule provenance rejected: {error}"),
                    );
                    tracing::error!(
                        job_id = %job.id,
                        error = %error,
                        "Rejected persisted schedule before restoring runtime state"
                    );
                }
            }
        }

        let mut reset_stale_running_state = false;
        for (index, job) in list.iter_mut().enumerate() {
            if verified[index] {
                reset_stale_running_state |= clear_running_state(job);
            }
        }
        if reset_stale_running_state {
            persist_job_list(&self.storage_path, &list)?;
        }

        for (index, job_to_load) in list.into_iter().enumerate() {
            if !verified[index] {
                continue;
            }
            if !Path::new(&job_to_load.source).exists() {
                tracing::warn!(
                    "Recipe file {} not found, skipping job '{}'",
                    job_to_load.source,
                    job_to_load.id
                );
                continue;
            }

            let (cron_task, generation) = match self.create_cron_task(job_to_load.clone()) {
                Ok(task) => task,
                Err(e) => {
                    self.degraded_schedules.lock().await.insert(
                        job_to_load.id.clone(),
                        format!("failed to create startup callback: {e}"),
                    );
                    tracing::error!(
                        "Failed to create cron task for job '{}': {}. Skipping.",
                        job_to_load.id,
                        e
                    );
                    continue;
                }
            };

            let job_uuid = match self.tokio_scheduler.add(cron_task).await {
                Ok(uuid) => uuid,
                Err(e) => {
                    self.degraded_schedules.lock().await.insert(
                        job_to_load.id.clone(),
                        format!("failed to register startup callback: {e}"),
                    );
                    tracing::error!(
                        "Failed to add job '{}' to scheduler: {}. Skipping.",
                        job_to_load.id,
                        e
                    );
                    continue;
                }
            };

            self.degraded_schedules.lock().await.remove(&job_to_load.id);
            self.jobs
                .lock()
                .await
                .insert(job_to_load.id.clone(), (job_uuid, job_to_load.clone()));
            self.callback_generations
                .lock()
                .await
                .insert(job_to_load.id, generation);
        }
        Ok(())
    }

    async fn sync_from_storage_locked(&self) {
        if let Err(error) = self.retry_pending_callback_cleanup().await {
            self.degraded_schedules
                .lock()
                .await
                .insert("__storage__".to_string(), error.to_string());
            tracing::error!(error = %error, "Scheduler callback cleanup remains pending");
            return;
        }
        if !self.storage_path.exists() {
            return;
        }
        let data = match fs::read_to_string(&self.storage_path) {
            Ok(d) => d,
            Err(error) => {
                self.degraded_schedules.lock().await.insert(
                    "__storage__".to_string(),
                    format!("schedule storage read failed: {error}"),
                );
                return;
            }
        };
        if data.trim().is_empty() {
            return;
        }
        let disk_jobs: Vec<ScheduledJob> = match serde_json::from_str(&data) {
            Ok(jobs) => jobs,
            Err(error) => {
                self.degraded_schedules.lock().await.insert(
                    "__storage__".to_string(),
                    format!("schedule storage parse failed: {error}"),
                );
                return;
            }
        };
        self.degraded_schedules.lock().await.remove("__storage__");

        let disk_id_counts = disk_jobs.iter().fold(HashMap::new(), |mut counts, job| {
            *counts.entry(job.id.clone()).or_insert(0_usize) += 1;
            counts
        });
        let disk_ids: HashSet<String> = disk_id_counts
            .iter()
            .filter(|(_, count)| **count == 1)
            .map(|(id, _)| id.clone())
            .collect();
        for (id, count) in &disk_id_counts {
            if *count > 1 {
                self.degraded_schedules.lock().await.insert(
                    id.clone(),
                    "duplicate schedule identifier in storage".to_string(),
                );
            }
        }

        let (jobs_to_add, jobs_to_remove): (Vec<ScheduledJob>, Vec<(String, JobId)>) = {
            let jobs_guard = self.jobs.lock().await;
            let to_add = disk_jobs
                .into_iter()
                .filter(|job| disk_ids.contains(&job.id) && !jobs_guard.contains_key(&job.id))
                .collect();
            let to_remove = jobs_guard
                .iter()
                .filter(|(id, (_, j))| !disk_ids.contains(*id) && !j.currently_running)
                .map(|(id, (uuid, _))| (id.clone(), *uuid))
                .collect();
            (to_add, to_remove)
        };

        for job in jobs_to_add {
            if !Path::new(&job.source).exists() {
                self.degraded_schedules.lock().await.insert(
                    job.id.clone(),
                    "synchronized recipe file is missing".to_string(),
                );
                tracing::warn!(
                    "Skipping sync of job '{}': recipe file not found at {}",
                    job.id,
                    job.source
                );
                continue;
            }
            if let Err(error) = self.verify_job_recipe_provenance(&job).await {
                self.degraded_schedules.lock().await.insert(
                    job.id.clone(),
                    format!("synchronized schedule provenance rejected: {error}"),
                );
                tracing::error!(
                    job_id = %job.id,
                    error = %error,
                    "Rejected synchronized schedule before registering cron callback"
                );
                continue;
            }
            let (cron_task, generation) = match self.create_cron_task(job.clone()) {
                Ok(t) => t,
                Err(e) => {
                    self.degraded_schedules.lock().await.insert(
                        job.id.clone(),
                        format!("synchronized callback creation failed: {e}"),
                    );
                    tracing::error!(
                        "Failed to create cron task for '{}' during sync: {}",
                        job.id,
                        e
                    );
                    continue;
                }
            };
            let uuid = match self.tokio_scheduler.add(cron_task).await {
                Ok(u) => u,
                Err(e) => {
                    self.degraded_schedules.lock().await.insert(
                        job.id.clone(),
                        format!("synchronized callback registration failed: {e}"),
                    );
                    tracing::error!("Failed to register job '{}' during sync: {}", job.id, e);
                    continue;
                }
            };
            let job_id = job.id.clone();
            self.jobs.lock().await.insert(job_id.clone(), (uuid, job));
            self.callback_generations
                .lock()
                .await
                .insert(job_id.clone(), generation);
            self.degraded_schedules.lock().await.remove(&job_id);
        }

        for (id, uuid) in jobs_to_remove {
            match self.tokio_scheduler.remove(&uuid).await {
                Ok(()) => {
                    self.jobs.lock().await.remove(&id);
                    self.callback_generations.lock().await.remove(&id);
                    self.degraded_schedules.lock().await.remove(&id);
                }
                Err(error) => {
                    self.degraded_schedules.lock().await.insert(
                        id.clone(),
                        format!("synchronized callback removal failed: {error}"),
                    );
                    tracing::error!(
                        job_id = %id,
                        error = %error,
                        "Failed to remove synchronized scheduler callback; retaining tracked state"
                    );
                }
            }
        }
    }

    pub async fn list_scheduled_jobs(&self) -> Vec<ScheduledJob> {
        let _mutation = self.mutation_lock.lock().await;
        self.sync_from_storage_locked().await;
        let mut jobs: Vec<ScheduledJob> = self
            .jobs
            .lock()
            .await
            .values()
            .map(|(_, j)| j.clone())
            .collect();
        jobs.sort_by(|a, b| a.id.cmp(&b.id));
        jobs
    }

    pub async fn degraded_schedule_states(&self) -> Vec<(String, String)> {
        let _mutation = self.mutation_lock.lock().await;
        let mut states = self
            .degraded_schedules
            .lock()
            .await
            .iter()
            .map(|(id, reason)| (id.clone(), reason.clone()))
            .collect::<Vec<_>>();
        states.sort_by(|left, right| left.0.cmp(&right.0));
        states
    }

    pub async fn remove_scheduled_job(
        &self,
        id: &str,
        remove_recipe: bool,
    ) -> Result<(), SchedulerError> {
        let _mutation = self.mutation_lock.lock().await;
        self.retry_pending_callback_cleanup().await?;
        let (job_uuid, job) = {
            let jobs_guard = self.jobs.lock().await;
            match jobs_guard.get(id) {
                Some((uuid, job)) => (*uuid, job.clone()),
                None => return Err(SchedulerError::JobNotFound(id.to_string())),
            }
        };

        if let Err(callback_error) = self.tokio_scheduler.remove(&job_uuid).await {
            self.callback_generations.lock().await.remove(id);
            self.jobs.lock().await.remove(id);
            self.pending_callback_cleanup
                .lock()
                .await
                .insert(job_uuid, id.to_string());
            let persist_error = persist_jobs(&self.storage_path, &self.jobs)
                .await
                .err()
                .map(|error| format!("; storage reconciliation failed: {error}"))
                .unwrap_or_default();
            let recipe_cleanup_error = if remove_recipe {
                let path = Path::new(&job.source);
                if path.exists() {
                    match fs::remove_file(path) {
                        Ok(()) => match path.parent().map(sync_parent_directory).transpose() {
                            Ok(_) => String::new(),
                            Err(error) => format!("; recipe directory sync failed: {error}"),
                        },
                        Err(error) => format!("; recipe cleanup failed: {error}"),
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            let failure = format!(
                "callback removal failed and cleanup is pending: {callback_error}{persist_error}{recipe_cleanup_error}"
            );
            self.degraded_schedules
                .lock()
                .await
                .insert(id.to_string(), failure.clone());
            return Err(SchedulerError::SchedulerInternalError(failure));
        }
        self.callback_generations.lock().await.remove(id);
        self.jobs.lock().await.remove(id);
        if let Err(persist_error) = persist_jobs(&self.storage_path, &self.jobs).await {
            let (replacement, generation) = self.create_cron_task(job.clone())?;
            return match self.tokio_scheduler.add(replacement).await {
                Ok(replacement_uuid) => {
                    self.jobs
                        .lock()
                        .await
                        .insert(id.to_string(), (replacement_uuid, job));
                    self.callback_generations
                        .lock()
                        .await
                        .insert(id.to_string(), generation);
                    Err(persist_error)
                }
                Err(error) => {
                    self.degraded_schedules.lock().await.insert(
                        id.to_string(),
                        format!(
                            "schedule persistence failed after callback removal and callback restore failed: {error}"
                        ),
                    );
                    Err(SchedulerError::SchedulerInternalError(format!(
                        "{persist_error}; failed to restore callback: {error}"
                    )))
                }
            };
        }
        if remove_recipe {
            let path = Path::new(&job.source);
            if path.exists() {
                if let Err(error) = fs::remove_file(path) {
                    self.degraded_schedules.lock().await.insert(
                        id.to_string(),
                        format!("schedule removed but recipe cleanup failed: {error}"),
                    );
                    return Err(error.into());
                }
                if let Some(parent) = path.parent() {
                    sync_parent_directory(parent)?;
                }
            }
        }
        self.degraded_schedules.lock().await.remove(id);
        Ok(())
    }

    pub async fn sessions(
        &self,
        sched_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, Session)>, SchedulerError> {
        let all_sessions = self
            .session_manager
            .list_sessions()
            .await
            .map_err(|e| SchedulerError::StorageError(io::Error::other(e)))?;

        let mut schedule_sessions: Vec<(String, Session)> = all_sessions
            .into_iter()
            .filter(|s| s.schedule_id.as_deref() == Some(sched_id))
            .map(|s| (s.id.clone(), s))
            .collect();

        schedule_sessions.sort_by_key(|(_, session)| std::cmp::Reverse(session.created_at));
        schedule_sessions.truncate(limit);

        Ok(schedule_sessions)
    }

    pub async fn run_now(&self, sched_id: &str) -> Result<String, SchedulerError> {
        let mutation = self.mutation_lock.lock().await;
        self.retry_pending_callback_cleanup().await?;
        let job_to_verify = {
            let jobs_guard = self.jobs.lock().await;
            jobs_guard
                .get(sched_id)
                .map(|(_, job)| job.clone())
                .ok_or_else(|| SchedulerError::JobNotFound(sched_id.to_string()))?
        };
        self.verify_job_recipe_provenance(&job_to_verify).await?;
        let job_to_run = {
            let mut jobs_guard = self.jobs.lock().await;
            let (_, job) = jobs_guard
                .get_mut(sched_id)
                .ok_or_else(|| SchedulerError::JobNotFound(sched_id.to_string()))?;
            if job.currently_running {
                return Err(SchedulerError::AnyhowError(anyhow!(
                    "Job '{}' is already running",
                    sched_id
                )));
            }
            job.currently_running = true;
            job.process_start_time = Some(Utc::now());
            job.clone()
        };

        if let Err(error) = persist_jobs(&self.storage_path, &self.jobs).await {
            if let Some((_, job)) = self.jobs.lock().await.get_mut(sched_id) {
                clear_running_state(job);
            }
            self.degraded_schedules.lock().await.insert(
                sched_id.to_string(),
                format!("run-now reservation persistence failed: {error}"),
            );
            return Err(error);
        }

        let cancel_token = CancellationToken::new();
        {
            let mut tasks = self.running_tasks.lock().await;
            tasks.insert(sched_id.to_string(), cancel_token.clone());
        }
        drop(mutation);

        let result = execute_job(
            job_to_run,
            self.jobs.clone(),
            sched_id.to_string(),
            cancel_token.clone(),
            self.session_manager.clone(),
            self.mutation_lock.clone(),
            self.storage_path.clone(),
        )
        .await;
        let was_cancelled = cancel_token.is_cancelled();

        let _mutation = self.mutation_lock.lock().await;
        {
            let mut tasks = self.running_tasks.lock().await;
            tasks.remove(sched_id);
        }

        {
            let mut jobs_guard = self.jobs.lock().await;
            if let Some((_, job)) = jobs_guard.get_mut(sched_id) {
                job.currently_running = false;
                job.current_session_id = None;
                job.process_start_time = None;
                job.last_run = Some(Utc::now());
            }
        }

        if let Err(error) = persist_jobs(&self.storage_path, &self.jobs).await {
            self.degraded_schedules.lock().await.insert(
                sched_id.to_string(),
                format!("run-now completion persistence failed: {error}"),
            );
            return Err(error);
        }
        self.degraded_schedules.lock().await.remove(sched_id);

        match result {
            _ if was_cancelled => Err(SchedulerError::AnyhowError(anyhow!(
                "Job '{}' was successfully cancelled",
                sched_id
            ))),
            Ok(session_id) => Ok(session_id),
            Err(error) => {
                if let Some(provenance) = error.downcast_ref::<ExtensionProvenanceError>() {
                    return Err(match provenance {
                        ExtensionProvenanceError::SecurityRejected => {
                            SchedulerError::SecurityRejected
                        }
                        ExtensionProvenanceError::RepositoryUnavailable => {
                            SchedulerError::RepositoryUnavailable
                        }
                    });
                }
                Err(SchedulerError::AnyhowError(anyhow!(
                    "Job '{}' failed: {}",
                    sched_id,
                    error
                )))
            }
        }
    }

    pub async fn pause_schedule(&self, sched_id: &str) -> Result<(), SchedulerError> {
        let _mutation = self.mutation_lock.lock().await;
        self.retry_pending_callback_cleanup().await?;
        let mut updated = {
            let jobs_guard = self.jobs.lock().await;
            let (_, job) = jobs_guard
                .get(sched_id)
                .ok_or_else(|| SchedulerError::JobNotFound(sched_id.to_string()))?;
            if job.currently_running {
                return Err(SchedulerError::AnyhowError(anyhow!(
                    "Cannot pause running schedule '{}'",
                    sched_id
                )));
            }
            job.clone()
        };
        updated.paused = true;
        let mut persisted: Vec<ScheduledJob> = self
            .jobs
            .lock()
            .await
            .values()
            .map(|(_, job)| {
                if job.id == sched_id {
                    updated.clone()
                } else {
                    job.clone()
                }
            })
            .collect();
        persisted.sort_by(|left, right| left.id.cmp(&right.id));
        persist_job_list(&self.storage_path, &persisted)?;
        if let Some((_, job)) = self.jobs.lock().await.get_mut(sched_id) {
            *job = updated;
        }
        Ok(())
    }

    pub async fn unpause_schedule(&self, sched_id: &str) -> Result<(), SchedulerError> {
        let _mutation = self.mutation_lock.lock().await;
        self.retry_pending_callback_cleanup().await?;
        let mut updated = self
            .jobs
            .lock()
            .await
            .get(sched_id)
            .map(|(_, job)| job.clone())
            .ok_or_else(|| SchedulerError::JobNotFound(sched_id.to_string()))?;
        updated.paused = false;
        let mut persisted: Vec<ScheduledJob> = self
            .jobs
            .lock()
            .await
            .values()
            .map(|(_, job)| {
                if job.id == sched_id {
                    updated.clone()
                } else {
                    job.clone()
                }
            })
            .collect();
        persisted.sort_by(|left, right| left.id.cmp(&right.id));
        persist_job_list(&self.storage_path, &persisted)?;
        if let Some((_, job)) = self.jobs.lock().await.get_mut(sched_id) {
            *job = updated;
        }
        Ok(())
    }

    pub async fn update_schedule(
        &self,
        sched_id: &str,
        new_cron: String,
    ) -> Result<(), SchedulerError> {
        let _mutation = self.mutation_lock.lock().await;
        self.retry_pending_callback_cleanup().await?;
        let job_to_verify = {
            let jobs_guard = self.jobs.lock().await;
            jobs_guard
                .get(sched_id)
                .map(|(_, job)| job.clone())
                .ok_or_else(|| SchedulerError::JobNotFound(sched_id.to_string()))?
        };
        self.verify_job_recipe_provenance(&job_to_verify).await?;
        let (old_uuid, old_job, updated_job) = {
            let jobs_guard = self.jobs.lock().await;
            match jobs_guard.get(sched_id) {
                Some((uuid, job)) => {
                    if job.currently_running {
                        return Err(SchedulerError::AnyhowError(anyhow!(
                            "Cannot update running schedule '{}'",
                            sched_id
                        )));
                    }
                    if new_cron == job.cron {
                        return Ok(());
                    }
                    let mut updated = job.clone();
                    updated.cron = new_cron;
                    (*uuid, job.clone(), updated)
                }
                None => return Err(SchedulerError::JobNotFound(sched_id.to_string())),
            }
        };

        let old_generation = self
            .callback_generations
            .lock()
            .await
            .get(sched_id)
            .cloned()
            .ok_or(SchedulerError::SecurityRejected)?;
        let (new_task, new_generation) = self.create_cron_task(updated_job.clone())?;
        let new_uuid = self
            .tokio_scheduler
            .add(new_task)
            .await
            .map_err(|error| SchedulerError::SchedulerInternalError(error.to_string()))?;
        self.jobs
            .lock()
            .await
            .insert(sched_id.to_string(), (new_uuid, updated_job));
        self.callback_generations
            .lock()
            .await
            .insert(sched_id.to_string(), new_generation);
        if let Err(error) = persist_jobs(&self.storage_path, &self.jobs).await {
            self.jobs
                .lock()
                .await
                .insert(sched_id.to_string(), (old_uuid, old_job));
            self.callback_generations
                .lock()
                .await
                .insert(sched_id.to_string(), old_generation.clone());
            if let Err(cleanup_error) = self.tokio_scheduler.remove(&new_uuid).await {
                self.pending_callback_cleanup
                    .lock()
                    .await
                    .insert(new_uuid, sched_id.to_string());
                self.degraded_schedules.lock().await.insert(
                    sched_id.to_string(),
                    format!("replacement persistence failed and callback cleanup is pending: {cleanup_error}"),
                );
                return Err(SchedulerError::SchedulerInternalError(format!(
                    "{error}; replacement callback cleanup failed: {cleanup_error}"
                )));
            }
            return Err(error);
        }
        if let Err(error) = self.tokio_scheduler.remove(&old_uuid).await {
            self.jobs
                .lock()
                .await
                .insert(sched_id.to_string(), (old_uuid, old_job));
            self.callback_generations
                .lock()
                .await
                .insert(sched_id.to_string(), old_generation);
            let rollback_persist = persist_jobs(&self.storage_path, &self.jobs).await;
            let cleanup = self.tokio_scheduler.remove(&new_uuid).await;
            if let Err(cleanup_error) = &cleanup {
                self.pending_callback_cleanup
                    .lock()
                    .await
                    .insert(new_uuid, sched_id.to_string());
                self.degraded_schedules.lock().await.insert(
                    sched_id.to_string(),
                    format!("old callback removal failed and replacement cleanup is pending: {cleanup_error}"),
                );
                return Err(SchedulerError::SchedulerInternalError(format!(
                    "failed to remove old callback: {error}; replacement cleanup failed: {cleanup_error}"
                )));
            }
            if let Err(rollback_error) = rollback_persist {
                self.callback_generations.lock().await.remove(sched_id);
                self.degraded_schedules.lock().await.insert(
                    sched_id.to_string(),
                    format!(
                        "old callback removal failed and storage rollback failed: {rollback_error}"
                    ),
                );
                return Err(SchedulerError::SchedulerInternalError(format!(
                    "failed to remove old callback: {error}; storage rollback failed: {rollback_error}"
                )));
            }
            return Err(SchedulerError::SchedulerInternalError(error.to_string()));
        }
        self.degraded_schedules.lock().await.remove(sched_id);
        Ok(())
    }

    pub async fn kill_running_job(&self, sched_id: &str) -> Result<(), SchedulerError> {
        let _mutation = self.mutation_lock.lock().await;
        self.retry_pending_callback_cleanup().await?;
        let mut updated = {
            let jobs_guard = self.jobs.lock().await;
            match jobs_guard.get(sched_id) {
                Some((_, job)) if !job.currently_running => {
                    return Err(SchedulerError::AnyhowError(anyhow!(
                        "Schedule '{}' is not running",
                        sched_id
                    )));
                }
                None => return Err(SchedulerError::JobNotFound(sched_id.to_string())),
                Some((_, job)) => job.clone(),
            }
        };
        clear_running_state(&mut updated);
        let mut persisted: Vec<ScheduledJob> = self
            .jobs
            .lock()
            .await
            .values()
            .map(|(_, job)| {
                if job.id == sched_id {
                    updated.clone()
                } else {
                    job.clone()
                }
            })
            .collect();
        persisted.sort_by(|left, right| left.id.cmp(&right.id));
        persist_job_list(&self.storage_path, &persisted)?;

        let token = {
            let mut tasks = self.running_tasks.lock().await;
            tasks.remove(sched_id)
        };
        if let Some(token) = token {
            token.cancel();
        }

        if let Some((_, job)) = self.jobs.lock().await.get_mut(sched_id) {
            *job = updated;
        }
        Ok(())
    }

    pub async fn get_running_job_info(
        &self,
        sched_id: &str,
    ) -> Result<Option<(String, DateTime<Utc>)>, SchedulerError> {
        let jobs_guard = self.jobs.lock().await;
        match jobs_guard.get(sched_id) {
            Some((_, job)) if job.currently_running => {
                match (&job.current_session_id, &job.process_start_time) {
                    (Some(sid), Some(start)) => Ok(Some((sid.clone(), *start))),
                    _ => Ok(None),
                }
            }
            Some(_) => Ok(None),
            None => Err(SchedulerError::JobNotFound(sched_id.to_string())),
        }
    }

    async fn verify_job_recipe_provenance(&self, job: &ScheduledJob) -> Result<(), SchedulerError> {
        verify_scheduled_job_recipe_provenance(self.session_manager.as_ref(), job).await
    }
}

async fn verify_scheduled_job_recipe_provenance(
    session_manager: &SessionManager,
    job: &ScheduledJob,
) -> Result<(), SchedulerError> {
    if job.source.is_empty() {
        return Ok(());
    }
    let recipe = load_scheduled_recipe(job)
        .map_err(|_| SchedulerError::RecipeLoadError("unable to load recipe".to_string()))?;
    session_manager
        .verify_recipe_extension_provenance(recipe.extensions.as_deref())
        .await
        .map_err(|error| match error {
            ExtensionProvenanceError::SecurityRejected => SchedulerError::SecurityRejected,
            ExtensionProvenanceError::RepositoryUnavailable => {
                SchedulerError::RepositoryUnavailable
            }
        })
}

#[allow(clippy::too_many_lines)]
async fn execute_job(
    job: ScheduledJob,
    jobs: Arc<Mutex<JobsMap>>,
    job_id: String,
    cancel_token: CancellationToken,
    session_manager: Arc<SessionManager>,
    mutation_lock: Arc<Mutex<()>>,
    storage_path: PathBuf,
) -> Result<String> {
    if job.source.is_empty() {
        return Ok(job.id.to_string());
    }

    let recipe_path = Path::new(&job.source);
    let recipe = load_scheduled_recipe(&job)?;
    session_manager
        .verify_recipe_extension_provenance(recipe.extensions.as_deref())
        .await
        .map_err(anyhow::Error::new)?;

    let agent = Agent::new();

    let config = Config::global();
    let provider_name = config.get_lumina_provider()?;
    let model_name = config.get_lumina_model()?;
    let model_config =
        crate::model_config::model_config_from_user_config(&provider_name, &model_name)?;

    let session = agent
        .config
        .session_manager
        .create_session(
            std::env::current_dir()?,
            format!("Scheduled job: {}", job.id),
            SessionType::Scheduled,
            agent.config.lumina_mode,
        )
        .await?;

    let mut extensions = resolve_extensions_for_new_session(recipe.extensions.as_deref(), None);
    if recipe.extensions.is_none() {
        extensions.extend(crate::plugins::mcp_servers::enabled_plugin_mcp_servers(
            std::env::current_dir().ok().as_deref(),
        ));
    }
    session_manager
        .verify_recipe_extension_provenance(Some(&extensions))
        .await
        .map_err(anyhow::Error::new)?;
    for ext in &extensions {
        agent.add_extension(ext.clone(), &session.id).await?;
    }

    let agent_provider = create(&provider_name, extensions).await?;
    agent
        .update_provider(agent_provider, model_config, &session.id)
        .await?;

    let _mutation = mutation_lock.lock().await;
    {
        let mut jobs_guard = jobs.lock().await;
        if let Some((_, job_def)) = jobs_guard.get_mut(job_id.as_str()) {
            job_def.current_session_id = Some(session.id.clone());
        }
    }
    if let Err(error) = persist_jobs(&storage_path, &jobs).await {
        if let Some((_, job_def)) = jobs.lock().await.get_mut(job_id.as_str()) {
            job_def.current_session_id = None;
        }
        return Err(error.into());
    }
    drop(_mutation);

    let start_time = std::time::Instant::now();

    let recipe_display_name = recipe_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&job.id);
    let recipe_version = recipe.version.clone();

    tracing::info!(
        monotonic_counter.lumina.session_starts = 1,
        session_type = "schedule",
        interface = "scheduler",
        interactive = false,
        "Scheduled session started"
    );

    tracing::info!(
        monotonic_counter.lumina.recipe_runs = 1,
        recipe_name = %recipe_display_name,
        recipe_version = %recipe_version,
        session_type = "schedule",
        interface = "scheduler",
        "Recipe execution started"
    );

    let prompt_text = recipe
        .prompt
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            recipe
                .instructions
                .as_deref()
                .filter(|s| !s.trim().is_empty())
        })
        .ok_or_else(|| {
            anyhow!("Recipe must specify at least one of `instructions` or `prompt`.")
        })?;

    let user_message = Message::user().with_text(prompt_text);
    let mut conversation = Conversation::new_unvalidated(vec![user_message.clone()]);

    let session_config = SessionConfig {
        id: session.id.clone(),
        schedule_id: Some(job.id.clone()),
        max_turns: None,
        retry_config: None,
    };

    let stream = agent
        .reply(user_message, session_config, Some(cancel_token))
        .await?;

    use futures::StreamExt;
    let mut stream = std::pin::pin!(stream);

    let mut stream_error = false;
    while let Some(message_result) = stream.next().await {
        tokio::task::yield_now().await;

        match message_result {
            Ok(AgentEvent::Message(msg)) => {
                conversation.push(msg);
            }
            Ok(AgentEvent::HistoryReplaced(updated)) => {
                conversation = updated;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!("Error in agent stream: {}", e);
                stream_error = true;
                break;
            }
        }
    }

    agent
        .config
        .session_manager
        .update(&session.id)
        .schedule_id(Some(job.id.clone()))
        .recipe(Some(recipe))
        .apply()
        .await?;

    {
        let session_duration = start_time.elapsed();
        let exit_type = if stream_error { "error" } else { "normal" };
        let (total_tokens, message_count) = agent
            .config
            .session_manager
            .get_session(&session.id, false)
            .await
            .map(|s| (s.usage.total_tokens.unwrap_or(0), s.message_count))
            .unwrap_or((0, 0));

        tracing::info!(
            monotonic_counter.lumina.session_completions = 1,
            session_type = "schedule",
            interface = "scheduler",
            exit_type,
            duration_ms = session_duration.as_millis() as u64,
            total_tokens,
            message_count,
            "Session completed"
        );

        tracing::info!(
            monotonic_counter.lumina.session_duration_ms = session_duration.as_millis() as u64,
            session_type = "schedule",
            interface = "scheduler",
            "Session duration"
        );

        if total_tokens > 0 {
            tracing::info!(
                monotonic_counter.lumina.session_tokens = total_tokens,
                session_type = "schedule",
                interface = "scheduler",
                "Session tokens"
            );
        }
    }

    Ok(session.id)
}

fn load_scheduled_recipe(job: &ScheduledJob) -> Result<Recipe> {
    let recipe_path = Path::new(&job.source);
    let recipe_content = fs::read_to_string(recipe_path)?;
    let recipe_dir_owned;
    let recipe_dir = if let Some(ref base) = job.recipe_base_dir {
        recipe_dir_owned = PathBuf::from(base);
        recipe_dir_owned.as_path()
    } else {
        recipe_path.parent().unwrap_or(Path::new("."))
    };
    build_recipe_from_template(
        recipe_content,
        recipe_dir,
        job.parameters.clone(),
        None::<fn(&str, &str) -> anyhow::Result<String>>,
    )
    .map_err(|error| anyhow!(error.to_string()))
}

#[async_trait]
impl SchedulerTrait for Scheduler {
    async fn add_scheduled_job(
        &self,
        job: ScheduledJob,
        make_copy: bool,
    ) -> Result<(), SchedulerError> {
        self.add_scheduled_job(job, make_copy).await
    }

    async fn add_scheduled_job_from_content(
        &self,
        job: ScheduledJob,
        recipe_content: &[u8],
    ) -> Result<ScheduledJob, SchedulerError> {
        self.add_scheduled_job_from_content(job, recipe_content)
            .await
    }

    async fn schedule_recipe(
        &self,
        recipe_path: PathBuf,
        cron_schedule: Option<String>,
    ) -> Result<(), SchedulerError> {
        self.schedule_recipe(recipe_path, cron_schedule).await
    }

    async fn list_scheduled_jobs(&self) -> Vec<ScheduledJob> {
        self.list_scheduled_jobs().await
    }

    async fn remove_scheduled_job(
        &self,
        id: &str,
        remove_recipe: bool,
    ) -> Result<(), SchedulerError> {
        self.remove_scheduled_job(id, remove_recipe).await
    }

    async fn pause_schedule(&self, id: &str) -> Result<(), SchedulerError> {
        self.pause_schedule(id).await
    }

    async fn unpause_schedule(&self, id: &str) -> Result<(), SchedulerError> {
        self.unpause_schedule(id).await
    }

    async fn run_now(&self, id: &str) -> Result<String, SchedulerError> {
        self.run_now(id).await
    }

    async fn sessions(
        &self,
        sched_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, Session)>, SchedulerError> {
        self.sessions(sched_id, limit).await
    }

    async fn update_schedule(
        &self,
        sched_id: &str,
        new_cron: String,
    ) -> Result<(), SchedulerError> {
        self.update_schedule(sched_id, new_cron).await
    }

    async fn kill_running_job(&self, sched_id: &str) -> Result<(), SchedulerError> {
        self.kill_running_job(sched_id).await
    }

    async fn get_running_job_info(
        &self,
        sched_id: &str,
    ) -> Result<Option<(String, DateTime<Utc>)>, SchedulerError> {
        self.get_running_job_info(sched_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::session_manager::{
        ManagedExtensionProvenance, SessionExtensionProvenanceVerifier,
    };
    use tempfile::tempdir;
    use tokio::time::{sleep, Duration};

    struct StaticProvenanceVerifier(Option<ManagedExtensionProvenance>);

    #[async_trait]
    impl SessionExtensionProvenanceVerifier for StaticProvenanceVerifier {
        async fn verified_provenance(&self) -> Result<ManagedExtensionProvenance> {
            self.0
                .clone()
                .ok_or_else(|| anyhow!("test provenance unavailable"))
        }
    }

    fn managed_extension(name: &str) -> crate::agents::ExtensionConfig {
        crate::agents::ExtensionConfig::Stdio {
            name: name.to_string(),
            description: "managed".to_string(),
            cmd: "managed-scheduler-command".to_string(),
            args: vec!["--managed".to_string()],
            envs: crate::agents::extension::Envs::default(),
            env_keys: Vec::new(),
            timeout: Some(30),
            cwd: None,
            bundled: None,
            available_tools: Vec::new(),
        }
    }

    fn managed_provenance() -> ManagedExtensionProvenance {
        let extension = managed_extension("managed-canonical");
        ManagedExtensionProvenance::with_evidence(
            std::collections::HashSet::from(["managed-canonical".to_string()]),
            std::collections::HashSet::from([crate::mcp_platform::extension_source_fingerprint(
                &extension,
            )
            .unwrap()]),
            std::collections::HashSet::new(),
        )
    }

    fn create_test_recipe(dir: &Path, name: &str) -> PathBuf {
        let recipe_path = dir.join(format!("{}.yaml", name));
        fs::write(&recipe_path, "prompt: test\n").unwrap();
        recipe_path
    }

    fn scheduled_job(id: &str, recipe_path: &Path) -> ScheduledJob {
        ScheduledJob {
            id: id.to_string(),
            source: recipe_path.to_string_lossy().to_string(),
            cron: "0 0 0 1 1 *".to_string(),
            last_run: None,
            currently_running: false,
            paused: false,
            current_session_id: None,
            process_start_time: None,
            parameters: vec![],
            recipe_base_dir: None,
        }
    }

    #[tokio::test]
    async fn scheduler_rejects_managed_alias_on_create_update_and_runtime() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = temp_dir.path().join("managed-alias.yaml");
        fs::write(
            &recipe_path,
            "prompt: test\nextensions:\n  - type: stdio\n    name: history-alias\n    description: alias\n    cmd: managed-scheduler-command\n    args: [--managed]\n    timeout: 30\n",
        )
        .unwrap();
        let verifier = Arc::new(StaticProvenanceVerifier(Some(managed_provenance())));
        let session_manager = Arc::new(SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            verifier,
        ));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();
        let job = scheduled_job("managed-alias", &recipe_path);

        assert!(scheduler
            .add_scheduled_job(job.clone(), false)
            .await
            .is_err());
        assert!(scheduler.list_scheduled_jobs().await.is_empty());
        assert!(!storage_path.exists());

        fs::write(
            &storage_path,
            serde_json::to_string_pretty(&vec![job]).unwrap(),
        )
        .unwrap();
        let verifier = Arc::new(StaticProvenanceVerifier(Some(managed_provenance())));
        let session_manager = Arc::new(SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            verifier,
        ));
        let scheduler = Scheduler::new(storage_path, session_manager).await.unwrap();
        assert!(scheduler
            .update_schedule("managed-alias", "0 1 0 1 1 *".to_string())
            .await
            .is_err());
        assert!(scheduler.run_now("managed-alias").await.is_err());
        let job = scheduler
            .list_scheduled_jobs()
            .await
            .into_iter()
            .find(|job| job.id == "managed-alias")
            .unwrap();
        assert_eq!(job.cron, "0 0 0 1 1 *");
        assert!(!job.currently_running);
    }

    #[tokio::test]
    async fn historical_managed_schedule_rejection_does_not_rewrite_running_state() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = temp_dir.path().join("historical-managed.yaml");
        fs::write(
            &recipe_path,
            "prompt: test\nextensions:\n  - type: stdio\n    name: historical-alias\n    description: alias\n    cmd: managed-scheduler-command\n    args: [--managed]\n    timeout: 30\n",
        )
        .unwrap();
        let mut job = scheduled_job("historical-managed", &recipe_path);
        job.currently_running = true;
        job.current_session_id = Some("unverified-session".to_string());
        job.process_start_time = Some(Utc::now());
        let original = serde_json::to_string_pretty(&vec![job]).unwrap();
        fs::write(&storage_path, &original).unwrap();

        let verifier = Arc::new(StaticProvenanceVerifier(Some(managed_provenance())));
        let session_manager = Arc::new(SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            verifier,
        ));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();

        assert!(scheduler.list_scheduled_jobs().await.is_empty());
        assert_eq!(fs::read_to_string(storage_path).unwrap(), original);
    }

    async fn assert_duplicate_mixed_safety_is_rejected(unsafe_first: bool) {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let plain_recipe = create_test_recipe(temp_dir.path(), "duplicate-plain");
        let managed_recipe = temp_dir.path().join("duplicate-managed.yaml");
        fs::write(
            &managed_recipe,
            "prompt: test\nextensions:\n  - type: stdio\n    name: duplicate-alias\n    description: alias\n    cmd: managed-scheduler-command\n    args: [--managed]\n    timeout: 30\n",
        )
        .unwrap();
        let mut plain = scheduled_job("duplicate", &plain_recipe);
        plain.currently_running = true;
        plain.current_session_id = Some("must-not-be-cleared".to_string());
        plain.process_start_time = Some(Utc::now());
        let managed = scheduled_job("duplicate", &managed_recipe);
        let jobs = if unsafe_first {
            vec![managed, plain]
        } else {
            vec![plain, managed]
        };
        let original = serde_json::to_string_pretty(&jobs).unwrap();
        fs::write(&storage_path, &original).unwrap();

        let verifier = Arc::new(StaticProvenanceVerifier(Some(managed_provenance())));
        let session_manager = Arc::new(SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            verifier,
        ));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();

        assert!(scheduler.list_scheduled_jobs().await.is_empty());
        assert_eq!(fs::read_to_string(storage_path).unwrap(), original);
    }

    #[tokio::test]
    async fn duplicate_id_does_not_let_safe_record_authorize_unsafe_sibling() {
        assert_duplicate_mixed_safety_is_rejected(false).await;
    }

    #[tokio::test]
    async fn duplicate_id_rejection_is_independent_of_record_order() {
        assert_duplicate_mixed_safety_is_rejected(true).await;
    }

    #[tokio::test]
    async fn invalid_cron_update_preserves_old_callback_and_job() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "atomic-update");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();
        scheduler
            .add_scheduled_job(scheduled_job("atomic-update", &recipe_path), false)
            .await
            .unwrap();
        let old_uuid = scheduler.jobs.lock().await["atomic-update"].0;

        let error = scheduler
            .update_schedule("atomic-update", "invalid cron".to_string())
            .await
            .unwrap_err();

        assert!(matches!(error, SchedulerError::CronParseError(_)));
        let jobs = scheduler.jobs.lock().await;
        assert_eq!(jobs["atomic-update"].0, old_uuid);
        assert_eq!(jobs["atomic-update"].1.cron, "0 0 0 1 1 *");
        drop(jobs);
        let persisted: Vec<ScheduledJob> =
            serde_json::from_str(&fs::read_to_string(storage_path).unwrap()).unwrap();
        assert_eq!(persisted[0].cron, "0 0 0 1 1 *");
    }

    #[tokio::test]
    async fn create_and_list_share_the_full_storage_and_callback_mutation_lock() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "create-list-race");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();
        let blocker = scheduler.mutation_lock.lock().await;
        let create_scheduler = scheduler.clone();
        let create_job = scheduled_job("create-list-race", &recipe_path);
        let create =
            tokio::spawn(
                async move { create_scheduler.add_scheduled_job(create_job, false).await },
            );
        let list_scheduler = scheduler.clone();
        let list = tokio::spawn(async move { list_scheduler.list_scheduled_jobs().await });
        tokio::task::yield_now().await;
        drop(blocker);
        create.await.unwrap().unwrap();
        let _ = list.await.unwrap();

        let final_jobs = scheduler.list_scheduled_jobs().await;
        assert_eq!(final_jobs.len(), 1);
        assert_eq!(final_jobs[0].id, "create-list-race");
        assert_eq!(scheduler.jobs.lock().await.len(), 1);
        assert!(scheduler.pending_callback_cleanup.lock().await.is_empty());
        let persisted: Vec<ScheduledJob> =
            serde_json::from_str(&fs::read_to_string(storage_path).unwrap()).unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].id, "create-list-race");
    }

    #[tokio::test]
    async fn atomic_replace_failure_preserves_storage_memory_and_callbacks_on_update() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "replace-failure");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();
        scheduler
            .add_scheduled_job(scheduled_job("replace-failure", &recipe_path), false)
            .await
            .unwrap();
        let original_storage = fs::read(&storage_path).unwrap();
        let original_uuid = scheduler.jobs.lock().await["replace-failure"].0;

        fail_next_atomic_replace(&storage_path);
        let error = scheduler
            .update_schedule("replace-failure", "0 1 0 1 1 *".to_string())
            .await
            .unwrap_err();

        assert!(matches!(error, SchedulerError::StorageError(_)));
        assert_eq!(fs::read(&storage_path).unwrap(), original_storage);
        let jobs = scheduler.jobs.lock().await;
        assert_eq!(jobs["replace-failure"].0, original_uuid);
        assert_eq!(jobs["replace-failure"].1.cron, "0 0 0 1 1 *");
        drop(jobs);
        assert!(scheduler.pending_callback_cleanup.lock().await.is_empty());
    }

    #[tokio::test]
    async fn callback_remove_failure_does_not_retain_a_fabricated_handle() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "remove-failure");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();
        scheduler
            .add_scheduled_job(scheduled_job("remove-failure", &recipe_path), false)
            .await
            .unwrap();
        let uuid = scheduler.jobs.lock().await["remove-failure"].0;
        scheduler.tokio_scheduler.remove(&uuid).await.unwrap();

        let error = scheduler
            .remove_scheduled_job("remove-failure", false)
            .await
            .unwrap_err();

        assert!(matches!(error, SchedulerError::SchedulerInternalError(_)));
        assert!(!scheduler.jobs.lock().await.contains_key("remove-failure"));
        assert!(!scheduler
            .callback_generations
            .lock()
            .await
            .contains_key("remove-failure"));
        assert!(scheduler
            .pending_callback_cleanup
            .lock()
            .await
            .contains_key(&uuid));
        assert!(scheduler
            .degraded_schedule_states()
            .await
            .iter()
            .any(|(id, _)| id == "remove-failure"));
        let persisted: Vec<ScheduledJob> =
            serde_json::from_str(&fs::read_to_string(storage_path).unwrap()).unwrap();
        assert!(persisted.is_empty());
    }

    #[tokio::test]
    async fn content_create_rolls_back_file_and_reservation_on_cron_failure() {
        let temp_dir = tempdir().unwrap();
        let root = temp_dir.path().to_string_lossy().into_owned();
        let _guard = env_lock::lock_env([("LUMINA_PATH_ROOT", Some(root.as_str()))]);
        let storage_path = temp_dir.path().join("schedule.json");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path, session_manager).await.unwrap();
        let mut job = scheduled_job("atomic-content", Path::new("unused"));
        job.source.clear();
        job.cron = "invalid cron".to_string();

        assert!(scheduler
            .add_scheduled_job_from_content(job, b"prompt: test\n")
            .await
            .is_err());
        assert!(scheduler.jobs.lock().await.is_empty());
        assert!(scheduler.reservations.lock().await.is_empty());
        assert!(!Paths::data_dir()
            .join("scheduled_recipes")
            .join("atomic-content.yaml")
            .exists());
    }

    #[tokio::test]
    async fn concurrent_content_create_never_overwrites_winner() {
        let temp_dir = tempdir().unwrap();
        let root = temp_dir.path().to_string_lossy().into_owned();
        let _guard = env_lock::lock_env([("LUMINA_PATH_ROOT", Some(root.as_str()))]);
        let storage_path = temp_dir.path().join("schedule.json");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path, session_manager).await.unwrap();
        let mut first = scheduled_job("same-id", Path::new("unused"));
        first.source.clear();
        let second = first.clone();

        let (left, right) = tokio::join!(
            scheduler.add_scheduled_job_from_content(first, b"prompt: first\n"),
            scheduler.add_scheduled_job_from_content(second, b"prompt: second\n")
        );

        assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
        let loser = if left.is_err() { left } else { right };
        assert!(matches!(loser, Err(SchedulerError::JobIdExists(_))));
        assert_eq!(scheduler.jobs.lock().await.len(), 1);
        assert!(scheduler.reservations.lock().await.is_empty());
        let content = fs::read_to_string(
            Paths::data_dir()
                .join("scheduled_recipes")
                .join("same-id.yaml"),
        )
        .unwrap();
        assert!(content == "prompt: first\n" || content == "prompt: second\n");
    }

    #[tokio::test]
    async fn cron_revalidates_before_reserving_or_persisting_runtime_state() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "cron-guard");
        let verifier = Arc::new(StaticProvenanceVerifier(Some(managed_provenance())));
        let session_manager = Arc::new(SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            verifier,
        ));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();
        let mut job = scheduled_job("cron-guard", &recipe_path);
        job.cron = "* * * * * *".to_string();
        scheduler.add_scheduled_job(job, false).await.unwrap();

        fs::write(
            &recipe_path,
            "prompt: test\nextensions:\n  - type: stdio\n    name: cron-alias\n    description: alias\n    cmd: managed-scheduler-command\n    args: [--managed]\n    timeout: 30\n",
        )
        .unwrap();
        sleep(Duration::from_millis(1500)).await;

        let jobs = scheduler.list_scheduled_jobs().await;
        let job = jobs.iter().find(|job| job.id == "cron-guard").unwrap();
        assert!(job.last_run.is_none());
        assert!(!job.currently_running);
        let persisted: Vec<ScheduledJob> =
            serde_json::from_str(&fs::read_to_string(storage_path).unwrap()).unwrap();
        assert!(persisted[0].last_run.is_none());
        assert!(!persisted[0].currently_running);
    }

    #[tokio::test]
    async fn scheduler_provenance_db_behavior_preserves_ordinary_recipes() {
        let temp_dir = tempdir().unwrap();
        let unavailable = Arc::new(StaticProvenanceVerifier(None));
        let session_manager = Arc::new(SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            unavailable.clone(),
        ));
        let scheduler =
            Scheduler::new(temp_dir.path().join("plain-schedule.json"), session_manager)
                .await
                .unwrap();
        let plain_recipe = create_test_recipe(temp_dir.path(), "plain");
        scheduler
            .add_scheduled_job(scheduled_job("plain", &plain_recipe), false)
            .await
            .unwrap();

        let extension_recipe = temp_dir.path().join("ordinary-extension.yaml");
        fs::write(
            &extension_recipe,
            "prompt: test\nextensions:\n  - type: builtin\n    name: ordinary\n    description: ordinary\n    display_name: Ordinary\n",
        )
        .unwrap();
        assert!(scheduler
            .add_scheduled_job(
                scheduled_job("ordinary-unavailable", &extension_recipe),
                false,
            )
            .await
            .is_err());

        let available = Arc::new(StaticProvenanceVerifier(Some(managed_provenance())));
        let session_manager = Arc::new(SessionManager::new_with_provenance_verifier(
            temp_dir.path().to_path_buf(),
            available,
        ));
        let scheduler = Scheduler::new(
            temp_dir.path().join("ordinary-schedule.json"),
            session_manager,
        )
        .await
        .unwrap();
        scheduler
            .add_scheduled_job(scheduled_job("ordinary", &extension_recipe), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_job_runs_on_schedule() {
        let _guard = env_lock::lock_env([
            ("LUMINA_PROVIDER", Some("openai")),
            ("LUMINA_MODEL", Some("gpt-4o")),
            ("OPENAI_API_KEY", Some("fake-openai-no-keyring")),
            ("OPENAI_CUSTOM_HEADERS", Some("")),
        ]);
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "scheduled_job");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path, session_manager).await.unwrap();

        let job = ScheduledJob {
            id: "scheduled_job".to_string(),
            source: recipe_path.to_string_lossy().to_string(),
            cron: "* * * * * *".to_string(),
            last_run: None,
            currently_running: false,
            paused: false,
            current_session_id: None,
            process_start_time: None,
            parameters: vec![],
            recipe_base_dir: None,
        };

        scheduler.add_scheduled_job(job, true).await.unwrap();
        sleep(Duration::from_millis(1500)).await;

        let jobs = scheduler.list_scheduled_jobs().await;
        assert!(jobs[0].last_run.is_some(), "Job should have run");
    }

    #[tokio::test]
    async fn test_paused_job_does_not_run() {
        let _guard = env_lock::lock_env([
            ("LUMINA_PROVIDER", Some("openai")),
            ("LUMINA_MODEL", Some("gpt-4o")),
            ("OPENAI_API_KEY", Some("fake-openai-no-keyring")),
            ("OPENAI_CUSTOM_HEADERS", Some("")),
        ]);
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "paused_job");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path, session_manager).await.unwrap();

        let job = ScheduledJob {
            id: "paused_job".to_string(),
            source: recipe_path.to_string_lossy().to_string(),
            cron: "* * * * * *".to_string(),
            last_run: None,
            currently_running: false,
            paused: false,
            current_session_id: None,
            process_start_time: None,
            parameters: vec![],
            recipe_base_dir: None,
        };

        scheduler.add_scheduled_job(job, true).await.unwrap();
        scheduler.pause_schedule("paused_job").await.unwrap();
        sleep(Duration::from_millis(1500)).await;

        let jobs = scheduler.list_scheduled_jobs().await;
        assert!(jobs[0].last_run.is_none(), "Paused job should not run");
    }

    #[tokio::test]
    async fn test_remove_scheduled_job_respects_recipe_removal_flag() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "recipe_removal_flag_job");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path, session_manager).await.unwrap();

        let job = ScheduledJob {
            id: "recipe_removal_flag_job".to_string(),
            source: recipe_path.to_string_lossy().to_string(),
            cron: "0 0 0 1 1 *".to_string(),
            last_run: None,
            currently_running: false,
            paused: false,
            current_session_id: None,
            process_start_time: None,
            parameters: vec![],
            recipe_base_dir: None,
        };

        scheduler
            .add_scheduled_job(job.clone(), false)
            .await
            .unwrap();
        scheduler
            .remove_scheduled_job("recipe_removal_flag_job", false)
            .await
            .unwrap();
        assert!(
            recipe_path.exists(),
            "Recipe should be kept when remove_recipe is false"
        );

        scheduler.add_scheduled_job(job, false).await.unwrap();
        scheduler
            .remove_scheduled_job("recipe_removal_flag_job", true)
            .await
            .unwrap();
        assert!(
            !recipe_path.exists(),
            "Recipe should be deleted when remove_recipe is true"
        );
    }

    #[tokio::test]
    async fn test_kill_running_job_clears_state_and_persists() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "running_job");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();

        let job = ScheduledJob {
            id: "running_job".to_string(),
            source: recipe_path.to_string_lossy().to_string(),
            cron: "0 0 0 1 1 *".to_string(),
            last_run: None,
            currently_running: false,
            paused: false,
            current_session_id: None,
            process_start_time: None,
            parameters: vec![],
            recipe_base_dir: None,
        };

        scheduler.add_scheduled_job(job, false).await.unwrap();
        {
            let mut jobs_guard = scheduler.jobs.lock().await;
            let (_, job) = jobs_guard.get_mut("running_job").unwrap();
            job.currently_running = true;
            job.current_session_id = Some("session-id".to_string());
            job.process_start_time = Some(Utc::now());
        }
        {
            let mut tasks = scheduler.running_tasks.lock().await;
            tasks.insert("running_job".to_string(), CancellationToken::new());
        }
        persist_jobs(&storage_path, &scheduler.jobs).await.unwrap();

        scheduler.kill_running_job("running_job").await.unwrap();

        let jobs = scheduler.list_scheduled_jobs().await;
        let killed_job = jobs.iter().find(|job| job.id == "running_job").unwrap();
        assert!(!killed_job.currently_running);
        assert!(killed_job.current_session_id.is_none());
        assert!(killed_job.process_start_time.is_none());
        assert!(scheduler.running_tasks.lock().await.is_empty());

        let persisted_jobs: Vec<ScheduledJob> =
            serde_json::from_str(&fs::read_to_string(storage_path).unwrap()).unwrap();
        let persisted_job = persisted_jobs
            .iter()
            .find(|job| job.id == "running_job")
            .unwrap();
        assert!(!persisted_job.currently_running);
        assert!(persisted_job.current_session_id.is_none());
        assert!(persisted_job.process_start_time.is_none());
    }

    #[tokio::test]
    async fn test_load_jobs_from_storage_clears_stale_running_state() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("schedule.json");
        let recipe_path = create_test_recipe(temp_dir.path(), "stale_running_job");
        let started_at = Utc::now();
        let stale_job = ScheduledJob {
            id: "stale_running_job".to_string(),
            source: recipe_path.to_string_lossy().to_string(),
            cron: "0 0 0 1 1 *".to_string(),
            last_run: None,
            currently_running: true,
            paused: false,
            current_session_id: Some("stale-session-id".to_string()),
            process_start_time: Some(started_at),
            parameters: vec![],
            recipe_base_dir: None,
        };
        fs::write(
            &storage_path,
            serde_json::to_string_pretty(&vec![stale_job]).unwrap(),
        )
        .unwrap();

        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path.clone(), session_manager)
            .await
            .unwrap();

        let jobs = scheduler.list_scheduled_jobs().await;
        let loaded_job = jobs
            .iter()
            .find(|job| job.id == "stale_running_job")
            .unwrap();
        assert!(!loaded_job.currently_running);
        assert!(loaded_job.current_session_id.is_none());
        assert!(loaded_job.process_start_time.is_none());

        let persisted_jobs: Vec<ScheduledJob> =
            serde_json::from_str(&fs::read_to_string(storage_path).unwrap()).unwrap();
        let persisted_job = persisted_jobs
            .iter()
            .find(|job| job.id == "stale_running_job")
            .unwrap();
        assert!(!persisted_job.currently_running);
        assert!(persisted_job.current_session_id.is_none());
        assert!(persisted_job.process_start_time.is_none());
    }

    #[tokio::test]
    async fn test_job_with_no_prompt_does_not_panic() {
        let _guard = env_lock::lock_env([
            ("LUMINA_PROVIDER", Some("openai")),
            ("LUMINA_MODEL", Some("gpt-4o")),
            ("OPENAI_API_KEY", Some("fake-openai-no-keyring")),
            ("OPENAI_CUSTOM_HEADERS", Some("")),
        ]);
        let temp_dir = tempdir().unwrap();
        let recipe_path = temp_dir.path().join("no_prompt.yaml");
        fs::write(
            &recipe_path,
            "title: missing\ndescription: no prompt or instructions\n",
        )
        .unwrap();

        let storage_path = temp_dir.path().join("schedule.json");
        let session_manager = Arc::new(SessionManager::new(temp_dir.path().to_path_buf()));
        let scheduler = Scheduler::new(storage_path, session_manager).await.unwrap();

        let job = ScheduledJob {
            id: "no_prompt_job".to_string(),
            source: recipe_path.to_string_lossy().to_string(),
            cron: "* * * * * *".to_string(),
            last_run: None,
            currently_running: false,
            paused: false,
            current_session_id: None,
            process_start_time: None,
            parameters: vec![],
            recipe_base_dir: None,
        };

        // Schedule the job and let it run — should not panic
        scheduler.add_scheduled_job(job, true).await.unwrap();
        sleep(Duration::from_millis(1500)).await;

        // The job should have attempted to run (last_run set) but not crashed the scheduler
        let jobs = scheduler.list_scheduled_jobs().await;
        assert!(
            jobs[0].last_run.is_some(),
            "Job should have attempted to run without panicking"
        );
    }
}
