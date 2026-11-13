use crate::lifecycle::*;
use anyhow::{ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet};

pub const GENERATION_INSTALL_RULES_VERSION: &str = "modelscope-diffusion-install-only-v2";
pub const LEGACY_GENERATION_INSTALL_RULES_VERSION: &str = "modelscope-diffusion-install-only-v1";

use super::inventory::WeightSource;

impl WeightSource {
    fn component_role(&self) -> ComponentRole {
        match self.role {
            GenerationAssetRole::Main => ComponentRole::Main,
            GenerationAssetRole::TextEncoder => ComponentRole::TextEncoder,
            GenerationAssetRole::VideoVae => ComponentRole::Vae,
            GenerationAssetRole::AudioVae => ComponentRole::AudioDecoder,
        }
    }

    fn verify(&self, artifact: &Artifact, snapshot: &RepositorySnapshot) -> Result<()> {
        ensure!(
            artifact.purpose == FilePurpose::GgufWeight
                && artifact.path.as_str() == self.file
                && artifact.id
                    == modelscope::source_artifact_id(
                        &artifact.snapshot_id,
                        &artifact.path,
                        artifact.purpose
                    )?
                && artifact.snapshot_id == snapshot.content_digest()?.as_str()
                && snapshot.repository.as_str() == self.repository
                && snapshot.resolved_revision == self.revision
                && artifact.source_sha256.as_ref() == Some(&self.sha256)
                && artifact.size_bytes == Some(self.size_bytes),
            "Generation component differs from its pinned source: {}",
            self.file
        );
        Ok(())
    }
}

impl GenerationProfile {
    fn install_peak_bytes(self, rules: &str) -> u64 {
        let copies = if rules == GENERATION_INSTALL_RULES_VERSION {
            2
        } else {
            3
        };
        self.recorded_assets()
            .values()
            .map(|(_, size)| size * copies)
            .sum::<u64>()
            + 16 * 1024 * 1024
    }

    pub(crate) fn validate_installation_assets(plan: &InstallPlan) -> Result<()> {
        let profile = Self::registered()
            .find(|profile| {
                plan.runtime == profile.runtime_build()
                    && plan.dependencies.components.get(&plan.dependencies.root)
                        .is_some_and(|root| root.evidence_ids.contains(profile.evidence()))
                    && plan.target_capabilities == profile.required_capabilities()
                    && plan.artifacts.len() == profile.weight_sources().len()
                    && profile.weight_sources().iter().all(|source| {
                        plan.artifacts.values().any(|artifact| {
                            plan.snapshots
                                .get(&artifact.snapshot_id)
                                .is_some_and(|snapshot| source.verify(artifact, snapshot).is_ok())
                        })
                    })
            })
            .context("No registered generation model matches this engine, capability set and component inventory")?;
        let sources = profile.weight_sources();
        let graph = &plan.dependencies;
        ensure!(
            plan.admission.is_none()
                && plan.hardware == HardwareCompatibility::Unknown
                && plan.required_disk_bytes == profile.install_peak_bytes(&plan.rules_version)
                && graph.components.len() == sources.len()
                && graph.edges.len() + 1 == sources.len(),
            "Generation installation has invalid capabilities, component count or budget"
        );
        let mut snapshot_ids = BTreeSet::new();
        for source in sources {
            let component = graph
                .components
                .values()
                .find(|c| c.role == source.component_role())
                .context("Required generation component missing")?;
            ensure!(
                component.artifact_ids.len() == 1
                    && component.compatibility == ComponentCompatibility::Verified,
                "Generation component requires exactly one verified artifact"
            );
            let artifact = plan
                .artifacts
                .get(component.artifact_ids.first().unwrap())
                .context("Missing generation artifact")?;
            let snapshot = plan
                .snapshots
                .get(&artifact.snapshot_id)
                .context("Missing generation source")?;
            source.verify(artifact, snapshot)?;
            snapshot_ids.insert(&artifact.snapshot_id);
            if source.role == GenerationAssetRole::Main {
                ensure!(component.id == graph.root, "Wrong generation root");
            } else {
                ensure!(
                    graph.edges.iter().any(|e| e.from == graph.root
                        && e.to == component.id
                        && e.requirement == DependencyRequirement::Required),
                    "Generation dependency must be required by the root"
                );
            }
        }
        ensure!(
            snapshot_ids.len() == plan.snapshots.len(),
            "Unreferenced generation repository snapshot"
        );
        Ok(())
    }
}

impl InstallationApplication {
    pub async fn prepare_generation_install_plan(
        &self,
        profile: GenerationProfile,
    ) -> Result<InstallPlan> {
        let token = self.modelscope_credentials().await?;
        let provider = ModelScopeProvider::new()?;
        let mut inventories = BTreeMap::new();
        let mut observations = BTreeMap::new();
        for source in profile.weight_sources() {
            let key = (source.repository.as_str(), source.revision.as_str());
            let inventory = match inventories.entry(key) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                    provider
                        .inspect_repository(
                            ModelScopeRepository::parse(source.repository.as_str())?,
                            &source.revision,
                            token.as_deref(),
                        )
                        .await?
                        .inventory,
                ),
            };
            let path = RepositoryPath::parse(source.file.as_str())?;
            source.verify(
                &inventory.artifact(&path, FilePurpose::GgufWeight)?,
                &inventory.snapshot,
            )?;
            let inspected = provider
                .inspect_selected_component(
                    inventory,
                    &[path],
                    source.component_role(),
                    GgufReadLimits::default(),
                    token.as_deref(),
                )
                .await?;
            observations.insert(source.component_role(), inspected.observation);
        }
        let service = self.service().await?;
        service.wait_ready().await?;
        tokio::task::spawn_blocking(move || {
            freeze_plan(
                service.repository(),
                profile,
                observations,
                fs2::available_space(service.store().root())?,
                chrono::Utc::now().timestamp(),
            )
        })
        .await
        .context("Generation installation plan worker failed")?
    }
}

fn freeze_plan(
    repository: &LifecycleRepository,
    profile: GenerationProfile,
    observations: BTreeMap<ComponentRole, ComponentObservation>,
    available: u64,
    now: i64,
) -> Result<InstallPlan> {
    let sources = profile.weight_sources();
    ensure!(
        observations.len() == sources.len(),
        "Incomplete generation component selection"
    );
    let mut artifacts = BTreeMap::new();
    let mut snapshots = BTreeMap::new();
    let mut components = BTreeMap::new();
    let mut edges = Vec::new();
    let mut bindings = BTreeMap::new();
    let mut tensor_types = BTreeSet::new();
    let mut quantizations = BTreeSet::new();
    let root = observations
        .get(&ComponentRole::Main)
        .context("Missing main generation component")?;
    root.validate()?;
    let main = root
        .files
        .first()
        .context("Missing main generation weight")?;
    let identities = observations
        .values()
        .map(ComponentObservation::digest)
        .collect::<Result<Vec<_>>>()?;
    let source_identity = (
        LEGACY_GENERATION_INSTALL_RULES_VERSION,
        profile.runtime_build(),
        identities,
    );
    let identity = if profile.is_external() {
        Sha256Digest::of_serializable(&(source_identity, profile.parameter_schema_id()))?
    } else {
        Sha256Digest::of_serializable(&source_identity)?
    };
    let component_id = |artifact_id: &str| {
        if profile.is_external() {
            format!("generation-component:{}:{artifact_id}", identity.as_str())
        } else {
            format!("generation-component:{artifact_id}")
        }
    };
    let root_id = component_id(&main.artifact.id);
    for source in sources {
        let observation = observations
            .get(&source.component_role())
            .context("Missing generation observation")?;
        observation.validate()?;
        ensure!(
            observation.role == source.component_role() && observation.files.len() == 1,
            "Wrong generation component selection"
        );
        let file = &observation.files[0];
        source.verify(&file.artifact, &file.snapshot)?;
        snapshots.insert(file.artifact.snapshot_id.clone(), file.snapshot.clone());
        artifacts.insert(file.artifact.id.clone(), file.artifact.clone());
        let id = component_id(&file.artifact.id);
        components.insert(
            id.clone(),
            Component {
                id: id.clone(),
                role: observation.role,
                artifact_ids: BTreeSet::from([file.artifact.id.clone()]),
                compatibility: ComponentCompatibility::Verified,
                evidence_ids: BTreeSet::from([
                    profile.evidence().into(),
                    gguf_header_evidence_id(&file.artifact.id, &file.header.header_digest),
                ]),
            },
        );
        if id != root_id {
            edges.push(DependencyEdge {
                from: root_id.clone(),
                to: id,
                requirement: DependencyRequirement::Required,
            });
        }
        for tensor in &file.header.tensors {
            tensor_types.insert(tensor.type_id);
            quantizations.insert(tensor.quantization.as_str());
        }
    }
    let classification = classify_gguf(
        &main.artifact,
        &main.artifact.snapshot_id,
        &main.snapshot,
        &main.header,
        &[],
    )?;
    let rules = if cfg!(windows) {
        GENERATION_INSTALL_RULES_VERSION
    } else {
        LEGACY_GENERATION_INSTALL_RULES_VERSION
    };
    let model_id = format!("generation-model:{}", identity.as_str());
    let variant_id = format!("generation-variant:{}", identity.as_str());
    let catalog = ModelCatalogSnapshot {
        model: ModelDefinition {
            id: model_id.clone(),
            display_name: main.snapshot.repository.as_str().into(),
            snapshot_ids: snapshots.keys().cloned().collect(),
            architecture: classification.architecture,
            structure: classification.structure,
            task_families: BTreeSet::from([if profile.is_image() {
                ModelTaskFamily::ImageDiffusion
            } else {
                ModelTaskFamily::VideoDiffusion
            }]),
            declared_capabilities: BTreeSet::new(),
        },
        variants: BTreeMap::from([(
            variant_id.clone(),
            ModelVariant {
                id: variant_id.clone(),
                model_id: model_id.clone(),
                artifact_ids: artifacts.keys().cloned().collect(),
                quantization_label: quantizations.into_iter().collect::<Vec<_>>().join(" + "),
                tensor_types,
                evidence_ids: BTreeSet::from([profile.evidence().into()]),
            },
        )]),
        components: components.clone(),
        artifacts: artifacts.clone(),
        snapshots: snapshots.clone(),
    };
    repository.save_catalog(&catalog)?;
    for observation in observations.values() {
        let file = &observation.files[0];
        let record = repository.save_gguf_observation(file, now)?;
        let id = component_id(&file.artifact.id);
        components
            .get_mut(&id)
            .context("Missing generation component")?
            .evidence_ids
            .insert(gguf_observation_evidence_id(&record.id));
        bindings.insert(file.artifact.id.clone(), record.id);
    }
    let plan = InstallPlan {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        id: format!("generation-plan:{}", uuid::Uuid::new_v4()),
        model_id,
        variant_id,
        target_capabilities: profile.required_capabilities(),
        snapshots,
        artifacts,
        dependencies: DependencyGraph {
            root: root_id,
            components,
            edges,
        },
        runtime: profile.runtime_build(),
        runtime_verified: true,
        hardware: HardwareCompatibility::Unknown,
        required_disk_bytes: profile.install_peak_bytes(rules),
        rules_version: rules.into(),
        expires_at: now.checked_add(300).context("Plan expiry overflow")?,
        admission: None,
    };
    repository.save_observed_plan(&plan, &bindings, now, available)?;
    Ok(plan)
}

#[cfg(test)]
#[path = "../../../tests/support/generation_install_plan.rs"]
mod tests;
