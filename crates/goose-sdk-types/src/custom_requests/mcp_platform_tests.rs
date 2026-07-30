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
    assert_method::<McpProjectionRecoveryResolveRequest>(MCP_PROJECTION_RECOVERY_RESOLVE_METHOD);
    assert_method::<McpHealthRunRequest>(MCP_HEALTH_RUN_METHOD);
    assert_method::<McpHealthGetRequest>(MCP_HEALTH_GET_METHOD);
    assert_method::<McpSetDefaultEnabledRequest>(MCP_SET_DEFAULT_ENABLED_METHOD);
    assert_method::<McpRuntimeControlRequest>(MCP_RUNTIME_CONTROL_METHOD);
    assert_method::<McpSourcesPolicyGetRequest>(MCP_SOURCES_POLICY_GET_METHOD);
    assert_method::<McpSourceRefreshRequest>(MCP_SOURCE_REFRESH_METHOD);
    assert_method::<McpSourceProvisionPrepareRequest>(MCP_SOURCE_PROVISION_PREPARE_METHOD);
    assert_method::<McpSourceProvisionConfirmRequest>(MCP_SOURCE_PROVISION_CONFIRM_METHOD);
    assert_method::<McpGovernedImportRequest>(MCP_GOVERNED_IMPORT_METHOD);
    assert_method::<McpManualStdioSourcesListRequest>(MCP_MANUAL_STDIO_SOURCES_LIST_METHOD);
    assert_method::<McpManualPlanCreateRequest>(MCP_MANUAL_PLAN_CREATE_METHOD);
    assert_method::<McpProfileListRequest>(MCP_PROFILE_LIST_METHOD);
    assert_method::<McpProfileGetRequest>(MCP_PROFILE_GET_METHOD);
    assert_method::<McpProfileCreateRequest>(MCP_PROFILE_CREATE_METHOD);
    assert_method::<McpProfileUpdateRequest>(MCP_PROFILE_UPDATE_METHOD);
    assert_method::<McpProfileRestoreRequest>(MCP_PROFILE_RESTORE_METHOD);
    assert_method::<McpProfileArchiveRequest>(MCP_PROFILE_ARCHIVE_METHOD);
    assert_method::<McpProfileDraftCreateRequest>(MCP_PROFILE_DRAFT_CREATE_METHOD);
    assert_method::<McpProfileModelRecommendRequest>(MCP_PROFILE_MODEL_RECOMMEND_METHOD);
    assert_method::<McpProfileConnectionTestRequest>(MCP_PROFILE_CONNECTION_TEST_METHOD);
    assert_method::<McpProfileApplyPlanCreateRequest>(MCP_PROFILE_APPLY_PLAN_CREATE_METHOD);
    assert_method::<McpProfileApplyConfirmRequest>(MCP_PROFILE_APPLY_CONFIRM_METHOD);
    assert_method::<McpSourceAdaptersListRequest>(MCP_SOURCE_ADAPTERS_LIST_METHOD);
    assert_method::<McpHttpsProvisionPlanCreateRequest>(MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD);
}

#[test]
fn https_provision_plan_create_wire_is_strict_and_redacts_debug() {
    let valid = serde_json::json!({
        "provisionId": "provision_1",
        "expectedManifestDigest": "a".repeat(64),
        "idempotencyKey": "https-plan-1"
    });
    let request: McpHttpsProvisionPlanCreateRequest =
        serde_json::from_value(valid.clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), valid);

    for missing in ["provisionId", "expectedManifestDigest", "idempotencyKey"] {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(missing);
        assert!(serde_json::from_value::<McpHttpsProvisionPlanCreateRequest>(value).is_err());
    }
    for extra in [
        "url",
        "source",
        "catalog",
        "proof",
        "trustTier",
        "rawBytes",
        "actor",
        "session",
        "token",
    ] {
        let mut value = valid.clone();
        value[extra] = serde_json::json!("must-not-be-accepted");
        assert!(serde_json::from_value::<McpHttpsProvisionPlanCreateRequest>(value).is_err());
    }
    for digest in [
        String::new(),
        "A".repeat(64),
        "g".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
    ] {
        let mut value = valid.clone();
        value["expectedManifestDigest"] = serde_json::json!(digest);
        assert!(serde_json::from_value::<McpHttpsProvisionPlanCreateRequest>(value).is_err());
    }
    for provision_id in ["", " "] {
        let mut value = valid.clone();
        value["provisionId"] = serde_json::json!(provision_id);
        assert!(serde_json::from_value::<McpHttpsProvisionPlanCreateRequest>(value).is_err());
    }
    for key in ["", " ", "https-plan-1\n"] {
        let mut value = valid.clone();
        value["idempotencyKey"] = serde_json::json!(key);
        assert!(serde_json::from_value::<McpHttpsProvisionPlanCreateRequest>(value).is_err());
    }

    let default = McpHttpsProvisionPlanCreateRequest::default();
    assert!(
        serde_json::from_value::<McpHttpsProvisionPlanCreateRequest>(
            serde_json::to_value(default).unwrap()
        )
        .is_err()
    );
    let debug = format!("{request:?}");
    assert!(!debug.contains("provision_1"));
    assert!(!debug.contains("https-plan-1"));
    assert!(!debug.contains(&"a".repeat(64)));

    let schema =
        serde_json::to_value(schemars::schema_for!(McpHttpsProvisionPlanCreateRequest)).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"].as_array().unwrap().len(), 3);
}

#[test]
fn https_provision_response_is_a_closed_safe_projection() {
    let schema = serde_json::to_value(schemars::schema_for!(McpHttpsProvisionPlanReview)).unwrap();
    let schema_text = schema.to_string();
    for forbidden in [
        "sourceId",
        "catalogTarget",
        "proof",
        "sourceProvenance",
        "publisher",
        "website",
        "signingIdentities",
        "immutableEvidence",
        "networkOrigins",
        "registrationIds",
        "url",
        "path",
        "token",
        "actor",
        "session",
        "rawBytes",
        "repositoryOrigin",
        "dns",
    ] {
        assert!(
            !schema_text.contains(forbidden),
            "response schema contains {forbidden}"
        );
    }
    assert_eq!(schema["additionalProperties"], false);

    let valid = serde_json::json!({
        "planId": "plan-1",
        "planDigest": "digest-1",
        "expiresAtMs": 10,
        "trustTier": "official",
        "mcpId": "mcp-1",
        "name": "Safe MCP",
        "version": "1.0.0",
        "selectedManifestDigest": "manifest-1",
        "permissions": [{"kind": "network", "required": true}],
        "fileEffects": {"writesFiles": false, "removesFiles": false, "ownedItems": 0},
        "processEffects": {"processRequiredForConnection": true, "startsDuringConfirmation": false},
        "reversibility": {"reversible": true, "strategy": "available"},
        "policy": {"outcome": "allow", "reasonCount": 0},
        "warnings": ["default_disabled"],
        "requiredConfirmations": [{"type": "policy", "reasonCode": "warning-policy-sentinel"}],
        "defaultDisabled": false,
        "recovery": "resolve_recovery"
    });
    assert!(serde_json::from_value::<McpHttpsProvisionPlanReview>(valid.clone()).is_ok());
    let review: McpHttpsProvisionPlanReview = serde_json::from_value(valid.clone()).unwrap();
    assert_eq!(review.default_disabled, false);
    let serialized = serde_json::to_value(&review).unwrap();
    assert_eq!(serialized["defaultDisabled"], false);
    assert!(serialized.get("default_disabled").is_none());
    assert_eq!(
        serde_json::to_value(
            serde_json::from_value::<McpHttpsProvisionPlanReview>(serialized).unwrap(),
        )
        .unwrap()["defaultDisabled"],
        false
    );

    let mut enabled = valid.clone();
    enabled["defaultDisabled"] = serde_json::json!(true);
    let enabled_review: McpHttpsProvisionPlanReview = serde_json::from_value(enabled).unwrap();
    assert_eq!(enabled_review.default_disabled, true);
    let enabled_wire = serde_json::to_value(&enabled_review).unwrap();
    assert_eq!(enabled_wire["defaultDisabled"], true);
    assert_eq!(
        serde_json::to_value(
            serde_json::from_value::<McpHttpsProvisionPlanReview>(enabled_wire).unwrap(),
        )
        .unwrap()["defaultDisabled"],
        true
    );

    let mut snake_case = valid.clone();
    snake_case["default_disabled"] = snake_case["defaultDisabled"].clone();
    snake_case
        .as_object_mut()
        .unwrap()
        .remove("defaultDisabled");
    assert!(serde_json::from_value::<McpHttpsProvisionPlanReview>(snake_case).is_err());

    let response = McpHttpsProvisionPlanCreateResponse {
        outcome: McpPlatformOutcome::success(review.clone()),
    };
    let debug = format!("{response:?}");
    for required in [
        "McpHttpsProvisionPlanCreateResponse",
        "Success",
        "McpHttpsProvisionPlanReview",
        "McpHttpsProvisionPermission",
        "McpHttpsProvisionFileEffects",
        "McpHttpsProvisionProcessEffects",
        "McpHttpsProvisionReversibility",
        "McpHttpsProvisionPolicy",
        "warnings: 1",
        "required_confirmations: [Policy]",
        "recovery: ResolveRecovery",
    ] {
        assert!(debug.contains(required), "debug omitted {required}");
    }
    for forbidden in [
        "plan-1",
        "digest-1",
        "mcp-1",
        "Safe MCP",
        "1.0.0",
        "manifest-1",
        "https://dns.example:8443/private",
        "10.0.0.1",
        "secret-token",
        "actor-1",
        "session-1",
        "publisher-1",
        "proof-1",
        "origin-1",
        "raw-bytes",
        "registration-1",
        "default_disabled",
        "warning-policy-sentinel",
    ] {
        assert!(!debug.contains(forbidden), "debug leaked {forbidden}");
    }
    assert!(debug.contains("warnings: 1"));
    assert!(!debug.contains("default_disabled"));
    assert!(!debug.contains("defaultDisabled"));
    assert!(debug.contains("required_confirmations: [Policy]"));
    assert!(!debug.contains("warning-policy-sentinel"));
    let nested_debug = format!(
        "{:?}{:?}{:?}{:?}{:?}",
        review.permissions[0],
        review.file_effects,
        review.process_effects,
        review.reversibility,
        review.policy
    );
    assert!(nested_debug.contains("required: true"));
    for forbidden in [
        "plan-1",
        "digest-1",
        "https://dns.example:8443/private",
        "secret-token",
    ] {
        assert!(
            !nested_debug.contains(forbidden),
            "nested debug leaked {forbidden}"
        );
    }
    for forbidden in [
        "sourceId",
        "networkOrigins",
        "url",
        "path",
        "token",
        "actor",
        "rawBytes",
    ] {
        let mut invalid = valid.clone();
        invalid[forbidden] = serde_json::json!("must-fail");
        assert!(serde_json::from_value::<McpHttpsProvisionPlanReview>(invalid).is_err());
    }
    let mut nested = valid;
    nested["permissions"][0]["scope"] = serde_json::json!("/secret");
    assert!(serde_json::from_value::<McpHttpsProvisionPlanReview>(nested).is_err());
}

#[test]
fn https_provision_response_error_debug_redacts_the_entire_envelope() {
    let response = McpHttpsProvisionPlanCreateResponse {
        outcome: McpPlatformOutcome::error(McpPlatformErrorEnvelope {
            code: McpPlatformErrorCodeDto::RepositoryUnavailable,
            message: "error-message-sentinel".to_string(),
            retryable: true,
            correlation_id: "correlation-id-sentinel".to_string(),
            details: Some(McpPlatformErrorDetails::PhaseUnavailable {
                phase: "phase-sentinel".to_string(),
                operation: "operation-sentinel".to_string(),
            }),
        }),
    };

    let debug = format!("{response:?}");
    assert!(debug.contains("McpHttpsProvisionPlanCreateResponse"));
    assert!(debug.contains("Error"));
    assert!(debug.contains("error: \"[REDACTED]\""));
    for sentinel in [
        "error-message-sentinel",
        "correlation-id-sentinel",
        "phase-sentinel",
        "operation-sentinel",
    ] {
        assert!(!debug.contains(sentinel), "debug leaked {sentinel}");
    }
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
fn profile_connection_test_accepts_only_existing_profile_provider_and_model_ids() {
    let request = serde_json::json!({
        "profileId": "profile_1",
        "providerId": "provider_1",
        "modelId": "model_1"
    });
    let decoded: McpProfileConnectionTestRequest = serde_json::from_value(request.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), request);

    for forbidden in [
        "endpoint",
        "command",
        "credentialValue",
        "authorization",
        "prompt",
    ] {
        let mut invalid = request.clone();
        invalid[forbidden] = serde_json::json!("must-not-be-accepted");
        assert!(serde_json::from_value::<McpProfileConnectionTestRequest>(invalid).is_err());
    }
}

#[test]
fn profile_connection_test_result_stays_closed_and_redacts_runtime_details() {
    let stage = McpConnectionTestStage {
        phase: McpConnectionTestPhase::Cleanup,
        status: McpConnectionTestStatus::Failed,
        code: McpConnectionTestCode::CleanupFailed,
        diagnostic: "The temporary managed MCP connection could not be safely closed.".to_string(),
        repair_suggestion: "Retry after verifying the managed MCP runtime is available."
            .to_string(),
        duration_ms: 1,
        managed_mcp_id: Some("managed_1".to_string()),
    };
    let value = serde_json::to_value(stage).unwrap();
    assert_eq!(value["phase"], "cleanup");
    assert_eq!(value["code"], "cleanup_failed");
    for forbidden in ["command", "url", "credential", "tool", "modelContent"] {
        assert!(!value.to_string().contains(forbidden));
    }
    let schema = serde_json::to_value(schemars::schema_for!(McpConnectionTestStage)).unwrap();
    assert_eq!(schema["additionalProperties"], false);
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
        schemars::schema_for!(McpProjectionRecoveryResolveRequest),
        schemars::schema_for!(McpHealthRunRequest),
        schemars::schema_for!(McpHealthGetRequest),
        schemars::schema_for!(McpSetDefaultEnabledRequest),
        schemars::schema_for!(McpSourcesPolicyGetRequest),
        schemars::schema_for!(McpSourceRefreshRequest),
        schemars::schema_for!(McpSourceProvisionPrepareRequest),
        schemars::schema_for!(McpSourceProvisionConfirmRequest),
        schemars::schema_for!(McpGovernedImportRequest),
        schemars::schema_for!(McpManualStdioSourcesListRequest),
        schemars::schema_for!(McpProfileListRequest),
        schemars::schema_for!(McpProfileGetRequest),
        schemars::schema_for!(McpProfileCreateRequest),
        schemars::schema_for!(McpProfileUpdateRequest),
        schemars::schema_for!(McpProfileRestoreRequest),
        schemars::schema_for!(McpProfileArchiveRequest),
        schemars::schema_for!(McpProfileDraftCreateRequest),
        schemars::schema_for!(McpProfileModelRecommendRequest),
        schemars::schema_for!(McpProfileConnectionTestRequest),
        schemars::schema_for!(McpProfileApplyPlanCreateRequest),
        schemars::schema_for!(McpProfileApplyConfirmRequest),
        schemars::schema_for!(McpSourceAdaptersListRequest),
    ];
    let forbidden = [
        "\"actor\"",
        "\"command\"",
        "\"shell\"",
        "\"credentialValue\"",
        "\"endpoint\"",
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
fn projection_recovery_resolve_contract_accepts_only_managed_mcp_id() {
    let request = serde_json::json!({"managedMcpId":"managed_1"});
    let decoded: McpProjectionRecoveryResolveRequest =
        serde_json::from_value(request.clone()).unwrap();
    assert_eq!(decoded.managed_mcp_id, "managed_1");
    assert_eq!(serde_json::to_value(decoded).unwrap(), request);

    assert!(
        serde_json::from_value::<McpProjectionRecoveryResolveRequest>(serde_json::json!({}))
            .is_err()
    );
    assert!(
        serde_json::from_value::<McpProjectionRecoveryResolveRequest>(serde_json::json!({
            "managedMcpId": ""
        }))
        .is_ok()
    );
    for invalid_id in [
        serde_json::json!(1),
        serde_json::json!([]),
        serde_json::json!({}),
    ] {
        assert!(
            serde_json::from_value::<McpProjectionRecoveryResolveRequest>(serde_json::json!({
                "managedMcpId": invalid_id
            }))
            .is_err()
        );
    }

    for forged_field in [
        "actor",
        "actorDigest",
        "correlationId",
        "taskId",
        "authority",
        "token",
        "config",
        "runtimeBinding",
        "path",
        "url",
    ] {
        let mut forged = request.clone();
        forged[forged_field] = serde_json::json!("forged");
        assert!(serde_json::from_value::<McpProjectionRecoveryResolveRequest>(forged).is_err());
    }

    let schema =
        serde_json::to_value(schemars::schema_for!(McpProjectionRecoveryResolveRequest)).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], serde_json::json!(["managedMcpId"]));
    assert_eq!(schema["properties"].as_object().unwrap().len(), 1);
    assert!(schema["properties"].get("managedMcpId").is_some());
}

#[test]
fn projection_recovery_resolve_response_is_minimal_and_uses_the_redacted_outcome() {
    let response = McpProjectionRecoveryResolveResponse {
        outcome: McpPlatformOutcome::success(McpProjectionRecoveryResolveResult {
            managed_mcp_id: "managed_1".to_string(),
            result: McpProjectionRecoveryResolveStatus::Resolved,
        }),
    };
    let value = serde_json::to_value(&response).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "outcome": {
                "status": "success",
                "value": {"managedMcpId": "managed_1", "result": "resolved"}
            }
        })
    );
    let decoded: McpProjectionRecoveryResolveResponse =
        serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);

    let error = serde_json::json!({
        "outcome": {
            "status": "error",
            "error": {
                "code": "projection_conflict",
                "message": "Recovery could not be resolved.",
                "retryable": false,
                "correlationId": "correlation_redacted",
                "details": {"type": "recovery_required"}
            }
        }
    });
    assert!(serde_json::from_value::<McpProjectionRecoveryResolveResponse>(error.clone()).is_ok());
    for forged_field in [
        "rawWitness",
        "token",
        "authority",
        "config",
        "runtimeBinding",
        "path",
        "url",
    ] {
        let mut forged = error.clone();
        forged["outcome"]["error"][forged_field] = serde_json::json!("sensitive");
        assert!(serde_json::from_value::<McpProjectionRecoveryResolveResponse>(forged).is_err());
    }

    let schema =
        serde_json::to_string(&schemars::schema_for!(McpProjectionRecoveryResolveResponse))
            .unwrap();
    for forbidden in [
        "rawWitness",
        "recoveryToken",
        "runtimeBinding",
        "rawConfig",
        "absolutePath",
        "rawUrl",
    ] {
        assert!(
            !schema.contains(forbidden),
            "projection recovery response schema exposes {forbidden}"
        );
    }
}

#[test]
fn governed_import_contract_accepts_only_local_manifest_or_directory_inputs() {
    let manifest = serde_json::json!({
        "source": {
            "type": "local_manifest",
            "filePath": "D:/catalogs/manifest.json"
        }
    });
    let directory = serde_json::json!({
        "source": {
            "type": "local_directory",
            "directoryPath": "D:/catalogs/source-alpha"
        }
    });

    assert!(serde_json::from_value::<McpGovernedImportRequest>(manifest.clone()).is_ok());
    assert!(serde_json::from_value::<McpGovernedImportRequest>(directory.clone()).is_ok());

    for forbidden in ["url", "endpoint", "command", "argv", "env", "headers"] {
        let mut forged = manifest.clone();
        forged["source"][forbidden] = serde_json::json!("forged");
        assert!(serde_json::from_value::<McpGovernedImportRequest>(forged).is_err());
    }

    let https = serde_json::json!({
        "source": {
            "type": "https_manifest",
            "url": "https://catalog.example.test/manifest.json"
        }
    });
    assert!(serde_json::from_value::<McpGovernedImportRequest>(https).is_err());

    let schema = serde_json::to_string(&schemars::schema_for!(McpGovernedImportRequest)).unwrap();
    for forbidden in ["endpoint", "command", "argv", "env", "headers"] {
        assert!(
            !schema.contains(forbidden),
            "governed import schema contains {forbidden}"
        );
    }
}

#[test]
fn source_refresh_contract_only_accepts_registered_source_ids() {
    let valid = serde_json::json!({
        "sourceId": "verified_source_catalog_source-alpha"
    });
    assert!(serde_json::from_value::<McpSourceRefreshRequest>(valid.clone()).is_ok());

    for forbidden in ["endpoint", "url", "command", "argv", "env", "secret"] {
        let mut forged = valid.clone();
        forged[forbidden] = serde_json::json!("forged");
        assert!(serde_json::from_value::<McpSourceRefreshRequest>(forged).is_err());
    }

    let schema = serde_json::to_string(&schemars::schema_for!(McpSourceRefreshRequest)).unwrap();
    for forbidden in ["endpoint", "url", "command", "argv", "env", "secret"] {
        assert!(
            !schema.contains(forbidden),
            "source refresh schema contains {forbidden}"
        );
    }
}

#[test]
fn source_provisioning_contract_is_closed_and_bounds_all_confirmation_inputs() {
    let prepare = serde_json::json!({"localDirectory":"D:/catalogs/source-alpha"});
    assert!(serde_json::from_value::<McpSourceProvisionPrepareRequest>(prepare.clone()).is_ok());
    for forbidden in ["endpoint", "url", "transport", "actor", "command", "env"] {
        let mut forged = prepare.clone();
        forged[forbidden] = serde_json::json!("forged");
        assert!(serde_json::from_value::<McpSourceProvisionPrepareRequest>(forged).is_err());
    }
    assert!(
        serde_json::from_value::<McpSourceProvisionPrepareRequest>(serde_json::json!({
            "localDirectory": "x".repeat(4097)
        }))
        .is_err()
    );

    let confirm = serde_json::json!({
        "provisionId":"source_provisioning_1",
        "confirmationToken":"source_provisioning_confirmation_1",
        "confirm":true
    });
    assert!(serde_json::from_value::<McpSourceProvisionConfirmRequest>(confirm.clone()).is_ok());
    let mut missing = confirm.clone();
    missing.as_object_mut().unwrap().remove("confirm");
    assert!(serde_json::from_value::<McpSourceProvisionConfirmRequest>(missing).is_err());
    let mut forged = confirm;
    forged["endpoint"] = serde_json::json!("https://untrusted.example");
    assert!(serde_json::from_value::<McpSourceProvisionConfirmRequest>(forged).is_err());

    let schema =
        serde_json::to_string(&schemars::schema_for!(McpSourceProvisionPrepareRequest)).unwrap();
    for forbidden in ["endpoint", "url", "transport", "command", "env"] {
        assert!(
            !schema.contains(forbidden),
            "source provisioning schema contains {forbidden}"
        );
    }
}

#[test]
fn profile_contracts_do_not_accept_managed_config_or_mutation_escape_hatches() {
    let create = serde_json::json!({
        "name":"Coding",
        "description":"Installed tools only",
        "managedMcpIds":["managed_1"],
        "idempotencyKey":"profile-create-1"
    });
    assert!(serde_json::from_value::<McpProfileCreateRequest>(create.clone()).is_ok());
    for forbidden in [
        "enabledExtensions",
        "mcpServers",
        "provider",
        "model",
        "actor",
    ] {
        let mut forged = create.clone();
        forged[forbidden] = serde_json::json!("forged");
        assert!(serde_json::from_value::<McpProfileCreateRequest>(forged).is_err());
    }

    let draft = serde_json::json!({"text":"use github for coding","locale":"en-US"});
    assert!(serde_json::from_value::<McpProfileDraftCreateRequest>(draft.clone()).is_ok());
    for forbidden in ["install", "enable", "network", "providerCall", "persist"] {
        let mut forged = draft.clone();
        forged[forbidden] = serde_json::json!(true);
        assert!(serde_json::from_value::<McpProfileDraftCreateRequest>(forged).is_err());
    }

    let confirm = serde_json::json!({
        "planId":"profile_plan_1",
        "confirmationToken":"profile_apply_confirmation_1",
        "confirm":true
    });
    assert!(serde_json::from_value::<McpProfileApplyConfirmRequest>(confirm.clone()).is_ok());
    let mut without_decision = confirm.clone();
    without_decision.as_object_mut().unwrap().remove("confirm");
    assert!(serde_json::from_value::<McpProfileApplyConfirmRequest>(without_decision).is_err());
    let mut forged = confirm;
    forged["enabledExtensions"] = serde_json::json!([]);
    assert!(serde_json::from_value::<McpProfileApplyConfirmRequest>(forged).is_err());
}

#[test]
fn profile_apply_public_views_do_not_expose_internal_digests_or_snapshot_metadata() {
    let plan_schema = serde_json::to_string(&schemars::schema_for!(McpProfileApplyPlan)).unwrap();
    for forbidden in [
        "planDigest",
        "manifestDigest",
        "projectionDigest",
        "managedRevision",
        "projectionRevision",
        "authEvidenceDigest",
        "policyEvidenceDigest",
        "actor",
        "createdAtMs",
    ] {
        assert!(
            !plan_schema.contains(forbidden),
            "profile apply plan schema exposes {forbidden}"
        );
    }

    let token_schema =
        serde_json::to_string(&schemars::schema_for!(McpProfileApplicationToken)).unwrap();
    assert!(!token_schema.contains("planDigest"));
}

#[test]
fn manual_add_contract_accepts_only_semantic_http_or_opaque_stdio_inputs() {
    let remote = serde_json::json!({
        "connection": {
            "type": "remote_http",
            "endpoint": "https://mcp.example.com/v1",
            "auth": {"type":"bearer_reference","authReference":"credential_1"}
        },
        "idempotencyKey": "manual-http-1"
    });
    assert!(serde_json::from_value::<McpManualPlanCreateRequest>(remote.clone()).is_ok());
    for forbidden in ["headers", "command", "argv", "env", "cwd", "shell"] {
        let mut forged = remote.clone();
        forged["connection"][forbidden] = serde_json::json!("forged");
        assert!(serde_json::from_value::<McpManualPlanCreateRequest>(forged).is_err());
    }

    let stdio = serde_json::json!({
        "connection":{"type":"stdio_provider","sourceId":"provider_item_1"},
        "idempotencyKey":"manual-stdio-1"
    });
    assert!(serde_json::from_value::<McpManualPlanCreateRequest>(stdio.clone()).is_ok());
    for forbidden in ["executable", "argv", "args", "env", "cwd", "command"] {
        let mut forged = stdio.clone();
        forged["connection"][forbidden] = serde_json::json!("forged");
        assert!(serde_json::from_value::<McpManualPlanCreateRequest>(forged).is_err());
    }

    let schema = serde_json::to_string(&schemars::schema_for!(McpManualPlanCreateRequest)).unwrap();
    for forbidden in ["executable", "argv", "headers", "credentialValue", "cwd"] {
        assert!(
            !schema.contains(forbidden),
            "manual schema contains {forbidden}"
        );
    }
}

#[test]
fn plan_create_contract_accepts_source_bound_catalog_targets_and_rejects_mixed_shapes() {
    let register_catalog = serde_json::json!({
        "intent": {
            "type": "register_catalog",
            "catalog": {
                "sourceId": "verified_source_catalog_source-a",
                "mcpId": "pkg.source.bound",
                "version": "1.2.3",
                "manifestDigest": "a".repeat(64)
            },
            "installation_scope": "user"
        },
        "idempotencyKey": "plan-register-catalog"
    });
    let request: McpPlanCreateRequest = serde_json::from_value(register_catalog.clone()).unwrap();
    let McpPlanIntent::RegisterCatalog {
        ref catalog,
        installation_scope,
    } = request.intent
    else {
        panic!("expected register_catalog intent");
    };
    assert_eq!(catalog.source_id, "verified_source_catalog_source-a");
    assert_eq!(catalog.mcp_id, "pkg.source.bound");
    assert_eq!(catalog.version, "1.2.3");
    assert_eq!(catalog.manifest_digest, "a".repeat(64));
    assert_eq!(installation_scope, Some(McpInstallationScope::User));
    assert_eq!(serde_json::to_value(request).unwrap(), register_catalog);

    let install_catalog = serde_json::json!({
        "intent": {
            "type": "install_catalog",
            "catalog": {
                "sourceId": "verified_source_catalog_source-b",
                "mcpId": "pkg.source.bound",
                "version": "2.0.0",
                "manifestDigest": "b".repeat(64)
            }
        },
        "idempotencyKey": "plan-install-catalog"
    });
    let request: McpPlanCreateRequest = serde_json::from_value(install_catalog.clone()).unwrap();
    let McpPlanIntent::InstallCatalog { ref catalog } = request.intent else {
        panic!("expected install_catalog intent");
    };
    assert_eq!(catalog.source_id, "verified_source_catalog_source-b");
    assert_eq!(catalog.manifest_digest, "b".repeat(64));
    assert_eq!(serde_json::to_value(request).unwrap(), install_catalog);

    let mut mixed_legacy = serde_json::json!({
        "intent": {
            "type": "register",
            "manifestDigest": "c".repeat(64),
            "installation_scope": "user"
        },
        "idempotencyKey": "plan-legacy-register"
    });
    mixed_legacy["intent"]["catalog"] = serde_json::json!({
        "sourceId": "verified_source_catalog_source-c",
        "mcpId": "pkg.source.bound",
        "version": "3.0.0",
        "manifestDigest": "c".repeat(64)
    });
    assert!(serde_json::from_value::<McpPlanCreateRequest>(mixed_legacy).is_err());

    let mut missing_digest = register_catalog.clone();
    missing_digest["intent"]["catalog"]
        .as_object_mut()
        .unwrap()
        .remove("manifestDigest");
    assert!(serde_json::from_value::<McpPlanCreateRequest>(missing_digest).is_err());

    let mut forged_catalog = install_catalog;
    forged_catalog["intent"]["catalog"]["path"] = serde_json::json!("C:/escape");
    assert!(serde_json::from_value::<McpPlanCreateRequest>(forged_catalog).is_err());
}

#[test]
fn public_contract_does_not_expose_auth_header_or_environment_key() {
    let schema = serde_json::to_string(&schemars::schema_for!(McpCatalogDetail)).unwrap();
    for forbidden in [
        "headerName",
        "environmentKey",
        "credentialName",
        "credentialValue",
        "oauthCode",
    ] {
        assert!(
            !schema.contains(forbidden),
            "catalog detail exposes {forbidden}"
        );
    }
    let task_schema = serde_json::to_string(&schemars::schema_for!(McpTaskRef)).unwrap();
    for forbidden in [
        "stderr",
        "stack",
        "command",
        "absolutePath",
        "daemonAddress",
    ] {
        assert!(
            !task_schema.contains(forbidden),
            "task contract exposes {forbidden}"
        );
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

    let witness = serde_json::json!({
        "outcome": {
            "status": "error",
            "error": {
                "code": "rollback_incomplete",
                "message": "Recovery required.",
                "retryable": false,
                "correlationId": "correlation_witness",
                "details": {
                    "type": "witness_expired"
                }
            }
        }
    });
    let decoded: McpTaskGetResponse = serde_json::from_value(witness.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), witness);

    let mut forged_witness = witness;
    forged_witness["outcome"]["error"]["details"]["rawWitness"] = serde_json::json!("secret");
    assert!(serde_json::from_value::<McpTaskGetResponse>(forged_witness).is_err());
}

#[test]
fn phase_capabilities_round_trip_available_values() {
    let value = serde_json::json!({
        "sessionEnablement": "available",
        "toolPolicy": "not_available_in_this_phase",
        "profiles": "available",
        "modelSuggestions": "available"
    });

    let capabilities: McpPhaseCapabilities = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        capabilities.session_enablement,
        McpPhaseCapability::Available
    );
    assert_eq!(
        capabilities.tool_policy,
        McpPhaseCapability::NotAvailableInThisPhase
    );
    assert_eq!(capabilities.profiles, McpPhaseCapability::Available);
    assert_eq!(
        capabilities.model_suggestions,
        McpPhaseCapability::Available
    );
    assert_eq!(serde_json::to_value(capabilities).unwrap(), value);
}

#[test]
fn credential_status_contract_exposes_only_stable_public_values() {
    let statuses = [
        ("unconfigured", McpCredentialStatus::Unconfigured),
        (
            "re_registration_required",
            McpCredentialStatus::ReRegistrationRequired,
        ),
        (
            "trusted_state_conflict",
            McpCredentialStatus::TrustedStateConflict,
        ),
        (
            "temporarily_unavailable",
            McpCredentialStatus::TemporarilyUnavailable,
        ),
        ("ready", McpCredentialStatus::Ready),
    ];

    for (wire, expected) in statuses {
        let decoded: McpCredentialStatus = serde_json::from_value(serde_json::json!(wire)).unwrap();
        assert_eq!(decoded, expected);
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::json!(wire)
        );
    }
}

#[test]
fn credential_status_optional_fields_preserve_legacy_shapes_and_hide_sensitive_terms() {
    let legacy_health = serde_json::json!({
        "managedMcpId": "managed_1",
        "state": "healthy",
        "latest": null
    });
    let decoded: McpHealthStatus = serde_json::from_value(legacy_health.clone()).unwrap();
    assert!(decoded.credential_status.is_none());
    assert_eq!(serde_json::to_value(decoded).unwrap(), legacy_health);

    let profile = McpProfileSummary {
        profile_id: "profile_1".to_string(),
        name: "Coding".to_string(),
        description: "Installed tools".to_string(),
        revision: 4,
        archived: false,
        entries: vec![],
        created_at_ms: 1,
        updated_at_ms: 2,
        credential_status: Some(McpCredentialStatus::TrustedStateConflict),
    };
    let health = McpHealthStatus {
        managed_mcp_id: "managed_1".to_string(),
        state: McpHealthState::Healthy,
        latest: None,
        credential_status: Some(McpCredentialStatus::ReRegistrationRequired),
    };

    for serialized in [
        serde_json::to_string(&profile).unwrap(),
        serde_json::to_string(&health).unwrap(),
    ] {
        for forbidden in ["secret", "handle", "digest", "witness", "keyring", "anchor"] {
            assert!(
                !serialized.contains(forbidden),
                "credential status projection leaked {forbidden}: {serialized}"
            );
        }
    }
}

#[test]
fn runtime_control_request_is_closed_and_requires_a_binding() {
    let request = serde_json::json!({
        "managedMcpId": "managed_1",
        "expectedRevision": 4,
        "action": "stop",
        "runtimeBinding": "trusted-binding"
    });
    let decoded: McpRuntimeControlRequest = serde_json::from_value(request.clone()).unwrap();
    assert_eq!(decoded.action, McpRuntimeAction::Stop);
    assert_eq!(serde_json::to_value(decoded).unwrap(), request);

    for field in ["runtimeBinding", "expectedRevision", "action"] {
        let mut missing = request.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<McpRuntimeControlRequest>(missing).is_err());
    }
    let mut forged = request;
    forged["pid"] = serde_json::json!(1234);
    assert!(serde_json::from_value::<McpRuntimeControlRequest>(forged).is_err());
}

fn source_adapter_descriptor() -> serde_json::Value {
    serde_json::json!({
        "displayName": "npm registry",
        "sourceKind": {"type": "npm"},
        "trustMode": "verified",
        "availability": {"status": "ready_for_intake"},
        "requiredApprovals": [],
        "risks": ["network_access", "process_execution", "host_dependency", "credential_reference"],
        "requiredHostDependencies": ["node"],
        "allowedTransports": ["catalog", "stdio_provider"],
        "inputFields": {
            "package": {
                "label": "Package",
                "kind": "text",
                "required": true,
                "multiline": false,
                "secretReferenceOnly": false
            },
            "credential": {
                "label": "Credential reference",
                "kind": "reference",
                "required": false,
                "multiline": false,
                "secretReferenceOnly": true
            }
        }
    })
}

fn source_adapters_list_result() -> serde_json::Value {
    serde_json::json!({"adapters": {"registry.npm": source_adapter_descriptor()}})
}

#[test]
fn source_adapter_descriptor_constructor_is_the_external_construction_boundary() {
    let inbound =
        serde_json::from_value::<McpSourceAdapterDescriptor>(source_adapter_descriptor()).unwrap();
    let constructed = McpSourceAdapterDescriptor::new(
        inbound.display_name.clone(),
        inbound.source_kind.clone(),
        inbound.trust_mode,
        inbound.availability,
        inbound.required_approvals.clone(),
        inbound.risks.clone(),
        inbound.required_host_dependencies.clone(),
        inbound.allowed_transports.clone(),
        inbound.input_fields.clone(),
    )
    .unwrap();

    // `McpSourceAdapterDescriptor` is non_exhaustive, so this equivalent struct literal is
    // rejected for callers outside goose_sdk_types; they must use the validated constructor.
    let encoded = serde_json::to_value(&constructed).unwrap();
    let decoded: McpSourceAdapterDescriptor = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
}

#[test]
fn source_adapter_descriptor_serialization_revalidates_mutated_values() {
    let valid = || {
        serde_json::from_value::<McpSourceAdapterDescriptor>(source_adapter_descriptor()).unwrap()
    };

    let mut ready_with_approval = valid();
    ready_with_approval.required_approvals = vec![McpSourceAdapterApproval::NetworkAccess];
    assert!(serde_json::to_value(ready_with_approval).is_err());

    let mut approval_without_risk = valid();
    approval_without_risk.availability = McpSourceAdapterAvailability::Blocked {
        reason: McpSourceAdapterBlockedReason::ApprovalRequired,
    };
    approval_without_risk.required_approvals = vec![McpSourceAdapterApproval::NetworkAccess];
    approval_without_risk
        .risks
        .retain(|risk| *risk != McpSourceAdapterRisk::NetworkAccess);
    assert!(serde_json::to_value(approval_without_risk).is_err());

    let mut opaque_known_kind = valid();
    opaque_known_kind.source_kind = McpSourceAdapterSourceKind::Opaque {
        kind_id: "npm".to_string(),
    };
    opaque_known_kind.trust_mode = McpSourceAdapterTrustMode::Unverified;
    opaque_known_kind.availability = McpSourceAdapterAvailability::Blocked {
        reason: McpSourceAdapterBlockedReason::Unverified,
    };
    opaque_known_kind.required_approvals.clear();
    opaque_known_kind.risks = vec![McpSourceAdapterRisk::ProcessExecution];
    opaque_known_kind.required_host_dependencies.clear();
    opaque_known_kind.allowed_transports.clear();
    opaque_known_kind.input_fields.clear();
    assert!(serde_json::to_value(opaque_known_kind).is_err());

    let mut invalid_input_map = valid();
    invalid_input_map.input_fields.insert(
        "invalid/id".to_string(),
        McpSourceAdapterInputField {
            label: "Input".to_string(),
            kind: McpSourceAdapterInputFieldKind::Text,
            required: false,
            multiline: false,
            secret_reference_only: false,
        },
    );
    assert!(serde_json::to_value(invalid_input_map).is_err());
}

#[test]
fn source_adapters_list_contract_is_discovery_only_and_round_trips() {
    let request: McpSourceAdaptersListRequest =
        serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        serde_json::json!({})
    );

    let response_value = serde_json::json!({
        "outcome": {
            "status": "success",
            "value": source_adapters_list_result()
        }
    });
    let response: McpSourceAdaptersListResponse =
        serde_json::from_value(response_value.clone()).unwrap();
    assert_eq!(serde_json::to_value(response).unwrap(), response_value);

    let response = McpSourceAdaptersListResponse {
        outcome: McpSourceAdaptersListOutcome::error(McpSourceAdaptersListError::Unavailable),
    };
    let value = serde_json::to_value(response).unwrap();
    assert!(serde_json::from_value::<McpSourceAdaptersListResponse>(value).is_ok());
}

#[test]
fn source_adapter_schema_and_response_round_trip_const_generic_matrix_lengths() {
    let adapters = serde_json::json!({
        "registry.catalog": {
            "displayName": "Catalog",
            "sourceKind": {"type": "catalog"},
            "trustMode": "verified",
            "availability": {"status": "ready_for_intake"},
            "requiredApprovals": [],
            "risks": ["network_access"],
            "requiredHostDependencies": [],
            "allowedTransports": ["catalog"],
            "inputFields": {}
        },
        "registry.npm": source_adapter_descriptor()
    });
    let value = serde_json::json!({
        "outcome": {"status": "success", "value": {"adapters": adapters}}
    });
    let decoded: McpSourceAdaptersListResponse = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
    let decoded = serde_json::from_value::<McpSourceAdaptersListResponse>(
        serde_json::to_value(decoded).unwrap(),
    )
    .unwrap();
    let McpSourceAdaptersListOutcome::Success { value } = decoded.outcome else {
        panic!("expected source adapter list success");
    };
    assert_eq!(value.adapters.len(), 2);

    let schema = serde_json::to_value(schemars::schema_for!(McpSourceAdapterDescriptor)).unwrap();
    let constraints = schema["allOf"].as_array().unwrap();
    let catalog = constraints
        .iter()
        .find(|constraint| {
            constraint["if"]["properties"]["sourceKind"]["properties"]["type"]["const"] == "catalog"
        })
        .unwrap();
    assert_eq!(
        catalog["then"]["properties"]["trustMode"]["enum"],
        serde_json::json!(["verified"])
    );
    assert_eq!(
        catalog["then"]["properties"]["risks"]["allOf"],
        serde_json::json!([{"contains": {"const": "network_access"}}])
    );

    let npm = constraints
        .iter()
        .find(|constraint| {
            constraint["if"]["properties"]["sourceKind"]["properties"]["type"]["const"] == "npm"
        })
        .unwrap();
    assert_eq!(
        npm["then"]["properties"]["trustMode"]["enum"],
        serde_json::json!(["verified", "user_managed"])
    );
    assert_eq!(
        npm["then"]["properties"]["risks"]["allOf"],
        serde_json::json!([
            {"contains": {"const": "process_execution"}},
            {"contains": {"const": "host_dependency"}}
        ])
    );
    assert_eq!(
        npm["then"]["properties"]["allowedTransports"]["items"]["enum"],
        serde_json::json!(["catalog", "stdio_provider"])
    );
    assert_eq!(
        npm["then"]["properties"]["allowedTransports"]["allOf"],
        serde_json::json!([{"contains": {"const": "stdio_provider"}}])
    );
}

#[test]
fn source_adapter_availability_is_closed_and_intake_safe() {
    let ready =
        serde_json::from_value::<McpSourceAdapterDescriptor>(source_adapter_descriptor()).unwrap();
    assert_eq!(
        serde_json::to_value(ready).unwrap()["availability"],
        serde_json::json!({"status": "ready_for_intake"})
    );

    let mut policy = source_adapter_descriptor();
    policy["availability"] = serde_json::json!({"status": "blocked", "reason": "policy"});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(policy).is_ok());

    for reason in ["runtime_dependency", "unsupported"] {
        let mut blocked_with_approvals = source_adapter_descriptor();
        blocked_with_approvals["availability"] =
            serde_json::json!({"status": "blocked", "reason": reason});
        blocked_with_approvals["requiredApprovals"] =
            serde_json::json!(["process_execution", "host_dependency"]);
        let decoded =
            serde_json::from_value::<McpSourceAdapterDescriptor>(blocked_with_approvals.clone())
                .unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            blocked_with_approvals
        );
    }

    let mut unverified = source_adapter_descriptor();
    unverified["displayName"] = serde_json::json!("Unverified source");
    unverified["sourceKind"] = serde_json::json!({"type": "opaque", "kindId": "unverified"});
    unverified["trustMode"] = serde_json::json!("unverified");
    unverified["requiredApprovals"] = serde_json::json!([]);
    unverified["risks"] = serde_json::json!(["process_execution"]);
    unverified["requiredHostDependencies"] = serde_json::json!([]);
    unverified["allowedTransports"] = serde_json::json!([]);
    unverified["inputFields"] = serde_json::json!({});
    unverified["availability"] = serde_json::json!({"status": "blocked", "reason": "unverified"});
    serde_json::from_value::<McpSourceAdapterDescriptor>(unverified.clone())
        .unwrap_or_else(|error| panic!("valid opaque descriptor rejected: {error}"));

    let mut opaque_network_baseline = unverified.clone();
    opaque_network_baseline["risks"] = serde_json::json!(["network_access"]);
    assert!(
        serde_json::from_value::<McpSourceAdapterDescriptor>(opaque_network_baseline.clone())
            .is_ok()
    );

    let mut opaque_approval = opaque_network_baseline.clone();
    opaque_approval["requiredApprovals"] = serde_json::json!(["network_access"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(opaque_approval).is_err());

    let mut false_unverified = source_adapter_descriptor();
    false_unverified["availability"] =
        serde_json::json!({"status": "blocked", "reason": "unverified"});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(false_unverified).is_err());

    let mut approval_gated = source_adapter_descriptor();
    approval_gated["availability"] =
        serde_json::json!({"status": "blocked", "reason": "approval_required"});
    approval_gated["requiredApprovals"] = serde_json::json!([
        "credential_reference",
        "network_access",
        "process_execution"
    ]);
    let normalized = serde_json::from_value::<McpSourceAdapterDescriptor>(approval_gated).unwrap();
    assert_eq!(
        serde_json::to_value(normalized).unwrap()["requiredApprovals"],
        serde_json::json!([
            "network_access",
            "process_execution",
            "credential_reference"
        ])
    );

    let mut ready_with_approval = source_adapter_descriptor();
    ready_with_approval["requiredApprovals"] = serde_json::json!(["network_access"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(ready_with_approval).is_err());

    let mut approval_without_requirement = source_adapter_descriptor();
    approval_without_requirement["availability"] =
        serde_json::json!({"status": "blocked", "reason": "approval_required"});
    assert!(
        serde_json::from_value::<McpSourceAdapterDescriptor>(approval_without_requirement).is_err()
    );

    let mut requirement_with_other_block = source_adapter_descriptor();
    requirement_with_other_block["availability"] =
        serde_json::json!({"status": "blocked", "reason": "policy"});
    requirement_with_other_block["requiredApprovals"] = serde_json::json!(["network_access"]);
    let policy_with_future_approval =
        serde_json::from_value::<McpSourceAdapterDescriptor>(requirement_with_other_block).unwrap();
    assert_eq!(
        serde_json::to_value(policy_with_future_approval).unwrap()["availability"],
        serde_json::json!({"status": "blocked", "reason": "policy"})
    );
}

#[test]
fn source_adapter_required_approvals_are_known_unique_and_match_risks() {
    let mut manual = source_adapter_descriptor();
    manual["sourceKind"] = serde_json::json!({"type": "manual_stdio"});
    manual["trustMode"] = serde_json::json!("user_managed");
    manual["risks"] = serde_json::json!([
        "process_execution",
        "filesystem_read",
        "filesystem_write",
        "credential_reference"
    ]);
    manual["requiredHostDependencies"] = serde_json::json!([]);
    manual["allowedTransports"] = serde_json::json!(["stdio_provider"]);
    manual["availability"] =
        serde_json::json!({"status": "blocked", "reason": "approval_required"});
    manual["requiredApprovals"] = serde_json::json!([
        "filesystem_write",
        "process_execution",
        "filesystem_read",
        "credential_reference"
    ]);
    let normalized = serde_json::from_value::<McpSourceAdapterDescriptor>(manual).unwrap();
    assert_eq!(
        serde_json::to_value(normalized).unwrap()["requiredApprovals"],
        serde_json::json!([
            "process_execution",
            "filesystem_read",
            "filesystem_write",
            "credential_reference"
        ])
    );

    let mut duplicate = source_adapter_descriptor();
    duplicate["availability"] =
        serde_json::json!({"status": "blocked", "reason": "approval_required"});
    duplicate["requiredApprovals"] = serde_json::json!(["network_access", "network_access"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(duplicate).is_err());

    let mut unknown = source_adapter_descriptor();
    unknown["availability"] =
        serde_json::json!({"status": "blocked", "reason": "approval_required"});
    unknown["requiredApprovals"] = serde_json::json!(["browser_automation"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(unknown).is_err());

    let mut mismatched = source_adapter_descriptor();
    mismatched["availability"] =
        serde_json::json!({"status": "blocked", "reason": "approval_required"});
    mismatched["requiredApprovals"] = serde_json::json!(["filesystem_read"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(mismatched).is_err());

    for invalid_availability in [
        serde_json::json!({"status": "available"}),
        serde_json::json!({"status": "blocked"}),
        serde_json::json!({"status": "blocked", "reason": "policy", "approved": true}),
        serde_json::json!({"status": "ready_for_intake", "reason": "policy"}),
        serde_json::json!({"status": "blocked", "reason": "diagnostic text"}),
    ] {
        let mut adapter = source_adapter_descriptor();
        adapter["availability"] = invalid_availability;
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());
    }
}

#[test]
fn source_adapters_list_error_is_closed_and_non_diagnostic() {
    let error = serde_json::json!({
        "outcome": {"status": "error", "error": "policy_denied"}
    });
    assert!(serde_json::from_value::<McpSourceAdaptersListResponse>(error).is_ok());

    for forbidden in [
        "message",
        "correlationId",
        "details",
        "token",
        "url",
        "path",
        "rawConfig",
        "default",
        "html",
    ] {
        let error = serde_json::json!({
            "outcome": {
                "status": "error",
                "error": {forbidden: "https://attacker.invalid/$TOKEN<script>"}
            }
        });
        assert!(serde_json::from_value::<McpSourceAdaptersListResponse>(error).is_err());
    }

    let schema =
        serde_json::to_string(&schemars::schema_for!(McpSourceAdaptersListResponse)).unwrap();
    for forbidden in [
        "message",
        "correlationId",
        "details",
        "rawConfig",
        "token",
        "url",
    ] {
        assert!(
            !schema.contains(forbidden),
            "discovery error schema exposes {forbidden}"
        );
    }
}

#[test]
fn source_adapters_list_wire_shape_is_closed_at_every_level() {
    for invalid_root in [
        serde_json::Value::Null,
        serde_json::json!(1),
        serde_json::json!([]),
        serde_json::json!("adapter"),
    ] {
        assert!(serde_json::from_value::<McpSourceAdaptersListRequest>(invalid_root).is_err());
    }

    let mut request = serde_json::json!({});
    request["execute"] = serde_json::json!(true);
    assert!(serde_json::from_value::<McpSourceAdaptersListRequest>(request).is_err());

    let mut response = serde_json::json!({
        "outcome": {"status": "success", "value": source_adapters_list_result()}
    });
    response["extra"] = serde_json::json!("not echoed");
    assert!(serde_json::from_value::<McpSourceAdaptersListResponse>(response).is_err());

    for (path, value) in [
        ("outcome", serde_json::json!(null)),
        ("outcome.status", serde_json::json!("unsafe")),
        (
            "outcome.value.adapters.registry.npm",
            serde_json::json!(null),
        ),
    ] {
        let mut response = serde_json::json!({
            "outcome": {"status": "success", "value": source_adapters_list_result()}
        });
        match path {
            "outcome" => response["outcome"] = value,
            "outcome.status" => response["outcome"]["status"] = value,
            "outcome.value.adapters" => response["outcome"]["value"]["adapters"] = value,
            "outcome.value.adapters.registry.npm" => {
                response["outcome"]["value"]["adapters"]["registry.npm"] = value
            }
            _ => unreachable!(),
        }
        assert!(serde_json::from_value::<McpSourceAdaptersListResponse>(response).is_err());
    }

    let empty = serde_json::json!({
        "outcome": {"status": "success", "value": {"adapters": {}}}
    });
    let parsed = serde_json::from_value::<McpSourceAdaptersListResponse>(empty).unwrap();
    assert_eq!(
        serde_json::to_value(parsed).unwrap()["outcome"]["value"]["adapters"],
        serde_json::json!({})
    );

    let mut unknown_result_field = serde_json::json!({"adapters": {}});
    unknown_result_field["futureField"] = serde_json::json!(true);
    assert!(serde_json::from_value::<McpSourceAdaptersListResult>(unknown_result_field).is_err());

    let mut dynamic_adapter =
        serde_json::json!({"adapters": {"future.adapter": source_adapter_descriptor()}});
    assert!(serde_json::from_value::<McpSourceAdaptersListResult>(dynamic_adapter.take()).is_ok());

    let mut nested = source_adapter_descriptor();
    nested["command"] = serde_json::json!("forged");
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(nested).is_err());
    let mut nested = source_adapter_descriptor();
    nested["inputFields"]["package"]["rawConfig"] = serde_json::json!({"forged": true});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(nested).is_err());
}

#[test]
fn source_adapter_descriptors_reject_unsafe_or_ambiguous_values() {
    for invalid_id in [
        "",
        "Npm",
        "npm/registry",
        "npm registry",
        "1npm",
        "npm\nregistry",
    ] {
        let mut result = source_adapters_list_result();
        let adapter = result["adapters"]["registry.npm"].take();
        result["adapters"]
            .as_object_mut()
            .unwrap()
            .remove("registry.npm");
        result["adapters"][invalid_id] = adapter;
        assert!(serde_json::from_value::<McpSourceAdaptersListResult>(result).is_err());
    }

    for invalid_field_id in ["", "secret/value", "1credential"] {
        let mut adapter = source_adapter_descriptor();
        let field = adapter["inputFields"]["package"].take();
        adapter["inputFields"]
            .as_object_mut()
            .unwrap()
            .remove("package");
        adapter["inputFields"][invalid_field_id] = field;
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());
    }

    for unsafe_kind in ["command", "shell", "url", "token", "raw_config"] {
        let mut adapter = source_adapter_descriptor();
        adapter["inputFields"]["package"]["kind"] = serde_json::json!(unsafe_kind);
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());
    }

    let mut adapter = source_adapter_descriptor();
    adapter["sourceKind"] = serde_json::json!({"type": "unrecognized_runtime"});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut adapter = source_adapter_descriptor();
    adapter["sourceKind"] = serde_json::json!({"type": "opaque", "kindId": "bad/kind"});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut adapter = source_adapter_descriptor();
    adapter["trustMode"] = serde_json::json!("execute");
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut adapter = source_adapter_descriptor();
    adapter["allowedTransports"][0] = serde_json::json!("stdio");
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut adapter = source_adapter_descriptor();
    adapter["requiredHostDependencies"] = serde_json::json!(["node", "node"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut adapter = source_adapter_descriptor();
    adapter["risks"] = serde_json::json!(["network_access", "network_access"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut adapter = source_adapter_descriptor();
    adapter["risks"] = serde_json::json!([
        "credential_reference",
        "process_execution",
        "network_access",
        "host_dependency"
    ]);
    let normalized = serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).unwrap();
    assert_eq!(
        serde_json::to_value(normalized).unwrap()["risks"],
        serde_json::json!([
            "network_access",
            "process_execution",
            "host_dependency",
            "credential_reference"
        ])
    );

    let mut adapter = source_adapter_descriptor();
    adapter["allowedTransports"] = serde_json::json!(["stdio_provider", "catalog"]);
    let normalized = serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).unwrap();
    assert_eq!(
        serde_json::to_value(normalized).unwrap()["allowedTransports"],
        serde_json::json!(["catalog", "stdio_provider"])
    );

    for field in [
        "default",
        "command",
        "url",
        "path",
        "token",
        "rawConfig",
        "secret",
    ] {
        let mut adapter = source_adapter_descriptor();
        adapter["inputFields"]["credential"][field] = serde_json::json!("forged");
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());
    }

    let mut adapter = source_adapter_descriptor();
    adapter["inputFields"]["package"]["secretReferenceOnly"] = serde_json::json!(true);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut adapter = source_adapter_descriptor();
    adapter["risks"] =
        serde_json::json!(["network_access", "process_execution", "host_dependency"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut adapter = source_adapter_descriptor();
    adapter["risks"] = serde_json::json!([
        "network_access",
        "process_execution",
        "host_dependency",
        "credential_reference"
    ]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_ok());

    let mut adapter = source_adapter_descriptor();
    adapter["inputFields"]["credential"]["multiline"] = serde_json::json!(true);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());
}

#[test]
fn source_adapter_display_text_is_a_conservative_transport_boundary() {
    for field in [
        "displayName",
        "inputFields.package.label",
        "inputFields.credential.label",
    ] {
        let valid = source_adapter_descriptor();
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(valid).is_ok());

        for unsafe_text in [
            "https://attacker.invalid/path",
            "C:\\secrets\\token",
            "/var/run/token",
            "<script>alert(1)</script>",
            "$(whoami)",
            "sk-1234567890abcdef",
            "ghp_abc_defg",
            "github_pat_123456789",
            "AKIA1234-5678",
            "Bearer abcdefghijklmnop",
            "Bearer abc defghijkl",
            "default=https://attacker.invalid",
            "{\"rawConfig\":\"forged\"}",
            " normal",
            "normal  text",
            "normal..text",
            "R\u{00e9}sum\u{00e9}",
        ] {
            let mut adapter = source_adapter_descriptor();
            match field {
                "displayName" => adapter["displayName"] = serde_json::json!(unsafe_text),
                "inputFields.package.label" => {
                    adapter["inputFields"]["package"]["label"] = serde_json::json!(unsafe_text)
                }
                "inputFields.credential.label" => {
                    adapter["inputFields"]["credential"]["label"] = serde_json::json!(unsafe_text)
                }
                _ => unreachable!(),
            }
            assert!(
                serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err(),
                "{field} accepted {unsafe_text:?}"
            );
        }
    }
}

#[test]
fn source_adapter_identifier_text_never_accepts_raw_payloads() {
    for unsafe_text in [
        "https://attacker.invalid/token",
        "C:\\secrets\\token",
        "/var/run/token",
        "<script>alert(1)</script>",
        "$(whoami)",
        "default=https://attacker.invalid",
        "{\"rawConfig\":\"forged\"}",
    ] {
        let mut result = source_adapters_list_result();
        let adapter = result["adapters"]["registry.npm"].take();
        result["adapters"]
            .as_object_mut()
            .unwrap()
            .remove("registry.npm");
        result["adapters"][unsafe_text] = adapter;
        assert!(serde_json::from_value::<McpSourceAdaptersListResult>(result).is_err());

        let mut adapter = source_adapter_descriptor();
        adapter["requiredHostDependencies"][0] = serde_json::json!(unsafe_text);
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

        let mut adapter = source_adapter_descriptor();
        let field = adapter["inputFields"]["package"].take();
        adapter["inputFields"]
            .as_object_mut()
            .unwrap()
            .remove("package");
        adapter["inputFields"][unsafe_text] = field;
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());
    }
}

#[test]
fn source_adapter_string_rules_reject_crlf_at_runtime_and_in_exported_schema() {
    for invalid_id in ["registry.npm\n", "registry.npm\r"] {
        let mut result = source_adapters_list_result();
        let adapter = result["adapters"]["registry.npm"].take();
        result["adapters"]
            .as_object_mut()
            .unwrap()
            .remove("registry.npm");
        result["adapters"][invalid_id] = adapter;
        assert!(serde_json::from_value::<McpSourceAdaptersListResult>(result).is_err());

        let mut adapter = source_adapter_descriptor();
        adapter["requiredHostDependencies"][0] = serde_json::json!(invalid_id);
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

        let mut adapter = source_adapter_descriptor();
        adapter["sourceKind"] = serde_json::json!({"type": "opaque", "kindId": invalid_id});
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

        let mut adapter = source_adapter_descriptor();
        let field = adapter["inputFields"]["package"].take();
        adapter["inputFields"]
            .as_object_mut()
            .unwrap()
            .remove("package");
        adapter["inputFields"][invalid_id] = field;
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());
    }

    for invalid_text in ["npm registry\n", "npm registry\r"] {
        let mut adapter = source_adapter_descriptor();
        adapter["displayName"] = serde_json::json!(invalid_text);
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

        let mut adapter = source_adapter_descriptor();
        adapter["inputFields"]["package"]["label"] = serde_json::json!(invalid_text);
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());
    }

    let descriptor =
        serde_json::to_value(schemars::schema_for!(McpSourceAdapterDescriptor)).unwrap();
    let identifier = &descriptor["properties"]["inputFields"]["propertyNames"];
    assert_eq!(identifier["allOf"][0]["not"]["pattern"], "[\\r\\n]");
    assert_eq!(
        descriptor["properties"]["requiredHostDependencies"]["items"]["allOf"][0]["not"]["pattern"],
        "[\\r\\n]"
    );

    let result = serde_json::to_value(schemars::schema_for!(McpSourceAdaptersListResult)).unwrap();
    assert_eq!(
        result["properties"]["adapters"]["propertyNames"]["allOf"][0]["not"]["pattern"],
        "[\\r\\n]"
    );

    let display_name = &descriptor["properties"]["displayName"];
    assert_eq!(display_name["allOf"][0]["not"]["pattern"], "[\\r\\n]");

    let input = serde_json::to_value(schemars::schema_for!(McpSourceAdapterInputField)).unwrap();
    for variant in input["oneOf"].as_array().unwrap() {
        assert_eq!(
            variant["properties"]["label"]["allOf"][0]["not"]["pattern"],
            "[\\r\\n]"
        );
    }

    let source_kind =
        serde_json::to_value(schemars::schema_for!(McpSourceAdapterSourceKind)).unwrap();
    let opaque = source_kind["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["type"]["const"] == "opaque")
        .unwrap();
    assert_eq!(
        opaque["properties"]["kindId"]["allOf"][0]["not"]["pattern"],
        "[\\r\\n]"
    );
}

#[test]
fn source_adapter_semantic_matrix_rejects_impersonation_and_execution_claims() {
    let mut adapter = source_adapter_descriptor();
    adapter["trustMode"] = serde_json::json!("unverified");
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut adapter = source_adapter_descriptor();
    adapter["sourceKind"] = serde_json::json!({"type": "catalog"});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(adapter).is_err());

    let mut opaque = source_adapter_descriptor();
    opaque["displayName"] = serde_json::json!("Unverified source");
    opaque["sourceKind"] = serde_json::json!({"type": "opaque", "kindId": "unverified"});
    opaque["trustMode"] = serde_json::json!("unverified");
    opaque["requiredApprovals"] = serde_json::json!([]);
    opaque["risks"] = serde_json::json!(["process_execution"]);
    opaque["requiredHostDependencies"] = serde_json::json!([]);
    opaque["allowedTransports"] = serde_json::json!([]);
    opaque["inputFields"] = serde_json::json!({});
    opaque["availability"] = serde_json::json!({"status": "blocked", "reason": "unverified"});
    serde_json::from_value::<McpSourceAdapterDescriptor>(opaque.clone())
        .unwrap_or_else(|error| panic!("valid opaque descriptor rejected: {error}"));
    let wire = serde_json::to_value(
        serde_json::from_value::<McpSourceAdapterDescriptor>(opaque.clone()).unwrap(),
    )
    .unwrap();
    assert_eq!(wire["sourceKind"]["kindId"], "unverified");
    assert!(wire["sourceKind"].get("kind_id").is_none());
    let mut snake_case = opaque.clone();
    snake_case["sourceKind"] = serde_json::json!({"type": "opaque", "kind_id": "unverified"});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(snake_case).is_err());

    let mut custom_label = opaque.clone();
    custom_label["displayName"] = serde_json::json!("Extension secret label");
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(custom_label).is_ok());

    let mut custom_kind = opaque.clone();
    custom_kind["sourceKind"] = serde_json::json!({"type": "opaque", "kindId": "future.registry"});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(custom_kind).is_ok());

    let mut policy_blocked = opaque.clone();
    policy_blocked["availability"] = serde_json::json!({"status": "blocked", "reason": "policy"});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(policy_blocked).is_err());

    let mut credential_shape = opaque.clone();
    credential_shape["inputFields"] = serde_json::json!({
        "credential": {
            "label": "Secret reference",
            "kind": "reference",
            "required": true,
            "multiline": false,
            "secretReferenceOnly": true
        }
    });
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(credential_shape).is_err());

    let mut opaque_approval = opaque.clone();
    opaque_approval["risks"] = serde_json::json!(["network_access"]);
    opaque_approval["requiredApprovals"] = serde_json::json!(["network_access"]);

    opaque["trustMode"] = serde_json::json!("verified");
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(opaque.clone()).is_err());

    opaque["trustMode"] = serde_json::json!("unverified");
    opaque["allowedTransports"] = serde_json::json!(["catalog"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(opaque.clone()).is_err());

    opaque["allowedTransports"] = serde_json::json!([]);
    opaque["risks"] = serde_json::json!(["network_access", "host_dependency"]);
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(opaque).is_err());

    let mut opaque_known_id = source_adapter_descriptor();
    opaque_known_id["sourceKind"] = serde_json::json!({"type": "opaque", "kindId": "npm"});
    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(opaque_known_id).is_err());

    assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(opaque_approval).is_err());
}

#[test]
fn source_adapter_known_and_opaque_matrix_is_exhaustive() {
    let cases = [
        (
            "catalog",
            serde_json::json!({
                "displayName": "Catalog",
                "sourceKind": {"type": "catalog"},
                "trustMode": "verified",
                "availability": {"status": "ready_for_intake"},
                "requiredApprovals": [],
                "risks": ["network_access"],
                "requiredHostDependencies": [],
                "allowedTransports": ["catalog"],
                "inputFields": {}
            }),
            serde_json::json!(["remote_http"]),
        ),
        (
            "remote_http",
            serde_json::json!({
                "displayName": "Remote HTTP",
                "sourceKind": {"type": "remote_http"},
                "trustMode": "verified",
                "availability": {"status": "ready_for_intake"},
                "requiredApprovals": [],
                "risks": ["network_access"],
                "requiredHostDependencies": [],
                "allowedTransports": ["remote_http"],
                "inputFields": {}
            }),
            serde_json::json!(["catalog"]),
        ),
        (
            "manual_stdio",
            serde_json::json!({
                "displayName": "Manual stdio",
                "sourceKind": {"type": "manual_stdio"},
                "trustMode": "user_managed",
                "availability": {"status": "ready_for_intake"},
                "requiredApprovals": [],
                "risks": ["process_execution"],
                "requiredHostDependencies": [],
                "allowedTransports": ["stdio_provider"],
                "inputFields": {}
            }),
            serde_json::json!(["catalog"]),
        ),
        (
            "npm",
            source_adapter_descriptor(),
            serde_json::json!(["remote_http"]),
        ),
        (
            "uvx",
            serde_json::json!({
                "displayName": "UVX",
                "sourceKind": {"type": "uvx"},
                "trustMode": "user_managed",
                "availability": {"status": "ready_for_intake"},
                "requiredApprovals": [],
                "risks": ["process_execution", "host_dependency"],
                "requiredHostDependencies": ["uv"],
                "allowedTransports": ["stdio_provider"],
                "inputFields": {}
            }),
            serde_json::json!(["remote_http"]),
        ),
        (
            "docker",
            serde_json::json!({
                "displayName": "Docker",
                "sourceKind": {"type": "docker"},
                "trustMode": "user_managed",
                "availability": {"status": "ready_for_intake"},
                "requiredApprovals": [],
                "risks": ["process_execution", "host_dependency"],
                "requiredHostDependencies": ["docker"],
                "allowedTransports": ["stdio_provider"],
                "inputFields": {}
            }),
            serde_json::json!(["catalog"]),
        ),
        (
            "git",
            serde_json::json!({
                "displayName": "Git",
                "sourceKind": {"type": "git"},
                "trustMode": "user_managed",
                "availability": {"status": "ready_for_intake"},
                "requiredApprovals": [],
                "risks": ["network_access", "process_execution"],
                "requiredHostDependencies": [],
                "allowedTransports": ["stdio_provider"],
                "inputFields": {}
            }),
            serde_json::json!(["catalog"]),
        ),
        (
            "opaque",
            serde_json::json!({
                "displayName": "Unverified source",
                "sourceKind": {"type": "opaque", "kindId": "unverified"},
                "trustMode": "unverified",
                "availability": {"status": "blocked", "reason": "unverified"},
                "requiredApprovals": [],
                "risks": ["process_execution"],
                "requiredHostDependencies": [],
                "allowedTransports": [],
                "inputFields": {}
            }),
            serde_json::json!(["catalog"]),
        ),
    ];

    for (kind, descriptor, forbidden_transport) in cases {
        serde_json::from_value::<McpSourceAdapterDescriptor>(descriptor.clone())
            .unwrap_or_else(|error| panic!("{kind} valid matrix rejected: {error}"));

        let mut invalid_transport = descriptor.clone();
        invalid_transport["allowedTransports"] = forbidden_transport;
        assert!(
            serde_json::from_value::<McpSourceAdapterDescriptor>(invalid_transport).is_err(),
            "{kind} accepted a forbidden transport"
        );

        let mut invalid_trust = descriptor;
        invalid_trust["trustMode"] = if kind == "opaque" {
            serde_json::json!("verified")
        } else {
            serde_json::json!("unverified")
        };
        assert!(
            serde_json::from_value::<McpSourceAdapterDescriptor>(invalid_trust).is_err(),
            "{kind} accepted a forbidden trust mode"
        );
    }
}

#[test]
fn source_adapter_descriptor_schema_has_no_execution_escape_hatch() {
    let schema =
        serde_json::to_value(schemars::schema_for!(McpSourceAdaptersListResponse)).unwrap();
    let forbidden = [
        "command",
        "shell",
        "rawConfig",
        "credentialValue",
        "absolutePath",
        "endpoint",
        "token",
    ];
    fn assert_no_forbidden_properties(value: &serde_json::Value, forbidden: &[&str]) {
        if let Some(properties) = value
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            for name in properties.keys() {
                assert!(
                    !forbidden.contains(&name.as_str()),
                    "source adapter schema exposes {name}"
                );
            }
        }
        match value {
            serde_json::Value::Object(object) => {
                for child in object.values() {
                    assert_no_forbidden_properties(child, forbidden);
                }
            }
            serde_json::Value::Array(array) => {
                for child in array {
                    assert_no_forbidden_properties(child, forbidden);
                }
            }
            _ => {}
        }
    }
    assert_no_forbidden_properties(&schema, &forbidden);

    for schema in [
        serde_json::to_value(schemars::schema_for!(McpSourceAdaptersListResponse)).unwrap(),
        serde_json::to_value(schemars::schema_for!(McpSourceAdaptersListResult)).unwrap(),
        serde_json::to_value(schemars::schema_for!(McpSourceAdapterDescriptor)).unwrap(),
        serde_json::to_value(schemars::schema_for!(McpSourceAdapterInputField)).unwrap(),
    ] {
        if schema["oneOf"].is_null() {
            assert_eq!(schema["additionalProperties"], false);
        }
    }
    let result = serde_json::to_value(schemars::schema_for!(McpSourceAdaptersListResult)).unwrap();
    assert_eq!(result["type"], "object");
    assert_eq!(result["additionalProperties"], false);
    assert_eq!(result["required"], serde_json::json!(["adapters"]));
    assert_eq!(result["properties"]["adapters"]["type"], "object");
    assert_ne!(
        result["properties"]["adapters"]["additionalProperties"],
        true
    );
    assert_ne!(
        result["properties"]["adapters"]["additionalProperties"],
        true
    );
    let descriptor =
        serde_json::to_value(schemars::schema_for!(McpSourceAdapterDescriptor)).unwrap();
    assert_ne!(
        descriptor["properties"]["inputFields"]["additionalProperties"],
        true
    );
}

#[test]
fn source_adapter_schema_carries_runtime_string_and_matrix_constraints() {
    let schema = serde_json::to_value(schemars::schema_for!(McpSourceAdapterDescriptor)).unwrap();
    let properties = &schema["properties"];
    let display_name = &properties["displayName"];
    assert_eq!(display_name["type"], "string");
    assert_eq!(display_name["minLength"], 1);
    assert_eq!(display_name["maxLength"], 160);
    assert_eq!(
        display_name["pattern"],
        "^[A-Za-z0-9](?:[A-Za-z0-9.,_-]| [A-Za-z0-9.,_-])*$"
    );
    assert!(display_name["allOf"]
        .as_array()
        .is_some_and(|items| items.len() >= 8));

    assert_eq!(properties["risks"]["uniqueItems"], true);
    assert_eq!(properties["allowedTransports"]["uniqueItems"], true);
    assert_eq!(properties["requiredApprovals"]["uniqueItems"], true);
    assert_eq!(properties["requiredApprovals"]["maxItems"], 6);
    assert_eq!(properties["risks"]["minItems"], 1);
    assert_eq!(properties["inputFields"]["type"], "object");
    assert_eq!(properties["inputFields"]["maxProperties"], 32);
    assert_eq!(
        properties["inputFields"]["propertyNames"]["pattern"],
        "^[a-z][a-z0-9_.-]{0,127}$"
    );
    let result = serde_json::to_value(schemars::schema_for!(McpSourceAdaptersListResult)).unwrap();
    assert_eq!(result["properties"]["adapters"]["type"], "object");
    assert_eq!(result["properties"]["adapters"]["maxProperties"], 32);
    assert_eq!(
        result["properties"]["adapters"]["propertyNames"]["pattern"],
        "^[a-z][a-z0-9_.-]{0,127}$"
    );
    let descriptor_schema =
        serde_json::to_value(schemars::schema_for!(McpSourceAdapterDescriptor)).unwrap();
    let opaque_constraint = descriptor_schema["allOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|constraint| {
            constraint["if"]["properties"]["sourceKind"]["properties"]["type"]["const"] == "opaque"
        })
        .unwrap();
    assert_eq!(
        opaque_constraint["then"]["properties"]["requiredApprovals"]["maxItems"],
        0
    );
    let input = serde_json::to_value(schemars::schema_for!(McpSourceAdapterInputField)).unwrap();
    assert_eq!(input["oneOf"].as_array().map(Vec::len), Some(4));
    for variant in input["oneOf"].as_array().unwrap() {
        assert_eq!(variant["additionalProperties"], false);
        assert!(variant["required"]
            .as_array()
            .is_some_and(|required| required.contains(&serde_json::json!("secretReferenceOnly"))));
    }
    assert!(schema["allOf"]
        .as_array()
        .is_some_and(|items| items.len() >= 19));

    let availability =
        serde_json::to_value(schemars::schema_for!(McpSourceAdapterAvailability)).unwrap();
    assert_eq!(availability["oneOf"].as_array().map(Vec::len), Some(2));
    let blocked = availability["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["status"]["const"] == "blocked")
        .unwrap();
    assert_eq!(blocked["additionalProperties"], false);
    let availability_schema = serde_json::to_string(&availability).unwrap();
    for reason in [
        "policy",
        "approval_required",
        "runtime_dependency",
        "unsupported",
        "unverified",
    ] {
        assert!(availability_schema.contains(reason));
    }

    let credential_risk_constraint = schema["allOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|constraint| {
            constraint["if"]["properties"]["risks"]["not"]["contains"]["const"]
                == "credential_reference"
        })
        .unwrap();
    assert_eq!(
        credential_risk_constraint["then"]["properties"]["inputFields"]["additionalProperties"]
            ["properties"]["secretReferenceOnly"]["const"],
        false
    );

    let opaque = serde_json::to_value(schemars::schema_for!(McpSourceAdapterSourceKind)).unwrap();
    let opaque_text = serde_json::to_string(&opaque).unwrap();
    assert!(opaque_text.contains("future kind identifier") || opaque_text.contains("kindId"));
    assert!(opaque_text.contains("manual_stdio"));

    for invalid in [
        serde_json::json!("https://attacker.invalid"),
        serde_json::json!("<script>alert(1)</script>"),
        serde_json::json!("$(whoami)"),
        serde_json::json!("sk-1234567890abcdef"),
        serde_json::json!("ghp_abc_defg"),
        serde_json::json!("normal "),
        serde_json::json!("github_pat_123456789"),
    ] {
        let mut descriptor = source_adapter_descriptor();
        descriptor["displayName"] = invalid;
        assert!(serde_json::from_value::<McpSourceAdapterDescriptor>(descriptor).is_err());
    }

    let mut reordered_dependencies = source_adapter_descriptor();
    reordered_dependencies["requiredHostDependencies"] = serde_json::json!(["node", "bun"]);
    let normalized =
        serde_json::from_value::<McpSourceAdapterDescriptor>(reordered_dependencies).unwrap();
    assert_eq!(
        serde_json::to_value(normalized).unwrap()["requiredHostDependencies"],
        serde_json::json!(["bun", "node"])
    );
}

#[test]
fn source_adapter_raw_json_rejects_duplicate_keys_and_aliases_at_every_nested_boundary() {
    let descriptor = serde_json::to_string(&source_adapter_descriptor()).unwrap();
    let duplicate_display = descriptor.replacen(
        "\"displayName\":\"npm registry\"",
        "\"displayName\":\"npm registry\",\"displayName\":\"sk-1234567890abcdef\"",
        1,
    );
    assert!(serde_json::from_str::<McpSourceAdapterDescriptor>(&duplicate_display).is_err());

    let duplicate_availability_status = descriptor.replacen(
        "\"status\":\"ready_for_intake\"",
        "\"status\":\"ready_for_intake\",\"status\":\"blocked\"",
        1,
    );
    assert!(
        serde_json::from_str::<McpSourceAdapterDescriptor>(&duplicate_availability_status).is_err()
    );

    let duplicate_required_approvals = descriptor.replacen(
        "\"requiredApprovals\":[]",
        "\"requiredApprovals\":[],\"requiredApprovals\":[]",
        1,
    );
    assert!(
        serde_json::from_str::<McpSourceAdapterDescriptor>(&duplicate_required_approvals).is_err()
    );

    let unknown_availability_status = descriptor.replacen(
        "\"status\":\"ready_for_intake\"",
        "\"status\":\"self_approved\"",
        1,
    );
    assert!(
        serde_json::from_str::<McpSourceAdapterDescriptor>(&unknown_availability_status).is_err()
    );

    let unknown_approval = descriptor.replacen(
        "\"requiredApprovals\":[]",
        "\"requiredApprovals\":[\"browser_automation\"]",
        1,
    );
    assert!(serde_json::from_str::<McpSourceAdapterDescriptor>(&unknown_approval).is_err());

    let duplicate_risk = descriptor.replacen("\"risks\":[", "\"risks\":[],\"risks\":[", 1);
    assert!(serde_json::from_str::<McpSourceAdapterDescriptor>(&duplicate_risk).is_err());

    let input_field =
        serde_json::to_string(&source_adapter_descriptor()["inputFields"]["package"]).unwrap();
    let descriptor_without_fields = serde_json::to_string(&serde_json::json!({
        "displayName": "npm registry",
        "sourceKind": {"type": "npm"},
        "trustMode": "verified",
        "availability": {"status": "ready_for_intake"},
        "requiredApprovals": [],
        "risks": ["network_access", "process_execution", "host_dependency"],
        "requiredHostDependencies": ["node"],
        "allowedTransports": ["catalog", "stdio_provider"],
        "inputFields": {}
    }))
    .unwrap();
    let duplicate_input_key = descriptor_without_fields.replacen(
        "\"inputFields\":{}",
        &format!("\"inputFields\":{{\"package\":{input_field},\"package\":{input_field}}}"),
        1,
    );
    assert!(serde_json::from_str::<McpSourceAdapterDescriptor>(&duplicate_input_key).is_err());

    let duplicate_input_label = descriptor.replacen(
        "\"label\":\"Package\"",
        "\"label\":\"Package\",\"label\":\"Other\"",
        1,
    );
    assert!(serde_json::from_str::<McpSourceAdapterDescriptor>(&duplicate_input_label).is_err());

    let duplicate_source_kind = descriptor.replacen(
        "\"type\":\"npm\"",
        "\"type\":\"npm\",\"type\":\"catalog\"",
        1,
    );
    assert!(serde_json::from_str::<McpSourceAdapterDescriptor>(&duplicate_source_kind).is_err());

    let opaque = serde_json::json!({
        "displayName": "future registry",
        "sourceKind": {"type": "opaque", "kindId": "future.registry"},
        "trustMode": "unverified",
        "availability": {"status": "blocked", "reason": "unverified"},
        "requiredApprovals": [],
        "risks": ["network_access", "process_execution", "host_dependency"],
        "requiredHostDependencies": ["node"],
        "allowedTransports": [],
        "inputFields": {}
    });
    let duplicate_opaque_kind_id = serde_json::to_string(&opaque).unwrap().replacen(
        "\"kindId\":\"future.registry\"",
        "\"kindId\":\"future.registry\",\"kindId\":\"other.registry\"",
        1,
    );
    assert!(serde_json::from_str::<McpSourceAdapterDescriptor>(&duplicate_opaque_kind_id).is_err());

    let descriptor = serde_json::to_string(&source_adapter_descriptor()).unwrap();
    let duplicate_adapter_key =
        format!("{{\"adapters\":{{\"registry.npm\":{descriptor},\"registry.npm\":{descriptor}}}}}");
    assert!(serde_json::from_str::<McpSourceAdaptersListResult>(&duplicate_adapter_key).is_err());

    let response = serde_json::to_string(&serde_json::json!({
        "outcome": {"status": "success", "value": source_adapters_list_result()}
    }))
    .unwrap();
    let duplicate_outcome = response.replacen("\"outcome\":{", "\"outcome\":{},\"outcome\":{", 1);
    assert!(serde_json::from_str::<McpSourceAdaptersListResponse>(&duplicate_outcome).is_err());

    let duplicate_outcome_status = response.replacen(
        "\"status\":\"success\"",
        "\"status\":\"success\",\"status\":\"error\"",
        1,
    );
    assert!(
        serde_json::from_str::<McpSourceAdaptersListResponse>(&duplicate_outcome_status).is_err()
    );

    assert!(serde_json::from_str::<McpSourceAdaptersListRequest>("{\"x\":1,\"x\":2}").is_err());

    let snake_case = descriptor.replacen("\"displayName\"", "\"display_name\"", 1);
    assert!(serde_json::from_str::<McpSourceAdapterDescriptor>(&snake_case).is_err());

    let null_field = descriptor.replacen(
        "\"displayName\":\"npm registry\"",
        "\"displayName\":null",
        1,
    );
    assert!(serde_json::from_str::<McpSourceAdapterDescriptor>(&null_field).is_err());
}
