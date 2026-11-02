use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{LifecycleJob, LifecycleRepository, WorkerLease};
use crate::lifecycle::{
    InstallationFailureDetails, InstallationFailureOrigin, InstallationFailureRecord,
    InstallationStore, JobStatus, Sha256Digest,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0010.sql");

impl LifecycleRepository {
    pub(super) fn record_failure_by(
        connection: &Connection,
        job: &LifecycleJob,
        details: InstallationFailureDetails,
        now: i64,
    ) -> Result<InstallationFailureRecord> {
        let record = InstallationFailureRecord::new(job.clone(), details, now)?;
        record.validate()?;
        if let Some(id) = &record.details.artifact_id {
            Self::read_file_by(connection, &job.id, id)?
                .context("Installation failure references an unknown file")?;
        }
        connection.execute("INSERT INTO installation_failures (job_id,job_version,digest,payload,created_at) VALUES (?1,?2,?3,?4,?5)",params![job.id,job.version,Sha256Digest::of_serializable(&record)?.as_str(),serde_json::to_string(&record)?,now])?;
        Ok(record)
    }

    pub fn latest_installation_failure(
        &self,
        job_id: &str,
    ) -> Result<Option<InstallationFailureRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let row: Option<(u64,String,String,i64)> = connection.query_row("SELECT job_version,digest,payload,created_at FROM installation_failures WHERE job_id=?1 ORDER BY job_version DESC LIMIT 1",[job_id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
        row.map(|(version, digest, payload, time)| {
            let record: InstallationFailureRecord = serde_json::from_str(&payload)?;
            record.validate()?;
            ensure!(
                record.job.id == job_id
                    && record.job.version == version
                    && record.occurred_at == time
                    && Sha256Digest::of_serializable(&record)?.as_str() == digest,
                "Installation failure integrity check failed"
            );
            let event: String = connection.query_row(
                "SELECT payload FROM job_events WHERE job_id=?1 AND version=?2",
                params![job_id, version],
                |row| row.get(0),
            )?;
            ensure!(
                serde_json::from_str::<LifecycleJob>(&event)? == record.job,
                "Installation failure differs from its committed job event"
            );
            Ok(record)
        })
        .transpose()
    }

    pub fn block_installation(
        &self,
        expected: &LifecycleJob,
        store: &InstallationStore,
        details: InstallationFailureDetails,
        now: i64,
    ) -> Result<Option<InstallationFailureRecord>> {
        ensure!(
            matches!(
                details.origin,
                InstallationFailureOrigin::Credentials | InstallationFailureOrigin::Admission
            ) && details.artifact_id.is_none()
                && details.automatic_retry_at.is_none(),
            "Invalid installation preflight failure"
        );
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job =
            Self::read_job_by(&tx, "id", &expected.id)?.context("Installation job not found")?;
        if job != *expected
            || !(job.status == JobStatus::Queued
                || (job.status == JobStatus::Running
                    && job.lease_until.is_some_and(|until| until <= now)))
        {
            return Ok(None);
        }
        let intent = Self::read_installation_input_by(&tx, &job)?;
        if intent.is_none_or(|intent| intent.input.store_root != store.root()) {
            return Ok(None);
        }
        ensure!(
            now >= job.updated_at,
            "Installation failure time moved backwards"
        );
        let blocked = Self::transition_by(&tx, &job.id, job.version, JobStatus::Blocked, now)?;
        let record = Self::record_failure_by(&tx, &blocked, details, now)?;
        tx.commit()?;
        Ok(Some(record))
    }

    pub fn fail_installation_operation(
        &self,
        lease: &WorkerLease,
        expected_version: u64,
        details: InstallationFailureDetails,
        now: i64,
    ) -> Result<InstallationFailureRecord> {
        ensure!(
            details.artifact_id.is_none() && details.automatic_retry_at.is_none(),
            "Non-file operation failure requires no file retry"
        );
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job =
            Self::read_job_by(&tx, "id", &lease.job_id)?.context("Installation job not found")?;
        ensure!(
            job.version == expected_version,
            "Job state version conflict"
        );
        Self::validate_lease(&job, lease, now)?;
        ensure!(
            now >= job.updated_at,
            "Installation failure time moved backwards"
        );
        let failed = Self::transition_by(&tx, &job.id, job.version, JobStatus::Failed, now)?;
        let record = Self::record_failure_by(&tx, &failed, details, now)?;
        tx.commit()?;
        Ok(record)
    }
}
