use super::*;
use anyhow::Context;

/// A measurement file is untrusted diagnostic data. Validation checks its
/// internal bounds; only the source-pinned registry grants it issuance authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskProcessResourceMeasurement {
    pub pid: u32,
    pub peak_working_set_bytes: u64,
    pub peak_commit_bytes: u64,
    pub elapsed_millis: u64,
    pub exit_code: i32,
    pub process_tree_exited: bool,
    pub memory_samples_sha256: Sha256Digest,
    pub observed_output_peak_bytes: u64,
}

impl TaskProcessResourceMeasurement {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.pid > 0
                && self.elapsed_millis > 0
                && self.exit_code == 0
                && self.process_tree_exited
                && self.peak_working_set_bytes > 0
                && self.peak_commit_bytes > 0,
            "Task process measurement is incomplete or did not finish successfully"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResultPersistenceKind {
    Media,
    TranscriptRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskPublicationMeasurement {
    pub process: TaskProcessResourceMeasurement,
    pub kind: TaskResultPersistenceKind,
    pub implementation: String,
    pub implementation_sources_sha256: Sha256Digest,
    pub helper_binary_sha256: Sha256Digest,
    pub input_sha256: Sha256Digest,
    pub published_sha256: Sha256Digest,
    pub published_bytes: u64,
    /// Includes actual SQLite/WAL/journal and final/staging files in the isolated
    /// publication directory, separately from the native temporary output.
    pub peak_directory_bytes: u64,
    pub reopened_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskVramMeasurement {
    CpuOnly,
    Missing,
    Measured {
        devices: Vec<TaskGpuMemoryBudget>,
        samples_sha256: Sha256Digest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskResourceMeasurementProvenance {
    pub runner_sha256: Sha256Digest,
    pub fixture_sha256: Sha256Digest,
    /// Pins the actual argv, helper identities, process logs and samples. Review
    /// must verify those artifacts before pinning the complete measurement.
    pub execution_evidence_sha256: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskLifecycleTreeMeasurement {
    pub process_ids: Vec<u32>,
    pub peak_job_commit_bytes: u64,
    /// A conservative upper bound for concurrently resident process working sets.
    pub summed_peak_working_set_bytes: u64,
    pub samples_sha256: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskResourceMeasurementRecord {
    pub schema_version: u32,
    pub rules_version: String,
    pub provenance: TaskResourceMeasurementProvenance,
    pub key: TaskResourceKey,
    pub snapshot_digest: Sha256Digest,
    pub hardware: HardwareResourceSnapshot,
    pub controller_peak_commit_bytes: u64,
    pub lifecycle_tree: TaskLifecycleTreeMeasurement,
    pub native: TaskProcessResourceMeasurement,
    pub publication: Option<TaskPublicationMeasurement>,
    pub gpu: TaskVramMeasurement,
    pub native_exited_before_publication: bool,
    /// An actual measured record initially has None. Review must explicitly
    /// approve a bounded profile, then pin that full record's SHA in source.
    pub reviewed_budget: Option<TaskResourceBudget>,
}

impl TaskResourceMeasurementRecord {
    pub fn measured_host_peak_bytes(&self) -> Result<u64> {
        ensure!(
            self.native_exited_before_publication,
            "Overlapping native/publication phases need a simultaneous memory measurement"
        );
        let publication = self
            .publication
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Actual managed publication measurement is missing"))?;
        let tree = &self.lifecycle_tree;
        let ids: BTreeSet<_> = tree.process_ids.iter().copied().collect();
        ensure!(
            ids.len() == tree.process_ids.len()
                && !ids.contains(&0)
                && self.native.pid != publication.process.pid
                && ids.contains(&self.native.pid)
                && ids.contains(&publication.process.pid)
                && tree.peak_job_commit_bytes
                    >= self
                        .native
                        .peak_commit_bytes
                        .max(publication.process.peak_commit_bytes)
                && tree.summed_peak_working_set_bytes
                    >= self
                        .native
                        .peak_working_set_bytes
                        .checked_add(publication.process.peak_working_set_bytes)
                        .context("Task host peak overflow")?,
            "Measured process tree does not cover the host and native owner"
        );
        self.controller_peak_commit_bytes
            .checked_add(
                tree.peak_job_commit_bytes
                    .max(tree.summed_peak_working_set_bytes),
            )
            .ok_or_else(|| anyhow::anyhow!("Task host peak overflow"))
    }

    pub fn validate_for_registration(&self) -> Result<()> {
        ensure!(
            self.schema_version == 2 && self.rules_version == TASK_RESOURCE_RULES_VERSION,
            "Unsupported task resource measurement schema"
        );
        check_observation(&self.hardware, self.hardware.observed_at)?;
        self.native.validate()?;
        ensure!(
            self.controller_peak_commit_bytes > 0,
            "Controller resident memory is not measured"
        );
        let publication = self
            .publication
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Actual managed publication measurement is missing"))?;
        publication.process.validate()?;
        ensure!(
            (if self.key.kind == TaskResourceKind::Asr {
                publication.kind == TaskResultPersistenceKind::TranscriptRecord
                    && !publication.implementation.trim().is_empty()
            } else {
                publication.kind == TaskResultPersistenceKind::Media
                    && publication.implementation == "media::task_output::import_readers"
            }) && publication.reopened_verified
                && publication.published_bytes > 0
                && publication.input_sha256 == publication.published_sha256
                && self.native.observed_output_peak_bytes >= publication.published_bytes
                && publication.peak_directory_bytes >= publication.published_bytes,
            "Publication did not verify the actual output through the managed store"
        );
        let budget = self
            .reviewed_budget
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Measured task budget has not been reviewed"))?;
        budget.validate(self.key.kind)?;
        ensure!(
            budget.host_ram_bytes >= self.measured_host_peak_bytes()?,
            "Reviewed host budget is below the measured sequential lifecycle peak"
        );
        ensure!(
            budget.temporary_output_bytes >= self.native.observed_output_peak_bytes
                && budget.output_byte_limit >= self.native.observed_output_peak_bytes
                && budget.publication_copy_bytes >= publication.published_bytes
                && budget
                    .publication_copy_bytes
                    .checked_add(budget.publication_scratch_bytes)
                    .is_some_and(|bytes| bytes >= publication.peak_directory_bytes),
            "Reviewed disk budget does not cover measured output and managed publication"
        );
        ensure!(
            budget
                .timeout_seconds
                .checked_mul(1000)
                .is_some_and(|limit| limit >= self.native.elapsed_millis),
            "Reviewed deadline is below measured native execution time"
        );
        match &self.gpu {
            TaskVramMeasurement::CpuOnly => ensure!(
                !self.key.requires_gpu && budget.gpu_memory.is_empty(),
                "CPU measurement cannot approve a GPU budget"
            ),
            TaskVramMeasurement::Missing => {
                anyhow::bail!("Actual device VRAM measurement is missing")
            }
            TaskVramMeasurement::Measured { devices, .. } => {
                let mut indices = BTreeSet::new();
                ensure!(
                    self.key.requires_gpu
                        && !devices.is_empty()
                        && devices.len() == budget.gpu_memory.len(),
                    "Measured GPU device set differs from its budget"
                );
                for measured in devices {
                    ensure!(
                        measured.required_bytes > 0
                            && measured.required_bytes <= measured.device.total_bytes
                            && indices.insert(measured.device.index)
                            && self
                                .hardware
                                .devices
                                .iter()
                                .any(|device| TaskResourceDevice::from(device) == measured.device)
                            && budget
                                .gpu_memory
                                .iter()
                                .any(|approved| approved.device == measured.device
                                    && approved.required_bytes >= measured.required_bytes),
                        "Reviewed VRAM budget is below its actual device measurement"
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn into_profile(self, evidence_sha256: Sha256Digest) -> Result<TaskResourceProfile> {
        self.validate_for_registration()?;
        let hosts: Vec<_> = self
            .hardware
            .devices
            .iter()
            .filter(|device| device.kind == "cpu")
            .collect();
        ensure!(
            hosts.len() == 1,
            "Measurement must identify one exact host device"
        );
        Ok(TaskResourceProfile {
            key: self.key,
            host: TaskResourceDevice::from(hosts[0]),
            budget: self.reviewed_budget.expect("validated budget"),
            evidence_sha256,
        })
    }
}
