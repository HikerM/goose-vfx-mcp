use anyhow::Result;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::acp::{
    extension_configs_to_mcp_servers, AcpProvider, AcpProviderConfig, ACP_CURRENT_MODEL,
};
use crate::config::search_path::SearchPaths;
use crate::config::{Config, LuminaMode};
use crate::providers::base::{
    current_working_dir, ProviderDef, ProviderDescriptor, ProviderMetadata,
};

pub(crate) const CODEX_ACP_PROVIDER_NAME: &str = "codex-acp";
const CODEX_ACP_DOC_URL: &str = "https://github.com/zed-industries/codex-acp";

pub struct CodexAcpProvider;

impl lumina_providers::base::ProviderDescriptor for CodexAcpProvider {
    fn metadata() -> ProviderMetadata {
        ProviderMetadata::new(
            CODEX_ACP_PROVIDER_NAME,
            "Codex CLI",
            "Use lumina with your ChatGPT Plus/Pro subscription via the codex-acp adapter.",
            ACP_CURRENT_MODEL,
            vec![],
            CODEX_ACP_DOC_URL,
            vec![],
        )
        .with_setup_steps(vec![
            "Install the ACP adapter: `npm install -g @zed-industries/codex-acp`",
            "Run `codex` once to authenticate with your OpenAI account",
            "Add to your lumina config file (`~/.config/lumina/config.yaml` on macOS/Linux):\n  LUMINA_PROVIDER: codex-acp\n  LUMINA_MODEL: current\n  codex-acp_configured: true",
            "Restart lumina for changes to take effect",
        ])
    }
}

impl ProviderDef for CodexAcpProvider {
    type Provider = AcpProvider;

    fn from_env(
        extensions: Vec<crate::config::ExtensionConfig>,
        tls_config: Option<crate::providers::api_client::TlsConfig>,
    ) -> BoxFuture<'static, Result<AcpProvider>> {
        Self::from_env_with_working_dir(extensions, current_working_dir(), tls_config)
    }

    fn from_env_with_working_dir(
        extensions: Vec<crate::config::ExtensionConfig>,
        working_dir: PathBuf,
        _tls_config: Option<crate::providers::api_client::TlsConfig>,
    ) -> BoxFuture<'static, Result<AcpProvider>> {
        Box::pin(async move {
            let config = Config::global();
            // with_npm() includes npm global bin dir (desktop app PATH may not)
            let resolved_command = SearchPaths::builder()
                .with_npm()
                .resolve(CODEX_ACP_PROVIDER_NAME)?;
            let env = vec![];
            let lumina_mode = config.get_lumina_mode().unwrap_or(LuminaMode::Auto);
            let mcp_servers = extension_configs_to_mcp_servers(&extensions);

            // fixed lumina mode via -c overrides until session/set-mode works
            let (approval_policy, sandbox_mode) = map_lumina_mode(lumina_mode);
            let mut args = vec![
                "-c".to_string(),
                format!("approval_policy={approval_policy}"),
                "-c".to_string(),
                format!("sandbox_mode={sandbox_mode}"),
            ];

            // Codex sandbox blocks network by default. Enable it when HTTP MCP
            // servers are configured so codex-acp can connect to them.
            let has_http_mcp = mcp_servers
                .iter()
                .any(|s| matches!(s, agent_client_protocol::schema::v1::McpServer::Http(_)));
            if has_http_mcp {
                args.extend([
                    "-c".to_string(),
                    "sandbox_workspace_write.network_access=true".to_string(),
                ]);
            }

            let mode_mapping = HashMap::from([
                (
                    LuminaMode::Auto,
                    vec!["full-access".to_string(), "agent-full-access".to_string()],
                ),
                (
                    LuminaMode::SmartApprove,
                    vec!["auto".to_string(), "agent".to_string()],
                ),
                (LuminaMode::Approve, vec!["read-only".to_string()]),
                (LuminaMode::Chat, vec!["read-only".to_string()]),
            ]);

            let provider_config = AcpProviderConfig {
                command: resolved_command,
                args,
                env,
                env_remove: vec![],
                work_dir: working_dir,
                mcp_servers,
                session_mode_id: None,
                session_config_options: vec![],
                model_config_option_id: None,
                mode_mapping,
                notification_callback: None,
            };

            let metadata = Self::metadata();
            AcpProvider::connect(metadata.name, lumina_mode, provider_config).await
        })
    }
}

// Codex sandbox scope determines what needs approval: operations within the
// sandbox are auto-approved, operations outside it trigger on-request prompts.
// So Approve uses read-only sandbox to force write approvals through lumina.
fn map_lumina_mode(lumina_mode: LuminaMode) -> (&'static str, &'static str) {
    match lumina_mode {
        LuminaMode::Auto => ("never", "danger-full-access"),
        LuminaMode::SmartApprove => ("on-request", "workspace-write"),
        LuminaMode::Approve => ("on-request", "read-only"),
        LuminaMode::Chat => ("never", "read-only"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(LuminaMode::Auto, "never", "danger-full-access")]
    #[test_case(LuminaMode::SmartApprove, "on-request", "workspace-write")]
    #[test_case(LuminaMode::Approve, "on-request", "read-only")]
    #[test_case(LuminaMode::Chat, "never", "read-only")]
    fn test_map_lumina_mode(mode: LuminaMode, expected_approval: &str, expected_sandbox: &str) {
        let (approval, sandbox) = map_lumina_mode(mode);
        assert_eq!(approval, expected_approval);
        assert_eq!(sandbox, expected_sandbox);
    }
}
