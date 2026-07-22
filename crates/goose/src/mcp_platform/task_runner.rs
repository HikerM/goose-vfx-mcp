use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::lifecycle::{AuthRequirement, HealthExecution, LifecyclePorts, ProjectionSnapshot};
use super::manifest::digest_serializable;
use super::repository::{
    CompensationTransition, ManagedMcpInventoryRecord, NewHealthObservation,
    ProjectionMutationStatus, PutOwnedProjection, RegisterManagedMcp, StepTransition, TaskRecord,
    TaskTransition,
};
use super::service::{Clock, McpPlatformRepositoryPort};
use super::task::{
    CompensationDescriptor, CompensationStatus, RecoveryDecision, RedactedError, RedactedErrorCode,
    RollbackEvidence, RollbackStatus, StepEvidence, TaskOperation, TaskStatus, TaskStepStatus,
};
use super::{HealthCheckMode, HealthDetailCode, HealthResultCode};

const LEASE_DURATION_MS: i64 = 30_000;
const STALE_HEARTBEAT_MS: i64 = 60_000;

pub struct TaskRunner {
    repository: Arc<dyn McpPlatformRepositoryPort>,
    clock: Arc<dyn Clock>,
    ports: LifecyclePorts,
    owner_id: String,
    active: Mutex<HashMap<String, CancellationToken>>,
    wake: Notify,
    shutdown: CancellationToken,
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
        }
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
        let inventory = self
            .repository
            .get_managed_inventory(managed_mcp_id)
            .await?;
        if inventory.managed.revision != expected_revision {
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
                        self.rollback(&record.task.task_id, false).await
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
            "managed_inventory_repository" | "connection_projection_repository" => {
                step.adapter_version == "1"
            }
            "extension_config_sink" => {
                step.adapter_version == self.ports.projection_sink.adapter_version()
            }
            "core_transport_projection" => {
                step.adapter_version == self.ports.transport.adapter_version()
            }
            "mcp_health" => step.adapter_version == self.ports.health.adapter_version(),
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
                    if mutation.desired_enabled {
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
                        if !snapshot.entry.enabled {
                            if mutation.previous_enabled
                                || mutation.status != ProjectionMutationStatus::Started
                            {
                                return Err(integrity_error());
                            }
                            self.ports
                                .projection_sink
                                .set_enabled(&projection.link_key, true)
                                .await?;
                        }
                    } else if snapshot.is_some_and(|snapshot| snapshot.entry.enabled) {
                        self.ports
                            .projection_sink
                            .set_enabled(&projection.link_key, false)
                            .await?;
                    }
                }
                if mutation.status == ProjectionMutationStatus::Started {
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
            TaskOperation::Install
            | TaskOperation::Update
            | TaskOperation::Repair
            | TaskOperation::Uninstall => Err(McpPlatformError::new(
                McpPlatformErrorCode::OperationNotSupported,
                "task operation is unavailable in phase 3A",
            )),
        }
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
            self.rollback(task_id, true).await?;
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
        _error: McpPlatformError,
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
        let effect_may_have_occurred = self
            .repository
            .list_task_steps(task_id)
            .await?
            .iter()
            .any(|step| step.status != TaskStepStatus::NotStarted && step.ordinal > 0);
        if effect_may_have_occurred || task.status == TaskStatus::Cancelling {
            self.rollback(task_id, task.status == TaskStatus::Cancelling)
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

    async fn rollback(&self, task_id: &str, cancelled: bool) -> McpPlatformResult<()> {
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
            let result = match (step.ordinal, &step.compensation) {
                (
                    3,
                    CompensationDescriptor::RemoveOwnedExtensionConfig {
                        created_by_task: true,
                        ..
                    },
                ) => self
                    .ports
                    .projection_sink
                    .remove_owned(&ids.link_key, &expected_snapshot)
                    .await
                    .map(|_| ()),
                (2, CompensationDescriptor::RemoveOwnedConnectionProjection { .. }) => self
                    .repository
                    .remove_owned_projection(&ids.managed_mcp_id, task_id)
                    .await
                    .map(|_| ()),
                (1, CompensationDescriptor::RemoveManagedMcp { .. }) => self
                    .repository
                    .remove_owned_managed_mcp(&ids.managed_mcp_id, task_id)
                    .await
                    .map(|_| ()),
                _ => Ok(()),
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
            let error = RedactedError::new(
                RedactedErrorCode::RollbackFailed,
                "MCP lifecycle compensation requires recovery",
                std::iter::empty::<&str>(),
            );
            self.transition_with_rollback(
                task_id,
                TaskStatus::RecoveryRequired,
                &error,
                RollbackStatus::Incomplete,
            )
            .await
        } else {
            let target = if cancelled {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Failed
            };
            let error = RedactedError::new(
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
            self.transition_with_rollback(task_id, target, &error, RollbackStatus::Complete)
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

struct StableIds {
    managed_mcp_id: String,
    link_key: String,
    installation_scope: String,
}

fn stable_ids(mcp_id: &str, installation_scope: &str) -> StableIds {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(format!("{mcp_id}\0{installation_scope}").as_bytes());
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    StableIds {
        managed_mcp_id: format!("managed_{suffix}"),
        link_key: format!("managed_mcp_{suffix}"),
        installation_scope: installation_scope.to_string(),
    }
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
