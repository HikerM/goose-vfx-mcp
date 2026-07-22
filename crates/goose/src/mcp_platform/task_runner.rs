use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::lifecycle::{
    AuthRequirement, DirectSpawnDescriptor, HealthExecution, LifecyclePorts, ProjectionSnapshot,
    RegistrationEffect,
};
use super::managed_distribution::{
    DistributionEffectAdapter, ExternalManagedAcquisition, ManagedInstallEffect,
};
use super::manifest::digest_serializable;
use super::plan::PlanStep;
use super::repository::{
    ActivateManagedInstallation, CompensationTransition, ManagedMcpInventoryRecord,
    NewHealthObservation, ProjectionMutationStatus, PutOwnedProjection, RegisterManagedMcp,
    RestoreOwnedProjection, StageManagedInstallation, StepTransition, TaskRecord, TaskTransition,
};
use super::service::{Clock, McpPlatformRepositoryPort};
use super::task::{
    CompensationDescriptor, CompensationStatus, RecoveryDecision, RedactedError, RedactedErrorCode,
    RollbackEvidence, RollbackStatus, StepEvidence, TaskOperation, TaskStatus, TaskStepStatus,
};
use super::{HealthCheckMode, HealthDetailCode, HealthResultCode};

const LEASE_DURATION_MS: i64 = 30_000;
const STALE_HEARTBEAT_MS: i64 = 60_000;
const STALE_ARTIFACT_CLAIM_MS: i64 = 30 * 60 * 1_000;

pub struct TaskRunner {
    repository: Arc<dyn McpPlatformRepositoryPort>,
    clock: Arc<dyn Clock>,
    ports: LifecyclePorts,
    owner_id: String,
    active: Mutex<HashMap<String, CancellationToken>>,
    wake: Notify,
    shutdown: CancellationToken,
    distribution: Option<Arc<dyn DistributionEffectAdapter>>,
}

impl TaskRunner {
    pub fn new(
        repository: Arc<dyn McpPlatformRepositoryPort>,
        clock: Arc<dyn Clock>,
        ports: LifecyclePorts,
        owner_id: String,
    ) -> Self {
        Self {
            repository,
            clock,
            ports,
            owner_id,
            active: Mutex::new(HashMap::new()),
            wake: Notify::new(),
            shutdown: CancellationToken::new(),
            distribution: None,
        }
    }

    pub fn with_distribution_adapter(
        mut self,
        adapter: Arc<dyn DistributionEffectAdapter>,
    ) -> Self {
        self.distribution = Some(adapter);
        self
    }

    pub fn notify(&self) {
        self.wake.notify_one();
    }

    pub async fn cancel(&self, task_id: &str) {
        if let Some(token) = self.active.lock().expect("runner active lock").get(task_id) {
            token.cancel();
        }
        self.notify();
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
        for token in self.active.lock().expect("runner active lock").values() {
            token.cancel();
        }
        self.wake.notify_waiters();
    }

    pub async fn tick(&self) -> McpPlatformResult<bool> {
        self.recover_projection_mutations().await?;
        let Some(task) = self
            .repository
            .claim_next_task(&self.owner_id, self.clock.now_ms(), LEASE_DURATION_MS)
            .await?
        else {
            return Ok(false);
        };
        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .expect("runner active lock")
            .insert(task.task_id.clone(), cancellation.clone());
        let heartbeat_stop = CancellationToken::new();
        let execution = self.execute(task.clone(), cancellation.clone());
        let heartbeat = self.heartbeat(
            task.task_id.clone(),
            cancellation.clone(),
            heartbeat_stop.clone(),
        );
        tokio::pin!(execution);
        tokio::pin!(heartbeat);
        let result = tokio::select! {
            result = &mut execution => {
                heartbeat_stop.cancel();
                heartbeat.await;
                result
            }
            () = &mut heartbeat => execution.await,
        };
        if let Some(token) = self
            .active
            .lock()
            .expect("runner active lock")
            .remove(&task.task_id)
        {
            token.cancel();
        }
        if let Err(error) = result {
            self.settle_execution_error(&task.task_id, error).await?;
        }
        Ok(true)
    }

    pub async fn set_default_enabled(
        &self,
        managed_mcp_id: &str,
        expected_revision: i64,
        enabled: bool,
    ) -> McpPlatformResult<ManagedMcpInventoryRecord> {
        let resumes_pending_mutation = self
            .repository
            .list_pending_projection_mutations()
            .await?
            .iter()
            .any(|mutation| {
                mutation.managed_mcp_id == managed_mcp_id
                    && mutation.expected_revision == expected_revision
                    && mutation.desired_enabled == enabled
            });
        self.recover_projection_mutations().await?;
        let inventory = self
            .repository
            .get_managed_inventory(managed_mcp_id)
            .await?;
        if inventory.managed.revision != expected_revision && !resumes_pending_mutation {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::RevisionConflict,
                "managed MCP revision changed",
            ));
        }
        if enabled {
            if inventory.managed.state.registration != super::RegistrationState::Registered {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::InvalidTransition,
                    "managed MCP is not registered",
                ));
            }
            let manifest_digest = inventory
                .lifecycle
                .active_manifest_digest
                .as_deref()
                .ok_or_else(integrity_error)?;
            let manifest = self.repository.get_manifest(manifest_digest).await?;
            if inventory.managed.state.health != super::HealthState::Healthy {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::HealthFailed,
                    "managed MCP must be healthy before it can be enabled",
                ));
            }
            if !matches!(
                self.ports
                    .auth
                    .requirement(&manifest.verified.manifest().auth)?,
                AuthRequirement::Ready
            ) {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::CredentialMissing,
                    "managed MCP credential handle is unavailable",
                ));
            }
            let projection = self
                .repository
                .get_connection_projection(managed_mcp_id)
                .await?;
            let plan_id = projection.plan_id.as_deref().ok_or_else(integrity_error)?;
            if self
                .repository
                .get_plan(plan_id)
                .await?
                .policy_evidence
                .is_denied()
            {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::PolicyDenied,
                    "managed MCP policy no longer permits enablement",
                ));
            }
        }
        let projection = match self
            .repository
            .get_connection_projection(managed_mcp_id)
            .await
        {
            Ok(projection) => Some(projection),
            Err(error) if !enabled && error.code() == McpPlatformErrorCode::NotFound => None,
            Err(error) => return Err(error),
        };
        if enabled {
            let projection = projection.as_ref().ok_or_else(integrity_error)?;
            let expected_config = self
                .ports
                .transport
                .extension_config(&projection.projection, &projection.link_key)?;
            let current = self
                .ports
                .projection_sink
                .get(&projection.link_key)
                .await?
                .ok_or_else(integrity_error)?;
            if current.entry.config != expected_config {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::ProjectionConflict,
                    "managed MCP projection drifted from platform authority",
                ));
            }
            if current.entry.enabled && inventory.managed.state.default_enabled {
                return Ok(inventory);
            }
        }
        if !enabled && !inventory.managed.state.default_enabled {
            if let Some(projection) = projection.as_ref() {
                if self
                    .ports
                    .projection_sink
                    .get(&projection.link_key)
                    .await?
                    .is_none_or(|snapshot| !snapshot.entry.enabled)
                {
                    return Ok(inventory);
                }
            } else {
                return Ok(inventory);
            }
        }
        let mutation = self
            .repository
            .begin_projection_mutation(
                managed_mcp_id,
                expected_revision,
                enabled,
                self.clock.now_ms(),
            )
            .await?;
        let sink_result = match projection.as_ref() {
            Some(projection) => match self.ports.projection_sink.get(&projection.link_key).await? {
                Some(_) => self
                    .ports
                    .projection_sink
                    .set_enabled(&projection.link_key, enabled)
                    .await
                    .map(|_| ()),
                None if !enabled => Ok(()),
                None => Err(integrity_error()),
            },
            None if !enabled => Ok(()),
            None => Err(integrity_error()),
        };
        if let Err(error) = sink_result {
            let _ = self.recover_projection_mutations().await;
            self.notify();
            return Err(error);
        }
        if mutation.status == ProjectionMutationStatus::Started {
            self.repository
                .mark_projection_config_committed(mutation.mutation_id, self.clock.now_ms())
                .await?;
        }
        self.repository
            .complete_projection_mutation(mutation.mutation_id, self.clock.now_ms())
            .await
            .map_err(|_| {
                McpPlatformError::new(
                    McpPlatformErrorCode::RollbackIncomplete,
                    "managed MCP projection mutation requires recovery",
                )
            })?;
        self.repository.get_managed_inventory(managed_mcp_id).await
    }

    pub async fn run(self: Arc<Self>) {
        if self.recover_startup().await.is_err() {
            return;
        }
        loop {
            if self.shutdown.is_cancelled() {
                break;
            }
            if let Ok(true) = self.tick().await {
                continue;
            }
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                _ = self.wake.notified() => {},
                _ = tokio::time::sleep(Duration::from_millis(500)) => {},
            }
        }
    }

    async fn heartbeat(
        &self,
        task_id: String,
        cancellation: CancellationToken,
        stop: CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = cancellation.cancelled() => break,
                _ = self.shutdown.cancelled() => {
                    cancellation.cancel();
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    if self.repository.renew_task_lease(
                        &task_id,
                        &self.owner_id,
                        self.clock.now_ms(),
                        LEASE_DURATION_MS,
                    ).await.is_err() {
                        cancellation.cancel();
                        break;
                    }
                }
            }
        }
    }

    pub async fn recover_startup(&self) -> McpPlatformResult<()> {
        self.recover_projection_mutations().await?;
        let now_ms = self.clock.now_ms();
        let recovered = self
            .repository
            .recover_stale_tasks(
                now_ms.saturating_sub(STALE_HEARTBEAT_MS),
                &self.owner_id,
                now_ms,
            )
            .await?;
        for record in recovered {
            let result = async {
                let adapters_compatible = self
                    .repository
                    .list_task_steps(&record.task.task_id)
                    .await?
                    .iter()
                    .all(|step| self.step_adapter_compatible(step));
                let decision = if adapters_compatible {
                    record.decision.clone()
                } else {
                    RecoveryDecision::RequiresManualRecovery
                };
                match decision {
                    RecoveryDecision::ResumeFromStep { .. } => self
                        .transition(
                            &record.task.task_id,
                            TaskStatus::Queued,
                            record.task.progress,
                        )
                        .await
                        .map(|_| ()),
                    RecoveryDecision::RollbackFromStep { .. } => {
                        self.rollback(&record.task.task_id, false, None).await
                    }
                    RecoveryDecision::RequiresManualRecovery => self
                        .transition(
                            &record.task.task_id,
                            TaskStatus::RecoveryRequired,
                            record.task.progress,
                        )
                        .await
                        .map(|_| ()),
                }
            }
            .await;
            if result.is_err() {
                let redacted = RedactedError::new(
                    RedactedErrorCode::Interrupted,
                    "MCP task recovery requires manual reconciliation",
                    std::iter::empty::<&str>(),
                );
                let _ = self
                    .transition_with_error(
                        &record.task.task_id,
                        TaskStatus::RecoveryRequired,
                        record.task.progress,
                        &redacted,
                    )
                    .await;
            }
        }
        Ok(())
    }

    fn step_adapter_compatible(&self, step: &super::repository::TaskStepRecord) -> bool {
        match step.adapter_id.as_str() {
            "connection_registration" => {
                step.adapter_version == self.ports.registration.adapter_version()
            }
            "connection_projection_repository" => step.adapter_version == "1",
            "extension_config_sink" => {
                step.adapter_version == self.ports.projection_sink.adapter_version()
            }
            "core_transport_projection" => {
                step.adapter_version == self.ports.transport.adapter_version()
            }
            "managed_projection_stage" | "managed_config_stage" => step.adapter_version == "1",
            "mcp_health" => step.adapter_version == self.ports.health.adapter_version(),
            "npm" | "python_wheel" | "binary_archive" => self
                .distribution
                .as_ref()
                .is_some_and(|adapter| step.adapter_version == adapter.adapter_version()),
            "managed_inventory_repository" => matches!(step.adapter_version.as_str(), "1" | "2"),
            _ => false,
        }
    }

    async fn recover_projection_mutations(&self) -> McpPlatformResult<()> {
        for mutation in self.repository.list_pending_projection_mutations().await? {
            let result = async {
                let projection = match self
                    .repository
                    .get_connection_projection(&mutation.managed_mcp_id)
                    .await
                {
                    Ok(projection) => Some(projection),
                    Err(error)
                        if !mutation.desired_enabled
                            && error.code() == McpPlatformErrorCode::NotFound =>
                    {
                        None
                    }
                    Err(error) => return Err(error),
                };
                if let Some(projection) = projection {
                    let snapshot = self.ports.projection_sink.get(&projection.link_key).await?;
                    let snapshot = snapshot.ok_or_else(integrity_error)?;
                    let expected = self
                        .ports
                        .transport
                        .extension_config(&projection.projection, &projection.link_key)?;
                    if snapshot.entry.config != expected {
                        return Err(McpPlatformError::new(
                            McpPlatformErrorCode::ProjectionConflict,
                            "projection mutation cannot be reconciled",
                        ));
                    }
                    if snapshot.entry.enabled != mutation.desired_enabled {
                        if snapshot.entry.enabled != mutation.previous_enabled {
                            return Err(McpPlatformError::new(
                                McpPlatformErrorCode::ProjectionConflict,
                                "projection mutation state drifted",
                            ));
                        }
                        self.ports
                            .projection_sink
                            .set_enabled(&projection.link_key, mutation.desired_enabled)
                            .await?;
                    }
                }
                if matches!(
                    mutation.status,
                    ProjectionMutationStatus::Started | ProjectionMutationStatus::RecoveryRequired
                ) {
                    self.repository
                        .mark_projection_config_committed(mutation.mutation_id, self.clock.now_ms())
                        .await?;
                }
                self.repository
                    .complete_projection_mutation(mutation.mutation_id, self.clock.now_ms())
                    .await
            }
            .await;
            if result.is_err() {
                self.repository
                    .mark_projection_mutation_recovery_required(
                        mutation.mutation_id,
                        self.clock.now_ms(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn execute(
        &self,
        task: TaskRecord,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        match task.operation {
            TaskOperation::Register => self.execute_register(task, cancellation).await,
            TaskOperation::Health => self.execute_health(task, cancellation).await,
            TaskOperation::Install | TaskOperation::Update | TaskOperation::Repair => {
                self.execute_managed_install(task, cancellation).await
            }
            TaskOperation::Uninstall => self.execute_managed_uninstall(task, cancellation).await,
        }
    }

    async fn execute_managed_install(
        &self,
        task: TaskRecord,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        let distribution = self
            .distribution
            .as_ref()
            .ok_or_else(adapter_incompatible)?;
        let plan_record = self.repository.get_plan(&task.plan_id).await?;
        let manifest_record = self
            .repository
            .get_manifest(plan_record.plan.manifest_digest())
            .await?;
        let plan = &plan_record.plan;
        plan.verify_integrity()?;
        if manifest_record.verified.digest() != plan.manifest_digest()
            || plan.adapter().version != distribution.adapter_version()
        {
            return Err(adapter_incompatible());
        }
        if !self
            .ports
            .host_integration
            .is_empty(manifest_record.verified.manifest())
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::OperationNotSupported,
                "managed host integration is unavailable",
            ));
        }
        let acquire = plan
            .steps()
            .iter()
            .find_map(|step| match step {
                PlanStep::AcquireManagedDistribution {
                    artifact_url,
                    artifact_digest,
                    expected_size_bytes,
                    platform,
                    arch,
                    ..
                } => Some((
                    artifact_url.clone(),
                    artifact_digest.value.clone(),
                    *expected_size_bytes,
                    format!("{:?}/{:?}", platform, arch).to_ascii_lowercase(),
                )),
                PlanStep::AcquireDockerDistribution { image, digest, .. } => Some((
                    format!("https://{}", image.split('/').next().unwrap_or_default()),
                    digest.value.clone(),
                    None,
                    "docker/immutable".to_string(),
                )),
                PlanStep::AcquireGitDevDistribution {
                    repository,
                    acquisition_digest,
                    ..
                } => Some((
                    repository.clone(),
                    acquisition_digest.clone(),
                    None,
                    "git_dev/exact_commit".to_string(),
                )),
                _ => None,
            })
            .ok_or_else(integrity_error)?;
        let scope = plan_record
            .target
            .installation_scope
            .as_deref()
            .ok_or_else(integrity_error)?;
        let ids = stable_ids(plan.manifest_id(), scope);
        if let Some(target) = plan_record.target.managed_mcp_id.as_deref() {
            if target != ids.managed_mcp_id {
                return Err(integrity_error());
            }
        }
        let mut effect = ManagedInstallEffect {
            task_id: task.task_id.clone(),
            managed_mcp_id: ids.managed_mcp_id.clone(),
            manifest: manifest_record.verified.manifest().clone(),
            source_url: acquire.0.clone(),
            expected_sha256: acquire.1.clone(),
            expected_size_bytes: acquire.2,
            platform_selector: acquire.3.clone(),
            now_ms: task.created_at_ms,
            operation: task.operation,
            expected_tree_digest: None,
            rebuild_uncommitted_version: false,
            external_acquisition: plan.steps().iter().find_map(|step| match step {
                PlanStep::AcquireDockerDistribution {
                    image,
                    digest,
                    mount_plan_digest,
                    ..
                } => Some(ExternalManagedAcquisition::Docker {
                    image: image.clone(),
                    digest: digest.value.clone(),
                    mount_plan_digest: mount_plan_digest.clone(),
                }),
                PlanStep::AcquireGitDevDistribution {
                    repository_origin,
                    repository,
                    commit,
                    subdirectory,
                    underlying_adapter,
                    acquisition_digest,
                } => Some(ExternalManagedAcquisition::GitDev {
                    repository_origin: repository_origin.clone(),
                    repository: repository.clone(),
                    commit: commit.clone(),
                    subdirectory: subdirectory.clone(),
                    underlying_adapter: *underlying_adapter,
                    acquisition_digest: acquisition_digest.clone(),
                }),
                _ => None,
            }),
        };
        let durable_steps = self.repository.list_task_steps(&task.task_id).await?;
        let snapshot_compensation =
            if let Some(step) = durable_steps.iter().find(|step| step.ordinal == 0) {
                step.compensation.clone()
            } else {
                let previous_inventory = match self
                    .repository
                    .get_managed_inventory(&ids.managed_mcp_id)
                    .await
                {
                    Ok(value) => Some(value),
                    Err(error) if error.code() == McpPlatformErrorCode::NotFound => None,
                    Err(error) => return Err(error),
                };
                let previous_projection = match self
                    .repository
                    .get_connection_projection(&ids.managed_mcp_id)
                    .await
                {
                    Ok(value) => Some(value),
                    Err(error) if error.code() == McpPlatformErrorCode::NotFound => None,
                    Err(error) => return Err(error),
                };
                CompensationDescriptor::ManagedLifecycleSnapshot {
                    managed_mcp_id: ids.managed_mcp_id.clone(),
                    previous_version: previous_inventory
                        .as_ref()
                        .and_then(|inventory| inventory.lifecycle.active_version.clone()),
                    previous_state: previous_inventory
                        .as_ref()
                        .map(|inventory| inventory.managed.state.clone()),
                    previous_projection: previous_projection
                        .as_ref()
                        .map(|projection| projection.projection.clone()),
                    previous_projection_digest: previous_projection
                        .as_ref()
                        .map(|projection| projection.projection_digest.clone()),
                    previous_manifest_digest: previous_projection
                        .as_ref()
                        .and_then(|projection| projection.manifest_digest.clone()),
                    previous_plan_id: previous_projection
                        .as_ref()
                        .and_then(|projection| projection.plan_id.clone()),
                    previous_owner_task_id: previous_projection
                        .as_ref()
                        .and_then(|projection| projection.owner_task_id.clone()),
                }
            };
        let (
            previous_version,
            previous_state,
            previous_projection,
            previous_projection_digest,
            previous_manifest_digest,
            previous_plan_id,
            previous_owner_task_id,
        ) = match &snapshot_compensation {
            CompensationDescriptor::ManagedLifecycleSnapshot {
                managed_mcp_id,
                previous_version,
                previous_state,
                previous_projection,
                previous_projection_digest,
                previous_manifest_digest,
                previous_plan_id,
                previous_owner_task_id,
            } if managed_mcp_id == &ids.managed_mcp_id => (
                previous_version.clone(),
                previous_state.clone(),
                previous_projection.clone(),
                previous_projection_digest.clone(),
                previous_manifest_digest.clone(),
                previous_plan_id.clone(),
                previous_owner_task_id.clone(),
            ),
            _ => return Err(integrity_error()),
        };
        let validate = self
            .start_step(
                &task,
                0,
                snapshot_compensation,
                plan.adapter().id.as_str(),
                plan.adapter().version.as_str(),
            )
            .await?;
        if validate != TaskStepStatus::Committed {
            self.commit_step(
                &task.task_id,
                0,
                StepEvidence::PlanRevalidated {
                    manifest_digest: plan.manifest_digest().to_string(),
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        self.transition(&task.task_id, TaskStatus::Verifying, 20)
            .await?;

        let materialize_compensation =
            if let Some(step) = durable_steps.iter().find(|step| step.ordinal == 1) {
                step.compensation.clone()
            } else if task.operation == TaskOperation::Repair {
                if distribution
                    .version_exists(&ids.managed_mcp_id, plan.manifest_version())
                    .await?
                {
                    CompensationDescriptor::RestoreQuarantinedVersion {
                        managed_mcp_id: ids.managed_mcp_id.clone(),
                        version: plan.manifest_version().to_string(),
                        quarantine_token: format!(
                            "{}-{}-{}-repair",
                            ids.managed_mcp_id,
                            plan.manifest_version(),
                            task.task_id
                        ),
                    }
                } else {
                    CompensationDescriptor::RemoveManagedVersion {
                        managed_mcp_id: ids.managed_mcp_id.clone(),
                        version: plan.manifest_version().to_string(),
                        created_by_task: true,
                    }
                }
            } else {
                let version_preexisted = distribution
                    .version_exists(&ids.managed_mcp_id, plan.manifest_version())
                    .await?;
                CompensationDescriptor::RemoveManagedVersion {
                    managed_mcp_id: ids.managed_mcp_id.clone(),
                    version: plan.manifest_version().to_string(),
                    created_by_task: !version_preexisted,
                }
            };
        effect.expected_tree_digest = durable_steps
            .iter()
            .find(|step| step.ordinal == 1)
            .and_then(|step| match step.evidence.as_ref() {
                Some(StepEvidence::ManagedDistributionMaterialized { tree_digest, .. }) => {
                    Some(tree_digest.clone())
                }
                _ => None,
            });
        if effect.expected_tree_digest.is_none() && task.operation != TaskOperation::Repair {
            effect.expected_tree_digest = match self
                .repository
                .get_managed_inventory(&ids.managed_mcp_id)
                .await
            {
                Ok(inventory) => inventory
                    .managed
                    .versions
                    .iter()
                    .find(|version| version.version == plan.manifest_version())
                    .and_then(|version| version.materialized_tree_digest.clone()),
                Err(error) if error.code() == McpPlatformErrorCode::NotFound => None,
                Err(error) => return Err(error),
            };
        }
        effect.rebuild_uncommitted_version = matches!(
            &materialize_compensation,
            CompensationDescriptor::RemoveManagedVersion {
                created_by_task: true,
                ..
            }
        ) || task.operation == TaskOperation::Repair;
        let materialize = self
            .start_step(
                &task,
                1,
                materialize_compensation,
                plan.adapter().id.as_str(),
                distribution.adapter_version(),
            )
            .await?;
        let outcome = if materialize == TaskStepStatus::Committed {
            match distribution.inspect_installed(&effect, &cancellation).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    let _ = self
                        .repository
                        .release_artifact_claim(&acquire.1, &task.task_id, self.clock.now_ms())
                        .await;
                    return Err(error);
                }
            }
        } else {
            let now_ms = self.clock.now_ms();
            self.repository
                .claim_artifact(
                    &acquire.1,
                    &task.task_id,
                    now_ms,
                    now_ms - STALE_ARTIFACT_CLAIM_MS,
                )
                .await?;
            let outcome = match distribution.install(&effect, &cancellation).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.repository
                        .release_artifact_claim(&acquire.1, &task.task_id, self.clock.now_ms())
                        .await?;
                    return Err(error);
                }
            };
            if let Err(error) = self
                .repository
                .mark_artifact_claim_verified(&acquire.1, &task.task_id, self.clock.now_ms())
                .await
            {
                let _ = self
                    .repository
                    .release_artifact_claim(&acquire.1, &task.task_id, self.clock.now_ms())
                    .await;
                return Err(error);
            }
            let commit = self
                .commit_step(
                    &task.task_id,
                    1,
                    StepEvidence::ManagedDistributionMaterialized {
                        digest: outcome.evidence.artifact_digest.clone(),
                        version: plan.manifest_version().to_string(),
                        tree_digest: outcome.materialized_tree_digest.clone(),
                    },
                )
                .await;
            self.repository
                .release_artifact_claim(&acquire.1, &task.task_id, self.clock.now_ms())
                .await?;
            commit?;
            outcome
        };
        if materialize == TaskStepStatus::Committed {
            self.repository
                .release_artifact_claim(&acquire.1, &task.task_id, self.clock.now_ms())
                .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        self.transition(&task.task_id, TaskStatus::Activating, 55)
            .await?;
        let adapter = task.adapter_evidence.as_ref().ok_or_else(integrity_error)?;
        let root = outcome.installation_root.to_string_lossy().into_owned();
        let inventory = self
            .start_step(
                &task,
                2,
                CompensationDescriptor::RestoreManagedActivation {
                    managed_mcp_id: ids.managed_mcp_id.clone(),
                    previous_version: previous_version.clone(),
                    target_version: plan.manifest_version().to_string(),
                },
                "managed_inventory_repository",
                "2",
            )
            .await?;
        if inventory != TaskStepStatus::Committed {
            let staged = self
                .repository
                .stage_managed_installation(StageManagedInstallation {
                    managed_mcp_id: &ids.managed_mcp_id,
                    mcp_id: plan.manifest_id(),
                    installation_scope: scope,
                    distribution_adapter: &plan.adapter().id,
                    manifest_digest: plan.manifest_digest(),
                    version: plan.manifest_version(),
                    installation_root: &root,
                    task_id: &task.task_id,
                    adapter_evidence: adapter,
                    verification_evidence: &outcome.evidence,
                    materialized_tree_digest: &outcome.materialized_tree_digest,
                    supply_chain_evidence: outcome.supply_chain_evidence.as_ref(),
                    owned_relative_paths: &outcome.owned_relative_paths,
                    now_ms: self.clock.now_ms(),
                })
                .await?;
            if staged.previous_version != previous_version {
                return Err(integrity_error());
            }
            self.commit_step(
                &task.task_id,
                2,
                StepEvidence::ManagedMcpUpserted {
                    managed_mcp_id: ids.managed_mcp_id.clone(),
                    created: staged.created_managed_mcp,
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        let extension_config = self
            .ports
            .transport
            .extension_config(&outcome.projection, &ids.link_key)?;
        let projection_digest = digest_serializable(&extension_config)?;
        let projection_compensation = previous_projection.as_ref().map_or_else(
            || CompensationDescriptor::RemoveOwnedConnectionProjection {
                managed_mcp_id: ids.managed_mcp_id.clone(),
                link_key: ids.link_key.clone(),
            },
            |previous| CompensationDescriptor::RestoreManagedProjection {
                managed_mcp_id: ids.managed_mcp_id.clone(),
                link_key: ids.link_key.clone(),
                projection: previous.clone(),
                enabled: previous_state
                    .as_ref()
                    .is_some_and(|state| state.default_enabled),
                projection_digest: previous_projection_digest.clone().unwrap_or_default(),
                manifest_digest: previous_manifest_digest.clone().unwrap_or_default(),
                plan_id: previous_plan_id.clone().unwrap_or_default(),
                owner_task_id: previous_owner_task_id.clone(),
            },
        );
        let projection_step = self
            .start_step(
                &task,
                3,
                CompensationDescriptor::NoCompensation,
                "managed_projection_stage",
                "1",
            )
            .await?;
        if projection_step != TaskStepStatus::Committed {
            self.commit_step(
                &task.task_id,
                3,
                StepEvidence::ProjectionVerified {
                    link_key: ids.link_key.clone(),
                    enabled: false,
                },
            )
            .await?;
        }
        let config_compensation = previous_projection.as_ref().map_or_else(
            || CompensationDescriptor::RemoveOwnedExtensionConfig {
                link_key: ids.link_key.clone(),
                projection_digest: projection_digest.clone(),
                created_by_task: true,
            },
            |_| projection_compensation.clone(),
        );
        let config_step = self
            .start_step(
                &task,
                4,
                CompensationDescriptor::NoCompensation,
                "managed_config_stage",
                "1",
            )
            .await?;
        if config_step != TaskStepStatus::Committed {
            self.commit_step(
                &task.task_id,
                4,
                StepEvidence::ExtensionConfigProjected {
                    link_key: ids.link_key.clone(),
                    projection_digest: projection_digest.clone(),
                    enabled: false,
                    created: false,
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        let health_step = self
            .start_step(
                &task,
                5,
                CompensationDescriptor::NoCompensation,
                self.ports.health.adapter_id(),
                self.ports.health.adapter_version(),
            )
            .await?;
        if health_step != TaskStepStatus::Committed {
            let effect = registration_effect_from_projection(&outcome.projection)?;
            self.ports
                .registration
                .verify(&effect, &cancellation)
                .await?;
            let result = match self
                .ports
                .auth
                .requirement(&manifest_record.verified.manifest().auth)?
            {
                AuthRequirement::MissingOpaqueHandle { .. } => {
                    super::lifecycle::HealthAdapterResult {
                        result_code: HealthResultCode::BlockedAuth,
                        latency_ms: 0,
                        capabilities_digest: None,
                        tools_digest: None,
                        detail_code: HealthDetailCode::CredentialHandleMissing,
                    }
                }
                AuthRequirement::Ready => {
                    self.ports
                        .health
                        .run(
                            HealthExecution {
                                effect,
                                check: manifest_record.verified.manifest().health_check.clone(),
                                projection_config: extension_config.clone(),
                            },
                            cancellation.clone(),
                        )
                        .await?
                }
            };
            self.repository
                .append_health_observation(NewHealthObservation {
                    managed_mcp_id: &ids.managed_mcp_id,
                    task_id: &task.task_id,
                    check_type: HealthCheckMode::Runtime.as_str(),
                    result_code: result.result_code,
                    latency_ms: result.latency_ms,
                    capabilities_digest: result.capabilities_digest.as_deref(),
                    tools_digest: result.tools_digest.as_deref(),
                    checked_at_ms: self.clock.now_ms(),
                    detail_code: result.detail_code,
                })
                .await?;
            self.commit_step(
                &task.task_id,
                5,
                StepEvidence::HealthObserved {
                    result_code: result.result_code.as_str().to_string(),
                },
            )
            .await?;
            if !matches!(
                result.result_code,
                HealthResultCode::Healthy | HealthResultCode::BlockedAuth
            ) {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::HealthFailed,
                    "managed MCP runtime health gate failed",
                ));
            }
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        let observation = self
            .repository
            .latest_health_observation(&ids.managed_mcp_id)
            .await?
            .filter(|observation| observation.task_id == task.task_id)
            .ok_or_else(integrity_error)?;
        if !matches!(
            observation.result_code,
            HealthResultCode::Healthy | HealthResultCode::BlockedAuth
        ) {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::HealthFailed,
                "managed MCP runtime health gate failed",
            ));
        }
        let runtime_activate = self
            .start_step(
                &task,
                6,
                CompensationDescriptor::RestoreRuntimeActivation {
                    managed_mcp_id: ids.managed_mcp_id.clone(),
                    previous_version: previous_version.clone(),
                    target_version: Some(plan.manifest_version().to_string()),
                },
                plan.adapter().id.as_str(),
                distribution.adapter_version(),
            )
            .await?;
        if runtime_activate != TaskStepStatus::Committed {
            distribution
                .activate_version(
                    &ids.managed_mcp_id,
                    plan.manifest_version(),
                    &task.task_id,
                    &cancellation,
                )
                .await?;
            if distribution
                .active_version(&ids.managed_mcp_id)
                .await?
                .as_deref()
                != Some(plan.manifest_version())
            {
                return Err(integrity_error());
            }
            self.repository
                .mark_managed_runtime_activated(
                    &ids.managed_mcp_id,
                    plan.manifest_version(),
                    &task.task_id,
                    self.clock.now_ms(),
                )
                .await?;
            self.commit_step(
                &task.task_id,
                6,
                StepEvidence::ActivationRecorded {
                    version: plan.manifest_version().to_string(),
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        let desired_enabled = previous_state
            .as_ref()
            .is_some_and(|state| state.default_enabled)
            && observation.result_code == HealthResultCode::Healthy;
        let activate = self
            .start_step(
                &task,
                7,
                CompensationDescriptor::RestoreManagedActivation {
                    managed_mcp_id: ids.managed_mcp_id.clone(),
                    previous_version: previous_version.clone(),
                    target_version: plan.manifest_version().to_string(),
                },
                "managed_inventory_repository",
                "2",
            )
            .await?;
        if activate != TaskStepStatus::Committed {
            self.repository
                .activate_managed_installation(ActivateManagedInstallation {
                    managed_mcp_id: &ids.managed_mcp_id,
                    target_version: plan.manifest_version(),
                    task_id: &task.task_id,
                    default_enabled: desired_enabled,
                    now_ms: self.clock.now_ms(),
                })
                .await?;
            self.commit_step(
                &task.task_id,
                7,
                StepEvidence::ActivationRecorded {
                    version: plan.manifest_version().to_string(),
                },
            )
            .await?;
        }
        let live_projection = self
            .start_step(
                &task,
                8,
                projection_compensation.clone(),
                "connection_projection_repository",
                "1",
            )
            .await?;
        if live_projection != TaskStepStatus::Committed {
            let projection = self
                .repository
                .put_owned_connection_projection(PutOwnedProjection {
                    managed_mcp_id: &ids.managed_mcp_id,
                    link_key: &ids.link_key,
                    projection: &outcome.projection,
                    plan_id: &task.plan_id,
                    manifest_digest: plan.manifest_digest(),
                    owner_task_id: &task.task_id,
                    projection_digest: &projection_digest,
                    now_ms: self.clock.now_ms(),
                })
                .await?;
            self.commit_step(
                &task.task_id,
                8,
                StepEvidence::ProjectionPersisted {
                    managed_mcp_id: ids.managed_mcp_id.clone(),
                    link_key: ids.link_key.clone(),
                    revision: projection.revision,
                    created: projection.owner_task_id.as_deref() == Some(&task.task_id),
                },
            )
            .await?;
        }
        let live_config = self
            .start_step(
                &task,
                9,
                config_compensation,
                self.ports.projection_sink.adapter_id(),
                self.ports.projection_sink.adapter_version(),
            )
            .await?;
        if live_config != TaskStepStatus::Committed {
            let target_entry = crate::config::extensions::ExtensionEntry {
                enabled: desired_enabled,
                config: extension_config.clone(),
            };
            let snapshot = match self.ports.projection_sink.get(&ids.link_key).await? {
                Some(existing) if existing.entry == target_entry => existing,
                Some(existing)
                    if previous_projection.as_ref().is_some_and(|projection| {
                        self.ports
                            .transport
                            .extension_config(projection, &ids.link_key)
                            .ok()
                            .is_some_and(|config| {
                                existing.entry
                                    == crate::config::extensions::ExtensionEntry {
                                        enabled: previous_state
                                            .as_ref()
                                            .is_some_and(|state| state.default_enabled),
                                        config,
                                    }
                            })
                    }) =>
                {
                    self.ports
                        .projection_sink
                        .replace_owned(
                            &ids.link_key,
                            &existing,
                            extension_config.clone(),
                            desired_enabled,
                        )
                        .await?
                }
                Some(_) => {
                    return Err(McpPlatformError::new(
                        McpPlatformErrorCode::ProjectionConflict,
                        "managed live projection drifted during activation",
                    ))
                }
                None if !desired_enabled => {
                    self.ports
                        .projection_sink
                        .put_disabled(&ids.link_key, extension_config.clone())
                        .await?
                }
                None => return Err(integrity_error()),
            };
            if snapshot.entry.enabled != desired_enabled {
                return Err(integrity_error());
            }
            self.commit_step(
                &task.task_id,
                9,
                StepEvidence::ExtensionConfigProjected {
                    link_key: ids.link_key.clone(),
                    projection_digest: projection_digest.clone(),
                    enabled: desired_enabled,
                    created: snapshot.created,
                },
            )
            .await?;
        }
        if task.operation == TaskOperation::Update {
            if let Some(previous) = previous_version
                .as_deref()
                .filter(|previous| *previous != plan.manifest_version())
            {
                let token = format!(
                    "{}-{previous}-{}-retained",
                    ids.managed_mcp_id, task.task_id
                );
                let quarantine = self
                    .start_step(
                        &task,
                        10,
                        CompensationDescriptor::RestoreQuarantinedVersion {
                            managed_mcp_id: ids.managed_mcp_id.clone(),
                            version: previous.to_string(),
                            quarantine_token: token.clone(),
                        },
                        plan.adapter().id.as_str(),
                        distribution.adapter_version(),
                    )
                    .await?;
                if quarantine != TaskStepStatus::Committed {
                    let actual = distribution
                        .quarantine_version(
                            &ids.managed_mcp_id,
                            previous,
                            &format!("{}-retained", task.task_id),
                            &cancellation,
                        )
                        .await?;
                    if actual != token {
                        return Err(integrity_error());
                    }
                    self.commit_step(
                        &task.task_id,
                        10,
                        StepEvidence::ActivationRecorded {
                            version: previous.to_string(),
                        },
                    )
                    .await?;
                }
                let finalize = self
                    .start_step(
                        &task,
                        11,
                        CompensationDescriptor::FinalizedManagedUninstall {
                            managed_mcp_id: ids.managed_mcp_id.clone(),
                            version: previous.to_string(),
                        },
                        "managed_inventory_repository",
                        "2",
                    )
                    .await?;
                if finalize != TaskStepStatus::Committed {
                    self.repository
                        .finalize_retained_version_cleanup(
                            &ids.managed_mcp_id,
                            previous,
                            &task.task_id,
                            self.clock.now_ms(),
                        )
                        .await?;
                    self.commit_step(
                        &task.task_id,
                        11,
                        StepEvidence::ActivationRecorded {
                            version: previous.to_string(),
                        },
                    )
                    .await?;
                }
                let purge = self
                    .start_step(
                        &task,
                        12,
                        CompensationDescriptor::FinalizedManagedUninstall {
                            managed_mcp_id: ids.managed_mcp_id.clone(),
                            version: previous.to_string(),
                        },
                        plan.adapter().id.as_str(),
                        distribution.adapter_version(),
                    )
                    .await?;
                if purge != TaskStepStatus::Committed {
                    distribution
                        .purge_quarantined(&token, &cancellation)
                        .await?;
                    self.commit_step(
                        &task.task_id,
                        12,
                        StepEvidence::ActivationRecorded {
                            version: previous.to_string(),
                        },
                    )
                    .await?;
                }
            }
        }
        if let Some(token) = outcome.replaced_quarantine_token.as_deref() {
            let purge = self
                .start_step(
                    &task,
                    10,
                    CompensationDescriptor::FinalizedManagedUninstall {
                        managed_mcp_id: ids.managed_mcp_id.clone(),
                        version: plan.manifest_version().to_string(),
                    },
                    plan.adapter().id.as_str(),
                    distribution.adapter_version(),
                )
                .await?;
            if purge != TaskStepStatus::Committed {
                distribution.purge_quarantined(token, &cancellation).await?;
                self.commit_step(
                    &task.task_id,
                    10,
                    StepEvidence::ActivationRecorded {
                        version: plan.manifest_version().to_string(),
                    },
                )
                .await?;
            }
        }
        self.transition(&task.task_id, TaskStatus::Succeeded, 100)
            .await?;
        Ok(())
    }

    async fn execute_managed_uninstall(
        &self,
        task: TaskRecord,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        let distribution = self
            .distribution
            .as_ref()
            .ok_or_else(adapter_incompatible)?;
        let record = self.repository.get_plan(&task.plan_id).await?;
        record.plan.verify_integrity()?;
        let (managed_mcp_id, version) = record
            .plan
            .steps()
            .iter()
            .find_map(|step| match step {
                PlanStep::RemoveManagedInstallation {
                    managed_mcp_id,
                    version,
                    ownership_only: true,
                    ..
                } => Some((managed_mcp_id.as_str(), version.as_str())),
                _ => None,
            })
            .ok_or_else(integrity_error)?;
        let durable_steps = self.repository.list_task_steps(&task.task_id).await?;
        let uninstall_snapshot =
            if let Some(step) = durable_steps.iter().find(|step| step.ordinal == 0) {
                step.compensation.clone()
            } else {
                let inventory = self
                    .repository
                    .get_managed_inventory(managed_mcp_id)
                    .await?;
                let projection = self
                    .repository
                    .get_connection_projection(managed_mcp_id)
                    .await?;
                CompensationDescriptor::ManagedUninstallSnapshot {
                    managed_mcp_id: managed_mcp_id.to_string(),
                    active_version: version.to_string(),
                    versions: inventory
                        .managed
                        .versions
                        .iter()
                        .map(|version| version.version.clone())
                        .collect(),
                    link_key: projection.link_key.clone(),
                    projection: projection.projection,
                    enabled: inventory.managed.state.default_enabled,
                    projection_digest: projection.projection_digest,
                    manifest_digest: projection.manifest_digest.unwrap_or_default(),
                    plan_id: projection.plan_id.unwrap_or_default(),
                    owner_task_id: projection.owner_task_id,
                    task_id: task.task_id.clone(),
                }
            };
        let (
            versions,
            link_key,
            projection,
            enabled,
            projection_digest,
            manifest_digest,
            previous_plan_id,
            owner_task_id,
        ) = match &uninstall_snapshot {
            CompensationDescriptor::ManagedUninstallSnapshot {
                managed_mcp_id: snapshot_id,
                active_version,
                versions,
                link_key,
                projection,
                enabled,
                projection_digest,
                manifest_digest,
                plan_id,
                owner_task_id,
                task_id,
            } if snapshot_id == managed_mcp_id
                && active_version == version
                && task_id == &task.task_id =>
            {
                (
                    versions.clone(),
                    link_key.clone(),
                    projection.clone(),
                    *enabled,
                    projection_digest.clone(),
                    manifest_digest.clone(),
                    plan_id.clone(),
                    owner_task_id.clone(),
                )
            }
            _ => return Err(integrity_error()),
        };
        if versions.is_empty() || !versions.iter().any(|candidate| candidate == version) {
            return Err(integrity_error());
        }
        let config = self
            .ports
            .transport
            .extension_config(&projection, &link_key)?;
        let restore_projection = CompensationDescriptor::RestoreManagedProjection {
            managed_mcp_id: managed_mcp_id.to_string(),
            link_key: link_key.clone(),
            projection: projection.clone(),
            enabled,
            projection_digest,
            manifest_digest,
            plan_id: previous_plan_id,
            owner_task_id,
        };
        let begin = self
            .start_step(
                &task,
                0,
                uninstall_snapshot,
                "managed_inventory_repository",
                "2",
            )
            .await?;
        if begin != TaskStepStatus::Committed {
            self.repository
                .begin_managed_uninstall(
                    managed_mcp_id,
                    version,
                    &task.task_id,
                    self.clock.now_ms(),
                )
                .await?;
            self.commit_step(
                &task.task_id,
                0,
                StepEvidence::PlanRevalidated {
                    manifest_digest: record.plan.manifest_digest().to_string(),
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        let detach = self
            .start_step(
                &task,
                1,
                restore_projection,
                self.ports.projection_sink.adapter_id(),
                self.ports.projection_sink.adapter_version(),
            )
            .await?;
        if detach != TaskStepStatus::Committed {
            if let Some(current) = self.ports.projection_sink.get(&link_key).await? {
                if current.entry.config != config || (current.entry.enabled && !enabled) {
                    return Err(McpPlatformError::new(
                        McpPlatformErrorCode::ProjectionConflict,
                        "managed uninstall projection drifted",
                    ));
                }
                self.ports
                    .projection_sink
                    .remove_owned(&link_key, &current)
                    .await?;
            }
            self.commit_step(
                &task.task_id,
                1,
                StepEvidence::ProjectionVerified {
                    link_key: link_key.clone(),
                    enabled: false,
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        self.transition(&task.task_id, TaskStatus::Verifying, 50)
            .await?;
        let deactivate = self
            .start_step(
                &task,
                2,
                CompensationDescriptor::RestoreRuntimeActivation {
                    managed_mcp_id: managed_mcp_id.to_string(),
                    previous_version: Some(version.to_string()),
                    target_version: None,
                },
                record.plan.adapter().id.as_str(),
                distribution.adapter_version(),
            )
            .await?;
        if deactivate != TaskStepStatus::Committed {
            distribution
                .restore_activation(managed_mcp_id, None, &task.task_id)
                .await?;
            if distribution.active_version(managed_mcp_id).await?.is_some() {
                return Err(integrity_error());
            }
            self.commit_step(
                &task.task_id,
                2,
                StepEvidence::ActivationRecorded {
                    version: version.to_string(),
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        let mut quarantine_tokens = Vec::with_capacity(versions.len());
        for (index, owned_version) in versions.iter().enumerate() {
            let token_task = format!("{}-uninstall-{index}", task.task_id);
            let token = format!("{managed_mcp_id}-{owned_version}-{token_task}");
            let ordinal = 3 + i64::try_from(index).map_err(|_| integrity_error())?;
            let quarantine = self
                .start_step(
                    &task,
                    ordinal,
                    CompensationDescriptor::RestoreQuarantinedVersion {
                        managed_mcp_id: managed_mcp_id.to_string(),
                        version: owned_version.clone(),
                        quarantine_token: token.clone(),
                    },
                    record.plan.adapter().id.as_str(),
                    distribution.adapter_version(),
                )
                .await?;
            if quarantine != TaskStepStatus::Committed {
                let actual = distribution
                    .quarantine_version(managed_mcp_id, owned_version, &token_task, &cancellation)
                    .await?;
                if actual != token {
                    return Err(integrity_error());
                }
                self.commit_step(
                    &task.task_id,
                    ordinal,
                    StepEvidence::ActivationRecorded {
                        version: owned_version.clone(),
                    },
                )
                .await?;
            }
            quarantine_tokens.push(token);
        }
        self.repository
            .mark_managed_uninstall_quarantined(&task.task_id, self.clock.now_ms())
            .await?;
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        self.transition(&task.task_id, TaskStatus::Activating, 75)
            .await?;
        let finalize_ordinal = 3 + i64::try_from(versions.len()).map_err(|_| integrity_error())?;
        let finalize = self
            .start_step(
                &task,
                finalize_ordinal,
                CompensationDescriptor::FinalizedManagedUninstall {
                    managed_mcp_id: managed_mcp_id.to_string(),
                    version: version.to_string(),
                },
                "managed_inventory_repository",
                "2",
            )
            .await?;
        if finalize != TaskStepStatus::Committed {
            self.repository
                .finalize_managed_uninstall(
                    managed_mcp_id,
                    version,
                    &task.task_id,
                    self.clock.now_ms(),
                )
                .await?;
            self.commit_step(
                &task.task_id,
                finalize_ordinal,
                StepEvidence::ActivationRecorded {
                    version: version.to_string(),
                },
            )
            .await?;
        }
        for (index, token) in quarantine_tokens.iter().enumerate() {
            let ordinal =
                finalize_ordinal + 1 + i64::try_from(index).map_err(|_| integrity_error())?;
            let purge = self
                .start_step(
                    &task,
                    ordinal,
                    CompensationDescriptor::FinalizedManagedUninstall {
                        managed_mcp_id: managed_mcp_id.to_string(),
                        version: version.to_string(),
                    },
                    record.plan.adapter().id.as_str(),
                    distribution.adapter_version(),
                )
                .await?;
            if purge != TaskStepStatus::Committed {
                distribution.purge_quarantined(token, &cancellation).await?;
                self.commit_step(
                    &task.task_id,
                    ordinal,
                    StepEvidence::ActivationRecorded {
                        version: version.to_string(),
                    },
                )
                .await?;
            }
        }
        self.transition(&task.task_id, TaskStatus::Succeeded, 100)
            .await?;
        Ok(())
    }

    async fn execute_register(
        &self,
        task: TaskRecord,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        let plan_record = self.repository.get_plan(&task.plan_id).await?;
        let manifest_record = self
            .repository
            .get_manifest(plan_record.plan.manifest_digest())
            .await?;
        let plan = &plan_record.plan;
        let manifest = manifest_record.verified.manifest();
        let adapter = task.adapter_evidence.as_ref().ok_or_else(integrity_error)?;
        if adapter.adapter_id != plan.adapter().id
            || adapter.adapter_version != plan.adapter().version
            || !adapter.compatible_for_recovery
            || adapter.adapter_version != "1"
        {
            return Err(adapter_incompatible());
        }
        if !self.ports.host_integration.is_empty(manifest) {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::OperationNotSupported,
                "host integration is unavailable in phase 3A",
            ));
        }
        let effect = self.ports.transport.registration_effect(manifest, plan)?;
        let stable_ids = stable_ids(
            plan.manifest_id(),
            plan_record
                .target
                .installation_scope
                .as_deref()
                .ok_or_else(integrity_error)?,
        );
        let extension_config = self
            .ports
            .transport
            .extension_config(plan.connection_projection(), &stable_ids.link_key)?;
        let projection_digest = digest_serializable(&extension_config)?;

        if self.cancel_requested(&task.task_id, &cancellation).await? {
            return self.cancel_without_effects(&task.task_id).await;
        }
        if self
            .start_step(
                &task,
                0,
                CompensationDescriptor::RemoveManagedMcp {
                    managed_mcp_id: stable_ids.managed_mcp_id.clone(),
                },
                self.ports.registration.adapter_id(),
                self.ports.registration.adapter_version(),
            )
            .await?
            != TaskStepStatus::Committed
        {
            plan.verify_integrity()?;
            if manifest_record.verified.digest() != plan.manifest_digest() {
                return Err(integrity_error());
            }
            let effect_evidence = self
                .ports
                .registration
                .verify(&effect, &cancellation)
                .await?;
            if effect_evidence.adapter_id != self.ports.registration.adapter_id()
                || effect_evidence.adapter_version != self.ports.registration.adapter_version()
            {
                return Err(adapter_incompatible());
            }
            self.commit_step(
                &task.task_id,
                0,
                StepEvidence::PlanRevalidated {
                    manifest_digest: plan.manifest_digest().to_string(),
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        self.transition(&task.task_id, TaskStatus::Verifying, 20)
            .await?;

        let inventory_status = self
            .start_step(
                &task,
                1,
                CompensationDescriptor::RemoveManagedMcp {
                    managed_mcp_id: stable_ids.managed_mcp_id.clone(),
                },
                "managed_inventory_repository",
                "1",
            )
            .await?;
        if inventory_status != TaskStepStatus::Committed {
            let outcome = self
                .repository
                .register_managed_mcp(RegisterManagedMcp {
                    managed_mcp_id: &stable_ids.managed_mcp_id,
                    mcp_id: plan.manifest_id(),
                    installation_scope: &stable_ids.installation_scope,
                    distribution_adapter: &plan.adapter().id,
                    manifest_digest: plan.manifest_digest(),
                    version: plan.manifest_version(),
                    task_id: &task.task_id,
                    adapter_evidence: adapter,
                    now_ms: self.clock.now_ms(),
                })
                .await?;
            self.commit_step(
                &task.task_id,
                1,
                StepEvidence::ManagedMcpUpserted {
                    managed_mcp_id: stable_ids.managed_mcp_id.clone(),
                    created: outcome.created,
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;

        let projection_status = self
            .start_step(
                &task,
                2,
                CompensationDescriptor::RemoveOwnedConnectionProjection {
                    managed_mcp_id: stable_ids.managed_mcp_id.clone(),
                    link_key: stable_ids.link_key.clone(),
                },
                "connection_projection_repository",
                "1",
            )
            .await?;
        if projection_status != TaskStepStatus::Committed {
            let projection = self
                .repository
                .put_owned_connection_projection(PutOwnedProjection {
                    managed_mcp_id: &stable_ids.managed_mcp_id,
                    link_key: &stable_ids.link_key,
                    projection: plan.connection_projection(),
                    plan_id: &task.plan_id,
                    manifest_digest: plan.manifest_digest(),
                    owner_task_id: &task.task_id,
                    projection_digest: &projection_digest,
                    now_ms: self.clock.now_ms(),
                })
                .await?;
            self.commit_step(
                &task.task_id,
                2,
                StepEvidence::ProjectionPersisted {
                    managed_mcp_id: stable_ids.managed_mcp_id.clone(),
                    link_key: stable_ids.link_key.clone(),
                    revision: projection.revision,
                    created: projection.owner_task_id.as_deref() == Some(&task.task_id),
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        self.transition(&task.task_id, TaskStatus::Activating, 60)
            .await?;

        let existing_config_step = self
            .repository
            .list_task_steps(&task.task_id)
            .await?
            .into_iter()
            .find(|step| step.ordinal == 3);
        let config_created_by_task =
            match existing_config_step.as_ref().map(|step| &step.compensation) {
                Some(CompensationDescriptor::RemoveOwnedExtensionConfig {
                    created_by_task,
                    ..
                }) => *created_by_task,
                Some(_) => return Err(integrity_error()),
                None => self
                    .ports
                    .projection_sink
                    .get(&stable_ids.link_key)
                    .await?
                    .is_none(),
            };
        let config_status = self
            .start_step(
                &task,
                3,
                CompensationDescriptor::RemoveOwnedExtensionConfig {
                    link_key: stable_ids.link_key.clone(),
                    projection_digest: projection_digest.clone(),
                    created_by_task: config_created_by_task,
                },
                self.ports.projection_sink.adapter_id(),
                self.ports.projection_sink.adapter_version(),
            )
            .await?;
        if config_status != TaskStepStatus::Committed {
            let snapshot = self
                .ports
                .projection_sink
                .put_disabled(&stable_ids.link_key, extension_config.clone())
                .await?;
            if snapshot.entry.enabled {
                return Err(integrity_error());
            }
            self.commit_step(
                &task.task_id,
                3,
                StepEvidence::ExtensionConfigProjected {
                    link_key: stable_ids.link_key.clone(),
                    projection_digest: projection_digest.clone(),
                    enabled: false,
                    created: config_created_by_task,
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;

        let verify_status = self
            .start_step(
                &task,
                4,
                CompensationDescriptor::RemoveOwnedExtensionConfig {
                    link_key: stable_ids.link_key.clone(),
                    projection_digest,
                    created_by_task: false,
                },
                self.ports.transport.adapter_id(),
                self.ports.transport.adapter_version(),
            )
            .await?;
        if verify_status != TaskStepStatus::Committed {
            let snapshot = self
                .ports
                .projection_sink
                .get(&stable_ids.link_key)
                .await?
                .ok_or_else(integrity_error)?;
            if snapshot.entry.enabled || snapshot.entry.config != extension_config {
                return Err(integrity_error());
            }
            self.repository
                .get_connection_projection(&stable_ids.managed_mcp_id)
                .await?;
            self.commit_step(
                &task.task_id,
                4,
                StepEvidence::ProjectionVerified {
                    link_key: stable_ids.link_key,
                    enabled: false,
                },
            )
            .await?;
        }
        self.transition(&task.task_id, TaskStatus::Succeeded, 100)
            .await?;
        Ok(())
    }

    async fn execute_health(
        &self,
        task: TaskRecord,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        let request = self
            .repository
            .get_health_task_request(&task.task_id)
            .await?;
        let inventory = self
            .repository
            .get_managed_inventory(&request.managed_mcp_id)
            .await?;
        let manifest_digest = inventory
            .lifecycle
            .active_manifest_digest
            .as_deref()
            .ok_or_else(integrity_error)?;
        let manifest = self.repository.get_manifest(manifest_digest).await?;
        let projection = self
            .repository
            .get_connection_projection(&request.managed_mcp_id)
            .await?;
        let config = self
            .ports
            .transport
            .extension_config(&projection.projection, &projection.link_key)?;
        let effect = self.ports.transport.registration_effect(
            manifest.verified.manifest(),
            &self.repository.get_plan(&task.plan_id).await?.plan,
        )?;
        let compensation = CompensationDescriptor::RemoveManagedMcp {
            managed_mcp_id: request.managed_mcp_id.clone(),
        };
        if self
            .start_step(
                &task,
                0,
                compensation,
                self.ports.health.adapter_id(),
                self.ports.health.adapter_version(),
            )
            .await?
            != TaskStepStatus::Committed
        {
            let result = match self
                .ports
                .auth
                .requirement(&manifest.verified.manifest().auth)?
            {
                AuthRequirement::MissingOpaqueHandle { .. } => {
                    super::lifecycle::HealthAdapterResult {
                        result_code: HealthResultCode::BlockedAuth,
                        latency_ms: 0,
                        capabilities_digest: None,
                        tools_digest: None,
                        detail_code: HealthDetailCode::CredentialHandleMissing,
                    }
                }
                AuthRequirement::Ready if request.mode == HealthCheckMode::Registration => {
                    registration_health(
                        self.ports.projection_sink.as_ref(),
                        &projection.link_key,
                        &config,
                        inventory.managed.state.default_enabled,
                    )
                    .await?
                }
                AuthRequirement::Ready => {
                    self.ports
                        .health
                        .run(
                            HealthExecution {
                                effect,
                                check: manifest.verified.manifest().health_check.clone(),
                                projection_config: config,
                            },
                            cancellation.clone(),
                        )
                        .await?
                }
            };
            if cancellation.is_cancelled() {
                return self.cancel_without_effects(&task.task_id).await;
            }
            self.repository
                .renew_task_lease(
                    &task.task_id,
                    &self.owner_id,
                    self.clock.now_ms(),
                    LEASE_DURATION_MS,
                )
                .await?;
            self.repository
                .append_health_observation(NewHealthObservation {
                    managed_mcp_id: &request.managed_mcp_id,
                    task_id: &task.task_id,
                    check_type: request.mode.as_str(),
                    result_code: result.result_code,
                    latency_ms: result.latency_ms,
                    capabilities_digest: result.capabilities_digest.as_deref(),
                    tools_digest: result.tools_digest.as_deref(),
                    checked_at_ms: self.clock.now_ms(),
                    detail_code: result.detail_code,
                })
                .await?;
            self.commit_step(
                &task.task_id,
                0,
                StepEvidence::HealthObserved {
                    result_code: result.result_code.as_str().to_string(),
                },
            )
            .await?;
        }
        self.transition(&task.task_id, TaskStatus::Verifying, 70)
            .await?;
        self.transition(&task.task_id, TaskStatus::Activating, 90)
            .await?;
        self.transition(&task.task_id, TaskStatus::Succeeded, 100)
            .await?;
        Ok(())
    }

    async fn start_step(
        &self,
        task: &TaskRecord,
        ordinal: i64,
        compensation: CompensationDescriptor,
        adapter_id: &str,
        adapter_version: &str,
    ) -> McpPlatformResult<TaskStepStatus> {
        if let Some(step) = self
            .repository
            .list_task_steps(&task.task_id)
            .await?
            .into_iter()
            .find(|step| step.ordinal == ordinal)
        {
            return existing_step_status(&step, &compensation, adapter_id, adapter_version);
        }
        let token = format!("{}:{}:{}", task.task_id, task.attempt_count, ordinal);
        let step = self
            .repository
            .add_task_step(
                &task.task_id,
                ordinal,
                &token,
                &compensation,
                adapter_id,
                adapter_version,
            )
            .await?;
        if step.status == TaskStepStatus::NotStarted {
            return self
                .repository
                .transition_task_step(StepTransition {
                    task_id: &task.task_id,
                    ordinal,
                    expected_status: TaskStepStatus::NotStarted,
                    next_status: TaskStepStatus::Started,
                    evidence: None,
                    actor: &self.owner_id,
                    now_ms: self.clock.now_ms(),
                })
                .await
                .map(|step| step.status);
        }
        Ok(step.status)
    }

    async fn commit_step(
        &self,
        task_id: &str,
        ordinal: i64,
        evidence: StepEvidence,
    ) -> McpPlatformResult<()> {
        self.repository
            .renew_task_lease(
                task_id,
                &self.owner_id,
                self.clock.now_ms(),
                LEASE_DURATION_MS,
            )
            .await?;
        let steps = self.repository.list_task_steps(task_id).await?;
        let step = steps
            .iter()
            .find(|step| step.ordinal == ordinal)
            .ok_or_else(integrity_error)?;
        if step.status == TaskStepStatus::Committed {
            return Ok(());
        }
        self.repository
            .transition_task_step(StepTransition {
                task_id,
                ordinal,
                expected_status: TaskStepStatus::Started,
                next_status: TaskStepStatus::Committed,
                evidence: Some(&evidence),
                actor: &self.owner_id,
                now_ms: self.clock.now_ms(),
            })
            .await?;
        Ok(())
    }

    async fn cancel_boundary(
        &self,
        task_id: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<()> {
        if self.cancel_requested(task_id, cancellation).await? {
            self.rollback(task_id, true, None).await?;
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::TaskNotCancellable,
                "task cancellation was settled at a safe boundary",
            ));
        }
        self.repository
            .renew_task_lease(
                task_id,
                &self.owner_id,
                self.clock.now_ms(),
                LEASE_DURATION_MS,
            )
            .await?;
        Ok(())
    }

    async fn cancel_requested(
        &self,
        task_id: &str,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<bool> {
        Ok(cancellation.is_cancelled()
            || self.repository.get_task(task_id).await?.status == TaskStatus::Cancelling)
    }

    async fn cancel_without_effects(&self, task_id: &str) -> McpPlatformResult<()> {
        let task = self.repository.get_task(task_id).await?;
        let next = if task.status == TaskStatus::Running {
            self.transition(task_id, TaskStatus::Cancelling, task.progress)
                .await?;
            TaskStatus::Cancelled
        } else {
            TaskStatus::Cancelled
        };
        self.transition(task_id, next, task.progress).await?;
        Ok(())
    }

    async fn settle_execution_error(
        &self,
        task_id: &str,
        error: McpPlatformError,
    ) -> McpPlatformResult<()> {
        let task = self.repository.get_task(task_id).await?;
        if matches!(
            task.status,
            TaskStatus::Cancelled | TaskStatus::RecoveryRequired
        ) {
            return Ok(());
        }
        if task.operation == TaskOperation::Health {
            if task.status == TaskStatus::Cancelling {
                self.transition(task_id, TaskStatus::Cancelled, task.progress)
                    .await?;
            } else if self.shutdown.is_cancelled() {
                self.transition(task_id, TaskStatus::Interrupted, task.progress)
                    .await?;
            } else {
                let redacted = RedactedError::new(
                    RedactedErrorCode::AdapterFailed,
                    "MCP health task failed at a typed execution boundary",
                    std::iter::empty::<&str>(),
                );
                self.transition_with_error(task_id, TaskStatus::Failed, task.progress, &redacted)
                    .await?;
            }
            return Ok(());
        }
        if self
            .repository
            .is_managed_finalizing(task_id, task.operation)
            .await?
        {
            let failures = self
                .repository
                .record_managed_finalization_failure(task_id, self.clock.now_ms())
                .await?;
            let redacted = RedactedError::new(
                RedactedErrorCode::Interrupted,
                if failures == 1 {
                    "managed lifecycle finalization will resume once from durable cleanup evidence"
                } else {
                    "managed lifecycle finalization requires an explicit retry"
                },
                std::iter::empty::<&str>(),
            );
            if failures > 1 {
                self.transition_with_error(
                    task_id,
                    TaskStatus::RecoveryRequired,
                    task.progress,
                    &redacted,
                )
                .await?;
                return Ok(());
            }
            if task.status != TaskStatus::Interrupted {
                self.transition_with_error(
                    task_id,
                    TaskStatus::Interrupted,
                    task.progress,
                    &redacted,
                )
                .await?;
            }
            self.transition(task_id, TaskStatus::Queued, task.progress)
                .await?;
            return Ok(());
        }
        let effect_may_have_occurred = self
            .repository
            .list_task_steps(task_id)
            .await?
            .iter()
            .any(|step| step.status != TaskStepStatus::NotStarted && step.ordinal > 0);
        if effect_may_have_occurred || task.status == TaskStatus::Cancelling {
            let redacted = RedactedError::new(
                if error.code() == McpPlatformErrorCode::IntegrityError {
                    RedactedErrorCode::VerificationFailed
                } else {
                    RedactedErrorCode::AdapterFailed
                },
                if error.code() == McpPlatformErrorCode::IntegrityError {
                    "MCP lifecycle authority failed integrity verification"
                } else {
                    "MCP lifecycle task failed at a typed execution boundary"
                },
                std::iter::empty::<&str>(),
            );
            self.rollback(
                task_id,
                task.status == TaskStatus::Cancelling,
                Some(&redacted),
            )
            .await
        } else {
            let redacted = RedactedError::new(
                RedactedErrorCode::AdapterFailed,
                "MCP lifecycle task failed at a typed execution boundary",
                std::iter::empty::<&str>(),
            );
            self.transition_with_error(task_id, TaskStatus::Failed, task.progress, &redacted)
                .await
        }
    }

    async fn rollback(
        &self,
        task_id: &str,
        cancelled: bool,
        execution_error: Option<&RedactedError>,
    ) -> McpPlatformResult<()> {
        let task = self.repository.get_task(task_id).await?;
        if task.status != TaskStatus::RollingBack {
            self.transition(task_id, TaskStatus::RollingBack, task.progress)
                .await?;
        }
        let plan = self.repository.get_plan(&task.plan_id).await?;
        let scope = plan.target.installation_scope.as_deref().unwrap_or("user");
        let ids = stable_ids(plan.plan.manifest_id(), scope);
        let expected_config = self
            .ports
            .transport
            .extension_config(plan.plan.connection_projection(), &ids.link_key)?;
        let expected_snapshot = ProjectionSnapshot {
            entry: crate::config::extensions::ExtensionEntry {
                enabled: false,
                config: expected_config,
            },
            created: true,
        };
        let steps = self.repository.list_task_steps(task_id).await?;
        let mut incomplete = false;
        for step in steps
            .iter()
            .filter(|step| step.status != TaskStepStatus::NotStarted)
            .rev()
        {
            if step.compensation_status == CompensationStatus::Committed {
                continue;
            }
            if step.compensation_status == CompensationStatus::Pending
                && self
                    .repository
                    .transition_compensation(CompensationTransition {
                        task_id,
                        ordinal: step.ordinal,
                        expected_status: CompensationStatus::Pending,
                        next_status: CompensationStatus::Started,
                        actor: &self.owner_id,
                        now_ms: self.clock.now_ms(),
                    })
                    .await
                    .is_err()
            {
                incomplete = true;
                break;
            }
            let result = match &step.compensation {
                CompensationDescriptor::RemoveOwnedExtensionConfig {
                    link_key,
                    created_by_task: true,
                    ..
                } => {
                    let expected = match self
                        .repository
                        .get_connection_projection(&ids.managed_mcp_id)
                        .await
                    {
                        Ok(record) => ProjectionSnapshot {
                            entry: crate::config::extensions::ExtensionEntry {
                                enabled: false,
                                config: self
                                    .ports
                                    .transport
                                    .extension_config(&record.projection, link_key)?,
                            },
                            created: true,
                        },
                        Err(_) => expected_snapshot.clone(),
                    };
                    self.ports
                        .projection_sink
                        .remove_owned(link_key, &expected)
                        .await
                        .map(|_| ())
                }
                CompensationDescriptor::RemoveOwnedExtensionConfig {
                    created_by_task: false,
                    ..
                } => Ok(()),
                CompensationDescriptor::RemoveOwnedConnectionProjection {
                    managed_mcp_id, ..
                } => self
                    .repository
                    .remove_owned_projection(managed_mcp_id, task_id)
                    .await
                    .map(|_| ()),
                CompensationDescriptor::RemoveManagedMcp { managed_mcp_id } => self
                    .repository
                    .remove_owned_managed_mcp(managed_mcp_id, task_id)
                    .await
                    .map(|_| ()),
                CompensationDescriptor::NoCompensation
                | CompensationDescriptor::ManagedLifecycleSnapshot { .. } => Ok(()),
                CompensationDescriptor::ManagedUninstallSnapshot {
                    managed_mcp_id,
                    task_id: uninstall_task_id,
                    ..
                } => {
                    self.repository
                        .cancel_managed_uninstall(
                            managed_mcp_id,
                            uninstall_task_id,
                            self.clock.now_ms(),
                        )
                        .await
                }
                CompensationDescriptor::RemoveManagedVersion {
                    managed_mcp_id,
                    version,
                    created_by_task,
                } => match (created_by_task, self.distribution.as_ref()) {
                    (true, Some(distribution)) => {
                        distribution
                            .remove_version(managed_mcp_id, version, &CancellationToken::new())
                            .await
                    }
                    (false, _) => Ok(()),
                    (true, None) => Err(adapter_incompatible()),
                },
                CompensationDescriptor::RestoreManagedActivation {
                    managed_mcp_id,
                    previous_version,
                    target_version,
                } => {
                    self.repository
                        .rollback_managed_installation(
                            managed_mcp_id,
                            previous_version.as_deref(),
                            target_version,
                            task_id,
                            self.clock.now_ms(),
                        )
                        .await
                }
                CompensationDescriptor::RestoreRuntimeActivation {
                    managed_mcp_id,
                    previous_version,
                    target_version,
                } => match self.distribution.as_ref() {
                    Some(distribution) => {
                        let current = distribution.active_version(managed_mcp_id).await?;
                        if current.as_deref() == target_version.as_deref() {
                            distribution
                                .restore_activation(
                                    managed_mcp_id,
                                    previous_version.as_deref(),
                                    task_id,
                                )
                                .await?;
                        } else if current.as_deref() != previous_version.as_deref() {
                            return Err(integrity_error());
                        }
                        if distribution
                            .active_version(managed_mcp_id)
                            .await?
                            .as_deref()
                            != previous_version.as_deref()
                        {
                            return Err(integrity_error());
                        }
                        Ok(())
                    }
                    None => Err(adapter_incompatible()),
                },
                CompensationDescriptor::RestoreManagedProjection {
                    managed_mcp_id,
                    link_key,
                    projection,
                    enabled,
                    projection_digest,
                    manifest_digest,
                    plan_id,
                    owner_task_id,
                } => {
                    let old_config = self
                        .ports
                        .transport
                        .extension_config(projection, link_key)?;
                    let old_entry = crate::config::extensions::ExtensionEntry {
                        enabled: *enabled,
                        config: old_config,
                    };
                    let current = self.ports.projection_sink.get(link_key).await?;
                    if current
                        .as_ref()
                        .is_none_or(|snapshot| snapshot.entry != old_entry)
                    {
                        let restored = match current {
                            Some(current) => {
                                self.ports
                                    .projection_sink
                                    .replace_owned_disabled(
                                        link_key,
                                        &current,
                                        old_entry.config.clone(),
                                    )
                                    .await?
                            }
                            None => {
                                self.ports
                                    .projection_sink
                                    .put_disabled(link_key, old_entry.config.clone())
                                    .await?
                            }
                        };
                        if *enabled {
                            self.ports
                                .projection_sink
                                .set_enabled(link_key, true)
                                .await?;
                        } else if restored.entry.enabled {
                            return Err(integrity_error());
                        }
                    }
                    let current_record = self
                        .repository
                        .get_connection_projection(managed_mcp_id)
                        .await?;
                    if current_record.projection_digest != *projection_digest {
                        self.repository
                            .restore_owned_connection_projection(RestoreOwnedProjection {
                                managed_mcp_id,
                                link_key,
                                projection,
                                plan_id,
                                manifest_digest,
                                owner_task_id: owner_task_id.as_deref(),
                                projection_digest,
                                replacing_task_id: task_id,
                                now_ms: self.clock.now_ms(),
                            })
                            .await?;
                    }
                    Ok(())
                }
                CompensationDescriptor::RestoreQuarantinedVersion {
                    managed_mcp_id,
                    version,
                    quarantine_token,
                } => match self.distribution.as_ref() {
                    Some(distribution) => {
                        distribution
                            .restore_quarantined(managed_mcp_id, version, quarantine_token)
                            .await
                    }
                    None => Err(adapter_incompatible()),
                },
                CompensationDescriptor::CancelManagedUninstall {
                    managed_mcp_id,
                    task_id: uninstall_task_id,
                } => {
                    self.repository
                        .cancel_managed_uninstall(
                            managed_mcp_id,
                            uninstall_task_id,
                            self.clock.now_ms(),
                        )
                        .await
                }
                CompensationDescriptor::FinalizedManagedUninstall { .. }
                | CompensationDescriptor::RemoveConnectionProjection { .. }
                | CompensationDescriptor::RestoreActivation { .. }
                | CompensationDescriptor::RemoveOwnedPath { .. }
                | CompensationDescriptor::RestoreConfigFragment { .. } => {
                    Err(adapter_incompatible())
                }
            };
            if result.is_err() {
                incomplete = true;
                break;
            }
            if self
                .repository
                .transition_compensation(CompensationTransition {
                    task_id,
                    ordinal: step.ordinal,
                    expected_status: CompensationStatus::Started,
                    next_status: CompensationStatus::Committed,
                    actor: &self.owner_id,
                    now_ms: self.clock.now_ms(),
                })
                .await
                .is_err()
            {
                incomplete = true;
                break;
            }
        }
        if incomplete {
            let rollback_error = RedactedError::new(
                RedactedErrorCode::RollbackFailed,
                "MCP lifecycle compensation requires recovery",
                std::iter::empty::<&str>(),
            );
            self.transition_with_rollback(
                task_id,
                TaskStatus::RecoveryRequired,
                execution_error.unwrap_or(&rollback_error),
                RollbackStatus::Incomplete,
            )
            .await
        } else {
            let target = if cancelled {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Failed
            };
            let fallback_error = RedactedError::new(
                if cancelled {
                    RedactedErrorCode::Cancelled
                } else {
                    RedactedErrorCode::AdapterFailed
                },
                if cancelled {
                    "MCP lifecycle task was cancelled and compensated"
                } else {
                    "MCP lifecycle task failed and was compensated"
                },
                std::iter::empty::<&str>(),
            );
            self.transition_with_rollback(
                task_id,
                target,
                execution_error.unwrap_or(&fallback_error),
                RollbackStatus::Complete,
            )
            .await
        }
    }

    async fn transition(
        &self,
        task_id: &str,
        next_status: TaskStatus,
        progress: u8,
    ) -> McpPlatformResult<TaskRecord> {
        let current = self.repository.get_task(task_id).await?;
        self.repository
            .transition_task(TaskTransition {
                task_id,
                expected_revision: current.revision,
                next_status,
                actor: &self.owner_id,
                now_ms: self.clock.now_ms(),
                heartbeat_at_ms: Some(self.clock.now_ms()),
                progress,
                redacted_error: current.redacted_error.as_ref(),
                rollback_status: current.rollback_status,
                rollback_evidence: current.rollback_evidence.as_ref(),
            })
            .await
    }

    async fn transition_with_error(
        &self,
        task_id: &str,
        next_status: TaskStatus,
        progress: u8,
        redacted_error: &RedactedError,
    ) -> McpPlatformResult<()> {
        let current = self.repository.get_task(task_id).await?;
        self.repository
            .transition_task(TaskTransition {
                task_id,
                expected_revision: current.revision,
                next_status,
                actor: &self.owner_id,
                now_ms: self.clock.now_ms(),
                heartbeat_at_ms: Some(self.clock.now_ms()),
                progress,
                redacted_error: Some(redacted_error),
                rollback_status: current.rollback_status,
                rollback_evidence: current.rollback_evidence.as_ref(),
            })
            .await?;
        Ok(())
    }

    async fn transition_with_rollback(
        &self,
        task_id: &str,
        next_status: TaskStatus,
        redacted_error: &RedactedError,
        rollback_status: RollbackStatus,
    ) -> McpPlatformResult<()> {
        let current = self.repository.get_task(task_id).await?;
        let evidence = RollbackEvidence {
            compensation_available: rollback_status != RollbackStatus::Complete,
            remaining_compensations: if rollback_status == RollbackStatus::Complete {
                Vec::new()
            } else {
                current
                    .rollback_evidence
                    .as_ref()
                    .map_or_else(Vec::new, |evidence| {
                        evidence.remaining_compensations.clone()
                    })
            },
        };
        self.repository
            .transition_task(TaskTransition {
                task_id,
                expected_revision: current.revision,
                next_status,
                actor: &self.owner_id,
                now_ms: self.clock.now_ms(),
                heartbeat_at_ms: Some(self.clock.now_ms()),
                progress: current.progress,
                redacted_error: Some(redacted_error),
                rollback_status,
                rollback_evidence: Some(&evidence),
            })
            .await?;
        Ok(())
    }
}

fn existing_step_status(
    step: &super::repository::TaskStepRecord,
    compensation: &CompensationDescriptor,
    adapter_id: &str,
    adapter_version: &str,
) -> McpPlatformResult<TaskStepStatus> {
    if &step.compensation != compensation
        || step.adapter_id != adapter_id
        || step.adapter_version != adapter_version
    {
        return Err(integrity_error());
    }
    Ok(step.status)
}

async fn registration_health(
    sink: &dyn super::lifecycle::ProjectionSink,
    key: &str,
    config: &crate::agents::ExtensionConfig,
    expected_enabled: bool,
) -> McpPlatformResult<super::lifecycle::HealthAdapterResult> {
    let healthy = sink.get(key).await?.is_some_and(|snapshot| {
        snapshot.entry.config == *config && snapshot.entry.enabled == expected_enabled
    });
    Ok(super::lifecycle::HealthAdapterResult {
        result_code: if healthy {
            HealthResultCode::Healthy
        } else {
            HealthResultCode::Unhealthy
        },
        latency_ms: 0,
        capabilities_digest: None,
        tools_digest: None,
        detail_code: if healthy {
            HealthDetailCode::ProjectionConsistent
        } else {
            HealthDetailCode::ProjectionDrift
        },
    })
}

fn registration_effect_from_projection(
    projection: &super::plan::ConnectionProjection,
) -> McpPlatformResult<RegistrationEffect> {
    match projection {
        super::plan::ConnectionProjection::ManagedStdio {
            executable,
            args,
            environment_keys,
            cwd,
            timeout_seconds,
            ..
        } => Ok(RegistrationEffect::ManualStdio {
            spawn: DirectSpawnDescriptor {
                executable: executable.clone(),
                argv: args.clone(),
                environment_keys: environment_keys.clone(),
                working_directory: cwd.clone(),
                timeout_seconds: *timeout_seconds,
            },
        }),
        super::plan::ConnectionProjection::ManagedDockerStdio {
            executable,
            args,
            cwd,
            timeout_seconds,
            ..
        } => Ok(RegistrationEffect::ManualStdio {
            spawn: DirectSpawnDescriptor {
                executable: executable.clone(),
                argv: args.clone(),
                environment_keys: Vec::new(),
                working_directory: cwd.clone(),
                timeout_seconds: *timeout_seconds,
            },
        }),
        _ => Err(integrity_error()),
    }
}

struct StableIds {
    managed_mcp_id: String,
    link_key: String,
    installation_scope: String,
}

fn stable_ids(mcp_id: &str, installation_scope: &str) -> StableIds {
    let managed_mcp_id = stable_managed_mcp_id(mcp_id, installation_scope);
    let suffix = managed_mcp_id
        .strip_prefix("managed_")
        .expect("stable managed id prefix")
        .to_string();
    StableIds {
        managed_mcp_id,
        link_key: format!("managed_mcp_{suffix}"),
        installation_scope: installation_scope.to_string(),
    }
}

pub(crate) fn stable_managed_mcp_id(mcp_id: &str, installation_scope: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(format!("{mcp_id}\0{installation_scope}").as_bytes());
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("managed_{suffix}")
}

const fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "MCP lifecycle journal failed integrity validation",
    )
}

const fn adapter_incompatible() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::AdapterIncompatible,
        "recorded MCP adapter is not compatible with this runner",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_platform::repository::TaskStepRecord;
    use crate::mcp_platform::task::CompensationStatus;

    fn durable_step() -> TaskStepRecord {
        TaskStepRecord {
            task_id: "task".to_string(),
            ordinal: 5,
            status: TaskStepStatus::Started,
            idempotency_token: "task:0:5".to_string(),
            compensation: CompensationDescriptor::FinalizedManagedUninstall {
                managed_mcp_id: "managed".to_string(),
                version: "1.0.0".to_string(),
            },
            evidence: None,
            started_at_ms: Some(1),
            committed_at_ms: None,
            adapter_id: "npm".to_string(),
            adapter_version: "1".to_string(),
            compensation_status: CompensationStatus::Pending,
            compensation_started_at_ms: None,
            compensation_committed_at_ms: None,
        }
    }

    #[test]
    fn preserved_step_requires_exact_compensation_and_adapter_authority() {
        let step = durable_step();
        assert_eq!(
            existing_step_status(
                &step,
                &step.compensation,
                &step.adapter_id,
                &step.adapter_version,
            )
            .unwrap(),
            TaskStepStatus::Started
        );
        for result in [
            existing_step_status(
                &step,
                &CompensationDescriptor::NoCompensation,
                &step.adapter_id,
                &step.adapter_version,
            ),
            existing_step_status(&step, &step.compensation, "git_dev", &step.adapter_version),
            existing_step_status(&step, &step.compensation, &step.adapter_id, "2"),
        ] {
            assert_eq!(
                result.unwrap_err().code(),
                McpPlatformErrorCode::IntegrityError
            );
        }
    }
}
