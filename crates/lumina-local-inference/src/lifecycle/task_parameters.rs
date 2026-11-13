//! Parameter compilation for pinned native generation profiles.
//!
//! This is the parameter boundary for the future managed task owner, not an execution
//! owner or a readiness claim. Paths are symbolic, and every compiled task currently
//! requires resource admission. A native smoke observation cannot grant admission.

mod compile;
mod profiles;
mod registry;

pub use registry::*;

use super::{ComponentRole, ModelCapability, ModelManifest, RuntimeBuild, Sha256Digest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const GENERATION_PARAMETER_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationProfile {
    Sd15CpuTextToImage,
    H3CudaTextToVideoAudio,
    H3CudaFirstFrameToVideoAudio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationAssetRole {
    Main,
    TextEncoder,
    VideoVae,
    AudioVae,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationAssetBinding {
    pub artifact_id: String,
    pub sha256: Sha256Digest,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationModelBinding {
    pub installation_id: String,
    pub version_id: String,
    pub manifest_digest: Sha256Digest,
    pub runtime: RuntimeBuild,
    pub target_capabilities: BTreeSet<ModelCapability>,
    pub assets: BTreeMap<GenerationAssetRole, GenerationAssetBinding>,
}

impl GenerationModelBinding {
    /// The caller must load this manifest through the registered installation store.
    /// File identity, process lifetime and resource reservation belong to the owner.
    pub fn from_manifest(manifest: &ModelManifest) -> anyhow::Result<Self> {
        manifest.validate()?;
        let mut assets = BTreeMap::new();
        for component in manifest.plan.dependencies.components.values() {
            let role = match component.role {
                ComponentRole::Main => GenerationAssetRole::Main,
                ComponentRole::TextEncoder => GenerationAssetRole::TextEncoder,
                ComponentRole::Vae => GenerationAssetRole::VideoVae,
                ComponentRole::AudioDecoder => GenerationAssetRole::AudioVae,
                _ => anyhow::bail!("Component role is not supported by generation profiles"),
            };
            anyhow::ensure!(
                component.artifact_ids.len() == 1,
                "Expected one artifact per component"
            );
            let id = component
                .artifact_ids
                .iter()
                .next()
                .expect("checked component");
            let file = &manifest.installed_files[id];
            anyhow::ensure!(
                assets
                    .insert(
                        role,
                        GenerationAssetBinding {
                            artifact_id: id.clone(),
                            sha256: file.computed_sha256.clone(),
                            size_bytes: file.size_bytes,
                        }
                    )
                    .is_none(),
                "Duplicate generation component role"
            );
        }
        anyhow::ensure!(
            assets.len() == manifest.installed_files.len(),
            "Unbound generation artifacts"
        );
        Ok(Self {
            installation_id: manifest.installation_id.clone(),
            version_id: manifest.version_id.clone(),
            manifest_digest: manifest.digest()?,
            runtime: manifest.plan.runtime.clone(),
            target_capabilities: manifest.plan.target_capabilities.clone(),
            assets,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationMediaInput {
    pub session_id: String,
    pub result_id: String,
    pub sha256: Sha256Digest,
    pub mime_type: String,
    pub size_bytes: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GenerationSeed {
    Value { value: u32 },
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationSampler {
    Euler,
    EulerA,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationScheduler {
    Discrete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationAudioMode {
    JointAudioVideo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationAspectRatio {
    pub width: u32,
    pub height: u32,
}

/// The same typed optional fields are used for creation and explicit patches.
/// Missing patch fields inherit; null cannot remove a required input or select a model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Sd15ImageParameters {
    pub prompt: Option<String>,
    pub negative_prompt: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub aspect_ratio: Option<GenerationAspectRatio>,
    pub seed: Option<GenerationSeed>,
    pub steps: Option<u32>,
    pub cfg_scale: Option<f32>,
    pub sampler: Option<GenerationSampler>,
    pub scheduler: Option<GenerationScheduler>,
    pub batch_count: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct H3VideoParameters {
    pub prompt: Option<String>,
    pub negative_prompt: Option<String>,
    pub first_frame: Option<GenerationMediaInput>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub aspect_ratio: Option<GenerationAspectRatio>,
    pub seed: Option<GenerationSeed>,
    pub steps: Option<u32>,
    pub cfg_scale: Option<f32>,
    pub sampler: Option<GenerationSampler>,
    pub scheduler: Option<GenerationScheduler>,
    pub frames: Option<u32>,
    pub fps: Option<u32>,
    /// Must represent an integral, model-aligned frame count. No rounding or clamping.
    pub duration_seconds: Option<f64>,
    pub audio_mode: Option<GenerationAudioMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "task",
    content = "parameters",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum GenerationParameters {
    Image(Sd15ImageParameters),
    Video(H3VideoParameters),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationTaskRequest {
    pub session_id: String,
    pub job_id: String,
    pub parameters: GenerationParameters,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EffectiveImageParameters {
    pub prompt: String,
    pub negative_prompt: String,
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: GenerationAspectRatio,
    pub seed: u32,
    pub steps: u32,
    pub cfg_scale: f32,
    pub sampler: GenerationSampler,
    pub scheduler: GenerationScheduler,
    pub batch_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EffectiveVideoParameters {
    pub prompt: String,
    pub negative_prompt: String,
    pub first_frame: Option<GenerationMediaInput>,
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: GenerationAspectRatio,
    pub seed: u32,
    pub steps: u32,
    pub cfg_scale: f32,
    pub sampler: GenerationSampler,
    pub scheduler: GenerationScheduler,
    pub frames: u32,
    pub fps: u32,
    /// Exact rational duration, frames / fps; avoids a rounded duration becoming an input.
    pub duration: GenerationDuration,
    pub audio_mode: GenerationAudioMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationDuration {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "task",
    content = "parameters",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum EffectiveGenerationParameters {
    Image(EffectiveImageParameters),
    Video(EffectiveVideoParameters),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GenerationParameterOrigin {
    Request,
    RecordedDefault { evidence: String },
    Derived,
    GeneratedSeed,
    Parent { result_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationParameterChange {
    pub before: Value,
    pub after: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationParent {
    pub result_id: String,
    pub snapshot_digest: Sha256Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationExecutionStatus {
    NeedsResourceAdmission,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationObservation {
    pub evidence: String,
    pub matches_observed_request: bool,
    pub quality_acceptance_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FrozenGenerationEngine {
    pub cpu_threads: u32,
    pub backend: String,
    pub parameters_backend: String,
    pub rng: String,
    pub sampler_rng: String,
    pub flash_attention: String,
    pub cache_mode: String,
    pub flow_shift: Option<u32>,
    pub max_vram_gib: Option<u32>,
    pub disable_prefetch: bool,
    pub output_format: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationParameterSnapshot {
    pub schema_version: u32,
    pub descriptor_digest: Sha256Digest,
    pub profile: GenerationProfile,
    pub binding: GenerationModelBinding,
    pub session_id: String,
    pub job_id: String,
    pub parameters: EffectiveGenerationParameters,
    pub engine: FrozenGenerationEngine,
    pub origins: BTreeMap<String, GenerationParameterOrigin>,
    pub parent: Option<GenerationParent>,
    pub changes: BTreeMap<String, GenerationParameterChange>,
    pub execution_status: GenerationExecutionStatus,
    pub observation: GenerationObservation,
}

impl GenerationParameterSnapshot {
    pub fn digest(&self) -> anyhow::Result<Sha256Digest> {
        Sha256Digest::of_serializable(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationOutputReference {
    pub session_id: String,
    pub result_id: String,
    pub sha256: Sha256Digest,
}

/// Loaded from the owning session's result store, never accepted as proof of execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationResultRecord {
    pub snapshot: GenerationParameterSnapshot,
    pub snapshot_digest: Sha256Digest,
    pub outputs: Vec<GenerationOutputReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationParameterIssue {
    pub field: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationParameterError {
    pub issues: Vec<GenerationParameterIssue>,
}

impl std::fmt::Display for GenerationParameterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, issue) in self.issues.iter().enumerate() {
            if index > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{}: {} ({})", issue.field, issue.message, issue.code)?;
        }
        Ok(())
    }
}
impl std::error::Error for GenerationParameterError {}

pub type GenerationParameterResult<T> = Result<T, GenerationParameterError>;

pub(super) fn issue(field: &str, code: &str, message: &str) -> GenerationParameterError {
    GenerationParameterError {
        issues: vec![GenerationParameterIssue {
            field: field.into(),
            code: code.into(),
            message: message.into(),
        }],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationParameterSupport {
    NativeSupported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationFieldDescriptor {
    pub value_type: String,
    pub support: GenerationParameterSupport,
    pub default: Option<Value>,
    pub default_source: Option<String>,
    pub unit: Option<String>,
    pub minimum: Option<f64>,
    /// A representation or protocol bound, not a measured safe resource limit.
    pub maximum: Option<f64>,
    pub step: Option<f64>,
    pub enum_values: Vec<Value>,
    pub constraints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationParameterDescriptor {
    pub schema_version: u32,
    pub profile: GenerationProfile,
    pub binding: GenerationModelBinding,
    pub fields: BTreeMap<String, GenerationFieldDescriptor>,
    pub execution_status: GenerationExecutionStatus,
    pub evidence: String,
    pub limitations: Vec<String>,
}

/// Each element is one argv value. Component/input/output paths are owner-resolved
/// slots, not strings supplied by a conversation and never shell fragments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeGenerationArgument {
    Literal(String),
    Component(GenerationAssetRole),
    InputResult(String),
    Output,
}

#[derive(Debug, Clone)]
pub struct PreparedGenerationTask {
    snapshot: GenerationParameterSnapshot,
    arguments: Vec<NativeGenerationArgument>,
}

impl PreparedGenerationTask {
    pub fn snapshot(&self) -> &GenerationParameterSnapshot {
        &self.snapshot
    }
    pub fn arguments(&self) -> &[NativeGenerationArgument] {
        &self.arguments
    }

    /// The managed owner must call this before resolving paths or acquiring a process.
    /// No generation resource envelope has been measured/registered yet, including smoke defaults.
    pub fn ensure_execution_admitted(&self) -> GenerationParameterResult<()> {
        Err(issue(
            "resourceAdmission",
            "required",
            "Generation resource admission is not implemented for this profile",
        ))
    }
}

pub struct GenerationParameterService {
    profile: GenerationProfile,
    binding: GenerationModelBinding,
    descriptor: GenerationParameterDescriptor,
}
