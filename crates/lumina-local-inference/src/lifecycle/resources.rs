use anyhow::{ensure, Context, Result};
use llama_cpp_2::{
    list_llama_ggml_backend_devices, llama_backend::LlamaBackend, LlamaBackendDeviceType,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

use super::{
    ComponentObservation, ComponentRole, GgufInspection, HardwareCompatibility, RuntimeBuild,
    Sha256Digest,
};

pub const RESOURCE_RULES_VERSION: &str = "local-resource-budget-v2";
pub const RESOURCE_ENGINE_REVISION: &str = "dde879bc4f954b809892fd0d660515f42536394a";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareMemoryDevice {
    pub index: usize,
    pub backend: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareResourceSnapshot {
    pub observed_at: i64,
    pub devices: Vec<HardwareMemoryDevice>,
    pub storage_volume: String,
    pub disk_free_bytes: u64,
}

pub fn probe_hardware_resources(
    _backend: &LlamaBackend,
    storage: &Path,
    observed_at: i64,
) -> Result<HardwareResourceSnapshot> {
    let storage = storage.canonicalize()?;
    ensure!(
        storage.is_dir(),
        "Resource storage path must be a directory"
    );
    let devices = list_llama_ggml_backend_devices()
        .into_iter()
        .map(|device| HardwareMemoryDevice {
            index: device.index,
            backend: device.backend,
            name: device.name,
            description: device.description,
            kind: match device.device_type {
                LlamaBackendDeviceType::Cpu => "cpu",
                LlamaBackendDeviceType::Gpu => "gpu",
                LlamaBackendDeviceType::IntegratedGpu => "integrated_gpu",
                LlamaBackendDeviceType::Accelerator => "accelerator",
                LlamaBackendDeviceType::Unknown => "unknown",
            }
            .into(),
            total_bytes: device.memory_total as u64,
            free_bytes: device.memory_free as u64,
        })
        .collect();
    Ok(HardwareResourceSnapshot {
        observed_at,
        devices,
        storage_volume: storage.to_string_lossy().into(),
        disk_free_bytes: fs2::available_space(storage)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceCacheType {
    F16,
    F32,
}

impl ResourceCacheType {
    fn element_bytes(self) -> u64 {
        match self {
            Self::F16 => 2,
            Self::F32 => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceWorkload {
    pub context_tokens: u32,
    pub batch_tokens: u32,
    pub parallel_sequences: u32,
    pub recurrent_rollback_slots: u32,
    pub kv_key_type: ResourceCacheType,
    pub kv_value_type: ResourceCacheType,
    pub prompt_cache_bytes: u64,
    pub context_checkpoint_slots: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterResourceEnvelope {
    pub runtime: RuntimeBuild,
    pub component_digests: BTreeSet<Sha256Digest>,
    pub workload: ResourceWorkload,
    pub compute_buffer_bytes: u64,
    pub auxiliary_buffer_bytes: u64,
    pub evidence_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceBudget {
    pub rules_version: String,
    pub binding_digest: Sha256Digest,
    pub compatibility: HardwareCompatibility,
    pub weight_bytes: u64,
    pub kv_cache_bytes: Option<u64>,
    pub recurrent_cache_bytes: Option<u64>,
    pub dynamic_cache_bytes: Option<u64>,
    pub compute_buffer_bytes: u64,
    pub auxiliary_buffer_bytes: u64,
    pub host_memory_required_bytes: Option<u64>,
    pub host_memory_available_bytes: Option<u64>,
    pub peak_incremental_disk_bytes: u64,
    pub blocking_reasons: Vec<String>,
}

fn product(values: &[u64]) -> Result<u64> {
    values.iter().try_fold(1u64, |acc, value| {
        acc.checked_mul(*value)
            .context("Resource budget multiplication overflow")
    })
}

fn sum(values: impl IntoIterator<Item = u64>) -> Result<u64> {
    values.into_iter().try_fold(0u64, |acc, value| {
        acc.checked_add(value)
            .context("Resource budget addition overflow")
    })
}

fn number(header: &GgufInspection, architecture: &str, key: &str) -> Option<u64> {
    header
        .metadata
        .get(&format!("{architecture}.{key}"))?
        .as_u64()
}

fn cache_budget(
    header: &GgufInspection,
    workload: &ResourceWorkload,
) -> Result<Option<(u64, u64)>> {
    let Some(architecture) = header.architecture() else {
        return Ok(None);
    };
    if !matches!(
        architecture,
        "llama" | "qwen2" | "qwen2moe" | "qwen3" | "qwen3moe" | "qwen35" | "qwen35moe"
    ) {
        return Ok(None);
    }
    let dimensions = (|| {
        let layers = number(header, architecture, "block_count")?;
        let embedding = number(header, architecture, "embedding_length")?;
        let heads = number(header, architecture, "attention.head_count")?;
        let kv_heads = number(header, architecture, "attention.head_count_kv")?;
        let training_context = number(header, architecture, "context_length")?;
        if [layers, embedding, heads, kv_heads, training_context].contains(&0)
            || embedding % heads != 0
            || kv_heads > heads
            || u64::from(workload.context_tokens) > training_context
        {
            return None;
        }
        let key = number(header, architecture, "attention.key_length").unwrap_or(embedding / heads);
        let value =
            number(header, architecture, "attention.value_length").unwrap_or(embedding / heads);
        (key > 0 && value > 0).then_some((layers, kv_heads, key, value))
    })();
    let Some((layers, kv_heads, key, value)) = dimensions else {
        return Ok(None);
    };
    let (attention_layers, recurrent_bytes) = if matches!(architecture, "qwen35" | "qwen35moe") {
        if header
            .metadata
            .contains_key(&format!("{architecture}.attention.recurrent_layers"))
        {
            return Ok(None);
        }
        let hybrid = (|| {
            Some((
                number(header, architecture, "full_attention_interval")?,
                number(header, architecture, "ssm.conv_kernel")?,
                number(header, architecture, "ssm.inner_size")?,
                number(header, architecture, "ssm.state_size")?,
                number(header, architecture, "ssm.group_count")?,
            ))
        })();
        let Some((interval, conv, inner, state, groups)) = hybrid else {
            return Ok(None);
        };
        let mtp = number(header, architecture, "nextn_predict_layers").unwrap_or(0);
        if interval == 0 || conv == 0 || inner == 0 || state == 0 || groups == 0 || mtp >= layers {
            return Ok(None);
        }
        let trunk = layers - mtp;
        let attention = trunk / interval + mtp;
        let recurrent = layers - attention;
        let conv_width = sum([inner, product(&[2, groups, state])?])?;
        let state_elements = sum([product(&[conv - 1, conv_width])?, product(&[state, inner])?])?;
        (
            attention,
            product(&[
                recurrent,
                state_elements,
                4,
                u64::from(workload.parallel_sequences),
                u64::from(workload.recurrent_rollback_slots) + 1,
            ])?,
        )
    } else {
        (layers, 0)
    };
    let padded_context = u64::from(workload.context_tokens).div_ceil(256) * 256;
    let kv = product(&[
        attention_layers,
        kv_heads,
        sum([
            product(&[key, workload.kv_key_type.element_bytes()])?,
            product(&[value, workload.kv_value_type.element_bytes()])?,
        ])?,
        padded_context,
        u64::from(workload.parallel_sequences),
    ])?;
    Ok(Some((kv, recurrent_bytes)))
}

pub fn assess_resource_budget(
    components: &[ComponentObservation],
    envelope: &AdapterResourceEnvelope,
    hardware: &HardwareResourceSnapshot,
) -> Result<ResourceBudget> {
    ensure!(
        !components.is_empty(),
        "Resource assessment has no components"
    );
    ensure!(
        envelope.workload.context_tokens > 0
            && envelope.workload.batch_tokens > 0
            && envelope.workload.batch_tokens <= envelope.workload.context_tokens
            && envelope.workload.parallel_sequences > 0,
        "Invalid resource workload"
    );
    ensure!(
        envelope.compute_buffer_bytes > 0 && !envelope.evidence_id.trim().is_empty(),
        "Adapter compute envelope requires workload-specific evidence"
    );
    let mut artifacts = BTreeSet::new();
    let mut digests = BTreeSet::new();
    let mut weight_bytes = 0u64;
    let mut kv = 0u64;
    let mut recurrent = 0u64;
    let mut reasons = Vec::new();
    if envelope.runtime.engine_revision != RESOURCE_ENGINE_REVISION {
        reasons.push("Runtime revision has no registered cache accounting rules".into());
    }
    for component in components {
        component.validate()?;
        ensure!(
            digests.insert(component.digest()?),
            "Duplicate resource component"
        );
        for file in &component.files {
            if artifacts.insert(file.artifact.id.clone()) {
                weight_bytes = sum([weight_bytes, file.header.file_bytes])?;
            }
        }
        if !matches!(
            component.role,
            ComponentRole::Main | ComponentRole::Mtp | ComponentRole::Dflash
        ) {
            reasons.push(format!(
                "No workload memory profile for component role {:?}",
                component.role
            ));
            continue;
        }
        let primary = component
            .files
            .iter()
            .find(|f| f.header.shard.as_ref().is_none_or(|s| s.index == 0))
            .context("Missing primary resource header")?;
        match cache_budget(&primary.header, &envelope.workload)? {
            Some((k, r)) => {
                kv = sum([kv, k])?;
                recurrent = sum([recurrent, r])?;
            }
            None => reasons.push(format!(
                "Unsupported or incomplete cache metadata for {}",
                primary.artifact.id
            )),
        }
    }
    ensure!(
        digests == envelope.component_digests,
        "Adapter resource envelope belongs to different components"
    );
    let available = hardware
        .devices
        .iter()
        .filter(|d| d.kind == "cpu" && d.free_bytes > 0 && d.free_bytes <= d.total_bytes)
        .map(|d| d.free_bytes)
        .max();
    if available.is_none() {
        reasons.push("Host memory could not be measured".into());
    }
    let known = reasons.is_empty();
    let dynamic_cache_bytes = sum([
        envelope.workload.prompt_cache_bytes,
        product(&[
            sum([kv, recurrent])?,
            u64::from(envelope.workload.context_checkpoint_slots),
        ])?,
    ])?;
    let required = known
        .then(|| {
            sum([
                weight_bytes,
                kv,
                recurrent,
                dynamic_cache_bytes,
                envelope.compute_buffer_bytes,
                envelope.auxiliary_buffer_bytes,
            ])
        })
        .transpose()?;
    let peak_disk = sum([product(&[weight_bytes, 3])?, 16 * 1024 * 1024])?;
    let compatibility = if peak_disk > hardware.disk_free_bytes {
        reasons.push("Insufficient disk for staging, blob and installed copies".into());
        HardwareCompatibility::Incompatible
    } else if !known {
        HardwareCompatibility::Unknown
    } else if required > available {
        reasons.push("Insufficient host memory for complete weights and runtime buffers".into());
        HardwareCompatibility::Incompatible
    } else {
        HardwareCompatibility::Runnable
    };
    Ok(ResourceBudget {
        rules_version: RESOURCE_RULES_VERSION.into(),
        binding_digest: Sha256Digest::of_serializable(&(
            RESOURCE_RULES_VERSION,
            envelope,
            hardware,
        ))?,
        compatibility,
        weight_bytes,
        kv_cache_bytes: known.then_some(kv),
        recurrent_cache_bytes: known.then_some(recurrent),
        dynamic_cache_bytes: known.then_some(dynamic_cache_bytes),
        compute_buffer_bytes: envelope.compute_buffer_bytes,
        auxiliary_buffer_bytes: envelope.auxiliary_buffer_bytes,
        host_memory_required_bytes: required,
        host_memory_available_bytes: available,
        peak_incremental_disk_bytes: peak_disk,
        blocking_reasons: reasons,
    })
}
