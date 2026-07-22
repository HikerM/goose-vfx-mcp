use std::sync::Arc;
use std::sync::Mutex;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::mcp_platform::adapters::plan_for_manifest;
use crate::mcp_platform::catalog::{CatalogCompatibility, CompatibilityTarget};
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::lifecycle::{
    ConfigAuthRequirementResolver, ConfigProjectionSink, CoreTransportProjectionAdapter,
    EmptyHostIntegrationAdapter, LifecyclePorts, SafeRegistrationEffectAdapter,
};
use crate::mcp_platform::manifest::{Architecture, Distribution, Platform, VerifiedManifest};
use crate::mcp_platform::policy::{PlanOperation, PolicyContext};
use crate::mcp_platform::repository::{
    ConfirmationEvidence, CreateHealthTask, CreateTask, ManagedInventoryFilter,
    ManagedMcpInventoryRecord, PlanRecord, PlanTarget, SavePlan, TaskTransition,
};
use crate::mcp_platform::task::{
    AdapterEvidence, CompensationDescriptor, RollbackEvidence, RollbackStatus, TaskOperation,
    TaskStatus,
};
use crate::mcp_platform::{ProductionHealthCheckAdapter, TaskRunner};

use super::dependencies::{Clock, IdGenerator, SystemClock, UuidGenerator};
use super::dto::{
    unique_task_ids, CatalogDetail, CatalogListInput, CatalogLocator, CatalogPage, CatalogSummary,
    EventsResumeInput, EventsResumePage, HealthGetInput, HealthRunInput, HealthStatus,
    InstallConfirmInput, ManagedGetInput, ManagedListInput, ManagedMcpDetail, ManagedMcpPage,
    ManagedMcpSummary, PlanCreateInput, PlanIntent, PlanReview, RequestContext,
    SetDefaultEnabledInput, TaskCancelInput, TaskGetInput, TaskRef, TaskRetryInput, UserDecision,
    LOCAL_PERSISTED_SOURCE_ID,
};
use super::port::McpPlatformRepositoryPort;

const DEFAULT_PAGE_SIZE: u16 = 25;
const MAX_PAGE_SIZE: u16 = 100;
const DEFAULT_EVENT_LIMIT: u16 = 50;
const MAX_EVENT_LIMIT: u16 = 200;
const MAX_TASK_FILTERS: usize = 100;
const MAX_ID_LENGTH: usize = 256;
const MAX_QUERY_LENGTH: usize = 256;
const MAX_CURSOR_LENGTH: usize = 1024;

#[derive(Debug, Clone, Copy)]
pub struct McpPlatformServiceOptions {
    pub compatibility_target: CompatibilityTarget,
    pub plan_ttl_ms: i64,
}

impl Default for McpPlatformServiceOptions {
    fn default() -> Self {
        Self {
            compatibility_target: CompatibilityTarget {
                platform: current_platform(),
                arch: current_architecture(),
            },
            plan_ttl_ms: 15 * 60 * 1000,
        }
    }
}

pub struct McpPlatformService {
    repository: Arc<dyn McpPlatformRepositoryPort>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    options: McpPlatformServiceOptions,
    runner: Arc<TaskRunner>,
    auto_worker: bool,
    worker: Mutex<WorkerState>,
}

enum WorkerState {
    NotStarted,
    Running(tokio::task::JoinHandle<()>),
    Stopped,
}

impl McpPlatformService {
    pub fn production(repository: Arc<dyn McpPlatformRepositoryPort>) -> Self {
        Self::new_internal(
            repository,
            Arc::new(SystemClock),
            Arc::new(UuidGenerator),
            McpPlatformServiceOptions::default(),
            default_lifecycle_ports(),
            true,
        )
    }

    pub fn new(
        repository: Arc<dyn McpPlatformRepositoryPort>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
    ) -> Self {
        Self::new_internal(
            repository,
            clock,
            ids,
            options,
            default_lifecycle_ports(),
            false,
        )
    }

    pub fn new_with_lifecycle_ports(
        repository: Arc<dyn McpPlatformRepositoryPort>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
        ports: LifecyclePorts,
    ) -> Self {
        Self::new_internal(repository, clock, ids, options, ports, false)
    }

    fn new_internal(
        repository: Arc<dyn McpPlatformRepositoryPort>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        options: McpPlatformServiceOptions,
        ports: LifecyclePorts,
        auto_worker: bool,
    ) -> Self {
        let runner = Arc::new(TaskRunner::new(
            repository.clone(),
            clock.clone(),
            ports,
            ids.next_id("worker"),
        ));
        Self {
            repository,
            clock,
            ids,
            options,
            runner,
            auto_worker,
            worker: Mutex::new(WorkerState::NotStarted),
        }
    }

    pub fn start_worker_if_configured(&self) {
        if !self.auto_worker {
            return;
        }
        let mut state = self.worker.lock().expect("MCP worker state lock");
        if matches!(*state, WorkerState::NotStarted) {
            *state = WorkerState::Running(tokio::spawn(self.runner.clone().run()));
        }
    }

    pub async fn runner_tick(&self) -> McpPlatformResult<bool> {
        self.runner.tick().await
    }

    pub async fn shutdown_worker(&self) {
        let handle = {
            let mut state = self.worker.lock().expect("MCP worker state lock");
            match std::mem::replace(&mut *state, WorkerState::Stopped) {
                WorkerState::Running(handle) => Some(handle),
                WorkerState::NotStarted | WorkerState::Stopped => None,
            }
        };
        self.runner.shutdown();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    pub fn trusted_local_context(&self) -> RequestContext {
        RequestContext::local_authenticated_client(self.ids.next_id("correlation"))
    }

    pub async fn catalog_list(
        &self,
        _context: &RequestContext,
        input: CatalogListInput,
    ) -> McpPlatformResult<CatalogPage> {
        validate_optional_text(input.query.as_deref(), MAX_QUERY_LENGTH)?;
        validate_optional_text(input.cursor.as_deref(), MAX_CURSOR_LENGTH)?;
        for source_id in &input.source_ids {
            validate_identifier(source_id)?;
        }
        let page_size = validate_page_size(input.page_size)? as usize;
        let after = input.cursor.as_deref().map(decode_cursor).transpose()?;
        let normalized_query = input.query.as_deref().map(str::to_ascii_lowercase);
        let source_selected = input.source_ids.is_empty()
            || input
                .source_ids
                .iter()
                .any(|source| source == LOCAL_PERSISTED_SOURCE_ID);

        let mut records = self.repository.list_manifests().await?;
        records.sort_by(|left, right| manifest_key(left).cmp(&manifest_key(right)));
        let matching_snapshot = records
            .into_iter()
            .filter(|_| source_selected)
            .filter(|record| {
                input.trust_tiers.is_empty() || input.trust_tiers.contains(&record.trust_tier)
            })
            .filter(|record| {
                normalized_query.as_ref().is_none_or(|query| {
                    let manifest = record.verified.manifest();
                    manifest.id.to_ascii_lowercase().contains(query)
                        || manifest.name.to_ascii_lowercase().contains(query)
                        || manifest.description.to_ascii_lowercase().contains(query)
                })
            })
            .filter(|record| {
                input.compatibility.is_none_or(|expected| {
                    compatibility(&record.verified, self.options.compatibility_target) == expected
                })
            })
            .collect::<Vec<_>>();
        let newest_verified_at_ms = matching_snapshot
            .iter()
            .map(|record| record.created_at_ms)
            .max();
        let mut matching = matching_snapshot
            .into_iter()
            .filter(|record| {
                after
                    .as_ref()
                    .is_none_or(|cursor| manifest_key(record) > cursor.key())
            })
            .collect::<Vec<_>>();

        let has_more = matching.len() > page_size;
        matching.truncate(page_size);
        let next_cursor = has_more
            .then(|| matching.last().map(encode_cursor))
            .flatten()
            .transpose()?;
        let items = matching
            .into_iter()
            .map(|record| catalog_summary(record, self.options.compatibility_target))
            .collect();

        Ok(CatalogPage {
            items,
            next_cursor,
            offline: true,
            local_persistence_only: true,
            newest_verified_at_ms,
        })
    }

    pub async fn catalog_detail(
        &self,
        _context: &RequestContext,
        locator: CatalogLocator,
    ) -> McpPlatformResult<CatalogDetail> {
        let record = match locator {
            CatalogLocator::ManifestDigest(digest) => {
                validate_digest(&digest)?;
                self.repository.get_manifest(&digest).await?
            }
            CatalogLocator::CatalogRef {
                source_id,
                mcp_id,
                version,
            } => {
                validate_identifier(&source_id)?;
                validate_identifier(&mcp_id)?;
                validate_identifier(&version)?;
                if source_id != LOCAL_PERSISTED_SOURCE_ID {
                    return Err(not_found());
                }
                self.repository
                    .get_manifest_by_identity(&mcp_id, &version)
                    .await?
            }
        };
        Ok(CatalogDetail {
            source_id: LOCAL_PERSISTED_SOURCE_ID.to_string(),
            manifest_digest: record.verified.digest().to_string(),
            proof: record.proof,
            trust_tier: record.trust_tier,
            compatibility: compatibility(&record.verified, self.options.compatibility_target),
            manifest: record.verified.manifest().clone(),
            verified_at_ms: record.created_at_ms,
        })
    }

    pub async fn plan_create(
        &self,
        context: &RequestContext,
        input: PlanCreateInput,
    ) -> McpPlatformResult<PlanReview> {
        validate_idempotency_key(&input.idempotency_key)?;
        let (manifest_digest, installation_scope) = match &input.intent {
            PlanIntent::Register {
                manifest_digest,
                installation_scope,
            } => (manifest_digest.as_str(), installation_scope.as_str()),
            PlanIntent::Install { manifest_digest } => {
                validate_digest(manifest_digest)?;
                return Err(operation_not_supported());
            }
            PlanIntent::Update {
                managed_mcp_id,
                target_version,
            } => {
                validate_identifier(managed_mcp_id)?;
                validate_identifier(target_version)?;
                return Err(operation_not_supported());
            }
            PlanIntent::Repair { managed_mcp_id }
            | PlanIntent::Uninstall { managed_mcp_id, .. } => {
                validate_identifier(managed_mcp_id)?;
                return Err(operation_not_supported());
            }
        };
        validate_digest(manifest_digest)?;

        if let Some(existing) = self
            .repository
            .get_plan_by_idempotency_key(&input.idempotency_key)
            .await?
        {
            ensure_plan_matches(&existing, manifest_digest, installation_scope)?;
            return self.review_for_plan(existing).await;
        }

        let manifest = self.repository.get_manifest(manifest_digest).await?;
        let policy_context = PolicyContext::new(manifest.trust_tier, PlanOperation::Register);
        let plan = plan_for_manifest(&manifest.verified, &policy_context)?;
        let now_ms = self.clock.now_ms();
        let expires_at_ms = now_ms
            .checked_add(self.options.plan_ttl_ms)
            .ok_or_else(invalid_request)?;
        let target = PlanTarget {
            managed_mcp_id: None,
            mcp_id: plan.manifest_id().to_string(),
            version: plan.manifest_version().to_string(),
            installation_scope: Some(installation_scope.to_string()),
        };
        let save = SavePlan {
            plan_id: &self.ids.next_id("plan"),
            idempotency_key: &input.idempotency_key,
            plan: &plan,
            target: &target,
            policy_evidence: plan.policy(),
            confirmation_evidence: &ConfirmationEvidence::Pending,
            expires_at_ms,
            created_at_ms: now_ms,
            actor: context.actor(),
        };
        let stored = match self.repository.save_plan(save).await {
            Ok(stored) => stored,
            Err(error) if error.code() == McpPlatformErrorCode::PlanConflict => {
                let existing = self
                    .repository
                    .get_plan_by_idempotency_key(&input.idempotency_key)
                    .await?
                    .ok_or(error)?;
                ensure_plan_matches(&existing, manifest_digest, installation_scope)?;
                existing
            }
            Err(error) => return Err(error),
        };
        self.review_for_plan(stored).await
    }

    pub async fn install_confirm(
        &self,
        context: &RequestContext,
        input: InstallConfirmInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.plan_id)?;
        validate_digest(&input.plan_digest)?;
        validate_idempotency_key(&input.idempotency_key)?;
        let plan = self.repository.get_plan(&input.plan_id).await?;
        if plan.plan.plan_digest() != input.plan_digest {
            return Err(plan_stale());
        }

        if let Some(existing) = self
            .repository
            .get_task_by_idempotency_key(
                TaskOperation::from(plan.plan.operation()),
                &input.idempotency_key,
            )
            .await?
        {
            if existing.plan_id != input.plan_id || existing.plan_digest != input.plan_digest {
                return Err(idempotency_conflict());
            }
            match self.confirmation_decision(&existing).await? {
                Some(decision) if decision == input.decision => return Ok(existing.into()),
                Some(_) => return Err(idempotency_conflict()),
                None => {
                    let settled = self
                        .settle_confirmation(context, existing, input.decision)
                        .await?;
                    return Ok(settled.into());
                }
            }
        }
        if self.clock.now_ms() >= plan.expires_at_ms {
            return Err(plan_expired());
        }

        let task_id = self.ids.next_id("task");
        let adapter_evidence = AdapterEvidence {
            adapter_id: plan.plan.adapter().id.clone(),
            adapter_version: plan.plan.adapter().version.clone(),
            compatible_for_recovery: true,
            resume_safe: true,
        };
        let rollback_evidence = RollbackEvidence {
            compensation_available: true,
            remaining_compensations: vec![CompensationDescriptor::RemoveConnectionProjection {
                link_key: "server_owned_at_execution".to_string(),
            }],
        };
        let created = self
            .repository
            .create_task(CreateTask {
                task_id: &task_id,
                plan_id: &input.plan_id,
                plan_digest: &input.plan_digest,
                operation: TaskOperation::from(plan.plan.operation()),
                idempotency_key: &input.idempotency_key,
                actor: context.actor(),
                now_ms: self.clock.now_ms(),
                adapter_evidence: Some(&adapter_evidence),
                rollback_evidence: Some(&rollback_evidence),
            })
            .await?;
        let settled = self
            .settle_confirmation(context, created, input.decision)
            .await?;
        let task_ref = settled.into();
        self.runner.notify();
        Ok(task_ref)
    }

    pub async fn task_get(
        &self,
        _context: &RequestContext,
        input: TaskGetInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.task_id)?;
        self.repository
            .get_task(&input.task_id)
            .await
            .map(Into::into)
    }

    pub async fn task_cancel(
        &self,
        context: &RequestContext,
        input: TaskCancelInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.task_id)?;
        validate_revision(input.expected_revision)?;
        let task = self
            .repository
            .request_cancel(
                &input.task_id,
                input.expected_revision,
                context.actor(),
                self.clock.now_ms(),
            )
            .await
            .map(TaskRef::from)?;
        self.runner.cancel(&input.task_id).await;
        Ok(task)
    }

    pub async fn task_retry(
        &self,
        context: &RequestContext,
        input: TaskRetryInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.task_id)?;
        validate_revision(input.expected_revision)?;
        validate_idempotency_key(&input.idempotency_key)?;
        let task = self
            .repository
            .retry_task(
                &input.task_id,
                input.expected_revision,
                &input.idempotency_key,
                context.actor(),
                self.clock.now_ms(),
            )
            .await
            .map(TaskRef::from)?;
        self.runner.notify();
        Ok(task)
    }

    pub async fn managed_list(
        &self,
        _context: &RequestContext,
        input: ManagedListInput,
    ) -> McpPlatformResult<ManagedMcpPage> {
        validate_optional_text(input.cursor.as_deref(), MAX_CURSOR_LENGTH)?;
        let page_size = validate_page_size(input.page_size)? as usize;
        let records = self
            .repository
            .list_managed_inventory(
                input.cursor.as_deref(),
                page_size + 1,
                &ManagedInventoryFilter {
                    registration: input.registration,
                    installation: input.installation,
                    runtime: input.runtime,
                    health: input.health,
                    default_enabled: input.default_enabled,
                },
            )
            .await?;
        let has_more = records.len() > page_size;
        let scanned_cursor =
            has_more.then(|| records[page_size - 1].managed.managed_mcp_id.clone());
        let items = records
            .into_iter()
            .take(page_size)
            .map(managed_summary)
            .collect();
        Ok(ManagedMcpPage {
            items,
            next_cursor: scanned_cursor,
        })
    }

    pub async fn managed_get(
        &self,
        _context: &RequestContext,
        input: ManagedGetInput,
    ) -> McpPlatformResult<ManagedMcpDetail> {
        validate_identifier(&input.managed_mcp_id)?;
        let inventory = self
            .repository
            .get_managed_inventory(&input.managed_mcp_id)
            .await?;
        let projection = self
            .repository
            .get_connection_projection(&input.managed_mcp_id)
            .await?;
        let latest_health = self
            .repository
            .latest_health_observation(&input.managed_mcp_id)
            .await?;
        let registration_task = match inventory.lifecycle.owner_task_id.as_deref() {
            Some(task_id) => Some(self.repository.get_task(task_id).await?.into()),
            None => None,
        };
        Ok(ManagedMcpDetail {
            summary: managed_summary(inventory.clone()),
            distribution_adapter: inventory.lifecycle.distribution_adapter,
            active_manifest_digest: inventory
                .lifecycle
                .active_manifest_digest
                .ok_or_else(integrity_error)?,
            active_version: inventory
                .lifecycle
                .active_version
                .ok_or_else(integrity_error)?,
            extension_config_key: projection.link_key,
            projection_digest: projection.projection_digest,
            latest_health,
            registration_task,
        })
    }

    pub async fn health_run(
        &self,
        context: &RequestContext,
        input: HealthRunInput,
    ) -> McpPlatformResult<TaskRef> {
        validate_identifier(&input.managed_mcp_id)?;
        validate_idempotency_key(&input.idempotency_key)?;
        let inventory = self
            .repository
            .get_managed_inventory(&input.managed_mcp_id)
            .await?;
        if inventory.managed.state.registration
            != crate::mcp_platform::RegistrationState::Registered
        {
            return Err(invalid_transition());
        }
        let task = self
            .repository
            .create_health_task(CreateHealthTask {
                task_id: &self.ids.next_id("task"),
                managed_mcp_id: &input.managed_mcp_id,
                mode: input.mode,
                idempotency_key: &input.idempotency_key,
                actor: context.actor(),
                now_ms: self.clock.now_ms(),
            })
            .await?;
        self.runner.notify();
        Ok(task.into())
    }

    pub async fn health_get(
        &self,
        _context: &RequestContext,
        input: HealthGetInput,
    ) -> McpPlatformResult<HealthStatus> {
        validate_identifier(&input.managed_mcp_id)?;
        let inventory = self
            .repository
            .get_managed_inventory(&input.managed_mcp_id)
            .await?;
        Ok(HealthStatus {
            managed_mcp_id: input.managed_mcp_id.clone(),
            state: inventory.managed.state.health,
            latest: self
                .repository
                .latest_health_observation(&input.managed_mcp_id)
                .await?,
        })
    }

    pub async fn set_default_enabled(
        &self,
        _context: &RequestContext,
        input: SetDefaultEnabledInput,
    ) -> McpPlatformResult<ManagedMcpSummary> {
        validate_identifier(&input.managed_mcp_id)?;
        validate_revision(input.expected_revision)?;
        self.runner
            .set_default_enabled(
                &input.managed_mcp_id,
                input.expected_revision,
                input.enabled,
            )
            .await
            .map(managed_summary)
    }

    pub async fn events_resume(
        &self,
        _context: &RequestContext,
        input: EventsResumeInput,
    ) -> McpPlatformResult<EventsResumePage> {
        let after_event_id = input.after_event_id.unwrap_or_default();
        if after_event_id < 0 {
            return Err(invalid_request());
        }
        if input.task_ids.len() > MAX_TASK_FILTERS {
            return Err(invalid_request());
        }
        for task_id in &input.task_ids {
            validate_identifier(task_id)?;
        }
        let limit = validate_event_limit(input.limit)? as usize;
        let page = self
            .repository
            .list_global_audit_events(after_event_id, limit, &input.task_ids)
            .await?;
        let task_ids = unique_task_ids(&page.events, &input.task_ids);
        let mut tasks = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            tasks.push(self.repository.get_task(&task_id).await?.into());
        }
        Ok(EventsResumePage {
            events: page.events,
            next_event_id: page.scanned_through_event_id.max(after_event_id),
            tasks,
        })
    }

    async fn review_for_plan(&self, plan: PlanRecord) -> McpPlatformResult<PlanReview> {
        let manifest = self
            .repository
            .get_manifest(plan.plan.manifest_digest())
            .await?;
        Ok(PlanReview {
            plan_id: plan.plan_id,
            plan_digest: plan.plan.plan_digest().to_string(),
            expires_at_ms: plan.expires_at_ms,
            source_id: LOCAL_PERSISTED_SOURCE_ID.to_string(),
            proof: manifest.proof,
            trust_tier: manifest.trust_tier,
            manifest: manifest.verified.manifest().clone(),
            plan: plan.plan,
            target: plan.target,
            policy: plan.policy_evidence,
        })
    }

    async fn settle_confirmation(
        &self,
        context: &RequestContext,
        mut task: crate::mcp_platform::repository::TaskRecord,
        decision: UserDecision,
    ) -> McpPlatformResult<crate::mcp_platform::repository::TaskRecord> {
        for _ in 0..8 {
            let result = match task.status {
                TaskStatus::Planned => {
                    self.repository
                        .transition_task(TaskTransition {
                            task_id: &task.task_id,
                            expected_revision: task.revision,
                            next_status: TaskStatus::AwaitingConfirmation,
                            actor: context.actor(),
                            now_ms: self.clock.now_ms(),
                            heartbeat_at_ms: None,
                            progress: 0,
                            redacted_error: None,
                            rollback_status: RollbackStatus::NotRequired,
                            rollback_evidence: None,
                        })
                        .await
                }
                TaskStatus::AwaitingConfirmation => match decision {
                    UserDecision::Confirm => {
                        self.repository
                            .confirm_task(
                                &task.task_id,
                                task.revision,
                                context.actor(),
                                self.clock.now_ms(),
                            )
                            .await
                    }
                    UserDecision::Reject => {
                        self.repository
                            .request_cancel(
                                &task.task_id,
                                task.revision,
                                context.actor(),
                                self.clock.now_ms(),
                            )
                            .await
                    }
                },
                TaskStatus::Queued if decision == UserDecision::Confirm => return Ok(task),
                TaskStatus::Cancelled if decision == UserDecision::Reject => return Ok(task),
                TaskStatus::Queued | TaskStatus::Cancelled => return Err(idempotency_conflict()),
                _ => return Err(invalid_transition()),
            };
            match result {
                Ok(updated) => task = updated,
                Err(error) if error.code() == McpPlatformErrorCode::RevisionConflict => {
                    task = self.repository.get_task(&task.task_id).await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(revision_conflict())
    }

    async fn confirmation_decision(
        &self,
        task: &crate::mcp_platform::repository::TaskRecord,
    ) -> McpPlatformResult<Option<UserDecision>> {
        let events = self.repository.list_audit_events(&task.task_id).await?;
        let confirmed = events.iter().any(|event| {
            event.event_type
                == crate::mcp_platform::repository::AuditEventType::ConfirmationRecorded
        });
        let rejected = events.iter().any(|event| {
            event.event_type
                == crate::mcp_platform::repository::AuditEventType::CancellationRequested
                && !confirmed
        });
        match (confirmed, rejected) {
            (true, _) => Ok(Some(UserDecision::Confirm)),
            (false, true) => Ok(Some(UserDecision::Reject)),
            (false, false) => Ok(None),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogCursor {
    mcp_id: String,
    version: String,
    digest: String,
}

impl CatalogCursor {
    fn key(&self) -> (&str, &str, &str) {
        (&self.mcp_id, &self.version, &self.digest)
    }
}

fn encode_cursor(
    record: &crate::mcp_platform::repository::ManifestRecord,
) -> McpPlatformResult<String> {
    let cursor = CatalogCursor {
        mcp_id: record.verified.manifest().id.clone(),
        version: record.verified.manifest().version.as_str().to_string(),
        digest: record.verified.digest().to_string(),
    };
    serde_json::to_vec(&cursor)
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| invalid_request())
}

fn decode_cursor(value: &str) -> McpPlatformResult<CatalogCursor> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid_request())?;
    let cursor: CatalogCursor = serde_json::from_slice(&bytes).map_err(|_| invalid_request())?;
    validate_identifier(&cursor.mcp_id)?;
    validate_identifier(&cursor.version)?;
    validate_digest(&cursor.digest)?;
    Ok(cursor)
}

fn catalog_summary(
    record: crate::mcp_platform::repository::ManifestRecord,
    target: CompatibilityTarget,
) -> CatalogSummary {
    let manifest = record.verified.manifest();
    CatalogSummary {
        source_id: LOCAL_PERSISTED_SOURCE_ID.to_string(),
        manifest_digest: record.verified.digest().to_string(),
        mcp_id: manifest.id.clone(),
        version: manifest.version.as_str().to_string(),
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        publisher_id: manifest.publisher.id.clone(),
        publisher_name: manifest.publisher.name.clone(),
        trust_tier: record.trust_tier,
        proof: record.proof,
        compatibility: compatibility(&record.verified, target),
        distribution_adapter: manifest.distribution.adapter_id().to_string(),
        verified_at_ms: record.created_at_ms,
    }
}

fn managed_summary(record: ManagedMcpInventoryRecord) -> ManagedMcpSummary {
    ManagedMcpSummary {
        managed_mcp_id: record.managed.managed_mcp_id,
        mcp_id: record.managed.mcp_id,
        installation_scope: record.managed.installation_scope,
        registration: record.managed.state.registration,
        installation: record.managed.state.installation,
        runtime: record.managed.state.runtime,
        health: record.managed.state.health,
        default_enabled: record.managed.state.default_enabled,
        revision: record.managed.revision,
        updated_at_ms: record.managed.updated_at_ms,
    }
}

fn manifest_key(record: &crate::mcp_platform::repository::ManifestRecord) -> (&str, &str, &str) {
    (
        &record.verified.manifest().id,
        record.verified.manifest().version.as_str(),
        record.verified.digest(),
    )
}

fn compatibility(manifest: &VerifiedManifest, target: CompatibilityTarget) -> CatalogCompatibility {
    match &manifest.manifest().distribution {
        Distribution::ManualStdio { platforms, .. } => {
            if platforms.is_empty()
                || platforms.contains(&Platform::Any)
                || platforms.contains(&target.platform)
            {
                CatalogCompatibility::Compatible
            } else {
                CatalogCompatibility::Incompatible
            }
        }
        Distribution::Npm { artifacts, .. }
        | Distribution::PythonWheel { artifacts, .. }
        | Distribution::BinaryArchive { artifacts, .. } => {
            if artifacts.iter().any(|artifact| {
                (artifact.platform == Platform::Any || artifact.platform == target.platform)
                    && (artifact.arch == Architecture::Any || artifact.arch == target.arch)
            }) {
                CatalogCompatibility::Compatible
            } else {
                CatalogCompatibility::Incompatible
            }
        }
        Distribution::RemoteHttp | Distribution::Docker { .. } | Distribution::GitDev { .. } => {
            CatalogCompatibility::Compatible
        }
    }
}

fn ensure_plan_matches(
    plan: &PlanRecord,
    manifest_digest: &str,
    installation_scope: &str,
) -> McpPlatformResult<()> {
    if plan.plan.operation() == PlanOperation::Register
        && plan.plan.manifest_digest() == manifest_digest
        && plan.target.installation_scope.as_deref() == Some(installation_scope)
    {
        Ok(())
    } else {
        Err(idempotency_conflict())
    }
}

fn validate_page_size(value: Option<u16>) -> McpPlatformResult<u16> {
    let value = value.unwrap_or(DEFAULT_PAGE_SIZE);
    if (1..=MAX_PAGE_SIZE).contains(&value) {
        Ok(value)
    } else {
        Err(invalid_request())
    }
}

fn validate_event_limit(value: Option<u16>) -> McpPlatformResult<u16> {
    let value = value.unwrap_or(DEFAULT_EVENT_LIMIT);
    if (1..=MAX_EVENT_LIMIT).contains(&value) {
        Ok(value)
    } else {
        Err(invalid_request())
    }
}

fn validate_optional_text(value: Option<&str>, max: usize) -> McpPlatformResult<()> {
    if value.is_some_and(|value| value.len() > max) {
        Err(invalid_request())
    } else {
        Ok(())
    }
}

fn validate_identifier(value: &str) -> McpPlatformResult<()> {
    if value.is_empty() || value.len() > MAX_ID_LENGTH || value.chars().any(char::is_control) {
        Err(invalid_request())
    } else {
        Ok(())
    }
}

fn validate_idempotency_key(value: &str) -> McpPlatformResult<()> {
    validate_identifier(value)
}

fn validate_digest(value: &str) -> McpPlatformResult<()> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(invalid_request())
    }
}

fn validate_revision(value: i64) -> McpPlatformResult<()> {
    if value >= 0 {
        Ok(())
    } else {
        Err(invalid_request())
    }
}

const fn current_platform() -> Platform {
    if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::Macos
    } else {
        Platform::Linux
    }
}

const fn current_architecture() -> Architecture {
    if cfg!(target_arch = "aarch64") {
        Architecture::Aarch64
    } else {
        Architecture::X86_64
    }
}

const fn invalid_request() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidRequest,
        "MCP platform request is invalid",
    )
}

const fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "stored MCP platform data failed integrity validation",
    )
}

const fn not_found() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::NotFound,
        "MCP platform record was not found",
    )
}

const fn operation_not_supported() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::OperationNotSupported,
        "operation is not supported in phase 2C",
    )
}

const fn plan_stale() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PlanStale,
        "installation plan digest no longer matches",
    )
}

const fn plan_expired() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PlanExpired,
        "installation plan has expired",
    )
}

const fn idempotency_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IdempotencyConflict,
        "idempotency key already refers to different content",
    )
}

const fn invalid_transition() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidTransition,
        "task state does not permit this operation",
    )
}

const fn revision_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RevisionConflict,
        "record revision no longer matches",
    )
}

fn default_lifecycle_ports() -> LifecyclePorts {
    LifecyclePorts {
        registration: Arc::new(SafeRegistrationEffectAdapter),
        host_integration: Arc::new(EmptyHostIntegrationAdapter),
        transport: Arc::new(CoreTransportProjectionAdapter),
        auth: Arc::new(ConfigAuthRequirementResolver),
        health: Arc::new(ProductionHealthCheckAdapter),
        projection_sink: Arc::new(ConfigProjectionSink::default()),
    }
}

impl Drop for McpPlatformService {
    fn drop(&mut self) {
        self.runner.shutdown();
        if let Ok(WorkerState::Running(handle)) = self.worker.get_mut() {
            handle.abort();
        }
    }
}
