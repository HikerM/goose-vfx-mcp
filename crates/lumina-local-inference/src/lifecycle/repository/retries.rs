use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{LifecycleJob, LifecycleRepository, WorkerLease};
use crate::lifecycle::{
    FileAttemptFailure, FileRetryBudget, FileRetryState, FileTransferRecord, FileTransferState,
    InstallationFileFailure, JobStatus, RetryHistoryOrigin, Sha256Digest, FILE_RETRY_MAX_ATTEMPTS,
    FILE_RETRY_POLICY_VERSION,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0009.sql");

impl LifecycleRepository {
    pub(super) fn initialize_retry_budget(
        connection: &Connection,
        file: &FileTransferRecord,
        origin: RetryHistoryOrigin,
    ) -> Result<()> {
        if Self::read_retry_by(connection, &file.job_id, &file.artifact_id)?.is_some() {
            return Ok(());
        }
        let budget = FileRetryBudget {
            job_id: file.job_id.clone(),
            artifact_id: file.artifact_id.clone(),
            policy_version: FILE_RETRY_POLICY_VERSION.into(),
            max_attempts: FILE_RETRY_MAX_ATTEMPTS,
            cycle: 0,
            cycle_attempts: 0,
            recorded_attempts: 0,
            history_origin: origin,
            tracking_started_at: file.updated_at,
            state: if matches!(file.state, FileTransferState::Verified { .. }) {
                FileRetryState::Verified
            } else {
                FileRetryState::Ready
            },
            last_failure: None,
            version: 0,
            updated_at: file.updated_at,
        };
        Self::write_retry_by(connection, &budget)
    }

    pub(super) fn migrate_retry_budgets(connection: &Connection) -> Result<()> {
        let mut statement = connection.prepare("SELECT job_id, artifact_id FROM job_files")?;
        let ids = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for (job_id, artifact_id) in ids {
            let file = Self::read_file_by(connection, &job_id, &artifact_id)?
                .context("File transfer missing during retry migration")?;
            Self::initialize_retry_budget(connection, &file, RetryHistoryOrigin::LegacyUntracked)?;
        }
        Ok(())
    }

    fn read_retry_by(
        connection: &Connection,
        job_id: &str,
        artifact_id: &str,
    ) -> Result<Option<FileRetryBudget>> {
        let row: Option<(u64, Option<i64>, String, String)> = connection.query_row(
            "SELECT version,next_retry_at,digest,payload FROM installation_file_retries WHERE job_id=?1 AND artifact_id=?2",
            params![job_id, artifact_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional()?;
        row.map(|(version, retry_at, digest, payload)| {
            let budget: FileRetryBudget = serde_json::from_str(&payload)?;
            budget.validate()?;
            ensure!(budget.job_id == job_id && budget.artifact_id == artifact_id
                && budget.version == version && budget.next_retry_at() == retry_at
                && Sha256Digest::of_serializable(&budget)?.as_str() == digest,
                "Installation file retry integrity check failed");
            let event: (String, String) = connection.query_row(
                "SELECT digest,payload FROM installation_file_retry_events WHERE job_id=?1 AND artifact_id=?2 AND version=?3",
                params![job_id,artifact_id,version], |row| Ok((row.get(0)?,row.get(1)?)),
            )?;
            ensure!(event == (digest, payload), "Installation retry differs from its committed event");
            Ok(budget)
        }).transpose()
    }

    fn write_retry_by(connection: &Connection, budget: &FileRetryBudget) -> Result<()> {
        budget.validate()?;
        let digest = Sha256Digest::of_serializable(budget)?;
        let payload = serde_json::to_string(budget)?;
        connection.execute("INSERT INTO installation_file_retry_events (job_id,artifact_id,version,digest,payload) VALUES (?1,?2,?3,?4,?5)",params![budget.job_id,budget.artifact_id,budget.version,digest.as_str(),payload])?;
        if budget.version == 0 {
            connection.execute("INSERT INTO installation_file_retries (job_id,artifact_id,version,next_retry_at,digest,payload) VALUES (?1,?2,?3,?4,?5,?6)",params![budget.job_id,budget.artifact_id,budget.version,budget.next_retry_at(),digest.as_str(),payload])?;
        } else {
            let changed = connection.execute("UPDATE installation_file_retries SET version=?3,next_retry_at=?4,digest=?5,payload=?6 WHERE job_id=?1 AND artifact_id=?2 AND version=?7",params![budget.job_id,budget.artifact_id,budget.version,budget.next_retry_at(),digest.as_str(),payload,budget.version-1])?;
            ensure!(changed == 1, "Installation retry version conflict");
        }
        Ok(())
    }

    fn retries_by(connection: &Connection, job_id: &str) -> Result<Vec<FileRetryBudget>> {
        let mut statement = connection
            .prepare("SELECT artifact_id FROM job_files WHERE job_id=?1 ORDER BY artifact_id")?;
        let ids = statement
            .query_map([job_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| {
                Self::read_retry_by(connection, job_id, id)?
                    .context("Installation file retry budget is missing")
            })
            .collect()
    }

    pub fn file_retry_budgets(&self, job_id: &str) -> Result<Vec<FileRetryBudget>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        Self::retries_by(&connection, job_id)
    }

    pub(super) fn validate_retry_claim(
        connection: &Connection,
        job_id: &str,
        now: i64,
    ) -> Result<()> {
        for budget in Self::retries_by(connection, job_id)? {
            ensure!(
                !matches!(
                    budget.state,
                    FileRetryState::Exhausted | FileRetryState::NeedsAction
                ),
                "Installation file requires an explicit retry"
            );
            ensure!(
                budget.next_retry_at().is_none_or(|at| at <= now),
                "Installation retry backoff has not elapsed"
            );
        }
        Ok(())
    }

    pub fn begin_file_attempt(
        &self,
        lease: &WorkerLease,
        artifact_id: &str,
        now: i64,
    ) -> Result<FileRetryBudget> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job =
            Self::read_job_by(&tx, "id", &lease.job_id)?.context("Installation job not found")?;
        Self::validate_lease(&job, lease, now)?;
        let mut budget = Self::read_retry_by(&tx, &job.id, artifact_id)?
            .context("Installation file retry budget is missing")?;
        ensure!(
            now >= budget.updated_at,
            "Installation retry time moved backwards"
        );
        if matches!(
            budget.state,
            FileRetryState::Active | FileRetryState::Verified
        ) {
            return Ok(budget);
        }
        ensure!(
            matches!(
                budget.state,
                FileRetryState::Ready | FileRetryState::Waiting { .. }
            ) && budget.next_retry_at().is_none_or(|at| at <= now)
                && budget.cycle_attempts < budget.max_attempts,
            "Installation file retry budget is unavailable"
        );
        budget.cycle_attempts += 1;
        budget.recorded_attempts = budget
            .recorded_attempts
            .checked_add(1)
            .context("Installation attempt count overflow")?;
        budget.state = FileRetryState::Active;
        budget.version += 1;
        budget.updated_at = now;
        Self::write_retry_by(&tx, &budget)?;
        tx.commit()?;
        Ok(budget)
    }

    pub fn fail_installation_file(
        &self,
        lease: &WorkerLease,
        artifact_id: &str,
        expected_version: u64,
        failure: FileAttemptFailure,
        now: i64,
    ) -> Result<FileRetryBudget> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job =
            Self::read_job_by(&tx, "id", &lease.job_id)?.context("Installation job not found")?;
        Self::validate_lease(&job, lease, now)?;
        let mut budget = Self::read_retry_by(&tx, &job.id, artifact_id)?
            .context("Installation file retry budget is missing")?;
        ensure!(
            budget.state == FileRetryState::Active && now >= budget.updated_at,
            "Installation file has no active retry attempt"
        );
        let code = failure.code;
        let retry_after_seconds = failure.retry_after_seconds.map(|seconds| seconds.min(60));
        budget.last_failure = Some(InstallationFileFailure {
            code,
            stage: job.stage,
            failed_at: now,
            retry_after_seconds,
        });
        budget.state = if !code.retryable() {
            FileRetryState::NeedsAction
        } else if budget.cycle_attempts >= budget.max_attempts {
            FileRetryState::Exhausted
        } else {
            let delay = 2i64
                .pow(budget.cycle_attempts)
                .max(retry_after_seconds.unwrap_or(0) as i64);
            FileRetryState::Waiting {
                retry_at: now
                    .checked_add(delay)
                    .context("Installation retry time overflow")?,
            }
        };
        budget.version += 1;
        budget.updated_at = now;
        Self::update_file_by(&tx, lease, artifact_id, expected_version, now, |file, _| {
            file.state = FileTransferState::Failed { code };
            Ok(())
        })?;
        Self::write_retry_by(&tx, &budget)?;
        let updated =
            Self::read_job_by(&tx, "id", &job.id)?.context("Installation job disappeared")?;
        let mut failed =
            Self::transition_by(&tx, &job.id, updated.version, JobStatus::Failed, now)?;
        if matches!(budget.state, FileRetryState::Waiting { .. }) {
            let version = failed.version;
            failed.status = failed.status.transition(JobStatus::Queued)?;
            failed.version += 1;
            failed.generation += 1;
            Self::write_job(&tx, &failed, version)?;
        }
        Self::record_failure_by(
            &tx,
            &failed,
            crate::lifecycle::InstallationFailureDetails {
                code,
                origin: crate::lifecycle::InstallationFailureOrigin::for_stage(failed.stage),
                artifact_id: Some(artifact_id.into()),
                automatic_retry_at: budget.next_retry_at(),
            },
            now,
        )?;
        tx.commit()?;
        Ok(budget)
    }

    pub(super) fn complete_retry_by(
        connection: &Connection,
        job: &LifecycleJob,
        artifact_id: &str,
        now: i64,
    ) -> Result<()> {
        let mut budget = Self::read_retry_by(connection, &job.id, artifact_id)?
            .context("Installation file retry budget is missing")?;
        if budget.state != FileRetryState::Verified {
            budget.state = FileRetryState::Verified;
            budget.version += 1;
            budget.updated_at = now;
            Self::write_retry_by(connection, &budget)?;
        }
        Ok(())
    }

    pub(super) fn reset_retries_by(connection: &Connection, job_id: &str, now: i64) -> Result<()> {
        for mut budget in Self::retries_by(connection, job_id)? {
            if matches!(
                budget.state,
                FileRetryState::NeedsAction
                    | FileRetryState::Exhausted
                    | FileRetryState::Waiting { .. }
            ) {
                ensure!(
                    now >= budget.updated_at,
                    "Installation retry time moved backwards"
                );
                budget.cycle = budget
                    .cycle
                    .checked_add(1)
                    .context("Installation retry cycle overflow")?;
                budget.cycle_attempts = 0;
                budget.state = FileRetryState::Ready;
                budget.version += 1;
                budget.updated_at = now;
                Self::write_retry_by(connection, &budget)?;
            }
        }
        Ok(())
    }
}
