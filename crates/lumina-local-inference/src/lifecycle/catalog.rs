use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use super::{
    ArchitectureKnowledge, Artifact, Component, ComponentRole, FilePurpose, ModelDefinition,
    ModelVariant, RepositorySnapshot, Sha256Digest,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogSnapshot {
    pub model: ModelDefinition,
    pub snapshots: BTreeMap<String, RepositorySnapshot>,
    pub artifacts: BTreeMap<String, Artifact>,
    pub variants: BTreeMap<String, ModelVariant>,
    pub components: BTreeMap<String, Component>,
}

impl ModelCatalogSnapshot {
    pub fn content_digest(&self) -> Result<Sha256Digest> {
        let sources = self
            .snapshots
            .iter()
            .map(|(id, snapshot)| Ok((id, snapshot.content_digest()?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        Sha256Digest::of_serializable(&(
            &self.model,
            sources,
            &self.artifacts,
            &self.variants,
            &self.components,
        ))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.model.id.is_empty() && !self.model.display_name.is_empty(),
            "Missing model identity"
        );
        ensure!(!self.snapshots.is_empty(), "Model has no source snapshots");
        if let ArchitectureKnowledge::Observed {
            architecture,
            evidence_ids,
        } = &self.model.architecture
        {
            ensure!(
                !architecture.trim().is_empty() && !evidence_ids.is_empty(),
                "Observed model architecture requires evidence"
            );
        }
        ensure!(
            self.model.snapshot_ids.iter().eq(self.snapshots.keys()),
            "Model source references do not match its snapshots"
        );
        for (id, snapshot) in &self.snapshots {
            ensure!(!id.is_empty(), "Missing source snapshot ID");
            snapshot.validate()?;
        }
        let mut source_paths = BTreeSet::new();
        for (id, artifact) in &self.artifacts {
            ensure!(id == &artifact.id, "Artifact key does not match its ID");
            artifact.validate()?;
            ensure!(
                self.snapshots.contains_key(&artifact.snapshot_id),
                "Artifact source is not in the model catalog"
            );
            ensure!(
                source_paths.insert((&artifact.snapshot_id, artifact.path.as_str())),
                "Duplicate catalog file identity"
            );
        }
        for (id, variant) in &self.variants {
            ensure!(
                !id.is_empty() && id == &variant.id && variant.model_id == self.model.id,
                "Variant model identity mismatch"
            );
            ensure!(!variant.artifact_ids.is_empty(), "Variant has no files");
            for artifact in &variant.artifact_ids {
                ensure!(
                    self.artifacts
                        .get(artifact)
                        .is_some_and(|file| file.purpose == FilePurpose::GgufWeight),
                    "Variant references missing or non-GGUF weights"
                );
            }
        }
        for (id, component) in &self.components {
            ensure!(
                !id.is_empty() && id == &component.id,
                "Component identity mismatch"
            );
            ensure!(
                !component.artifact_ids.is_empty(),
                "Catalog component has no files"
            );
            for id in &component.artifact_ids {
                let artifact = self.artifacts.get(id).ok_or_else(|| {
                    anyhow::anyhow!("Catalog component references a missing file")
                })?;
                let role_matches = match component.role {
                    ComponentRole::Metadata => artifact.purpose != FilePurpose::GgufWeight,
                    _ => artifact.purpose == FilePurpose::GgufWeight,
                };
                ensure!(
                    role_matches,
                    "Component role does not match the artifact purpose"
                );
            }
        }
        Ok(())
    }
}
