use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{LifecycleJob, LifecycleRepository, WorkerLease};
use crate::lifecycle::{
    build_installation_manifest, ActiveInstallation, Artifact, InstallationCommitJournal,
    InstallationCommitStage, InstallationFilesWitness, InstalledVersion, JobStatus, LifecycleStage,
    ModelManifest, Sha256Digest,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0004.sql");

impl LifecycleRepository {
    fn read_commit_by(
        connection: &Connection,
        job_id: &str,
    ) -> Result<Option<InstallationCommitJournal>> {
        let row: Option<(String, String, u64, String, String)> = connection.query_row(
            "SELECT version_id, manifest_digest, version, digest, payload FROM installation_commit_journals WHERE job_id = ?1",
            [job_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).optional()?;
        row.map(|(version_id, manifest_digest, version, digest, payload)| {
            let journal: InstallationCommitJournal = serde_json::from_str(&payload)?;
            journal.manifest.validate()?;
            ensure!(
                journal.job_id == job_id
                    && journal.manifest.version_id == version_id
                    && journal.version == version
                    && journal.manifest_digest.as_str() == manifest_digest
                    && journal.manifest.digest()? == journal.manifest_digest
                    && Sha256Digest::of_serializable(&journal)?.as_str() == digest
                    && journal.created_at <= journal.updated_at,
                "Installation commit journal integrity check failed"
            );
            Ok(journal)
        })
        .transpose()
    }

    pub fn installation_commit(&self, job_id: &str) -> Result<Option<InstallationCommitJournal>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        Self::read_commit_by(&connection, job_id)
    }

    pub fn pending_installation_commits(
        &self,
        limit: u32,
    ) -> Result<Vec<InstallationCommitJournal>> {
        ensure!(
            (1..=1000).contains(&limit),
            "Recovery page size must be 1..=1000"
        );
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let mut statement = connection.prepare("SELECT j.job_id FROM installation_commit_journals j LEFT JOIN installation_versions v ON v.job_id = j.job_id WHERE v.version_id IS NULL ORDER BY j.job_id LIMIT ?1")?;
        let ids = statement
            .query_map([limit], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| {
                Self::read_commit_by(&connection, id)?.context("Installation commit disappeared")
            })
            .collect()
    }

    fn store_commit(connection: &Connection, journal: &InstallationCommitJournal) -> Result<()> {
        let changed = connection.execute("UPDATE installation_commit_journals SET version = ?1, digest = ?2, payload = ?3 WHERE job_id = ?4 AND version = ?5",
            params![journal.version, Sha256Digest::of_serializable(journal)?.as_str(), serde_json::to_string(journal)?, journal.job_id, journal.version - 1])?;
        ensure!(changed == 1, "Installation commit version conflict");
        Ok(())
    }

    fn require_installation_worker(
        connection: &Connection,
        lease: &WorkerLease,
        expected_version: u64,
        now: i64,
    ) -> Result<LifecycleJob> {
        let job = Self::read_job_by(connection, "id", &lease.job_id)?.context("Job not found")?;
        Self::validate_lease(&job, lease, now)?;
        ensure!(
            job.version == expected_version && job.stage == LifecycleStage::Installing,
            "Installation requires the current job version and installation stage"
        );
        Ok(job)
    }

    fn validate_manifest_catalog(connection: &Connection, manifest: &ModelManifest) -> Result<()> {
        let catalog = Self::read_catalog_by(connection, &manifest.plan.model_id)?
            .context("Installation model is absent from the catalog")?;
        let variant = catalog
            .variants
            .get(&manifest.plan.variant_id)
            .context("Installation variant does not belong to this model")?;
        ensure!(
            variant
                .artifact_ids
                .iter()
                .all(|id| manifest.plan.artifacts.contains_key(id)),
            "Installation is missing variant artifacts"
        );
        for (id, artifact) in &manifest.plan.artifacts {
            let (digest, payload): (String, String) = connection.query_row(
                "SELECT digest, payload FROM catalog_artifacts WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let stored: Artifact = serde_json::from_str(&payload)?;
            ensure!(
                stored == *artifact && Sha256Digest::of_serializable(&stored)?.as_str() == digest,
                "Installation artifact differs from the immutable catalog"
            );
        }
        Ok(())
    }

    pub fn begin_installation_commit(
        &self,
        lease: &WorkerLease,
        expected_version: u64,
        manifest: &ModelManifest,
        now: i64,
    ) -> Result<InstallationCommitJournal> {
        manifest.validate()?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job = Self::require_installation_worker(&tx, lease, expected_version, now)?;
        ensure!(
            manifest.plan.digest()? == job.plan_digest
                && manifest.plan_accepted_at == job.accepted_at
                && manifest.installed_at <= now,
            "Manifest does not match the accepted installation"
        );
        Self::validate_manifest_catalog(&tx, manifest)?;
        let files = manifest
            .plan
            .artifacts
            .keys()
            .map(|id| {
                Self::read_file_by(&tx, &job.id, id)?.context("Installation transfer is missing")
            })
            .collect::<Result<Vec<_>>>()?;
        let expected = build_installation_manifest(
            manifest.installation_id.clone(),
            manifest.version_id.clone(),
            manifest.plan.clone(),
            &files,
            job.accepted_at,
            manifest.installed_at,
        )?;
        ensure!(
            expected == *manifest,
            "Manifest differs from the verified file ledger"
        );
        let manifest_digest = manifest.digest()?;
        if let Some(existing) = Self::read_commit_by(&tx, &job.id)? {
            ensure!(
                existing.manifest_digest == manifest_digest,
                "Job already has a different installation commit"
            );
            return Ok(existing);
        }
        let journal = InstallationCommitJournal {
            job_id: job.id.clone(),
            manifest: manifest.clone(),
            manifest_digest,
            stage: InstallationCommitStage::Staging,
            version: 0,
            created_at: now,
            updated_at: now,
        };
        tx.execute("INSERT INTO installation_commit_journals (version_id, job_id, manifest_digest, version, digest, payload) VALUES (?1, ?2, ?3, 0, ?4, ?5)", params![manifest.version_id, job.id, journal.manifest_digest.as_str(), Sha256Digest::of_serializable(&journal)?.as_str(), serde_json::to_string(&journal)?])?;
        job.version += 1;
        job.updated_at = now;
        Self::write_job(&tx, &job, expected_version)?;
        tx.commit()?;
        Ok(journal)
    }

    pub fn advance_installation_commit(
        &self,
        lease: &WorkerLease,
        expected_version: u64,
        witness: &InstallationFilesWitness,
        next: InstallationCommitStage,
        now: i64,
    ) -> Result<InstallationCommitJournal> {
        ensure!(
            matches!(
                next,
                InstallationCommitStage::Prepared
                    | InstallationCommitStage::FilesCommitted
                    | InstallationCommitStage::Registered
            ),
            "Invalid installation commit target"
        );
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut journal =
            Self::read_commit_by(&tx, &lease.job_id)?.context("Installation commit not found")?;
        ensure!(
            witness.installation_id == journal.manifest.installation_id
                && witness.version_id == journal.manifest.version_id
                && witness.manifest_digest == journal.manifest_digest
                && witness.committed == (next != InstallationCommitStage::Prepared),
            "Filesystem verification does not match this installation commit"
        );
        if journal.stage == InstallationCommitStage::Registered && next == journal.stage {
            Self::read_installed_by(&tx, &journal.manifest.version_id)?
                .context("Registered installation is missing")?;
            return Ok(journal);
        }
        let mut job = Self::require_installation_worker(&tx, lease, expected_version, now)?;
        if journal.stage == next {
            return Ok(journal);
        }
        ensure!(
            matches!(
                (journal.stage, next),
                (
                    InstallationCommitStage::Staging,
                    InstallationCommitStage::Prepared
                ) | (
                    InstallationCommitStage::Prepared,
                    InstallationCommitStage::FilesCommitted
                ) | (
                    InstallationCommitStage::FilesCommitted,
                    InstallationCommitStage::Registered
                )
            ),
            "Illegal installation commit transition"
        );
        ensure!(
            now >= journal.updated_at,
            "Installation commit time moved backwards"
        );
        if next == InstallationCommitStage::Registered {
            Self::register_installation_by(&tx, &journal, now)?;
            job.stage = job.stage.advance(LifecycleStage::Installed)?;
            job.status = job.status.transition(JobStatus::Completed)?;
            job.generation += 1;
            job.lease_owner = None;
            job.lease_until = None;
        }
        journal.stage = next;
        journal.version += 1;
        journal.updated_at = now;
        Self::store_commit(&tx, &journal)?;
        job.version += 1;
        job.updated_at = now;
        Self::write_job(&tx, &job, expected_version)?;
        tx.commit()?;
        Ok(journal)
    }

    fn register_installation_by(
        connection: &Connection,
        journal: &InstallationCommitJournal,
        now: i64,
    ) -> Result<()> {
        let manifest = &journal.manifest;
        Self::validate_manifest_catalog(connection, manifest)?;
        connection.execute(
            "INSERT INTO installations (id) VALUES (?1) ON CONFLICT DO NOTHING",
            [&manifest.installation_id],
        )?;
        connection.execute("INSERT INTO installation_versions (version_id, installation_id, job_id, model_id, variant_id, manifest_digest, payload, registered_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![manifest.version_id, manifest.installation_id, journal.job_id, manifest.plan.model_id, manifest.plan.variant_id, journal.manifest_digest.as_str(), serde_json::to_string(manifest)?, now])?;
        for (id, file) in &manifest.installed_files {
            connection.execute("INSERT INTO managed_blobs (sha256, size_bytes) VALUES (?1, ?2) ON CONFLICT DO NOTHING", params![file.computed_sha256.as_str(), file.size_bytes])?;
            let size: u64 = connection.query_row(
                "SELECT size_bytes FROM managed_blobs WHERE sha256 = ?1",
                [file.computed_sha256.as_str()],
                |row| row.get(0),
            )?;
            ensure!(
                size == file.size_bytes,
                "Shared artifact digest has conflicting sizes"
            );
            connection.execute("INSERT INTO installation_blob_refs (version_id, artifact_id, sha256) VALUES (?1, ?2, ?3)", params![manifest.version_id, id, file.computed_sha256.as_str()])?;
        }
        connection.execute("INSERT INTO installation_activations (installation_id, version_id) VALUES (?1, ?2) ON CONFLICT(installation_id) DO NOTHING", params![manifest.installation_id, manifest.version_id])?;
        Ok(())
    }

    fn read_installed_by(
        connection: &Connection,
        version_id: &str,
    ) -> Result<Option<InstalledVersion>> {
        let row: Option<(String, String, String, String, String, i64)> = connection.query_row(
            "SELECT installation_id, model_id, variant_id, manifest_digest, payload, registered_at FROM installation_versions WHERE version_id = ?1", [version_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        ).optional()?;
        row.map(
            |(installation_id, model_id, variant_id, digest, payload, registered_at)| {
                let manifest: ModelManifest = serde_json::from_str(&payload)?;
                manifest.validate()?;
                let manifest_digest = manifest.digest()?;
                ensure!(
                    manifest.version_id == version_id
                        && manifest.installation_id == installation_id
                        && manifest.plan.model_id == model_id
                        && manifest.plan.variant_id == variant_id
                        && manifest_digest.as_str() == digest
                        && registered_at >= manifest.installed_at,
                    "Installed version integrity check failed"
                );
                Ok(InstalledVersion {
                    manifest,
                    manifest_digest,
                    registered_at,
                })
            },
        )
        .transpose()
    }

    pub fn installed_version(&self, version_id: &str) -> Result<Option<InstalledVersion>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        Self::read_installed_by(&connection, version_id)
    }

    pub fn installation_versions(&self, installation_id: &str) -> Result<Vec<InstalledVersion>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let mut statement = connection.prepare("SELECT version_id FROM installation_versions WHERE installation_id = ?1 ORDER BY registered_at, version_id")?;
        let ids = statement
            .query_map([installation_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .map(|id| {
                Self::read_installed_by(&connection, id)?.context("Installed version disappeared")
            })
            .collect()
    }

    pub fn active_installations(&self) -> Result<Vec<ActiveInstallation>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let mut statement = connection.prepare("SELECT installation_id, version_id FROM installation_activations ORDER BY installation_id")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(installation_id, version_id)| {
                let installed = Self::read_installed_by(&connection, &version_id)?
                    .context("Active installation version is missing")?;
                ensure!(
                    installed.manifest.installation_id == installation_id,
                    "Active version belongs to a different installation"
                );
                Ok(ActiveInstallation {
                    installation_id,
                    active_version_id: version_id,
                    model_id: installed.manifest.plan.model_id,
                    variant_id: installed.manifest.plan.variant_id,
                    manifest_digest: installed.manifest_digest,
                })
            })
            .collect()
    }

    pub fn activate_installation_version(
        &self,
        installation_id: &str,
        expected_active: &str,
        version_id: &str,
    ) -> Result<InstalledVersion> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let installed = Self::read_installed_by(&tx, version_id)?
            .context("Installation version is not registered")?;
        ensure!(
            installed.manifest.installation_id == installation_id,
            "Version belongs to a different installation"
        );
        let updated = tx.execute("UPDATE installation_activations SET version_id = ?1 WHERE installation_id = ?2 AND version_id = ?3", params![version_id, installation_id, expected_active])?;
        ensure!(updated == 1, "Active installation version conflict");
        tx.commit()?;
        Ok(installed)
    }

    pub fn blob_reference_count(&self, digest: &Sha256Digest) -> Result<u64> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        Ok(connection.query_row(
            "SELECT count(DISTINCT version_id) FROM installation_blob_refs WHERE sha256 = ?1",
            [digest.as_str()],
            |row| row.get(0),
        )?)
    }
}
