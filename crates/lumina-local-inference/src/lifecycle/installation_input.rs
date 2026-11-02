use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::{
    install_from_local_cache, install_from_modelscope, CacheInstallRequest, InstallPlan,
    InstallationStore, InstalledVersion, LifecycleRepository, ModelScopeInstallRequest,
    Sha256Digest, PREPARED_GGUF_PLAN_RULES_VERSION,
};

pub const INSTALLATION_INPUT_RULES_VERSION: &str = "installation-input-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstallationSource {
    ModelScope {},
    LocalCache { files: BTreeMap<String, PathBuf> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationInput {
    pub installation_id: String,
    pub version_id: String,
    pub store_root: PathBuf,
    pub source: InstallationSource,
}

impl InstallationInput {
    pub fn new(
        store: &InstallationStore,
        installation_id: String,
        version_id: String,
        source: InstallationSource,
    ) -> Result<Self> {
        let input = Self {
            installation_id,
            version_id,
            store_root: store.root().to_path_buf(),
            source,
        };
        input.validate()?;
        Ok(input)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            [&self.installation_id, &self.version_id]
                .iter()
                .all(|id| !id.trim().is_empty()
                    && id.len() <= 4096
                    && !id.chars().any(char::is_control)),
            "Installation input has an invalid installation or version identity"
        );
        ensure!(
            self.store_root.is_absolute(),
            "Installation store must be absolute"
        );
        if let InstallationSource::LocalCache { files } = &self.source {
            ensure!(
                !files.is_empty() && files.values().all(|path| path.is_absolute()),
                "Cache input requires absolute source file paths"
            );
        }
        Ok(())
    }

    pub(crate) fn validate_plan(&self, plan: &InstallPlan) -> Result<()> {
        self.validate()?;
        ensure!(
            plan.rules_version == PREPARED_GGUF_PLAN_RULES_VERSION,
            "Installation input requires an accepted frozen GGUF plan"
        );
        if let InstallationSource::LocalCache { files } = &self.source {
            ensure!(
                plan.artifacts.keys().eq(files.keys())
                    && plan
                        .artifacts
                        .values()
                        .all(|artifact| artifact.source_sha256.is_some()),
                "Cache input differs from the pinned source selection"
            );
        }
        Ok(())
    }

    pub fn validate_destination(
        &self,
        store: &InstallationStore,
        installation_id: &str,
        version_id: &str,
    ) -> Result<()> {
        ensure!(
            self.store_root == store.root()
                && self.installation_id == installation_id
                && self.version_id == version_id,
            "Installation request differs from its durable destination"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationIntent {
    pub rules_version: String,
    pub job_id: String,
    pub plan_digest: Sha256Digest,
    pub bound_at: i64,
    pub input: InstallationInput,
}

#[derive(Debug, Clone)]
pub struct JobAcceptance {
    pub id: String,
    pub idempotency_key: String,
    pub plan_id: String,
    pub plan_digest: Sha256Digest,
}

pub async fn resume_installation<F>(
    repository: Arc<LifecycleRepository>,
    store: Arc<InstallationStore>,
    job_id: String,
    token: Option<String>,
    worker_name: String,
    clock: F,
) -> Result<InstalledVersion>
where
    F: Fn() -> i64 + Send + 'static,
{
    let intent = repository
        .read_installation_input(&job_id)?
        .context("Legacy installation requires explicit input adoption before recovery")?;
    intent.input.validate_destination(
        &store,
        &intent.input.installation_id,
        &intent.input.version_id,
    )?;
    match intent.input.source {
        InstallationSource::ModelScope {} => {
            install_from_modelscope(
                repository,
                store,
                ModelScopeInstallRequest {
                    job_id,
                    installation_id: intent.input.installation_id,
                    version_id: intent.input.version_id,
                },
                token,
                worker_name,
                clock,
            )
            .await
        }
        InstallationSource::LocalCache { files } => tokio::task::spawn_blocking(move || {
            install_from_local_cache(
                &repository,
                &store,
                &CacheInstallRequest {
                    job_id,
                    installation_id: intent.input.installation_id,
                    version_id: intent.input.version_id,
                    source_files: files,
                },
                &worker_name,
                clock,
            )
        })
        .await
        .context("Cache installation recovery worker stopped")?,
    }
}
