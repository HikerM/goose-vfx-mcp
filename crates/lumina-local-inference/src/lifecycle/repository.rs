use anyhow::{anyhow, ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use super::{
    CapabilityEvidence, InstallPlan, JobStatus, LifecycleStage, RepositorySnapshot, Sha256Digest,
};

mod catalog;
mod files;
mod installation;
mod observations;
mod plan_observations;
pub use files::FileTransferEvent;
pub use observations::{GgufObservationRecord, GGUF_OBSERVATION_VERSION};
pub use plan_observations::{
    gguf_header_evidence_id, gguf_observation_evidence_id, OBSERVED_GGUF_PLAN_RULES_VERSION,
};

pub const MODEL_DATABASE_SCHEMA_VERSION: u32 = 6;

const SCHEMA: &str = include_str!("repository/migrations/0001.sql");

pub struct LifecycleRepository {
    connection: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleJob {
    pub id: String,
    pub idempotency_key: String,
    pub plan_id: String,
    pub plan_digest: Sha256Digest,
    pub stage: LifecycleStage,
    pub status: JobStatus,
    pub version: u64,
    pub generation: u64,
    pub lease_owner: Option<String>,
    pub lease_until: Option<i64>,
    pub accepted_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEvent {
    pub sequence: u64,
    pub job: LifecycleJob,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLease {
    pub job_id: String,
    pub owner: String,
    pub generation: u64,
}

impl LifecycleRepository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version <= MODEL_DATABASE_SCHEMA_VERSION,
            "Model database schema is newer than this application"
        );
        if version == 0 {
            tx.execute_batch(SCHEMA)?;
        }
        if version < 2 {
            let mut statement =
                tx.prepare("SELECT id, digest, payload FROM repository_snapshots")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            for (id, digest, payload) in rows {
                let snapshot: RepositorySnapshot = serde_json::from_str(&payload)?;
                snapshot.validate()?;
                ensure!(
                    Sha256Digest::of_serializable(&snapshot)?.as_str() == digest,
                    "Snapshot integrity failed before database migration"
                );
                tx.execute(
                    "UPDATE repository_snapshots SET digest = ?1 WHERE id = ?2",
                    params![snapshot.content_digest()?.as_str(), id],
                )?;
            }
            tx.execute_batch(catalog::SCHEMA)?;
        }
        if version < 3 {
            tx.execute_batch(files::SCHEMA)?;
        }
        if version < 4 {
            tx.execute_batch(installation::SCHEMA)?;
        }
        if version < 5 {
            tx.execute_batch(observations::SCHEMA)?;
        }
        if version < 6 {
            tx.execute_batch(plan_observations::SCHEMA)?;
        }
        tx.commit()?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn save_plan(
        &self,
        plan: &InstallPlan,
        now: i64,
        available_disk_bytes: u64,
    ) -> Result<Sha256Digest> {
        ensure!(
            !plan_observations::requires_gguf_observations(plan),
            "Observed GGUF plans require save_observed_plan"
        );
        plan.validate_for_install(now, available_disk_bytes)?;
        let digest = plan.digest()?;
        let payload = serde_json::to_string(plan)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::store_plan_by(&tx, plan, &digest, &payload)?;
        tx.commit()?;
        Ok(digest)
    }

    fn store_plan_by(
        connection: &Connection,
        plan: &InstallPlan,
        digest: &Sha256Digest,
        payload: &str,
    ) -> Result<()> {
        let existing: Option<(String, String)> = connection
            .query_row(
                "SELECT digest, payload FROM installation_plans WHERE id = ?1",
                [&plan.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((existing, stored_payload)) = existing {
            let stored: InstallPlan = serde_json::from_str(&stored_payload)?;
            ensure!(
                existing == digest.as_str() && stored.digest()? == *digest,
                "Installation plan ID already contains different or corrupted content"
            );
            return Ok(());
        }
        for (id, snapshot) in &plan.snapshots {
            Self::store_snapshot(connection, id, snapshot)?;
        }
        connection.execute("INSERT INTO installation_plans (id, digest, payload, expires_at) VALUES (?1, ?2, ?3, ?4)", params![plan.id, digest.as_str(), payload, plan.expires_at])?;
        for id in plan.snapshots.keys() {
            connection.execute(
                "INSERT INTO plan_snapshots (plan_id, snapshot_id) VALUES (?1, ?2)",
                params![plan.id, id],
            )?;
        }
        Ok(())
    }

    pub fn read_plan(&self, id: &str) -> Result<Option<InstallPlan>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT digest, payload FROM installation_plans WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(digest, payload)| {
            let plan: InstallPlan = serde_json::from_str(&payload)?;
            ensure!(
                plan.id == id && plan.digest()?.as_str() == digest,
                "Stored installation plan integrity check failed"
            );
            Self::validate_plan_observations_by(&connection, &plan)?;
            Ok(plan)
        })
        .transpose()
    }

    pub fn read_snapshot(&self, id: &str) -> Result<Option<RepositorySnapshot>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT digest, payload FROM repository_snapshots WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(digest, payload)| {
            let snapshot: RepositorySnapshot = serde_json::from_str(&payload)?;
            snapshot.validate()?;
            ensure!(
                snapshot.content_digest()?.as_str() == digest,
                "Stored repository snapshot integrity check failed"
            );
            Ok(snapshot)
        })
        .transpose()
    }

    fn store_snapshot(
        connection: &Connection,
        id: &str,
        snapshot: &RepositorySnapshot,
    ) -> Result<()> {
        ensure!(!id.is_empty(), "Missing source snapshot ID");
        snapshot.validate()?;
        let digest = snapshot.content_digest()?;
        connection.execute("INSERT INTO repository_snapshots (id, repository, revision, digest, payload) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(id) DO NOTHING", params![id, snapshot.repository.as_str(), snapshot.resolved_revision, digest.as_str(), serde_json::to_string(snapshot)?])?;
        let (stored_digest, payload): (String, String) = connection.query_row(
            "SELECT digest, payload FROM repository_snapshots WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let stored: RepositorySnapshot = serde_json::from_str(&payload)?;
        stored.validate()?;
        ensure!(
            stored_digest == digest.as_str() && stored.content_digest()? == digest,
            "Repository snapshot ID already contains different content"
        );
        Ok(())
    }

    pub fn enqueue(
        &self,
        id: &str,
        idempotency_key: &str,
        plan_id: &str,
        expected_digest: &Sha256Digest,
        now: i64,
        available_disk_bytes: u64,
    ) -> Result<LifecycleJob> {
        ensure!(
            !id.is_empty() && !idempotency_key.trim().is_empty(),
            "Missing job ID or idempotency key"
        );
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = Self::read_job_by(&tx, "idempotency_key", idempotency_key)?;
        if let Some(existing) = existing {
            ensure!(
                existing.plan_id == plan_id && &existing.plan_digest == expected_digest,
                "Idempotency key belongs to a different installation plan"
            );
            return Ok(existing);
        }
        let (stored_digest, payload): (String, String) = tx.query_row(
            "SELECT digest, payload FROM installation_plans WHERE id = ?1",
            [plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let plan: InstallPlan = serde_json::from_str(&payload)?;
        ensure!(
            plan.id == plan_id
                && stored_digest == expected_digest.as_str()
                && plan.digest()? == *expected_digest,
            "Installation plan digest does not match"
        );
        plan.validate_for_install(now, available_disk_bytes)?;
        Self::validate_plan_observations_by(&tx, &plan)?;
        tx.execute("INSERT INTO lifecycle_jobs (id, idempotency_key, plan_id, plan_digest, stage, status, accepted_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)", params![id, idempotency_key, plan_id, expected_digest.as_str(), serde_json::to_string(&LifecycleStage::ReadyToInstall)?, serde_json::to_string(&JobStatus::Queued)?, now])?;
        let job =
            Self::read_job_by(&tx, "id", id)?.ok_or_else(|| anyhow!("Accepted job is missing"))?;
        Self::initialize_files(&tx, &job, &plan)?;
        Self::append_event(&tx, &job)?;
        tx.commit()?;
        Ok(job)
    }

    pub fn read_job(&self, id: &str) -> Result<Option<LifecycleJob>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        Self::read_job_by(&connection, "id", id)
    }

    pub fn list_jobs(&self, after_id: Option<&str>, limit: u32) -> Result<Vec<LifecycleJob>> {
        self.query_jobs(after_id, limit, None)
    }

    pub fn recoverable_jobs(
        &self,
        now: i64,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<LifecycleJob>> {
        self.query_jobs(after_id, limit, Some(now))
    }

    fn query_jobs(
        &self,
        after_id: Option<&str>,
        limit: u32,
        recovery_at: Option<i64>,
    ) -> Result<Vec<LifecycleJob>> {
        ensure!(
            (1..=1000).contains(&limit),
            "Job page size must be 1..=1000"
        );
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let mut statement = connection.prepare(
            "SELECT id FROM lifecycle_jobs WHERE (?1 IS NULL OR id > ?1) AND (?2 IS NULL OR status = ?3 OR (status = ?4 AND lease_until <= ?2) OR status = ?5) ORDER BY id LIMIT ?6"
        )?;
        let ids = statement
            .query_map(
                params![
                    after_id,
                    recovery_at,
                    serde_json::to_string(&JobStatus::Queued)?,
                    serde_json::to_string(&JobStatus::Running)?,
                    serde_json::to_string(&JobStatus::Cancelling)?,
                    limit
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| {
                Self::read_job_by(&connection, "id", id)?
                    .ok_or_else(|| anyhow!("Listed job disappeared"))
            })
            .collect()
    }

    fn read_job_by(
        connection: &Connection,
        column: &str,
        value: &str,
    ) -> Result<Option<LifecycleJob>> {
        let sql = match column {
            "id" => "SELECT id, idempotency_key, plan_id, plan_digest, stage, status, version, generation, lease_owner, lease_until, updated_at, accepted_at FROM lifecycle_jobs WHERE id = ?1",
            "idempotency_key" => "SELECT id, idempotency_key, plan_id, plan_digest, stage, status, version, generation, lease_owner, lease_until, updated_at, accepted_at FROM lifecycle_jobs WHERE idempotency_key = ?1",
            _ => unreachable!("job lookup column is internal"),
        };
        let mut stmt = connection.prepare(sql)?;
        let mut rows = stmt.query([value])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(LifecycleJob {
            id: row.get(0)?,
            idempotency_key: row.get(1)?,
            plan_id: row.get(2)?,
            plan_digest: Sha256Digest::parse(row.get::<_, String>(3)?)?,
            stage: serde_json::from_str(&row.get::<_, String>(4)?)?,
            status: serde_json::from_str(&row.get::<_, String>(5)?)?,
            version: row.get(6)?,
            generation: row.get(7)?,
            lease_owner: row.get(8)?,
            lease_until: row.get(9)?,
            updated_at: row.get(10)?,
            accepted_at: row.get(11)?,
        }))
    }

    fn append_event(connection: &Connection, job: &LifecycleJob) -> Result<()> {
        connection.execute(
            "INSERT INTO job_events (job_id, version, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                job.id,
                job.version,
                serde_json::to_string(job)?,
                job.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn claim(
        &self,
        id: &str,
        expected_version: u64,
        owner: &str,
        now: i64,
        lease_seconds: u32,
    ) -> Result<WorkerLease> {
        ensure!(
            !owner.trim().is_empty() && lease_seconds > 0,
            "Invalid worker lease"
        );
        let until = now
            .checked_add(i64::from(lease_seconds))
            .ok_or_else(|| anyhow!("Worker lease time overflow"))?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job = Self::read_job_by(&tx, "id", id)?.ok_or_else(|| anyhow!("Job not found"))?;
        ensure!(
            job.version == expected_version,
            "Job state version conflict"
        );
        let payload: String = tx.query_row(
            "SELECT payload FROM installation_plans WHERE id = ?1",
            [&job.plan_id],
            |row| row.get(0),
        )?;
        let plan: InstallPlan = serde_json::from_str(&payload)?;
        ensure!(
            plan.digest()? == job.plan_digest,
            "Stored installation plan integrity check failed"
        );
        plan.validate_for_install(job.accepted_at, u64::MAX)?;
        Self::initialize_files(&tx, &job, &plan)?;
        ensure!(
            job.status == JobStatus::Queued
                || (job.status == JobStatus::Running
                    && job.lease_until.is_some_and(|expiry| expiry <= now)),
            "Job is not available for a worker"
        );
        job.status = JobStatus::Running;
        job.version += 1;
        job.generation += 1;
        job.lease_owner = Some(owner.to_string());
        job.lease_until = Some(until);
        job.updated_at = now;
        Self::write_job(&tx, &job, expected_version)?;
        tx.commit()?;
        Ok(WorkerLease {
            job_id: id.to_string(),
            owner: owner.to_string(),
            generation: job.generation,
        })
    }

    pub fn renew_lease(
        &self,
        lease: &WorkerLease,
        expected_version: u64,
        now: i64,
        lease_seconds: u32,
    ) -> Result<LifecycleJob> {
        ensure!(lease_seconds > 0, "Invalid worker lease duration");
        let until = now
            .checked_add(i64::from(lease_seconds))
            .ok_or_else(|| anyhow!("Worker lease time overflow"))?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job =
            Self::read_job_by(&tx, "id", &lease.job_id)?.ok_or_else(|| anyhow!("Job not found"))?;
        ensure!(
            job.version == expected_version,
            "Job state version conflict"
        );
        Self::validate_lease(&job, lease, now)?;
        job.lease_until = Some(until);
        job.updated_at = now;
        job.version += 1;
        Self::write_job(&tx, &job, expected_version)?;
        tx.commit()?;
        Ok(job)
    }

    pub(crate) fn validate_lease(job: &LifecycleJob, lease: &WorkerLease, now: i64) -> Result<()> {
        ensure!(
            job.status == JobStatus::Running
                && job.lease_owner.as_deref() == Some(lease.owner.as_str())
                && job.generation == lease.generation
                && job.lease_until.is_some_and(|until| until > now),
            "Worker lease is expired or fenced"
        );
        Ok(())
    }

    fn write_job(connection: &Connection, job: &LifecycleJob, expected_version: u64) -> Result<()> {
        let updated = connection.execute("UPDATE lifecycle_jobs SET stage = ?1, status = ?2, version = ?3, generation = ?4, lease_owner = ?5, lease_until = ?6, updated_at = ?7 WHERE id = ?8 AND version = ?9", params![serde_json::to_string(&job.stage)?, serde_json::to_string(&job.status)?, job.version, job.generation, job.lease_owner, job.lease_until, job.updated_at, job.id, expected_version])?;
        ensure!(updated == 1, "Job state version conflict");
        Self::append_event(connection, job)
    }

    pub fn advance(
        &self,
        lease: &WorkerLease,
        expected_version: u64,
        next_stage: LifecycleStage,
        now: i64,
    ) -> Result<LifecycleJob> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job =
            Self::read_job_by(&tx, "id", &lease.job_id)?.ok_or_else(|| anyhow!("Job not found"))?;
        ensure!(
            job.version == expected_version,
            "Job state version conflict"
        );
        Self::validate_lease(&job, lease, now)?;
        job.stage = job.stage.advance(next_stage)?;
        Self::validate_file_stage(&tx, &job)?;
        job.version += 1;
        job.updated_at = now;
        Self::write_job(&tx, &job, expected_version)?;
        tx.commit()?;
        Ok(job)
    }

    pub fn transition(
        &self,
        id: &str,
        expected_version: u64,
        status: JobStatus,
        now: i64,
    ) -> Result<LifecycleJob> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job = Self::read_job_by(&tx, "id", id)?.ok_or_else(|| anyhow!("Job not found"))?;
        ensure!(
            job.version == expected_version,
            "Job state version conflict"
        );
        ensure!(
            status != JobStatus::Running,
            "Running jobs require a worker lease"
        );
        ensure!(
            status != JobStatus::Completed,
            "Job completion requires a verified stage-specific result"
        );
        job.status = job.status.transition(status)?;
        job.version += 1;
        job.generation += 1;
        job.lease_owner = None;
        job.lease_until = None;
        job.updated_at = now;
        Self::write_job(&tx, &job, expected_version)?;
        tx.commit()?;
        Ok(job)
    }

    pub fn events(&self, job_id: &str, after_sequence: u64, limit: u32) -> Result<Vec<JobEvent>> {
        ensure!(
            (1..=1000).contains(&limit),
            "Event page size must be 1..=1000"
        );
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let mut stmt = connection.prepare("SELECT sequence, payload FROM job_events WHERE job_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT ?3")?;
        let rows = stmt.query_map(params![job_id, after_sequence, limit], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (sequence, payload) = row?;
            Ok(JobEvent {
                sequence,
                job: serde_json::from_str(&payload)?,
            })
        })
        .collect()
    }

    pub fn save_evidence(&self, evidence: &CapabilityEvidence) -> Result<()> {
        evidence.validate()?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        connection.execute("INSERT INTO capability_evidence (installation_id, capability, digest, payload) VALUES (?1, ?2, ?3, ?4)", params![evidence.installation_id, serde_json::to_string(&evidence.capability)?, Sha256Digest::of_serializable(evidence)?.as_str(), serde_json::to_string(evidence)?])?;
        Ok(())
    }

    pub fn evidence(&self, installation_id: &str) -> Result<Vec<CapabilityEvidence>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let mut stmt = connection.prepare("SELECT digest, payload FROM capability_evidence WHERE installation_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([installation_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (digest, payload) = row?;
            let evidence: CapabilityEvidence = serde_json::from_str(&payload)?;
            evidence.validate()?;
            ensure!(
                Sha256Digest::of_serializable(&evidence)?.as_str() == digest,
                "Stored capability evidence integrity check failed"
            );
            Ok(evidence)
        })
        .collect()
    }
}
