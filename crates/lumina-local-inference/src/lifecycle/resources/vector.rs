use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{AdapterResourceEnvelope, ResourceCacheType, ResourceWorkload};
use crate::lifecycle::vector_runtime::VectorRuntimeProfile;
use crate::lifecycle::{
    baseline_pairing_runtime, ComponentObservation, ComponentRole, HardwareCompatibility,
    PlanAdmission, RuntimeInstallProfile, Sha256Digest, RUNTIME_INSTALL_PROFILE_VERSION,
};

pub const VECTOR_RESOURCE_RULES_VERSION: &str = "local-resource-budget-vector-v1";
pub const VECTOR_RESOURCE_EVIDENCE: &str =
    "m4:m4-vector-resource-protocol-20261112-r1:bounded-cpu-v1";
const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorResourceWorkload {
    pub profile: VectorRuntimeProfile,
    pub microbatch_tokens: u32,
    pub cpu_threads: u32,
    pub batch_cpu_threads: u32,
    pub gpu_layers: u32,
    pub flash_attention: bool,
    pub cache_prompt: bool,
    pub cache_idle_slots: bool,
    pub cache_reuse_tokens: u32,
    pub auto_fit: bool,
    pub speculative_decoding: bool,
    pub warmup: bool,
    pub max_batch_items: u32,
    pub max_input_bytes: u32,
    pub max_batch_bytes: u32,
    pub max_input_tokens: u32,
}

impl VectorResourceWorkload {
    pub fn measured_cpu(profile: VectorRuntimeProfile) -> Self {
        Self {
            profile,
            microbatch_tokens: 512,
            cpu_threads: 4,
            batch_cpu_threads: 4,
            gpu_layers: 0,
            flash_attention: true,
            cache_prompt: false,
            cache_idle_slots: false,
            cache_reuse_tokens: 0,
            auto_fit: false,
            speculative_decoding: false,
            warmup: false,
            max_batch_items: 16,
            max_input_bytes: 8192,
            max_batch_bytes: 65536,
            max_input_tokens: match profile {
                VectorRuntimeProfile::Qwen3Embedding06bQ8 => 511,
                VectorRuntimeProfile::BgeRerankerV2M3Q4Km => 512,
            },
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            *self == Self::measured_cpu(self.profile),
            "Vector workload differs from the measured CPU parameter and input envelope"
        );
        Ok(())
    }
}

impl VectorRuntimeProfile {
    pub fn resource_workload(self) -> ResourceWorkload {
        ResourceWorkload {
            context_tokens: 512,
            batch_tokens: 512,
            parallel_sequences: 1,
            recurrent_rollback_slots: 0,
            kv_key_type: ResourceCacheType::F16,
            kv_value_type: ResourceCacheType::F16,
            prompt_cache_bytes: 0,
            context_checkpoint_slots: 0,
            vision: None,
            vector: Some(Box::new(VectorResourceWorkload::measured_cpu(self))),
        }
    }

    fn resource_architecture(self) -> &'static str {
        match self {
            Self::Qwen3Embedding06bQ8 => "qwen3",
            Self::BgeRerankerV2M3Q4Km => "bert",
        }
    }

    fn auxiliary_buffer_bytes(self) -> u64 {
        match self {
            Self::Qwen3Embedding06bQ8 => 256 * MIB,
            Self::BgeRerankerV2M3Q4Km => 512 * MIB,
        }
    }

    fn kv_cache_bytes(self) -> u64 {
        match self {
            Self::Qwen3Embedding06bQ8 => 56 * MIB,
            Self::BgeRerankerV2M3Q4Km => 0,
        }
    }
}

pub fn validate_measured_vector_workload(
    workload: &ResourceWorkload,
) -> Result<VectorRuntimeProfile> {
    let vector = workload
        .vector
        .as_ref()
        .context("Missing measured vector workload")?;
    vector.validate()?;
    ensure!(
        *workload == vector.profile.resource_workload(),
        "No measured vector memory profile matches this resource workload"
    );
    Ok(vector.profile)
}

pub fn baseline_vector_install_profile(
    profile: VectorRuntimeProfile,
) -> Result<RuntimeInstallProfile> {
    let id = match profile {
        VectorRuntimeProfile::Qwen3Embedding06bQ8 => "measured-qwen3-embedding-06b-q8-cpu-v1",
        VectorRuntimeProfile::BgeRerankerV2M3Q4Km => "measured-bge-reranker-v2-m3-q4km-cpu-v1",
    };
    Ok(RuntimeInstallProfile {
        id: id.into(),
        rules_version: RUNTIME_INSTALL_PROFILE_VERSION.into(),
        runtime: baseline_pairing_runtime()?,
        architectures: BTreeSet::from([profile.resource_architecture().into()]),
        capabilities: BTreeSet::from([profile.capability()]),
        required_roles: BTreeSet::from([ComponentRole::Main]),
        evidence_ids: BTreeSet::from([VECTOR_RESOURCE_EVIDENCE.into()]),
    })
}

fn validate_envelope_parameters(
    envelope: &AdapterResourceEnvelope,
) -> Result<VectorRuntimeProfile> {
    let profile = validate_measured_vector_workload(&envelope.workload)?;
    ensure!(
        envelope.runtime == baseline_pairing_runtime()?
            && envelope.evidence_id == VECTOR_RESOURCE_EVIDENCE
            && envelope.compute_buffer_bytes >= 64 * MIB
            && envelope.auxiliary_buffer_bytes >= profile.auxiliary_buffer_bytes(),
        "No measured vector memory profile matches this runtime and buffer allowance"
    );
    Ok(profile)
}

/// Checks the frozen admission without reinterpreting model files or launching
/// a helper. The launch binder must additionally verify its manifest content,
/// capability and complete specialized parameters against the returned profile.
pub fn validate_vector_admission(admission: &PlanAdmission) -> Result<VectorRuntimeProfile> {
    let envelope = &admission.envelope;
    let profile = validate_envelope_parameters(envelope)?;
    let budget = &admission.budget;
    let required = super::sum([
        profile.model_bytes(),
        profile.kv_cache_bytes(),
        envelope.compute_buffer_bytes,
        envelope.auxiliary_buffer_bytes,
    ])?;
    let peak_disk = super::sum([super::product(&[profile.model_bytes(), 3])?, 16 * MIB])?;
    let available = admission.hardware.available_host_bytes();
    ensure!(
        admission.runtime_profile == baseline_vector_install_profile(profile)?
            && envelope.component_digests.len() == 1
            && budget.rules_version == VECTOR_RESOURCE_RULES_VERSION
            && budget.binding_digest
                == Sha256Digest::of_serializable(&(
                    VECTOR_RESOURCE_RULES_VERSION,
                    envelope,
                    &admission.hardware,
                ))?
            && budget.compatibility == HardwareCompatibility::Runnable
            && budget.blocking_reasons.is_empty()
            && budget.weight_bytes == profile.model_bytes()
            && budget.kv_cache_bytes == Some(profile.kv_cache_bytes())
            && budget.recurrent_cache_bytes == Some(0)
            && budget.dynamic_cache_bytes == Some(0)
            && budget.compute_buffer_bytes == envelope.compute_buffer_bytes
            && budget.auxiliary_buffer_bytes == envelope.auxiliary_buffer_bytes
            && budget.host_memory_required_bytes == Some(required)
            && budget.host_memory_available_bytes == available
            && available.is_some_and(|bytes| bytes >= required)
            && budget.peak_incremental_disk_bytes == peak_disk
            && admission.hardware.disk_free_bytes >= peak_disk,
        "Frozen vector admission differs from its measured resource profile"
    );
    Ok(profile)
}

/// These allowances cover one serialized operation on the measured CPU build.
/// They include the full GGUF weight charge, and do not assert a universal peak
/// for other builds, hardware backends, models or unbounded request batches.
pub fn baseline_vector_resource_envelope(
    profile: VectorRuntimeProfile,
    components: &[ComponentObservation],
) -> Result<AdapterResourceEnvelope> {
    let envelope = AdapterResourceEnvelope {
        runtime: baseline_pairing_runtime()?,
        component_digests: components
            .iter()
            .map(ComponentObservation::digest)
            .collect::<Result<_>>()?,
        workload: profile.resource_workload(),
        compute_buffer_bytes: 64 * MIB,
        auxiliary_buffer_bytes: profile.auxiliary_buffer_bytes(),
        evidence_id: VECTOR_RESOURCE_EVIDENCE.into(),
    };
    validate_measured_envelope(components, &envelope)?;
    Ok(envelope)
}

pub(super) fn validate_measured_envelope(
    components: &[ComponentObservation],
    envelope: &AdapterResourceEnvelope,
) -> Result<u64> {
    let profile = validate_envelope_parameters(envelope)?;
    ensure!(
        components.len() == 1
            && components[0].role == ComponentRole::Main
            && components[0].files.len() == 1,
        "Measured vector profile requires exactly one main GGUF component"
    );
    let component = &components[0];
    component.validate()?;
    let file = &component.files[0];
    ensure!(
        file.artifact
            .source_sha256
            .as_ref()
            .is_some_and(|sha| sha.as_str() == profile.model_sha256())
            && file.header.file_bytes == profile.model_bytes()
            && file.header.architecture() == Some(profile.resource_architecture())
            && file.header.shard.is_none(),
        "Vector component content differs from the measured memory profile"
    );
    match profile {
        VectorRuntimeProfile::Qwen3Embedding06bQ8 => {
            ensure!(
                super::cache_budget(&file.header, &envelope.workload)? == Some((56 * MIB, 0)),
                "Vector decoder cache metadata differs from its measured 56 MiB allocation"
            );
            Ok(56 * MIB)
        }
        // This exact BERT content/build has no autoregressive cache. The rule
        // must never generalize zero KV to an unrecognized encoder architecture.
        VectorRuntimeProfile::BgeRerankerV2M3Q4Km => Ok(0),
    }
}
