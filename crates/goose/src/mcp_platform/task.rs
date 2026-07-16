use serde::{Deserialize, Serialize};

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOperation {
    Register,
    Install,
    Update,
    Repair,
    Uninstall,
    Health,
}

impl TaskOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Install => "install",
            Self::Update => "update",
            Self::Repair => "repair",
            Self::Uninstall => "uninstall",
            Self::Health => "health",
        }
    }
}

impl From<super::policy::PlanOperation> for TaskOperation {
    fn from(value: super::policy::PlanOperation) -> Self {
        match value {
            super::policy::PlanOperation::Register => Self::Register,
            super::policy::PlanOperation::Install => Self::Install,
            super::policy::PlanOperation::Update => Self::Update,
            super::policy::PlanOperation::Repair => Self::Repair,
            super::policy::PlanOperation::Uninstall => Self::Uninstall,
            super::policy::PlanOperation::Health => Self::Health,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Planned,
    AwaitingConfirmation,
    Queued,
    Running,
    Cancelling,
    Verifying,
    Activating,
    RollingBack,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    RecoveryRequired,
}

impl TaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Verifying => "verifying",
            Self::Activating => "activating",
            Self::RollingBack => "rolling_back",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::RecoveryRequired => "recovery_required",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Planned, Self::AwaitingConfirmation)
                | (Self::AwaitingConfirmation, Self::Queued | Self::Cancelled)
                | (Self::Queued, Self::Running | Self::Cancelled)
                | (
                    Self::Running,
                    Self::Cancelling
                        | Self::Verifying
                        | Self::RollingBack
                        | Self::Failed
                        | Self::Interrupted
                )
                | (
                    Self::Cancelling,
                    Self::RollingBack | Self::Cancelled | Self::Interrupted
                )
                | (
                    Self::Verifying,
                    Self::Activating | Self::RollingBack | Self::Failed | Self::Interrupted
                )
                | (
                    Self::Activating,
                    Self::Succeeded | Self::RollingBack | Self::Failed | Self::Interrupted
                )
                | (
                    Self::RollingBack,
                    Self::Failed | Self::RecoveryRequired | Self::Interrupted
                )
                | (
                    Self::Interrupted,
                    Self::Queued | Self::RollingBack | Self::RecoveryRequired
                )
                | (Self::Failed | Self::RecoveryRequired, Self::Queued)
        )
    }

    pub fn ensure_transition(self, next: Self) -> McpPlatformResult<()> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(McpPlatformError::new(
                McpPlatformErrorCode::InvalidTransition,
                "task status transition is not allowed",
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStepStatus {
    NotStarted,
    Started,
    Committed,
}

impl TaskStepStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Started => "started",
            Self::Committed => "committed",
        }
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::NotStarted, Self::Started) | (Self::Started, Self::Committed)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackStatus {
    NotRequired,
    Pending,
    InProgress,
    Complete,
    Incomplete,
}

impl RollbackStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterEvidence {
    pub adapter_id: String,
    pub adapter_version: String,
    pub compatible_for_recovery: bool,
    pub resume_safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackEvidence {
    pub compensation_available: bool,
    pub remaining_compensations: Vec<CompensationDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompensationDescriptor {
    RemoveConnectionProjection { link_key: String },
    RestoreActivation { version: String },
    RemoveOwnedPath { root: String, relative_path: String },
    RestoreConfigFragment { registration_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepEvidence {
    ConnectionProjected { link_key: String, revision: i64 },
    ArtifactVerified { digest: String },
    ActivationRecorded { version: String },
    HealthObserved { result_code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryDecision {
    ResumeFromStep { ordinal: i64 },
    RollbackFromStep { ordinal: i64 },
    RequiresManualRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactedErrorCode {
    AdapterFailed,
    VerificationFailed,
    ActivationFailed,
    RollbackFailed,
    Cancelled,
    Interrupted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactedError {
    code: RedactedErrorCode,
    message: String,
}

impl RedactedError {
    pub fn new(
        code: RedactedErrorCode,
        message: &str,
        sensitive_values: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        let mut redacted = message.to_string();
        for value in sensitive_values {
            let value = value.as_ref();
            if !value.is_empty() {
                redacted = redacted.replace(value, "[REDACTED]");
            }
        }
        Self {
            code,
            message: redact_common_credentials(&redacted),
        }
    }

    pub const fn code(&self) -> RedactedErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

fn redact_common_credentials(message: &str) -> String {
    let words = message.split_whitespace().collect::<Vec<_>>();
    let mut redacted = Vec::with_capacity(words.len());
    let mut index = 0;
    while index < words.len() {
        let normalized = words[index].to_ascii_lowercase();
        if normalized == "authorization:" || normalized == "authorization=" {
            redacted.push("[REDACTED]".to_string());
            index += if words
                .get(index + 1)
                .is_some_and(|next| next.eq_ignore_ascii_case("bearer"))
            {
                3.min(words.len() - index)
            } else {
                2.min(words.len() - index)
            };
            continue;
        }
        if normalized == "bearer" {
            redacted.push("[REDACTED]".to_string());
            index += 2.min(words.len() - index);
            continue;
        }
        if normalized == "oauth"
            && words
                .get(index + 1)
                .is_some_and(|next| next.eq_ignore_ascii_case("code"))
        {
            redacted.push("[REDACTED]".to_string());
            index += 3.min(words.len() - index);
            continue;
        }
        if normalized.starts_with("bearer")
            || normalized.starts_with("authorization:")
            || normalized.starts_with("authorization=")
            || normalized.starts_with("code=")
            || normalized.starts_with("oauth_code=")
            || normalized.starts_with("access_token=")
            || normalized.starts_with("refresh_token=")
        {
            redacted.push("[REDACTED]".to_string());
        } else {
            redacted.push(words[index].to_string());
        }
        index += 1;
    }
    redact_embedded_key_values(redacted.join(" "))
}

fn redact_embedded_key_values(mut message: String) -> String {
    for key in ["code=", "oauth_code=", "access_token=", "refresh_token="] {
        loop {
            let normalized = message.to_ascii_lowercase();
            let Some(key_start) = normalized.find(key) else {
                break;
            };
            let value_start = key_start + key.len();
            let mut value_end = value_start;
            while let Some(character) = message
                .get(value_end..)
                .and_then(|remaining| remaining.chars().next())
            {
                if character.is_whitespace() || matches!(character, '&' | ',' | ';' | ')' | ']') {
                    break;
                }
                value_end += character.len_utf8();
            }
            message.replace_range(key_start..value_end, "[REDACTED]");
        }
    }
    message
}
