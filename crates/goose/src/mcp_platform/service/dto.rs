use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::mcp_platform::catalog::CatalogCompatibility;
use crate::mcp_platform::domain::TrustTier;
use crate::mcp_platform::domain::{
    HealthCheckMode, HealthState, InstallationState, RegistrationState, RuntimeState,
};
use crate::mcp_platform::manifest::{Manifest, ManifestProof};
use crate::mcp_platform::plan::InstallationPlan;
use crate::mcp_platform::policy::PolicyDecision;
use crate::mcp_platform::repository::{
    AuditEventRecord, HealthObservationRecord, PlanTarget, TaskRecord,
};

pub const LOCAL_PERSISTED_SOURCE_ID: &str = "local_persistence";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    actor: &'static str,
    correlation_id: String,
}

impl RequestContext {
    pub fn local_authenticated_client(correlation_id: String) -> Self {
        Self {
            actor: "local_authenticated_client",
            correlation_id,
        }
    }

    pub const fn actor(&self) -> &'static str {
        self.actor
    }

    pub fn correlation_id(&self) -> &str {
        &self.correlation_id
    }
}

#[derive(Debug, Clone, Default)]
pub struct CatalogListInput {
    pub query: Option<String>,
    pub trust_tiers: Vec<TrustTier>,
    pub source_ids: Vec<String>,
    pub compatibility: Option<CatalogCompatibility>,
    pub cursor: Option<String>,
    pub page_size: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSummary {
    pub source_id: String,
    pub manifest_digest: String,
    pub mcp_id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub publisher_id: String,
    pub publisher_name: String,
    pub trust_tier: TrustTier,
    pub proof: ManifestProof,
    pub compatibility: CatalogCompatibility,
    pub distribution_adapter: String,
    pub verified_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPage {
    pub items: Vec<CatalogSummary>,
    pub next_cursor: Option<String>,
    pub offline: bool,
    pub local_persistence_only: bool,
    pub newest_verified_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogLocator {
    ManifestDigest(String),
    CatalogRef {
        source_id: String,
        mcp_id: String,
        version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogDetail {
    pub source_id: String,
    pub manifest_digest: String,
    pub proof: ManifestProof,
    pub trust_tier: TrustTier,
    pub compatibility: CatalogCompatibility,
    pub manifest: Manifest,
    pub verified_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationScope {
    User,
}

impl InstallationScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanIntent {
    Register {
        manifest_digest: String,
        installation_scope: InstallationScope,
    },
    Install {
        manifest_digest: String,
    },
    Update {
        managed_mcp_id: String,
        target_version: String,
    },
    Repair {
        managed_mcp_id: String,
    },
    Uninstall {
        managed_mcp_id: String,
        preserve_user_data: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCreateInput {
    pub intent: PlanIntent,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanReview {
    pub plan_id: String,
    pub plan_digest: String,
    pub expires_at_ms: i64,
    pub source_id: String,
    pub proof: ManifestProof,
    pub trust_tier: TrustTier,
    pub manifest: Manifest,
    pub plan: InstallationPlan,
    pub target: PlanTarget,
    pub policy: PolicyDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserDecision {
    Confirm,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallConfirmInput {
    pub plan_id: String,
    pub plan_digest: String,
    pub decision: UserDecision,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskGetInput {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCancelInput {
    pub task_id: String,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRetryInput {
    pub task_id: String,
    pub expected_revision: i64,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedListInput {
    pub cursor: Option<String>,
    pub page_size: Option<u16>,
    pub registration: Option<RegistrationState>,
    pub installation: Option<InstallationState>,
    pub runtime: Option<RuntimeState>,
    pub health: Option<HealthState>,
    pub default_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedMcpSummary {
    pub managed_mcp_id: String,
    pub mcp_id: String,
    pub installation_scope: String,
    pub registration: RegistrationState,
    pub installation: InstallationState,
    pub runtime: RuntimeState,
    pub health: HealthState,
    pub default_enabled: bool,
    pub revision: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedMcpPage {
    pub items: Vec<ManagedMcpSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedGetInput {
    pub managed_mcp_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedMcpDetail {
    pub summary: ManagedMcpSummary,
    pub distribution_adapter: String,
    pub active_manifest_digest: String,
    pub active_version: String,
    pub extension_config_key: String,
    pub projection_digest: String,
    pub latest_health: Option<HealthObservationRecord>,
    pub registration_task: Option<TaskRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthRunInput {
    pub managed_mcp_id: String,
    pub mode: HealthCheckMode,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthGetInput {
    pub managed_mcp_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthStatus {
    pub managed_mcp_id: String,
    pub state: HealthState,
    pub latest: Option<HealthObservationRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetDefaultEnabledInput {
    pub managed_mcp_id: String,
    pub enabled: bool,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRef {
    pub task_id: String,
    pub operation: crate::mcp_platform::task::TaskOperation,
    pub status: crate::mcp_platform::task::TaskStatus,
    pub progress: u8,
    pub cancellable: bool,
    pub revision: i64,
    pub updated_at_ms: i64,
}

impl From<TaskRecord> for TaskRef {
    fn from(task: TaskRecord) -> Self {
        Self {
            cancellable: matches!(
                task.status,
                crate::mcp_platform::task::TaskStatus::AwaitingConfirmation
                    | crate::mcp_platform::task::TaskStatus::Queued
                    | crate::mcp_platform::task::TaskStatus::Running
            ),
            task_id: task.task_id,
            operation: task.operation,
            status: task.status,
            progress: task.progress,
            revision: task.revision,
            updated_at_ms: task.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventsResumeInput {
    pub after_event_id: Option<i64>,
    pub limit: Option<u16>,
    pub task_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventsResumePage {
    pub events: Vec<AuditEventRecord>,
    pub next_event_id: i64,
    pub tasks: Vec<TaskRef>,
}

pub(crate) fn unique_task_ids(events: &[AuditEventRecord], requested: &[String]) -> Vec<String> {
    requested
        .iter()
        .cloned()
        .chain(events.iter().map(|event| event.task_id.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
