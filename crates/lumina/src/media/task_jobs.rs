use super::{
    domain::validate_id, files, task_output::native_job_result_id, ImportedTaskOutput,
    MediaAssociation, MediaKind, MediaResultStore, TaskConfigurationRecord,
    TaskConfigurationSnapshot, TaskConfigurationStore,
};
use anyhow::{bail, ensure, Result};
use lumina_providers::local_inference::lifecycle::{
    EffectiveGenerationParameters, Sha256Digest, TaskParameterIdentity,
};
use serde_json::Value;
use sqlx::{Pool, Row, Sqlite};
use std::path::Path;

pub(crate) struct NativeTaskJobs {
    pool: Pool<Sqlite>,
}

pub(crate) enum NativeTaskClaim {
    Execute(NativeTaskAttempt),
    Replay(ImportedTaskOutput),
    Busy,
    Stopped(NativeTaskTerminal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeTaskTerminal {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Deferred,
}
impl NativeTaskTerminal {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Deferred => "deferred",
        }
    }
}

struct CallLock {
    _file: std::fs::File,
    _directory: files::GuardedDirectory,
}

#[cfg(windows)]
fn lock_job(root: &Path, session: &str, job: &str) -> Result<Option<CallLock>> {
    use fs2::FileExt;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let directory = files::GuardedDirectory::open(root, true)?.child("native-task-locks", true)?;
    let name = Sha256Digest::of_serializable(&(session, job))?;
    // All contenders may open this stable file. Reparse points and deletion are disallowed;
    // LockFileEx arbitrates ownership and the OS releases it on process death.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory.path.join(name.as_str()))?;
    files::identity(&file, false)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(CallLock {
            _file: file,
            _directory: directory,
        })),
        Err(error)
            if error.raw_os_error() == Some(33)
                || error.kind() == std::io::ErrorKind::WouldBlock =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(windows))]
fn lock_job(_: &Path, _: &str, _: &str) -> Result<Option<CallLock>> {
    bail!("Guarded native task execution is unavailable on this platform")
}

pub(crate) struct NativeTaskAttempt {
    pool: Pool<Sqlite>,
    session: String,
    job: String,
    request: String,
    _lock: CallLock,
}

impl NativeTaskAttempt {
    pub(crate) async fn finish(self, terminal: NativeTaskTerminal) -> Result<()> {
        let affected = sqlx::query(
            "UPDATE native_task_calls_v1 SET state=?
             WHERE session_id=? AND job_id=? AND request_sha256=? AND state='running'",
        )
        .bind(terminal.as_str())
        .bind(&self.session)
        .bind(&self.job)
        .bind(&self.request)
        .execute(&self.pool)
        .await?
        .rows_affected();
        ensure!(affected == 1, "Native task call state changed while owned");
        Ok(())
    }
}

impl NativeTaskJobs {
    pub(crate) fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
    pub(crate) async fn initialize(&self) -> Result<()> {
        sqlx::query("CREATE TABLE IF NOT EXISTS native_task_calls_v1 (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            job_id TEXT NOT NULL, request_sha256 TEXT NOT NULL CHECK(length(request_sha256)=64),
            state TEXT NOT NULL CHECK(state IN ('running','completed','failed','cancelled','interrupted','deferred')),
            PRIMARY KEY(session_id,job_id))").execute(&self.pool).await?;
        Ok(())
    }

    pub(crate) async fn claim(
        &self,
        root: &Path,
        session: &str,
        call: &str,
        method: &str,
        arguments: &Value,
        identity: &TaskParameterIdentity,
    ) -> Result<NativeTaskClaim> {
        validate_id(session)?;
        let job = Self::job_id(session, call)?;
        let mut canonical = arguments.clone();
        canonical.sort_all_objects();
        let request = Sha256Digest::of_serializable(&("native-task-call-v1", method, canonical))?;
        let root = root.to_owned();
        let scope = (session.to_owned(), job.clone());
        let Some(lock) =
            tokio::task::spawn_blocking(move || lock_job(&root, &scope.0, &scope.1)).await??
        else {
            return Ok(NativeTaskClaim::Busy);
        };
        let previous = sqlx::query(
            "SELECT request_sha256,state FROM native_task_calls_v1 WHERE session_id=? AND job_id=?",
        )
        .bind(session)
        .bind(&job)
        .fetch_optional(&self.pool)
        .await?;
        let previous = previous
            .map(|row| -> Result<String> {
                ensure!(
                    row.try_get::<String, _>("request_sha256")? == request.as_str(),
                    "Tool call already belongs to another native task request"
                );
                Ok(row.try_get("state")?)
            })
            .transpose()?;
        let configurations = TaskConfigurationStore::new(self.pool.clone());
        let config = configurations.read(session, &job).await?;
        if let Some(config) = &config {
            ensure!(
                config.identity() == identity,
                "Persisted native task model identity differs"
            );
            if let Some(output) = self.replay_completed(&configurations, config, call).await? {
                // Rows without a dispatch record predate request fingerprinting. Their immutable
                // output is recoverable, but the original arguments (including omitted seeds)
                // cannot be authenticated, so a retry must not attach itself to that old call.
                ensure!(
                    previous.is_some(),
                    "Existing task has no bound tool call request"
                );
                self.set_state(
                    session,
                    &job,
                    &request,
                    NativeTaskTerminal::Completed.as_str(),
                )
                .await?;
                return Ok(NativeTaskClaim::Replay(output));
            }
        }
        match previous.as_deref() {
            Some("failed") => return Ok(NativeTaskClaim::Stopped(NativeTaskTerminal::Failed)),
            Some("cancelled") => {
                return Ok(NativeTaskClaim::Stopped(NativeTaskTerminal::Cancelled))
            }
            Some("completed") => bail!("Previously completed native task output is unavailable"),
            Some("interrupted" | "running") => {
                self.set_state(session, &job, &request, "interrupted")
                    .await?;
                return Ok(NativeTaskClaim::Stopped(NativeTaskTerminal::Interrupted));
            }
            Some("deferred") | None if config.is_none() => {}
            None | Some("deferred") => {
                bail!("Persisted native task configuration is missing execution evidence")
            }
            Some(_) => bail!("Unknown native task call state"),
        }
        self.set_state(session, &job, &request, "running").await?;
        Ok(NativeTaskClaim::Execute(NativeTaskAttempt {
            pool: self.pool.clone(),
            session: session.into(),
            job,
            request: request.as_str().into(),
            _lock: lock,
        }))
    }

    pub(crate) fn job_id(session: &str, call: &str) -> Result<String> {
        Ok(format!(
            "native:{}",
            Sha256Digest::of_serializable(&(session, call))?.as_str()
        ))
    }

    async fn set_state(
        &self,
        session: &str,
        job: &str,
        request: &Sha256Digest,
        state: &str,
    ) -> Result<()> {
        let changed = sqlx::query(
            "INSERT INTO native_task_calls_v1(session_id,job_id,request_sha256,state) VALUES(?,?,?,?)
             ON CONFLICT(session_id,job_id) DO UPDATE SET state=excluded.state
             WHERE native_task_calls_v1.request_sha256=excluded.request_sha256",
        )
        .bind(session)
        .bind(job)
        .bind(request.as_str())
        .bind(state)
        .execute(&self.pool)
        .await?
        .rows_affected();
        ensure!(changed == 1, "Native task belongs to another tool request");
        Ok(())
    }

    async fn replay_completed(
        &self,
        configurations: &TaskConfigurationStore,
        config: &TaskConfigurationRecord,
        call: &str,
    ) -> Result<Option<ImportedTaskOutput>> {
        let session = config.session_id();
        let job = config.job_id();
        let association = MediaAssociation::ToolCall {
            tool_call_id: call.into(),
        };
        let (kind, count) = match config.snapshot() {
            TaskConfigurationSnapshot::Qwen3Asr(_) => {
                let Some(text) = configurations.read_transcript(session, job).await? else {
                    return Ok(None);
                };
                ensure!(
                    text.association == association,
                    "Transcript belongs to a different tool call"
                );
                return Ok(Some(ImportedTaskOutput::Transcript(text)));
            }
            TaskConfigurationSnapshot::PocketTts(_) => (MediaKind::Audio, 1),
            TaskConfigurationSnapshot::Generation(snapshot) => match &snapshot.parameters {
                EffectiveGenerationParameters::Image(p) => (MediaKind::Image, p.batch_count),
                EffectiveGenerationParameters::Video(_) => (MediaKind::Video, 1),
            },
        };
        ensure!(
            (1..=1024).contains(&count),
            "Invalid native task output count"
        );
        let media = MediaResultStore::new(self.pool.clone());
        let hash = Sha256Digest::parse(config.generation_config_sha256())?;
        let mut results = Vec::new();
        for index in 0..count {
            let id = native_job_result_id(session, job, &hash, index as usize);
            let Some(status) = media.read(session, &id).await? else {
                return Ok(None);
            };
            if status.availability != super::MediaAvailability::Available {
                return Ok(None);
            }
            let result = status.result;
            ensure!(
                result.kind == kind
                    && result.job_id == job
                    && result.association == association
                    && configurations.verify_result(&result).await? == *config,
                "Native task output provenance differs from its configuration or tool call"
            );
            results.push(result);
        }
        Ok(Some(ImportedTaskOutput::Media(results)))
    }
}

#[cfg(all(test, windows))]
#[path = "task_jobs_tests.rs"]
mod tests;
