use crate::lifecycle::*;
use anyhow::{ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet};

pub const AUDIO_INSTALL_RULES_VERSION: &str = "modelscope-audio-cpp-install-v2";
pub const LEGACY_AUDIO_INSTALL_RULES_VERSION: &str = "modelscope-audio-cpp-install-only-v1";
#[cfg(test)]
use super::models::{WEIGHTS_REPOSITORY, WEIGHTS_REVISION};
const ENGINE_REPOSITORY: &str = "https://github.com/0xShug0/audio.cpp";
const ARCHIVE_SHA256: &str = "5203660c882af6ec21f370de18dcb91d1260eb0dc3fa6fe7c26cfd6339aa4788";

impl AudioTaskProfile {
    pub fn weight_repository_path(self) -> &'static str {
        self.external_weight()
            .map_or(self.record().weights.path, |(path, _, _)| path)
    }

    pub fn spec_repository_path(self) -> &'static str {
        self.record().spec.path
    }

    // Exact upstream files from the pinned audio.cpp archive, with its license alongside them.
    // Upstream package/download directives in these bytes are never used by the installer.
    pub(crate) fn runtime_spec_bytes(self) -> Result<&'static [u8]> {
        let bytes = self.record().spec_bytes;
        let (digest, size) = self.recorded_model_spec();
        ensure!(
            bytes.len() as u64 == size && Sha256Digest::of_bytes(bytes) == digest,
            "Bundled audio.cpp runtime spec differs from its pinned distribution"
        );
        Ok(bytes)
    }

    pub fn install_peak_bytes(self) -> u64 {
        // Transfer, blob and installed copy can coexist. Match the store's metadata allowance.
        (self.recorded_model().1 + self.recorded_model_spec().1) * 3 + 16 * 1024 * 1024
    }

    fn spec_id(self, model_snapshot_id: &str) -> Result<String> {
        let identity = Sha256Digest::of_serializable(&(
            LEGACY_AUDIO_INSTALL_RULES_VERSION,
            model_snapshot_id,
            ENGINE_REPOSITORY,
            &self.runtime_build().engine_revision,
            ARCHIVE_SHA256,
            self.spec_repository_path(),
            self.recorded_model_spec(),
        ))?;
        Ok(format!("audio-cpp-spec:{}", identity.as_str()))
    }

    pub(crate) fn matches_spec_artifact(self, artifact: &Artifact) -> bool {
        let (digest, size) = self.recorded_model_spec();
        artifact.path.as_str() == self.spec_repository_path()
            && artifact.source_sha256.as_ref() == Some(&digest)
            && artifact.size_bytes == Some(size)
            && self
                .spec_id(&artifact.snapshot_id)
                .is_ok_and(|id| id == artifact.id)
    }

    fn validate_model_snapshot(self, snapshot: &RepositorySnapshot) -> Result<()> {
        snapshot.validate()?;
        ensure!(
            snapshot.repository.as_str() == self.source().0
                && snapshot.resolved_revision == self.source().1,
            "Audio weight source does not match pinned ModelScope repository and revision"
        );
        Ok(())
    }

    /// The snapshot selects the model this engine-distribution file belongs to.
    /// Its path is within the pinned engine archive, never a ModelScope download path.
    pub fn runtime_spec_artifact(self, snapshot: &RepositorySnapshot) -> Result<Artifact> {
        self.validate_model_snapshot(snapshot)?;
        self.distribution_profile().distribution_artifact(snapshot)
    }

    fn distribution_profile(self) -> Self {
        match self.protocol() {
            AudioTaskProtocol::PocketTtsOffline => Self::PocketTtsEnglishCpu,
            AudioTaskProtocol::Qwen3AsrOffline => Self::Qwen3Asr06bCpu,
        }
    }

    pub(crate) fn distribution_artifact(self, snapshot: &RepositorySnapshot) -> Result<Artifact> {
        snapshot.validate()?;
        ensure!(
            Self::registered().any(|candidate| candidate.protocol() == self.protocol()
                && candidate.source()
                    == (
                        snapshot.repository.as_str(),
                        snapshot.resolved_revision.as_str()
                    )),
            "No registered audio source binds this snapshot to the distribution protocol"
        );
        self.runtime_spec_bytes()?;
        let snapshot_id = snapshot.content_digest()?.as_str().to_owned();
        let (digest, size) = self.recorded_model_spec();
        Ok(Artifact {
            id: self.spec_id(&snapshot_id)?,
            snapshot_id,
            path: RepositoryPath::parse(self.spec_repository_path())?,
            purpose: FilePurpose::RuntimeDistributionConfig(self),
            size_bytes: Some(size),
            source_sha256: Some(digest),
        })
    }

    fn accepts_distribution_artifact(
        self,
        artifact: &Artifact,
        snapshot: &RepositorySnapshot,
    ) -> Result<bool> {
        let FilePurpose::RuntimeDistributionConfig(origin) = artifact.purpose else {
            return Ok(false);
        };
        Ok(self.distribution_profile() == origin.distribution_profile()
            && *artifact == origin.distribution_artifact(snapshot)?)
    }

    fn for_weight(artifact: &Artifact, snapshot: &RepositorySnapshot) -> Result<Self> {
        snapshot.validate()?;
        artifact.validate()?;
        ensure!(
            artifact.purpose == FilePurpose::GgufWeight
                && artifact.snapshot_id == snapshot.content_digest()?.as_str(),
            "Audio weight is not bound to its immutable source"
        );
        Self::registered()
            .find(|profile| {
                let (sha, size) = profile.recorded_model();
                profile.source()
                    == (
                        snapshot.repository.as_str(),
                        snapshot.resolved_revision.as_str(),
                    )
                    && artifact.path.as_str() == profile.weight_repository_path()
                    && artifact.source_sha256.as_ref() == Some(&sha)
                    && artifact.size_bytes == Some(size)
            })
            .context("No recorded audio profile matches the weight path, digest and length")
    }

    pub(crate) fn for_installation_assets(plan: &InstallPlan) -> Result<Self> {
        let graph = &plan.dependencies;
        ensure!(
            plan.artifacts.len() == 2
                && graph.components.len() == 2
                && graph.edges.len() == 1
                && plan.snapshots.len() == 1,
            "Audio installation requires one main GGUF and one explicit spec"
        );
        let root = graph
            .components
            .get(&graph.root)
            .context("Missing audio model root")?;
        let edge = &graph.edges[0];
        ensure!(
            edge.from == graph.root && edge.requirement == DependencyRequirement::Required,
            "Audio model must require the runtime spec"
        );
        let spec_component = graph
            .components
            .get(&edge.to)
            .context("Missing audio spec component")?;
        let artifact_for = |component: &Component, role| -> Result<&Artifact> {
            ensure!(
                component.role == role
                    && component.artifact_ids.len() == 1
                    && component.compatibility == ComponentCompatibility::Verified,
                "Audio component role, count or compatibility differs"
            );
            plan.artifacts
                .get(
                    component
                        .artifact_ids
                        .first()
                        .expect("checked single artifact"),
                )
                .context("Missing audio artifact")
        };
        let weight = artifact_for(root, ComponentRole::Main)?;
        let spec = artifact_for(spec_component, ComponentRole::Metadata)?;
        let snapshot = plan
            .snapshots
            .get(&weight.snapshot_id)
            .context("Missing audio weight snapshot")?;
        let profile = Self::for_weight(weight, snapshot)?;
        ensure!(
            ((plan.rules_version == AUDIO_INSTALL_RULES_VERSION
                && plan.runtime == profile.runtime_build())
                || (plan.rules_version == LEGACY_AUDIO_INSTALL_RULES_VERSION
                    && plan.runtime == profile.legacy_runtime_build()))
                && plan.target_capabilities == BTreeSet::from([profile.capability()])
                && profile.accepts_distribution_artifact(spec, snapshot)?,
            "Audio runtime, capability or pinned distribution spec differs"
        );
        Ok(profile)
    }
}

impl LifecycleRepository {
    /// Freeze an installation disk budget. Runtime RAM, workload and input admission happen at execution.
    pub fn prepare_audio_install_plan(
        &self,
        observation: &ComponentObservation,
        available_disk_bytes: u64,
        now: i64,
    ) -> Result<InstallPlan> {
        observation.validate()?;
        ensure!(
            observation.role == ComponentRole::Main && observation.files.len() == 1,
            "Audio installation needs one main GGUF observation"
        );
        let file = &observation.files[0];
        let profile = AudioTaskProfile::for_weight(&file.artifact, &file.snapshot)?;
        let expected_spec = profile.runtime_spec_artifact(&file.snapshot)?;
        // An older store can already bind the same distribution file to a language-specific tag.
        let spec = self
            .catalog_artifact(&expected_spec.id)?
            .unwrap_or(expected_spec);
        ensure!(
            profile.accepts_distribution_artifact(&spec, &file.snapshot)?,
            "Stored distribution member differs from the selected audio protocol"
        );
        let source_identity = (AUDIO_INSTALL_RULES_VERSION, observation.digest()?, &spec.id);
        let identity = if profile.external_weight().is_some() {
            Sha256Digest::of_serializable(&(source_identity, profile.schema_id()))?
        } else {
            Sha256Digest::of_serializable(&source_identity)?
        };
        let model_id = format!("audio-model:{}", identity.as_str());
        let variant_id = format!("audio-variant:{}", identity.as_str());
        let main_id = format!("audio-main:{}", identity.as_str());
        let metadata_id = format!("audio-spec:{}", identity.as_str());
        let artifacts = BTreeMap::from([
            (file.artifact.id.clone(), file.artifact.clone()),
            (spec.id.clone(), spec.clone()),
        ]);
        let snapshots =
            BTreeMap::from([(file.artifact.snapshot_id.clone(), file.snapshot.clone())]);
        let evidence = BTreeSet::from([profile.evidence().to_owned()]);
        let component = |id: &str, role, artifact: String| Component {
            id: id.into(),
            role,
            artifact_ids: BTreeSet::from([artifact]),
            compatibility: ComponentCompatibility::Verified,
            evidence_ids: evidence.clone(),
        };
        let mut graph = DependencyGraph {
            root: main_id.clone(),
            components: BTreeMap::from([
                (
                    main_id.clone(),
                    component(&main_id, ComponentRole::Main, file.artifact.id.clone()),
                ),
                (
                    metadata_id.clone(),
                    component(&metadata_id, ComponentRole::Metadata, spec.id),
                ),
            ]),
            edges: vec![DependencyEdge {
                from: main_id.clone(),
                to: metadata_id.clone(),
                requirement: DependencyRequirement::Required,
            }],
        };
        graph
            .components
            .get_mut(&metadata_id)
            .expect("constructed spec")
            .evidence_ids
            .extend([
                format!(
                    "{ENGINE_REPOSITORY}/blob/{}/{}",
                    profile.runtime_build().engine_revision,
                    profile.spec_repository_path()
                ),
                format!("audio-cpp-source-archive-sha256:{ARCHIVE_SHA256}"),
                "audio-cpp-runtime-spec-license:Apache-2.0".into(),
            ]);
        let classification = classify_gguf(
            &file.artifact,
            &file.artifact.snapshot_id,
            &file.snapshot,
            &file.header,
            &[],
        )?;
        let catalog = ModelCatalogSnapshot {
            model: ModelDefinition {
                id: model_id.clone(),
                display_name: file.artifact.path.as_str().into(),
                snapshot_ids: snapshots.keys().cloned().collect(),
                architecture: classification.architecture,
                structure: classification.structure,
                task_families: classification.task_candidates,
                declared_capabilities: BTreeSet::new(),
            },
            variants: BTreeMap::from([(
                variant_id.clone(),
                ModelVariant {
                    id: variant_id.clone(),
                    model_id: model_id.clone(),
                    artifact_ids: BTreeSet::from([file.artifact.id.clone()]),
                    tensor_types: file.header.tensors.iter().map(|t| t.type_id).collect(),
                    quantization_label: file
                        .header
                        .tensors
                        .iter()
                        .map(|t| t.quantization.as_str())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(" + "),
                    evidence_ids: evidence,
                },
            )]),
            components: graph.components.clone(),
            artifacts: artifacts.clone(),
            snapshots: snapshots.clone(),
        };
        self.save_catalog(&catalog)?;
        let record = self.save_gguf_observation(file, now)?;
        graph
            .components
            .get_mut(&main_id)
            .expect("constructed root")
            .evidence_ids
            .extend([
                gguf_observation_evidence_id(&record.id),
                gguf_header_evidence_id(&file.artifact.id, &file.header.header_digest),
            ]);
        let plan = InstallPlan {
            schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
            id: format!("audio-plan:{}", uuid::Uuid::new_v4()),
            model_id,
            variant_id,
            target_capabilities: BTreeSet::from([profile.capability()]),
            snapshots,
            artifacts,
            dependencies: graph,
            runtime: profile.runtime_build(),
            runtime_verified: true,
            hardware: HardwareCompatibility::Unknown,
            required_disk_bytes: profile.install_peak_bytes(),
            rules_version: AUDIO_INSTALL_RULES_VERSION.into(),
            expires_at: now.checked_add(300).context("Plan expiry overflow")?,
            admission: None,
        };
        self.save_observed_plan(
            &plan,
            &BTreeMap::from([(file.artifact.id.clone(), record.id)]),
            now,
            available_disk_bytes,
        )?;
        Ok(plan)
    }
}

impl InstallationApplication {
    pub async fn prepare_audio_install_plan(
        &self,
        profile: AudioTaskProfile,
    ) -> Result<InstallPlan> {
        let token = self.modelscope_credentials().await?;
        let provider = ModelScopeProvider::new()?;
        let inventory = provider
            .inspect_repository(
                ModelScopeRepository::parse(profile.source().0)?,
                profile.source().1,
                token.as_deref(),
            )
            .await?
            .inventory;
        let path = RepositoryPath::parse(profile.weight_repository_path())?;
        let weight = inventory.artifact(&path, FilePurpose::GgufWeight)?;
        ensure!(
            AudioTaskProfile::for_weight(&weight, &inventory.snapshot)? == profile,
            "Audio source changed"
        );
        let component = provider
            .inspect_selected_component(
                &inventory,
                &[path],
                ComponentRole::Main,
                GgufReadLimits::default(),
                token.as_deref(),
            )
            .await?;
        let service = self.service().await?;
        // A long startup recovery must finish before the plan's submission window begins.
        service.wait_ready().await?;
        tokio::task::spawn_blocking(move || {
            service.repository().prepare_audio_install_plan(
                &component.observation,
                fs2::available_space(service.store().root())?,
                chrono::Utc::now().timestamp(),
            )
        })
        .await
        .context("Audio installation plan preparation task failed")?
    }
}

#[cfg(test)]
#[path = "../../../tests/support/audio_install_plan.rs"]
mod tests;
