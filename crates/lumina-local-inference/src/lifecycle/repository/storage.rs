use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{LifecycleJob, LifecycleRepository};
use crate::lifecycle::{
    installation_storage_volume, InstallationCommitStage, InstallationFailureOrigin,
    InstallationInput, InstallationOperationError, InstallationStore, JobStatus,
    LifecycleErrorCode, Sha256Digest, StorageReservation, StorageReservationState, WorkerLease,
    PUBLICATION_STORAGE_RULES_VERSION, STORAGE_RESERVATION_RULES_VERSION,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0011.sql");

fn insufficient() -> anyhow::Error {
    InstallationOperationError {
        code: LifecycleErrorCode::DiskInsufficient,
        origin: InstallationFailureOrigin::Admission,
    }
    .into()
}

impl LifecycleRepository {
    fn read_storage_by(
        connection: &Connection,
        job_id: &str,
    ) -> Result<Option<StorageReservation>> {
        let row:Option<(u64,Option<String>,u64,String,String)>=connection.query_row(
            "SELECT version,volume_id,reserved_bytes,digest,payload FROM installation_storage_reservations WHERE job_id=?1",[job_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
        row.map(|(version,volume,bytes,digest,payload)| {
            let reservation:StorageReservation=serde_json::from_str(&payload)?;
            reservation.validate()?;
            ensure!(reservation.job_id==job_id && reservation.version==version && reservation.volume_id==volume
                && reservation.reserved_bytes==bytes && Sha256Digest::of_serializable(&reservation)?.as_str()==digest,
                "Installation storage reservation integrity check failed");
            let event:(String,String)=connection.query_row("SELECT digest,payload FROM installation_storage_events WHERE job_id=?1 AND version=?2",params![job_id,version],|row|Ok((row.get(0)?,row.get(1)?)))?;
            ensure!(event==(digest,payload),"Storage reservation differs from its committed event");
            Ok(reservation)
        }).transpose()
    }

    fn write_storage_by(connection: &Connection, reservation: &StorageReservation) -> Result<()> {
        reservation.validate()?;
        let digest = Sha256Digest::of_serializable(reservation)?;
        let payload = serde_json::to_string(reservation)?;
        connection.execute("INSERT INTO installation_storage_events (job_id,version,digest,payload) VALUES (?1,?2,?3,?4)",params![reservation.job_id,reservation.version,digest.as_str(),payload])?;
        if reservation.version == 0 {
            connection.execute("INSERT INTO installation_storage_reservations (job_id,version,volume_id,reserved_bytes,digest,payload) VALUES (?1,?2,?3,?4,?5,?6)",params![reservation.job_id,reservation.version,reservation.volume_id,reservation.reserved_bytes,digest.as_str(),payload])?;
        } else {
            let count=connection.execute("UPDATE installation_storage_reservations SET version=?2,volume_id=?3,reserved_bytes=?4,digest=?5,payload=?6 WHERE job_id=?1 AND version=?7",params![reservation.job_id,reservation.version,reservation.volume_id,reservation.reserved_bytes,digest.as_str(),payload,reservation.version-1])?;
            ensure!(count == 1, "Storage reservation version conflict");
        }
        Ok(())
    }

    pub(super) fn migrate_storage_reservations(connection: &Connection) -> Result<()> {
        let mut statement =
            connection.prepare("SELECT job_id FROM installation_inputs ORDER BY job_id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for id in ids {
            let job = Self::read_job_by(connection, "id", &id)?
                .context("Storage migration job missing")?;
            let input = Self::read_installation_input_by(connection, &job)?
                .context("Storage migration input missing")?;
            let plan = Self::input_plan_by(connection, &job)?;
            let inactive = matches!(job.status, JobStatus::Completed | JobStatus::Cancelled);
            let reservation = StorageReservation {
                rules_version: STORAGE_RESERVATION_RULES_VERSION.into(),
                job_id: id,
                plan_digest: job.plan_digest,
                store_root: input.input.store_root,
                volume_id: None,
                peak_bytes: plan.required_disk_bytes,
                reserved_bytes: if inactive {
                    0
                } else {
                    plan.required_disk_bytes
                },
                state: if inactive {
                    StorageReservationState::Released
                } else {
                    StorageReservationState::NeedsAdmission
                },
                version: 0,
                created_at: job.updated_at,
                updated_at: job.updated_at,
            };
            Self::write_storage_by(connection, &reservation)?;
        }
        Ok(())
    }

    fn other_storage_by(connection: &Connection, job_id: &str, volume: &str) -> Result<u64> {
        let mut statement = connection
            .prepare("SELECT job_id FROM installation_storage_reservations WHERE job_id!=?1")?;
        let ids = statement
            .query_map([job_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().try_fold(0u64, |sum, id| {
            let reservation =
                Self::read_storage_by(connection, &id)?.context("Storage reservation missing")?;
            if reservation
                .volume_id
                .as_deref()
                .is_none_or(|id| id == volume)
            {
                sum.checked_add(reservation.reserved_bytes)
                    .context("Storage reservation sum overflow")
            } else {
                Ok(sum)
            }
        })
    }

    pub(super) fn reserve_storage_by(
        connection: &Connection,
        job: &LifecycleJob,
        input: &InstallationInput,
        limit: u64,
        now: i64,
    ) -> Result<StorageReservation> {
        let plan = Self::input_plan_by(connection, job)?;
        let volume = installation_storage_volume(&input.store_root)?;
        let existing = Self::read_storage_by(connection, &job.id)?;
        if let Some(existing) = &existing {
            ensure!(
                existing.plan_digest == job.plan_digest
                    && existing.store_root == input.store_root
                    && existing.peak_bytes == plan.required_disk_bytes,
                "Storage reservation differs from the accepted installation"
            );
            if existing.state == StorageReservationState::Held {
                ensure!(
                    existing.volume_id.as_deref() == Some(volume.as_str()),
                    "Installation storage volume changed"
                );
                return Ok(existing.clone());
            }
        }
        ensure!(
            now >= job.updated_at && existing.as_ref().is_none_or(|r| now >= r.updated_at),
            "Storage reservation time moved backwards"
        );
        let inactive = matches!(job.status, JobStatus::Completed | JobStatus::Cancelled);
        if !inactive {
            let other = Self::other_storage_by(connection, &job.id, &volume)?;
            let free = fs2::available_space(&input.store_root)?.min(limit);
            if plan.required_disk_bytes > free.saturating_sub(other) {
                return Err(insufficient());
            }
        }
        let reservation = StorageReservation {
            rules_version: STORAGE_RESERVATION_RULES_VERSION.into(),
            job_id: job.id.clone(),
            plan_digest: job.plan_digest.clone(),
            store_root: input.store_root.clone(),
            volume_id: Some(volume),
            peak_bytes: plan.required_disk_bytes,
            reserved_bytes: if inactive {
                0
            } else {
                plan.required_disk_bytes
            },
            state: if inactive {
                StorageReservationState::Released
            } else {
                StorageReservationState::Held
            },
            version: existing.as_ref().map_or(0, |r| r.version + 1),
            created_at: existing.map_or(now, |r| r.created_at),
            updated_at: now,
        };
        Self::write_storage_by(connection, &reservation)?;
        Ok(reservation)
    }

    pub fn storage_reservation(&self, job_id: &str) -> Result<Option<StorageReservation>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        Self::read_storage_by(&connection, job_id)
    }

    pub(super) fn validate_storage_claim(
        connection: &Connection,
        job: &LifecycleJob,
    ) -> Result<()> {
        if let Some(reservation) = Self::read_storage_by(connection, &job.id)? {
            ensure!(
                reservation.state == StorageReservationState::Held
                    && reservation.plan_digest == job.plan_digest,
                "Installation requires storage admission before worker claim"
            );
        } else {
            ensure!(
                Self::read_installation_input_by(connection, job)?.is_none(),
                "Bound installation has no storage reservation"
            );
        }
        Ok(())
    }

    pub fn admit_installation_storage(
        &self,
        expected: &LifecycleJob,
        store: &InstallationStore,
        now: i64,
    ) -> Result<Option<StorageReservation>> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job =
            Self::read_job_by(&tx, "id", &expected.id)?.context("Installation job missing")?;
        ensure!(job == *expected, "Job state version conflict");
        ensure!(
            job.status == JobStatus::Queued
                || (job.status == JobStatus::Running
                    && job.lease_until.is_some_and(|until| until <= now)),
            "Installation is not available for storage admission"
        );
        let Some(input) = Self::read_installation_input_by(&tx, &job)? else {
            return Ok(None);
        };
        ensure!(
            input.input.store_root == store.root(),
            "Storage admission belongs to another store"
        );
        let reservation = Self::reserve_storage_by(&tx, &job, &input.input, u64::MAX, now)?;
        tx.commit()?;
        Ok(Some(reservation))
    }

    pub fn require_installation_space(
        &self,
        job_id: &str,
        store: &InstallationStore,
        needed: u64,
    ) -> Result<()> {
        let volume = installation_storage_volume(store.root())?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        if let Some(own) = Self::read_storage_by(&connection, job_id)? {
            ensure!(
                own.store_root == store.root()
                    && own.volume_id.as_deref() == Some(volume.as_str())
                    && own.state == StorageReservationState::Held,
                "Installation has no active reservation on this volume"
            );
            ensure!(
                needed <= own.reserved_bytes,
                "Installation write exceeds its remaining storage reservation"
            );
        }
        let other = Self::other_storage_by(&connection, job_id, &volume)?;
        if needed > fs2::available_space(store.root())?.saturating_sub(other) {
            return Err(insufficient());
        }
        Ok(())
    }

    pub(crate) fn update_publication_storage(
        &self,
        lease: &WorkerLease,
        store: &InstallationStore,
        remaining: u64,
        now: i64,
    ) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job =
            Self::read_job_by(&tx, "id", &lease.job_id)?.context("Installation job missing")?;
        Self::validate_lease(&job, lease, now)?;
        let journal = Self::read_commit_by(&tx, &job.id)?.context("Installation commit missing")?;
        ensure!(
            journal.stage == InstallationCommitStage::Staging,
            "Publication storage can only change during staging"
        );
        let Some(mut reservation) = Self::read_storage_by(&tx, &job.id)? else {
            ensure!(
                Self::read_installation_input_by(&tx, &job)?.is_none(),
                "Bound installation has no storage reservation"
            );
            return Ok(());
        };
        let volume = installation_storage_volume(store.root())?;
        ensure!(
            reservation.state == StorageReservationState::Held
                && reservation.plan_digest == job.plan_digest
                && reservation.store_root == store.root()
                && reservation.volume_id.as_deref() == Some(volume.as_str())
                && remaining <= reservation.peak_bytes
                && now >= reservation.updated_at,
            "Publication storage does not match the held installation reservation"
        );
        if remaining > reservation.reserved_bytes {
            let other = Self::other_storage_by(&tx, &job.id, &volume)?;
            if remaining > fs2::available_space(store.root())?.saturating_sub(other) {
                return Err(insufficient());
            }
        }
        if reservation.rules_version != PUBLICATION_STORAGE_RULES_VERSION
            || remaining != reservation.reserved_bytes
        {
            reservation.rules_version = PUBLICATION_STORAGE_RULES_VERSION.into();
            reservation.reserved_bytes = remaining;
            reservation.version += 1;
            reservation.updated_at = now;
            Self::write_storage_by(&tx, &reservation)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn release_storage_by(
        connection: &Connection,
        job_id: &str,
        now: i64,
    ) -> Result<()> {
        if let Some(mut reservation) = Self::read_storage_by(connection, job_id)?
            .filter(|r| r.state != StorageReservationState::Released)
        {
            ensure!(
                now >= reservation.updated_at,
                "Storage release time moved backwards"
            );
            reservation.reserved_bytes = 0;
            reservation.state = StorageReservationState::Released;
            reservation.version += 1;
            reservation.updated_at = now;
            Self::write_storage_by(connection, &reservation)?;
        }
        Ok(())
    }

    pub fn release_inactive_storage(
        &self,
        job_id: &str,
        store: &InstallationStore,
        now: i64,
    ) -> Result<bool> {
        let before = self.read_job(job_id)?.context("Installation job missing")?;
        if !matches!(
            before.status,
            JobStatus::Completed | JobStatus::Cancelled | JobStatus::Failed | JobStatus::Blocked
        ) {
            return Ok(false);
        }
        let Some(record) = self.storage_reservation(job_id)? else {
            return Ok(false);
        };
        if record.store_root != store.root() || record.state == StorageReservationState::Released {
            return Ok(false);
        }
        let _writer = match store.transfer_lock(job_id) {
            Ok(lock) => lock,
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                }) =>
            {
                return Ok(false)
            }
            Err(error) => return Err(error),
        };
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job = Self::read_job_by(&tx, "id", job_id)?.context("Installation job missing")?;
        if job != before {
            return Ok(false);
        }
        Self::release_storage_by(&tx, job_id, now)?;
        tx.commit()?;
        Ok(true)
    }

    pub fn reconcile_installation_storage(
        &self,
        store: &InstallationStore,
        now: i64,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<crate::lifecycle::StorageReconciliationPage> {
        ensure!(
            (1..=1000).contains(&limit),
            "Storage reconciliation page size must be 1..=1000"
        );
        let ids = {
            let connection = self
                .connection
                .lock()
                .map_err(|_| anyhow!("Model database lock poisoned"))?;
            let mut statement=connection.prepare("SELECT r.job_id FROM installation_storage_reservations r JOIN lifecycle_jobs j ON j.id=r.job_id WHERE json_extract(r.payload,'$.state')!='released' AND j.status IN (?1,?2,?3,?4) AND json_extract(r.payload,'$.store_root')=?6 AND (?7 IS NULL OR r.job_id>?7) ORDER BY r.job_id LIMIT ?5")?;
            let rows = statement.query_map(
                params![
                    serde_json::to_string(&JobStatus::Completed)?,
                    serde_json::to_string(&JobStatus::Cancelled)?,
                    serde_json::to_string(&JobStatus::Failed)?,
                    serde_json::to_string(&JobStatus::Blocked)?,
                    limit,
                    store.root().to_str().context("Storage root is not UTF-8")?,
                    after_id
                ],
                |row| row.get::<_, String>(0),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let next_after_id = (ids.len() == limit as usize)
            .then(|| ids.last().expect("Storage page is nonempty").clone());
        let mut released = 0;
        for id in ids {
            if self.release_inactive_storage(&id, store, now)? {
                released += 1;
            }
        }
        Ok(crate::lifecycle::StorageReconciliationPage {
            released,
            next_after_id,
        })
    }
}
