//! Admission for finite native tasks. A receipt is evidence-bound accounting,
//! not an OS memory reservation or a capability/quality readiness assertion.

mod conditions;
mod measurement;
mod sd_conditions;
pub use measurement::*;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{
    EffectiveGenerationParameters, HardwareMemoryDevice, HardwareResourceSnapshot, NativeTaskPlan,
    Sha256Digest,
};

pub const TASK_RESOURCE_RULES_VERSION: &str = "native-task-resources-v1";
pub const TASK_RESOURCE_OBSERVATION_MAX_AGE_SECONDS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResourceKind {
    Image,
    Video,
    Asr,
    Tts,
}

/// Binds the effective workload and any model-specific equivalence established by measurement.
/// Installation/session IDs do not affect measured costs; the receipt separately
/// binds the full snapshot, including those IDs, its manifest and component IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskResourceKey {
    pub kind: TaskResourceKind,
    pub runtime_digest: Sha256Digest,
    pub components_digest: Sha256Digest,
    pub workload_digest: Sha256Digest,
    pub requires_gpu: bool,
}

impl TaskResourceKey {
    pub fn from_plan(plan: &NativeTaskPlan) -> Result<Self> {
        let (kind, runtime_digest, components_digest, workload_digest) = match plan {
            NativeTaskPlan::Generation(task) => {
                let s = task.snapshot();
                let mut parameters = s.parameters.clone();
                if let EffectiveGenerationParameters::Video(video) = &mut parameters {
                    if let Some(input) = &mut video.first_frame {
                        input.session_id.clear();
                        input.result_id.clear();
                    }
                }
                let components: Vec<_> = s
                    .binding
                    .assets
                    .iter()
                    .map(|(role, asset)| (role, &asset.sha256, asset.size_bytes))
                    .collect();
                let runtime = Sha256Digest::of_serializable(&s.binding.runtime)?;
                let components = Sha256Digest::of_serializable(&components)?;
                let workload =
                    sd_conditions::generation_workload(s, &parameters, &runtime, &components)?;
                (
                    match &s.parameters {
                        EffectiveGenerationParameters::Image(_) => TaskResourceKind::Image,
                        EffectiveGenerationParameters::Video(_) => TaskResourceKind::Video,
                    },
                    runtime,
                    components,
                    workload,
                )
            }
            NativeTaskPlan::PocketTts(task) => {
                let (runtime, components, workload) = conditions::pocket_key(task.snapshot())?;
                (TaskResourceKind::Tts, runtime, components, workload)
            }
            NativeTaskPlan::Qwen3Asr(task) => {
                let s = task.snapshot();
                let b = &s.metadata.binding;
                let mut parameters = s.parameters.clone();
                parameters.audio.session_id.clear();
                parameters.audio.result_id.clear();
                (
                    TaskResourceKind::Asr,
                    Sha256Digest::of_serializable(&b.runtime)?,
                    Sha256Digest::of_serializable(&(
                        &b.model.sha256,
                        b.model.size_bytes,
                        &b.model_spec.sha256,
                        b.model_spec.size_bytes,
                    ))?,
                    Sha256Digest::of_serializable(&(
                        &parameters,
                        &s.metadata.backend,
                        s.metadata.cpu_threads,
                        s.metadata.requires_new_native_preparation,
                    ))?,
                )
            }
        };
        let requires_gpu = matches!(plan, NativeTaskPlan::Generation(task)
            if task.snapshot().engine.backend != "cpu");
        Ok(Self {
            kind,
            runtime_digest,
            components_digest,
            workload_digest,
            requires_gpu,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskResourceDevice {
    pub index: usize,
    pub backend: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub total_bytes: u64,
}

impl From<&HardwareMemoryDevice> for TaskResourceDevice {
    fn from(value: &HardwareMemoryDevice) -> Self {
        Self {
            index: value.index,
            backend: value.backend.clone(),
            name: value.name.clone(),
            description: value.description.clone(),
            kind: value.kind.clone(),
            total_bytes: value.total_bytes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskGpuMemoryBudget {
    pub device: TaskResourceDevice,
    pub required_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskResourceBudget {
    /// Covers the native process and the actual publication phase, including
    /// concurrently resident work. It is not just GGUF size or peak RSS.
    pub host_ram_bytes: u64,
    pub gpu_memory: Vec<TaskGpuMemoryBudget>,
    pub temporary_output_bytes: u64,
    pub publication_copy_bytes: u64,
    pub publication_scratch_bytes: u64,
    /// Aggregate limit over all outputs of this task.
    pub output_byte_limit: u64,
    pub timeout_seconds: u64,
}

impl TaskResourceBudget {
    pub fn peak_disk_bytes(&self) -> Result<u64> {
        self.temporary_output_bytes
            .checked_add(self.publication_copy_bytes)
            .and_then(|n| n.checked_add(self.publication_scratch_bytes))
            .ok_or_else(|| anyhow::anyhow!("Task disk budget overflow"))
    }

    fn validate(&self, kind: TaskResourceKind) -> Result<()> {
        ensure!(
            self.host_ram_bytes > 0 && self.output_byte_limit > 0 && self.timeout_seconds > 0,
            "Task budget is incomplete"
        );
        ensure!(
            self.temporary_output_bytes >= self.output_byte_limit,
            "Task output limit exceeds its temporary disk budget"
        );
        ensure!(
            kind == TaskResourceKind::Asr || self.publication_copy_bytes >= self.output_byte_limit,
            "Task publication copy is not budgeted"
        );
        self.peak_disk_bytes()?;
        let mut devices = BTreeSet::new();
        for gpu in &self.gpu_memory {
            ensure!(
                gpu.required_bytes > 0
                    && gpu.required_bytes <= gpu.device.total_bytes
                    && matches!(
                        gpu.device.kind.as_str(),
                        "gpu" | "integrated_gpu" | "accelerator"
                    )
                    && devices.insert(gpu.device.index),
                "Invalid task GPU budget"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProfilingRequirement {
    pub key: TaskResourceKey,
    pub evidence: &'static str,
    pub missing: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResourceRejection {
    pub code: &'static str,
    pub detail: String,
}

#[derive(Debug)]
pub enum TaskResourceAssessment {
    Admitted(TaskResourceAdmission),
    NeedsProfiling(TaskProfilingRequirement),
    Unavailable(TaskResourceRejection),
}

/// No Deserialize, public fields or public constructor. Only pinned, reviewed
/// measurement records can issue this receipt. The process owner must retain it
/// alongside the shared runtime permit until the real process has exited.
#[derive(Debug)]
pub struct TaskResourceAdmission {
    snapshot_digest: Sha256Digest,
    key: TaskResourceKey,
    observed: Box<HardwareResourceSnapshot>,
    observed_digest: Sha256Digest,
    profile: Box<TaskResourceProfile>,
}

impl TaskResourceAdmission {
    pub fn budget(&self) -> &TaskResourceBudget {
        &self.profile.budget
    }
    pub fn evidence_sha256(&self) -> &Sha256Digest {
        &self.profile.evidence_sha256
    }
    pub fn observation_digest(&self) -> &Sha256Digest {
        &self.observed_digest
    }

    /// `now` uses Unix seconds, like HardwareResourceSnapshot. Call under the
    /// runtime permit after probing the actual shared temporary/publication root.
    pub(crate) fn verify_now(
        &mut self,
        plan: &NativeTaskPlan,
        hardware: &HardwareResourceSnapshot,
        now: i64,
    ) -> Result<()> {
        ensure!(
            plan.snapshot_digest()? == self.snapshot_digest
                && TaskResourceKey::from_plan(plan)? == self.key,
            "Task resource receipt binding changed"
        );
        ensure!(
            hardware.storage_volume == self.observed.storage_volume,
            "Task output storage changed after resource admission"
        );
        ensure!(
            hardware.observed_at >= self.observed.observed_at,
            "Task hardware observation precedes the prior admission"
        );
        check_capacity(&self.profile, hardware, now)?;
        self.observed_digest = Sha256Digest::of_serializable(hardware)?;
        self.observed.as_mut().clone_from(hardware);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct TaskResourceProfile {
    key: TaskResourceKey,
    host: TaskResourceDevice,
    budget: TaskResourceBudget,
    evidence_sha256: Sha256Digest,
}

// Register only an immutable, reviewed record via include_str! and its separately
// pinned SHA. User configuration and measurement output paths are never loaded.
const TRUSTED_PROFILE_RECORDS: &[(&str, &str)] = &[
    (
        include_str!("task_resources/sd15-cpu-512-euler-a.json"),
        "aac4362892ac3c7bd151a9f7cec264a8c0217eec84f18b791ae6e96085154f29",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-1.json"),
        "309f6e67c7ce053d610429b637c6ea9eaeb2f0d9530e2fbb167407e499ba870b",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-2.json"),
        "18af9183690854ca2aab507c7a1be700ad1e7abf767b2361edf46e96cd37ff39",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-3.json"),
        "0649457c53438eaf803b1ed831607d8ce4fb5601d93ddb2c2a5c9869f13f0041",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-4.json"),
        "70d5c9ce3b92b9383478dfb38c37765a87d8aabc00d2a1e398d2804a1f8afb5c",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-5.json"),
        "d83c4263ac89f443c6985ca23bbb1e0d3c196c96f814fe9d27f522c45e260fa2",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-6.json"),
        "e23d9206f0a725444e61ec35366807076264c82f55bda623f7ce3453b45d12b9",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-7.json"),
        "7d48aac2af0b65cee76e152cfa292125d882b1efdbcbcad6dbf63774c494bf75",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-8.json"),
        "94210025e8ee88265982544f106776aa67b2069744951952da94cab9aaa93331",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-9.json"),
        "178a3f33adf1457ca833cfec838f2ca4cc1ec38d50ed8c30507be6b88f965dd3",
    ),
    (
        include_str!("task_resources/pocket-tts-cpu-case-10.json"),
        "23b532506f8001122e9370c323f0c301cd7991dcdb37be135e71dc9f146e677f",
    ),
];

fn trusted_profiles() -> Result<Vec<TaskResourceProfile>> {
    TRUSTED_PROFILE_RECORDS
        .iter()
        .map(|(body, expected)| {
            let digest = Sha256Digest::of_bytes(body.as_bytes());
            ensure!(
                digest == Sha256Digest::parse(*expected)?,
                "Trusted task resource evidence changed"
            );
            let record: TaskResourceMeasurementRecord = serde_json::from_str(body)?;
            record.into_profile(digest)
        })
        .collect()
}

/// The owner supplies a fresh native hardware observation, never a client JSON
/// observation. Only reviewed model, workload and hardware combinations can be admitted.
pub fn assess_task_resources(
    plan: &NativeTaskPlan,
    hardware: &HardwareResourceSnapshot,
    now: i64,
) -> Result<TaskResourceAssessment> {
    assess_with_profiles(plan, hardware, now, &trusted_profiles()?)
}

/// Samples the active runtime's devices and the configured task output root.
pub fn assess_current_task_resources(
    plan: &NativeTaskPlan,
    storage: &std::path::Path,
) -> Result<TaskResourceAssessment> {
    let _runtime = crate::InferenceRuntime::get_or_init()?;
    let now = chrono::Utc::now().timestamp();
    let observed = super::resources::probe_initialized_hardware_resources(storage, now)?;
    assess_task_resources(plan, &observed, chrono::Utc::now().timestamp())
}

fn assess_with_profiles(
    plan: &NativeTaskPlan,
    hardware: &HardwareResourceSnapshot,
    now: i64,
    profiles: &[TaskResourceProfile],
) -> Result<TaskResourceAssessment> {
    let key = TaskResourceKey::from_plan(plan)?;
    if let Err(error) = check_observation(hardware, now) {
        return Ok(TaskResourceAssessment::Unavailable(TaskResourceRejection {
            code: "invalid_or_expired_observation",
            detail: error.to_string(),
        }));
    }
    let mut reasons = Vec::new();
    for profile in profiles.iter().filter(|profile| profile.key == key) {
        if let Err(error) = check_capacity(profile, hardware, now) {
            reasons.push(error.to_string());
            continue;
        }
        return Ok(TaskResourceAssessment::Admitted(TaskResourceAdmission {
            snapshot_digest: plan.snapshot_digest()?,
            key,
            observed: Box::new(hardware.clone()),
            observed_digest: Sha256Digest::of_serializable(hardware)?,
            profile: Box::new(profile.clone()),
        }));
    }
    if reasons.is_empty() {
        Ok(TaskResourceAssessment::NeedsProfiling(
            profiling_requirement(plan, key),
        ))
    } else {
        Ok(TaskResourceAssessment::Unavailable(TaskResourceRejection {
            code: "resource_capacity_or_device_mismatch",
            detail: reasons.join("; "),
        }))
    }
}

fn check_observation(hardware: &HardwareResourceSnapshot, now: i64) -> Result<()> {
    ensure!(
        hardware.observed_at > 0
            && now >= hardware.observed_at
            && now.saturating_sub(hardware.observed_at)
                <= TASK_RESOURCE_OBSERVATION_MAX_AGE_SECONDS,
        "Task hardware observation is expired or from the future"
    );
    ensure!(
        !hardware.storage_volume.trim().is_empty(),
        "Task output storage observation is absent"
    );
    let mut indices = BTreeSet::new();
    for device in &hardware.devices {
        ensure!(
            device.total_bytes > 0
                && device.free_bytes <= device.total_bytes
                && !device.backend.is_empty()
                && !device.name.is_empty()
                && indices.insert(device.index),
            "Task hardware observation has an invalid or duplicate device"
        );
    }
    ensure!(
        hardware.devices.iter().any(|d| d.kind == "cpu"),
        "Task host memory observation is absent"
    );
    Ok(())
}

fn check_capacity(
    profile: &TaskResourceProfile,
    hardware: &HardwareResourceSnapshot,
    now: i64,
) -> Result<()> {
    check_observation(hardware, now)?;
    profile.budget.validate(profile.key.kind)?;
    ensure!(
        profile.key.requires_gpu != profile.budget.gpu_memory.is_empty(),
        "Task profile GPU requirement differs from its budget"
    );
    ensure!(
        profile.host.kind == "cpu",
        "Task profile has no host device"
    );
    let available = |identity: &TaskResourceDevice| {
        hardware
            .devices
            .iter()
            .find(|device| TaskResourceDevice::from(*device) == *identity)
            .map(|device| device.free_bytes)
    };
    ensure!(
        available(&profile.host).is_some_and(|free| free >= profile.budget.host_ram_bytes),
        "Task host differs from its measured device or has insufficient free RAM"
    );
    for gpu in &profile.budget.gpu_memory {
        ensure!(
            available(&gpu.device).is_some_and(|free| free >= gpu.required_bytes),
            "Task GPU differs from its measured device or has insufficient free VRAM"
        );
    }
    ensure!(
        hardware.disk_free_bytes >= profile.budget.peak_disk_bytes()?,
        "Task temporary output, publication copy and scratch exceed free disk space"
    );
    Ok(())
}

fn profiling_requirement(plan: &NativeTaskPlan, key: TaskResourceKey) -> TaskProfilingRequirement {
    let (evidence, missing) = match plan {
        NativeTaskPlan::Generation(task) => {
            let mut missing = vec!["Exact effective workload and real media publication RAM/disk measurement are required"];
            if key.requires_gpu {
                missing.push(
                    "Actual CUDA device VRAM peak is absent; --max-vram is not measured total VRAM",
                );
            } else {
                missing
                    .push("A validated whole-process RAM budget is required for this CPU workload");
            }
            (task.snapshot().profile.evidence(), missing)
        }
        NativeTaskPlan::PocketTts(_) | NativeTaskPlan::Qwen3Asr(_) => (
            super::AudioTaskProfile::for_parameter_schema(&plan.runtime().binding_version)
                .map_or("unregistered audio parameter schema", |profile| {
                    profile.evidence()
                }),
            vec![
                "Native smoke peaks do not cover this complete effective request and input domain",
                "Real result persistence and publication RAM/disk measurement are required",
            ],
        ),
    };
    TaskProfilingRequirement {
        key,
        evidence,
        missing,
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, windows))]
pub(crate) use tests::controlled_test_admission;
