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
