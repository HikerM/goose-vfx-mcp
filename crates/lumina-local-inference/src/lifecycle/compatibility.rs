use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{
    validate_gguf_shard_set, Artifact, ComponentCompatibility, ComponentRole, FilePurpose,
    GgufInspection, GgufScalar, GgufValue, RepositorySnapshot, RuntimeBuild, Sha256Digest,
};

mod profiles;
mod tokenizer;
pub use profiles::*;

pub const COMPONENT_PAIRING_RULES_VERSION: &str = "component-pairing-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceGgufObservation {
    pub artifact: Artifact,
    pub snapshot: RepositorySnapshot,
    pub header: GgufInspection,
}

impl SourceGgufObservation {
    pub fn digest(&self) -> Result<Sha256Digest> {
        self.validate()?;
        Sha256Digest::of_serializable(&(
            &self.artifact,
            self.snapshot.content_digest()?,
            &self.header,
        ))
    }

    pub fn validate(&self) -> Result<()> {
        self.artifact.validate()?;
        self.snapshot.validate()?;
        ensure!(
            self.artifact.purpose == FilePurpose::GgufWeight,
            "Component observation is not GGUF"
        );
        ensure!(
            self.artifact.snapshot_id == self.snapshot.content_digest()?.as_str(),
            "Component artifact snapshot identity mismatch"
        );
        ensure!(
            self.artifact.size_bytes == Some(self.header.file_bytes)
                && !self.header.tensors.is_empty(),
            "Component header does not match its artifact"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentObservation {
    pub role: ComponentRole,
    pub files: Vec<SourceGgufObservation>,
}

impl ComponentObservation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.files.is_empty() && self.files.len() <= 4096,
            "Invalid component file count"
        );
        let mut ids = BTreeSet::new();
        let mut source = None;
        let mut header_bytes = 0u64;
        for file in &self.files {
            file.validate()?;
            ensure!(ids.insert(&file.artifact.id), "Repeated component artifact");
            let digest = file.snapshot.content_digest()?;
            ensure!(
                source.as_ref().is_none_or(|source| source == &digest),
                "Component mixes source snapshots"
            );
            source = Some(digest);
            header_bytes = header_bytes
                .checked_add(file.header.header_bytes)
                .ok_or_else(|| anyhow::anyhow!("Component header budget overflow"))?;
        }
        ensure!(
            header_bytes <= 128 * 1024 * 1024,
            "Component headers exceed read budget"
        );
        validate_gguf_shard_set(
            &self
                .files
                .iter()
                .map(|file| file.header.clone())
                .collect::<Vec<_>>(),
        )
    }

    fn primary(&self) -> &GgufInspection {
        &self
            .files
            .iter()
            .find(|file| {
                file.header
                    .shard
                    .as_ref()
                    .is_none_or(|shard| shard.index == 0)
            })
            .expect("Component was validated")
            .header
    }

    pub fn digest(&self) -> Result<Sha256Digest> {
        self.validate()?;
        let mut files = self
            .files
            .iter()
            .map(|file| Ok((file.artifact.id.clone(), file.digest()?)))
            .collect::<Result<Vec<_>>>()?;
        files.sort();
        Sha256Digest::of_serializable(&(self.role, files))
    }

    fn known_hashes(&self) -> Option<BTreeSet<Sha256Digest>> {
        self.files
            .iter()
            .map(|file| file.artifact.source_sha256.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingSide {
    Main,
    Dependency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenizerIdentityPolicy {
    Exact,
    DflashMask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingConstraint {
    Text {
        side: PairingSide,
        key: String,
        value: String,
    },
    Boolean {
        side: PairingSide,
        key: String,
        value: bool,
    },
    UnsignedMinimum {
        side: PairingSide,
        key: String,
        minimum: u64,
    },
    UnsignedEqual {
        left_side: PairingSide,
        left_key: String,
        right_side: PairingSide,
        right_key: String,
    },
    TensorAxisEqual {
        side: PairingSide,
        tensor: String,
        axis: usize,
        field_side: PairingSide,
        field_key: String,
    },
    LayerIndices {
        side: PairingSide,
        key: String,
        target_side: PairingSide,
        target_block_count_key: String,
    },
    TokenizerIdentity {
        policy: TokenizerIdentityPolicy,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentPairingProfile {
    pub id: String,
    pub version: String,
    pub runtime: RuntimeBuild,
    pub dependency_role: ComponentRole,
    pub main_content: BTreeSet<Sha256Digest>,
    pub dependency_content: BTreeSet<Sha256Digest>,
    pub constraints: Vec<PairingConstraint>,
    pub evidence_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingIssue {
    RuntimeUnverified,
    ContentRelationshipUnverified,
    RoleMismatch,
    MissingMetadata { side: PairingSide, key: String },
    InvalidMetadata { side: PairingSide, key: String },
    ValueMismatch { key: String },
    MissingTensor { side: PairingSide, tensor: String },
    InvalidTensorAxis { side: PairingSide, tensor: String },
    IncompleteArray { side: PairingSide, key: String },
    TokenizerMismatch { key: String },
}

impl PairingIssue {
    fn incompatible(&self) -> bool {
        matches!(
            self,
            Self::RoleMismatch
                | Self::InvalidMetadata { .. }
                | Self::ValueMismatch { .. }
                | Self::InvalidTensorAxis { .. }
                | Self::TokenizerMismatch { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentPairingDecision {
    pub rules_version: String,
    pub profile_digest: Sha256Digest,
    pub runtime_digest: Sha256Digest,
    pub main_observation_digest: Sha256Digest,
    pub dependency_observation_digest: Sha256Digest,
    pub compatibility: ComponentCompatibility,
    pub issues: Vec<PairingIssue>,
}

impl ComponentPairingDecision {
    pub fn digest(&self) -> Result<Sha256Digest> {
        Sha256Digest::of_serializable(self)
    }
}

struct PairingContext<'a> {
    main: &'a GgufInspection,
    dependency: &'a GgufInspection,
}

impl PairingContext<'_> {
    fn header(&self, side: PairingSide) -> &GgufInspection {
        match side {
            PairingSide::Main => self.main,
            PairingSide::Dependency => self.dependency,
        }
    }

    fn field(&self, side: PairingSide, key: &str) -> std::result::Result<&GgufValue, PairingIssue> {
        self.header(side)
            .metadata
            .get(key)
            .ok_or_else(|| PairingIssue::MissingMetadata {
                side,
                key: key.into(),
            })
    }

    fn unsigned(&self, side: PairingSide, key: &str) -> std::result::Result<u64, PairingIssue> {
        self.field(side, key)?
            .as_u64()
            .ok_or_else(|| PairingIssue::InvalidMetadata {
                side,
                key: key.into(),
            })
    }

    fn check(&self, constraint: &PairingConstraint) -> std::result::Result<(), PairingIssue> {
        let mismatch = |key: &str| PairingIssue::ValueMismatch { key: key.into() };
        match constraint {
            PairingConstraint::Text { side, key, value } => {
                let actual = self.field(*side, key)?.as_str().ok_or_else(|| {
                    PairingIssue::InvalidMetadata {
                        side: *side,
                        key: key.clone(),
                    }
                })?;
                if actual != value {
                    return Err(mismatch(key));
                }
            }
            PairingConstraint::Boolean { side, key, value } => match self.field(*side, key)? {
                GgufValue::Scalar {
                    value: GgufScalar::Bool(actual),
                    ..
                } if actual == value => {}
                GgufValue::Scalar {
                    value: GgufScalar::Bool(_),
                    ..
                } => return Err(mismatch(key)),
                _ => {
                    return Err(PairingIssue::InvalidMetadata {
                        side: *side,
                        key: key.clone(),
                    })
                }
            },
            PairingConstraint::UnsignedMinimum { side, key, minimum } => {
                if self.unsigned(*side, key)? < *minimum {
                    return Err(mismatch(key));
                }
            }
            PairingConstraint::UnsignedEqual {
                left_side,
                left_key,
                right_side,
                right_key,
            } => {
                if self.unsigned(*left_side, left_key)? != self.unsigned(*right_side, right_key)? {
                    return Err(mismatch(&format!("{left_key}={right_key}")));
                }
            }
            PairingConstraint::TensorAxisEqual {
                side,
                tensor,
                axis,
                field_side,
                field_key,
            } => {
                let weight = self
                    .header(*side)
                    .tensors
                    .iter()
                    .find(|weight| &weight.name == tensor)
                    .ok_or_else(|| PairingIssue::MissingTensor {
                        side: *side,
                        tensor: tensor.clone(),
                    })?;
                let size = weight.dimensions.get(*axis).ok_or_else(|| {
                    PairingIssue::InvalidTensorAxis {
                        side: *side,
                        tensor: tensor.clone(),
                    }
                })?;
                if *size != self.unsigned(*field_side, field_key)? {
                    return Err(mismatch(tensor));
                }
            }
            PairingConstraint::LayerIndices {
                side,
                key,
                target_side,
                target_block_count_key,
            } => {
                let maximum = self.unsigned(*target_side, target_block_count_key)?;
                let GgufValue::Array {
                    count,
                    retained_values,
                    ..
                } = self.field(*side, key)?
                else {
                    return Err(PairingIssue::InvalidMetadata {
                        side: *side,
                        key: key.clone(),
                    });
                };
                if *count != retained_values.len() as u64 {
                    return Err(PairingIssue::IncompleteArray {
                        side: *side,
                        key: key.clone(),
                    });
                }
                let mut seen = BTreeSet::new();
                if *count == 0
                    || retained_values.iter().any(|value| {
                        let index = match value {
                            GgufScalar::Unsigned(value) => Some(*value),
                            GgufScalar::Signed(value) => u64::try_from(*value).ok(),
                            _ => None,
                        };
                        index.is_none_or(|index| index >= maximum || !seen.insert(index))
                    })
                {
                    return Err(PairingIssue::InvalidMetadata {
                        side: *side,
                        key: key.clone(),
                    });
                }
            }
            PairingConstraint::TokenizerIdentity { policy } => {
                tokenizer::compare(self.main, self.dependency, *policy)?
            }
        }
        Ok(())
    }
}

pub fn assess_component_pairing(
    main: &ComponentObservation,
    dependency: &ComponentObservation,
    runtime: &RuntimeBuild,
    profile: &ComponentPairingProfile,
) -> Result<ComponentPairingDecision> {
    main.validate()?;
    dependency.validate()?;
    ensure!(
        !profile.id.is_empty()
            && !profile.version.is_empty()
            && !profile.evidence_ids.is_empty()
            && !profile.constraints.is_empty()
            && profile.constraints.len() <= 64
            && !profile.main_content.is_empty()
            && !profile.dependency_content.is_empty(),
        "Pairing profile lacks registered identity, constraints or provenance"
    );
    let mut issues = Vec::new();
    if main.role != ComponentRole::Main || dependency.role != profile.dependency_role {
        issues.push(PairingIssue::RoleMismatch);
    }
    if runtime != &profile.runtime {
        issues.push(PairingIssue::RuntimeUnverified);
    }
    if main.known_hashes().as_ref() != Some(&profile.main_content)
        || dependency.known_hashes().as_ref() != Some(&profile.dependency_content)
    {
        issues.push(PairingIssue::ContentRelationshipUnverified);
    }
    let context = PairingContext {
        main: main.primary(),
        dependency: dependency.primary(),
    };
    for constraint in &profile.constraints {
        if let Err(issue) = context.check(constraint) {
            issues.push(issue);
        }
    }
    let compatibility = if issues.iter().any(PairingIssue::incompatible) {
        ComponentCompatibility::Incompatible
    } else if issues.is_empty() {
        ComponentCompatibility::Verified
    } else {
        ComponentCompatibility::Unverified
    };
    Ok(ComponentPairingDecision {
        rules_version: COMPONENT_PAIRING_RULES_VERSION.into(),
        profile_digest: Sha256Digest::of_serializable(profile)?,
        runtime_digest: Sha256Digest::of_serializable(runtime)?,
        main_observation_digest: main.digest()?,
        dependency_observation_digest: dependency.digest()?,
        compatibility,
        issues,
    })
}
