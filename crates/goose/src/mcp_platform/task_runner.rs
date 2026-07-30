mod enrollment_runtime_binding_resolver;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio_util::sync::CancellationToken;

use super::credential_runtime_gate::CredentialRuntimeSnapshot;
use super::credential_runtime_gate::CredentialRuntimeStatus;
use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::lifecycle::{
    observed_projection_digest, DirectSpawnDescriptor, HealthExecution, LifecyclePorts,
    ProjectionCommitPlan, ProjectionSnapshot, RegistrationEffect,
};
use super::managed_distribution::{
    DistributionEffectAdapter, ExternalManagedAcquisition, ManagedInstallEffect,
};
use super::manifest::{digest_serializable, Architecture, Auth, Distribution, Platform};
use super::plan::{
    ManagedCapacityContractStatus, PlanStep, TrustedPlanAdapter, TrustedPlanSelectionContext,
};
use super::policy::PlanOperation;
use super::projection_runtime::{
    authoritative_persisted_projection_entry, RuntimeProjectionAuthority,
};
use super::repository::SqliteMcpPlatformRepository;
use super::repository::{
    ActivateManagedInstallation, CompensationTransition, ExecutionAuthorization,
    ManagedMcpInventoryRecord, NewHealthObservation, ProjectionAuthorization,
    ProjectionMutationStatus, ProjectionRecoveryConfirmationReceipt, PutOwnedProjection,
    QueuedTaskPreflight, RecoveryEligibility, RegisterManagedMcp, RemoveOwnedProjection,
    RestoreOwnedProjection, StageManagedInstallation, StepTransition, TaskRecord, TaskTransition,
};
use super::service::{Clock, McpPlatformRepositoryPort};
use super::task::{
    CompensationDescriptor, CompensationStatus, RecoveryDecision, RedactedError, RedactedErrorCode,
    RollbackEvidence, RollbackStatus, StepEvidence, TaskOperation, TaskStatus, TaskStepStatus,
};
use super::{HealthCheckMode, HealthDetailCode, HealthResultCode};
use enrollment_runtime_binding_resolver::FailClosedEnrollmentRuntimeBindingResolver;
#[cfg(test)]
pub(crate) use enrollment_runtime_binding_resolver::RecordingEnrollmentRuntimeBindingResolver;
#[cfg(test)]
pub(crate) use enrollment_runtime_binding_resolver::StaticEnrollmentRuntimeBindingResolver;
pub(crate) use enrollment_runtime_binding_resolver::{
    EnrollmentRuntimeBindingResolver, RepositoryEnrollmentRuntimeBindingResolver,
    SharedManagedCredentialStatusEvaluator,
};

const LEASE_DURATION_MS: i64 = 30_000;
const STALE_HEARTBEAT_MS: i64 = 60_000;
const STALE_ARTIFACT_CLAIM_MS: i64 = 30 * 60 * 1_000;

enum ProjectionRecoveryObservation {
    Desired(super::repository::ProjectionSinkCommitReceipt),
    PersistedAuthority(ProjectionRecoveryConfirmationReceipt),
    Mismatch,
}

struct RecoveryAuthorizationGrant {
    authorization: ProjectionAuthorization,
}

impl RecoveryAuthorizationGrant {
    fn authorization(&self) -> &ProjectionAuthorization {
        &self.authorization
    }
}

pub struct TaskRunner {
    repository: Arc<dyn McpPlatformRepositoryPort>,
    projection_repository: Arc<SqliteMcpPlatformRepository>,
    projection_authority: RuntimeProjectionAuthority,
    clock: Arc<dyn Clock>,
    ports: LifecyclePorts,
    managed_credential_status_evaluator: Arc<SharedManagedCredentialStatusEvaluator>,
    owner_id: String,
    active: Mutex<HashMap<String, CancellationToken>>,
    recovery_gate: AsyncMutex<()>,
    wake: Notify,
    shutdown: CancellationToken,
    distribution: Option<Arc<dyn DistributionEffectAdapter>>,
    distribution_unavailable_error: Option<McpPlatformError>,
}

impl TaskRunner {
    #[cfg(windows)]
    pub(crate) async fn acquire_verified_runtime_activation(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<super::managed_distribution::VerifiedManagedRuntimeActivation> {
        self.distribution
            .as_ref()
            .ok_or_else(|| {
                McpPlatformError::new(
                    McpPlatformErrorCode::RuntimeControlUnavailable,
                    "managed runtime activation verification is unavailable",
                )
            })?
            .acquire_verified_runtime_activation(managed_mcp_id)
            .await
    }

    pub(super) fn new_with_authority(
        repository: Arc<SqliteMcpPlatformRepository>,
        projection_authority: RuntimeProjectionAuthority,
        clock: Arc<dyn Clock>,
        ports: LifecyclePorts,
        owner_id: String,
    ) -> Self {
        Self {
            repository: repository.clone(),
            projection_repository: repository,
            projection_authority,
            clock,
            ports,
            managed_credential_status_evaluator: SharedManagedCredentialStatusEvaluator::new(
                Arc::new(FailClosedEnrollmentRuntimeBindingResolver::new()),
            ),
            owner_id,
            active: Mutex::new(HashMap::new()),
            recovery_gate: AsyncMutex::new(()),
            wake: Notify::new(),
            shutdown: CancellationToken::new(),
            distribution: None,
            distribution_unavailable_error: None,
        }
    }

    pub(crate) fn new(
        repository: Arc<SqliteMcpPlatformRepository>,
        clock: Arc<dyn Clock>,
        ports: LifecyclePorts,
        owner_id: String,
    ) -> Self {
        let projection_authority = super::projection_runtime::bootstrap_debug_runner_authority(
            repository.as_ref(),
            &ports,
        );
        Self::new_with_authority(repository, projection_authority, clock, ports, owner_id)
    }

    pub fn with_distribution_adapter(
        mut self,
        adapter: Arc<dyn DistributionEffectAdapter>,
    ) -> Self {
        self.distribution = Some(adapter);
        self
    }

    pub(crate) fn with_distribution_unavailable_error(mut self, error: McpPlatformError) -> Self {
        self.distribution_unavailable_error = Some(error);
        self
    }

    pub(crate) fn set_distribution_unavailable_error(&mut self, error: McpPlatformError) {
        self.distribution_unavailable_error = Some(error);
    }

    pub(crate) fn with_enrollment_runtime_binding_resolver(
        self,
        resolver: Arc<dyn EnrollmentRuntimeBindingResolver>,
    ) -> Self {
        self.set_enrollment_runtime_binding_resolver(resolver);
        self
    }

    pub(crate) fn with_managed_credential_status_evaluator(
        mut self,
        evaluator: Arc<SharedManagedCredentialStatusEvaluator>,
    ) -> Self {
        self.managed_credential_status_evaluator = evaluator;
        self
    }

    pub(crate) fn set_enrollment_runtime_binding_resolver(
        &self,
        resolver: Arc<dyn EnrollmentRuntimeBindingResolver>,
    ) {
        self.managed_credential_status_evaluator
            .set_resolver(resolver);
    }

    pub fn notify(&self) {
        self.wake.notify_one();
    }

    fn distribution(&self) -> McpPlatformResult<&Arc<dyn DistributionEffectAdapter>> {
        if let Some(error) = self.distribution_unavailable_error.clone() {
            return Err(error);
        }
        if let Some(distribution) = self.distribution.as_ref() {
            return Ok(distribution);
        }
        Err(adapter_incompatible())
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
        let task = loop {
            let now_ms = self.clock.now_ms();
            let Some(candidate) = self.repository.next_queued_task_candidate(now_ms).await? else {
                return Ok(false);
            };
            let preflight = match self.repository.load_queued_task_preflight(&candidate).await {
                Ok(preflight) => preflight,
                Err(_) => {
                    if self
                        .repository
                        .reject_queued_task_candidate(
                            &candidate,
                            &self.owner_id,
                            now_ms,
                            &preflight_rejected_error(),
                        )
                        .await?
                    {
                        return Ok(true);
                    }
                    continue;
                }
            };
            if self.preflight_queued_task(&preflight).await.is_err() {
                if self
                    .repository
                    .reject_queued_task_candidate(
                        &candidate,
                        &self.owner_id,
                        now_ms,
                        &preflight_rejected_error(),
                    )
                    .await?
                {
                    return Ok(true);
                }
                continue;
            }
            if let Some(task) = self
                .repository
                .claim_preflighted_task(&preflight, &self.owner_id, now_ms, LEASE_DURATION_MS)
                .await?
            {
                break task;
            }
        };
        let authorization = self
            .repository
            .authorize_execution(&task.task_id, &self.owner_id, self.clock.now_ms())
            .await?;
        let cancellation = CancellationToken::new();
        self.active
            .lock()
            .expect("runner active lock")
            .insert(task.task_id.clone(), cancellation.clone());
        let heartbeat_stop = CancellationToken::new();
        let execution = self.execute(authorization, cancellation.clone());
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
                mutation.status != ProjectionMutationStatus::RecoveryRequired
                    && mutation.managed_mcp_id == managed_mcp_id
                    && mutation.expected_revision == expected_revision
                    && mutation.desired_enabled == enabled
            });
        self.recover_projection_mutations().await?;
        if self
            .projection_repository
            .projection_recovery_required(managed_mcp_id)
            .await?
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::ProjectionConflict,
                "managed MCP projection recovery must be resolved before changing enablement",
            ));
        }
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
            reject_managed_remote_http_auth(manifest.verified.manifest())?;
            if inventory.managed.state.health != super::HealthState::Healthy {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::HealthFailed,
                    "managed MCP must be healthy before it can be enabled",
                ));
            }
            let projection = self
                .repository
                .get_connection_projection(managed_mcp_id)
                .await?;
            if projection.manifest_digest.as_deref() != Some(manifest_digest) {
                return Err(integrity_error());
            }
            self.ensure_managed_enable_ready(
                &manifest.verified.manifest().auth,
                managed_mcp_id,
                inventory.managed.revision,
                manifest_digest,
                "managed MCP credential handle is unavailable",
            )
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
        if let Some(projection) = projection.as_ref() {
            if projection.manifest_digest.as_deref()
                != inventory.lifecycle.active_manifest_digest.as_deref()
                || projection.plan_id.is_none()
                || projection.owner_task_id.is_none()
            {
                return Err(integrity_error());
            }
        }
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
            .projection_repository
            .begin_projection_mutation_authorized(
                self.projection_authority.write_capability(),
                &self.projection_authority,
                managed_mcp_id,
                expected_revision,
                enabled,
                self.clock.now_ms(),
            )
            .await?;
        let authorization = self
            .projection_repository
            .authorize_projection_mutation(
                self.projection_authority.write_capability(),
                &self.projection_authority,
                mutation.mutation_id,
            )
            .await?;
        if enabled {
            let manifest = authorization.manifest().ok_or_else(integrity_error)?;
            let projection = authorization.projection().ok_or_else(integrity_error)?;
            reject_managed_remote_http_auth(manifest.verified.manifest())?;
            if projection.manifest_digest.as_deref() != Some(manifest.verified.digest())
                || projection.plan_id.is_none()
                || projection.owner_task_id.is_none()
            {
                return Err(integrity_error());
            }
            self.ensure_managed_enable_ready(
                &manifest.verified.manifest().auth,
                managed_mcp_id,
                authorization.inventory().managed.revision,
                manifest.verified.digest(),
                "managed MCP credential handle is unavailable",
            )
            .await?;
        }
        let committed = async {
            let receipt = self.projection_sink_commit_receipt(&authorization).await?;
            let completion_authorization = if matches!(
                authorization.mutation().status,
                ProjectionMutationStatus::Started | ProjectionMutationStatus::RecoveryRequired
            ) {
                self.projection_repository
                    .mark_projection_config_committed_authorized(
                        self.projection_authority.write_capability(),
                        &self.projection_authority,
                        &authorization,
                        self.clock.now_ms(),
                    )
                    .await?;
                self.projection_repository
                    .authorize_projection_mutation(
                        self.projection_authority.write_capability(),
                        &self.projection_authority,
                        mutation.mutation_id,
                    )
                    .await?
            } else {
                authorization.clone()
            };
            if matches!(
                completion_authorization.mutation().status,
                ProjectionMutationStatus::Started | ProjectionMutationStatus::RecoveryRequired
            ) {
                return Err(integrity_error());
            }
            if enabled {
                let manifest = completion_authorization
                    .manifest()
                    .ok_or_else(integrity_error)?;
                let projection = completion_authorization
                    .projection()
                    .ok_or_else(integrity_error)?;
                if projection.manifest_digest.as_deref() != Some(manifest.verified.digest())
                    || projection.plan_id.is_none()
                    || projection.owner_task_id.is_none()
                {
                    return Err(integrity_error());
                }
                self.ensure_managed_enable_ready(
                    &manifest.verified.manifest().auth,
                    managed_mcp_id,
                    completion_authorization.inventory().managed.revision,
                    manifest.verified.digest(),
                    "managed MCP credential handle changed during projection mutation",
                )
                .await?;
            }
            self.projection_repository
                .complete_projection_mutation_authorized(
                    self.projection_authority.write_capability(),
                    &self.projection_authority,
                    &completion_authorization,
                    &receipt,
                    self.clock.now_ms(),
                )
                .await
        }
        .await;
        if let Err(error) = committed {
            self.recover_projection_mutations().await?;
            if self
                .projection_repository
                .projection_recovery_required(managed_mcp_id)
                .await?
            {
                return Err(match error.code() {
                    McpPlatformErrorCode::ProjectionWitnessExpired
                    | McpPlatformErrorCode::ProjectionWitnessConsumed => error,
                    _ => projection_recovery_required(),
                });
            }
            return self.repository.get_managed_inventory(managed_mcp_id).await;
        }
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

    async fn authorize_effect(&self, task_id: &str) -> McpPlatformResult<ExecutionAuthorization> {
        let now_ms = self.clock.now_ms();
        let authorization = self
            .repository
            .authorize_execution(task_id, &self.owner_id, now_ms)
            .await?;
        self.repository
            .validate_execution_authorization(&authorization, now_ms)
            .await?;
        Ok(authorization)
    }

    pub(crate) async fn managed_enrollment_runtime_snapshot(
        &self,
        auth: &Auth,
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
    ) -> CredentialRuntimeSnapshot {
        self.managed_credential_status_evaluator
            .safe_runtime_snapshot(
                managed_mcp_id,
                current_managed_revision,
                current_manifest_digest,
                auth,
            )
            .await
    }

    async fn managed_enrollment_runtime_status(
        &self,
        auth: &Auth,
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
    ) -> CredentialRuntimeStatus {
        self.managed_enrollment_runtime_snapshot(
            auth,
            managed_mcp_id,
            current_managed_revision,
            current_manifest_digest,
        )
        .await
        .status()
    }

    async fn ensure_managed_enable_ready(
        &self,
        auth: &Auth,
        managed_mcp_id: &str,
        current_managed_revision: i64,
        current_manifest_digest: &str,
        message: &'static str,
    ) -> McpPlatformResult<()> {
        match self
            .managed_enrollment_runtime_status(
                auth,
                managed_mcp_id,
                current_managed_revision,
                current_manifest_digest,
            )
            .await
        {
            CredentialRuntimeStatus::Ready => Ok(()),
            CredentialRuntimeStatus::Unconfigured
            | CredentialRuntimeStatus::ReRegistrationRequired => Err(McpPlatformError::new(
                McpPlatformErrorCode::CredentialMissing,
                message,
            )),
            CredentialRuntimeStatus::TrustedStateConflict => Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                message,
            )),
            CredentialRuntimeStatus::TemporarilyUnavailable => Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                message,
            )),
        }
    }

    async fn ensure_desired_managed_enable_ready(
        &self,
        desired_enabled: bool,
        task_id: &str,
        managed_mcp_id: &str,
        manifest_digest: &str,
        message: &'static str,
    ) -> McpPlatformResult<()> {
        if !desired_enabled {
            return Ok(());
        }
        let authorization = self.authorize_effect(task_id).await?;
        let inventory = authorization
            .inventory
            .as_ref()
            .ok_or_else(integrity_error)?;
        let manifest = &authorization.manifest;
        if inventory.managed.managed_mcp_id != managed_mcp_id
            || inventory.lifecycle.active_manifest_digest.as_deref() != Some(manifest_digest)
            || manifest.verified.digest() != manifest_digest
        {
            return Err(integrity_error());
        }
        self.ensure_managed_enable_ready(
            &manifest.verified.manifest().auth,
            managed_mcp_id,
            inventory.managed.revision,
            manifest_digest,
            message,
        )
        .await
    }

    async fn ensure_desired_managed_enable_ready_or_disable(
        &self,
        desired_enabled: bool,
        task_id: &str,
        managed_mcp_id: &str,
        link_key: &str,
        manifest_digest: &str,
        message: &'static str,
    ) -> McpPlatformResult<()> {
        match self
            .ensure_desired_managed_enable_ready(
                desired_enabled,
                task_id,
                managed_mcp_id,
                manifest_digest,
                message,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                self.disable_managed_projection_and_runtime(
                    task_id,
                    managed_mcp_id,
                    Some(link_key),
                )
                .await?;
                Err(error)
            }
        }
    }

    async fn disable_managed_projection_and_runtime(
        &self,
        task_id: &str,
        managed_mcp_id: &str,
        link_key: Option<&str>,
    ) -> McpPlatformResult<()> {
        if let Some(distribution) = self.distribution.as_ref() {
            distribution
                .restore_activation(managed_mcp_id, None, task_id)
                .await?;
            if distribution.active_version(managed_mcp_id).await?.is_some() {
                return Err(integrity_error());
            }
        }
        let link_key = match link_key {
            Some(link_key) => Some(link_key.to_string()),
            None => match self
                .repository
                .get_connection_projection(managed_mcp_id)
                .await
            {
                Ok(projection) => Some(projection.link_key),
                Err(error) if error.code() == McpPlatformErrorCode::NotFound => None,
                Err(error) => return Err(error),
            },
        };
        if let Some(link_key) = link_key {
            if matches!(
                self.ports.projection_sink.get(&link_key).await?,
                Some(snapshot) if snapshot.entry.enabled
            ) {
                self.ports
                    .projection_sink
                    .set_enabled(&link_key, false)
                    .await?;
            }
        }
        Ok(())
    }

    async fn ensure_runtime_activation_restore_ready_or_disable(
        &self,
        task_id: &str,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<()> {
        let readiness = async {
            let inventory = self
                .repository
                .get_managed_inventory(managed_mcp_id)
                .await?;
            let manifest_digest = inventory
                .lifecycle
                .active_manifest_digest
                .as_deref()
                .ok_or_else(integrity_error)?;
            let manifest = self.repository.get_manifest(manifest_digest).await?;
            reject_managed_remote_http_auth(manifest.verified.manifest())?;
            self.ensure_managed_enable_ready(
                &manifest.verified.manifest().auth,
                managed_mcp_id,
                inventory.managed.revision,
                manifest_digest,
                "managed MCP credential handle is unavailable for runtime activation restoration",
            )
            .await
        }
        .await;
        match readiness {
            Ok(()) => Ok(()),
            Err(error) => {
                self.disable_managed_projection_and_runtime(task_id, managed_mcp_id, None)
                    .await?;
                Err(error)
            }
        }
    }

    async fn ensure_projection_recovery_enable_ready_or_disable(
        &self,
        authorization: &ProjectionAuthorization,
        message: &'static str,
    ) -> McpPlatformResult<()> {
        match self
            .ensure_projection_recovery_enable_ready(authorization, message)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                self.disable_managed_projection_and_runtime(
                    "projection-recovery",
                    &authorization.inventory().managed.managed_mcp_id,
                    authorization
                        .projection()
                        .map(|projection| projection.link_key.as_str()),
                )
                .await?;
                Err(error)
            }
        }
    }

    async fn ensure_projection_recovery_enable_ready(
        &self,
        authorization: &ProjectionAuthorization,
        message: &'static str,
    ) -> McpPlatformResult<()> {
        if !self
            .projection_recovery_has_enabled_sink(authorization)
            .await?
        {
            return Ok(());
        }
        let manifest = authorization.manifest().ok_or_else(integrity_error)?;
        let manifest_digest = manifest.verified.digest();
        let projection = authorization.projection().ok_or_else(integrity_error)?;
        if authorization
            .inventory()
            .lifecycle
            .active_manifest_digest
            .as_deref()
            != Some(manifest_digest)
            || projection.manifest_digest.as_deref() != Some(manifest_digest)
            || projection.plan_id.is_none()
            || projection.owner_task_id.is_none()
        {
            return Err(integrity_error());
        }
        self.ensure_managed_enable_ready(
            &manifest.verified.manifest().auth,
            &authorization.inventory().managed.managed_mcp_id,
            authorization.inventory().managed.revision,
            manifest_digest,
            message,
        )
        .await
    }

    async fn projection_recovery_has_enabled_sink(
        &self,
        authorization: &ProjectionAuthorization,
    ) -> McpPlatformResult<bool> {
        let persisted = authoritative_persisted_projection_entry(
            authorization.inventory().managed.state.default_enabled,
            authorization.projection(),
        )?;
        if persisted.is_some_and(|entry| entry.enabled) {
            return Ok(true);
        }
        let witness = authorization.witness_v2()?;
        Ok(self
            .ports
            .projection_sink
            .get(&witness.runtime_id)
            .await?
            .is_some_and(|snapshot| snapshot.entry.enabled))
    }

    async fn projection_sink_commit_receipt(
        &self,
        authorization: &super::repository::ProjectionAuthorization,
    ) -> McpPlatformResult<super::repository::ProjectionSinkCommitReceipt> {
        self.ensure_projection_recovery_enable_ready(
            authorization,
            "managed MCP credential handle became unavailable before projection enablement",
        )
        .await?;
        let plan = self.projection_commit_plan(authorization)?;
        let proof = self
            .ports
            .projection_sink
            .commit_enabled_projection(&plan)
            .await?;
        self.projection_authority
            .issue_sink_commit_receipt(authorization, proof)
    }

    fn projection_commit_plan(
        &self,
        authorization: &super::repository::ProjectionAuthorization,
    ) -> McpPlatformResult<ProjectionCommitPlan> {
        let witness = authorization.witness_v2()?;
        match authorization.projection() {
            Some(projection) => self.projection_authority.commit_plan(
                Some(crate::config::extensions::ExtensionEntry {
                    enabled: witness.desired_enabled,
                    config: self
                        .ports
                        .transport
                        .extension_config(&projection.projection, &projection.link_key)?,
                }),
                witness,
            ),
            None if !witness.desired_enabled => {
                self.projection_authority.commit_plan(None, witness)
            }
            None => Err(integrity_error()),
        }
    }

    fn projection_recovery_confirm_plan(
        &self,
        authorization: &ProjectionAuthorization,
    ) -> McpPlatformResult<ProjectionCommitPlan> {
        let witness = authorization.witness_v2()?;
        let expected = authoritative_persisted_projection_entry(
            authorization.inventory().managed.state.default_enabled,
            authorization.projection(),
        )?;
        self.projection_authority
            .confirm_existing_plan(expected, witness)
    }

    async fn projection_recovery_confirmation_receipt(
        &self,
        authorization: &ProjectionAuthorization,
    ) -> McpPlatformResult<ProjectionRecoveryConfirmationReceipt> {
        self.validate_recovery_authorization(authorization).await?;
        let plan = self.projection_recovery_confirm_plan(authorization)?;
        let proof = self
            .ports
            .projection_sink
            .confirm_target_state(&plan)
            .await
            .map_err(|_| projection_witness_expired())?
            .ok_or_else(projection_witness_expired)?;
        self.projection_authority
            .issue_sink_recovery_confirmation_receipt(
                authorization,
                plan.target_state_digest(),
                proof,
            )
            .map_err(|_| projection_witness_expired())
    }

    pub async fn recover_startup(&self) -> McpPlatformResult<()> {
        self.recover_projection_mutations().await?;
        let _recovery_guard = self.recovery_gate.lock().await;
        self.ensure_recovery_eligible().await?;
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
                let authorization = self.authorize_effect(&record.task.task_id).await?;
                let adapters_compatible = authorization
                    .steps
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

    pub(crate) async fn resolve_projection_recovery(
        &self,
        managed_mcp_id: &str,
    ) -> McpPlatformResult<()> {
        let _recovery_guard = self.recovery_gate.lock().await;
        self.ensure_recovery_eligible().await?;
        let mutation = self
            .projection_repository
            .list_pending_projection_mutations()
            .await?
            .into_iter()
            .find(|mutation| {
                mutation.managed_mcp_id == managed_mcp_id
                    && mutation.status == ProjectionMutationStatus::RecoveryRequired
            })
            .ok_or_else(|| {
                McpPlatformError::new(
                    McpPlatformErrorCode::NotFound,
                    "managed MCP projection recovery record not found",
                )
            })?;
        let recovery_grant = self
            .issue_recovery_authorization(mutation.mutation_id)
            .await?;
        let authorization = recovery_grant.authorization();
        let result = async {
            match self.observe_projection_recovery(&authorization).await? {
                ProjectionRecoveryObservation::Desired(receipt) => {
                    self.complete_projection_mutation_without_writing(&authorization, &receipt)
                        .await
                }
                ProjectionRecoveryObservation::PersistedAuthority(receipt) => {
                    self.ensure_projection_recovery_enable_ready_or_disable(
                        &authorization,
                        "managed MCP projection recovery requires refreshed credential enrollment",
                    )
                    .await?;
                    self.projection_repository
                        .resolve_projection_mutation_recovery_authorized(
                            self.projection_authority.write_capability(),
                            &self.projection_authority,
                            &authorization,
                            &receipt,
                            self.clock.now_ms(),
                        )
                        .await
                }
                ProjectionRecoveryObservation::Mismatch => {
                    self.disable_managed_projection_and_runtime(
                        "projection-recovery",
                        managed_mcp_id,
                        authorization
                            .projection()
                            .map(|projection| projection.link_key.as_str()),
                    )
                    .await?;
                    Err(projection_recovery_required())
                }
            }
        }
        .await;
        if let Err(error) = result {
            self.projection_repository
                .force_projection_mutation_recovery_required(
                    self.projection_authority.write_capability(),
                    mutation.mutation_id,
                    self.clock.now_ms(),
                )
                .await?;
            return Err(error);
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
        let _recovery_guard = self.recovery_gate.lock().await;
        self.ensure_recovery_eligible().await?;
        for mutation in self
            .projection_repository
            .list_pending_projection_mutations()
            .await?
        {
            let result = async {
                let recovery_grant = self
                    .issue_recovery_authorization(mutation.mutation_id)
                    .await?;
                let authorization = recovery_grant.authorization();
                match self.observe_projection_recovery(&authorization).await? {
                    ProjectionRecoveryObservation::Desired(receipt) => {
                        self.complete_projection_mutation_without_writing(&authorization, &receipt)
                            .await
                    }
                    ProjectionRecoveryObservation::PersistedAuthority(_) => {
                        self.ensure_projection_recovery_enable_ready_or_disable(
                            &authorization,
                            "managed MCP projection recovery requires refreshed credential enrollment",
                        )
                        .await?;
                        Err(projection_recovery_required())
                    }
                    ProjectionRecoveryObservation::Mismatch => {
                        self.disable_managed_projection_and_runtime(
                            "projection-recovery",
                            &authorization.inventory().managed.managed_mcp_id,
                            authorization
                                .projection()
                                .map(|projection| projection.link_key.as_str()),
                        )
                        .await?;
                        Err(projection_recovery_required())
                    }
                }
            }
            .await;
            if let Err(error) = result {
                self.projection_repository
                    .force_projection_mutation_recovery_required(
                        self.projection_authority.write_capability(),
                        mutation.mutation_id,
                        self.clock.now_ms(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn ensure_recovery_eligible(&self) -> McpPlatformResult<()> {
        if self
            .repository
            .recovery_eligibility()
            .await
            .unwrap_or(RecoveryEligibility::Blocked)
            != RecoveryEligibility::Eligible
        {
            return Err(recovery_blocked());
        }
        Ok(())
    }

    async fn validate_recovery_authorization(
        &self,
        authorization: &ProjectionAuthorization,
    ) -> McpPlatformResult<()> {
        let refreshed = self
            .issue_recovery_authorization(authorization.mutation().mutation_id)
            .await?;
        let refreshed = refreshed.authorization();
        if refreshed.checkpoint() != authorization.checkpoint()
            || refreshed.mutation().expected_revision != authorization.mutation().expected_revision
            || refreshed.mutation().writer_commitment != authorization.mutation().writer_commitment
        {
            return Err(projection_witness_expired());
        }
        Ok(())
    }

    async fn issue_recovery_authorization(
        &self,
        mutation_id: i64,
    ) -> McpPlatformResult<RecoveryAuthorizationGrant> {
        self.ensure_recovery_eligible().await?;
        let authorization = self
            .projection_repository
            .authorize_projection_mutation(
                self.projection_authority.write_capability(),
                &self.projection_authority,
                mutation_id,
            )
            .await?;
        self.ensure_recovery_eligible().await?;
        Ok(RecoveryAuthorizationGrant { authorization })
    }

    async fn observe_projection_recovery(
        &self,
        authorization: &ProjectionAuthorization,
    ) -> McpPlatformResult<ProjectionRecoveryObservation> {
        self.validate_recovery_authorization(authorization).await?;
        let witness = authorization.witness_v2()?;
        let current = self.ports.projection_sink.get(&witness.runtime_id).await?;
        let current_digest = observed_projection_digest(
            &witness.sink_identity,
            &witness.runtime_id,
            current.as_ref().map(|snapshot| &snapshot.entry),
        )?;
        if current_digest == witness.observed_state_digest {
            let plan = self.projection_commit_plan(authorization)?;
            self.validate_recovery_authorization(authorization).await?;
            let Some(proof) = self
                .ports
                .projection_sink
                .confirm_target_state(&plan)
                .await?
            else {
                return Err(projection_witness_expired());
            };
            let receipt = self
                .projection_authority
                .issue_sink_commit_receipt(authorization, proof)?;
            return Ok(ProjectionRecoveryObservation::Desired(receipt));
        }
        let persisted = authoritative_persisted_projection_entry(
            authorization.inventory().managed.state.default_enabled,
            authorization.projection(),
        )
        .map_err(|_| projection_witness_expired())?;
        let persisted_digest = observed_projection_digest(
            &witness.sink_identity,
            &witness.runtime_id,
            persisted.as_ref(),
        )?;
        if current_digest == persisted_digest {
            self.validate_recovery_authorization(authorization).await?;
            let receipt = self
                .projection_recovery_confirmation_receipt(authorization)
                .await?;
            Ok(ProjectionRecoveryObservation::PersistedAuthority(receipt))
        } else {
            Ok(ProjectionRecoveryObservation::Mismatch)
        }
    }

    async fn complete_projection_mutation_without_writing(
        &self,
        authorization: &ProjectionAuthorization,
        receipt: &super::repository::ProjectionSinkCommitReceipt,
    ) -> McpPlatformResult<()> {
        let completion_authorization = if matches!(
            authorization.mutation().status,
            ProjectionMutationStatus::Started | ProjectionMutationStatus::RecoveryRequired
        ) {
            self.projection_repository
                .mark_projection_config_committed_authorized(
                    self.projection_authority.write_capability(),
                    &self.projection_authority,
                    authorization,
                    self.clock.now_ms(),
                )
                .await?;
            self.projection_repository
                .authorize_projection_mutation(
                    self.projection_authority.write_capability(),
                    &self.projection_authority,
                    authorization.mutation().mutation_id,
                )
                .await?
        } else {
            authorization.clone()
        };
        if completion_authorization.mutation().status != ProjectionMutationStatus::ConfigCommitted {
            return Err(projection_witness_consumed());
        }
        self.ensure_projection_recovery_enable_ready_or_disable(
            &completion_authorization,
            "managed MCP projection recovery requires refreshed credential enrollment",
        )
        .await?;
        self.projection_repository
            .complete_projection_mutation_authorized(
                self.projection_authority.write_capability(),
                &self.projection_authority,
                &completion_authorization,
                receipt,
                self.clock.now_ms(),
            )
            .await
    }

    async fn execute(
        &self,
        authorization: ExecutionAuthorization,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        let operation = authorization.task.operation;
        match operation {
            TaskOperation::Register => self.execute_register(authorization, cancellation).await,
            TaskOperation::Health => self.execute_health(authorization, cancellation).await,
            TaskOperation::Install | TaskOperation::Update | TaskOperation::Repair => {
                self.execute_managed_install(authorization, cancellation)
                    .await
            }
            TaskOperation::Uninstall => {
                self.execute_managed_uninstall(authorization, cancellation)
                    .await
            }
        }
    }

    async fn preflight_managed_storage_capacity(
        &self,
        distribution: &Arc<dyn DistributionEffectAdapter>,
        acquisition: &PlanStep,
        capacity_contract: ManagedCapacityContractStatus,
    ) -> McpPlatformResult<()> {
        #[cfg(target_os = "windows")]
        {
            if !matches!(acquisition, PlanStep::AcquireManagedDistribution { .. }) {
                return Ok(());
            }
            let ManagedCapacityContractStatus::Known {
                required_peak_bytes,
            } = capacity_contract
            else {
                return Err(managed_storage_capacity_preflight_unavailable());
            };
            let observation = distribution
                .check_managed_storage_capacity(required_peak_bytes)
                .await
                .map_err(|_| managed_storage_capacity_preflight_unavailable())?;
            if observation.required_peak_bytes != required_peak_bytes
                || observation.available_bytes < required_peak_bytes
            {
                return Err(managed_storage_capacity_preflight_unavailable());
            }
            Ok(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (distribution, acquisition, capacity_contract);
            Ok(())
        }
    }

    async fn preflight_queued_task(
        &self,
        preflight: &QueuedTaskPreflight,
    ) -> McpPlatformResult<()> {
        if !matches!(
            preflight.candidate().task().operation,
            TaskOperation::Install | TaskOperation::Update | TaskOperation::Repair
        ) {
            return Ok(());
        }
        let distribution = self.distribution()?;
        let plan = &preflight.plan().plan;
        plan.verify_integrity()?;
        if preflight.manifest().verified.digest() != plan.manifest_digest()
            || plan.adapter().version != distribution.adapter_version()
        {
            return Err(adapter_incompatible());
        }
        let selection = TrustedPlanSelectionContext::new(
            executing_platform(),
            executing_architecture(),
            plan_operation_for_task(preflight.candidate().task().operation),
            trusted_dispatch_adapter(&preflight.manifest().verified.manifest().distribution),
        );
        let capacity_contract =
            plan.verify_trusted_acquisition_contract(&preflight.manifest().verified, selection)?;
        let acquisitions = plan
            .steps()
            .iter()
            .filter(|step| {
                matches!(
                    step,
                    PlanStep::AcquireManagedDistribution { .. }
                        | PlanStep::AcquireDockerDistribution { .. }
                        | PlanStep::AcquireGitDevDistribution { .. }
                )
            })
            .collect::<Vec<_>>();
        let [acquisition] = acquisitions.as_slice() else {
            return Err(integrity_error());
        };
        self.preflight_managed_storage_capacity(distribution, acquisition, capacity_contract)
            .await
    }

    async fn execute_managed_install(
        &self,
        authorization: ExecutionAuthorization,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        let task = authorization.task.clone();
        let distribution = self.distribution()?;
        let plan_record = authorization.plan.clone();
        let manifest_record = authorization.manifest.clone();
        let plan = &plan_record.plan;
        plan.verify_integrity()?;
        if manifest_record.verified.digest() != plan.manifest_digest()
            || plan.adapter().version != distribution.adapter_version()
        {
            return Err(adapter_incompatible());
        }
        let selection = TrustedPlanSelectionContext::new(
            executing_platform(),
            executing_architecture(),
            plan_operation_for_task(task.operation),
            trusted_dispatch_adapter(&manifest_record.verified.manifest().distribution),
        );
        let capacity_contract =
            plan.verify_trusted_acquisition_contract(&manifest_record.verified, selection)?;
        let acquisition = {
            let candidates = plan
                .steps()
                .iter()
                .filter(|step| {
                    matches!(
                        step,
                        PlanStep::AcquireManagedDistribution { .. }
                            | PlanStep::AcquireDockerDistribution { .. }
                            | PlanStep::AcquireGitDevDistribution { .. }
                    )
                })
                .collect::<Vec<_>>();
            match candidates.as_slice() {
                [acquisition] => *acquisition,
                _ => return Err(integrity_error()),
            }
        };
        self.preflight_managed_storage_capacity(distribution, acquisition, capacity_contract)
            .await?;
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
        let (
            source_url,
            expected_sha256,
            expected_size_bytes,
            platform_selector,
            external_acquisition,
        ) = match acquisition {
            PlanStep::AcquireManagedDistribution {
                artifact_url,
                artifact_digest,
                expected_size_bytes,
                platform,
                arch,
                ..
            } => (
                artifact_url.clone(),
                artifact_digest.value.clone(),
                *expected_size_bytes,
                format!("{:?}/{:?}", platform, arch).to_ascii_lowercase(),
                None,
            ),
            PlanStep::AcquireDockerDistribution {
                image,
                digest,
                mount_plan_digest,
                ..
            } => (
                format!("https://{}", image.split('/').next().unwrap_or_default()),
                digest.value.clone(),
                None,
                "docker/immutable".to_string(),
                Some(ExternalManagedAcquisition::Docker {
                    image: image.clone(),
                    digest: digest.value.clone(),
                    mount_plan_digest: mount_plan_digest.clone(),
                }),
            ),
            PlanStep::AcquireGitDevDistribution {
                repository,
                acquisition_digest,
                repository_origin,
                commit,
                subdirectory,
                underlying_adapter,
            } => (
                repository.clone(),
                acquisition_digest.clone(),
                None,
                "git_dev/exact_commit".to_string(),
                Some(ExternalManagedAcquisition::GitDev {
                    repository_origin: repository_origin.clone(),
                    repository: repository.clone(),
                    commit: commit.clone(),
                    subdirectory: subdirectory.clone(),
                    underlying_adapter: *underlying_adapter,
                    acquisition_digest: acquisition_digest.clone(),
                }),
            ),
            _ => return Err(integrity_error()),
        };
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
            source_url,
            expected_sha256,
            expected_size_bytes,
            platform_selector,
            now_ms: task.created_at_ms,
            operation: task.operation,
            expected_tree_digest: None,
            rebuild_uncommitted_version: false,
            external_acquisition,
        };
        let durable_steps = authorization.steps.clone();
        let snapshot_compensation =
            if let Some(step) = durable_steps.iter().find(|step| step.ordinal == 0) {
                step.compensation.clone()
            } else {
                let previous_inventory = authorization.inventory.clone();
                let previous_projection = authorization.projection.clone();
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
        let desired_enabled = previous_state
            .as_ref()
            .is_some_and(|state| state.default_enabled);
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
                self.authorize_effect(&task.task_id).await?;
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
                self.authorize_effect(&task.task_id).await?;
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
            effect.expected_tree_digest = authorization.inventory.as_ref().and_then(|inventory| {
                inventory
                    .managed
                    .versions
                    .iter()
                    .find(|version| version.version == plan.manifest_version())
                    .and_then(|version| version.materialized_tree_digest.clone())
            });
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
            self.authorize_effect(&task.task_id).await?;
            match distribution.inspect_installed(&effect, &cancellation).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    let _ = self
                        .repository
                        .release_artifact_claim(
                            &effect.expected_sha256,
                            &task.task_id,
                            self.clock.now_ms(),
                        )
                        .await;
                    return Err(error);
                }
            }
        } else {
            let now_ms = self.clock.now_ms();
            self.repository
                .claim_artifact(
                    &effect.expected_sha256,
                    &task.task_id,
                    now_ms,
                    now_ms - STALE_ARTIFACT_CLAIM_MS,
                )
                .await?;
            self.authorize_effect(&task.task_id).await?;
            let outcome = match distribution.install(&effect, &cancellation).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.repository
                        .release_artifact_claim(
                            &effect.expected_sha256,
                            &task.task_id,
                            self.clock.now_ms(),
                        )
                        .await?;
                    return Err(error);
                }
            };
            if let Err(error) = self
                .repository
                .mark_artifact_claim_verified(
                    &effect.expected_sha256,
                    &task.task_id,
                    self.clock.now_ms(),
                )
                .await
            {
                let _ = self
                    .repository
                    .release_artifact_claim(
                        &effect.expected_sha256,
                        &task.task_id,
                        self.clock.now_ms(),
                    )
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
                .release_artifact_claim(&effect.expected_sha256, &task.task_id, self.clock.now_ms())
                .await?;
            commit?;
            outcome
        };
        if materialize == TaskStepStatus::Committed {
            self.repository
                .release_artifact_claim(&effect.expected_sha256, &task.task_id, self.clock.now_ms())
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
        let expected_managed_revision = if inventory != TaskStepStatus::Committed {
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
            let revision = staged.record.managed.revision;
            self.commit_step(
                &task.task_id,
                2,
                StepEvidence::ManagedMcpUpserted {
                    managed_mcp_id: ids.managed_mcp_id.clone(),
                    created: staged.created_managed_mcp,
                },
            )
            .await?;
            Some(revision)
        } else {
            None
        };
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        let extension_config = self
            .ports
            .transport
            .extension_config(&outcome.projection, &ids.link_key)?;
        let extension_config_digest = digest_serializable(&extension_config)?;
        let projection_digest = crate::mcp_platform::projection_config_digest(&outcome.projection)?;
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
                projection_digest: extension_config_digest.clone(),
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
                    projection_digest: extension_config_digest.clone(),
                    enabled: false,
                    created: false,
                },
            )
            .await?;
        }
        self.cancel_boundary(&task.task_id, &cancellation).await?;
        #[cfg(windows)]
        let verified_activation = {
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
                self.ensure_desired_managed_enable_ready_or_disable(
                    desired_enabled,
                    &task.task_id,
                    &ids.managed_mcp_id,
                    &ids.link_key,
                    plan.manifest_digest(),
                    "managed MCP credential handle became unavailable before runtime activation",
                )
                .await?;
                self.authorize_effect(&task.task_id).await?;
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
            let activation = distribution
                .acquire_verified_runtime_activation(&ids.managed_mcp_id)
                .await?;
            if activation.projection() != &outcome.projection {
                return Err(integrity_error());
            }
            activation
        };
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
            #[cfg(windows)]
            let runtime_projection = verified_activation.projection();
            #[cfg(not(windows))]
            let runtime_projection = &outcome.projection;
            let effect = registration_effect_from_projection(runtime_projection)?;
            let runtime_extension_config = self
                .ports
                .transport
                .extension_config(runtime_projection, &ids.link_key)?;
            let current_authorization = self.authorize_effect(&task.task_id).await?;
            self.ports
                .registration
                .verify(&effect, &cancellation)
                .await?;
            let current_inventory = current_authorization
                .inventory
                .as_ref()
                .ok_or_else(integrity_error)?;
            let current_manifest = current_authorization.manifest.clone();
            let current_manifest_digest = current_manifest.verified.digest();
            if current_inventory.managed.managed_mcp_id != ids.managed_mcp_id
                || expected_managed_revision
                    .is_some_and(|revision| current_inventory.managed.revision != revision)
                || current_inventory
                    .lifecycle
                    .active_manifest_digest
                    .as_deref()
                    != Some(current_manifest_digest)
                || current_manifest_digest != manifest_record.verified.digest()
            {
                return Err(integrity_error());
            }
            let result = match self
                .managed_enrollment_runtime_status(
                    &current_manifest.verified.manifest().auth,
                    &ids.managed_mcp_id,
                    current_inventory.managed.revision,
                    current_manifest_digest,
                )
                .await
            {
                CredentialRuntimeStatus::Ready => {
                    self.ports
                        .health
                        .run(
                            HealthExecution {
                                effect,
                                check: manifest_record.verified.manifest().health_check.clone(),
                                projection_config: runtime_extension_config,
                            },
                            cancellation.clone(),
                        )
                        .await?
                }
                CredentialRuntimeStatus::Unconfigured
                | CredentialRuntimeStatus::ReRegistrationRequired
                | CredentialRuntimeStatus::TrustedStateConflict
                | CredentialRuntimeStatus::TemporarilyUnavailable => blocked_auth_result(),
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
        self.ensure_desired_managed_enable_ready_or_disable(
            desired_enabled,
            &task.task_id,
            &ids.managed_mcp_id,
            &ids.link_key,
            plan.manifest_digest(),
            "managed MCP credential handle became unavailable before activation",
        )
        .await?;
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
            self.ensure_desired_managed_enable_ready_or_disable(
                desired_enabled,
                &task.task_id,
                &ids.managed_mcp_id,
                &ids.link_key,
                plan.manifest_digest(),
                "managed MCP credential handle became unavailable before runtime activation",
            )
            .await?;
            self.authorize_effect(&task.task_id).await?;
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
            self.ensure_desired_managed_enable_ready_or_disable(
                desired_enabled,
                &task.task_id,
                &ids.managed_mcp_id,
                &ids.link_key,
                plan.manifest_digest(),
                "managed MCP credential handle became unavailable before installation activation",
            )
            .await?;
            self.repository
                .activate_managed_installation(ActivateManagedInstallation {
                    managed_mcp_id: &ids.managed_mcp_id,
                    target_version: plan.manifest_version(),
                    task_id: &task.task_id,
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
                    plan_id: &task.plan_id,
                    owner_task_id: &task.task_id,
                    worker_owner_id: &self.owner_id,
                    step_ordinal: 8,
                    step_token: &format!("{}:{}:8", task.task_id, task.attempt_count),
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
            self.ensure_desired_managed_enable_ready_or_disable(
                desired_enabled,
                &task.task_id,
                &ids.managed_mcp_id,
                &ids.link_key,
                plan.manifest_digest(),
                "managed MCP credential handle became unavailable before projection enablement",
            )
            .await?;
            let target_entry = crate::config::extensions::ExtensionEntry {
                enabled: desired_enabled,
                config: extension_config.clone(),
            };
            self.authorize_effect(&task.task_id).await?;
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
                    self.ensure_desired_managed_enable_ready_or_disable(
                        desired_enabled,
                        &task.task_id,
                        &ids.managed_mcp_id,
                        &ids.link_key,
                        plan.manifest_digest(),
                        "managed MCP credential handle became unavailable before projection enablement",
                    )
                    .await?;
                    self.authorize_effect(&task.task_id).await?;
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
                    ));
                }
                None if !desired_enabled => {
                    self.authorize_effect(&task.task_id).await?;
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
                    projection_digest: extension_config_digest.clone(),
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
                    self.authorize_effect(&task.task_id).await?;
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
                    self.authorize_effect(&task.task_id).await?;
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
                self.authorize_effect(&task.task_id).await?;
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
        authorization: ExecutionAuthorization,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        let task = authorization.task.clone();
        let distribution = self.distribution()?;
        let record = authorization.plan.clone();
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
        let durable_steps = authorization.steps.clone();
        let uninstall_snapshot =
            if let Some(step) = durable_steps.iter().find(|step| step.ordinal == 0) {
                step.compensation.clone()
            } else {
                let inventory = authorization
                    .inventory
                    .clone()
                    .ok_or_else(integrity_error)?;
                let projection = authorization
                    .projection
                    .clone()
                    .ok_or_else(integrity_error)?;
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
            self.authorize_effect(&task.task_id).await?;
            if let Some(current) = self.ports.projection_sink.get(&link_key).await? {
                if current.entry.config != config || (current.entry.enabled && !enabled) {
                    return Err(McpPlatformError::new(
                        McpPlatformErrorCode::ProjectionConflict,
                        "managed uninstall projection drifted",
                    ));
                }
                self.authorize_effect(&task.task_id).await?;
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
            self.authorize_effect(&task.task_id).await?;
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
                self.authorize_effect(&task.task_id).await?;
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
                self.authorize_effect(&task.task_id).await?;
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
        authorization: ExecutionAuthorization,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        let task = authorization.task.clone();
        let plan_record = authorization.plan.clone();
        let manifest_record = authorization.manifest.clone();
        let plan = &plan_record.plan;
        let manifest = manifest_record.verified.manifest();
        if matches!(
            manifest.distribution,
            super::manifest::Distribution::RemoteHttp
        ) && !matches!(manifest.auth, super::manifest::Auth::None)
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::CredentialMissing,
                "managed remote HTTP credentials are unavailable",
            ));
        }
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
        let stable_ids = register_stable_ids(
            plan.manifest_id(),
            plan_record.target.installation_scope.as_deref(),
        );
        let extension_config = self
            .ports
            .transport
            .extension_config(plan.connection_projection(), &stable_ids.link_key)?;
        let extension_config_digest = digest_serializable(&extension_config)?;
        let projection_digest =
            crate::mcp_platform::projection_config_digest(plan.connection_projection())?;

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
            self.authorize_effect(&task.task_id).await?;
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
                    plan_id: &task.plan_id,
                    owner_task_id: &task.task_id,
                    worker_owner_id: &self.owner_id,
                    step_ordinal: 2,
                    step_token: &format!("{}:{}:2", task.task_id, task.attempt_count),
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

        let config_authorization = self.authorize_effect(&task.task_id).await?;
        let existing_config_step = config_authorization
            .steps
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
                    projection_digest: extension_config_digest.clone(),
                    created_by_task: config_created_by_task,
                },
                self.ports.projection_sink.adapter_id(),
                self.ports.projection_sink.adapter_version(),
            )
            .await?;
        if config_status != TaskStepStatus::Committed {
            self.authorize_effect(&task.task_id).await?;
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
                    projection_digest: extension_config_digest.clone(),
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
                    projection_digest: extension_config_digest,
                    created_by_task: false,
                },
                self.ports.transport.adapter_id(),
                self.ports.transport.adapter_version(),
            )
            .await?;
        if verify_status != TaskStepStatus::Committed {
            let authorization = self.authorize_effect(&task.task_id).await?;
            let snapshot = self
                .ports
                .projection_sink
                .get(&stable_ids.link_key)
                .await?
                .ok_or_else(integrity_error)?;
            if snapshot.entry.enabled || snapshot.entry.config != extension_config {
                return Err(integrity_error());
            }
            let projection = authorization.projection.ok_or_else(integrity_error)?;
            if projection.managed_mcp_id != stable_ids.managed_mcp_id {
                return Err(integrity_error());
            }
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
        authorization: ExecutionAuthorization,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<()> {
        let task = authorization.task.clone();
        let request = authorization
            .health_request
            .clone()
            .ok_or_else(integrity_error)?;
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
            let current_authorization = self.authorize_effect(&task.task_id).await?;
            let current_request = current_authorization
                .health_request
                .clone()
                .ok_or_else(integrity_error)?;
            let inventory = current_authorization
                .inventory
                .clone()
                .ok_or_else(integrity_error)?;
            let manifest_digest = inventory
                .lifecycle
                .active_manifest_digest
                .as_deref()
                .ok_or_else(integrity_error)?;
            let manifest = current_authorization.manifest.clone();
            reject_managed_remote_http_auth(manifest.verified.manifest())?;
            let projection = current_authorization
                .projection
                .clone()
                .ok_or_else(integrity_error)?;
            let config = self
                .ports
                .transport
                .extension_config(&projection.projection, &projection.link_key)?;
            let effect = self.ports.transport.registration_effect(
                manifest.verified.manifest(),
                &current_authorization.plan.plan,
            )?;
            let result = match self
                .managed_enrollment_runtime_status(
                    &manifest.verified.manifest().auth,
                    &current_request.managed_mcp_id,
                    inventory.managed.revision,
                    manifest_digest,
                )
                .await
            {
                CredentialRuntimeStatus::Ready
                    if current_request.mode == HealthCheckMode::Registration =>
                {
                    registration_health(
                        self.ports.projection_sink.as_ref(),
                        &projection.link_key,
                        &config,
                        inventory.managed.state.default_enabled,
                    )
                    .await?
                }
                CredentialRuntimeStatus::Ready => {
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
                CredentialRuntimeStatus::Unconfigured
                | CredentialRuntimeStatus::ReRegistrationRequired
                | CredentialRuntimeStatus::TrustedStateConflict
                | CredentialRuntimeStatus::TemporarilyUnavailable => blocked_auth_result(),
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
        let authorization = self.authorize_effect(&task.task_id).await?;
        if let Some(step) = authorization
            .steps
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
                &self.owner_id,
                authorization.task.revision,
                self.clock.now_ms(),
            )
            .await?;
        if step.status == TaskStepStatus::NotStarted {
            let authorization = self.authorize_effect(&task.task_id).await?;
            return self
                .repository
                .transition_task_step(StepTransition {
                    task_id: &task.task_id,
                    ordinal,
                    owner_id: &self.owner_id,
                    expected_task_revision: authorization.task.revision,
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
        let authorization = self.authorize_effect(task_id).await?;
        let step = authorization
            .steps
            .iter()
            .find(|step| step.ordinal == ordinal)
            .ok_or_else(integrity_error)?;
        if step.status == TaskStepStatus::Committed {
            return Ok(());
        }
        let expected_task_revision = authorization.task.revision;
        self.repository
            .transition_task_step(StepTransition {
                task_id,
                ordinal,
                owner_id: &self.owner_id,
                expected_task_revision,
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
            || self.authorize_effect(task_id).await?.task.status == TaskStatus::Cancelling)
    }

    async fn cancel_without_effects(&self, task_id: &str) -> McpPlatformResult<()> {
        let task = self.authorize_effect(task_id).await?.task;
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
        let task = self
            .repository
            .authorize_execution(task_id, &self.owner_id, self.clock.now_ms())
            .await?
            .task;
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
                let (code, message) = task_error_boundary(&error);
                let redacted = RedactedError::new(code, message, std::iter::empty::<&str>());
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
            .authorize_effect(task_id)
            .await?
            .steps
            .iter()
            .any(|step| step.status != TaskStepStatus::NotStarted && step.ordinal > 0);
        if effect_may_have_occurred || task.status == TaskStatus::Cancelling {
            let (code, message) = task_error_boundary(&error);
            let redacted = RedactedError::new(code, message, std::iter::empty::<&str>());
            self.rollback(
                task_id,
                task.status == TaskStatus::Cancelling,
                Some(&redacted),
            )
            .await
        } else {
            let (code, message) = task_error_boundary(&error);
            let redacted = RedactedError::new(code, message, std::iter::empty::<&str>());
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
        let task = self.authorize_effect(task_id).await?.task;
        if task.status != TaskStatus::RollingBack {
            self.transition(task_id, TaskStatus::RollingBack, task.progress)
                .await?;
        }
        let rollback_authorization = self.authorize_effect(task_id).await?;
        let task = rollback_authorization.task.clone();
        let plan = rollback_authorization.plan.clone();
        let scope = plan.target.installation_scope.as_deref().unwrap_or("user");
        let ids = stable_ids(plan.plan.manifest_id(), scope);
        let steps = rollback_authorization.steps.clone();
        let mut incomplete = false;
        for step in steps
            .iter()
            .filter(|step| step.status != TaskStepStatus::NotStarted)
            .rev()
        {
            if step.compensation_status == CompensationStatus::Committed {
                continue;
            }
            if step.compensation_status == CompensationStatus::Pending {
                let authorization = self.authorize_effect(task_id).await?;
                if self
                    .repository
                    .transition_compensation(CompensationTransition {
                        task_id,
                        ordinal: step.ordinal,
                        owner_id: &self.owner_id,
                        expected_task_revision: authorization.task.revision,
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
            }
            let effect_authorization = self.authorize_effect(task_id).await?;
            let result = match &step.compensation {
                CompensationDescriptor::RemoveOwnedExtensionConfig {
                    link_key,
                    projection_digest,
                    created_by_task: true,
                } => {
                    let record = effect_authorization
                        .projection
                        .clone()
                        .ok_or_else(integrity_error)?;
                    if record.owner_task_id.as_deref() != Some(task_id)
                        || record.plan_id.as_deref() != Some(&task.plan_id)
                        || record.link_key != *link_key
                        || record.projection_digest != *projection_digest
                    {
                        return Err(integrity_error());
                    }
                    let expected = ProjectionSnapshot {
                        entry: crate::config::extensions::ExtensionEntry {
                            enabled: false,
                            config: self
                                .ports
                                .transport
                                .extension_config(&record.projection, link_key)?,
                        },
                        created: true,
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
                    .remove_owned_projection(RemoveOwnedProjection {
                        managed_mcp_id,
                        task_id,
                        worker_owner_id: &self.owner_id,
                        compensation_ordinal: step.ordinal,
                        now_ms: self.clock.now_ms(),
                    })
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
                        let previous_projection_enabled = steps
                            .iter()
                            .find_map(|candidate| match &candidate.compensation {
                                CompensationDescriptor::ManagedLifecycleSnapshot {
                                    managed_mcp_id: snapshot_managed_mcp_id,
                                    previous_state,
                                    ..
                                } if snapshot_managed_mcp_id == managed_mcp_id => {
                                    previous_state.as_ref().map(|state| state.default_enabled)
                                }
                                CompensationDescriptor::RestoreManagedProjection {
                                    managed_mcp_id: snapshot_managed_mcp_id,
                                    enabled,
                                    ..
                                } if snapshot_managed_mcp_id == managed_mcp_id => Some(*enabled),
                                _ => None,
                            })
                            .ok_or_else(integrity_error)?;
                        if previous_version.is_some() && previous_projection_enabled {
                            self.ensure_runtime_activation_restore_ready_or_disable(
                                task_id,
                                managed_mcp_id,
                            )
                            .await?;
                        }
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
                    let restore = RestoreOwnedProjection {
                        managed_mcp_id,
                        link_key,
                        projection,
                        plan_id,
                        manifest_digest,
                        owner_task_id: owner_task_id.as_deref(),
                        projection_digest,
                        replacing_task_id: task_id,
                        worker_owner_id: &self.owner_id,
                        compensation_ordinal: step.ordinal,
                        now_ms: self.clock.now_ms(),
                    };
                    self.repository
                        .validate_owned_connection_projection_restore(restore.clone())
                        .await?;
                    let manifest = effect_authorization.manifest.clone();
                    if manifest.verified.digest() != manifest_digest {
                        return Err(integrity_error());
                    }
                    reject_managed_remote_http_auth(manifest.verified.manifest())?;
                    if *enabled {
                        let inventory = effect_authorization
                            .inventory
                            .as_ref()
                            .ok_or_else(integrity_error)?;
                        if inventory.managed.managed_mcp_id != managed_mcp_id.as_str() {
                            return Err(integrity_error());
                        }
                        if let Err(error) = self
                            .ensure_managed_enable_ready(
                            &manifest.verified.manifest().auth,
                            managed_mcp_id,
                            inventory.managed.revision,
                            manifest_digest,
                            "managed MCP credential handle is unavailable for projection restoration",
                        )
                        .await
                        {
                            self.disable_managed_projection_and_runtime(
                                task_id,
                                managed_mcp_id,
                                Some(link_key),
                            )
                            .await?;
                            return Err(error);
                        }
                    }
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
                            let inventory = effect_authorization
                                .inventory
                                .as_ref()
                                .ok_or_else(integrity_error)?;
                            if let Err(error) = self
                                .ensure_managed_enable_ready(
                                &manifest.verified.manifest().auth,
                                managed_mcp_id,
                                inventory.managed.revision,
                                manifest_digest,
                                "managed MCP credential handle became unavailable before projection restoration",
                            )
                            .await
                            {
                                self.disable_managed_projection_and_runtime(
                                    task_id,
                                    managed_mcp_id,
                                    Some(link_key),
                                )
                                .await?;
                                return Err(error);
                            }
                            self.ports
                                .projection_sink
                                .set_enabled(link_key, true)
                                .await?;
                        } else if restored.entry.enabled {
                            return Err(integrity_error());
                        }
                    }
                    self.repository
                        .restore_owned_connection_projection(restore)
                        .await?;
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
            let authorization = self.authorize_effect(task_id).await?;
            if self
                .repository
                .transition_compensation(CompensationTransition {
                    task_id,
                    ordinal: step.ordinal,
                    owner_id: &self.owner_id,
                    expected_task_revision: authorization.task.revision,
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
        let current = self.authorize_effect(task_id).await?.task;
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
        let current = self.authorize_effect(task_id).await?.task;
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
        let current = self.authorize_effect(task_id).await?.task;
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

fn blocked_auth_result() -> super::lifecycle::HealthAdapterResult {
    super::lifecycle::HealthAdapterResult {
        result_code: HealthResultCode::BlockedAuth,
        latency_ms: 0,
        capabilities_digest: None,
        tools_digest: None,
        detail_code: HealthDetailCode::CredentialHandleMissing,
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

fn registration_effect_from_projection(
    projection: &super::plan::ConnectionProjection,
) -> McpPlatformResult<RegistrationEffect> {
    projection.require_runtime_transport()?;
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

fn register_stable_ids(mcp_id: &str, installation_scope: Option<&str>) -> StableIds {
    stable_ids(mcp_id, installation_scope.unwrap_or("user"))
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

fn executing_platform() -> Platform {
    if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::Macos
    } else {
        Platform::Linux
    }
}

fn executing_architecture() -> Architecture {
    if cfg!(target_arch = "aarch64") {
        Architecture::Aarch64
    } else {
        Architecture::X86_64
    }
}

fn plan_operation_for_task(operation: TaskOperation) -> PlanOperation {
    match operation {
        TaskOperation::Register => PlanOperation::Register,
        TaskOperation::Install => PlanOperation::Install,
        TaskOperation::Update => PlanOperation::Update,
        TaskOperation::Repair => PlanOperation::Repair,
        TaskOperation::Uninstall => PlanOperation::Uninstall,
        TaskOperation::Health => PlanOperation::Health,
    }
}

fn trusted_dispatch_adapter(distribution: &Distribution) -> TrustedPlanAdapter {
    match distribution {
        Distribution::RemoteHttp => TrustedPlanAdapter::RemoteHttp,
        Distribution::ManualStdio { .. } => TrustedPlanAdapter::ManualStdio,
        Distribution::Npm { .. } => TrustedPlanAdapter::Npm,
        Distribution::PythonWheel { .. } => TrustedPlanAdapter::PythonWheel,
        Distribution::BinaryArchive { .. } => TrustedPlanAdapter::BinaryArchive,
        Distribution::Docker { .. } => TrustedPlanAdapter::Docker,
        Distribution::GitDev { .. } => TrustedPlanAdapter::GitDev,
    }
}

const fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "MCP lifecycle journal failed integrity validation",
    )
}

const fn managed_storage_capacity_preflight_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityUnavailable,
        "managed storage capacity preflight is unavailable",
    )
}

fn preflight_rejected_error() -> RedactedError {
    RedactedError::new(
        RedactedErrorCode::VerificationFailed,
        "MCP task preflight rejected before execution",
        std::iter::empty::<&str>(),
    )
}

fn task_error_boundary(error: &McpPlatformError) -> (RedactedErrorCode, &'static str) {
    match error.code() {
        McpPlatformErrorCode::CredentialMissing => (
            RedactedErrorCode::AdapterFailed,
            "managed remote HTTP credentials are unavailable",
        ),
        McpPlatformErrorCode::IntegrityError | McpPlatformErrorCode::IntegrityUnavailable => (
            RedactedErrorCode::VerificationFailed,
            "MCP lifecycle authority failed integrity verification",
        ),
        _ => (
            RedactedErrorCode::AdapterFailed,
            "MCP lifecycle task failed at a typed execution boundary",
        ),
    }
}

const fn projection_recovery_required() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RollbackIncomplete,
        "managed MCP projection mutation requires recovery",
    )
}

const fn recovery_blocked() -> McpPlatformError {
    McpPlatformError::new(McpPlatformErrorCode::IntegrityError, "recovery_blocked")
}

const fn projection_witness_expired() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::ProjectionWitnessExpired,
        "stored projection witness no longer matches current authority",
    )
}

const fn projection_witness_consumed() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::ProjectionWitnessConsumed,
        "stored projection witness was already consumed",
    )
}

fn reject_managed_remote_http_auth(manifest: &super::manifest::Manifest) -> McpPlatformResult<()> {
    if matches!(
        manifest.distribution,
        super::manifest::Distribution::RemoteHttp
    ) && !matches!(manifest.auth, super::manifest::Auth::None)
    {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::CredentialMissing,
            "managed remote HTTP credentials are unavailable",
        ));
    }
    Ok(())
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
    use crate::mcp_platform::credential_runtime_gate::CredentialAuthorityRuntimeGateAdapter;
    use crate::mcp_platform::error::MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE;
    use crate::mcp_platform::lifecycle::{
        ConfigProjectionSink, CoreTransportProjectionAdapter, EmptyHostIntegrationAdapter,
        SafeRegistrationEffectAdapter,
    };
    use crate::mcp_platform::managed_remote::UnavailableRemoteHttpNetworkPolicy;
    use crate::mcp_platform::repository::InMemoryIntegritySigner;
    use crate::mcp_platform::repository::TaskStepRecord;
    use crate::mcp_platform::service::SystemClock;
    use crate::mcp_platform::task::CompensationStatus;
    use crate::mcp_platform::ProductionHealthCheckAdapter;
    use async_trait::async_trait;
    use std::sync::Arc;

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
    fn task_error_boundary_never_reuses_integrity_unavailable_detail() {
        let raw = "token=secret https://example.invalid/path {\"credential\":\"secret\"}";
        let error = McpPlatformError::new(McpPlatformErrorCode::IntegrityUnavailable, raw);

        let (code, message) = task_error_boundary(&error);

        assert_eq!(code, RedactedErrorCode::VerificationFailed);
        assert_eq!(
            message,
            "MCP lifecycle authority failed integrity verification"
        );
        assert!(!message.contains("secret"));
        assert!(!message.contains("example.invalid"));
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

    #[derive(Default)]
    struct NoopDistribution {
        capacity_checks: std::sync::atomic::AtomicU64,
    }

    #[async_trait]
    impl DistributionEffectAdapter for NoopDistribution {
        fn adapter_version(&self) -> &'static str {
            "1"
        }

        async fn check_managed_storage_capacity(
            &self,
            _required_peak_bytes: u64,
        ) -> McpPlatformResult<crate::mcp_platform::ManagedStorageCapacity> {
            self.capacity_checks
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(managed_storage_capacity_preflight_unavailable())
        }

        async fn install(
            &self,
            _effect: &ManagedInstallEffect,
            _cancellation: &CancellationToken,
        ) -> McpPlatformResult<crate::mcp_platform::ManagedInstallOutcome> {
            unreachable!("distribution lookup test should not execute installs")
        }

        async fn inspect_installed(
            &self,
            _effect: &ManagedInstallEffect,
            _cancellation: &CancellationToken,
        ) -> McpPlatformResult<crate::mcp_platform::ManagedInstallOutcome> {
            unreachable!("distribution lookup test should not inspect installs")
        }

        async fn version_exists(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
        ) -> McpPlatformResult<bool> {
            Ok(false)
        }

        async fn activate_version(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
            _task_id: &str,
            _cancellation: &CancellationToken,
        ) -> McpPlatformResult<()> {
            Ok(())
        }

        async fn restore_activation(
            &self,
            _managed_mcp_id: &str,
            _previous_version: Option<&str>,
            _task_id: &str,
        ) -> McpPlatformResult<()> {
            Ok(())
        }

        async fn active_version(&self, _managed_mcp_id: &str) -> McpPlatformResult<Option<String>> {
            Ok(None)
        }

        async fn remove_version(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
            _cancellation: &CancellationToken,
        ) -> McpPlatformResult<()> {
            Ok(())
        }

        async fn quarantine_version(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
            task_id: &str,
            _cancellation: &CancellationToken,
        ) -> McpPlatformResult<String> {
            Ok(format!("quarantine-{task_id}"))
        }

        async fn restore_quarantined(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
            _token: &str,
        ) -> McpPlatformResult<()> {
            Ok(())
        }

        async fn purge_quarantined(
            &self,
            _token: &str,
            _cancellation: &CancellationToken,
        ) -> McpPlatformResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn adapters_without_anchor_authority_cannot_issue_runtime_activation() {
        let error = NoopDistribution::default()
            .acquire_verified_runtime_activation("managed_unanchored")
            .await
            .expect_err("unanchored runtime activation must fail closed");
        assert_eq!(
            error.code(),
            McpPlatformErrorCode::RuntimeControlUnavailable
        );
    }

    async fn runner_fixture() -> TaskRunner {
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_url_with_integrity_signer(
                "sqlite::memory:",
                Arc::new(InMemoryIntegritySigner::new_for_testing([0x41; 32])),
            )
            .await
            .unwrap(),
        );
        let remote_http = Arc::new(UnavailableRemoteHttpNetworkPolicy);
        let ports = LifecyclePorts {
            registration: Arc::new(SafeRegistrationEffectAdapter::new(remote_http.clone())),
            host_integration: Arc::new(EmptyHostIntegrationAdapter),
            transport: Arc::new(CoreTransportProjectionAdapter),
            auth: Arc::new(CredentialAuthorityRuntimeGateAdapter::production_default()),
            health: Arc::new(ProductionHealthCheckAdapter::new(remote_http)),
            projection_sink: Arc::new(ConfigProjectionSink::default()),
        };
        TaskRunner::new(
            repository,
            Arc::new(SystemClock),
            ports,
            "tester".to_string(),
        )
    }

    #[tokio::test]
    async fn distribution_lookup_prefers_stable_storage_root_error() {
        let runner = runner_fixture()
            .await
            .with_distribution_adapter(Arc::new(NoopDistribution::default()))
            .with_distribution_unavailable_error(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE,
            ));

        let error = match runner.distribution() {
            Ok(_) => panic!("distribution lookup should fail closed on the stored root error"),
            Err(error) => error,
        };

        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityUnavailable);
        assert_eq!(error.message(), MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE);
    }

    #[tokio::test]
    async fn storage_capacity_preflight_never_calls_adapter_for_nonmanaged_acquisition() {
        let runner = runner_fixture().await;
        let no_op = Arc::new(NoopDistribution::default());
        let distribution: Arc<dyn DistributionEffectAdapter> = no_op.clone();
        let acquisition = PlanStep::AcquireGitDevDistribution {
            repository_origin: "https://example.invalid".to_string(),
            repository: "https://example.invalid/managed.git".to_string(),
            commit: "a".repeat(40),
            subdirectory: None,
            underlying_adapter: crate::mcp_platform::manifest::GitDevAdapter::Npm,
            acquisition_digest: "b".repeat(64),
        };

        runner
            .preflight_managed_storage_capacity(
                &distribution,
                &acquisition,
                ManagedCapacityContractStatus::Unknown,
            )
            .await
            .unwrap();

        assert_eq!(
            no_op
                .capacity_checks
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn credential_drift_fails_closed_before_enabled_managed_activation() {
        let runner = runner_fixture().await;
        let auth = Auth::Environment {
            environment_key: "MANAGED_MCP_TEST_TOKEN".to_string(),
            credential_name: "managed-mcp-test-token".to_string(),
        };
        let manifest_digest = "d".repeat(64);

        runner.set_enrollment_runtime_binding_resolver(
            StaticEnrollmentRuntimeBindingResolver::ready(
                "managed-test",
                7,
                &manifest_digest,
                &auth,
            ),
        );
        runner
            .ensure_managed_enable_ready(
                &auth,
                "managed-test",
                7,
                &manifest_digest,
                "managed MCP credential handle is unavailable",
            )
            .await
            .unwrap();

        runner.set_enrollment_runtime_binding_resolver(
            StaticEnrollmentRuntimeBindingResolver::missing(),
        );
        let error = runner
            .ensure_managed_enable_ready(
                &auth,
                "managed-test",
                7,
                &manifest_digest,
                "managed MCP credential handle is unavailable",
            )
            .await
            .unwrap_err();

        assert_eq!(error.code(), McpPlatformErrorCode::CredentialMissing);
    }

    #[test]
    fn windows_managed_local_health_cannot_create_a_direct_spawn_effect() {
        let projection = crate::mcp_platform::ConnectionProjection::ManagedDockerStdio {
            name: "managed-docker".to_string(),
            description: "managed docker".to_string(),
            executable: "docker".to_string(),
            args: Vec::new(),
            cwd: None,
            timeout_seconds: None,
        };

        #[cfg(windows)]
        {
            let error = registration_effect_from_projection(&projection).unwrap_err();
            assert_eq!(
                error.code(),
                McpPlatformErrorCode::RuntimeControlUnavailable
            );
        }

        #[cfg(not(windows))]
        assert!(matches!(
            registration_effect_from_projection(&projection),
            Ok(RegistrationEffect::ManualStdio { .. })
        ));
    }

    #[test]
    fn register_stable_id_defaults_missing_scope_to_user() {
        let mcp_id = "example.mcp";
        let missing_scope = None::<&str>;

        let missing_scope_ids = register_stable_ids(mcp_id, missing_scope);
        let user_scope_ids = stable_ids(mcp_id, "user");
        let system_scope_ids = stable_ids(mcp_id, "system");

        assert_eq!(
            missing_scope_ids.managed_mcp_id,
            user_scope_ids.managed_mcp_id
        );
        assert_eq!(missing_scope_ids.link_key, user_scope_ids.link_key);
        assert_ne!(
            user_scope_ids.managed_mcp_id,
            system_scope_ids.managed_mcp_id
        );
        assert_ne!(user_scope_ids.link_key, system_scope_ids.link_key);
    }
}
