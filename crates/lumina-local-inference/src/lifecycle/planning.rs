use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use super::*;

pub const RUNTIME_INSTALL_PROFILE_VERSION: &str = "runtime-install-profile-v1";
pub const PREPARED_GGUF_PLAN_RULES_VERSION: &str = "modelscope-gguf-plan-v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanAdmission {
    pub runtime_profile: RuntimeInstallProfile,
    pub envelope: AdapterResourceEnvelope,
    pub hardware: HardwareResourceSnapshot,
    pub budget: ResourceBudget,
}

impl PlanAdmission {
    pub fn validate(&self, plan: &InstallPlan, now: i64) -> Result<()> {
        ensure!(
            self.runtime_profile.rules_version == RUNTIME_INSTALL_PROFILE_VERSION
                && self.budget.rules_version == RESOURCE_RULES_VERSION,
            "Unsupported frozen admission rules"
        );
        ensure!(
            self.runtime_profile.runtime == plan.runtime
                && self.envelope.runtime == plan.runtime
                && plan
                    .target_capabilities
                    .is_subset(&self.runtime_profile.capabilities),
            "Frozen runtime profile differs from the plan"
        );
        ensure!(
            now >= self.hardware.observed_at && now.saturating_sub(self.hardware.observed_at) <= 60,
            "Frozen hardware observation has expired"
        );
        ensure!(
            self.budget.binding_digest
                == Sha256Digest::of_serializable(&(
                    RESOURCE_RULES_VERSION,
                    &self.envelope,
                    &self.hardware
                ))?
                && self.budget.compatibility == plan.hardware
                && self.budget.peak_incremental_disk_bytes == plan.required_disk_bytes,
            "Frozen resource assessment differs from its plan"
        );
        let root = plan
            .dependencies
            .components
            .get(&plan.dependencies.root)
            .context("Missing frozen root component")?;
        ensure!(
            root.evidence_ids.contains(&format!(
                "runtime-install-profile:{}",
                Sha256Digest::of_serializable(&self.runtime_profile)?.as_str()
            )) && root.evidence_ids.contains(&format!(
                "resource-budget:{}",
                self.budget.binding_digest.as_str()
            )),
            "Plan digest does not bind its frozen admission profile"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInstallProfile {
    pub id: String,
    pub rules_version: String,
    pub runtime: RuntimeBuild,
    pub architectures: BTreeSet<String>,
    pub capabilities: BTreeSet<ModelCapability>,
    pub required_roles: BTreeSet<ComponentRole>,
    pub evidence_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedPlanRequest {
    pub id: String,
    pub model_id: String,
    pub variant_id: String,
    pub target_capabilities: BTreeSet<ModelCapability>,
    pub dependencies: DependencyGraph,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanBlockingReason {
    pub code: LifecycleErrorCode,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedPlanAssessment {
    pub plan: Option<InstallPlan>,
    pub observation_bindings: BTreeMap<String, Sha256Digest>,
    pub resource_budget: Option<ResourceBudget>,
    pub dependencies: DependencyGraph,
    pub blocking_reasons: Vec<PlanBlockingReason>,
}

impl ObservedPlanAssessment {
    fn block(&mut self, code: LifecycleErrorCode, detail: impl Into<String>) {
        self.blocking_reasons.push(PlanBlockingReason {
            code,
            detail: detail.into(),
        });
    }
}

impl LifecycleRepository {
    /// Profiles and envelopes must come from the server-owned runtime registry, never client input.
    pub fn prepare_observed_plan(
        &self,
        request: &ObservedPlanRequest,
        profile: &RuntimeInstallProfile,
        envelope: &AdapterResourceEnvelope,
        hardware: &HardwareResourceSnapshot,
        now: i64,
    ) -> Result<ObservedPlanAssessment> {
        request.dependencies.validate()?;
        ensure!(
            !request.id.trim().is_empty() && !request.target_capabilities.is_empty(),
            "Plan request has no identity or target capability"
        );
        let catalog = self
            .read_catalog(&request.model_id)?
            .context("Plan model is absent from the catalog")?;
        let variant = catalog
            .variants
            .get(&request.variant_id)
            .context("Plan variant is absent from this model")?;
        let mut result = ObservedPlanAssessment {
            plan: None,
            observation_bindings: BTreeMap::new(),
            resource_budget: None,
            dependencies: request.dependencies.clone(),
            blocking_reasons: vec![],
        };
        if request.dependencies.completeness()? != ComponentCompleteness::Complete {
            result.block(
                LifecycleErrorCode::DependencyUnverified,
                "Selected components have no verified dependency decision",
            );
        }
        if profile.rules_version != RUNTIME_INSTALL_PROFILE_VERSION
            || profile.id.trim().is_empty()
            || profile.evidence_ids.is_empty()
            || !request.target_capabilities.is_subset(&profile.capabilities)
            || profile.runtime != envelope.runtime
        {
            result.block(
                LifecycleErrorCode::RuntimeUnsupported,
                "Runtime profile or workload envelope does not support this request",
            );
        }
        if now < hardware.observed_at || now.saturating_sub(hardware.observed_at) > 60 {
            result.block(
                LifecycleErrorCode::PlanStale,
                "Hardware observation is stale",
            );
        }
        let mut artifacts = BTreeMap::new();
        let mut observations = BTreeMap::new();
        for (id, component) in &request.dependencies.components {
            let registered = catalog
                .components
                .get(id)
                .context("Selected component is absent from the catalog")?;
            ensure!(
                registered.role == component.role
                    && registered.artifact_ids == component.artifact_ids,
                "Selected component differs from its registered files"
            );
            let mut files = Vec::new();
            for artifact_id in &component.artifact_ids {
                let artifact = catalog
                    .artifacts
                    .get(artifact_id)
                    .context("Selected artifact is absent from the catalog")?;
                artifacts.insert(artifact_id.clone(), artifact.clone());
                if artifact.purpose != FilePurpose::GgufWeight {
                    result.block(
                        LifecycleErrorCode::DependencyUnverified,
                        "This planner requires a dedicated metadata admission profile",
                    );
                    continue;
                }
                let records = self.gguf_observations_for_artifact(artifact_id)?;
                if records.len() != 1 {
                    result.block(
                        LifecycleErrorCode::DependencyUnverified,
                        format!("Artifact has no unique cached GGUF observation: {artifact_id}"),
                    );
                    continue;
                }
                let record = &records[0];
                ensure!(
                    record.observation.artifact == *artifact,
                    "Cached observation differs from the selected artifact"
                );
                result
                    .observation_bindings
                    .insert(artifact_id.clone(), record.id.clone());
                let component = result
                    .dependencies
                    .components
                    .get_mut(id)
                    .context("Plan component disappeared")?;
                component.evidence_ids.extend([
                    gguf_observation_evidence_id(&record.id),
                    gguf_header_evidence_id(artifact_id, &record.observation.header.header_digest),
                ]);
                files.push(record.observation.clone());
            }
            if files.len() == component.artifact_ids.len() && !files.is_empty() {
                let observed = ComponentObservation {
                    role: component.role,
                    files,
                };
                observed.validate()?;
                observations.insert(id.clone(), observed);
            }
        }
        ensure!(
            variant
                .artifact_ids
                .iter()
                .all(|id| artifacts.contains_key(id)),
            "Plan selection omits a variant file"
        );
        let roles: BTreeSet<_> = result
            .dependencies
            .components
            .values()
            .map(|component| component.role)
            .collect();
        if !profile.required_roles.is_subset(&roles) {
            result.block(
                LifecycleErrorCode::DependencyMissing,
                "Runtime profile is missing required component roles",
            );
        }
        let root = observations.get(&request.dependencies.root);
        if let Some(root) = root {
            let primary = root
                .files
                .iter()
                .find(|file| {
                    file.header
                        .shard
                        .as_ref()
                        .is_none_or(|shard| shard.index == 0)
                })
                .context("Root component has no primary GGUF header")?;
            if primary
                .header
                .architecture()
                .is_none_or(|architecture| !profile.architectures.contains(architecture))
            {
                result.block(
                    LifecycleErrorCode::RuntimeUnsupported,
                    "Observed model architecture has no matching runtime profile",
                );
            }
        } else {
            result.block(
                LifecycleErrorCode::DependencyUnverified,
                "Root component metadata is incomplete",
            );
        }
        if result.dependencies.completeness()? != ComponentCompleteness::Complete {
            result.block(
                LifecycleErrorCode::DependencyUnverified,
                "Component compatibility or dependency conditions remain unresolved",
            );
        }
        if observations.len() == request.dependencies.components.len() {
            let components: Vec<_> = observations.into_values().collect();
            let budget = assess_resource_budget(&components, envelope, hardware)?;
            if !matches!(
                budget.compatibility,
                HardwareCompatibility::Runnable | HardwareCompatibility::Limited
            ) {
                let code = if budget.peak_incremental_disk_bytes > hardware.disk_free_bytes {
                    LifecycleErrorCode::DiskInsufficient
                } else {
                    LifecycleErrorCode::HardwareInsufficient
                };
                result.block(code, "Resource budget is insufficient or unverified");
            }
            result.resource_budget = Some(budget);
        }
        if !result.blocking_reasons.is_empty() {
            return Ok(result);
        }
        let budget = result
            .resource_budget
            .as_ref()
            .context("Executable plan has no resource budget")?;
        let root = result
            .dependencies
            .components
            .get_mut(&request.dependencies.root)
            .context("Missing root component")?;
        root.evidence_ids
            .extend(profile.evidence_ids.iter().cloned());
        root.evidence_ids.extend([
            format!(
                "runtime-install-profile:{}",
                Sha256Digest::of_serializable(profile)?.as_str()
            ),
            format!("resource-budget:{}", budget.binding_digest.as_str()),
        ]);
        let selected_snapshots: BTreeSet<_> = artifacts
            .values()
            .map(|artifact| &artifact.snapshot_id)
            .collect();
        let snapshots = catalog
            .snapshots
            .into_iter()
            .filter(|(id, _)| selected_snapshots.contains(id))
            .collect();
        let plan = InstallPlan {
            schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
            id: request.id.clone(),
            model_id: request.model_id.clone(),
            variant_id: request.variant_id.clone(),
            target_capabilities: request.target_capabilities.clone(),
            snapshots,
            artifacts,
            dependencies: result.dependencies.clone(),
            runtime: profile.runtime.clone(),
            runtime_verified: true,
            hardware: budget.compatibility,
            required_disk_bytes: budget.peak_incremental_disk_bytes,
            rules_version: PREPARED_GGUF_PLAN_RULES_VERSION.into(),
            expires_at: now.checked_add(300).context("Plan expiry overflow")?,
            admission: Some(PlanAdmission {
                runtime_profile: profile.clone(),
                envelope: envelope.clone(),
                hardware: hardware.clone(),
                budget: budget.clone(),
            }),
        };
        plan.validate_for_install(now, hardware.disk_free_bytes)?;
        self.save_observed_plan(
            &plan,
            &result.observation_bindings,
            now,
            hardware.disk_free_bytes,
        )?;
        result.plan = Some(plan);
        Ok(result)
    }
}
