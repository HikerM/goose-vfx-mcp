use anyhow::{ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet};

use super::*;

fn profile_for_artifact(artifact: &Artifact) -> Result<VectorRuntimeProfile> {
    artifact.validate()?;
    [
        VectorRuntimeProfile::Qwen3Embedding06bQ8,
        VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
    ]
    .into_iter()
    .find(|profile| {
        artifact.purpose == FilePurpose::GgufWeight
            && artifact.size_bytes == Some(profile.model_bytes())
            && artifact
                .source_sha256
                .as_ref()
                .is_some_and(|hash| hash.as_str() == profile.model_sha256())
    })
    .context("Selected model content has no measured vector runtime profile")
}

impl LifecycleRepository {
    pub fn prepare_vector_plan(
        &self,
        observation: &ComponentObservation,
        hardware: &HardwareResourceSnapshot,
        now: i64,
    ) -> Result<ObservedPlanAssessment> {
        observation.validate()?;
        ensure!(
            observation.role == ComponentRole::Main && observation.files.len() == 1,
            "Vector planning requires exactly one main GGUF file"
        );
        let file = &observation.files[0];
        let profile = profile_for_artifact(&file.artifact)?;
        let runtime_profile = baseline_vector_install_profile(profile)?;
        let envelope =
            baseline_vector_resource_envelope(profile, std::slice::from_ref(observation))?;
        let classification = classify_gguf(
            &file.artifact,
            &file.artifact.snapshot_id,
            &file.snapshot,
            &file.header,
            &[],
        )?;
        let identity = Sha256Digest::of_serializable(&(observation.digest()?, &runtime_profile))?;
        let model_id = format!("vector-model:{}", identity.as_str());
        let variant_id = format!("vector-variant:{}", identity.as_str());
        let component_id = format!("vector-component:{}", identity.as_str());
        let evidence_ids = runtime_profile.evidence_ids.clone();
        let artifact_ids = BTreeSet::from([file.artifact.id.clone()]);
        let catalog = ModelCatalogSnapshot {
            model: ModelDefinition {
                id: model_id.clone(),
                display_name: file.snapshot.repository.as_str().into(),
                snapshot_ids: BTreeSet::from([file.artifact.snapshot_id.clone()]),
                architecture: classification.architecture,
                structure: classification.structure,
                task_families: classification.task_candidates,
                declared_capabilities: BTreeSet::new(),
            },
            snapshots: BTreeMap::from([(file.artifact.snapshot_id.clone(), file.snapshot.clone())]),
            artifacts: BTreeMap::from([(file.artifact.id.clone(), file.artifact.clone())]),
            variants: BTreeMap::from([(
                variant_id.clone(),
                ModelVariant {
                    id: variant_id.clone(),
                    model_id: model_id.clone(),
                    artifact_ids: artifact_ids.clone(),
                    quantization_label: file
                        .header
                        .tensors
                        .iter()
                        .map(|tensor| tensor.quantization.clone())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(" + "),
                    tensor_types: file
                        .header
                        .tensors
                        .iter()
                        .map(|tensor| tensor.type_id)
                        .collect(),
                    evidence_ids: evidence_ids.clone(),
                },
            )]),
            components: BTreeMap::from([(
                component_id.clone(),
                Component {
                    id: component_id.clone(),
                    role: ComponentRole::Main,
                    artifact_ids,
                    compatibility: ComponentCompatibility::Verified,
                    evidence_ids,
                },
            )]),
        };
        self.save_catalog(&catalog)?;
        self.save_gguf_observation(file, now)?;
        self.prepare_observed_plan(
            &ObservedPlanRequest {
                id: format!("vector-plan:{}", uuid::Uuid::new_v4()),
                model_id,
                variant_id,
                target_capabilities: runtime_profile.capabilities.clone(),
                dependencies: DependencyGraph {
                    root: component_id,
                    components: catalog.components,
                    edges: vec![],
                },
            },
            &runtime_profile,
            &envelope,
            hardware,
            now,
        )
    }
}

impl InstallationApplication {
    pub async fn prepare_vector_plan(
        &self,
        repository: ModelScopeRepository,
        revision: &str,
        path: RepositoryPath,
    ) -> Result<ObservedPlanAssessment> {
        ensure!(
            path.as_str().to_ascii_lowercase().ends_with(".gguf"),
            ModelSourceError {
                code: LifecycleErrorCode::FormatForbidden,
                http_status: None
            }
        );
        let token = self.modelscope_credentials().await?;
        let provider = ModelScopeProvider::new()?;
        let inspected = provider
            .inspect_repository(repository, revision, token.as_deref())
            .await?;
        profile_for_artifact(
            &inspected
                .inventory
                .artifact(&path, FilePurpose::GgufWeight)?,
        )?;
        let component = provider
            .inspect_selected_component(
                &inspected.inventory,
                &[path],
                ComponentRole::Main,
                GgufReadLimits::for_llama_cpp(),
                token.as_deref(),
            )
            .await?;
        let root = self.root().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let store = InstallationStore::open(&root)?;
            let repository = LifecycleRepository::open(&root.join("models.db"))?;
            // Reuse the process backend while probing; a second LlamaBackend
            // initialization would conflict with an already active desktop runtime.
            let _runtime = crate::InferenceRuntime::get_or_init()?;
            let hardware = super::resources::probe_initialized_hardware_resources(
                store.root(),
                chrono::Utc::now().timestamp(),
            )?;
            repository.prepare_vector_plan(
                &component.observation,
                &hardware,
                chrono::Utc::now().timestamp(),
            )
        })
        .await
        .context("Vector plan preparation task failed")?
    }
}
