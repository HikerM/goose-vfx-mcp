#![cfg(all(feature = "integration-test-support", feature = "rustls-tls"))]

#[path = "acp_common_tests/mod.rs"]
mod common_tests;

use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::v1::ToolCallStatus;
use common_tests::fixtures::https_fixture_mcp_platform_service;
use common_tests::fixtures::remote_https::{RemoteHttpsFixture, FIXTURE_CODE};
use common_tests::fixtures::server::AcpServerConnection;
use common_tests::fixtures::{
    run_test, Connection, OpenAiFixture, PermissionDecision, Session, TestConnectionConfig,
};
use lumina::custom_requests::{
    McpConnectionTestPhase, McpConnectionTestStatus, McpHealthCheckMode, McpHealthRunRequest,
    McpHealthRunResponse, McpHttpsManifestConfirmRequest, McpHttpsManifestConfirmResponse,
    McpHttpsManifestPrepareRequest, McpHttpsManifestPrepareResponse,
    McpHttpsProvisionPlanCreateRequest, McpHttpsProvisionPlanCreateResponse,
    McpInstallConfirmRequest, McpInstallConfirmResponse, McpListRequest, McpListResponse,
    McpPlatformOutcome, McpProfileApplyConfirmRequest, McpProfileApplyConfirmResponse,
    McpProfileApplyPlanCreateRequest, McpProfileApplyPlanCreateResponse,
    McpProfileConnectionTestRequest, McpProfileCreateRequest, McpProfileCreateResponse,
    McpSetDefaultEnabledRequest, McpSetDefaultEnabledResponse, McpTaskGetRequest,
    McpTaskGetResponse, McpTaskStatus, McpUserDecision, MCP_HTTPS_MANIFEST_CONFIRM_METHOD,
    MCP_HTTPS_MANIFEST_PREPARE_METHOD, MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD,
};
use lumina::mcp_platform::{
    diagnose_integration_https_manifest_fetch, new_integration_https_manifest_fetcher,
    HttpsManifestSourceFetcher, IntegrationHttpsManifestFetchStage, McpPlatformService,
    McpPlatformServiceOptions, SqliteMcpPlatformRepository, SystemClock, UuidGenerator,
};
use lumina_test_support::IgnoreSessionId;
use sha2::{Digest, Sha256};
use sqlx::error::DatabaseError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteError, SqlitePoolOptions};
use sqlx::Row;
use tokio::sync::OnceCell;

const USER_PROMPT: &str = "Call the fixture tool and return its code.";

fn final_response() -> &'static str {
    "data: {\"id\":\"chatcmpl-final\",\"object\":\"chat.completion.chunk\",\"created\":1766709751,\"model\":\"gpt-5-nano-2025-08-07\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"MCP_FIXTURE_CODE\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"chatcmpl-final\",\"object\":\"chat.completion.chunk\",\"created\":1766709751,\"model\":\"gpt-5-nano-2025-08-07\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n"
}

fn fixture_managed_tool_name() -> String {
    let digest = Sha256::digest(b"mcp-fixture\0user");
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("managed_mcp_{suffix}__get_code")
}

async fn task_until_succeeded(
    connection: &AcpServerConnection,
    service: &McpPlatformService,
    task_id: &str,
    data_root: &std::path::Path,
) -> anyhow::Result<lumina::custom_requests::McpTaskRef> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Err(error) = service.runner_tick().await {
            let diagnostic = runner_tick_failure_diagnostic(data_root, task_id).await;
            anyhow::bail!("ACP HTTPS E2E task runner tick failed: {error:?}; {diagnostic}");
        }
        let response: McpTaskGetResponse = connection
            .cx()
            .send_request(McpTaskGetRequest {
                task_id: task_id.to_owned(),
            })
            .block_task()
            .await
            .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: task_get"))?;
        let value = match response.outcome {
            McpPlatformOutcome::Success { value } => value,
            McpPlatformOutcome::Error { error } => {
                anyhow::bail!("task lookup failed: code={:?}", error.code)
            }
        };
        match value.status {
            McpTaskStatus::Succeeded => return Ok(value),
            McpTaskStatus::Failed
            | McpTaskStatus::Cancelled
            | McpTaskStatus::Interrupted
            | McpTaskStatus::RecoveryRequired => {
                let database_path = data_root.join("mcp-platform").join("platform.db");
                let diagnostic = match SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(
                        SqliteConnectOptions::new()
                            .filename(database_path)
                            .read_only(true),
                    )
                    .await
                {
                    Ok(pool) => sqlx::query_scalar::<_, Option<String>>(
                        "SELECT redacted_error_json FROM tasks WHERE task_id=?",
                    )
                    .bind(task_id)
                    .fetch_optional(&pool)
                    .await
                    .ok()
                    .flatten(),
                    Err(_) => None,
                };
                let lifecycle = runner_tick_failure_diagnostic(data_root, task_id).await;
                anyhow::bail!(
                    "install task did not succeed: status={:?}, error={:?}, redacted={diagnostic:?}; {lifecycle}",
                    value.status, value.outcome.error
                )
            }
            _ if tokio::time::Instant::now() >= deadline => {
                anyhow::bail!("install task deadline exceeded: status={:?}", value.status)
            }
            _ => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
}

async fn runner_tick_failure_diagnostic(data_root: &std::path::Path, task_id: &str) -> String {
    let database_path = data_root.join("mcp-platform").join("platform.db");
    let Ok(pool) = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(database_path)
                .read_only(true),
        )
        .await
    else {
        return "task_diagnostic=db_open_failed".to_owned();
    };
    let task = sqlx::query("SELECT status, revision, progress FROM tasks WHERE task_id=?")
        .bind(task_id)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .map(|row| {
            format!(
                "status={},revision={},progress={}",
                row.get::<String, _>("status"),
                row.get::<i64, _>("revision"),
                row.get::<i64, _>("progress")
            )
        })
        .unwrap_or_else(|| "missing".to_owned());
    let steps = sqlx::query(
        "SELECT ordinal, status, adapter_id FROM task_steps WHERE task_id=? ORDER BY ordinal",
    )
    .bind(task_id)
    .fetch_all(&pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|row| {
                format!(
                    "{}:{}:{}",
                    row.get::<i64, _>("ordinal"),
                    row.get::<String, _>("status"),
                    row.get::<String, _>("adapter_id")
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    })
    .unwrap_or_else(|_| "query_failed".to_owned());
    pool.close().await;
    format!("task={task}; steps=[{steps}]")
}

#[test]
fn acp_https_install_profile_model_tool_call_round_trip() {
    run_test(async move {
        if let Err(error) = acp_https_install_profile_model_tool_call_round_trip_inner().await {
            panic!("{error:#}");
        }
    });
}

async fn acp_https_install_profile_model_tool_call_round_trip_inner() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let data_root = root.path().join("agent-data");
    std::fs::create_dir_all(&data_root)?;
    let fixture = RemoteHttpsFixture::start().await?;
    let manifest_fetcher = new_integration_https_manifest_fetcher(
        fixture.manifest_url.clone(),
        fixture.socket_addr,
        fixture.ca_der.clone(),
    )
    .map_err(|_| anyhow::anyhow!("HTTPS manifest preflight setup failed"))?;
    let fetched = match manifest_fetcher.fetch(&fixture.manifest_url).await {
        Ok(fetched) => fetched,
        Err(_) => {
            let diagnostic = diagnose_integration_https_manifest_fetch(
                fixture.manifest_url.clone(),
                fixture.socket_addr,
                fixture.ca_der.clone(),
            )
            .await;
            let message = match diagnostic {
                Ok(()) => {
                    "HTTPS manifest preflight fetch failed: stage=nondeterministic".to_owned()
                }
                Err(stage) => format!(
                    "HTTPS manifest preflight fetch failed: stage={}",
                    match stage {
                        IntegrationHttpsManifestFetchStage::InvalidUrl => "invalid_url",
                        IntegrationHttpsManifestFetchStage::UnsafeResolution => "unsafe_resolution",
                        IntegrationHttpsManifestFetchStage::Transport => "transport",
                        IntegrationHttpsManifestFetchStage::Redirect => "redirect",
                        IntegrationHttpsManifestFetchStage::Status => "status",
                        IntegrationHttpsManifestFetchStage::ContentType => "content_type",
                        IntegrationHttpsManifestFetchStage::Body => "body",
                    }
                ),
            };
            return Err(anyhow::anyhow!(message));
        }
    };
    let expected_origin = format!(
        "https://mcp-fixture.example.com:{}",
        fixture.socket_addr.port()
    );
    assert!(fetched.requested.origin() == expected_origin);
    assert!(fetched.final_url.origin() == expected_origin);
    assert!(!fetched.raw_bytes.is_empty());
    let repository = Arc::new(
        SqliteMcpPlatformRepository::open_path_for_integration_test(
            &data_root.join("mcp-platform").join("platform.db"),
        )
        .await?,
    );
    let service = https_fixture_mcp_platform_service(
        repository,
        Arc::new(SystemClock),
        Arc::new(UuidGenerator),
        McpPlatformServiceOptions::default(),
        &fixture,
        &data_root,
    )?;
    let service_cell = Arc::new(OnceCell::new());
    assert!(service_cell.set(Arc::new(service)).is_ok());

    let managed_tool_name = fixture_managed_tool_name();
    let first_exchange = Box::leak(
        include_str!("acp_test_data/openai_tool_call.txt")
            .replace("mcp-fixture__get_code", &managed_tool_name)
            .into_boxed_str(),
    );
    let connection_probe_fixture = OpenAiFixture::new(
        vec![
            ("Reply with OK only.".to_owned(), final_response()),
            (USER_PROMPT.to_owned(), first_exchange),
            (FIXTURE_CODE.to_owned(), final_response()),
        ],
        Arc::new(IgnoreSessionId),
    )
    .await;
    let connection_probe_observer = connection_probe_fixture.observer();
    let config = TestConnectionConfig {
        data_root: data_root.clone(),
        trusted_transport: true,
        mcp_platform_service_cell: Some(Arc::clone(&service_cell)),
        ..Default::default()
    };
    let mut connection = AcpServerConnection::new(config, connection_probe_fixture).await;

    let prepared: McpHttpsManifestPrepareResponse = connection
        .cx()
        .send_request(McpHttpsManifestPrepareRequest {
            url: fixture.manifest_url.clone(),
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: manifest_prepare"))?;
    let prepared = match prepared.outcome {
        McpPlatformOutcome::Success { value } => value,
        McpPlatformOutcome::Error { error } => {
            anyhow::bail!("manifest prepare failed: code={:?}", error.code)
        }
    };
    let public_prepare = serde_json::to_string(&McpHttpsManifestPrepareResponse {
        outcome: McpPlatformOutcome::Success {
            value: prepared.clone(),
        },
    })?;
    assert!(!public_prepare.contains("127.0.0.1"));

    let confirmed: McpHttpsManifestConfirmResponse = connection
        .cx()
        .send_request(McpHttpsManifestConfirmRequest {
            provision_id: prepared.provision_id.clone(),
            confirmation_token: prepared.confirmation_token,
            confirm: true,
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: manifest_confirm"))?;
    let confirmed = match confirmed.outcome {
        McpPlatformOutcome::Success { value } => value,
        McpPlatformOutcome::Error { error } => {
            let stage = service_cell
                .get()
                .and_then(|service| service.https_manifest_confirm_stage(&prepared.provision_id))
                .unwrap_or("unavailable");
            anyhow::bail!(
                "manifest confirm failed: code={:?}, stage={stage}",
                error.code
            )
        }
    };
    let plan: McpHttpsProvisionPlanCreateResponse = connection
        .cx()
        .send_request(McpHttpsProvisionPlanCreateRequest {
            provision_id: confirmed.provision_id,
            expected_manifest_digest: confirmed.manifest_digest.clone(),
            idempotency_key: "acp-https-e2e-plan".to_owned(),
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: provision_plan_create"))?;
    let McpPlatformOutcome::Success { value: plan } = plan.outcome else {
        anyhow::bail!("provision plan failed");
    };
    assert_eq!(plan.mcp_id, "mcp-fixture");
    assert_eq!(plan.selected_manifest_digest, confirmed.manifest_digest);
    assert!(!plan.plan_id.is_empty());
    assert!(!plan.plan_digest.is_empty());
    assert!(plan.expires_at_ms > 0);
    assert!(plan
        .required_confirmations
        .iter()
        .any(|confirmation| matches!(
            confirmation,
            lumina::custom_requests::McpHttpsProvisionConfirmation::Policy
        )));
    assert!(plan
        .permissions
        .iter()
        .all(|permission| permission.required));
    assert!(!plan.file_effects.writes_files || plan.file_effects.owned_items > 0);
    assert!(!plan.process_effects.starts_during_confirmation);
    let public_plan = serde_json::to_string(&plan)?;
    assert!(!public_plan.contains("confirmationToken"));
    assert!(!public_plan.contains("127.0.0.1"));
    let install: McpInstallConfirmResponse = connection
        .cx()
        .send_request(McpInstallConfirmRequest {
            plan_id: plan.plan_id.clone(),
            plan_digest: plan.plan_digest.clone(),
            user_decision: McpUserDecision::Confirm,
            idempotency_key: "acp-https-e2e-install".to_owned(),
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: install_confirm"))?;
    let McpPlatformOutcome::Success { value: install } = install.outcome else {
        anyhow::bail!("install confirm failed");
    };
    assert!(!install.task_id.is_empty());
    let service = service_cell
        .get()
        .ok_or_else(|| anyhow::anyhow!("ACP HTTPS E2E service unavailable"))?;
    let task = task_until_succeeded(&connection, service, &install.task_id, &data_root).await?;
    assert_eq!(task.status, McpTaskStatus::Succeeded);
    assert_eq!(task.progress, 100);
    assert!(task.revision > 0);
    assert!(task.outcome.remaining_effects.is_empty());
    // McpTaskRef deliberately exposes no plan identity; the ACP install response only
    // exposes task_id, so plan/task binding cannot be asserted without private state.

    let managed: McpListResponse = connection
        .cx()
        .send_request(McpListRequest {
            page_size: Some(20),
            ..Default::default()
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: managed_list"))?;
    let managed = match managed.outcome {
        McpPlatformOutcome::Success { value } => value,
        McpPlatformOutcome::Error { error } => {
            let diagnostic_stage = service
                .managed_list_diagnostic_stage_for_integration(20)
                .await;
            let readback_stage = managed_list_manifest_readback_stage(&data_root).await;
            anyhow::bail!(
                "managed MCP list failed: code={:?}, diagnostic_stage={diagnostic_stage}, readback_stage={readback_stage}",
                error.code
            )
        }
    };
    let managed_mcp_id = managed
        .items
        .iter()
        .find(|item| item.mcp_id == "mcp-fixture")
        .ok_or_else(|| anyhow::anyhow!("installed MCP mcp-fixture missing"))?
        .managed_mcp_id
        .clone();

    let health: McpHealthRunResponse = connection
        .cx()
        .send_request(McpHealthRunRequest {
            managed_mcp_id: managed_mcp_id.clone(),
            mode: McpHealthCheckMode::Registration,
            idempotency_key: "acp-https-e2e-profile-health".to_owned(),
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: health_run"))?;
    let McpPlatformOutcome::Success { value: health_task } = health.outcome else {
        anyhow::bail!("health run failed");
    };
    let health_task =
        task_until_succeeded(&connection, service, &health_task.task_id, &data_root).await?;
    assert_eq!(health_task.status, McpTaskStatus::Succeeded);

    let managed: McpListResponse = connection
        .cx()
        .send_request(McpListRequest {
            page_size: Some(20),
            ..Default::default()
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: managed_list_after_health"))?;
    let managed = match managed.outcome {
        McpPlatformOutcome::Success { value } => value,
        McpPlatformOutcome::Error { error } => {
            anyhow::bail!(
                "managed MCP list after health failed: code={:?}",
                error.code
            )
        }
    };
    let managed_mcp = managed
        .items
        .iter()
        .find(|item| item.managed_mcp_id == managed_mcp_id)
        .ok_or_else(|| anyhow::anyhow!("installed MCP mcp-fixture missing after health"))?;
    let enabled: McpSetDefaultEnabledResponse = connection
        .cx()
        .send_request(McpSetDefaultEnabledRequest {
            managed_mcp_id: managed_mcp_id.clone(),
            enabled: true,
            expected_revision: managed_mcp.revision,
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: set_default_enabled"))?;
    let enabled = match enabled.outcome {
        McpPlatformOutcome::Success { value } => value,
        McpPlatformOutcome::Error { error } => {
            anyhow::bail!("set default enabled failed: code={:?}", error.code)
        }
    };
    assert_eq!(enabled.managed_mcp_id, managed_mcp_id);
    assert!(enabled.default_enabled);
    assert!(enabled.revision > managed_mcp.revision);

    let profile: McpProfileCreateResponse = connection
        .cx()
        .send_request(McpProfileCreateRequest {
            name: "fixture profile".to_owned(),
            description: String::new(),
            managed_mcp_ids: vec![managed_mcp_id.clone()],
            idempotency_key: "acp-https-e2e-profile".to_owned(),
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: profile_create"))?;
    let McpPlatformOutcome::Success { value: profile } = profile.outcome else {
        anyhow::bail!("profile create failed");
    };
    let profile_id = profile.profile_id.clone();
    let apply_plan: McpProfileApplyPlanCreateResponse = connection
        .cx()
        .send_request(McpProfileApplyPlanCreateRequest {
            profile_id: profile_id.clone(),
            profile_revision: profile.revision,
            idempotency_key: "acp-https-e2e-profile-apply".to_owned(),
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: profile_apply_plan_create"))?;
    let McpPlatformOutcome::Success { value: apply_plan } = apply_plan.outcome else {
        anyhow::bail!("profile apply plan failed");
    };
    assert_eq!(apply_plan.profile_id, profile_id);
    assert_eq!(apply_plan.profile_revision, profile.revision);
    assert!(apply_plan.entries.iter().any(|entry| {
        entry.managed_mcp_id == managed_mcp_id
            && entry.mcp_id.as_deref() == Some("mcp-fixture")
            && entry.auth_ready
            && entry.policy_ready
    }));
    let apply_plan_id = apply_plan.plan_id.clone();
    let applied: McpProfileApplyConfirmResponse = connection
        .cx()
        .send_request(McpProfileApplyConfirmRequest {
            plan_id: apply_plan_id.clone(),
            confirmation_token: apply_plan.confirmation.confirmation_token,
            confirm: true,
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: profile_apply_confirm"))?;
    let McpPlatformOutcome::Success { value: application } = applied.outcome else {
        anyhow::bail!("profile apply confirm failed");
    };
    assert_eq!(application.plan_id, apply_plan_id);
    assert_eq!(application.profile_id, profile_id);
    assert_eq!(application.profile_revision, profile.revision);
    let application_token = application.token.clone();
    let connection_test: lumina::custom_requests::McpProfileConnectionTestResponse = connection
        .cx()
        .send_request(McpProfileConnectionTestRequest {
            profile_id,
            provider_id: "openai".to_owned(),
            model_id: "gpt-4.1".to_owned(),
        })
        .block_task()
        .await
        .map_err(|_| anyhow::anyhow!("ACP HTTPS E2E phase failed: profile_connection_test"))?;
    let McpPlatformOutcome::Success {
        value: connection_test,
    } = connection_test.outcome
    else {
        anyhow::bail!("profile connection test failed");
    };
    let stage_summary = connection_test
        .stages
        .iter()
        .map(|stage| {
            format!(
                "phase={:?},status={:?},code={:?},duration_ms={}",
                stage.phase, stage.status, stage.code, stage.duration_ms
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        connection_test.passed,
        "profile connection test failed; stages: {stage_summary}"
    );
    let expected_phases = [
        McpConnectionTestPhase::Eligibility,
        McpConnectionTestPhase::McpConnectInitialize,
        McpConnectionTestPhase::ToolDiscovery,
        McpConnectionTestPhase::Cleanup,
        McpConnectionTestPhase::ModelRequest,
        McpConnectionTestPhase::ToolVisibility,
    ];
    assert_eq!(
        connection_test
            .stages
            .iter()
            .map(|stage| stage.phase)
            .collect::<Vec<_>>(),
        expected_phases
    );
    assert!(connection_test
        .stages
        .iter()
        .all(|stage| stage.status == McpConnectionTestStatus::Passed));
    assert!(connection_test.stages.iter().all(|stage| {
        stage
            .managed_mcp_id
            .as_deref()
            .is_none_or(|managed_id| managed_id == managed_mcp_id)
    }));
    for phase in [
        McpConnectionTestPhase::McpConnectInitialize,
        McpConnectionTestPhase::ToolDiscovery,
    ] {
        assert!(connection_test.stages.iter().any(|stage| {
            stage.phase == phase && stage.managed_mcp_id.as_deref() == Some(managed_mcp_id.as_str())
        }));
    }
    assert!(connection_test.stages.iter().any(|stage| {
        stage.phase == McpConnectionTestPhase::ToolVisibility
            && stage.code == lumina::custom_requests::McpConnectionTestCode::ToolVisibilityValidated
    }));
    let mut session = match connection
        .new_session_with_profile_application_token(&application_token)
        .await
    {
        Ok(session) => session,
        Err(_) => {
            return Err(anyhow::anyhow!(
                profile_application_failure_diagnostic(&data_root, &application_token).await
            ));
        }
    };
    let output = match session
        .session
        .prompt(USER_PROMPT, PermissionDecision::AllowOnce)
        .await
    {
        Ok(output) => output,
        Err(_) => {
            return Err(anyhow::anyhow!(
                "ACP profile application model-tool prompt failed; {}",
                profile_application_failure_diagnostic(&data_root, &application_token).await
            ));
        }
    };
    assert_eq!(output.text, FIXTURE_CODE);
    assert_eq!(output.tool_status, Some(ToolCallStatus::Completed));
    let updates = session.session.session_updates();
    let managed_tool_call_ids = updates
        .iter()
        .filter_map(|update| match update {
            agent_client_protocol::schema::v1::SessionUpdate::ToolCall(call)
                if call
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.get("lumina"))
                    .and_then(|lumina| lumina.get("toolCall"))
                    .and_then(|tool_call| tool_call.get("toolName"))
                    .and_then(serde_json::Value::as_str)
                    == Some(managed_tool_name.as_str()) =>
            {
                Some(call.tool_call_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let completed_tool_call = updates.iter().any(|update| {
        let agent_client_protocol::schema::v1::SessionUpdate::ToolCallUpdate(update) = update
        else {
            return false;
        };
        managed_tool_call_ids.contains(&update.tool_call_id)
            && update.fields.status == Some(ToolCallStatus::Completed)
            && serde_json::to_value(&update.fields.content)
                .is_ok_and(|content| content.to_string().contains(FIXTURE_CODE))
    });
    assert!(
        completed_tool_call,
        "ACP must publish the completed fixture tool result"
    );
    connection_probe_observer.assert_exhausted();
    drop(connection);
    assert_eq!(fixture.tool_calls(), vec!["get_code"]);
    Ok(())
}

async fn managed_list_manifest_readback_stage(data_root: &std::path::Path) -> &'static str {
    let database_path = data_root.join("mcp-platform").join("platform.db");
    let options = SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(false)
        .read_only(true);
    let Ok(pool) = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
    else {
        return "db_open";
    };

    let digest = match sqlx::query_scalar::<_, String>(
        "SELECT active_manifest_digest FROM managed_mcps WHERE active_manifest_digest IS NOT NULL LIMIT 1",
    )
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(digest)) => digest,
        Ok(None) => {
            pool.close().await;
            return "managed_active_digest_missing";
        }
        Err(_) => {
            pool.close().await;
            return "managed_active_digest_query";
        }
    };

    if let Err(error) = sqlx::query(
        r#"SELECT
                s.source_id, s.import_kind, s.source_ref_json, s.display_name,
                s.created_at_ms AS source_created_at_ms, s.updated_at_ms AS source_updated_at_ms,
                d.document_id, d.document_digest, d.document_kind, d.created_at_ms AS document_created_at_ms,
                d.document_digest AS trusted_document_digest,
                d.canonical_digest AS trusted_canonical_digest,
                d.signed_digest AS trusted_signed_digest,
                d.binding_digest AS trusted_binding_digest,
                (SELECT group_concat(kid, char(31)) FROM (SELECT kid FROM source_signatures WHERE source_id = d.source_id ORDER BY signature_order)) AS trusted_signature_kids,
                r.release_id AS trusted_release_id, r.mcp_id AS trusted_release_mcp_id,
                r.version AS trusted_release_version,
                r.release_status AS trusted_release_status,
                e.entry_id, e.proof_json, e.trust_tier_json, e.source_metadata_json, e.created_at_ms,
                m.manifest_digest, m.mcp_id, m.version, m.canonical_bytes
           FROM governed_catalog_entries e
           JOIN governed_catalog_sources s ON s.source_id = e.source_id
           JOIN governed_catalog_documents d ON d.source_id = e.source_id AND d.document_id = e.document_id
           JOIN manifest_blobs m ON m.manifest_digest = e.manifest_digest
           LEFT JOIN source_releases r ON r.source_id = e.source_id AND r.release_id = e.entry_id
           WHERE e.manifest_digest = ?
           ORDER BY e.source_id, e.mcp_id, e.version, e.entry_id"#,
    )
    .bind(&digest)
    .fetch_all(&pool)
    .await
    {
        let stage = match error {
            sqlx::Error::PoolTimedOut => "catalog_busy",
            sqlx::Error::Database(error) => {
                let error = error.downcast_ref::<SqliteError>();
                match error
                    .code()
                    .and_then(|code| code.parse::<i32>().ok())
                    .map(|code| code & 0xff)
                {
                    Some(17) => "catalog_schema",
                    Some(5) => "catalog_busy",
                    Some(6) => "catalog_locked",
                    _ => "catalog_other",
                }
            }
            _ => "catalog_other",
        };
        pool.close().await;
        return stage;
    }

    let stage = match sqlx::query(
        r#"SELECT manifest_digest, mcp_id, version, canonical_bytes, proof_json,
                trust_tier_json, source_ref_json, import_kind, release_id,
                origin_provenance_json, update_channel_json, created_at_ms
           FROM manifest_blobs WHERE manifest_digest = ?"#,
    )
    .bind(&digest)
    .fetch_optional(&pool)
    .await
    {
        Ok(Some(_)) => "complete",
        Ok(None) => "blob_missing",
        Err(_) => "blob_v27_query",
    };
    pool.close().await;
    stage
}

async fn profile_application_failure_diagnostic(
    data_root: &std::path::Path,
    token: &str,
) -> String {
    let unavailable = || {
        "ACP profile application diagnostic: status=unavailable; failure_code=unavailable; events=unavailable"
            .to_owned()
    };
    let token_hash = Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let database_path = data_root.join("mcp-platform").join("platform.db");
    let options = SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(false)
        .foreign_keys(true);
    let Ok(pool) = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
    else {
        return unavailable();
    };
    let row = sqlx::query(
        "SELECT status, failure_code FROM mcp_profile_applications WHERE application_id=(SELECT application_id FROM mcp_profile_application_tokens WHERE token_hash=?)",
    )
    .bind(&token_hash)
    .fetch_optional(&pool)
    .await;
    let Ok(Some(row)) = row else {
        pool.close().await;
        return unavailable();
    };
    let status = row.try_get::<String, _>("status").ok().map_or_else(
        || "unknown".to_owned(),
        |value| profile_application_status_label(&value),
    );
    let failure_code = row
        .try_get::<Option<String>, _>("failure_code")
        .ok()
        .flatten()
        .map_or_else(
            || "none".to_owned(),
            |value| profile_application_failure_code_label(&value),
        );
    let events = sqlx::query(
        "SELECT event_type, detail_code FROM mcp_profile_application_events WHERE application_id=(SELECT application_id FROM mcp_profile_application_tokens WHERE token_hash=?) ORDER BY event_id",
    )
    .bind(&token_hash)
    .fetch_all(&pool)
    .await;
    pool.close().await;
    let Ok(events) = events else {
        return unavailable();
    };
    let events = events
        .into_iter()
        .map(|event| {
            let event_type = event.try_get::<String, _>("event_type").ok().map_or_else(
                || "unknown".to_owned(),
                |value| profile_application_event_type_label(&value),
            );
            let detail_code = event.try_get::<String, _>("detail_code").ok().map_or_else(
                || "unknown".to_owned(),
                |value| profile_application_event_detail_code_label(&value),
            );
            format!("{event_type}:{detail_code}")
        })
        .collect::<Vec<_>>();
    let events = if events.is_empty() {
        "none".to_owned()
    } else {
        events.join(",")
    };
    format!(
        "ACP profile application diagnostic: status={status}; failure_code={failure_code}; events={events}"
    )
}

fn profile_application_status_label(value: &str) -> String {
    if matches!(
        value,
        "confirmed"
            | "consumed"
            | "created"
            | "failed"
            | "rolled_back"
            | "recovery_required"
            | "deleted"
    ) {
        value.to_owned()
    } else {
        "unknown".to_owned()
    }
}

fn profile_application_failure_code_label(value: &str) -> String {
    if matches!(
        value,
        "session_create_failed"
            | "session_binding_failed"
            | "application_record_failed"
            | "session_setup_failed"
            | "session_and_agent_cleanup_failed"
            | "session_cleanup_failed"
            | "agent_cleanup_failed"
            | "session_cleanup_complete"
            | "application_state_transition_failed"
            | "compensation_state_transition_failed"
    ) {
        value.to_owned()
    } else {
        "unknown".to_owned()
    }
}

fn profile_application_event_type_label(value: &str) -> String {
    if matches!(
        value,
        "token_created"
            | "token_consumed"
            | "session_created"
            | "application_failed"
            | "application_rolled_back"
            | "recovery_required"
            | "session_deleted"
    ) {
        value.to_owned()
    } else {
        "unknown".to_owned()
    }
}

fn profile_application_event_detail_code_label(value: &str) -> String {
    if matches!(
        value,
        "confirmed"
            | "accepted"
            | "session_created"
            | "session_create_failed"
            | "session_binding_failed"
            | "application_record_failed"
            | "session_setup_failed"
            | "session_and_agent_cleanup_failed"
            | "session_cleanup_failed"
            | "agent_cleanup_failed"
            | "session_cleanup_complete"
            | "application_state_transition_failed"
            | "compensation_state_transition_failed"
    ) {
        value.to_owned()
    } else {
        "unknown".to_owned()
    }
}
