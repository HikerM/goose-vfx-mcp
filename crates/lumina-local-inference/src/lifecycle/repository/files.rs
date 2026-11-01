use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use super::{LifecycleJob, LifecycleRepository, WorkerLease};
use crate::lifecycle::{
    FileRestartReason, FileTransferRecord, FileTransferState, InstallPlan, LifecycleErrorCode,
    LifecycleStage, RepositoryPath, Sha256Digest, TransferCheckpoint, VerifiedArtifactFile,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0003.sql");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTransferEvent {
    pub sequence: u64,
    pub file: FileTransferRecord,
}

impl LifecycleRepository {
    pub(super) fn validate_file_stage(connection: &Connection, job: &LifecycleJob) -> Result<()> {
        if !matches!(
            job.stage,
            LifecycleStage::Verifying | LifecycleStage::Installing | LifecycleStage::Installed
        ) {
            return Ok(());
        }
        ensure!(
            job.stage != LifecycleStage::Installed,
            "Installation registration requires a verified filesystem commit"
        );
        let payload: String = connection.query_row(
            "SELECT payload FROM installation_plans WHERE id = ?1",
            [&job.plan_id],
            |row| row.get(0),
        )?;
        let plan: InstallPlan = serde_json::from_str(&payload)?;
        ensure!(
            plan.digest()? == job.plan_digest,
            "Stored installation plan integrity check failed"
        );
        for id in plan.artifacts.keys() {
            let record = Self::read_file_by(connection, &job.id, id)?
                .context("Accepted file transfer is missing")?;
            let ready = match job.stage {
                LifecycleStage::Verifying => matches!(
                    record.state,
                    FileTransferState::Downloaded | FileTransferState::Verified { .. }
                ),
                LifecycleStage::Installing => {
                    matches!(record.state, FileTransferState::Verified { .. })
                }
                _ => false,
            };
            ensure!(
                ready,
                "Required artifacts are not ready for the next installation stage"
            );
        }
        Ok(())
    }

    pub(super) fn initialize_files(
        connection: &Connection,
        job: &LifecycleJob,
        plan: &InstallPlan,
    ) -> Result<()> {
        for (id, artifact) in &plan.artifacts {
            let record = FileTransferRecord {
                job_id: job.id.clone(),
                artifact_id: id.clone(),
                relative_path: RepositoryPath::parse(format!(
                    "{}.part",
                    Sha256Digest::of_bytes(id.as_bytes()).as_str()
                ))?,
                expected_bytes: artifact.size_bytes.context("Artifact size is unknown")?,
                checkpoint: TransferCheckpoint::empty(),
                state: FileTransferState::Pending,
                restart_reason: None,
                version: 0,
                generation: 0,
                updated_at: job.accepted_at,
            };
            let existing = Self::read_file_by(connection, &job.id, id)?;
            if let Some(existing) = existing {
                ensure!(
                    existing.expected_bytes == record.expected_bytes
                        && existing.relative_path == record.relative_path,
                    "File transfer disagrees with its immutable plan"
                );
                continue;
            }
            connection.execute("INSERT INTO job_files (job_id, artifact_id, version, digest, payload) VALUES (?1, ?2, 0, ?3, ?4)", params![job.id, id, Sha256Digest::of_serializable(&record)?.as_str(), serde_json::to_string(&record)?])?;
            Self::append_file_event(connection, &record)?;
        }
        Ok(())
    }

    pub(super) fn read_file_by(
        connection: &Connection,
        job_id: &str,
        artifact_id: &str,
    ) -> Result<Option<FileTransferRecord>> {
        let row: Option<(u64, String, String)> = connection.query_row(
            "SELECT version, digest, payload FROM job_files WHERE job_id = ?1 AND artifact_id = ?2", params![job_id, artifact_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?;
        row.map(|(version, digest, payload)| {
            let record: FileTransferRecord = serde_json::from_str(&payload)?;
            record.validate()?;
            ensure!(
                record.job_id == job_id
                    && record.artifact_id == artifact_id
                    && record.version == version
                    && Sha256Digest::of_serializable(&record)?.as_str() == digest,
                "File transfer integrity check failed"
            );
            Ok(record)
        })
        .transpose()
    }

    pub fn read_file_transfer(
        &self,
        job_id: &str,
        artifact_id: &str,
    ) -> Result<Option<FileTransferRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        Self::read_file_by(&connection, job_id, artifact_id)
    }

    pub fn file_transfers(&self, job_id: &str) -> Result<Vec<FileTransferRecord>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let mut statement = connection
            .prepare("SELECT artifact_id FROM job_files WHERE job_id = ?1 ORDER BY artifact_id")?;
        let ids = statement
            .query_map([job_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| {
                Self::read_file_by(&connection, job_id, id)?.context("File transfer disappeared")
            })
            .collect()
    }

    fn append_file_event(connection: &Connection, record: &FileTransferRecord) -> Result<()> {
        connection.execute("INSERT INTO job_file_events (job_id, artifact_id, file_version, digest, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![record.job_id, record.artifact_id, record.version, Sha256Digest::of_serializable(record)?.as_str(), serde_json::to_string(record)?, record.updated_at])?;
        Ok(())
    }

    fn update_file(
        &self,
        lease: &WorkerLease,
        artifact_id: &str,
        expected_version: u64,
        now: i64,
        operation: impl FnOnce(&mut FileTransferRecord, &LifecycleJob) -> Result<()>,
    ) -> Result<FileTransferRecord> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job = Self::read_job_by(&tx, "id", &lease.job_id)?.context("Job not found")?;
        Self::validate_lease(&job, lease, now)?;
        ensure!(
            matches!(
                job.stage,
                LifecycleStage::Downloading | LifecycleStage::Verifying
            ),
            "File writes require a transfer or verification stage"
        );
        let mut record = Self::read_file_by(&tx, &lease.job_id, artifact_id)?
            .context("File is absent from the accepted plan")?;
        ensure!(
            record.version == expected_version,
            "File transfer state version conflict"
        );
        ensure!(
            !matches!(record.state, FileTransferState::Verified { .. }),
            "Verified file transfer is immutable"
        );
        operation(&mut record, &job)?;
        record.version += 1;
        record.generation = lease.generation;
        record.updated_at = now;
        record.validate()?;
        let updated = tx.execute("UPDATE job_files SET version = ?1, digest = ?2, payload = ?3 WHERE job_id = ?4 AND artifact_id = ?5 AND version = ?6", params![record.version, Sha256Digest::of_serializable(&record)?.as_str(), serde_json::to_string(&record)?, record.job_id, record.artifact_id, expected_version])?;
        ensure!(updated == 1, "File transfer state version conflict");
        Self::append_file_event(&tx, &record)?;
        let previous_version = job.version;
        job.version += 1;
        job.updated_at = now;
        Self::write_job(&tx, &job, previous_version)?;
        tx.commit()?;
        Ok(record)
    }

    pub fn checkpoint_file(
        &self,
        lease: &WorkerLease,
        artifact_id: &str,
        expected_version: u64,
        checkpoint: TransferCheckpoint,
        now: i64,
    ) -> Result<FileTransferRecord> {
        self.update_file(lease, artifact_id, expected_version, now, |record, job| {
            ensure!(
                job.stage == LifecycleStage::Downloading
                    && !matches!(record.state, FileTransferState::Failed { .. }),
                "File is not accepting download checkpoints"
            );
            checkpoint.validate(record.expected_bytes)?;
            ensure!(
                checkpoint.completed_bytes >= record.checkpoint.completed_bytes,
                "File checkpoint regressed without an explicit restart"
            );
            ensure!(
                record.checkpoint.completed_bytes == 0
                    || checkpoint.resume_validator == record.checkpoint.resume_validator,
                "Resume validator changed without an explicit restart"
            );
            record.state = if checkpoint.completed_bytes == record.expected_bytes {
                FileTransferState::Downloaded
            } else {
                FileTransferState::Downloading
            };
            record.checkpoint = checkpoint;
            Ok(())
        })
    }

    pub fn restart_file(
        &self,
        lease: &WorkerLease,
        artifact_id: &str,
        expected_version: u64,
        reason: FileRestartReason,
        now: i64,
    ) -> Result<FileTransferRecord> {
        self.update_file(lease, artifact_id, expected_version, now, |record, job| {
            ensure!(
                job.stage == LifecycleStage::Downloading,
                "File restarts require the download stage"
            );
            record.state = FileTransferState::Pending;
            record.checkpoint = TransferCheckpoint::empty();
            record.restart_reason = Some(reason);
            Ok(())
        })
    }

    pub fn fail_file(
        &self,
        lease: &WorkerLease,
        artifact_id: &str,
        expected_version: u64,
        code: LifecycleErrorCode,
        now: i64,
    ) -> Result<FileTransferRecord> {
        self.update_file(lease, artifact_id, expected_version, now, |record, _| {
            record.state = FileTransferState::Failed { code };
            Ok(())
        })
    }

    pub fn record_verified_file(
        &self,
        lease: &WorkerLease,
        artifact_id: &str,
        expected_version: u64,
        verified: &VerifiedArtifactFile,
        now: i64,
    ) -> Result<FileTransferRecord> {
        self.update_file(lease, artifact_id, expected_version, now, |record, job| {
            ensure!(
                verified.artifact_id == artifact_id
                    && verified.relative_path == record.relative_path
                    && verified.plan_digest == job.plan_digest
                    && verified.size_bytes == record.expected_bytes,
                "Artifact verification belongs to a different transfer or plan"
            );
            record.checkpoint.completed_bytes = verified.size_bytes;
            record.checkpoint.prefix_sha256 = verified.computed_sha256.clone();
            record.state = FileTransferState::Verified {
                computed_sha256: verified.computed_sha256.clone(),
                evidence_digest: verified.evidence_digest.clone(),
            };
            Ok(())
        })
    }

    pub fn file_transfer_events(
        &self,
        job_id: &str,
        after_sequence: u64,
        limit: u32,
    ) -> Result<Vec<FileTransferEvent>> {
        ensure!(
            (1..=1000).contains(&limit),
            "Event page size must be 1..=1000"
        );
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let mut statement = connection.prepare("SELECT sequence, artifact_id, file_version, digest, payload FROM job_file_events WHERE job_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT ?3")?;
        let rows = statement.query_map(params![job_id, after_sequence, limit], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (sequence, artifact_id, version, digest, payload) = row?;
            let file: FileTransferRecord = serde_json::from_str(&payload)?;
            file.validate()?;
            ensure!(
                file.job_id == job_id
                    && file.artifact_id == artifact_id
                    && file.version == version
                    && Sha256Digest::of_serializable(&file)?.as_str() == digest,
                "File transfer event integrity check failed"
            );
            Ok(FileTransferEvent { sequence, file })
        })
        .collect()
    }
}
