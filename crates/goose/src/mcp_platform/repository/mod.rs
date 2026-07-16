pub mod sqlite;

use serde::{Deserialize, Serialize};

use super::domain::{ManagedMcpState, TrustTier};
use super::manifest::{ManifestProof, VerifiedManifest};
use super::plan::{ConnectionProjection, InstallationPlan};
use super::policy::PolicyDecision;
use super::task::{
    AdapterEvidence, CompensationDescriptor, RecoveryDecision, RedactedError, RollbackEvidence,
    RollbackStatus, StepEvidence, TaskOperation, TaskStatus, TaskStepStatus,
};

pub use sqlite::{DatabaseDiagnostics, SqliteMcpPlatformRepository};

#[derive(Debug, Clone)]
pub struct ManifestRecord {
    pub verified: VerifiedManifest,
    pub proof: ManifestProof,
    pub trust_tier: TrustTier,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    IdempotentReplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedVersionRecord {
    pub version: String,
    pub manifest_digest: String,
    pub installation_root: Option<String>,
    pub verified: bool,
    pub active: bool,
    pub adapter_evidence: Option<AdapterEvidence>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedMcpRecord {
    pub managed_mcp_id: String,
    pub mcp_id: String,
    pub installation_scope: String,
    pub state: ManagedMcpState,
    pub revision: i64,
    pub versions: Vec<ManagedVersionRecord>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewManagedMcp {
    pub managed_mcp_id: String,
    pub mcp_id: String,
    pub installation_scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionProjectionRecord {
    pub managed_mcp_id: String,
    pub link_key: String,
    pub projection: ConnectionProjection,
    pub revision: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanTarget {
    pub managed_mcp_id: Option<String>,
    pub mcp_id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfirmationEvidence {
    NotRequired,
    Pending,
    Confirmed { actor: String, confirmed_at_ms: i64 },
}

#[derive(Debug, Clone)]
pub struct SavePlan<'a> {
    pub plan_id: &'a str,
    pub idempotency_key: &'a str,
    pub plan: &'a InstallationPlan,
    pub target: &'a PlanTarget,
    pub policy_evidence: &'a PolicyDecision,
    pub confirmation_evidence: &'a ConfirmationEvidence,
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
    pub actor: &'a str,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRecord {
    pub plan_id: String,
    pub idempotency_key: String,
    pub envelope_digest: String,
    pub plan: InstallationPlan,
    pub target: PlanTarget,
    pub policy_evidence: PolicyDecision,
    pub confirmation_evidence: ConfirmationEvidence,
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
    pub actor: String,
}

#[derive(Debug, Clone)]
pub struct CreateTask<'a> {
    pub task_id: &'a str,
    pub plan_id: &'a str,
    pub plan_digest: &'a str,
    pub operation: TaskOperation,
    pub idempotency_key: &'a str,
    pub actor: &'a str,
    pub now_ms: i64,
    pub adapter_evidence: Option<&'a AdapterEvidence>,
    pub rollback_evidence: Option<&'a RollbackEvidence>,
}

#[derive(Debug, Clone)]
pub struct TaskTransition<'a> {
    pub task_id: &'a str,
    pub expected_revision: i64,
    pub next_status: TaskStatus,
    pub actor: &'a str,
    pub now_ms: i64,
    pub heartbeat_at_ms: Option<i64>,
    pub progress: u8,
    pub redacted_error: Option<&'a RedactedError>,
    pub rollback_status: RollbackStatus,
    pub rollback_evidence: Option<&'a RollbackEvidence>,
}

#[derive(Debug, Clone)]
pub struct StepTransition<'a> {
    pub task_id: &'a str,
    pub ordinal: i64,
    pub expected_status: TaskStepStatus,
    pub next_status: TaskStepStatus,
    pub evidence: Option<&'a StepEvidence>,
    pub actor: &'a str,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRecord {
    pub task_id: String,
    pub plan_id: String,
    pub plan_digest: String,
    pub operation: TaskOperation,
    pub idempotency_key: String,
    pub status: TaskStatus,
    pub actor: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub heartbeat_at_ms: Option<i64>,
    pub progress: u8,
    pub step_cursor: i64,
    pub adapter_evidence: Option<AdapterEvidence>,
    pub redacted_error: Option<RedactedError>,
    pub rollback_status: RollbackStatus,
    pub rollback_evidence: Option<RollbackEvidence>,
    pub revision: i64,
    pub event_sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskStepRecord {
    pub task_id: String,
    pub ordinal: i64,
    pub status: TaskStepStatus,
    pub idempotency_token: String,
    pub compensation: CompensationDescriptor,
    pub evidence: Option<StepEvidence>,
    pub started_at_ms: Option<i64>,
    pub committed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    TaskCreated,
    TaskStatusChanged,
    StepStarted,
    StepCommitted,
    RecoveryInterrupted,
    ConfirmationRecorded,
    CancellationRequested,
    RetryQueued,
}

impl AuditEventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskCreated => "task_created",
            Self::TaskStatusChanged => "task_status_changed",
            Self::StepStarted => "step_started",
            Self::StepCommitted => "step_committed",
            Self::RecoveryInterrupted => "recovery_interrupted",
            Self::ConfirmationRecorded => "confirmation_recorded",
            Self::CancellationRequested => "cancellation_requested",
            Self::RetryQueued => "retry_queued",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuditPayload {
    TaskCreated {
        status: TaskStatus,
    },
    TaskStatusChanged {
        from: TaskStatus,
        to: TaskStatus,
    },
    ConfirmationRecorded {
        from: TaskStatus,
        to: TaskStatus,
        plan_id: String,
        plan_digest: String,
    },
    CancellationRequested {
        from: TaskStatus,
        to: TaskStatus,
    },
    StepStatusChanged {
        ordinal: i64,
        from: TaskStepStatus,
        to: TaskStepStatus,
    },
    RecoveryDecision {
        decision: RecoveryDecision,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEventRecord {
    pub event_id: i64,
    pub task_id: String,
    pub sequence: i64,
    pub event_type: AuditEventType,
    pub actor: String,
    pub occurred_at_ms: i64,
    pub payload: AuditPayload,
    pub redacted_error: Option<RedactedError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRecord {
    pub task: TaskRecord,
    pub decision: RecoveryDecision,
}
