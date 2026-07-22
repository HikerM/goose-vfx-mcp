use super::*;
use agent_client_protocol::JsonRpcMessage;

fn assert_method<T>(expected: &str)
where
    T: Default + JsonRpcMessage,
{
    let request = T::default();
    assert_eq!(expected, request.method());
}

#[test]
fn method_constants_match_request_contracts() {
    assert_method::<McpCatalogListRequest>(MCP_CATALOG_LIST_METHOD);
    assert_method::<McpCatalogDetailRequest>(MCP_CATALOG_DETAIL_METHOD);
    assert_method::<McpPlanCreateRequest>(MCP_PLAN_CREATE_METHOD);
    assert_method::<McpInstallConfirmRequest>(MCP_INSTALL_CONFIRM_METHOD);
    assert_method::<McpTaskGetRequest>(MCP_TASK_GET_METHOD);
    assert_method::<McpTaskCancelRequest>(MCP_TASK_CANCEL_METHOD);
    assert_method::<McpTaskRetryRequest>(MCP_TASK_RETRY_METHOD);
    assert_method::<McpEventsResumeRequest>(MCP_EVENTS_RESUME_METHOD);
    assert_method::<McpListRequest>(MCP_LIST_METHOD);
    assert_method::<McpGetRequest>(MCP_GET_METHOD);
    assert_method::<McpHealthRunRequest>(MCP_HEALTH_RUN_METHOD);
    assert_method::<McpHealthGetRequest>(MCP_HEALTH_GET_METHOD);
    assert_method::<McpSetDefaultEnabledRequest>(MCP_SET_DEFAULT_ENABLED_METHOD);
}

#[test]
fn requests_are_closed_and_require_explicit_confirmation_decision() {
    let valid = serde_json::json!({
        "planId": "plan_1",
        "planDigest": "a".repeat(64),
        "userDecision": "confirm",
        "idempotencyKey": "confirm-key"
    });
    let request: McpInstallConfirmRequest = serde_json::from_value(valid.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap()["userDecision"],
        "confirm"
    );

    let mut unknown = valid.clone();
    unknown["actor"] = serde_json::json!("renderer-forged");
    assert!(serde_json::from_value::<McpInstallConfirmRequest>(unknown).is_err());

    let mut missing = valid;
    missing.as_object_mut().unwrap().remove("userDecision");
    assert!(serde_json::from_value::<McpInstallConfirmRequest>(missing).is_err());
}

#[test]
fn generated_request_schemas_are_closed_and_have_no_execution_escape_hatch() {
    let schemas = [
        schemars::schema_for!(McpCatalogListRequest),
        schemars::schema_for!(McpCatalogDetailRequest),
        schemars::schema_for!(McpPlanCreateRequest),
        schemars::schema_for!(McpInstallConfirmRequest),
        schemars::schema_for!(McpTaskGetRequest),
        schemars::schema_for!(McpTaskCancelRequest),
        schemars::schema_for!(McpTaskRetryRequest),
        schemars::schema_for!(McpEventsResumeRequest),
        schemars::schema_for!(McpListRequest),
        schemars::schema_for!(McpGetRequest),
        schemars::schema_for!(McpHealthRunRequest),
        schemars::schema_for!(McpHealthGetRequest),
        schemars::schema_for!(McpSetDefaultEnabledRequest),
    ];
    let forbidden = [
        "\"actor\"",
        "\"command\"",
        "\"shell\"",
        "\"credentialValue\"",
        "\"endpoint\"",
        "\"path\"",
        "\"env\"",
        "flatten",
        "serde_json::Value",
    ];
    for schema in schemas {
        let value = serde_json::to_value(schema).unwrap();
        let text = serde_json::to_string(&value).unwrap();
        assert_eq!(value["additionalProperties"], false);
        for field in forbidden {
            assert!(!text.contains(field), "schema contains forbidden {field}");
        }
    }
}

#[test]
fn phase_3a_health_and_enable_requests_reject_execution_fields() {
    let valid = serde_json::json!({
        "managedMcpId": "managed_1",
        "mode": "runtime",
        "idempotencyKey": "health-1"
    });
    assert!(serde_json::from_value::<McpHealthRunRequest>(valid.clone()).is_ok());
    for field in [
        "endpoint",
        "command",
        "path",
        "env",
        "credentialValue",
        "actor",
    ] {
        let mut forged = valid.clone();
        forged[field] = serde_json::json!("forged");
        assert!(serde_json::from_value::<McpHealthRunRequest>(forged).is_err());
    }
    let enable =
        serde_json::json!({"managedMcpId":"managed_1","enabled":true,"expectedRevision":4});
    assert!(serde_json::from_value::<McpSetDefaultEnabledRequest>(enable.clone()).is_ok());
    let mut forged = enable;
    forged["execute"] = serde_json::json!(true);
    assert!(serde_json::from_value::<McpSetDefaultEnabledRequest>(forged).is_err());
}

#[test]
fn response_error_envelope_round_trips_without_open_details() {
    let response = McpTaskGetResponse {
        outcome: McpPlatformOutcome::Error {
            error: McpPlatformErrorEnvelope {
                code: McpPlatformErrorCodeDto::RepositoryUnavailable,
                message: "Repository unavailable.".to_string(),
                retryable: true,
                correlation_id: "correlation_test".to_string(),
                details: Some(McpPlatformErrorDetails::RepositoryTemporarilyUnavailable {}),
            },
        },
    };
    let value = serde_json::to_value(&response).unwrap();
    let decoded: McpTaskGetResponse = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);

    let mut forged = value;
    forged["outcome"]["error"]["details"]["sql"] = serde_json::json!("SELECT secret");
    assert!(serde_json::from_value::<McpTaskGetResponse>(forged).is_err());

    let without_details = serde_json::json!({
        "outcome": {
            "status": "error",
            "error": {
                "code": "invalid_request",
                "message": "Invalid request.",
                "retryable": false,
                "correlationId": "correlation_without_details"
            }
        }
    });
    let decoded: McpTaskGetResponse = serde_json::from_value(without_details).unwrap();
    let McpPlatformOutcome::Error { error } = decoded.outcome else {
        panic!("expected error outcome");
    };
    assert!(error.details.is_none());
    assert!(serde_json::to_value(error)
        .unwrap()
        .get("details")
        .is_none());

    let schema = serde_json::to_value(schemars::schema_for!(McpPlatformErrorEnvelope)).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert!(schema["required"]
        .as_array()
        .is_some_and(|required| !required.iter().any(|field| field == "details")));
}
