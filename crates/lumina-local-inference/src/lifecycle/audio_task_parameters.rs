//! Model-specific offline audio parameter compilers. These produce bound plans, not
//! execution permission; the managed owner must verify inputs and admit resources.

mod install;
mod models;
mod profiles;
mod reuse;
mod services;

pub use install::{AUDIO_INSTALL_RULES_VERSION, LEGACY_AUDIO_INSTALL_RULES_VERSION};
pub use reuse::*;
pub use services::*;

use super::{
    ComponentRole, GenerationParameterError, GenerationParameterIssue, GenerationParameterResult,
    ModelCapability, ModelManifest, ModelTaskParameterAdapter, RuntimeBuild, Sha256Digest,
    TaskParameterIdentity,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const AUDIO_TASK_PARAMETER_SCHEMA: u32 = 1;
pub const AUDIO_TASK_PARAMETER_EVIDENCE: &str = "2026-11-01-audio-asr-tts-feasibility.json";

pub use models::AudioTaskProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioTaskProtocol {
    PocketTtsOffline,
    Qwen3AsrOffline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioTaskAssetBinding {
    pub artifact_id: String,
    pub sha256: Sha256Digest,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioTaskModelBinding {
    pub installation_id: String,
    pub version_id: String,
    pub manifest_digest: Sha256Digest,
    pub runtime: RuntimeBuild,
    pub target_capabilities: BTreeSet<ModelCapability>,
    pub model: AudioTaskAssetBinding,
    pub model_spec: AudioTaskAssetBinding,
}

impl AudioTaskModelBinding {
    /// The owner supplies a manifest loaded from its registered installation store.
    /// The spec is an explicit installed metadata artifact, never the GGUF's embedded spec.
    pub fn from_manifest(manifest: &ModelManifest) -> anyhow::Result<Self> {
        manifest.validate()?;
        AudioTaskProfile::for_installation_assets(&manifest.plan)?;
        let mut assets = BTreeMap::new();
        for component in manifest.plan.dependencies.components.values() {
            anyhow::ensure!(
                matches!(
                    component.role,
                    ComponentRole::Main | ComponentRole::Metadata
                ) && component.artifact_ids.len() == 1,
                "Audio task profile requires one main GGUF and one explicit model spec"
            );
            let id = component.artifact_ids.iter().next().expect("one artifact");
            let file = &manifest.installed_files[id];
            anyhow::ensure!(
                assets
                    .insert(
                        component.role,
                        AudioTaskAssetBinding {
                            artifact_id: id.clone(),
                            sha256: file.computed_sha256.clone(),
                            size_bytes: file.size_bytes,
                        }
                    )
                    .is_none(),
                "Duplicate audio component role"
            );
        }
        anyhow::ensure!(
            assets.len() == 2 && manifest.installed_files.len() == 2,
            "Unbound audio components"
        );
        Ok(Self {
            installation_id: manifest.installation_id.clone(),
            version_id: manifest.version_id.clone(),
            manifest_digest: manifest.digest()?,
            runtime: manifest.plan.runtime.clone(),
            target_capabilities: manifest.plan.target_capabilities.clone(),
            model: assets
                .remove(&ComponentRole::Main)
                .expect("validated roles"),
            model_spec: assets
                .remove(&ComponentRole::Metadata)
                .expect("validated roles"),
        })
    }
}

/// Metadata must come from the authenticated session's verified media store.
/// Compilation validates the reference shape, not its authorization or actual samples.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioTaskMediaReference {
    pub session_id: String,
    pub result_id: String,
    pub sha256: Sha256Digest,
    pub mime_type: String,
    pub size_bytes: u64,
    pub sample_rate: u32,
    pub channels: u32,
    pub sample_frames: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeAudioArgument {
    Literal(String),
    Model,
    ModelSpec,
    Input(AudioTaskMediaReference),
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioTaskExecutionStatus {
    NeedsResourceAdmission,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioTaskSnapshotMetadata {
    pub schema_version: u32,
    pub identity: TaskParameterIdentity,
    pub binding: AudioTaskModelBinding,
    pub descriptor_digest: Sha256Digest,
    pub session_id: String,
    pub job_id: String,
    pub backend: String,
    pub cpu_threads: u32,
    pub execution_status: AudioTaskExecutionStatus,
    /// PocketTTS captures voice, seed and several options in prepare(), not run().
    /// Neither adapter permits reuse of a previously prepared native session.
    pub requires_new_native_preparation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation: Option<AudioTaskDerivation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioParameterField {
    pub value_type: String,
    pub required: bool,
    pub default: Option<Value>,
    pub native_option: Option<String>,
    pub constraints: Vec<String>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PocketTtsDescriptor {
    pub identity: TaskParameterIdentity,
    pub binding: AudioTaskModelBinding,
    pub fields: BTreeMap<String, AudioParameterField>,
    pub unsupported: BTreeMap<String, String>,
    pub model_language: String,
    pub output_sample_rate: u32,
    pub evidence: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Qwen3AsrDescriptor {
    pub identity: TaskParameterIdentity,
    pub binding: AudioTaskModelBinding,
    pub fields: BTreeMap<String, AudioParameterField>,
    pub unsupported: BTreeMap<String, String>,
    pub recognition_languages: Vec<Qwen3AsrLanguage>,
    pub evidence: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PocketTtsRequest {
    pub session_id: String,
    pub job_id: String,
    pub text: String,
    pub voice_reference: AudioTaskMediaReference,
    pub seed: Option<u32>,
    pub temperature: Option<f32>,
    pub max_frames: Option<u32>,
    pub text_chunk_tokens: Option<u32>,
    pub frames_after_eos: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EffectivePocketTtsParameters {
    pub text: String,
    pub voice_reference: AudioTaskMediaReference,
    pub seed: u32,
    pub temperature: f32,
    /// Zero explicitly selects the native text-derived frame cap.
    pub max_frames: u32,
    pub text_chunk_tokens: u32,
    /// Minus one explicitly selects the native model/text-derived EOS tail.
    pub frames_after_eos: i32,
    pub noise_clamp: f32,
    pub eos_threshold: f32,
    pub truncate_clone_audio: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PocketTtsSnapshot {
    pub metadata: AudioTaskSnapshotMetadata,
    pub parameters: EffectivePocketTtsParameters,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Qwen3AsrChunking {
    Fixed { seconds: f32 },
    WholeFile {},
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Qwen3AsrRequest {
    pub session_id: String,
    pub job_id: String,
    pub audio: AudioTaskMediaReference,
    pub context: Option<String>,
    pub language: Option<Qwen3AsrLanguage>,
    pub max_tokens: Option<u32>,
    pub chunking: Option<Qwen3AsrChunking>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EffectiveQwen3AsrParameters {
    pub audio: AudioTaskMediaReference,
    pub context: String,
    pub language: Qwen3AsrLanguage,
    pub max_tokens: u32,
    pub chunking: Qwen3AsrChunking,
    pub return_timestamps: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Qwen3AsrSnapshot {
    pub metadata: AudioTaskSnapshotMetadata,
    pub parameters: EffectiveQwen3AsrParameters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Qwen3AsrLanguage {
    Auto,
    Chinese,
    English,
    Cantonese,
    Arabic,
    German,
    French,
    Spanish,
    Portuguese,
    Indonesian,
    Italian,
    Korean,
    Russian,
    Thai,
    Vietnamese,
    Japanese,
    Turkish,
    Hindi,
    Malay,
    Dutch,
    Swedish,
    Danish,
    Finnish,
    Polish,
    Czech,
    Filipino,
    Persian,
    Greek,
    Romanian,
    Hungarian,
    Macedonian,
}

fn invalid(field: &str, code: &str, message: &str) -> GenerationParameterError {
    GenerationParameterError {
        issues: vec![GenerationParameterIssue {
            field: field.into(),
            code: code.into(),
            message: message.into(),
        }],
    }
}

fn digest(value: &impl Serialize) -> GenerationParameterResult<Sha256Digest> {
    Sha256Digest::of_serializable(value).map_err(|_| {
        invalid(
            "binding",
            "serialization_failed",
            "Cannot digest audio task data",
        )
    })
}

fn admission_required() -> GenerationParameterResult<()> {
    Err(invalid("resourceAdmission", "required", "Audio task resource admission is not implemented; native feasibility is not execution permission"))
}
