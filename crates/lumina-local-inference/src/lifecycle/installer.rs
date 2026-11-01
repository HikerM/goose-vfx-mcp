use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::files::{checked_local_path, open_for_verification, verify_artifact_file_with_progress};
use super::installation::verify_installation_directory_with_progress;
use super::{
    installation_directory, InstallationCommitJournal, InstallationCommitStage, InstalledVersion,
    LifecycleJob, LifecycleRepository, LifecycleStage, ModelManifest, RepositoryPath, Sha256Digest,
    WorkerLease,
};

pub struct InstallationStore {
    root: PathBuf,
}

struct InstallationExecution<'a, F> {
    repository: &'a LifecycleRepository,
    lease: &'a WorkerLease,
    clock: F,
}

impl<F: Fn() -> i64> InstallationExecution<'_, F> {
    fn checkpoint(&self) -> Result<LifecycleJob> {
        let now = (self.clock)();
        let job = self
            .repository
            .read_job(&self.lease.job_id)?
            .context("Job not found")?;
        LifecycleRepository::validate_lease(&job, self.lease, now)?;
        ensure!(
            job.stage == LifecycleStage::Installing,
            "Worker is not installing this model"
        );
        if job
            .lease_until
            .context("Worker has no lease expiry")?
            .saturating_sub(now)
            <= 10
        {
            self.repository
                .renew_lease(self.lease, job.version, now, 60)
        } else {
            Ok(job)
        }
    }

    fn advance(
        &self,
        witness: &super::InstallationFilesWitness,
        stage: InstallationCommitStage,
    ) -> Result<InstallationCommitJournal> {
        let job = self.checkpoint()?;
        self.repository.advance_installation_commit(
            self.lease,
            job.version,
            witness,
            stage,
            (self.clock)(),
        )
    }
}

impl InstallationStore {
    pub fn open(root: &Path) -> Result<Self> {
        ensure!(root.is_absolute(), "Installation root must be absolute");
        std::fs::create_dir_all(root)?;
        ensure!(root.is_dir(), "Installation root is not a directory");
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn transfer_lock(&self, job_id: &str) -> Result<File> {
        self.lock("transfers", job_id)
    }

    pub(crate) fn transfer_directory(&self, job_id: &str) -> Result<PathBuf> {
        self.directory(&RepositoryPath::parse(format!(
            "transfers/{}",
            Sha256Digest::of_bytes(job_id.as_bytes()).as_str()
        ))?)
    }

    pub(crate) fn transfer_file_path(
        &self,
        job_id: &str,
        relative: &RepositoryPath,
    ) -> Result<PathBuf> {
        self.target(&RepositoryPath::parse(format!(
            "transfers/{}/{}",
            Sha256Digest::of_bytes(job_id.as_bytes()).as_str(),
            relative.as_str()
        ))?)
    }

    pub(crate) fn verify_installed_version(&self, installed: &InstalledVersion) -> Result<()> {
        let manifest = &installed.manifest;
        ensure!(
            Sha256Digest::of_serializable(manifest)? == installed.manifest_digest,
            "Installed manifest digest differs from its version"
        );
        super::verify_installation_directory(&self.root, manifest, true)?;
        self.verify_blobs(manifest, |_| Ok(()))?;
        self.verify_read_only_version(manifest)
    }

    pub fn blob_path(digest: &Sha256Digest) -> Result<RepositoryPath> {
        RepositoryPath::parse(format!(
            "blobs/sha256/{}/{}",
            digest.as_str().get(..2).expect("SHA256 is validated ASCII"),
            digest.as_str()
        ))
    }

    fn existing(&self, relative: &RepositoryPath) -> Result<Option<PathBuf>> {
        match std::fs::symlink_metadata(self.root.join(relative.as_str())) {
            Ok(_) => Ok(Some(checked_local_path(&self.root, relative)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn directory(&self, relative: &RepositoryPath) -> Result<PathBuf> {
        let mut prefix = String::new();
        for segment in relative.as_str().split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            let part = RepositoryPath::parse(prefix.clone())?;
            let path = self.root.join(part.as_str());
            match std::fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            ensure!(
                checked_local_path(&self.root, &part)?.is_dir(),
                "Installation path is not a directory"
            );
        }
        Ok(self.root.join(relative.as_str()))
    }

    fn target(&self, relative: &RepositoryPath) -> Result<PathBuf> {
        let (parent, _) = relative
            .as_str()
            .rsplit_once('/')
            .context("Managed file has no parent directory")?;
        self.directory(&RepositoryPath::parse(parent)?)?;
        let path = self.root.join(relative.as_str());
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                checked_local_path(&self.root, relative)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(path)
    }

    fn lock(&self, namespace: &str, id: &str) -> Result<File> {
        let relative = RepositoryPath::parse(format!(
            "locks/{namespace}/{}.lock",
            Sha256Digest::of_bytes(id.as_bytes()).as_str()
        ))?;
        let path = self.target(&relative)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        checked_local_path(&self.root, &relative)?;
        file.try_lock_exclusive()
            .context("Installation content is locked by another worker")?;
        Ok(file)
    }

    fn require_space(&self, needed: u64) -> Result<()> {
        ensure!(
            fs2::available_space(&self.root)? >= needed,
            "Insufficient disk space for installation commit"
        );
        Ok(())
    }

    fn make_read_only(path: &Path) -> Result<()> {
        let file = open_for_verification(path)?;
        super::files::require_single_link_file(&file)?;
        let mut permissions = file.metadata()?.permissions();
        if !permissions.readonly() {
            permissions.set_readonly(true);
            std::fs::set_permissions(path, permissions)?;
        }
        Ok(())
    }

    fn verify_file<F: Fn() -> i64>(
        &self,
        execution: &InstallationExecution<'_, F>,
        manifest: &ModelManifest,
        id: &str,
        root: &Path,
        relative: &RepositoryPath,
    ) -> Result<()> {
        execution.checkpoint()?;
        let file = verify_artifact_file_with_progress(&manifest.plan, id, root, relative, |_| {
            execution.checkpoint().map(|_| ())
        })?;
        let expected = &manifest.installed_files[id];
        ensure!(
            file.sha256() == &expected.computed_sha256 && file.size_bytes() == expected.size_bytes,
            "Installation content differs from its verified manifest"
        );
        execution.checkpoint()?;
        Ok(())
    }

    fn copy_file<F: Fn() -> i64>(
        &self,
        execution: &InstallationExecution<'_, F>,
        manifest: &ModelManifest,
        id: &str,
        source_root: &Path,
        source: &RepositoryPath,
        destination: &RepositoryPath,
    ) -> Result<()> {
        let target = self.target(destination)?;
        ensure!(!target.exists(), "Installation destination already exists");
        self.require_space(manifest.installed_files[id].size_bytes)?;
        let mut input = open_for_verification(&checked_local_path(source_root, source)?)?;
        self.verify_file(execution, manifest, id, source_root, source)?;
        let parent = target
            .parent()
            .context("Installation destination has no parent")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        let mut buffer = vec![0; 1024 * 1024];
        let mut copied = 0u64;
        loop {
            execution.checkpoint()?;
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .context("Installation copy size overflow")?;
            ensure!(
                copied <= manifest.installed_files[id].size_bytes,
                "Installation source grew during copy"
            );
            temporary.write_all(&buffer[..count])?;
        }
        ensure!(
            copied == manifest.installed_files[id].size_bytes,
            "Installation source was truncated during copy"
        );
        temporary.as_file().sync_all()?;
        let temporary = temporary.into_temp_path();
        let temporary_name = RepositoryPath::parse(
            temporary
                .file_name()
                .context("Temporary installation file has no name")?
                .to_str()
                .context("Temporary installation name is not UTF-8")?,
        )?;
        self.verify_file(execution, manifest, id, parent, &temporary_name)?;
        execution.checkpoint()?;
        temporary.persist_noclobber(&target)?;
        Self::make_read_only(&target)?;
        sync_directory(parent)?;
        Ok(())
    }

    fn prepare_files<F: Fn() -> i64>(
        &self,
        execution: &InstallationExecution<'_, F>,
        manifest: &ModelManifest,
        transfer_root: &Path,
    ) -> Result<()> {
        let staging = installation_directory(manifest, false)?;
        self.directory(&staging)?;
        let transfers = execution
            .repository
            .file_transfers(&execution.lease.job_id)?;
        let manifest_bytes = serde_json::to_vec(manifest)?;
        ensure!(
            manifest_bytes.len() <= 16 * 1024 * 1024,
            "Installation manifest exceeds byte limit"
        );
        let mut remaining = manifest_bytes.len() as u64;
        for installed in manifest.installed_files.values() {
            for relative in [
                Self::blob_path(&installed.computed_sha256)?,
                RepositoryPath::parse(format!(
                    "{}/{}",
                    staging.as_str(),
                    installed.relative_path.as_str()
                ))?,
            ] {
                if !self.target(&relative)?.exists() {
                    remaining = remaining
                        .checked_add(installed.size_bytes)
                        .context("Installation disk budget overflow")?;
                }
            }
        }
        self.require_space(remaining)?;
        for (id, installed) in &manifest.installed_files {
            execution.checkpoint()?;
            let _blob_lock = self.lock("blobs", installed.computed_sha256.as_str())?;
            let blob = Self::blob_path(&installed.computed_sha256)?;
            let blob_path = self.target(&blob)?;
            if !blob_path.exists() {
                let transfer = transfers
                    .iter()
                    .find(|file| &file.artifact_id == id)
                    .context("Accepted transfer is missing")?;
                self.copy_file(
                    execution,
                    manifest,
                    id,
                    transfer_root,
                    &transfer.relative_path,
                    &blob,
                )?;
            }
            self.verify_file(execution, manifest, id, &self.root, &blob)?;
            Self::make_read_only(&blob_path)?;
            let staged = RepositoryPath::parse(format!(
                "{}/{}",
                staging.as_str(),
                installed.relative_path.as_str()
            ))?;
            let staged_path = self.target(&staged)?;
            if !staged_path.exists() {
                self.copy_file(execution, manifest, id, &self.root, &blob, &staged)?;
            }
            self.verify_file(execution, manifest, id, &self.root, &staged)?;
            Self::make_read_only(&staged_path)?;
        }
        let manifest_path = self.target(&RepositoryPath::parse(format!(
            "{}/manifest.json",
            staging.as_str()
        ))?)?;
        if !manifest_path.exists() {
            self.require_space(manifest_bytes.len() as u64)?;
            let directory = manifest_path
                .parent()
                .context("Manifest has no directory")?;
            let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
            temporary.write_all(&manifest_bytes)?;
            temporary.as_file().sync_all()?;
            execution.checkpoint()?;
            temporary
                .into_temp_path()
                .persist_noclobber(&manifest_path)?;
            sync_directory(directory)?;
        }
        Self::make_read_only(&manifest_path)?;
        sync_directory(&self.root.join(staging.as_str()))?;
        Ok(())
    }

    pub fn prepare(
        &self,
        repository: &LifecycleRepository,
        lease: &WorkerLease,
        expected_job_version: u64,
        manifest: &ModelManifest,
        transfer_root: &Path,
        clock: impl Fn() -> i64,
    ) -> Result<InstallationCommitJournal> {
        manifest.validate()?;
        let _version_lock = self.lock("versions", &manifest.version_id)?;
        let journal =
            repository.begin_installation_commit(lease, expected_job_version, manifest, clock())?;
        if journal.stage != InstallationCommitStage::Staging {
            return Ok(journal);
        }
        ensure!(
            self.existing(&installation_directory(manifest, true)?)?
                .is_none(),
            "Unprepared installation already has a published directory"
        );
        let execution = InstallationExecution {
            repository,
            lease,
            clock,
        };
        self.prepare_files(&execution, manifest, transfer_root)?;
        let witness =
            verify_installation_directory_with_progress(&self.root, manifest, false, |_| {
                execution.checkpoint().map(|_| ())
            })?;
        execution.advance(&witness, InstallationCommitStage::Prepared)
    }

    fn verify_blobs(
        &self,
        manifest: &ModelManifest,
        mut progress: impl FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        for (id, installed) in &manifest.installed_files {
            let blob = Self::blob_path(&installed.computed_sha256)?;
            let verified = verify_artifact_file_with_progress(
                &manifest.plan,
                id,
                &self.root,
                &blob,
                &mut progress,
            )?;
            ensure!(
                verified.sha256() == &installed.computed_sha256,
                "Shared installation blob is corrupt"
            );
            ensure!(
                Self::require_owned_read_only(&checked_local_path(&self.root, &blob)?)?,
                "Shared installation blob is writable"
            );
        }
        Ok(())
    }

    fn verify_read_only_version(&self, manifest: &ModelManifest) -> Result<()> {
        let directory = checked_local_path(&self.root, &installation_directory(manifest, true)?)?;
        let mut files: Vec<_> = manifest
            .installed_files
            .values()
            .map(|file| file.relative_path.clone())
            .collect();
        files.push(RepositoryPath::parse("manifest.json")?);
        for relative in files {
            ensure!(
                Self::require_owned_read_only(&checked_local_path(&directory, &relative)?)?,
                "Published installation content is writable"
            );
        }
        Ok(())
    }

    fn require_owned_read_only(path: &Path) -> Result<bool> {
        let file = open_for_verification(path)?;
        super::files::require_single_link_file(&file)?;
        Ok(file.metadata()?.permissions().readonly())
    }

    pub fn commit(
        &self,
        repository: &LifecycleRepository,
        lease: &WorkerLease,
        clock: impl Fn() -> i64,
    ) -> Result<InstalledVersion> {
        let mut journal = repository
            .installation_commit(&lease.job_id)?
            .context("Installation commit is missing")?;
        let _version_lock = self.lock("versions", &journal.manifest.version_id)?;
        let manifest = journal.manifest.clone();
        if journal.stage == InstallationCommitStage::Registered {
            let _witness = super::verify_installation_directory(&self.root, &manifest, true)?;
            self.verify_blobs(&manifest, |_| Ok(()))?;
            self.verify_read_only_version(&manifest)?;
            return repository
                .installed_version(&manifest.version_id)?
                .context("Registered installation version is missing");
        }
        let execution = InstallationExecution {
            repository,
            lease,
            clock,
        };
        execution.checkpoint()?;
        ensure!(
            journal.stage != InstallationCommitStage::Staging,
            "Installation preparation has not completed"
        );
        if journal.stage == InstallationCommitStage::Prepared {
            let final_relative = installation_directory(&manifest, true)?;
            let (parent, _) = final_relative
                .as_str()
                .rsplit_once('/')
                .context("Published version has no parent")?;
            self.directory(&RepositoryPath::parse(parent)?)?;
            let final_path = self.root.join(final_relative.as_str());
            let staging_relative = installation_directory(&manifest, false)?;
            let staging_path = self.root.join(staging_relative.as_str());
            if self.existing(&final_relative)?.is_none() {
                let witness = verify_installation_directory_with_progress(
                    &self.root,
                    &manifest,
                    false,
                    |_| execution.checkpoint().map(|_| ()),
                )?;
                drop(witness);
                execution.checkpoint()?;
                std::fs::rename(
                    checked_local_path(&self.root, &staging_relative)?,
                    &final_path,
                )?;
                sync_directory(
                    staging_path
                        .parent()
                        .context("Staged version has no parent")?,
                )?;
                sync_directory(
                    final_path
                        .parent()
                        .context("Published version has no parent")?,
                )?;
            } else {
                checked_local_path(&self.root, &final_relative)?;
                ensure!(
                    !staging_path.exists(),
                    "Both staged and published installation directories exist"
                );
            }
            let witness =
                verify_installation_directory_with_progress(&self.root, &manifest, true, |_| {
                    execution.checkpoint().map(|_| ())
                })?;
            journal = execution.advance(&witness, InstallationCommitStage::FilesCommitted)?;
        }
        ensure!(
            journal.stage == InstallationCommitStage::FilesCommitted,
            "Installation files have not been committed"
        );
        self.verify_blobs(&manifest, |_| execution.checkpoint().map(|_| ()))?;
        self.verify_read_only_version(&manifest)?;
        let witness =
            verify_installation_directory_with_progress(&self.root, &manifest, true, |_| {
                execution.checkpoint().map(|_| ())
            })?;
        execution.advance(&witness, InstallationCommitStage::Registered)?;
        repository
            .installed_version(&manifest.version_id)?
            .context("Installation registration did not produce a version")
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
