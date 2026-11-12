use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::lifecycle::{
    baseline_pairing_runtime, ComponentRole, LaunchConfig, ModelCapability, ModelManifest,
    RuntimeExecutionMode, RuntimeParameters,
};

const SCHEMA: &str = "llama-vector-helper-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorRuntimeProfile {
    Qwen3Embedding06bQ8,
    BgeRerankerV2M3Q4Km,
}

impl VectorRuntimeProfile {
    pub fn capability(self) -> ModelCapability {
        match self {
            Self::Qwen3Embedding06bQ8 => ModelCapability::Embedding,
            Self::BgeRerankerV2M3Q4Km => ModelCapability::Reranking,
        }
    }

    pub fn model_sha256(self) -> &'static str {
        match self {
            Self::Qwen3Embedding06bQ8 => {
                "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439"
            }
            Self::BgeRerankerV2M3Q4Km => {
                "e186a244ed455b4ab66ec64339ce7427a6ae13f5c0b5e544de96e50f0f8b3673"
            }
        }
    }

    pub fn model_bytes(self) -> u64 {
        match self {
            Self::Qwen3Embedding06bQ8 => 639_150_592,
            Self::BgeRerankerV2M3Q4Km => 438_376_864,
        }
    }

    pub fn launch_parameters(self) -> RuntimeParameters {
        RuntimeParameters::Specialized {
            schema_id: SCHEMA.into(),
            settings: self.settings(),
        }
    }

    fn settings(self) -> BTreeMap<String, String> {
        let (profile, pooling, normalization, dimensions, max_tokens) = match self {
            Self::Qwen3Embedding06bQ8 => ("qwen3-embedding-06b-q8-v1", "last", "2", "1024", "511"),
            Self::BgeRerankerV2M3Q4Km => (
                "bge-reranker-v2-m3-q4km-v1",
                "rank",
                "not_applicable",
                "1",
                "512",
            ),
        };
        [
            ("profile", profile),
            ("pooling", pooling),
            ("embedding_normalization", normalization),
            ("dimensions", dimensions),
            ("context_length", "512"),
            ("batch_size", "512"),
            ("microbatch_size", "512"),
            ("parallel_sequences", "1"),
            ("cpu_threads", "4"),
            ("batch_cpu_threads", "4"),
            ("gpu_layers", "0"),
            ("flash_attention", "on"),
            ("cache_type_k", "f16"),
            ("cache_type_v", "f16"),
            ("prompt_cache_mib", "0"),
            ("context_checkpoints", "0"),
            ("cache_prompt", "false"),
            ("cache_idle_slots", "false"),
            ("cache_reuse", "0"),
            ("auto_fit", "false"),
            ("speculative_type", "none"),
            ("warmup", "false"),
            ("max_input_bytes", "8192"),
            ("max_batch_bytes", "65536"),
            ("max_batch_items", "16"),
            ("max_input_tokens", max_tokens),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
    }

    pub(crate) fn from_binding(launch: &LaunchConfig, manifest: &ModelManifest) -> Result<Self> {
        launch.validate_binding(manifest)?;
        let profile = Self::from_parameters(&launch.parameters)?;
        ensure!(
            launch.capabilities == BTreeSet::from([profile.capability()]),
            "Vector launch must select exactly the profile capability"
        );
        ensure!(
            launch.runtime == baseline_pairing_runtime()?,
            "Vector launch does not use the measured native helper build"
        );
        ensure!(
            matches!(launch.mode, RuntimeExecutionMode::Helper { port } if port > 0),
            "Vector adapter requires a helper with a nonzero loopback port"
        );
        ensure!(
            manifest.installed_files.len() == 1
                && launch.component_files.len() == 1
                && launch
                    .component_files
                    .get(&ComponentRole::Main)
                    .is_some_and(|files| files.len() == 1),
            "Vector profile requires exactly one main GGUF file"
        );
        let installed = manifest.installed_files.values().next().unwrap();
        ensure!(
            installed.computed_sha256.as_str() == profile.model_sha256()
                && installed.size_bytes == profile.model_bytes(),
            "Vector model content differs from the measured profile"
        );
        Ok(profile)
    }

    pub(crate) fn from_parameters(parameters: &RuntimeParameters) -> Result<Self> {
        let RuntimeParameters::Specialized {
            schema_id,
            settings,
        } = parameters
        else {
            bail!("Vector helper requires frozen specialized runtime parameters");
        };
        ensure!(
            schema_id == SCHEMA,
            "Unsupported vector helper parameter schema"
        );
        let profile = match settings.get("profile").map(String::as_str) {
            Some("qwen3-embedding-06b-q8-v1") => Self::Qwen3Embedding06bQ8,
            Some("bge-reranker-v2-m3-q4km-v1") => Self::BgeRerankerV2M3Q4Km,
            _ => bail!("Unknown measured vector runtime profile"),
        };
        ensure!(
            *settings == profile.settings(),
            "Vector helper parameters differ from the frozen profile"
        );
        Ok(profile)
    }

    pub(crate) fn arguments(self) -> Vec<String> {
        let mut args: Vec<String> = [
            "--threads",
            "4",
            "--threads-batch",
            "4",
            "--ctx-size",
            "512",
            "--batch-size",
            "512",
            "--ubatch-size",
            "512",
            "--parallel",
            "1",
            "--gpu-layers",
            "0",
            "--no-webui",
            "--no-warmup",
            "--cache-ram",
            "0",
            "--ctx-checkpoints",
            "0",
            "--no-cache-prompt",
            "--no-cache-idle-slots",
            "--cache-reuse",
            "0",
            "--fit",
            "off",
            "--spec-type",
            "none",
            "--cache-type-k",
            "f16",
            "--cache-type-v",
            "f16",
            "--flash-attn",
            "on",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let task: &[&str] = match self {
            Self::Qwen3Embedding06bQ8 => {
                &["--embedding", "--pooling", "last", "--embd-normalize", "2"]
            }
            Self::BgeRerankerV2M3Q4Km => &["--reranking"],
        };
        args.extend(task.iter().map(|value| (*value).to_string()));
        args
    }
}
