use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use super::{AdapterResourceEnvelope, ResourceCacheType, ResourceWorkload};
use crate::lifecycle::{
    baseline_pairing_runtime, ComponentObservation, ComponentRole, Sha256Digest,
};

mod png;

pub const VISION_RESOURCE_RULES_VERSION: &str = "local-resource-budget-vision-v1";
pub const VISION_RESOURCE_EVIDENCE: &str =
    "m1:2026-11-12-m1-vision-resource-measurements.json:bounded-cpu-v1";
const MAIN_SHA256: &str = "c54f8b67069c70085b98440de696b44da8250250ac69a961b41133def876e262";
const PROJECTOR_SHA256: &str = "ea8519d0c6240e465a0265d6912f73d750a17ca7d42150281b778b8b59f05798";
const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisionResizePolicy {
    Qwen3VlNativeV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisionResourceWorkload {
    pub resize_policy: VisionResizePolicy,
    pub max_images: u32,
    pub max_input_width: u32,
    pub max_input_height: u32,
    pub max_input_bytes: u64,
    pub native_min_image_tokens: u32,
    pub native_max_image_tokens: u32,
    pub max_encoded_image_tokens: u32,
    pub microbatch_tokens: u32,
    pub image_batch_tokens: u32,
    pub cpu_threads: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisionImageInput {
    pub width: u32,
    pub height: u32,
    pub encoded_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisionImageAllocation {
    pub resized_width: u32,
    pub resized_height: u32,
    pub image_tokens: u32,
}

#[derive(Clone)]
pub struct BoundedVisionImage {
    bytes: Vec<u8>,
    allocation: VisionImageAllocation,
    sha256: Sha256Digest,
}

impl BoundedVisionImage {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn allocation(&self) -> VisionImageAllocation {
        self.allocation
    }

    pub fn sha256(&self) -> &Sha256Digest {
        &self.sha256
    }
}

impl VisionResourceWorkload {
    pub fn measured_qwen35_cpu() -> Self {
        Self {
            resize_policy: VisionResizePolicy::Qwen3VlNativeV1,
            max_images: 1,
            max_input_width: 1024,
            max_input_height: 1024,
            max_input_bytes: 8 * MIB,
            native_min_image_tokens: 1024,
            native_max_image_tokens: 1024,
            max_encoded_image_tokens: 2048,
            microbatch_tokens: 512,
            image_batch_tokens: 1024,
            cpu_threads: 4,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_images == 1
                && (1..=1024).contains(&self.max_input_width)
                && (1..=1024).contains(&self.max_input_height)
                && (1..=8 * MIB).contains(&self.max_input_bytes)
                && self.native_min_image_tokens == 1024
                && self.native_max_image_tokens == 1024
                && self.max_encoded_image_tokens == 2048
                && self.microbatch_tokens == 512
                && self.image_batch_tokens == 1024
                && self.cpu_threads == 4,
            "Vision workload exceeds the measured CPU input and execution envelope"
        );
        Ok(())
    }

    /// The caller must supply every image in the submitted conversation, including history.
    pub fn inspect_images(&self, images: &[&[u8]]) -> Result<Vec<BoundedVisionImage>> {
        self.validate()?;
        ensure!(
            !images.is_empty() && images.len() <= self.max_images as usize,
            "Image count exceeds the frozen vision workload"
        );
        images
            .iter()
            .map(|bytes| {
                ensure!(
                    (33..=self.max_input_bytes).contains(&(bytes.len() as u64)),
                    "Encoded image exceeds the vision byte limit or has no complete PNG header"
                );
                ensure!(
                    &bytes[..8] == b"\x89PNG\r\n\x1a\n"
                        && bytes[8..12] == 13u32.to_be_bytes()
                        && &bytes[12..16] == b"IHDR"
                        && bytes[24] == 8
                        && matches!(bytes[25], 2 | 6)
                        && bytes[26..29] == [0, 0, 0],
                    "Measured vision inputs require non-interlaced 8-bit RGB or RGBA PNG data"
                );
                let input = VisionImageInput {
                    width: u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
                    height: u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
                    encoded_bytes: bytes.len() as u64,
                };
                let allocation = self.assess_inputs(&[input])?[0];
                png::validate_pixels(bytes, input.width, input.height, bytes[25])?;
                Ok(BoundedVisionImage {
                    bytes: bytes.to_vec(),
                    allocation,
                    sha256: Sha256Digest::from_sha256(Sha256::new_with_prefix(bytes)),
                })
            })
            .collect()
    }

    /// Inputs must be measured from bounded encoded data before full image decoding.
    pub fn assess_inputs(&self, images: &[VisionImageInput]) -> Result<Vec<VisionImageAllocation>> {
        self.validate()?;
        ensure!(
            !images.is_empty() && images.len() <= self.max_images as usize,
            "Image count exceeds the frozen vision workload"
        );
        images
            .iter()
            .map(|image| {
                ensure!(
                    (1..=self.max_input_width).contains(&image.width)
                        && (1..=self.max_input_height).contains(&image.height)
                        && (1..=self.max_input_bytes).contains(&image.encoded_bytes),
                    "Encoded image size or dimensions exceed the frozen vision workload"
                );
                let width = image.width as f32;
                let height = image.height as f32;
                let pixels = self.native_min_image_tokens as f32 * 1024.0;
                let mut resized_width = ((width / 32.0).round() as u32 * 32).max(32);
                let mut resized_height = ((height / 32.0).round() as u32 * 32).max(32);
                // Match the pinned helper's float32 ceil after minimum-pixel upscaling.
                // Its native maximum is not reapplied here and can be exceeded twofold.
                if resized_width * resized_height < pixels as u32 {
                    let scale = (pixels / (height * width)).sqrt();
                    resized_width = ((width * scale / 32.0).ceil() as u32) * 32;
                    resized_height = ((height * scale / 32.0).ceil() as u32) * 32;
                }
                let image_tokens = resized_width * resized_height / 1024;
                ensure!(
                    image_tokens <= self.max_encoded_image_tokens,
                    "Native image expansion exceeds the measured encoder token envelope"
                );
                Ok(VisionImageAllocation {
                    resized_width,
                    resized_height,
                    image_tokens,
                })
            })
            .collect()
    }
}

impl ResourceWorkload {
    /// The helper's multimodal input_tokens endpoint supplies the complete prompt count.
    pub fn validate_vision_prompt(&self, input_tokens: u64, output_tokens: u32) -> Result<()> {
        self.vision
            .as_ref()
            .context("The frozen workload has no vision input envelope")?
            .validate()?;
        let total = input_tokens
            .checked_add(u64::from(output_tokens))
            .context("Vision prompt token count overflow")?;
        ensure!(
            input_tokens > 0 && output_tokens > 0 && total <= u64::from(self.context_tokens),
            "Multimodal prompt and output exceed the frozen context capacity"
        );
        Ok(())
    }
}

pub fn baseline_vision_resource_envelope(
    components: &[ComponentObservation],
) -> Result<AdapterResourceEnvelope> {
    let envelope = AdapterResourceEnvelope {
        runtime: baseline_pairing_runtime()?,
        component_digests: components
            .iter()
            .map(ComponentObservation::digest)
            .collect::<Result<BTreeSet<_>>>()?,
        workload: ResourceWorkload {
            context_tokens: 4096,
            batch_tokens: 512,
            parallel_sequences: 1,
            recurrent_rollback_slots: 0,
            kv_key_type: ResourceCacheType::F16,
            kv_value_type: ResourceCacheType::F16,
            prompt_cache_bytes: 0,
            context_checkpoint_slots: 0,
            vision: Some(VisionResourceWorkload::measured_qwen35_cpu()),
            vector: None,
        },
        compute_buffer_bytes: 256 * MIB,
        auxiliary_buffer_bytes: 512 * MIB,
        evidence_id: VISION_RESOURCE_EVIDENCE.into(),
    };
    validate_measured_envelope(components, &envelope)?;
    Ok(envelope)
}

pub(super) fn validate_measured_envelope(
    components: &[ComponentObservation],
    envelope: &AdapterResourceEnvelope,
) -> Result<()> {
    let workload = &envelope.workload;
    workload
        .vision
        .as_ref()
        .context("Missing measured vision workload")?
        .validate()?;
    ensure!(
        envelope.runtime == baseline_pairing_runtime()?
            && envelope.evidence_id == VISION_RESOURCE_EVIDENCE
            && envelope.compute_buffer_bytes >= 256 * MIB
            && envelope.auxiliary_buffer_bytes >= 512 * MIB
            && workload.context_tokens == 4096
            && workload.batch_tokens == 512
            && workload.parallel_sequences == 1
            && workload.recurrent_rollback_slots == 0
            && workload.kv_key_type == ResourceCacheType::F16
            && workload.kv_value_type == ResourceCacheType::F16
            && workload.prompt_cache_bytes == 0
            && workload.context_checkpoint_slots == 0,
        "No measured vision memory profile matches this runtime and workload"
    );
    ensure!(
        components.len() == 2,
        "Measured vision requires main and projector components"
    );
    for (role, sha, architecture, bytes) in [
        (ComponentRole::Main, MAIN_SHA256, "qwen35", 833_592_736),
        (
            ComponentRole::VisionProjector,
            PROJECTOR_SHA256,
            "clip",
            204_987_104,
        ),
    ] {
        let component = components
            .iter()
            .find(|component| component.role == role)
            .context("Measured vision is missing a required component role")?;
        ensure!(
            component.files.len() == 1,
            "Measured vision component file set differs"
        );
        let file = &component.files[0];
        ensure!(
            file.artifact
                .source_sha256
                .as_ref()
                .is_some_and(|digest| digest.as_str() == sha)
                && file.header.architecture() == Some(architecture)
                && file.header.file_bytes == bytes,
            "Vision component content differs from the measured memory profile"
        );
    }
    Ok(())
}
