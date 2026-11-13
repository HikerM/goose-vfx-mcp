use super::vector_plan::SingleModelTask;
use super::*;
use anyhow::{ensure, Context, Result};
use std::collections::BTreeSet;

const HELPER_SHA256: &str = "5031f8cc21505b931581b96e37270e1cc8c6c378d38b3dfc9b1c254db8f9c58e";
const DENSE_4B_MEASUREMENT: &str =
    "cpu-text-measurement-v1:78776f846ad80bf0d548ba1219d6c253217b42010a7d66a077359d63582ffd24";

struct MeasuredTextProfile {
    id: &'static str,
    repository: &'static str,
    revision: &'static str,
    filename: &'static str,
    bytes: u64,
    weight_sha256: &'static str,
    header_sha256: &'static str,
    architecture: &'static str,
    profile_evidence: &'static str,
    budget_evidence: &'static str,
    compute_bytes: u64,
    auxiliary_bytes: u64,
    allows_extra_headroom: bool,
}

const PROFILES: &[MeasuredTextProfile] = &[
    MeasuredTextProfile {
        id: "measured-qwen35-08b-text-cpu",
        repository: "unsloth/Qwen3.5-0.8B-MTP-GGUF",
        revision: "3bec95a4bc613f19c4a38afd2cec6383aa2535ad",
        filename: "Qwen3.5-0.8B-Q8_0.gguf",
        bytes: 833_592_736,
        weight_sha256: "c54f8b67069c70085b98440de696b44da8250250ac69a961b41133def876e262",
        header_sha256: "872b80438a93d5cdf6a79edd4dcd57c620f24b40fe9314f1bd6ac335ae6a7508",
        architecture: "qwen35",
        profile_evidence: "m1:2026-11-01-m1-native-resource-measurements.json:bounded-exact-workload",
        budget_evidence: "m1:native-resource-measurements:r2:37.02MiB-compute-0.95MiB-output-with-explicit-headroom",
        compute_bytes: 64 * 1024 * 1024,
        auxiliary_bytes: 256 * 1024 * 1024,
        allows_extra_headroom: true,
    },
    MeasuredTextProfile {
        id: "measured-qwen35-4b-q4km-text-cpu",
        repository: "AtomicChat/Qwen3.5-4B-GGUF",
        revision: "3ba229851bf643577ee703364116751f3ac120b8",
        filename: "qwen35-4b-Q4_K_M.gguf",
        bytes: 2_708_804_608,
        weight_sha256: "c1c33fb44b3cea1d296174d58dc9f1449a0150cbc353679b7ad86955c240d49a",
        header_sha256: "a46de10368d44a335bb218b338c5c6c172c5089b6713d324f044456bbaf83981",
        architecture: "qwen35",
        profile_evidence: DENSE_4B_MEASUREMENT,
        budget_evidence: DENSE_4B_MEASUREMENT,
        // Windows peak private commit includes the repacked model, in addition to the mapped weights.
        compute_bytes: 128 * 1024 * 1024,
        auxiliary_bytes: 2560 * 1024 * 1024,
        allows_extra_headroom: false,
    },
];

fn profile_for_source(
    artifact: &Artifact,
    snapshot: &RepositorySnapshot,
) -> Result<&'static MeasuredTextProfile> {
    artifact.validate()?;
    snapshot.validate()?;
    ensure!(
        artifact.snapshot_id == snapshot.content_digest()?.as_str()
            && artifact.purpose == FilePurpose::GgufWeight,
        "Selected text model is not a GGUF weight from the frozen snapshot"
    );
    PROFILES
        .iter()
        .find(|profile| {
            snapshot.repository.as_str() == profile.repository
                && snapshot.resolved_revision == profile.revision
                && artifact.path.as_str() == profile.filename
                && artifact.size_bytes == Some(profile.bytes)
                && artifact
                    .source_sha256
                    .as_ref()
                    .is_some_and(|sha| sha.as_str() == profile.weight_sha256)
        })
        .context("Selected text model has no matching measured CPU profile")
}

pub(super) fn verify_text_source(artifact: &Artifact, snapshot: &RepositorySnapshot) -> Result<()> {
    profile_for_source(artifact, snapshot).map(|_| ())
}

pub(super) fn validate_text_launch(
    admission: &PlanAdmission,
    manifest: &ModelManifest,
) -> Result<()> {
    ensure!(
        manifest.plan.runtime == baseline_pairing_runtime()?
            && manifest.plan.runtime.binary_sha256.as_str() == HELPER_SHA256
            && manifest.plan.target_capabilities == BTreeSet::from([ModelCapability::Text])
            && manifest.plan.artifacts.len() == 1
            && manifest.installed_files.len() == 1,
        "Text launch has no matching measured resource profile"
    );
    let (id, artifact) = manifest.plan.artifacts.first_key_value().unwrap();
    let snapshot = manifest
        .plan
        .snapshots
        .get(&artifact.snapshot_id)
        .context("Missing text weight source snapshot")?;
    let profile = profile_for_source(artifact, snapshot)?;
    ensure!(
        admission.runtime_profile.id == profile.id
            && admission
                .runtime_profile
                .evidence_ids
                .contains(profile.profile_evidence)
            && admission.envelope.evidence_id == profile.budget_evidence,
        "Installed text weights have no matching measured CPU profile"
    );
    let envelope = &admission.envelope;
    // The original 0.8B plan allowed conservative headroom above the measured minima.
    let buffers_match = if profile.allows_extra_headroom {
        envelope.compute_buffer_bytes >= profile.compute_bytes
            && envelope.auxiliary_buffer_bytes >= profile.auxiliary_bytes
    } else {
        envelope.compute_buffer_bytes == profile.compute_bytes
            && envelope.auxiliary_buffer_bytes == profile.auxiliary_bytes
    };
    let header_evidence = gguf_header_evidence_id(id, &Sha256Digest::parse(profile.header_sha256)?);
    ensure!(
        buffers_match
            && manifest.plan.dependencies.components[&manifest.plan.dependencies.root]
                .evidence_ids
                .contains(&header_evidence)
            && manifest
                .installed_files
                .get(id)
                .is_some_and(
                    |file| file.computed_sha256.as_str() == profile.weight_sha256
                        && file.size_bytes == profile.bytes
                ),
        "Installed text weights, header or budget differ from the measured runtime profile"
    );
    Ok(())
}

pub(super) fn measured_text_plan_inputs(
    observation: &ComponentObservation,
) -> Result<(RuntimeInstallProfile, AdapterResourceEnvelope)> {
    let [file] = observation.files.as_slice() else {
        anyhow::bail!("Measured text profile requires exactly one main weight")
    };
    let measured = profile_for_source(&file.artifact, &file.snapshot)?;
    ensure!(
        observation.role == ComponentRole::Main
            && file.header.header_digest.as_str() == measured.header_sha256
            && file.header.architecture() == Some(measured.architecture),
        "Installed GGUF header differs from the measured text profile"
    );
    let runtime = baseline_pairing_runtime()?;
    ensure!(
        runtime.binary_sha256.as_str() == HELPER_SHA256,
        "CPU helper changed since text resource measurement"
    );
    let profile = RuntimeInstallProfile {
        id: measured.id.into(),
        rules_version: RUNTIME_INSTALL_PROFILE_VERSION.into(),
        runtime: runtime.clone(),
        architectures: BTreeSet::from([measured.architecture.into()]),
        capabilities: BTreeSet::from([ModelCapability::Text]),
        required_roles: BTreeSet::from([ComponentRole::Main]),
        evidence_ids: BTreeSet::from([measured.profile_evidence.into()]),
    };
    let envelope = AdapterResourceEnvelope {
        runtime,
        component_digests: BTreeSet::from([observation.digest()?]),
        workload: ResourceWorkload {
            context_tokens: 2048,
            batch_tokens: 512,
            parallel_sequences: 1,
            recurrent_rollback_slots: 0,
            kv_key_type: ResourceCacheType::F16,
            kv_value_type: ResourceCacheType::F16,
            prompt_cache_bytes: 0,
            context_checkpoint_slots: 0,
            vision: None,
            vector: None,
        },
        compute_buffer_bytes: measured.compute_bytes,
        auxiliary_buffer_bytes: measured.auxiliary_bytes,
        evidence_id: measured.budget_evidence.into(),
    };
    Ok((profile, envelope))
}

impl InstallationApplication {
    pub async fn prepare_text_plan(
        &self,
        repository: ModelScopeRepository,
        revision: &str,
        path: RepositoryPath,
    ) -> Result<ObservedPlanAssessment> {
        self.prepare_single_model_plan(SingleModelTask::Text, repository, revision, path)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_small_dense_model_gets_its_recorded_cpu_budget() -> Result<()> {
        let observation: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/lifecycle-vision/main-observation.json"
        ))?;
        let mut observation: ComponentObservation =
            serde_json::from_value(observation["observation"].clone())?;
        let recorded: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/lifecycle-vision/frozen-text-v2-plan.json"
        ))?;
        let historical: InstallPlan = serde_json::from_value(recorded["plan"].clone())?;
        let previous = historical.admission.unwrap();
        let db = LifecycleRepository::open(std::path::Path::new(":memory:"))?;
        let assessment = db.prepare_single_model_plan(
            SingleModelTask::Text,
            &observation,
            &previous.hardware,
            previous.hardware.observed_at,
        )?;
        let prepared = assessment.plan.unwrap().admission.unwrap();
        assert_eq!(prepared.runtime_profile, previous.runtime_profile);
        assert_eq!(prepared.envelope, previous.envelope);
        assert_eq!(prepared.budget, previous.budget);
        observation.files[0].header.header_digest = Sha256Digest::of_bytes(b"another header");
        assert!(measured_text_plan_inputs(&observation)
            .unwrap_err()
            .to_string()
            .contains("GGUF header differs"));
        Ok(())
    }

    #[test]
    fn exact_content_and_revision_are_required_before_text_plan_preparation() {
        let snapshot = RepositorySnapshot {
            repository: ModelScopeRepository::parse("AtomicChat/Qwen3.5-4B-GGUF").unwrap(),
            requested_revision: "3ba229851bf643577ee703364116751f3ac120b8".into(),
            resolved_revision: "3ba229851bf643577ee703364116751f3ac120b8".into(),
            files_digest: Sha256Digest::of_bytes(b"test inventory"),
            fetched_at: 1,
        };
        let mut artifact = Artifact {
            id: "test-weight".into(),
            snapshot_id: snapshot.content_digest().unwrap().as_str().to_owned(),
            path: RepositoryPath::parse("qwen35-4b-Q4_K_M.gguf").unwrap(),
            purpose: FilePurpose::GgufWeight,
            size_bytes: Some(2_708_804_608),
            source_sha256: Some(
                Sha256Digest::parse(
                    "c1c33fb44b3cea1d296174d58dc9f1449a0150cbc353679b7ad86955c240d49a",
                )
                .unwrap(),
            ),
        };
        verify_text_source(&artifact, &snapshot).unwrap();
        let mut changed = snapshot.clone();
        changed.resolved_revision = "b".repeat(40);
        artifact.snapshot_id = changed.content_digest().unwrap().as_str().to_owned();
        assert!(verify_text_source(&artifact, &changed).is_err());
        assert!(verify_text_source(&artifact, &snapshot).is_err());
        artifact.snapshot_id = snapshot.content_digest().unwrap().as_str().to_owned();
        artifact.source_sha256 = Some(Sha256Digest::of_bytes(b"different weights"));
        assert!(verify_text_source(&artifact, &snapshot).is_err());
    }
}
