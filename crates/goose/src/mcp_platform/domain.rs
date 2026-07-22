use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    Official,
    Community,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationState {
    Absent,
    Registered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationState {
    NotApplicable,
    NotInstalled,
    Staged,
    Installed,
    UpdateAvailable,
    RepairRequired,
    UninstallPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Unknown,
    Checking,
    Healthy,
    Degraded,
    Unhealthy,
    BlockedAuth,
    Incompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthCheckMode {
    Registration,
    Runtime,
}

impl HealthCheckMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registration => "registration",
            Self::Runtime => "runtime",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthResultCode {
    Healthy,
    Unhealthy,
    BlockedAuth,
    Incompatible,
    Timeout,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthDetailCode {
    ProjectionConsistent,
    ProjectionDrift,
    CredentialHandleMissing,
    ExpectedStatus,
    UnexpectedStatus,
    McpInitializeSucceeded,
    McpInitializeFailed,
    McpListToolsSucceeded,
    McpListToolsFailed,
    IncompatibleHealthContract,
    CleanupFailed,
    Timeout,
    Cancelled,
}

impl HealthResultCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::BlockedAuth => "blocked_auth",
            Self::Incompatible => "incompatible",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }

    pub const fn health_state(self) -> HealthState {
        match self {
            Self::Healthy => HealthState::Healthy,
            Self::BlockedAuth => HealthState::BlockedAuth,
            Self::Incompatible => HealthState::Incompatible,
            Self::Unhealthy | Self::Timeout | Self::Cancelled => HealthState::Unhealthy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyDecision {
    Ask,
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPolicy {
    pub decision: ToolPolicyDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedMcpState {
    pub registration: RegistrationState,
    pub installation: InstallationState,
    pub runtime: RuntimeState,
    pub health: HealthState,
    pub default_enabled: bool,
    pub session_enabled: BTreeMap<String, bool>,
    pub tool_policies: BTreeMap<String, ToolPolicy>,
}

impl Default for ManagedMcpState {
    fn default() -> Self {
        Self {
            registration: RegistrationState::Absent,
            installation: InstallationState::NotInstalled,
            runtime: RuntimeState::Stopped,
            health: HealthState::Unknown,
            default_enabled: false,
            session_enabled: BTreeMap::new(),
            tool_policies: BTreeMap::new(),
        }
    }
}
