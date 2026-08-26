use anyhow::{anyhow, Result};
use async_trait::async_trait;

struct MockInspectorOk {
    name: &'static str,
    results: Vec<lumina::tool_inspection::InspectionResult>,
}

struct MockInspectorErr {
    name: &'static str,
}

#[async_trait]
impl lumina::tool_inspection::ToolInspector for MockInspectorOk {
    fn name(&self) -> &'static str {
        self.name
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn inspect(
        &self,
        _session_id: &str,
        _tool_requests: &[lumina::conversation::message::ToolRequest],
        _messages: &[lumina::conversation::message::Message],
        _lumina_mode: lumina::config::LuminaMode,
    ) -> Result<Vec<lumina::tool_inspection::InspectionResult>> {
        Ok(self.results.clone())
    }
}

#[async_trait]
impl lumina::tool_inspection::ToolInspector for MockInspectorErr {
    fn name(&self) -> &'static str {
        self.name
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn inspect(
        &self,
        _session_id: &str,
        _tool_requests: &[lumina::conversation::message::ToolRequest],
        _messages: &[lumina::conversation::message::Message],
        _lumina_mode: lumina::config::LuminaMode,
    ) -> Result<Vec<lumina::tool_inspection::InspectionResult>> {
        Err(anyhow!("simulated failure"))
    }
}

#[tokio::test]
async fn test_inspect_tools_aggregates_and_handles_errors() {
    // Arrange: create a manager with one successful and one failing inspector
    let ok_results = vec![
        lumina::tool_inspection::InspectionResult {
            tool_request_id: "req_1".to_string(),
            action: lumina::tool_inspection::InspectionAction::Allow,
            reason: "looks safe".to_string(),
            confidence: 0.95,
            inspector_name: "ok".to_string(),
            finding_id: None,
        },
        lumina::tool_inspection::InspectionResult {
            tool_request_id: "req_2".to_string(),
            action: lumina::tool_inspection::InspectionAction::RequireApproval(Some(
                "double check".to_string(),
            )),
            reason: "needs user confirmation".to_string(),
            confidence: 0.7,
            inspector_name: "ok".to_string(),
            finding_id: Some("FND-123".to_string()),
        },
    ];

    let mut manager = lumina::tool_inspection::ToolInspectionManager::new();
    manager.add_inspector(Box::new(MockInspectorOk {
        name: "ok",
        results: ok_results.clone(),
    }));
    manager.add_inspector(Box::new(MockInspectorErr { name: "err" }));

    // No specific input is required for this aggregation behavior
    let tool_requests: Vec<lumina::conversation::message::ToolRequest> = vec![];
    let messages: Vec<lumina::conversation::message::Message> = vec![];

    // Act
    let results = manager
        .inspect_tools(
            lumina_test_support::TEST_SESSION_ID,
            &tool_requests,
            &messages,
            lumina::config::LuminaMode::Approve,
        )
        .await
        .expect("inspect_tools should not fail when one inspector errors");

    // Assert: results from the successful inspector are returned; failing inspector is ignored
    assert_eq!(
        results.len(),
        2,
        "Should aggregate results from successful inspectors only"
    );
    // Also verify inspector_names() order/presence
    let names = manager.inspector_names();
    assert_eq!(
        names,
        vec!["ok", "err"],
        "Inspector names should reflect registration order"
    );

    // Verify that specific actions are preserved
    assert!(results
        .iter()
        .any(|r| matches!(r.action, lumina::tool_inspection::InspectionAction::Allow)));
    assert!(results.iter().any(|r| matches!(
        r.action,
        lumina::tool_inspection::InspectionAction::RequireApproval(_)
    )));
}
