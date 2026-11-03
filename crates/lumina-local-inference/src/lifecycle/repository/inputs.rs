use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{LifecycleJob, LifecycleRepository};
use crate::lifecycle::{
    InstallPlan, InstallationInput, InstallationIntent, InstallationSource, InstallationStore,
    InstallationSubmission, JobAcceptance, JobStatus, Sha256Digest,
    INSTALLATION_INPUT_RULES_VERSION,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0007.sql");

impl LifecycleRepository {
    pub fn accept_modelscope_installation(
        &self,
        request: &InstallationSubmission,
        store: &InstallationStore,
        now: i64,
    ) -> Result<LifecycleJob> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(job) = Self::read_job_by(&tx, "idempotency_key", &request.idempotency_key)? {
            ensure!(
                job.plan_id == request.plan_id && job.plan_digest == request.plan_digest,
                "Idempotency key belongs to a different installation plan"
            );
            let intent = Self::read_installation_input_by(&tx, &job)?
                .context("Legacy installation requires explicit input adoption")?;
            ensure!(
                intent.input.installation_id == request.installation_id
                    && intent.input.store_root == store.root()
                    && matches!(intent.input.source, InstallationSource::ModelScope {}),
                "Idempotency key belongs to different installation inputs"
            );
            return Ok(job);
        }
        let acceptance = JobAcceptance {
            id: format!("job:{}", uuid::Uuid::new_v4()),
            idempotency_key: request.idempotency_key.clone(),
            plan_id: request.plan_id.clone(),
            plan_digest: request.plan_digest.clone(),
        };
        let input = InstallationInput::new(
            store,
            request.installation_id.clone(),
            format!("version:{}", uuid::Uuid::new_v4()),
            InstallationSource::ModelScope {},
        )?;
        let job = Self::enqueue_by(&tx, &acceptance, now, fs2::available_space(store.root())?)?;
        Self::bind_installation_input_by(&tx, &job, &input, now)?;
        Self::reserve_storage_by(&tx, &job, &input, u64::MAX, now)?;
        tx.commit()?;
        Ok(job)
    }
    pub(super) fn input_plan_by(
        connection: &Connection,
        job: &LifecycleJob,
    ) -> Result<InstallPlan> {
        let (digest, payload): (String, String) = connection.query_row(
            "SELECT digest, payload FROM installation_plans WHERE id = ?1",
            [&job.plan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let plan: InstallPlan = serde_json::from_str(&payload)?;
        ensure!(
            plan.id == job.plan_id
                && digest == job.plan_digest.as_str()
                && plan.digest()? == job.plan_digest,
            "Installation input plan integrity check failed"
        );
        Self::validate_plan_observations_by(connection, &plan)?;
        Ok(plan)
    }

    pub(super) fn read_installation_input_by(
        connection: &Connection,
        job: &LifecycleJob,
    ) -> Result<Option<InstallationIntent>> {
        let stored: Option<(String, String, String)> = connection
            .query_row(
                "SELECT version_id, digest, payload FROM installation_inputs WHERE job_id = ?1",
                [&job.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((version, digest, payload)) = stored else {
            return Ok(None);
        };
        let intent: InstallationIntent = serde_json::from_str(&payload)?;
        ensure!(
            intent.rules_version == INSTALLATION_INPUT_RULES_VERSION
                && intent.job_id == job.id
                && intent.plan_digest == job.plan_digest
                && intent.bound_at >= job.accepted_at
                && intent.input.version_id == version
                && Sha256Digest::of_serializable(&intent)?.as_str() == digest,
            "Installation input integrity check failed"
        );
        intent
            .input
            .validate_plan(&Self::input_plan_by(connection, job)?)?;
        if let Some(journal) = Self::read_commit_by(connection, &job.id)? {
            ensure!(
                journal.manifest.installation_id == intent.input.installation_id
                    && journal.manifest.version_id == intent.input.version_id,
                "Installation input differs from the committed destination"
            );
        }
        Ok(Some(intent))
    }

    pub fn read_installation_input(&self, job_id: &str) -> Result<Option<InstallationIntent>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let job =
            Self::read_job_by(&connection, "id", job_id)?.context("Installation job not found")?;
        Self::read_installation_input_by(&connection, &job)
    }

    fn bind_installation_input_by(
        connection: &Connection,
        job: &LifecycleJob,
        input: &InstallationInput,
        now: i64,
    ) -> Result<InstallationIntent> {
        if let Some(existing) = Self::read_installation_input_by(connection, job)? {
            ensure!(
                existing.input == *input,
                "Installation job already has different immutable inputs"
            );
            return Ok(existing);
        }
        ensure!(
            job.status != JobStatus::Completed,
            "Completed legacy installations cannot adopt new inputs"
        );
        input.validate_plan(&Self::input_plan_by(connection, job)?)?;
        ensure!(
            input.store_root.canonicalize()? == input.store_root && now >= job.updated_at,
            "Installation input requires its canonical store root and a valid binding time"
        );
        if let Some(journal) = Self::read_commit_by(connection, &job.id)? {
            ensure!(
                journal.manifest.installation_id == input.installation_id
                    && journal.manifest.version_id == input.version_id,
                "Legacy input differs from the existing installation journal"
            );
        }
        let registered_job: Option<String> = connection
            .query_row(
                "SELECT job_id FROM installation_versions WHERE version_id = ?1",
                [&input.version_id],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(
            registered_job.as_deref().is_none_or(|id| id == job.id),
            "Installation version belongs to another job"
        );
        let intent = InstallationIntent {
            rules_version: INSTALLATION_INPUT_RULES_VERSION.into(),
            job_id: job.id.clone(),
            plan_digest: job.plan_digest.clone(),
            bound_at: now,
            input: input.clone(),
        };
        connection.execute(
            "INSERT INTO installation_inputs (job_id, version_id, digest, payload) VALUES (?1, ?2, ?3, ?4)",
            params![job.id, input.version_id, Sha256Digest::of_serializable(&intent)?.as_str(), serde_json::to_string(&intent)?],
        )?;
        Ok(intent)
    }

    pub fn adopt_installation_input(
        &self,
        job_id: &str,
        input: &InstallationInput,
        now: i64,
    ) -> Result<InstallationIntent> {
        input.validate()?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let job = Self::read_job_by(&tx, "id", job_id)?.context("Installation job not found")?;
        let already_bound = Self::read_installation_input_by(&tx, &job)?.is_some();
        let intent = Self::bind_installation_input_by(&tx, &job, input, now)?;
        if !already_bound {
            Self::reserve_storage_by(&tx, &job, input, u64::MAX, now)?;
        }
        tx.commit()?;
        Ok(intent)
    }

    pub fn enqueue_installation(
        &self,
        request: &JobAcceptance,
        input: &InstallationInput,
        now: i64,
        available_disk_bytes: u64,
    ) -> Result<LifecycleJob> {
        input.validate()?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = Self::read_job_by(&tx, "idempotency_key", &request.idempotency_key)?;
        if let Some(existing) = existing {
            let intent = Self::read_installation_input_by(&tx, &existing)?
                .context("Legacy job requires explicit input adoption")?;
            ensure!(
                existing.plan_id == request.plan_id
                    && existing.plan_digest == request.plan_digest
                    && intent.input == *input,
                "Idempotency key belongs to a different installation request"
            );
            return Ok(existing);
        }
        let job = Self::enqueue_by(&tx, request, now, available_disk_bytes)?;
        Self::bind_installation_input_by(&tx, &job, input, now)?;
        Self::reserve_storage_by(&tx, &job, input, available_disk_bytes, now)?;
        tx.commit()?;
        Ok(job)
    }
}
