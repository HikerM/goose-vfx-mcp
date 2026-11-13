//! Describes finite tasks from registered installations. Discovery does not load a
//! model or issue resource admission. Preparation selects an exact adapter identity.

pub use super::task_parameters::NativeTaskIntent;
use super::{
    AudioTaskModelBinding, AudioTaskProfile, AudioTaskProtocol, GenerationModelBinding,
    GenerationParameterDescriptor, GenerationParameterService, GenerationProfile, InstalledVersion,
    LifecycleRepository, ModelTaskParameterAdapter, NativeTaskPlan, PocketTtsDescriptor,
    PocketTtsParameterService, Qwen3AsrDescriptor, Qwen3AsrParameterService, TaskParameterIdentity,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

mod media_fields;
pub use media_fields::SessionMediaParameterKind;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "family", content = "parameters", rename_all = "snake_case")]
pub enum NativeTaskParameterContract {
    Generation(Box<GenerationParameterDescriptor>),
    PocketTts(Box<PocketTtsDescriptor>),
    Qwen3Asr(Box<Qwen3AsrDescriptor>),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledTaskDescription {
    pub identity: TaskParameterIdentity,
    pub intent: NativeTaskIntent,
    pub descriptor: NativeTaskParameterContract,
}

enum Adapter {
    Generation(Box<GenerationParameterService>),
    PocketTts(Box<PocketTtsParameterService>),
    Qwen3Asr(Box<Qwen3AsrParameterService>),
}

impl Adapter {
    fn identity(&self) -> Result<TaskParameterIdentity> {
        Ok(match self {
            Self::Generation(service) => service.identity()?,
            Self::PocketTts(service) => service.identity()?,
            Self::Qwen3Asr(service) => service.identity()?,
        })
    }

    fn contract(&self) -> NativeTaskParameterContract {
        match self {
            Self::Generation(service) => {
                NativeTaskParameterContract::Generation(Box::new(service.describe().clone()))
            }
            Self::PocketTts(service) => {
                NativeTaskParameterContract::PocketTts(Box::new(service.describe().clone()))
            }
            Self::Qwen3Asr(service) => {
                NativeTaskParameterContract::Qwen3Asr(Box::new(service.describe().clone()))
            }
        }
    }

    fn prepare(&self, request: Value) -> Result<NativeTaskPlan> {
        Ok(match self {
            Self::Generation(service) => service.compile_json(request)?.into(),
            Self::PocketTts(service) => service.compile_json(request)?.into(),
            Self::Qwen3Asr(service) => service.compile_json(request)?.into(),
        })
    }
}

#[derive(Default)]
pub struct NativeTaskCatalog {
    entries: BTreeMap<TaskParameterIdentity, (NativeTaskIntent, Adapter)>,
}

impl NativeTaskCatalog {
    /// Snapshot the versions activated in the managed repository. A version with
    /// no supported component/runtime combination contributes no descriptors.
    pub fn active(repository: &LifecycleRepository) -> Result<Self> {
        let mut catalog = Self::default();
        for active in repository.active_installations()? {
            let version = repository
                .installed_version(&active.active_version_id)?
                .context("Active installation version disappeared")?;
            ensure!(
                version.manifest.installation_id == active.installation_id
                    && version.manifest_digest == active.manifest_digest,
                "Active version differs from its registered installation"
            );
            catalog.register_version(&version)?;
        }
        Ok(catalog)
    }

    /// For a pinned session or replay, select its exact registered version even
    /// when a different version has since become active.
    pub fn for_version(
        repository: &LifecycleRepository,
        installation_id: &str,
        version_id: &str,
    ) -> Result<Self> {
        let version = repository
            .installed_version(version_id)?
            .context("Task version is not registered")?;
        ensure!(
            version.manifest.installation_id == installation_id,
            "Task version belongs to a different installation"
        );
        let mut catalog = Self::default();
        catalog.register_version(&version)?;
        Ok(catalog)
    }

    pub fn list(&self, intent: NativeTaskIntent) -> Vec<InstalledTaskDescription> {
        self.list_matching(Some(intent))
    }

    pub fn list_matching(&self, intent: Option<NativeTaskIntent>) -> Vec<InstalledTaskDescription> {
        self.entries
            .iter()
            .filter(|(_, (entry_intent, _))| intent.is_none_or(|filter| filter == *entry_intent))
            .map(
                |(identity, (entry_intent, adapter))| InstalledTaskDescription {
                    identity: identity.clone(),
                    intent: *entry_intent,
                    descriptor: adapter.contract(),
                },
            )
            .collect()
    }

    pub fn describe(&self, identity: &TaskParameterIdentity) -> Result<InstalledTaskDescription> {
        let (intent, adapter) = self
            .entries
            .get(identity)
            .context("No registered native task matches the complete parameter identity")?;
        Ok(InstalledTaskDescription {
            identity: identity.clone(),
            intent: *intent,
            descriptor: adapter.contract(),
        })
    }

    pub fn prepare(
        &self,
        identity: &TaskParameterIdentity,
        request: Value,
    ) -> Result<NativeTaskPlan> {
        let (_, adapter) = self
            .entries
            .get(identity)
            .context("No registered native task matches the complete parameter identity")?;
        adapter.prepare(request)
    }

    fn register(&mut self, intent: NativeTaskIntent, adapter: Adapter) -> Result<()> {
        let key = adapter.identity()?;
        ensure!(
            !self.entries.contains_key(&key),
            "Duplicate installed native task identity"
        );
        self.entries.insert(key, (intent, adapter));
        Ok(())
    }

    fn register_version(&mut self, version: &InstalledVersion) -> Result<()> {
        let manifest = &version.manifest;
        ensure!(
            manifest.digest()? == version.manifest_digest,
            "Installed task manifest digest changed"
        );
        manifest.validate()?;
        let runtime = &manifest.plan.runtime;
        if let Ok(profile) = AudioTaskProfile::for_installation_assets(&manifest.plan) {
            let binding = AudioTaskModelBinding::from_manifest(manifest)?;
            match profile.protocol() {
                AudioTaskProtocol::PocketTtsOffline => {
                    if let Ok(service) = PocketTtsParameterService::for_profile(profile, binding) {
                        self.register(
                            NativeTaskIntent::SpeechSynthesis,
                            Adapter::PocketTts(Box::new(service)),
                        )?;
                    }
                }
                AudioTaskProtocol::Qwen3AsrOffline => {
                    if let Ok(service) = Qwen3AsrParameterService::for_profile(profile, binding) {
                        self.register(
                            NativeTaskIntent::Transcription,
                            Adapter::Qwen3Asr(Box::new(service)),
                        )?;
                    }
                }
            }
            return Ok(());
        }
        let profiles = GenerationProfile::registered().filter(|p| &p.runtime_build() == runtime);
        let mut profiles = profiles.peekable();
        if profiles.peek().is_none() {
            return Ok(());
        }
        let Ok(binding) = GenerationModelBinding::from_manifest(manifest) else {
            return Ok(());
        };
        for profile in profiles {
            let Ok(service) = GenerationParameterService::new(profile, binding.clone()) else {
                continue;
            };
            self.register(profile.intent(), Adapter::Generation(Box::new(service)))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
