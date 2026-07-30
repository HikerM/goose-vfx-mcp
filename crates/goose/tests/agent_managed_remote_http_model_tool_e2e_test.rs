#![cfg(all(feature = "integration-test-support", feature = "rustls-tls"))]

#[path = "acp_fixtures/mod.rs"]
mod fixtures;
#[path = "acp_fixtures/remote_https.rs"]
mod remote_https;

use std::path::PathBuf;
use std::sync::Arc;

use fixtures::{OpenAiFixture, PermissionDecision};
use futures::StreamExt;
use goose::agents::extension_manager::{ExtensionManager, ExtensionManagerCapabilities};
use goose::agents::{
    Agent, AgentConfig, AgentEvent, ExtensionConfig, GoosePlatform, SessionConfig,
};
use goose::config::{GooseMode, PermissionManager};
use goose::conversation::message::{ActionRequiredData, Message, MessageContent};
use goose::permission::permission_confirmation::PrincipalType;
use goose::permission::{Permission, PermissionConfirmation};
use goose::providers::api_client::{ApiClient, AuthMethod};
use goose::providers::base::Provider;
use goose::providers::openai::OpenAiProviderBuilder;
use goose::session::{SessionManager, SessionType};
use goose_providers::model::ModelConfig;
use goose_test_support::IgnoreSessionId;
use remote_https::{FixtureRemoteHttpNetworkPolicy, RemoteHttpsFixture, FIXTURE_CODE};

const USER_PROMPT: &str = "Call the fixture tool and return its code.";

fn final_response() -> &'static str {
    "data: {\"id\":\"chatcmpl-final\",\"object\":\"chat.completion.chunk\",\"created\":1766709751,\"model\":\"gpt-5-nano-2025-08-07\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"MCP_FIXTURE_CODE\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"chatcmpl-final\",\"object\":\"chat.completion.chunk\",\"created\":1766709751,\"model\":\"gpt-5-nano-2025-08-07\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n"
}

#[tokio::test]
async fn agent_managed_remote_http_model_tool_round_trip() -> anyhow::Result<()> {
    let data_dir = tempfile::tempdir()?;
    let fixture = RemoteHttpsFixture::start().await?;
    let openai = OpenAiFixture::new(
        vec![
            (
                USER_PROMPT.to_owned(),
                include_str!("acp_test_data/openai_tool_call.txt"),
            ),
            (format!(r#""content":"{FIXTURE_CODE}""#), final_response()),
        ],
        Arc::new(IgnoreSessionId),
    )
    .await;
    let observer = openai.observer();

    let session_manager = Arc::new(SessionManager::new(data_dir.path().to_path_buf()));
    let mut agent = Agent::with_config(AgentConfig::new(
        Arc::clone(&session_manager),
        Arc::new(PermissionManager::new(data_dir.path().to_path_buf())),
        None,
        GooseMode::default(),
        true,
        GoosePlatform::GooseCli,
    ));
    let manager_provider: goose::agents::types::SharedProvider =
        Arc::new(tokio::sync::Mutex::new(None));
    agent.extension_manager = Arc::new(ExtensionManager::new_with_managed_remote_http_policy(
        manager_provider,
        Arc::clone(&session_manager),
        "goose-cli".to_owned(),
        ExtensionManagerCapabilities {
            mcpui: false,
            host_info: None,
        },
        false,
        Arc::new(FixtureRemoteHttpNetworkPolicy::new(&fixture)),
    ));

    let session = session_manager
        .create_session(
            PathBuf::default(),
            "managed remote HTTP model tool fixture".to_owned(),
            SessionType::Hidden,
            GooseMode::default(),
        )
        .await?;
    let provider: Arc<dyn Provider> = Arc::new(
        OpenAiProviderBuilder::new(ApiClient::new_with_tls(
            openai.uri().to_owned(),
            AuthMethod::BearerToken("fixture-key".to_owned()),
            None,
        )?)
        .base_path("/v1/chat/completions")
        .build(),
    );
    agent
        .update_provider(
            provider,
            ModelConfig::new("gpt-5-nano-2025-08-07"),
            &session.id,
        )
        .await?;
    agent
        .extension_manager
        .add_extension(
            ExtensionConfig::ManagedStreamableHttp {
                name: "mcp-fixture".to_owned(),
                description: "Managed HTTPS fixture".to_owned(),
                uri: fixture.mcp_url.clone(),
                timeout: None,
                bundled: Some(false),
                available_tools: vec![],
            },
            Some(session.working_dir.clone()),
            None,
            Some(&session.id),
        )
        .await?;

    let reply = agent
        .reply(
            Message::user().with_text(USER_PROMPT),
            SessionConfig {
                id: session.id,
                schedule_id: None,
                max_turns: Some(5),
                retry_config: None,
            },
            None,
        )
        .await?;
    tokio::pin!(reply);

    let mut text = String::new();
    while let Some(event) = reply.next().await {
        match event? {
            AgentEvent::Message(message) => {
                for content in message.content {
                    match content {
                        MessageContent::Text(content) => text.push_str(&content.text),
                        MessageContent::ActionRequired(action) => {
                            if let ActionRequiredData::ToolConfirmation { id, .. } = action.data {
                                agent
                                    .handle_confirmation(
                                        id,
                                        PermissionConfirmation {
                                            principal_type: PrincipalType::Tool,
                                            permission: Permission::from(
                                                PermissionDecision::AllowOnce,
                                            ),
                                        },
                                    )
                                    .await;
                            }
                        }
                        _ => {}
                    }
                }
            }
            AgentEvent::McpNotification(_)
            | AgentEvent::Usage(_)
            | AgentEvent::MessageUsage { .. }
            | AgentEvent::HistoryReplaced(_) => {}
        }
    }

    assert_eq!(text, FIXTURE_CODE);
    assert_eq!(fixture.tool_calls(), ["get_code"]);
    observer.assert_exhausted();
    Ok(())
}
