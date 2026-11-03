use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{LifecycleJob, LifecycleRepository};
use crate::lifecycle::{
    InstallationControlOutcome, InstallationControlRequest, InstallationJobAction,
    InstallationStore, JobStatus, Sha256Digest,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0008.sql");
const CONTROL_RECEIPT_VERSION: &str = "installation-control-v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlReceipt {
    rules_version: String,
    request: InstallationControlRequest,
    store_root: PathBuf,
    job: LifecycleJob,
    accepted_at: i64,
}

fn target(action: InstallationJobAction, status: JobStatus) -> Result<Option<JobStatus>> {
    Ok(Some(match (action, status) {
        (InstallationJobAction::Pause, JobStatus::Queued | JobStatus::Running) => JobStatus::Paused,
        (InstallationJobAction::Resume, JobStatus::Paused)
        | (InstallationJobAction::Retry, JobStatus::Failed | JobStatus::Blocked) => {
            JobStatus::Queued
        }
        (InstallationJobAction::Cancel, JobStatus::Running) => JobStatus::Cancelling,
        (InstallationJobAction::Cancel, JobStatus::Queued | JobStatus::Paused) => {
            JobStatus::Cancelled
        }
        (
            InstallationJobAction::Cancel,
            JobStatus::Cancelling | JobStatus::Cancelled | JobStatus::Completed,
        ) => return Ok(None),
        _ => anyhow::bail!("Installation action is not valid for this job state"),
    }))
}

impl LifecycleRepository {
    fn read_control_receipt_by(
        connection: &Connection,
        request: &InstallationControlRequest,
        store: &InstallationStore,
    ) -> Result<Option<LifecycleJob>> {
        let row: Option<(String, u64, String, String, i64)> = connection.query_row(
            "SELECT job_id,result_version,digest,payload,created_at FROM installation_control_receipts WHERE idempotency_key=?1",
            [&request.idempotency_key],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?;
        let Some((job_id, version, digest, payload, accepted_at)) = row else {
            return Ok(None);
        };
        let receipt: ControlReceipt = serde_json::from_str(&payload)?;
        ensure!(
            receipt.rules_version == CONTROL_RECEIPT_VERSION
                && receipt.request.idempotency_key == request.idempotency_key
                && receipt.job.id == job_id
                && receipt.request.job_id == job_id
                && receipt.job.version == version
                && receipt.accepted_at == accepted_at
                && Sha256Digest::of_serializable(&receipt)?.as_str() == digest,
            "Installation control receipt integrity check failed"
        );
        ensure!(
            receipt.request == *request && receipt.store_root == store.root(),
            "Control idempotency key belongs to a different request or store"
        );
        let event: String = connection.query_row(
            "SELECT payload FROM job_events WHERE job_id=?1 AND version=?2",
            params![job_id, version],
            |row| row.get(0),
        )?;
        ensure!(
            serde_json::from_str::<LifecycleJob>(&event)? == receipt.job,
            "Installation control receipt differs from its committed job event"
        );
        Ok(Some(receipt.job))
    }

    pub fn control_installation(
        &self,
        request: &InstallationControlRequest,
        store: &InstallationStore,
        now: i64,
    ) -> Result<InstallationControlOutcome> {
        ensure!(!request.idempotency_key.trim().is_empty()
            && request.idempotency_key.len() <= 256
            && !request.idempotency_key.chars().any(char::is_control),
            "Installation control requires an idempotency key of at most 256 bytes without control characters");
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(job) = Self::read_control_receipt_by(&tx, request, store)? {
            return Ok(InstallationControlOutcome {
                job,
                replayed: true,
            });
        }
        let job =
            Self::read_job_by(&tx, "id", &request.job_id)?.context("Installation job not found")?;
        ensure!(
            job.version == request.expected_version,
            "Job state version conflict"
        );
        ensure!(
            now >= job.updated_at,
            "Installation control time moved backwards"
        );
        if let Some(intent) = Self::read_installation_input_by(&tx, &job)? {
            ensure!(
                intent.input.store_root == store.root(),
                "Installation belongs to another store"
            );
            if matches!(
                request.action,
                InstallationJobAction::Resume | InstallationJobAction::Retry
            ) {
                Self::reserve_storage_by(&tx, &job, &intent.input, u64::MAX, now)?;
            }
        }
        let job = match target(request.action, job.status)? {
            Some(status) => {
                Self::transition_by(&tx, &job.id, request.expected_version, status, now)?
            }
            None => job,
        };
        let receipt = ControlReceipt {
            rules_version: CONTROL_RECEIPT_VERSION.into(),
            request: request.clone(),
            store_root: store.root().into(),
            job: job.clone(),
            accepted_at: now,
        };
        tx.execute("INSERT INTO installation_control_receipts (idempotency_key,job_id,result_version,digest,payload,created_at) VALUES (?1,?2,?3,?4,?5,?6)",params![request.idempotency_key,job.id,job.version,Sha256Digest::of_serializable(&receipt)?.as_str(),serde_json::to_string(&receipt)?,now])?;
        tx.commit()?;
        Ok(InstallationControlOutcome {
            job,
            replayed: false,
        })
    }
}
