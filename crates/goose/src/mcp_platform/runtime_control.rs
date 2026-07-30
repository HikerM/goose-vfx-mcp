use async_trait::async_trait;

use crate::agents::ExtensionConfig;

use super::error::McpPlatformResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedRuntimeAction {
    Start,
    Stop,
}

#[derive(Clone)]
pub struct ManagedRuntimeCommand {
    pub managed_mcp_id: String,
    pub extension_key: String,
    pub expected_config: ExtensionConfig,
    pub action: ManagedRuntimeAction,
}

#[async_trait]
pub trait ManagedRuntimeControlPort: Send + Sync {
    /// Receives a verified Goose projection, never a caller-supplied PID, command, or path.
    /// Returns whether an owned runtime was affected. A running record with no owned
    /// runtime must fail closed instead of being represented as stopped.
    async fn control(&self, command: ManagedRuntimeCommand) -> McpPlatformResult<bool>;
}
