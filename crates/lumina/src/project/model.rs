use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProfile {
    pub manifests: Vec<String>,
    pub detected_stacks: Vec<String>,
    pub git_repository: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: PathBuf,
    pub canonical_root: PathBuf,
    pub profile: ProjectProfile,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_opened_at_ms: i64,
    pub archived_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemStatus {
    #[default]
    Draft,
    Planned,
    Running,
    Validating,
    Review,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
}

impl WorkItemStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Planned => "planned",
            Self::Running => "running",
            Self::Validating => "validating",
            Self::Review => "review",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "draft" => Ok(Self::Draft),
            "planned" => Ok(Self::Planned),
            "running" => Ok(Self::Running),
            "validating" => Ok(Self::Validating),
            "review" => Ok(Self::Review),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            "blocked" => Ok(Self::Blocked),
            _ => anyhow::bail!("invalid work item status"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub status: WorkItemStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            _ => anyhow::bail!("invalid run status"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    #[default]
    Discovering,
    Planning,
    AwaitingApproval,
    Executing,
    Validating,
    Reviewing,
    Complete,
}

impl RunPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovering => "discovering",
            Self::Planning => "planning",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Executing => "executing",
            Self::Validating => "validating",
            Self::Reviewing => "reviewing",
            Self::Complete => "complete",
        }
    }

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "discovering" => Ok(Self::Discovering),
            "planning" => Ok(Self::Planning),
            "awaiting_approval" => Ok(Self::AwaitingApproval),
            "executing" => Ok(Self::Executing),
            "validating" => Ok(Self::Validating),
            "reviewing" => Ok(Self::Reviewing),
            "complete" => Ok(Self::Complete),
            _ => anyhow::bail!("invalid run phase"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub work_item_id: String,
    pub session_id: Option<String>,
    pub status: RunStatus,
    pub phase: RunPhase,
    pub started_at_ms: Option<i64>,
    pub updated_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStepStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

impl RunStepStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            _ => anyhow::bail!("invalid run step status"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStep {
    pub id: String,
    pub run_id: String,
    pub sequence: i64,
    pub kind: String,
    pub title: String,
    pub status: RunStepStatus,
    pub detail: serde_json::Value,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCheckpoint {
    pub id: String,
    pub run_id: String,
    pub sequence: i64,
    pub phase: RunPhase,
    pub state: serde_json::Value,
    pub safe_to_resume: bool,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetStatus {
    #[default]
    Open,
    Conflict,
    ReadyForReview,
    Accepted,
    Reverted,
}

impl ChangeSetStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Conflict => "conflict",
            Self::ReadyForReview => "ready_for_review",
            Self::Accepted => "accepted",
            Self::Reverted => "reverted",
        }
    }

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "open" => Ok(Self::Open),
            "conflict" => Ok(Self::Conflict),
            "ready_for_review" => Ok(Self::ReadyForReview),
            "accepted" => Ok(Self::Accepted),
            "reverted" => Ok(Self::Reverted),
            _ => anyhow::bail!("invalid change set status"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSet {
    pub id: String,
    pub run_id: String,
    pub base_revision: Option<String>,
    pub status: ChangeSetStatus,
    pub summary: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeFile {
    pub change_set_id: String,
    pub path: String,
    pub change_kind: String,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub patch: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSnapshot {
    pub project: Project,
    pub work_items: Vec<WorkItem>,
    pub runs: Vec<Run>,
    pub run_steps: Vec<RunStep>,
    pub checkpoints: Vec<WorkspaceCheckpoint>,
    pub interrupted_run_count: usize,
}
