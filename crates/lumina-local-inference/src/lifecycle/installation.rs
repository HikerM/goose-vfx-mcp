use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use super::{
    FilePurpose, FileTransferRecord, FileTransferState, InstallPlan, InstalledArtifact,
    ModelManifest, RepositoryPath, Sha256Digest, MODEL_LIFECYCLE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationCommitStage {
    Staging,
    Prepared,
    FilesCommitted,
    Registered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationCommitJournal {
    pub job_id: String,
    pub manifest: ModelManifest,
    pub manifest_digest: Sha256Digest,
    pub stage: InstallationCommitStage,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledVersion {
    pub manifest: ModelManifest,
    pub manifest_digest: Sha256Digest,
    pub registered_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveInstallation {
    pub installation_id: String,
    pub active_version_id: String,
    pub model_id: String,
    pub variant_id: String,
    pub manifest_digest: Sha256Digest,
}

pub fn installed_artifact_path(
    artifact_id: &str,
    purpose: FilePurpose,
    source: &RepositoryPath,
) -> Result<RepositoryPath> {
    let extension = if purpose == FilePurpose::GgufWeight {
        "gguf"
    } else {
        source
            .as_str()
            .rsplit_once('.')
            .context("Metadata has no file extension")?
            .1
    };
    RepositoryPath::parse(format!(
        "files/{}.{}",
        Sha256Digest::of_bytes(artifact_id.as_bytes()).as_str(),
        extension
    ))
}

pub fn build_installation_manifest(
    installation_id: String,
    version_id: String,
    plan: InstallPlan,
    files: &[FileTransferRecord],
    accepted_at: i64,
    prepared_at: i64,
) -> Result<ModelManifest> {
    let mut installed_files = BTreeMap::new();
    for file in files {
        file.validate()?;
        let artifact = plan
            .artifacts
            .get(&file.artifact_id)
            .context("Verified transfer does not belong to the installation plan")?;
        let FileTransferState::Verified {
            computed_sha256,
            evidence_digest,
        } = &file.state
        else {
            anyhow::bail!("Installation requires verified files");
        };
        ensure!(
            installed_files
                .insert(
                    file.artifact_id.clone(),
                    InstalledArtifact {
                        artifact_id: file.artifact_id.clone(),
                        relative_path: installed_artifact_path(
                            &file.artifact_id,
                            artifact.purpose,
                            &artifact.path
                        )?,
                        computed_sha256: computed_sha256.clone(),
                        size_bytes: file.expected_bytes,
                        verification_evidence: evidence_digest.clone(),
                    }
                )
                .is_none(),
            "Duplicate installation artifact"
        );
    }
    let manifest = ModelManifest {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        installation_id,
        version_id,
        plan,
        installed_files,
        plan_accepted_at: accepted_at,
        installed_at: prepared_at,
    };
    manifest.validate()?;
    Ok(manifest)
}

pub fn installation_directory(manifest: &ModelManifest, committed: bool) -> Result<RepositoryPath> {
    let parent = if committed {
        "installations"
    } else {
        "staging/commits"
    };
    RepositoryPath::parse(format!(
        "{parent}/{}/{}",
        Sha256Digest::of_bytes(manifest.installation_id.as_bytes()).as_str(),
        Sha256Digest::of_bytes(manifest.version_id.as_bytes()).as_str()
    ))
}

#[derive(Debug)]
pub struct InstallationFilesWitness {
    pub(crate) installation_id: String,
    pub(crate) version_id: String,
    pub(crate) manifest_digest: Sha256Digest,
    pub(crate) committed: bool,
    _guards: Vec<std::fs::File>,
}

pub fn verify_installation_directory(
    root: &Path,
    manifest: &ModelManifest,
    committed: bool,
) -> Result<InstallationFilesWitness> {
    verify_installation_directory_with_progress(root, manifest, committed, |_| Ok(()))
}

pub(crate) fn verify_installation_directory_with_progress(
    root: &Path,
    manifest: &ModelManifest,
    committed: bool,
    mut progress: impl FnMut(u64) -> Result<()>,
) -> Result<InstallationFilesWitness> {
    manifest.validate()?;
    let relative = installation_directory(manifest, committed)?;
    let directory = super::files::checked_local_path(root, &relative)?;
    ensure!(
        directory.is_dir(),
        "Installation directory is not a directory"
    );
    let manifest_path =
        super::files::checked_local_path(&directory, &RepositoryPath::parse("manifest.json")?)?;
    let mut manifest_file = super::files::open_for_verification(&manifest_path)?;
    ensure!(
        manifest_file.metadata()?.is_file(),
        "Installation manifest is not a regular file"
    );
    let mut bytes = Vec::new();
    manifest_file
        .by_ref()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Installation manifest exceeds byte limit"
    );
    let actual: ModelManifest = serde_json::from_slice(&bytes)?;
    actual.validate()?;
    let digest = manifest.digest()?;
    ensure!(
        actual.digest()? == digest,
        "Installation manifest differs from the prepared journal"
    );
    let mut guards = vec![manifest_file];
    for component in manifest.plan.dependencies.components.values() {
        let mut weights = Vec::new();
        let mut header_bytes = 0u64;
        for id in &component.artifact_ids {
            let installed = &manifest.installed_files[id];
            let path = super::files::checked_local_path(&directory, &installed.relative_path)?;
            guards.push(super::files::open_for_verification(&path)?);
            let verified = super::files::verify_artifact_file_with_progress(
                &manifest.plan,
                id,
                &directory,
                &installed.relative_path,
                &mut progress,
            )?;
            ensure!(
                verified.sha256() == &installed.computed_sha256
                    && verified.size_bytes() == installed.size_bytes,
                "Installation file differs from its manifest"
            );
            if let Some(inspection) = verified.gguf {
                header_bytes = header_bytes
                    .checked_add(inspection.header_bytes)
                    .context("Component GGUF header byte count overflow")?;
                ensure!(
                    header_bytes <= 128 * 1024 * 1024,
                    "Component GGUF headers exceed the inspection budget"
                );
                weights.push(inspection);
            }
        }
        if !weights.is_empty() {
            super::validate_gguf_shard_set(&weights)
                .with_context(|| format!("Invalid GGUF component: {}", component.id))?;
        }
    }
    Ok(InstallationFilesWitness {
        installation_id: manifest.installation_id.clone(),
        version_id: manifest.version_id.clone(),
        manifest_digest: digest,
        committed,
        _guards: guards,
    })
}
