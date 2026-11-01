use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use std::collections::BTreeMap;

use super::{observations, GgufObservationRecord, LifecycleRepository};
use crate::lifecycle::{
    assess_resource_budget, ComponentObservation, FilePurpose, InstallPlan, Sha256Digest,
    PREPARED_GGUF_PLAN_RULES_VERSION,
};

pub(super) const SCHEMA: &str = include_str!("migrations/0006.sql");
pub const OBSERVED_GGUF_PLAN_RULES_VERSION: &str = "modelscope-gguf-plan-v1";

pub(crate) fn requires_gguf_observations(plan: &InstallPlan) -> bool {
    matches!(
        plan.rules_version.as_str(),
        OBSERVED_GGUF_PLAN_RULES_VERSION | PREPARED_GGUF_PLAN_RULES_VERSION
    )
}

pub fn gguf_observation_evidence_id(id: &Sha256Digest) -> String {
    format!("gguf-observation:{}", id.as_str())
}

pub fn gguf_header_evidence_id(artifact_id: &str, digest: &Sha256Digest) -> String {
    format!("gguf-header:{artifact_id}:{}", digest.as_str())
}

fn validate_bindings(
    connection: &Connection,
    plan: &InstallPlan,
    bindings: &BTreeMap<String, Sha256Digest>,
) -> Result<Vec<GgufObservationRecord>> {
    ensure!(
        plan.admission.is_some() == (plan.rules_version == PREPARED_GGUF_PLAN_RULES_VERSION),
        "Plan rules do not match its frozen admission profile"
    );
    if plan.rules_version == PREPARED_GGUF_PLAN_RULES_VERSION {
        let admission = plan
            .admission
            .as_ref()
            .context("Prepared plan has no frozen admission profile")?;
        admission.validate(plan, admission.hardware.observed_at)?;
    }
    let catalog = LifecycleRepository::read_catalog_by(connection, &plan.model_id)?
        .context("Observed plan model is absent from the catalog")?;
    let variant = catalog
        .variants
        .get(&plan.variant_id)
        .context("Observed plan variant is absent from its model")?;
    ensure!(
        variant
            .artifact_ids
            .iter()
            .all(|id| plan.artifacts.contains_key(id)),
        "Observed plan is missing variant artifacts"
    );
    for (id, artifact) in &plan.artifacts {
        ensure!(
            catalog.artifacts.get(id) == Some(artifact),
            "Observed plan artifact differs from the immutable catalog"
        );
    }
    for (id, component) in &plan.dependencies.components {
        let source = catalog
            .components
            .get(id)
            .context("Observed plan component is absent from the catalog")?;
        ensure!(
            source.role == component.role && source.artifact_ids == component.artifact_ids,
            "Observed plan component selection differs from the catalog"
        );
    }
    let weights: Vec<_> = plan
        .artifacts
        .values()
        .filter(|a| a.purpose == FilePurpose::GgufWeight)
        .collect();
    ensure!(
        !weights.is_empty() && weights.len() == bindings.len(),
        "Plan requires exactly one observation for every GGUF artifact"
    );
    let records: Vec<GgufObservationRecord> = weights
        .into_iter()
        .map(|artifact| {
            let id = bindings
                .get(&artifact.id)
                .context("Plan GGUF observation is missing")?;
            let record = observations::read_by(connection, id)?
                .context("Plan GGUF observation is absent from the cache")?;
            ensure!(
                record.observation.artifact == *artifact,
                "Plan observation belongs to a different artifact"
            );
            let snapshot = plan
                .snapshots
                .get(&artifact.snapshot_id)
                .context("Plan observation snapshot is missing")?;
            ensure!(
                snapshot.content_digest()? == record.observation.snapshot.content_digest()?,
                "Plan observation snapshot differs from its source"
            );
            for component in plan
                .dependencies
                .components
                .values()
                .filter(|c| c.artifact_ids.contains(&artifact.id))
            {
                ensure!(
                    component
                        .evidence_ids
                        .contains(&gguf_observation_evidence_id(id))
                        && component.evidence_ids.contains(&gguf_header_evidence_id(
                            &artifact.id,
                            &record.observation.header.header_digest
                        )),
                    "Plan digest does not bind its GGUF observation and header"
                );
            }
            Ok(record)
        })
        .collect::<Result<_>>()?;
    if let Some(admission) = &plan.admission {
        let by_artifact: BTreeMap<_, _> = records
            .iter()
            .map(|record| (&record.observation.artifact.id, &record.observation))
            .collect();
        let components: Vec<_> = plan
            .dependencies
            .components
            .values()
            .map(|component| {
                let files = component
                    .artifact_ids
                    .iter()
                    .map(|id| {
                        by_artifact
                            .get(id)
                            .map(|file| (*file).clone())
                            .context("Frozen admission component has no GGUF observation")
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(ComponentObservation {
                    role: component.role,
                    files,
                })
            })
            .collect::<Result<_>>()?;
        ensure!(
            assess_resource_budget(&components, &admission.envelope, &admission.hardware)?
                == admission.budget,
            "Frozen resource budget differs from cached component observations"
        );
        let root = plan
            .dependencies
            .components
            .get(&plan.dependencies.root)
            .context("Frozen admission has no root component")?;
        let primary = root
            .artifact_ids
            .iter()
            .filter_map(|id| by_artifact.get(id))
            .find(|file| {
                file.header
                    .shard
                    .as_ref()
                    .is_none_or(|shard| shard.index == 0)
            })
            .context("Frozen admission has no primary GGUF")?;
        ensure!(
            primary
                .header
                .architecture()
                .is_some_and(|architecture| admission
                    .runtime_profile
                    .architectures
                    .contains(architecture)),
            "Frozen runtime profile does not support the observed architecture"
        );
    }
    Ok(records)
}

impl LifecycleRepository {
    pub fn save_observed_plan(
        &self,
        plan: &InstallPlan,
        bindings: &BTreeMap<String, Sha256Digest>,
        now: i64,
        available_disk_bytes: u64,
    ) -> Result<Sha256Digest> {
        ensure!(
            requires_gguf_observations(plan),
            "Unsupported observed plan rules version"
        );
        plan.validate_for_install(now, available_disk_bytes)?;
        let digest = plan.digest()?;
        let payload = serde_json::to_string(plan)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("Model database lock poisoned"))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_bindings(&tx, plan, bindings)?;
        Self::store_plan_by(&tx, plan, &digest, &payload)?;
        for (artifact, observation) in bindings {
            tx.execute("INSERT INTO plan_gguf_observations (plan_id, artifact_id, observation_id) VALUES (?1, ?2, ?3) ON CONFLICT(plan_id, artifact_id) DO NOTHING",params![plan.id, artifact, observation.as_str()])?;
        }
        Self::validate_plan_observations_by(&tx, plan)?;
        tx.commit()?;
        Ok(digest)
    }

    pub(super) fn validate_plan_observations_by(
        connection: &Connection,
        plan: &InstallPlan,
    ) -> Result<()> {
        if !requires_gguf_observations(plan) {
            return Ok(());
        }
        let mut statement = connection.prepare("SELECT artifact_id, observation_id FROM plan_gguf_observations WHERE plan_id = ?1 ORDER BY artifact_id")?;
        let rows = statement
            .query_map([&plan.id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let bindings = rows
            .into_iter()
            .map(|(artifact, id)| Ok((artifact, Sha256Digest::parse(id)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        validate_bindings(connection, plan, &bindings)?;
        Ok(())
    }
}
