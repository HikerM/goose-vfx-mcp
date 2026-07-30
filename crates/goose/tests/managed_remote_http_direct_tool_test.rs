#![cfg(all(feature = "integration-test-support", feature = "rustls-tls"))]

#[path = "acp_fixtures/remote_https.rs"]
mod remote_https;

use std::sync::Arc;

use goose::agents::extension_manager::ToolDiscoveryLimits;
use goose::agents::{ExtensionConfig, ExtensionManager, ToolCallContext};
use remote_https::{FixtureRemoteHttpNetworkPolicy, RemoteHttpsFixture, FIXTURE_CODE};
use rmcp::model::CallToolRequestParams;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn managed_streamable_http_dispatches_fixture_tool_directly() {
    let fixture = RemoteHttpsFixture::start().await.unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(
        ExtensionManager::new_without_provider_with_managed_remote_http_policy(
            data_dir.path().to_path_buf(),
            Arc::new(FixtureRemoteHttpNetworkPolicy::new(&fixture)),
        ),
    );
    let session_id = "fixture-session";
    let extension_name = "mcp-fixture";

    manager
        .add_extension(
            ExtensionConfig::ManagedStreamableHttp {
                name: extension_name.to_owned(),
                description: "Managed HTTPS fixture".to_owned(),
                uri: fixture.mcp_url.clone(),
                timeout: None,
                bundled: Some(false),
                available_tools: vec![],
            },
            None,
            None,
            Some(session_id),
        )
        .await
        .unwrap();

    let tool_name = "mcp-fixture__get_code";
    let tools = manager
        .get_prefixed_tools_bounded(
            session_id,
            extension_name,
            ToolDiscoveryLimits {
                max_tools: 10,
                max_tool_bytes: 4096,
                max_total_bytes: 4096,
                max_pages: 10,
                max_cursors: 10,
                max_cursor_bytes: 1024,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(tools.iter().any(|tool| tool.name == tool_name));

    let dispatch = manager
        .dispatch_tool_call(
            &ToolCallContext::new(session_id.to_owned(), None, Some("request".to_owned())),
            CallToolRequestParams::new(tool_name).with_arguments(serde_json::Map::new()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let result = dispatch.result.await.unwrap();

    assert_eq!(result.content[0].as_text().unwrap().text, FIXTURE_CODE);
    assert_eq!(fixture.tool_calls(), ["get_code"]);
}
