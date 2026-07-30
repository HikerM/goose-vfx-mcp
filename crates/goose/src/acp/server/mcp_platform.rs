use super::*;
use std::collections::BTreeSet;
use std::fs;
#[cfg(windows)]
use std::path::{Component, Prefix};
use std::path::{Path, PathBuf};

use crate::agents::extension_manager::{ExtensionManager, ToolDiscoveryLimits};
use crate::mcp_platform::error::MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE;
use crate::mcp_platform::manifest::{
    Architecture, ArchiveFormat, Auth, Capability, Distribution, GitDevAdapter, HealthCheck,
    Manifest, ManifestProof, ManifestSourceMetadata, OAuthClientRegistration, OriginProvenance,
    PermissionKind, Platform, SourceImportKind, SourceRef, Transport, UpdateChannel,
    VerifiedSourceDocumentRef,
};
use crate::mcp_platform::policy::PolicyOutcome;
use crate::mcp_platform::runtime_control::{
    ManagedRuntimeAction as HostRuntimeAction, ManagedRuntimeCommand, ManagedRuntimeControlPort,
};
use crate::mcp_platform::service::{
    CatalogListInput, CatalogLocator, CatalogPlanTarget, EventsResumeInput,
    GovernedDirectoryImportInput, GovernedDirectoryManifestImportInput,
    GovernedManifestImportInput, HealthGetInput, HealthRunInput, HttpsManifestConfirmInput,
    HttpsManifestPlanReviewInput, HttpsManifestPrepareInput, InstallConfirmInput,
    InstallationScope, ManagedGetInput, ManagedListInput,
    ManagedRuntimeAction as ServiceRuntimeAction, ManagedRuntimeControlInput, PlanCreateInput,
    PlanIntent, ProfileApplyPlanInput, ProfileArchiveInput, ProfileCreateInput,
    ProfileRestoreInput, ProfileUpdateInput, SetDefaultEnabledInput, TaskCancelInput, TaskGetInput,
    TaskRetryInput, UserDecision,
};
use crate::mcp_platform::task::{RecoveryDecision, TaskOperation, TaskStatus, TaskStepStatus};
use crate::mcp_platform::{AuditEventType, AuditPayload};
use async_trait::async_trait;
use rmcp::model::Tool;
use sha2::{Digest as _, Sha256};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

const MAX_GOVERNED_IMPORT_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_GOVERNED_IMPORT_DIRECTORY_FILES: usize = 512;
const MAX_GOVERNED_IMPORT_DIRECTORY_BYTES: u64 = 32 * 1024 * 1024;
const LOCAL_ONLY_GOVERNED_IMPORT_MESSAGE: &str =
    "Only local files and local source directories can be imported from this page.";
const CONNECTION_TEST_TOTAL_DEADLINE: Duration = Duration::from_secs(30);
const CONNECTION_TEST_MAX_PROFILE_MCPS: usize = 8;
const CONNECTION_TEST_MAX_TOOLS: usize = 32;
const CONNECTION_TEST_MAX_TOOL_BYTES: usize = 16 * 1024;
const CONNECTION_TEST_MAX_TOTAL_TOOL_BYTES: usize = 128 * 1024;
const CONNECTION_TEST_MAX_TOOL_PAGES: usize = 16;
const CONNECTION_TEST_MAX_TOOL_CURSORS: usize = 16;
const CONNECTION_TEST_MAX_CURSOR_BYTES: usize = 1024;
static CONNECTION_TEST_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[cfg(test)]
struct ConnectionTestDiscoveryClient;

#[cfg(test)]
#[async_trait]
impl crate::agents::mcp_client::McpClientTrait for ConnectionTestDiscoveryClient {
    async fn close(&self) -> Result<(), rmcp::service::ServiceError> {
        Ok(())
    }

    async fn list_tools(
        &self,
        _session_id: &str,
        _next_cursor: Option<String>,
        _cancel_token: CancellationToken,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::service::ServiceError> {
        Ok(rmcp::model::ListToolsResult {
            tools: vec![Tool::new(
                "fixture_mcp_tool".to_string(),
                "A deterministic tool discovered by the MCP fixture.".to_string(),
                Arc::new(
                    serde_json::json!({"type": "object"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )],
            next_cursor: None,
            meta: None,
        })
    }

    fn get_info(&self) -> Option<&rmcp::model::InitializeResult> {
        None
    }

    async fn call_tool(
        &self,
        _ctx: &crate::agents::ToolCallContext,
        _name: &str,
        _arguments: Option<rmcp::model::JsonObject>,
        _cancel_token: CancellationToken,
    ) -> Result<rmcp::model::CallToolResult, rmcp::service::ServiceError> {
        Err(rmcp::service::ServiceError::TransportClosed)
    }
}

struct ProfileConnectionTestTargetResult {
    stages: Vec<McpConnectionTestStage>,
    tools: Vec<Tool>,
    passed: bool,
}

enum BoundedDiscovery {
    Completed(crate::agents::extension::ExtensionResult<Vec<Tool>>),
    TimedOut,
}

struct AcpManagedRuntimeControlPort<'a> {
    agent: &'a GooseAcpAgent,
}

#[async_trait]
impl ManagedRuntimeControlPort for AcpManagedRuntimeControlPort<'_> {
    async fn control(
        &self,
        command: ManagedRuntimeCommand,
    ) -> crate::mcp_platform::McpPlatformResult<bool> {
        let sessions = {
            let sessions = self.agent.sessions.lock().await;
            sessions
                .iter()
                .map(|(session_id, session)| (session_id.clone(), session.agent.clone()))
                .collect::<Vec<_>>()
        };
        match command.action {
            HostRuntimeAction::Stop => {
                let mut stopped = false;
                let mut failed = false;
                for (session_id, agent) in sessions {
                    if agent
                        .extension_manager
                        .get_extension_config(&command.extension_key)
                        .await
                        .is_some_and(|config| config == command.expected_config)
                    {
                        match agent
                            .extension_manager
                            .remove_extension(&command.extension_key)
                            .await
                        {
                            Ok(()) => {
                                stopped |= self
                                    .agent
                                    .record_stopped_managed_runtime_session(
                                        &command.managed_mcp_id,
                                        &session_id,
                                        &agent,
                                    )
                                    .await;
                            }
                            Err(_) => failed = true,
                        }
                    }
                }
                if failed {
                    return Err(crate::mcp_platform::McpPlatformError::new(
                        crate::mcp_platform::McpPlatformErrorCode::RuntimeControlUnavailable,
                        "Goose could not close every owned managed MCP connection; stopped sessions can be restarted and the remaining connection can be retried",
                    ));
                }
                Ok(stopped)
            }
            HostRuntimeAction::Start => {
                let target_sessions = self
                    .agent
                    .managed_runtime_sessions
                    .lock()
                    .await
                    .stopped
                    .get(&command.managed_mcp_id)
                    .map(|sessions| {
                        sessions
                            .iter()
                            .map(|(session_id, agent)| (session_id.clone(), agent.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if target_sessions.is_empty() {
                    return Err(crate::mcp_platform::McpPlatformError::new(
                        crate::mcp_platform::McpPlatformErrorCode::RuntimeControlUnavailable,
                        "No Goose-owned stopped MCP runtime is available; reopen the session that previously used this MCP",
                    ));
                }
                let mut started = false;
                let mut failed = false;
                for (session_id, agent) in target_sessions {
                    if !self.agent.session_is_current(&session_id, &agent).await {
                        self.agent
                            .remove_stopped_managed_runtime_session(
                                &command.managed_mcp_id,
                                &session_id,
                                Some(&agent),
                            )
                            .await;
                        failed = true;
                        continue;
                    }
                    match agent
                        .start_managed_runtime_extension(
                            command.expected_config.clone(),
                            &session_id,
                        )
                        .await
                    {
                        Ok(()) => {
                            started |= self
                                .agent
                                .remove_stopped_managed_runtime_session(
                                    &command.managed_mcp_id,
                                    &session_id,
                                    Some(&agent),
                                )
                                .await;
                        }
                        Err(_) => failed = true,
                    }
                }
                if failed {
                    return Err(crate::mcp_platform::McpPlatformError::new(
                        crate::mcp_platform::McpPlatformErrorCode::RuntimeControlUnavailable,
                        "Goose could not restart every owned managed MCP connection; retry the remaining stopped sessions",
                    ));
                }
                if started {
                    Ok(true)
                } else {
                    Err(crate::mcp_platform::McpPlatformError::new(
                        crate::mcp_platform::McpPlatformErrorCode::RuntimeControlUnavailable,
                        "The owned MCP session is no longer active; reopen it before starting the runtime",
                    ))
                }
            }
        }
    }
}

impl GooseAcpAgent {
    async fn managed_runtime_stopped_session_ids(&self, managed_mcp_id: &str) -> HashSet<String> {
        self.managed_runtime_sessions
            .lock()
            .await
            .stopped
            .get(managed_mcp_id)
            .map(|sessions| sessions.keys().cloned().collect())
            .unwrap_or_default()
    }

    async fn session_is_current(&self, session_id: &str, agent: &Arc<Agent>) -> bool {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .is_some_and(|session| Arc::ptr_eq(&session.agent, agent))
    }

    async fn record_stopped_managed_runtime_session(
        &self,
        managed_mcp_id: &str,
        session_id: &str,
        agent: &Arc<Agent>,
    ) -> bool {
        if !self.session_is_current(session_id, agent).await {
            return false;
        }
        let mut runtimes = self.managed_runtime_sessions.lock().await;
        if !runtimes
            .active
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, agent))
        {
            return false;
        }
        runtimes
            .stopped
            .entry(managed_mcp_id.to_string())
            .or_default()
            .insert(session_id.to_string(), agent.clone())
            .is_none()
    }

    async fn remove_stopped_managed_runtime_session(
        &self,
        managed_mcp_id: &str,
        session_id: &str,
        expected_agent: Option<&Arc<Agent>>,
    ) -> bool {
        let mut runtimes = self.managed_runtime_sessions.lock().await;
        if let Some(agent) = expected_agent {
            let Some(sessions) = runtimes.stopped.get_mut(managed_mcp_id) else {
                return false;
            };
            if !sessions
                .get(session_id)
                .is_some_and(|recorded| Arc::ptr_eq(recorded, agent))
            {
                return false;
            }
            let removed = sessions.remove(session_id).is_some();
            if sessions.is_empty() {
                runtimes.stopped.remove(managed_mcp_id);
            }
            return removed;
        }
        if runtimes.active.contains_key(session_id) {
            return false;
        }
        let Some(sessions) = runtimes.stopped.get_mut(managed_mcp_id) else {
            return false;
        };
        let removed = sessions.remove(session_id);
        if sessions.is_empty() {
            runtimes.stopped.remove(managed_mcp_id);
        }
        removed.is_some()
    }
}

impl GooseAcpAgent {
    pub(super) async fn on_mcp_catalog_list(
        &self,
        req: McpCatalogListRequest,
    ) -> McpCatalogListResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => service
                .catalog_list(
                    &context,
                    CatalogListInput {
                        query: req.query,
                        trust_tiers: req.trust_tiers.into_iter().map(trust_from_wire).collect(),
                        source_ids: req.source_ids,
                        compatibility: req.compatibility.map(compatibility_from_wire),
                        cursor: req.cursor,
                        page_size: req.page_size,
                    },
                )
                .await
                .map(catalog_page_to_wire),
            Err(error) => Err(error),
        };
        McpCatalogListResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_catalog_detail(
        &self,
        req: McpCatalogDetailRequest,
    ) -> McpCatalogDetailResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let locator = match req.catalog {
            McpCatalogLocator::ManifestDigest { manifest_digest } => {
                CatalogLocator::ManifestDigest(manifest_digest)
            }
            McpCatalogLocator::CatalogRef {
                source_id,
                mcp_id,
                version,
            } => CatalogLocator::CatalogRef {
                source_id,
                mcp_id,
                version,
            },
        };
        let result = match service {
            Ok(service) => service
                .catalog_detail(&context, locator)
                .await
                .map(catalog_detail_to_wire),
            Err(error) => Err(error),
        };
        McpCatalogDetailResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_sources_policy_get(
        &self,
        _req: McpSourcesPolicyGetRequest,
    ) -> McpSourcesPolicyGetResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => service
                .sources_policy_get(&context)
                .await
                .map(source_policy_to_wire),
            Err(error) => Err(error),
        };
        McpSourcesPolicyGetResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_source_refresh(
        &self,
        req: McpSourceRefreshRequest,
    ) -> McpSourceRefreshResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .governed_source_refresh(
                    &context,
                    crate::mcp_platform::service::GovernedSourceRefreshInput {
                        source_id: req.source_id,
                    },
                )
                .await
                .map(governed_source_refresh_to_wire),
            Err(error) => Err(error),
        };
        McpSourceRefreshResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_source_provision_prepare(
        &self,
        req: McpSourceProvisionPrepareRequest,
    ) -> McpSourceProvisionPrepareResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => match load_local_source_provisioning_snapshot(&req.local_directory) {
                Ok(frozen) => service
                    .source_provision_prepare(
                        &context,
                        crate::mcp_platform::service::SourceProvisionPrepareInput { frozen },
                    )
                    .await
                    .map(source_provision_prepare_to_wire),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        McpSourceProvisionPrepareResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_source_provision_confirm(
        &self,
        req: McpSourceProvisionConfirmRequest,
    ) -> McpSourceProvisionConfirmResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .source_provision_confirm(
                    &context,
                    crate::mcp_platform::service::SourceProvisionConfirmInput {
                        provision_id: req.provision_id,
                        confirmation_token: req.confirmation_token,
                        confirm: req.confirm,
                    },
                )
                .await
                .map(source_provision_confirm_to_wire),
            Err(error) => Err(error),
        };
        McpSourceProvisionConfirmResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_governed_import(
        &self,
        req: McpGovernedImportRequest,
    ) -> McpGovernedImportResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => match load_governed_import_input(req.source) {
                Ok(LoadedGovernedImportInput::Manifest(input)) => service
                    .governed_manifest_import(&context, input)
                    .await
                    .map(governed_import_to_wire),
                Ok(LoadedGovernedImportInput::Directory(input)) => service
                    .governed_directory_import(&context, input)
                    .await
                    .map(governed_import_to_wire),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        McpGovernedImportResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_manual_stdio_sources_list(
        &self,
        _req: McpManualStdioSourcesListRequest,
    ) -> McpManualStdioSourcesListResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => service
                .manual_stdio_sources_list(&context)
                .map(manual_stdio_sources_to_wire),
            Err(error) => Err(error),
        };
        McpManualStdioSourcesListResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_manual_plan_create(
        &self,
        req: McpManualPlanCreateRequest,
    ) -> McpManualPlanCreateResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let connection = match req.connection {
            McpManualConnectionInput::RemoteHttp { endpoint, auth } => {
                crate::mcp_platform::service::ManualConnectionInput::RemoteHttp {
                    endpoint,
                    auth: match auth {
                        McpManualHttpAuth::None => {
                            crate::mcp_platform::service::ManualHttpAuth::None
                        }
                        McpManualHttpAuth::BearerReference { auth_reference } => {
                            crate::mcp_platform::service::ManualHttpAuth::BearerReference {
                                auth_reference,
                            }
                        }
                    },
                }
            }
            McpManualConnectionInput::StdioProvider { source_id } => {
                crate::mcp_platform::service::ManualConnectionInput::StdioProvider { source_id }
            }
        };
        let result = match service {
            Ok(service) => service
                .manual_plan_create(
                    &context,
                    crate::mcp_platform::service::ManualPlanCreateInput {
                        connection,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(plan_review_to_wire),
            Err(error) => Err(error),
        };
        McpManualPlanCreateResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_plan_create(
        &self,
        req: McpPlanCreateRequest,
    ) -> McpPlanCreateResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let intent = match req.intent {
            McpPlanIntent::Register {
                manifest_digest,
                installation_scope,
            } => PlanIntent::Register {
                manifest_digest,
                installation_scope: match installation_scope.unwrap_or_default() {
                    McpInstallationScope::User => InstallationScope::User,
                },
            },
            McpPlanIntent::RegisterCatalog {
                catalog,
                installation_scope,
            } => PlanIntent::RegisterCatalog {
                catalog: CatalogPlanTarget {
                    source_id: catalog.source_id,
                    mcp_id: catalog.mcp_id,
                    version: catalog.version,
                    manifest_digest: catalog.manifest_digest,
                },
                installation_scope: match installation_scope.unwrap_or_default() {
                    McpInstallationScope::User => InstallationScope::User,
                },
            },
            McpPlanIntent::Install { manifest_digest } => PlanIntent::Install { manifest_digest },
            McpPlanIntent::InstallCatalog { catalog } => PlanIntent::InstallCatalog {
                catalog: CatalogPlanTarget {
                    source_id: catalog.source_id,
                    mcp_id: catalog.mcp_id,
                    version: catalog.version,
                    manifest_digest: catalog.manifest_digest,
                },
            },
            McpPlanIntent::Update {
                managed_mcp_id,
                target_version,
            } => PlanIntent::Update {
                managed_mcp_id,
                target_version,
            },
            McpPlanIntent::Repair { managed_mcp_id } => PlanIntent::Repair { managed_mcp_id },
            McpPlanIntent::Uninstall {
                managed_mcp_id,
                preserve_user_data,
            } => PlanIntent::Uninstall {
                managed_mcp_id,
                preserve_user_data,
            },
        };
        let result = match service {
            Ok(service) => service
                .plan_create(
                    &context,
                    PlanCreateInput {
                        intent,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(plan_review_to_wire),
            Err(error) => Err(error),
        };
        McpPlanCreateResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_install_confirm(
        &self,
        req: McpInstallConfirmRequest,
    ) -> McpInstallConfirmResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .install_confirm(
                    &context,
                    InstallConfirmInput {
                        plan_id: req.plan_id,
                        plan_digest: req.plan_digest,
                        decision: match req.user_decision {
                            McpUserDecision::Confirm => UserDecision::Confirm,
                            McpUserDecision::Reject => UserDecision::Reject,
                        },
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpInstallConfirmResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_task_get(&self, req: McpTaskGetRequest) -> McpTaskGetResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => service
                .task_get(
                    &context,
                    TaskGetInput {
                        task_id: req.task_id,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpTaskGetResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_task_cancel(
        &self,
        req: McpTaskCancelRequest,
    ) -> McpTaskCancelResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .task_cancel(
                    &context,
                    TaskCancelInput {
                        task_id: req.task_id,
                        expected_revision: req.expected_revision,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpTaskCancelResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_task_retry(&self, req: McpTaskRetryRequest) -> McpTaskRetryResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .task_retry(
                    &context,
                    TaskRetryInput {
                        task_id: req.task_id,
                        expected_revision: req.expected_revision,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpTaskRetryResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_events_resume(
        &self,
        req: McpEventsResumeRequest,
    ) -> McpEventsResumeResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => service
                .events_resume(
                    &context,
                    EventsResumeInput {
                        after_event_id: req.after_event_id,
                        limit: req.limit,
                        task_ids: req.task_ids,
                    },
                )
                .await
                .map(events_to_wire),
            Err(error) => Err(error),
        };
        McpEventsResumeResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_list(&self, req: McpListRequest) -> McpListResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => service
                .managed_list(
                    &context,
                    ManagedListInput {
                        cursor: req.cursor,
                        page_size: req.page_size,
                        registration: req.registration.map(registration_from_wire),
                        installation: req.installation.map(installation_from_wire),
                        runtime: req.runtime.map(runtime_from_wire),
                        health: req.health.map(health_from_wire),
                        default_enabled: req.default_enabled,
                    },
                )
                .await
                .map(managed_page_to_wire),
            Err(error) => Err(error),
        };
        McpListResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_get(&self, req: McpGetRequest) -> McpGetResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => service
                .managed_get(
                    &context,
                    ManagedGetInput {
                        managed_mcp_id: req.managed_mcp_id,
                    },
                )
                .await
                .map(managed_detail_to_wire),
            Err(error) => Err(error),
        };
        McpGetResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_health_run(&self, req: McpHealthRunRequest) -> McpHealthRunResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .health_run(
                    &context,
                    HealthRunInput {
                        managed_mcp_id: req.managed_mcp_id,
                        mode: match req.mode {
                            McpHealthCheckMode::Registration => {
                                crate::mcp_platform::HealthCheckMode::Registration
                            }
                            McpHealthCheckMode::Runtime => {
                                crate::mcp_platform::HealthCheckMode::Runtime
                            }
                        },
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(task_to_wire),
            Err(error) => Err(error),
        };
        McpHealthRunResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_health_get(&self, req: McpHealthGetRequest) -> McpHealthGetResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => service
                .health_get(
                    &context,
                    HealthGetInput {
                        managed_mcp_id: req.managed_mcp_id,
                    },
                )
                .await
                .map(health_status_to_wire),
            Err(error) => Err(error),
        };
        McpHealthGetResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_set_default_enabled(
        &self,
        req: McpSetDefaultEnabledRequest,
    ) -> McpSetDefaultEnabledResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .set_default_enabled(
                    &context,
                    SetDefaultEnabledInput {
                        managed_mcp_id: req.managed_mcp_id,
                        enabled: req.enabled,
                        expected_revision: req.expected_revision,
                    },
                )
                .await
                .map(managed_summary_to_wire),
            Err(error) => Err(error),
        };
        McpSetDefaultEnabledResponse {
            outcome: outcome(&context, result),
        }
    }

    pub(super) async fn on_mcp_runtime_control(
        &self,
        req: McpRuntimeControlRequest,
    ) -> McpRuntimeControlResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let expected_binding = runtime_control_binding(&req.managed_mcp_id, req.expected_revision);
        let result = match service {
            Ok(_) if expected_binding.as_deref() != Some(req.runtime_binding.as_str()) => {
                Err(McpPlatformError::new(
                    crate::mcp_platform::McpPlatformErrorCode::PolicyDenied,
                    "managed MCP runtime binding does not match this authenticated Goose connection",
                ))
            }
            Ok(service) => service
                .managed_runtime_control(
                    &context,
                    ManagedRuntimeControlInput {
                        managed_mcp_id: req.managed_mcp_id,
                        expected_revision: req.expected_revision,
                        action: match req.action {
                            McpRuntimeAction::Start => ServiceRuntimeAction::Start,
                            McpRuntimeAction::Stop => ServiceRuntimeAction::Stop,
                        },
                    },
                    &AcpManagedRuntimeControlPort { agent: self },
                )
                .await
                .map(managed_summary_to_wire),
            Err(error) => Err(error),
        };
        McpRuntimeControlResponse {
            outcome: outcome(&context, result),
        }
    }
}

impl GooseAcpAgent {
    pub(super) async fn on_mcp_profile_list(
        &self,
        req: McpProfileListRequest,
    ) -> McpProfileListResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => match service.profile_list(&context, req.include_archived).await {
                Ok(items) => {
                    let mut summaries = Vec::with_capacity(items.len());
                    for profile in items {
                        let credential_status = service.profile_credential_status(&profile).await;
                        summaries.push(profile_to_wire(profile, Some(credential_status)));
                    }
                    Ok(McpProfilePage { items: summaries })
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        McpProfileListResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_LIST_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_profile_get(
        &self,
        req: McpProfileGetRequest,
    ) -> McpProfileGetResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => {
                let profile = service.profile_get(&context, &req.profile_id).await;
                let history = service.profile_history(&context, &req.profile_id).await;
                match (profile, history) {
                    (Ok(profile), Ok(history)) => {
                        let credential_status = service.profile_credential_status(&profile).await;
                        Ok(McpProfileDetail {
                            profile: profile_to_wire(profile, Some(credential_status)),
                            history: history
                                .into_iter()
                                .map(|item| McpProfileRevisionSummary {
                                    revision: item.revision,
                                    operation: item.operation,
                                    actor: item.actor,
                                    created_at_ms: item.created_at_ms,
                                })
                                .collect(),
                        })
                    }
                    (Err(error), _) | (_, Err(error)) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        McpProfileGetResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_GET_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_profile_create(
        &self,
        req: McpProfileCreateRequest,
    ) -> McpProfileCreateResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => match service
                .profile_create(
                    &context,
                    ProfileCreateInput {
                        name: req.name,
                        description: req.description,
                        managed_mcp_ids: req.managed_mcp_ids,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
            {
                Ok(profile) => {
                    let credential_status = service.profile_credential_status(&profile).await;
                    Ok(profile_to_wire(profile, Some(credential_status)))
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        McpProfileCreateResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_CREATE_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_profile_update(
        &self,
        req: McpProfileUpdateRequest,
    ) -> McpProfileUpdateResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => match service
                .profile_update(
                    &context,
                    ProfileUpdateInput {
                        profile_id: req.profile_id,
                        expected_revision: req.expected_revision,
                        name: req.name,
                        description: req.description,
                        managed_mcp_ids: req.managed_mcp_ids,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
            {
                Ok(profile) => {
                    let credential_status = service.profile_credential_status(&profile).await;
                    Ok(profile_to_wire(profile, Some(credential_status)))
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        McpProfileUpdateResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_UPDATE_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_profile_restore(
        &self,
        req: McpProfileRestoreRequest,
    ) -> McpProfileRestoreResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => match service
                .profile_restore(
                    &context,
                    ProfileRestoreInput {
                        profile_id: req.profile_id,
                        source_revision: req.source_revision,
                        expected_revision: req.expected_revision,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
            {
                Ok(profile) => {
                    let credential_status = service.profile_credential_status(&profile).await;
                    Ok(profile_to_wire(profile, Some(credential_status)))
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        McpProfileRestoreResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_RESTORE_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_profile_archive(
        &self,
        req: McpProfileArchiveRequest,
    ) -> McpProfileArchiveResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => match service
                .profile_archive(
                    &context,
                    ProfileArchiveInput {
                        profile_id: req.profile_id,
                        expected_revision: req.expected_revision,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
            {
                Ok(profile) => {
                    let credential_status = service.profile_credential_status(&profile).await;
                    Ok(profile_to_wire(profile, Some(credential_status)))
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        McpProfileArchiveResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_ARCHIVE_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_profile_draft_create(
        &self,
        req: McpProfileDraftCreateRequest,
    ) -> McpProfileDraftCreateResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Ok(service) => service
                .profile_draft_create(&context, &req.text, &req.locale)
                .await
                .map(draft_to_wire),
            Err(error) => Err(error),
        };
        McpProfileDraftCreateResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_DRAFT_CREATE_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_profile_model_recommend(
        &self,
        req: McpProfileModelRecommendRequest,
    ) -> McpProfileModelRecommendResponse {
        let (context, service) = self.mcp_platform_context_and_service_read_only().await;
        let result = match service {
            Err(error) => Err(error),
            Ok(_) => match self.provider_inventory.entries(&req.provider_ids).await {
                Err(_) => Err(McpPlatformError::new(
                    crate::mcp_platform::McpPlatformErrorCode::RepositoryUnavailable,
                    "provider inventory unavailable",
                )),
                Ok(entries) => {
                    let prefers_reasoning = req
                        .text
                        .to_ascii_lowercase()
                        .split_whitespace()
                        .any(|term| matches!(term, "code" | "coding" | "reason" | "analysis"));
                    let mut candidates = entries
                        .into_iter()
                        .filter(|entry| entry.configured)
                        .flat_map(|entry| {
                            let provider_id = entry.provider_id;
                            let default_model = entry.default_model;
                            entry.models.into_iter().map(move |model| {
                                let reasoning_match =
                                    prefers_reasoning && model.reasoning.unwrap_or(false);
                                let is_default = model.id == default_model;
                                let reason_code = if reasoning_match {
                                    "inventory_reasoning_match"
                                } else if model.recommended {
                                    "inventory_recommended"
                                } else if is_default {
                                    "provider_default"
                                } else {
                                    "known_model"
                                };
                                let confidence = if model.recommended {
                                    0.9
                                } else if is_default {
                                    0.8
                                } else {
                                    0.55
                                } + if reasoning_match { 0.05 } else { 0.0 };
                                McpModelRecommendationCandidate {
                                    provider_id: provider_id.clone(),
                                    model_id: model.id,
                                    confidence,
                                    reason_codes: vec![reason_code.to_string()],
                                    caveat_codes: if model.context_limit.is_none() {
                                        vec!["context_limit_unknown".to_string()]
                                    } else {
                                        Vec::new()
                                    },
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    candidates.sort_by(|left, right| {
                        right
                            .confidence
                            .total_cmp(&left.confidence)
                            .then_with(|| left.provider_id.cmp(&right.provider_id))
                            .then_with(|| left.model_id.cmp(&right.model_id))
                    });
                    candidates.truncate(12);
                    Ok(McpModelRecommendation {
                        candidates,
                        inventory_only: true,
                        mutated: false,
                    })
                }
            },
        };
        McpProfileModelRecommendResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_MODEL_RECOMMEND_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_profile_connection_test(
        &self,
        req: McpProfileConnectionTestRequest,
    ) -> McpProfileConnectionTestResponse {
        let mut stages = Vec::new();
        let deadline = Instant::now() + CONNECTION_TEST_TOTAL_DEADLINE;
        let permit = connection_test_permits().try_acquire_owned();
        if permit.is_err() {
            stages.push(connection_test_stage(
                McpConnectionTestPhase::Eligibility,
                McpConnectionTestStatus::Failed,
                McpConnectionTestCode::ResourceLimitExceeded,
                "A connection test is already in progress.",
                "Wait for the active test to finish and retry.",
                None,
                0,
            ));
            return connection_test_response(stages, false);
        }
        let mut permit = Some(permit.expect("checked above"));
        let (context, service) = match tokio::time::timeout(
            remaining_connection_test_time(deadline).unwrap_or(Duration::ZERO),
            self.mcp_platform_context_and_service(),
        )
        .await
        {
            Ok(context_and_service) => context_and_service,
            Err(_) => {
                stages.push(connection_test_stage(
                    McpConnectionTestPhase::Eligibility,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::McpTimeout,
                    "The connection test reached its total deadline before verification began.",
                    "Retry after checking MCP platform availability.",
                    None,
                    0,
                ));
                return connection_test_response(stages, false);
            }
        };
        let service = match service {
            Err(_) => {
                stages.push(connection_test_stage(
                    McpConnectionTestPhase::Eligibility,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::PolicyDenied,
                    "The managed MCP profile could not be verified for testing.",
                    "Review the profile governance status and retry.",
                    None,
                    0,
                ));
                return connection_test_response(stages, false);
            }
            Ok(service) => service,
        };
        let remote_http_network_policy = service.managed_remote_http_network_policy();
        let started = Instant::now();
        let targets = match tokio::time::timeout(
            remaining_connection_test_time(deadline).unwrap_or(Duration::ZERO),
            service.profile_connection_test_targets(&context, &req.profile_id),
        )
        .await
        {
            Ok(Ok(targets)) => {
                stages.push(connection_test_stage(
                    McpConnectionTestPhase::Eligibility,
                    McpConnectionTestStatus::Passed,
                    McpConnectionTestCode::Eligible,
                    "The profile contains enabled, healthy, governed MCP connections.",
                    "No action is needed.",
                    None,
                    elapsed_test_ms(started),
                ));
                targets
            }
            Ok(Err(_)) | Err(_) => {
                stages.push(connection_test_stage(
                    McpConnectionTestPhase::Eligibility,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::ProfileNotReady,
                    "The profile or one of its managed MCP connections is not ready for testing.",
                    "Enable, repair, and complete health checks for every managed MCP in the profile.",
                    None,
                    elapsed_test_ms(started),
                ));
                return connection_test_response(stages, false);
            }
        };

        if targets.len() > CONNECTION_TEST_MAX_PROFILE_MCPS {
            stages.push(connection_test_stage(
                McpConnectionTestPhase::Eligibility,
                McpConnectionTestStatus::Failed,
                McpConnectionTestCode::ResourceLimitExceeded,
                "The profile exceeds the connection-test resource limit.",
                "Reduce the number of enabled managed MCPs and retry.",
                None,
                0,
            ));
            return connection_test_response(stages, false);
        }

        let mut discovered_tools = Vec::new();
        let mut mcp_ok = true;
        for managed_mcp_id in targets {
            let target_started = Instant::now();
            let target = match remaining_connection_test_time(deadline) {
                Some(remaining) => match tokio::time::timeout(
                    remaining,
                    service.profile_connection_test_target(&managed_mcp_id),
                )
                .await
                {
                    Ok(Ok(target)) => target,
                    _ => {
                        stages.push(connection_test_stage(
                            McpConnectionTestPhase::McpConnectInitialize,
                            McpConnectionTestStatus::Failed,
                            McpConnectionTestCode::RuntimeActivationFailed,
                            "The managed MCP runtime could not be safely activated for testing.",
                            "Verify the managed MCP installation and retry.",
                            Some(managed_mcp_id),
                            elapsed_test_ms(target_started),
                        ));
                        mcp_ok = false;
                        break;
                    }
                },
                None => {
                    stages.push(connection_test_stage(
                        McpConnectionTestPhase::McpConnectInitialize,
                        McpConnectionTestStatus::Failed,
                        McpConnectionTestCode::McpTimeout,
                        "The connection test reached its total deadline.",
                        "Retry after checking MCP availability.",
                        Some(managed_mcp_id),
                        elapsed_test_ms(target_started),
                    ));
                    mcp_ok = false;
                    break;
                }
            };
            let result = self
                .test_profile_mcp_target(
                    target,
                    remote_http_network_policy.clone(),
                    deadline,
                    &mut permit,
                )
                .await;
            mcp_ok &= result.passed;
            stages.extend(result.stages);
            if !mcp_ok {
                break;
            }
            if !append_bounded_tools(&mut discovered_tools, result.tools) {
                stages.push(connection_test_stage(
                    McpConnectionTestPhase::ToolDiscovery,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::ResourceLimitExceeded,
                    "Discovered tools exceed the connection-test resource limit.",
                    "Reduce the tools exposed by the managed MCPs and retry.",
                    None,
                    0,
                ));
                mcp_ok = false;
                break;
            }
        }

        let model_stage = if connection_test_may_send_model(mcp_ok, &discovered_tools) {
            self.test_profile_model(&req.provider_id, &req.model_id, &discovered_tools, deadline)
                .await
        } else {
            connection_test_stage(
                McpConnectionTestPhase::ModelRequest,
                McpConnectionTestStatus::Skipped,
                McpConnectionTestCode::PrerequisitesFailed,
                "The model request was not sent because managed MCP verification did not complete.",
                "Resolve the managed MCP test failure and retry.",
                None,
                0,
            )
        };
        let model_ok = model_stage.status == McpConnectionTestStatus::Passed;
        stages.push(model_stage);

        let tool_stage = if discovered_tools.is_empty() {
            connection_test_stage(
                McpConnectionTestPhase::ToolVisibility,
                McpConnectionTestStatus::Skipped,
                McpConnectionTestCode::ToolVisibilitySkipped,
                "No governed MCP tools were available to include in the model request.",
                "Review MCP tool discovery and retry.",
                None,
                0,
            )
        } else if model_ok {
            connection_test_stage(
                McpConnectionTestPhase::ToolVisibility,
                McpConnectionTestStatus::Passed,
                McpConnectionTestCode::ToolVisibilityValidated,
                "Tool discovery succeeded; the isolated model request intentionally sent no MCP tool definitions.",
                "No action is needed.",
                None,
                0,
            )
        } else {
            connection_test_stage(
                McpConnectionTestPhase::ToolVisibility,
                McpConnectionTestStatus::Skipped,
                McpConnectionTestCode::ToolVisibilitySkipped,
                "Tool visibility was not verified because the model request did not complete.",
                "Resolve the model connection issue and retry.",
                None,
                0,
            )
        };
        stages.push(tool_stage);

        connection_test_response(stages, mcp_ok && model_ok && !discovered_tools.is_empty())
    }

    async fn test_profile_mcp_target(
        &self,
        target: crate::mcp_platform::ProfileConnectionTestTarget,
        remote_http_network_policy: Arc<dyn crate::mcp_platform::RemoteHttpNetworkPolicy>,
        deadline: Instant,
        permit: &mut Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> ProfileConnectionTestTargetResult {
        if connection_test_forbids_unbounded_transport(&target.extension_config) {
            return ProfileConnectionTestTargetResult {
                stages: vec![connection_test_stage(
                    McpConnectionTestPhase::McpConnectInitialize,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::RuntimeActivationFailed,
                    "Local managed MCP runtimes cannot be safely activated for connection testing because their transport has no pre-decode response limit.",
                    "Use a governed remote HTTP MCP until local runtime containment is available.",
                    Some(target.managed_mcp_id),
                    0,
                )],
                tools: Vec::new(),
                passed: false,
            };
        }
        let manager = Arc::new(
            ExtensionManager::new_without_provider_with_managed_remote_http_policy(
                self.config_dir.join("mcp-connection-test"),
                remote_http_network_policy,
            ),
        );
        let extension_name = target.extension_config.key();
        let setup_extension_name = extension_name.clone();
        #[cfg(test)]
        let setup_extension_name_for_task = setup_extension_name.clone();
        let started = Instant::now();
        let setup_manager = manager.clone();
        #[cfg(not(test))]
        let setup_config = target.extension_config;
        #[cfg(test)]
        let mut setup_task = tokio::spawn(async move {
            setup_manager
                .add_mock_extension(
                    setup_extension_name_for_task,
                    Arc::new(ConnectionTestDiscoveryClient),
                )
                .await;
            Ok(())
        });
        #[cfg(not(test))]
        let mut setup_task = tokio::spawn(async move {
            setup_manager
                .add_extension(
                    setup_config,
                    None,
                    None,
                    Some("mcp-profile-connection-test"),
                )
                .await
        });
        let connected = tokio::time::timeout(
            remaining_connection_test_time(deadline).unwrap_or(Duration::ZERO),
            &mut setup_task,
        )
        .await;
        let connect_stage = match connected {
            Ok(Ok(Ok(()))) => connection_test_stage(
                McpConnectionTestPhase::McpConnectInitialize,
                McpConnectionTestStatus::Passed,
                McpConnectionTestCode::Eligible,
                "The MCP transport connected and initialized successfully.",
                "No action is needed.",
                Some(target.managed_mcp_id.clone()),
                elapsed_test_ms(started),
            ),
            Ok(Ok(Err(_))) | Ok(Err(_)) => connection_test_stage(
                McpConnectionTestPhase::McpConnectInitialize,
                McpConnectionTestStatus::Failed,
                McpConnectionTestCode::McpConnectFailed,
                "The MCP transport could not connect or initialize.",
                "Check the managed MCP health status and its approved credentials, then retry.",
                Some(target.managed_mcp_id.clone()),
                elapsed_test_ms(started),
            ),
            Err(_) => {
                let _ = spawn_connection_test_reaper(
                    manager.clone(),
                    setup_extension_name,
                    Some(setup_task),
                    take_connection_test_permit(permit),
                );
                return ProfileConnectionTestTargetResult {
                    stages: vec![
                        connection_test_stage(
                            McpConnectionTestPhase::McpConnectInitialize,
                            McpConnectionTestStatus::Failed,
                            McpConnectionTestCode::McpTimeout,
                            "The MCP transport did not initialize before the test deadline.",
                            "Check network or runtime availability and retry.",
                            Some(target.managed_mcp_id.clone()),
                            elapsed_test_ms(started),
                        ),
                        connection_test_stage(
                            McpConnectionTestPhase::Cleanup,
                            McpConnectionTestStatus::Failed,
                            McpConnectionTestCode::CleanupFailed,
                            "Temporary MCP cleanup is being supervised after initialization timed out.",
                            "Retry only after the managed MCP runtime is confirmed stopped.",
                            Some(target.managed_mcp_id),
                            0,
                        ),
                    ],
                    tools: Vec::new(),
                    passed: false,
                };
            }
        };
        let discovery_started = Instant::now();
        let discovery_cancellation = CancellationToken::new();
        let discovered = if connect_stage.status == McpConnectionTestStatus::Passed {
            match remaining_connection_test_time(deadline) {
                Some(remaining) => {
                    let discovery = manager.get_prefixed_tools_bounded(
                        "mcp-profile-connection-test",
                        &extension_name,
                        ToolDiscoveryLimits {
                            max_tools: CONNECTION_TEST_MAX_TOOLS,
                            max_tool_bytes: CONNECTION_TEST_MAX_TOOL_BYTES,
                            max_total_bytes: CONNECTION_TEST_MAX_TOTAL_TOOL_BYTES,
                            max_pages: CONNECTION_TEST_MAX_TOOL_PAGES,
                            max_cursors: CONNECTION_TEST_MAX_TOOL_CURSORS,
                            max_cursor_bytes: CONNECTION_TEST_MAX_CURSOR_BYTES,
                        },
                        discovery_cancellation.clone(),
                    );
                    Some(tokio::select! {
                        result = discovery => BoundedDiscovery::Completed(result),
                        _ = tokio::time::sleep(remaining) => {
                            discovery_cancellation.cancel();
                            BoundedDiscovery::TimedOut
                        }
                    })
                }
                None => {
                    discovery_cancellation.cancel();
                    Some(BoundedDiscovery::TimedOut)
                }
            }
        } else {
            None
        };
        let cleanup_started = Instant::now();
        let cleanup_ok = matches!(
            tokio::time::timeout(
                remaining_connection_test_time(deadline).unwrap_or(Duration::ZERO),
                manager.remove_extension(&extension_name),
            )
            .await,
            Ok(Ok(()))
        );
        if !cleanup_ok {
            let _ = spawn_connection_test_reaper(
                manager.clone(),
                extension_name.clone(),
                None,
                take_connection_test_permit(permit),
            );
        }
        let managed_mcp_id = target.managed_mcp_id;
        let (discovery_stage, tools) = match discovered {
            Some(BoundedDiscovery::Completed(Ok(tools))) if !tools.is_empty() => (
                connection_test_stage(
                    McpConnectionTestPhase::ToolDiscovery,
                    McpConnectionTestStatus::Passed,
                    McpConnectionTestCode::Eligible,
                    "The MCP exposed governed tools for this test.",
                    "No action is needed.",
                    Some(managed_mcp_id.clone()),
                    elapsed_test_ms(discovery_started),
                ),
                tools,
            ),
            Some(BoundedDiscovery::Completed(Ok(_))) => (
                connection_test_stage(
                    McpConnectionTestPhase::ToolDiscovery,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::NoToolsExposed,
                    "The MCP initialized but did not expose any governed tools.",
                    "Review the MCP server configuration and retry.",
                    Some(managed_mcp_id.clone()),
                    elapsed_test_ms(discovery_started),
                ),
                Vec::new(),
            ),
            Some(BoundedDiscovery::Completed(Err(_))) => (
                connection_test_stage(
                    McpConnectionTestPhase::ToolDiscovery,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::ToolDiscoveryFailed,
                    "The MCP initialized but tool discovery failed.",
                    "Check the MCP server health and retry.",
                    Some(managed_mcp_id.clone()),
                    elapsed_test_ms(discovery_started),
                ),
                Vec::new(),
            ),
            Some(BoundedDiscovery::TimedOut) => (
                connection_test_stage(
                    McpConnectionTestPhase::ToolDiscovery,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::McpTimeout,
                    "Tool discovery did not complete before the test deadline.",
                    "Check network or runtime availability and retry.",
                    Some(managed_mcp_id.clone()),
                    elapsed_test_ms(discovery_started),
                ),
                Vec::new(),
            ),
            None => (
                connection_test_stage(
                    McpConnectionTestPhase::ToolDiscovery,
                    McpConnectionTestStatus::Skipped,
                    McpConnectionTestCode::PrerequisitesFailed,
                    "Tool discovery was not attempted because MCP initialization did not complete.",
                    "Resolve the MCP connection issue and retry.",
                    Some(managed_mcp_id.clone()),
                    0,
                ),
                Vec::new(),
            ),
        };
        let cleanup_stage = connection_test_stage(
            McpConnectionTestPhase::Cleanup,
            if cleanup_ok {
                McpConnectionTestStatus::Passed
            } else {
                McpConnectionTestStatus::Failed
            },
            if cleanup_ok {
                McpConnectionTestCode::Eligible
            } else {
                McpConnectionTestCode::CleanupFailed
            },
            if cleanup_ok {
                "The temporary managed MCP connection was closed."
            } else {
                "The temporary managed MCP connection could not be safely closed."
            },
            if cleanup_ok {
                "No action is needed."
            } else {
                "Retry after verifying the managed MCP runtime is available."
            },
            Some(managed_mcp_id),
            elapsed_test_ms(cleanup_started),
        );
        let passed = connect_stage.status == McpConnectionTestStatus::Passed
            && discovery_stage.status == McpConnectionTestStatus::Passed
            && cleanup_ok;
        ProfileConnectionTestTargetResult {
            stages: vec![connect_stage, discovery_stage, cleanup_stage],
            tools: if passed { tools } else { Vec::new() },
            passed,
        }
    }

    async fn test_profile_model(
        &self,
        provider_id: &str,
        model_id: &str,
        _tools: &[Tool],
        deadline: Instant,
    ) -> McpConnectionTestStage {
        let started = Instant::now();
        let entries = match tokio::time::timeout(
            remaining_connection_test_time(deadline).unwrap_or(Duration::ZERO),
            self.provider_inventory.entries(&[provider_id.to_string()]),
        )
        .await
        {
            Ok(Ok(entries)) => entries,
            Ok(Err(_)) | Err(_) => {
                return connection_test_stage(
                    McpConnectionTestPhase::ModelRequest,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::ProviderNotConfigured,
                    "The selected provider configuration is unavailable.",
                    "Configure the selected provider and retry.",
                    None,
                    elapsed_test_ms(started),
                );
            }
        };
        let Some(entry) = entries
            .into_iter()
            .find(|entry| entry.provider_id == provider_id)
        else {
            return connection_test_stage(
                McpConnectionTestPhase::ModelRequest,
                McpConnectionTestStatus::Failed,
                McpConnectionTestCode::ProviderNotConfigured,
                "The selected provider is not an existing configured provider.",
                "Choose an existing configured provider and retry.",
                None,
                elapsed_test_ms(started),
            );
        };
        if !entry.configured {
            return connection_test_stage(
                McpConnectionTestPhase::ModelRequest,
                McpConnectionTestStatus::Failed,
                McpConnectionTestCode::ProviderNotConfigured,
                "The selected provider is not configured.",
                "Complete provider configuration and retry.",
                None,
                elapsed_test_ms(started),
            );
        }
        if !entry.models.iter().any(|model| model.id == model_id) {
            return connection_test_stage(
                McpConnectionTestPhase::ModelRequest,
                McpConnectionTestStatus::Failed,
                McpConnectionTestCode::ModelNotConfigured,
                "The selected model is not available from the configured provider inventory.",
                "Choose a model from the configured provider inventory and retry.",
                None,
                elapsed_test_ms(started),
            );
        }
        let provider = match tokio::time::timeout(
            remaining_connection_test_time(deadline).unwrap_or(Duration::ZERO),
            self.create_provider(provider_id, None),
        )
        .await
        {
            Ok(Ok(provider)) => provider,
            _ => {
                return connection_test_stage(
                    McpConnectionTestPhase::ModelRequest,
                    McpConnectionTestStatus::Failed,
                    McpConnectionTestCode::ProviderInitializationFailed,
                    "The selected provider could not be initialized with its existing configuration.",
                    "Review provider authentication and connectivity, then retry.",
                    None,
                    elapsed_test_ms(started),
                );
            }
        };
        let messages = vec![Message::user().with_text("Reply with OK only.")];
        let model = goose_providers::model::ModelConfig::new(model_id);
        let completed = tokio::time::timeout(
            remaining_connection_test_time(deadline).unwrap_or(Duration::ZERO),
            provider.complete(
                &model,
                "You are performing a bounded connectivity test. Do not call tools.",
                &messages,
                &[],
            ),
        )
        .await;
        match completed {
            Ok(Ok(_)) => connection_test_stage(
                McpConnectionTestPhase::ModelRequest,
                McpConnectionTestStatus::Passed,
                McpConnectionTestCode::Eligible,
                "The selected provider and model completed a minimal request.",
                "No action is needed.",
                None,
                elapsed_test_ms(started),
            ),
            _ => connection_test_stage(
                McpConnectionTestPhase::ModelRequest,
                McpConnectionTestStatus::Failed,
                McpConnectionTestCode::ModelRequestFailed,
                "The selected provider or model could not complete the minimal request.",
                "Review provider authentication, model access, and connectivity, then retry.",
                None,
                elapsed_test_ms(started),
            ),
        }
    }

    pub(super) async fn on_mcp_profile_apply_plan_create(
        &self,
        req: McpProfileApplyPlanCreateRequest,
    ) -> McpProfileApplyPlanCreateResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .profile_apply_plan_create(
                    &context,
                    ProfileApplyPlanInput {
                        profile_id: req.profile_id,
                        profile_revision: req.profile_revision,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(apply_plan_to_wire),
            Err(error) => Err(error),
        };
        McpProfileApplyPlanCreateResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_APPLY_PLAN_CREATE_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_profile_apply_confirm(
        &self,
        req: McpProfileApplyConfirmRequest,
    ) -> McpProfileApplyConfirmResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .profile_apply_confirm(&context, &req.plan_id, &req.confirmation_token, req.confirm)
                .await
                .map(|token| McpProfileApplicationToken {
                    token: token.token,
                    plan_id: token.plan_id,
                    profile_id: token.profile_id,
                    profile_revision: token.profile_revision,
                    expires_at_ms: token.expires_at_ms,
                }),
            Err(error) => Err(error),
        };
        McpProfileApplyConfirmResponse {
            outcome: outcome_with_error_context(
                &context,
                ErrorContext::profile(MCP_PROFILE_APPLY_CONFIRM_METHOD),
                result,
            ),
        }
    }

    pub(super) async fn on_mcp_https_manifest_prepare(
        &self,
        req: McpHttpsManifestPrepareRequest,
    ) -> McpHttpsManifestPrepareResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .https_manifest_prepare(&context, HttpsManifestPrepareInput { url: req.url })
                .await
                .map(https_manifest_prepare_to_wire),
            Err(error) => Err(error),
        };
        McpHttpsManifestPrepareResponse {
            outcome: outcome_with_error_context(&context, ErrorContext::default(), result),
        }
    }

    pub(super) async fn on_mcp_https_manifest_confirm(
        &self,
        req: McpHttpsManifestConfirmRequest,
    ) -> McpHttpsManifestConfirmResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .https_manifest_confirm(
                    &context,
                    HttpsManifestConfirmInput {
                        provision_id: req.provision_id,
                        token: req.confirmation_token,
                        confirm: req.confirm,
                    },
                )
                .await
                .map(https_manifest_confirm_to_wire),
            Err(error) => Err(error),
        };
        McpHttpsManifestConfirmResponse {
            outcome: outcome_with_error_context(&context, ErrorContext::default(), result),
        }
    }

    pub(super) async fn on_mcp_https_provision_plan_create(
        &self,
        req: McpHttpsProvisionPlanCreateRequest,
    ) -> McpHttpsProvisionPlanCreateResponse {
        let (context, service) = self.mcp_platform_context_and_service().await;
        let result = match service {
            Ok(service) => service
                .https_manifest_plan_review(
                    &context,
                    HttpsManifestPlanReviewInput {
                        provision_id: req.provision_id,
                        expected_manifest_digest: req.expected_manifest_digest,
                        idempotency_key: req.idempotency_key,
                    },
                )
                .await
                .map(|review| review)
                .map(plan_review_to_wire)
                .map(|wire| wire)
                .map(McpHttpsProvisionPlanReview::from),
            Err(error) => Err(error),
        };
        McpHttpsProvisionPlanCreateResponse {
            outcome: outcome_with_error_context(&context, ErrorContext::default(), result),
        }
    }
}

fn https_manifest_prepare_to_wire(
    value: crate::mcp_platform::service::HttpsManifestPrepareResult,
) -> McpHttpsManifestPrepareResult {
    McpHttpsManifestPrepareResult {
        provision_id: value.provision_id,
        confirmation_token: value.server_token,
        expires_at_ms: value.expires_at_ms,
        preview: https_manifest_preview_to_wire(value.preview),
    }
}

fn https_manifest_confirm_to_wire(
    value: crate::mcp_platform::service::HttpsManifestConfirmResult,
) -> McpHttpsManifestConfirmResult {
    McpHttpsManifestConfirmResult {
        provision_id: value.provision_id,
        confirmed_at_ms: value.confirmed_at_ms,
        manifest_digest: value.manifest_digest,
        preview: https_manifest_preview_to_wire(value.preview),
    }
}

fn https_manifest_preview_to_wire(
    value: crate::mcp_platform::service::HttpsManifestPreview,
) -> McpHttpsManifestSecurityPreview {
    McpHttpsManifestSecurityPreview {
        manifest_id: value.manifest_id,
        version: value.version,
        redacted_origin: value.redacted_origin,
        raw_digest: value.raw_digest,
        parsed_digest: value.parsed_digest,
        redirect_chain_digest: value.redirect_chain_digest,
        dns_evidence_digest: value.dns_evidence_digest,
        warnings: Vec::new(),
    }
}

fn profile_to_wire(
    profile: crate::mcp_platform::McpProfile,
    credential_status: Option<crate::mcp_platform::service::CredentialStatus>,
) -> McpProfileSummary {
    McpProfileSummary {
        profile_id: profile.profile_id,
        name: profile.name,
        description: profile.description,
        revision: profile.revision,
        archived: profile.archived,
        entries: profile
            .entries
            .into_iter()
            .map(|entry| McpProfileEntry {
                managed_mcp_id: entry.managed_mcp_id,
                ordinal: entry.ordinal,
            })
            .collect(),
        created_at_ms: profile.created_at_ms,
        updated_at_ms: profile.updated_at_ms,
        credential_status: credential_status.map(credential_status_to_wire),
    }
}

fn draft_to_wire(draft: crate::mcp_platform::ProfileDraft) -> McpProfileDraft {
    McpProfileDraft {
        locale: draft.locale,
        name: draft.name,
        description: draft.description,
        entries: draft
            .entries
            .into_iter()
            .map(|entry| McpProfileEntry {
                managed_mcp_id: entry.managed_mcp_id,
                ordinal: entry.ordinal,
            })
            .collect(),
        candidates: draft
            .candidates
            .into_iter()
            .map(|candidate| McpProfileDraftCandidate {
                managed_mcp_id: candidate.managed_mcp_id,
                mcp_id: candidate.mcp_id,
                name: candidate.name,
                description: candidate.description,
                confidence: candidate.confidence,
                reason_code: candidate.reason_code,
            })
            .collect(),
        unresolved_terms: draft.unresolved_terms,
        low_confidence: draft.low_confidence,
        persisted: draft.persisted,
    }
}

fn apply_plan_to_wire(plan: crate::mcp_platform::ProfileApplyPlanView) -> McpProfileApplyPlan {
    McpProfileApplyPlan {
        plan_id: plan.plan_id,
        profile_id: plan.profile_id,
        profile_revision: plan.profile_revision,
        merge_policy: plan.merge_policy,
        entries: plan
            .entries
            .into_iter()
            .map(|entry| McpProfileApplyEntry {
                managed_mcp_id: entry.managed_mcp_id,
                mcp_id: entry.mcp_id,
                name: entry.name,
                version: entry.version,
                health: entry.health,
                auth_ready: entry.auth_ready,
                policy_ready: entry.policy_ready,
                readiness: entry.readiness,
            })
            .collect(),
        expires_at_ms: plan.expires_at_ms,
        confirmation: McpProfileApplyConfirmation {
            confirmation_token: plan.confirmation.confirmation_token,
        },
    }
}

fn outcome<T>(
    context: &RequestContext,
    result: crate::mcp_platform::McpPlatformResult<T>,
) -> McpPlatformOutcome<T> {
    outcome_with_error_context(context, ErrorContext::default(), result)
}

fn outcome_with_error_context<T>(
    context: &RequestContext,
    error_context: ErrorContext,
    result: crate::mcp_platform::McpPlatformResult<T>,
) -> McpPlatformOutcome<T> {
    match result {
        Ok(value) => McpPlatformOutcome::success(value),
        Err(error) => McpPlatformOutcome::error(error_to_wire(context, error_context, error)),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ErrorContext {
    phase_unavailable: Option<PhaseUnavailableContext>,
}

impl ErrorContext {
    const PROFILE_PHASE: &'static str = "4B";

    const fn profile(operation: &'static str) -> Self {
        Self {
            phase_unavailable: Some(PhaseUnavailableContext {
                phase: Self::PROFILE_PHASE,
                operation,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PhaseUnavailableContext {
    phase: &'static str,
    operation: &'static str,
}

fn error_to_wire(
    context: &RequestContext,
    error_context: ErrorContext,
    error: McpPlatformError,
) -> McpPlatformErrorEnvelope {
    use crate::mcp_platform::McpPlatformErrorCode as Code;

    let (code, message, retryable, details) = match error.code() {
        Code::NotFound => (
            McpPlatformErrorCodeDto::NotFound,
            "The requested MCP platform record was not found.",
            false,
            Some(McpPlatformErrorDetails::RecordMissing {}),
        ),
        Code::IntegrityError => (
            McpPlatformErrorCodeDto::IntegrityError,
            "Stored MCP platform data failed integrity validation.",
            false,
            Some(McpPlatformErrorDetails::IntegrityValidationFailed {}),
        ),
        Code::IntegrityUnavailable => (
            McpPlatformErrorCodeDto::RepositoryUnavailable,
            "The MCP platform repository is temporarily unavailable.",
            true,
            Some(McpPlatformErrorDetails::RepositoryTemporarilyUnavailable {}),
        ),
        Code::PolicyDenied => (
            McpPlatformErrorCodeDto::PolicyDenied,
            "The MCP platform policy denied this request.",
            false,
            Some(McpPlatformErrorDetails::PolicyDecision {
                reason_codes: Vec::new(),
            }),
        ),
        Code::NotImplementedForPhase => (
            McpPlatformErrorCodeDto::NotImplementedForPhase,
            if error_context.phase_unavailable.is_some() {
                "This operation is not available in phase 4B."
            } else {
                "This distribution is visible but cannot be planned in phase 2C."
            },
            false,
            Some(match error_context.phase_unavailable {
                Some(phase_unavailable) => McpPlatformErrorDetails::PhaseUnavailable {
                    phase: phase_unavailable.phase.to_string(),
                    operation: phase_unavailable.operation.to_string(),
                },
                None => McpPlatformErrorDetails::PhaseUnavailable {
                    phase: "2C".to_string(),
                    operation: "plan".to_string(),
                },
            }),
        ),
        Code::OperationNotSupported => (
            McpPlatformErrorCodeDto::OperationNotSupported,
            "This operation is not supported by the current closed adapter contract.",
            false,
            Some(McpPlatformErrorDetails::PhaseUnavailable {
                phase: "3C".to_string(),
                operation: "lifecycle".to_string(),
            }),
        ),
        Code::RuntimeControlUnavailable => (
            McpPlatformErrorCodeDto::RuntimeControlUnavailable,
            "The MCP managed runtime control surface is temporarily unavailable.",
            true,
            Some(McpPlatformErrorDetails::ExternalCapabilityUnavailable {
                capability: "managed_runtime_control".to_string(),
            }),
        ),
        Code::ManualStdioProviderUnavailable => (
            McpPlatformErrorCodeDto::ManualStdioProviderUnavailable,
            "No Core-owned manual stdio source provider is available.",
            false,
            Some(McpPlatformErrorDetails::ManualStdioProviderUnavailable {
                recovery: McpRecoverySuggestion::ContactPolicyAdministrator,
            }),
        ),
        Code::RemoteHttpPolicyUnavailable => (
            McpPlatformErrorCodeDto::RemoteHttpPolicyUnavailable,
            "Remote HTTP is unavailable because no verified connection policy is configured.",
            false,
            Some(McpPlatformErrorDetails::RemoteHttpPolicyUnavailable {
                recovery: McpRecoverySuggestion::ContactPolicyAdministrator,
            }),
        ),
        Code::UnsafeUrl => (
            McpPlatformErrorCodeDto::UnsafeUrl,
            "The remote MCP endpoint was denied by network policy.",
            false,
            Some(McpPlatformErrorDetails::OriginRejected {
                origin_type: "remote_https".to_string(),
            }),
        ),
        Code::DockerUnavailable => (
            McpPlatformErrorCodeDto::DockerUnavailable,
            "The server-owned Docker capability is unavailable.",
            true,
            Some(McpPlatformErrorDetails::ExternalCapabilityUnavailable {
                capability: "docker".to_string(),
            }),
        ),
        Code::DaemonPolicyDenied => (
            McpPlatformErrorCodeDto::DaemonPolicyDenied,
            "The discovered Docker daemon does not satisfy platform policy.",
            false,
            Some(McpPlatformErrorDetails::ExternalPolicyDenied {
                capability: "docker_daemon".to_string(),
            }),
        ),
        Code::ImageDigestMismatch => (
            McpPlatformErrorCodeDto::ImageDigestMismatch,
            "The Docker image did not match the confirmed immutable digest.",
            false,
            Some(McpPlatformErrorDetails::SupplyChainMismatch {
                authority: "image_digest".to_string(),
            }),
        ),
        Code::RegistryAuthRequired => (
            McpPlatformErrorCodeDto::RegistryAuthRequired,
            "Registry authentication is required but no opaque credential provider is available.",
            false,
            Some(McpPlatformErrorDetails::AuthenticationRequired {
                provider: "container_registry".to_string(),
            }),
        ),
        Code::MountPermissionDenied => (
            McpPlatformErrorCodeDto::MountPermissionDenied,
            "The confirmed filesystem permission cannot be used for this container mount.",
            false,
            Some(McpPlatformErrorDetails::PermissionGrantRequired {
                permission: "filesystem".to_string(),
            }),
        ),
        Code::GitUnavailable => (
            McpPlatformErrorCodeDto::GitUnavailable,
            "The server-owned Git capability is unavailable.",
            true,
            Some(McpPlatformErrorDetails::ExternalCapabilityUnavailable {
                capability: "git".to_string(),
            }),
        ),
        Code::GitOriginDenied => (
            McpPlatformErrorCodeDto::GitOriginDenied,
            "The Git origin is not an allowed canonical public HTTPS repository.",
            false,
            Some(McpPlatformErrorDetails::OriginRejected {
                origin_type: "git_https".to_string(),
            }),
        ),
        Code::CommitUnavailable => (
            McpPlatformErrorCodeDto::CommitUnavailable,
            "The exact immutable Git commit is unavailable.",
            false,
            Some(McpPlatformErrorDetails::ImmutableCommitUnavailable {}),
        ),
        Code::UnsafeRepositoryTree => (
            McpPlatformErrorCodeDto::UnsafeRepositoryTree,
            "The repository tree violates the safe materialization policy.",
            false,
            Some(McpPlatformErrorDetails::RepositoryTreeRejected {}),
        ),
        Code::DevelopmentModeRequired => (
            McpPlatformErrorCodeDto::DevelopmentModeRequired,
            "This local development source requires explicit development mode.",
            false,
            Some(McpPlatformErrorDetails::DevelopmentModeRequired {}),
        ),
        Code::PlanStale | Code::PlanConflict => (
            McpPlatformErrorCodeDto::PlanStale,
            "The reviewed plan no longer matches the confirmation request.",
            false,
            Some(McpPlatformErrorDetails::PlanMismatch {}),
        ),
        Code::PlanExpired => (
            McpPlatformErrorCodeDto::PlanExpired,
            "The reviewed plan has expired.",
            false,
            Some(McpPlatformErrorDetails::PlanExpired {}),
        ),
        Code::IdempotencyConflict => (
            McpPlatformErrorCodeDto::IdempotencyConflict,
            "The idempotency key already refers to different content.",
            false,
            Some(McpPlatformErrorDetails::IdempotencyConflict {}),
        ),
        Code::CandidateStateConflict | Code::ManifestIdentityConflict => (
            McpPlatformErrorCodeDto::InvalidTransition,
            "The intake candidate state conflicts with the recorded manifest identity.",
            false,
            Some(McpPlatformErrorDetails::TransitionRejected {}),
        ),
        Code::RevisionConflict => (
            McpPlatformErrorCodeDto::RevisionConflict,
            "The record revision changed; reload before retrying.",
            true,
            Some(McpPlatformErrorDetails::RevisionConflict {}),
        ),
        Code::InvalidTransition => (
            McpPlatformErrorCodeDto::InvalidTransition,
            "The task state does not permit this transition.",
            false,
            Some(McpPlatformErrorDetails::TransitionRejected {}),
        ),
        Code::RepositoryUnavailable | Code::SchemaTooNew => (
            McpPlatformErrorCodeDto::RepositoryUnavailable,
            "The MCP platform repository is temporarily unavailable.",
            true,
            Some(McpPlatformErrorDetails::RepositoryTemporarilyUnavailable {}),
        ),
        Code::ProjectionConflict => (
            McpPlatformErrorCodeDto::ProjectionConflict,
            "The managed MCP projection conflicts with an existing extension.",
            false,
            Some(McpPlatformErrorDetails::ProjectionConflict {}),
        ),
        Code::CredentialMissing => (
            McpPlatformErrorCodeDto::CredentialMissing,
            "A required credential handle is missing.",
            false,
            Some(McpPlatformErrorDetails::CredentialMissing {}),
        ),
        Code::HealthFailed => (
            McpPlatformErrorCodeDto::HealthFailed,
            "The MCP health check failed.",
            true,
            Some(McpPlatformErrorDetails::HealthGateFailed {}),
        ),
        Code::TaskNotCancellable => (
            McpPlatformErrorCodeDto::TaskNotCancellable,
            "Task cancellation is deferred to a safe boundary.",
            false,
            Some(McpPlatformErrorDetails::CancellationDeferred {}),
        ),
        Code::RollbackIncomplete => (
            McpPlatformErrorCodeDto::RollbackIncomplete,
            "The lifecycle mutation requires durable recovery.",
            false,
            Some(McpPlatformErrorDetails::RecoveryRequired {}),
        ),
        Code::ProjectionWitnessExpired => (
            McpPlatformErrorCodeDto::RollbackIncomplete,
            "The stored projection recovery witness no longer matches current authority.",
            false,
            Some(McpPlatformErrorDetails::WitnessExpired {}),
        ),
        Code::ProjectionWitnessConsumed => (
            McpPlatformErrorCodeDto::RollbackIncomplete,
            "The stored projection recovery witness was already consumed.",
            false,
            Some(McpPlatformErrorDetails::WitnessConsumed {}),
        ),
        Code::AdapterIncompatible => (
            McpPlatformErrorCodeDto::AdapterIncompatible,
            "The journal adapter version is incompatible with this worker.",
            false,
            Some(McpPlatformErrorDetails::AdapterVersionIncompatible {}),
        ),
        Code::InvalidRequest
        | Code::InvalidJson
        | Code::InvalidManifest
        | Code::UnsupportedSchema
        | Code::VersionNotExact
        | Code::InvalidDigest
        | Code::ImmutableReferenceRequired
        | Code::DuplicateSelector
        | Code::TransportMismatch
        | Code::PathTraversal
        | Code::UnknownTemplateVariable
        | Code::UnknownAdapter
        | Code::ManifestConflict
        | Code::SerializationFailed => (
            McpPlatformErrorCodeDto::InvalidRequest,
            "The MCP platform request is invalid.",
            false,
            None,
        ),
    };
    McpPlatformErrorEnvelope {
        code,
        message: message.to_string(),
        retryable,
        correlation_id: context.correlation_id().to_string(),
        details,
    }
}

fn source_provenance_to_wire(
    metadata: &crate::mcp_platform::manifest::ManifestSourceMetadata,
) -> McpSourceProvenance {
    use crate::mcp_platform::manifest::SourceRef;
    let source_ref = match &metadata.source_ref {
        SourceRef::LocalPersistence => McpSourceRef::LocalPersistence,
        SourceRef::VerifiedSourceCatalog { source_id } => McpSourceRef::VerifiedSourceCatalog {
            source_id: source_id.clone(),
        },
        SourceRef::HttpsManifestUrl { manifest_url } => McpSourceRef::HttpsManifestUrl {
            manifest_url: manifest_url.clone(),
        },
        SourceRef::EnterpriseDirectory {
            directory_id,
            entry_id,
        } => McpSourceRef::EnterpriseDirectory {
            directory_id: directory_id.clone(),
            entry_id: entry_id.clone(),
        },
    };
    let import_kind = match metadata.import_kind {
        crate::mcp_platform::SourceImportKind::LocalPersistence => {
            McpSourceImportKind::LocalPersistence
        }
        crate::mcp_platform::SourceImportKind::VerifiedSourceCatalog => {
            McpSourceImportKind::VerifiedSourceCatalog
        }
        crate::mcp_platform::SourceImportKind::HttpsManifestUrl => {
            McpSourceImportKind::HttpsManifestUrl
        }
        crate::mcp_platform::SourceImportKind::EnterpriseDirectory => {
            McpSourceImportKind::EnterpriseDirectory
        }
    };
    McpSourceProvenance {
        source_ref,
        import_kind,
        release_id: metadata.release_id.clone(),
        verified_source_document: metadata
            .origin_provenance
            .verified_source_document
            .as_ref()
            .map(|p| McpVerifiedSourceDocumentProvenance {
                source_id: p.source_id.clone(),
                document_digest: p.document_digest.clone(),
                canonical_digest: p.canonical_digest.clone(),
                signed_digest: p.signed_digest.clone(),
                binding_digest: p.binding_digest.clone(),
                signature_kids: p.signature_kids.clone(),
            }),
    }
}

fn catalog_page_to_wire(page: crate::mcp_platform::service::CatalogPage) -> McpCatalogPage {
    McpCatalogPage {
        items: page
            .items
            .into_iter()
            .map(catalog_summary_to_wire)
            .collect(),
        next_cursor: page.next_cursor,
        cache: McpCatalogCacheMetadata {
            offline: page.offline,
            local_persistence_only: page.local_persistence_only,
            newest_verified_at_ms: page.newest_verified_at_ms,
            freshness: if page.newest_verified_at_ms.is_some() {
                McpCacheFreshness::OfflineVerified
            } else {
                McpCacheFreshness::Empty
            },
            refresh_state: McpRefreshState::LocalOnly,
            recovery: if page.newest_verified_at_ms.is_some() {
                McpRecoverySuggestion::None
            } else {
                McpRecoverySuggestion::RestoreVerifiedCache
            },
        },
    }
}

fn catalog_summary_to_wire(
    summary: crate::mcp_platform::service::CatalogSummary,
) -> McpCatalogSummary {
    McpCatalogSummary {
        source_id: summary.source_id,
        mcp_id: summary.mcp_id,
        version: summary.version,
        manifest_digest: summary.manifest_digest,
        name: summary.name,
        description: summary.description,
        publisher_id: summary.publisher_id,
        publisher_name: summary.publisher_name,
        trust_tier: trust_to_wire(summary.trust_tier),
        proof: proof_to_wire(summary.proof),
        compatibility: compatibility_to_wire(summary.compatibility),
        source_provenance: source_provenance_to_wire(&summary.source_metadata),
        distribution: distribution_kind(&summary.distribution_adapter),
        verified_at_ms: summary.verified_at_ms,
        eligibility: eligibility_to_wire(summary.eligibility),
    }
}

fn governed_import_to_wire(
    value: crate::mcp_platform::service::GovernedImportResult,
) -> McpGovernedImportResult {
    McpGovernedImportResult {
        source: McpGovernedCatalogSourceSummary {
            source_id: value.source.source_id,
            import_kind: governed_import_kind_to_wire(value.source.import_kind),
            display_name: value.source.display_name,
        },
        document: McpGovernedCatalogDocumentSummary {
            source_id: value.document.source_id,
            document_id: value.document.document_id,
            document_digest: value.document.document_digest,
            document_kind: governed_document_kind_to_wire(value.document.document_kind),
        },
        entries: value
            .entries
            .into_iter()
            .map(catalog_summary_to_wire)
            .collect(),
    }
}

fn governed_source_refresh_to_wire(
    value: crate::mcp_platform::service::GovernedSourceRefreshResult,
) -> McpSourceRefreshResult {
    McpSourceRefreshResult {
        source_id: value.source_id,
        display_name: value.display_name,
        document_digest: value.document_digest,
        manifest_count: value.manifest_count,
        refreshed_at_ms: value.refreshed_at_ms,
    }
}

fn source_provision_prepare_to_wire(
    value: crate::mcp_platform::service::SourceProvisionPrepareResult,
) -> McpSourceProvisionPrepareResult {
    McpSourceProvisionPrepareResult {
        provision_id: value.provision_id,
        confirmation_token: value.confirmation_token,
        expires_at_ms: value.expires_at_ms,
        preview: source_provision_preview_to_wire(value.preview),
    }
}

fn source_provision_confirm_to_wire(
    value: crate::mcp_platform::service::SourceProvisionConfirmResult,
) -> McpSourceProvisionConfirmResult {
    McpSourceProvisionConfirmResult {
        provision_id: value.provision_id,
        confirmed_at_ms: value.confirmed_at_ms,
        preview: source_provision_preview_to_wire(value.preview),
    }
}

fn source_provision_preview_to_wire(
    value: crate::mcp_platform::service::SourceProvisionPreview,
) -> McpSourceProvisionPreview {
    McpSourceProvisionPreview {
        source_id: value.source_id,
        display_name: value.display_name,
        root_digest: value.root_digest,
        document_digest: value.document_digest,
        endpoint_host: value.endpoint_host,
        refresh_transport: match value.refresh_transport {
            crate::mcp_platform::service::SourceProvisionRefreshTransport::VerifiedSourceBundleV1 => {
                McpSourceProvisionRefreshTransport::VerifiedSourceBundleV1
            }
        },
        manifest_count: value.manifest_count,
        warnings: value.warnings,
        trust_basis: match value.trust_basis {
            crate::mcp_platform::service::SourceProvisionTrustBasis::UserPin => {
                McpSourceProvisionTrustBasis::UserPin
            }
        },
    }
}

fn governed_import_kind_to_wire(
    value: crate::mcp_platform::SourceImportKind,
) -> McpGovernedImportKind {
    match value {
        crate::mcp_platform::SourceImportKind::LocalPersistence => {
            McpGovernedImportKind::LocalPersistence
        }
        crate::mcp_platform::SourceImportKind::VerifiedSourceCatalog => {
            McpGovernedImportKind::VerifiedSourceCatalog
        }
        crate::mcp_platform::SourceImportKind::HttpsManifestUrl => {
            McpGovernedImportKind::HttpsManifestUrl
        }
        crate::mcp_platform::SourceImportKind::EnterpriseDirectory => {
            McpGovernedImportKind::EnterpriseDirectory
        }
    }
}

fn governed_document_kind_to_wire(
    value: crate::mcp_platform::repository::GovernedCatalogDocumentKind,
) -> McpGovernedCatalogDocumentKind {
    match value {
        crate::mcp_platform::repository::GovernedCatalogDocumentKind::Manifest => {
            McpGovernedCatalogDocumentKind::Manifest
        }
        crate::mcp_platform::repository::GovernedCatalogDocumentKind::Directory => {
            McpGovernedCatalogDocumentKind::Directory
        }
    }
}

enum LoadedGovernedImportInput {
    Manifest(GovernedManifestImportInput),
    Directory(GovernedDirectoryImportInput),
}

#[derive(Clone)]
struct LoadedDirectoryManifestCandidate {
    document_bytes: Vec<u8>,
    manifest_digest: String,
    mcp_id: String,
    version: String,
}

struct LoadedLocalDirectoryScan {
    signed_documents: Vec<(
        crate::verified_source_catalog::VerifiedSourceDocument,
        Vec<u8>,
    )>,
    source_provisioning_descriptors: Vec<Vec<u8>>,
    manifest_candidates: Vec<LoadedDirectoryManifestCandidate>,
}

fn load_governed_import_input(
    source: McpGovernedImportSource,
) -> crate::mcp_platform::McpPlatformResult<LoadedGovernedImportInput> {
    match source {
        McpGovernedImportSource::LocalManifest { file_path } => Ok(
            LoadedGovernedImportInput::Manifest(load_local_manifest_import(&file_path)?),
        ),
        McpGovernedImportSource::LocalDirectory { directory_path } => Ok(
            LoadedGovernedImportInput::Directory(load_local_directory_import(&directory_path)?),
        ),
    }
}

fn load_local_manifest_import(
    raw_path: &str,
) -> crate::mcp_platform::McpPlatformResult<GovernedManifestImportInput> {
    let path =
        canonicalize_local_import_path(raw_path, "Choose a local manifest file before importing.")?;
    let metadata = read_local_metadata(&path)?;
    if !metadata.is_file() {
        return Err(governed_import_invalid_request(
            "Choose a local manifest file before importing.",
        ));
    }
    let document_bytes = read_local_file_bytes(&path, metadata.len())?;
    Ok(GovernedManifestImportInput {
        document_bytes,
        source_metadata: ManifestSourceMetadata::local_persistence(),
    })
}

fn load_local_directory_import(
    raw_path: &str,
) -> crate::mcp_platform::McpPlatformResult<GovernedDirectoryImportInput> {
    let LoadedLocalDirectoryScan {
        signed_documents,
        manifest_candidates,
        ..
    } = scan_local_source_directory(raw_path)?;

    let [(verified_document, directory_document_bytes)] = signed_documents.as_slice() else {
        return Err(governed_import_invalid_request(
            "The selected source directory must contain exactly one signed source catalog document.",
        ));
    };
    let source_metadata = ManifestSourceMetadata {
        source_ref: SourceRef::VerifiedSourceCatalog {
            source_id: verified_document.envelope.payload.source_id.clone(),
        },
        import_kind: SourceImportKind::VerifiedSourceCatalog,
        release_id: None,
        origin_provenance: OriginProvenance {
            verified_source_document: Some(VerifiedSourceDocumentRef {
                source_id: verified_document.envelope.payload.source_id.clone(),
                document_digest: verified_document.digests.document_digest.clone(),
                canonical_digest: verified_document.digests.canonical_digest.clone(),
                signed_digest: verified_document.digests.signed_digest.clone(),
                binding_digest: verified_document.digests.binding_digest.clone(),
                signature_kids: verified_document
                    .envelope
                    .signatures
                    .iter()
                    .map(|signature| signature.kid.clone())
                    .collect(),
            }),
        },
        update_channel: UpdateChannel::Default,
    };

    let manifests = select_directory_manifests(verified_document, &manifest_candidates)?;

    Ok(GovernedDirectoryImportInput {
        directory_document_bytes: directory_document_bytes.clone(),
        source_metadata,
        manifests,
    })
}

fn load_local_source_provisioning_snapshot(
    raw_path: &str,
) -> crate::mcp_platform::McpPlatformResult<
    crate::mcp_platform::source_provisioning::FrozenSourceProvisioningSnapshot,
> {
    let LoadedLocalDirectoryScan {
        signed_documents,
        source_provisioning_descriptors,
        manifest_candidates,
    } = scan_local_source_directory(raw_path)?;
    let [(verified_document, directory_document_bytes)] = signed_documents.as_slice() else {
        return Err(governed_import_invalid_request(
            "The selected source directory must contain exactly one signed source catalog document.",
        ));
    };
    let [descriptor_bytes] = source_provisioning_descriptors.as_slice() else {
        return Err(governed_import_invalid_request(
            "The selected source directory must contain exactly one signed source provisioning descriptor.",
        ));
    };
    let manifests = select_directory_manifests(verified_document, &manifest_candidates)?;
    Ok(
        crate::mcp_platform::source_provisioning::FrozenSourceProvisioningSnapshot {
            descriptor_bytes: descriptor_bytes.clone(),
            source_document_bytes: directory_document_bytes.clone(),
            manifests: manifests
                .into_iter()
                .map(|manifest| {
                    crate::mcp_platform::source_provisioning::FrozenSourceProvisioningManifest {
                        release_id: manifest.release_id,
                        document_bytes: manifest.document_bytes,
                    }
                })
                .collect(),
        },
    )
}

fn scan_local_source_directory(
    raw_path: &str,
) -> crate::mcp_platform::McpPlatformResult<LoadedLocalDirectoryScan> {
    let root = canonicalize_local_import_path(
        raw_path,
        "Choose a local source directory before importing.",
    )?;
    let metadata = read_local_metadata(&root)?;
    if !metadata.is_dir() {
        return Err(governed_import_invalid_request(
            "Choose a local source directory before importing.",
        ));
    }

    let mut signed_documents = Vec::new();
    let mut source_provisioning_descriptors = Vec::new();
    let mut manifest_candidates = Vec::new();
    let mut visited_dirs = BTreeSet::new();
    let mut pending_dirs = vec![root.clone()];
    let mut scanned_files = 0usize;
    let mut total_bytes = 0u64;

    while let Some(directory) = pending_dirs.pop() {
        if !visited_dirs.insert(directory.clone()) {
            continue;
        }
        for entry in fs::read_dir(&directory).map_err(|_| governed_import_unreadable())? {
            let entry = entry.map_err(|_| governed_import_unreadable())?;
            let file_type = entry
                .file_type()
                .map_err(|_| governed_import_unreadable())?;
            if file_type.is_symlink() {
                return Err(governed_import_invalid_request(
                    "The selected source directory contains unsupported links. Copy the source files into a regular folder and retry.",
                ));
            }
            let canonical_path =
                fs::canonicalize(entry.path()).map_err(|_| governed_import_unreadable())?;
            if !canonical_path.starts_with(&root) {
                return Err(governed_import_invalid_request(
                    "The selected source directory contains unsupported links. Copy the source files into a regular folder and retry.",
                ));
            }
            if file_type.is_dir() {
                pending_dirs.push(canonical_path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            scanned_files += 1;
            if scanned_files > MAX_GOVERNED_IMPORT_DIRECTORY_FILES {
                return Err(governed_import_invalid_request(
                    "The selected source directory is too large to import from this page.",
                ));
            }
            let file_metadata = read_local_metadata(&canonical_path)?;
            total_bytes = total_bytes.saturating_add(file_metadata.len());
            if total_bytes > MAX_GOVERNED_IMPORT_DIRECTORY_BYTES {
                return Err(governed_import_invalid_request(
                    "The selected source directory is too large to import from this page.",
                ));
            }
            let document_bytes = read_local_file_bytes(&canonical_path, file_metadata.len())?;
            if let Ok(document) =
                crate::verified_source_catalog::parse_signed_envelope(&document_bytes)
            {
                signed_documents.push((document, document_bytes));
                continue;
            }
            if crate::mcp_platform::source_provisioning::parse_source_provisioning_descriptor_unverified(
                &document_bytes,
            )
            .is_ok()
            {
                source_provisioning_descriptors.push(document_bytes);
                continue;
            }
            if let Ok(verified) = crate::mcp_platform::parse_manifest(&document_bytes) {
                manifest_candidates.push(LoadedDirectoryManifestCandidate {
                    manifest_digest: verified.digest().to_string(),
                    mcp_id: verified.manifest().id.clone(),
                    version: verified.manifest().version.as_str().to_string(),
                    document_bytes,
                });
            }
        }
    }
    Ok(LoadedLocalDirectoryScan {
        signed_documents,
        source_provisioning_descriptors,
        manifest_candidates,
    })
}

fn select_directory_manifests(
    verified_document: &crate::verified_source_catalog::VerifiedSourceDocument,
    manifest_candidates: &[LoadedDirectoryManifestCandidate],
) -> crate::mcp_platform::McpPlatformResult<Vec<GovernedDirectoryManifestImportInput>> {
    let revoked_release_ids = verified_document
        .envelope
        .payload
        .snapshot
        .revocations
        .iter()
        .map(|revocation| revocation.release_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut manifests = Vec::new();
    for release in verified_document
        .envelope
        .payload
        .snapshot
        .releases
        .iter()
        .filter(|release| !revoked_release_ids.contains(release.release_id.as_str()))
    {
        let matching = manifest_candidates
            .iter()
            .filter(|candidate| {
                candidate.mcp_id == release.mcp_id && candidate.version == release.version
            })
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return Err(governed_import_invalid_request(
                "The selected source directory is missing one or more manifests referenced by the signed catalog.",
            ));
        }
        let distinct_digests = matching
            .iter()
            .map(|candidate| candidate.manifest_digest.as_str())
            .collect::<BTreeSet<_>>();
        if distinct_digests.len() != 1 {
            return Err(governed_import_invalid_request(
                "The selected source directory has multiple manifest files for the same catalog release.",
            ));
        }
        manifests.push(GovernedDirectoryManifestImportInput {
            release_id: release.release_id.clone(),
            document_bytes: matching[0].document_bytes.clone(),
        });
    }

    Ok(manifests)
}

fn canonicalize_local_import_path(
    raw_path: &str,
    empty_message: &'static str,
) -> crate::mcp_platform::McpPlatformResult<PathBuf> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(governed_import_invalid_request(empty_message));
    }
    if looks_like_non_local_descriptor(trimmed) {
        return Err(governed_import_invalid_request(
            LOCAL_ONLY_GOVERNED_IMPORT_MESSAGE,
        ));
    }
    fs::canonicalize(trimmed).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            governed_import_not_found()
        } else {
            governed_import_unreadable()
        }
    })
}

fn read_local_metadata(path: &Path) -> crate::mcp_platform::McpPlatformResult<fs::Metadata> {
    fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            governed_import_not_found()
        } else {
            governed_import_unreadable()
        }
    })
}

fn read_local_file_bytes(
    path: &Path,
    size_bytes: u64,
) -> crate::mcp_platform::McpPlatformResult<Vec<u8>> {
    if size_bytes > MAX_GOVERNED_IMPORT_FILE_BYTES {
        return Err(governed_import_invalid_request(
            "The selected file is too large to import from this page.",
        ));
    }
    fs::read(path).map_err(|_| governed_import_unreadable())
}

fn looks_like_non_local_descriptor(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("file:")
        || looks_like_non_local_windows_path(value)
}

#[cfg(windows)]
fn looks_like_non_local_windows_path(value: &str) -> bool {
    let normalized = value.replace('/', "\\");
    let lower = normalized.to_ascii_lowercase();
    if lower.starts_with(r"\\?\") || lower.starts_with(r"\\.\") || lower.starts_with(r"\??\") {
        return true;
    }

    match Path::new(&normalized).components().next() {
        Some(Component::Prefix(prefix)) => matches!(
            prefix.kind(),
            Prefix::UNC(..)
                | Prefix::Verbatim(..)
                | Prefix::VerbatimUNC(..)
                | Prefix::VerbatimDisk(..)
                | Prefix::DeviceNS(..)
        ),
        _ => normalized.starts_with(r"\\"),
    }
}

#[cfg(not(windows))]
fn looks_like_non_local_windows_path(_value: &str) -> bool {
    false
}

fn connection_test_stage(
    phase: McpConnectionTestPhase,
    status: McpConnectionTestStatus,
    code: McpConnectionTestCode,
    diagnostic: &str,
    repair_suggestion: &str,
    managed_mcp_id: Option<String>,
    duration_ms: i64,
) -> McpConnectionTestStage {
    McpConnectionTestStage {
        phase,
        status,
        code,
        diagnostic: diagnostic.to_string(),
        repair_suggestion: repair_suggestion.to_string(),
        duration_ms,
        managed_mcp_id,
    }
}

fn connection_test_permits() -> Arc<Semaphore> {
    CONNECTION_TEST_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone()
}

fn remaining_connection_test_time(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

fn spawn_connection_test_reaper(
    manager: Arc<ExtensionManager>,
    extension_name: String,
    pending_setup: Option<tokio::task::JoinHandle<crate::agents::extension::ExtensionResult<()>>>,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _permit = permit;
        if let Some(mut task) = pending_setup {
            if tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        for _ in 0..3 {
            if matches!(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    manager.remove_extension(&extension_name),
                )
                .await,
                Ok(Ok(()))
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        manager.abort_extension(&extension_name).await;
    })
}

fn take_connection_test_permit(
    permit: &mut Option<tokio::sync::OwnedSemaphorePermit>,
) -> tokio::sync::OwnedSemaphorePermit {
    permit
        .take()
        .expect("connection-test reaper must inherit the active permit")
}

fn append_bounded_tools(destination: &mut Vec<Tool>, source: Vec<Tool>) -> bool {
    let mut total_bytes = destination
        .iter()
        .try_fold(0usize, |total, tool| {
            serde_json::to_vec(tool)
                .ok()
                .and_then(|encoded| total.checked_add(encoded.len()))
        })
        .unwrap_or(usize::MAX);
    if total_bytes > CONNECTION_TEST_MAX_TOTAL_TOOL_BYTES {
        return false;
    }
    for tool in &source {
        if destination.len().saturating_add(source.len()) > CONNECTION_TEST_MAX_TOOLS {
            return false;
        }
        let Ok(encoded) = serde_json::to_vec(tool) else {
            return false;
        };
        if encoded.len() > CONNECTION_TEST_MAX_TOOL_BYTES {
            return false;
        }
        let Some(updated_total) = total_bytes.checked_add(encoded.len()) else {
            return false;
        };
        if updated_total > CONNECTION_TEST_MAX_TOTAL_TOOL_BYTES {
            return false;
        }
        total_bytes = updated_total;
    }
    destination.extend(source);
    true
}

fn connection_test_forbids_unbounded_transport(config: &crate::agents::ExtensionConfig) -> bool {
    !matches!(
        config,
        crate::agents::ExtensionConfig::ManagedStreamableHttp { .. }
    )
}

fn connection_test_may_send_model(mcp_ok: bool, tools: &[Tool]) -> bool {
    mcp_ok && !tools.is_empty()
}

fn connection_test_response(
    stages: Vec<McpConnectionTestStage>,
    passed: bool,
) -> McpProfileConnectionTestResponse {
    McpProfileConnectionTestResponse {
        outcome: McpPlatformOutcome::success(McpProfileConnectionTestResult { passed, stages }),
    }
}

fn elapsed_test_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

fn governed_import_invalid_request(message: &'static str) -> crate::mcp_platform::McpPlatformError {
    crate::mcp_platform::McpPlatformError::new(
        crate::mcp_platform::McpPlatformErrorCode::InvalidRequest,
        message,
    )
}

fn governed_import_not_found() -> crate::mcp_platform::McpPlatformError {
    crate::mcp_platform::McpPlatformError::new(
        crate::mcp_platform::McpPlatformErrorCode::NotFound,
        "The selected local item is no longer available. Choose it again and retry.",
    )
}

fn governed_import_unreadable() -> crate::mcp_platform::McpPlatformError {
    crate::mcp_platform::McpPlatformError::new(
        crate::mcp_platform::McpPlatformErrorCode::InvalidRequest,
        "The selected local item could not be read. Check access and retry.",
    )
}

fn catalog_detail_to_wire(detail: crate::mcp_platform::service::CatalogDetail) -> McpCatalogDetail {
    let manifest = detail.manifest;
    McpCatalogDetail {
        source_id: detail.source_id,
        mcp_id: manifest.id.clone(),
        version: manifest.version.as_str().to_string(),
        manifest_digest: detail.manifest_digest,
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        publisher: publisher_to_wire(&manifest),
        proof: proof_to_wire(detail.proof),
        trust_tier: trust_to_wire(detail.trust_tier),
        source_provenance: source_provenance_to_wire(&detail.source_metadata),
        distribution: distribution_to_wire(&manifest.distribution),
        transport: transport_to_wire(&manifest.transport),
        auth: auth_to_wire(&manifest.auth),
        health: health_to_wire(&manifest.health_check),
        capabilities: manifest
            .capabilities
            .iter()
            .copied()
            .map(capability_to_wire)
            .collect(),
        permissions: manifest
            .permissions
            .iter()
            .map(permission_to_wire)
            .collect(),
        compatibility: compatibility_to_wire(detail.compatibility),
        verified_at_ms: detail.verified_at_ms,
        eligibility: eligibility_to_wire(detail.eligibility),
    }
}

fn plan_review_to_wire(review: crate::mcp_platform::service::PlanReview) -> McpPlanReview {
    let immutable_evidence = immutable_evidence_to_wire(review.immutable_evidence);
    let policy_recovery = recovery_to_wire(review.recovery);
    let reversibility = reversibility_to_wire(review.reversibility);
    let manifest = review.manifest;
    McpPlanReview {
        plan_id: review.plan_id,
        plan_digest: review.plan_digest,
        expires_at_ms: review.expires_at_ms,
        source_id: review.source_id,
        catalog_target: review.catalog_target.map(|catalog| McpCatalogPlanTarget {
            source_id: catalog.source_id,
            mcp_id: catalog.mcp_id,
            version: catalog.version,
            manifest_digest: catalog.manifest_digest,
        }),
        proof: proof_to_wire(review.proof),
        trust_tier: trust_to_wire(review.trust_tier),
        source_provenance: source_provenance_to_wire(&review.source_metadata),
        publisher: publisher_to_wire(&manifest),
        mcp_id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.as_str().to_string(),
        selected_manifest_digest: review.plan.manifest_digest().to_string(),
        immutable_evidence,
        permissions: manifest
            .permissions
            .iter()
            .map(permission_to_wire)
            .collect(),
        network_origins: review.plan.effects().network_origins.clone(),
        file_effects: McpFileEffects {
            writes_files: review.plan.effects().writes_files,
            removes_files: review.plan.effects().removes_files,
            owned_items: manifest.owned_files.len() as u32,
        },
        host_effects: McpHostEffects {
            registration_ids: manifest
                .host_integrations
                .iter()
                .map(|integration| integration.id.clone())
                .collect(),
        },
        process_effects: McpProcessEffects {
            process_required_for_connection: review.plan.effects().requires_process_spawn,
            starts_during_confirmation: false,
        },
        reversibility,
        policy: McpPolicyReview {
            outcome: policy_outcome_to_wire(review.policy.outcome),
            reasons: review
                .policy
                .reasons
                .into_iter()
                .map(|reason| McpPolicyReason {
                    code: reason.code.as_str().to_string(),
                    message: reason.message,
                })
                .collect(),
        },
        warnings: review
            .plan
            .warnings()
            .iter()
            .map(|warning| match warning {
                crate::mcp_platform::PlanWarning::DefaultDisabled => {
                    McpPlanWarning::DefaultDisabled
                }
                crate::mcp_platform::PlanWarning::RegistrationOnly => {
                    McpPlanWarning::RegistrationOnly
                }
                crate::mcp_platform::PlanWarning::ManagedArtifactDownload => {
                    McpPlanWarning::ManagedArtifactDownload
                }
                crate::mcp_platform::PlanWarning::ExistingVersionRetainedUntilCommit => {
                    McpPlanWarning::ExistingVersionRetainedUntilCommit
                }
                crate::mcp_platform::PlanWarning::RemovesOwnedFilesOnly => {
                    McpPlanWarning::RemovesOwnedFilesOnly
                }
                crate::mcp_platform::PlanWarning::ImmutableContainerImage => {
                    McpPlanWarning::ImmutableContainerImage
                }
                crate::mcp_platform::PlanWarning::WritableContainerMount => {
                    McpPlanWarning::WritableContainerMount
                }
                crate::mcp_platform::PlanWarning::DevelopmentSourcePinnedCommitNoBuild => {
                    McpPlanWarning::DevelopmentSourcePinnedCommitNoBuild
                }
            })
            .collect(),
        required_confirmations: review
            .plan
            .required_confirmations()
            .iter()
            .map(|confirmation| match confirmation {
                crate::mcp_platform::RequiredConfirmation::Policy { reason_code } => {
                    McpRequiredConfirmation::Policy {
                        reason_code: reason_code.as_str().to_string(),
                    }
                }
                crate::mcp_platform::RequiredConfirmation::Permission { permission_id } => {
                    McpRequiredConfirmation::Permission {
                        permission_id: permission_id.clone(),
                    }
                }
            })
            .collect(),
        default_disabled: !review.plan.default_enabled(),
        recovery: policy_recovery,
    }
}

fn immutable_evidence_to_wire(
    evidence: crate::mcp_platform::service::ImmutableEvidence,
) -> McpImmutableEvidence {
    match evidence {
        crate::mcp_platform::service::ImmutableEvidence::Artifact { sha256, size_bytes } => {
            McpImmutableEvidence::Artifact { sha256, size_bytes }
        }
        crate::mcp_platform::service::ImmutableEvidence::Docker {
            image,
            image_digest,
        } => McpImmutableEvidence::Docker {
            image,
            image_digest,
        },
        crate::mcp_platform::service::ImmutableEvidence::GitDev {
            repository_origin,
            commit,
            tree,
            materialized_digest,
        } => McpImmutableEvidence::GitDev {
            repository_origin,
            commit,
            tree: evidence_value_to_wire(tree),
            materialized_digest: evidence_value_to_wire(materialized_digest),
        },
        crate::mcp_platform::service::ImmutableEvidence::Unavailable { reason } => {
            McpImmutableEvidence::Unavailable {
                reason: evidence_unavailable_to_wire(reason),
            }
        }
    }
}

fn evidence_value_to_wire(value: crate::mcp_platform::service::EvidenceValue) -> McpEvidenceValue {
    match value {
        crate::mcp_platform::service::EvidenceValue::Verified(value) => {
            McpEvidenceValue::Verified { value }
        }
        crate::mcp_platform::service::EvidenceValue::Unavailable(reason) => {
            McpEvidenceValue::Unavailable {
                reason: evidence_unavailable_to_wire(reason),
            }
        }
    }
}

fn evidence_unavailable_to_wire(
    reason: crate::mcp_platform::service::EvidenceUnavailableReason,
) -> McpEvidenceUnavailableReason {
    match reason {
        crate::mcp_platform::service::EvidenceUnavailableReason::NotApplicable => {
            McpEvidenceUnavailableReason::NotApplicable
        }
        crate::mcp_platform::service::EvidenceUnavailableReason::AvailableAfterMaterialization => {
            McpEvidenceUnavailableReason::AvailableAfterMaterialization
        }
        crate::mcp_platform::service::EvidenceUnavailableReason::NoArtifactForRegistration => {
            McpEvidenceUnavailableReason::NoArtifactForRegistration
        }
    }
}

fn reversibility_to_wire(
    value: crate::mcp_platform::service::PlanReversibility,
) -> McpReversibility {
    McpReversibility {
        reversible: value.reversible,
        strategy: match value.strategy {
            crate::mcp_platform::service::RollbackStrategy::RemoveConnectionRegistration => {
                McpRollbackStrategy::RemoveConnectionRegistration
            }
            crate::mcp_platform::service::RollbackStrategy::StagedActivationRestoresPreviousVersion => {
                McpRollbackStrategy::StagedActivationRestoresPreviousVersion
            }
            crate::mcp_platform::service::RollbackStrategy::RepairRestoresVerifiedOwnedContent => {
                McpRollbackStrategy::RepairRestoresVerifiedOwnedContent
            }
            crate::mcp_platform::service::RollbackStrategy::UninstallRemovesOwnedFiles {
                preserve_user_data,
            } => McpRollbackStrategy::UninstallRemovesOwnedFiles { preserve_user_data },
            crate::mcp_platform::service::RollbackStrategy::Unavailable => {
                McpRollbackStrategy::Unavailable
            }
        },
    }
}

fn task_to_wire(task: crate::mcp_platform::service::TaskRef) -> McpTaskRef {
    let outcome = task_outcome_to_wire(&task);
    McpTaskRef {
        task_id: task.task_id,
        operation: task_operation_to_wire(task.operation),
        status: task_status_to_wire(task.status),
        progress: task.progress,
        cancellable: task.cancellable,
        revision: task.revision,
        updated_at_ms: task.updated_at_ms,
        outcome,
    }
}

fn task_outcome_to_wire(task: &crate::mcp_platform::service::TaskRef) -> McpTaskOutcomeSummary {
    use crate::mcp_platform::task::{CompensationDescriptor, RedactedErrorCode, RollbackStatus};

    let state = match task.status {
        TaskStatus::Succeeded => McpTaskOutcomeState::Succeeded,
        TaskStatus::Failed => McpTaskOutcomeState::Failed,
        TaskStatus::Cancelled => McpTaskOutcomeState::Cancelled,
        TaskStatus::Interrupted => McpTaskOutcomeState::Interrupted,
        TaskStatus::RecoveryRequired => McpTaskOutcomeState::RecoveryRequired,
        _ => McpTaskOutcomeState::Pending,
    };
    let error = task.redacted_error.as_ref().map(|error| {
        let (code, message, suggestion, retryable) = match error.code() {
            RedactedErrorCode::AdapterFailed => (
                McpTaskErrorCode::AdapterFailed,
                if error.message() == MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE {
                    MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE
                } else {
                    "The MCP adapter could not complete the approved operation."
                },
                McpRecoverySuggestion::Retry,
                true,
            ),
            RedactedErrorCode::VerificationFailed => (
                McpTaskErrorCode::VerificationFailed,
                "Verification of the approved MCP material failed.",
                McpRecoverySuggestion::RecreatePlan,
                false,
            ),
            RedactedErrorCode::ActivationFailed => (
                McpTaskErrorCode::ActivationFailed,
                "The verified MCP could not be activated.",
                McpRecoverySuggestion::Retry,
                true,
            ),
            RedactedErrorCode::RollbackFailed => (
                McpTaskErrorCode::RollbackFailed,
                "Rollback did not remove every recorded effect.",
                McpRecoverySuggestion::ResolveRecovery,
                false,
            ),
            RedactedErrorCode::Cancelled => (
                McpTaskErrorCode::Cancelled,
                "The operation was cancelled at a safe boundary.",
                McpRecoverySuggestion::None,
                false,
            ),
            RedactedErrorCode::Interrupted => (
                McpTaskErrorCode::Interrupted,
                "The operation was interrupted and requires a recorded recovery decision.",
                McpRecoverySuggestion::Retry,
                true,
            ),
            RedactedErrorCode::Unknown => (
                McpTaskErrorCode::Unknown,
                "The operation ended with a redacted internal error.",
                McpRecoverySuggestion::Retry,
                true,
            ),
        };
        McpTaskClosedError {
            code,
            retryable,
            correlation_id: task.task_id.clone(),
            message: message.to_string(),
            suggestion,
        }
    });
    let rollback = match task.rollback_status {
        RollbackStatus::NotRequired => McpTaskRollbackSummary::NotRequired,
        RollbackStatus::Pending => McpTaskRollbackSummary::Pending,
        RollbackStatus::InProgress => McpTaskRollbackSummary::InProgress,
        RollbackStatus::Complete => McpTaskRollbackSummary::Complete,
        RollbackStatus::Incomplete => McpTaskRollbackSummary::Incomplete,
    };
    let finalization = if task.status == TaskStatus::RecoveryRequired
        || task.rollback_status == RollbackStatus::Incomplete
    {
        McpTaskFinalizationState::RecoveryRequired
    } else if matches!(
        task.status,
        TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled
    ) {
        McpTaskFinalizationState::Complete
    } else {
        McpTaskFinalizationState::Pending
    };
    let remaining_effects = task
        .rollback_evidence
        .as_ref()
        .into_iter()
        .flat_map(|evidence| evidence.remaining_compensations.iter())
        .filter_map(|effect| match effect {
            CompensationDescriptor::NoCompensation => None,
            CompensationDescriptor::RemoveConnectionProjection { .. }
            | CompensationDescriptor::RestoreManagedProjection { .. }
            | CompensationDescriptor::RemoveOwnedConnectionProjection { .. }
            | CompensationDescriptor::RemoveOwnedExtensionConfig { .. } => {
                Some(McpRemainingEffect::ConnectionProjection)
            }
            CompensationDescriptor::ManagedUninstallSnapshot { .. }
            | CompensationDescriptor::RestoreQuarantinedVersion { .. }
            | CompensationDescriptor::CancelManagedUninstall { .. }
            | CompensationDescriptor::FinalizedManagedUninstall { .. } => {
                Some(McpRemainingEffect::ManagedUninstall)
            }
            CompensationDescriptor::RestoreConfigFragment { .. } => {
                Some(McpRemainingEffect::ExternalResource)
            }
            _ => Some(McpRemainingEffect::ManagedInstallation),
        })
        .collect();
    let next_action = match task.status {
        TaskStatus::RecoveryRequired => McpTaskNextAction::ResolveRecovery,
        TaskStatus::Interrupted => McpTaskNextAction::Resume,
        TaskStatus::Failed => McpTaskNextAction::Retry,
        TaskStatus::Planned | TaskStatus::AwaitingConfirmation => McpTaskNextAction::RecreatePlan,
        TaskStatus::Queued
        | TaskStatus::Running
        | TaskStatus::Cancelling
        | TaskStatus::Verifying
        | TaskStatus::Activating
        | TaskStatus::RollingBack => McpTaskNextAction::Wait,
        TaskStatus::Succeeded | TaskStatus::Cancelled => McpTaskNextAction::None,
    };
    McpTaskOutcomeSummary {
        state,
        error,
        rollback,
        finalization,
        remaining_effects,
        next_action,
    }
}

fn events_to_wire(page: crate::mcp_platform::service::EventsResumePage) -> McpEventsPage {
    McpEventsPage {
        events: page
            .events
            .into_iter()
            .map(|event| McpEventEnvelope {
                event_id: event.event_id,
                task_id: event.task_id,
                task_local_sequence: event.sequence,
                occurred_at_ms: event.occurred_at_ms,
                actor: event.actor,
                event_type: event_type_to_wire(event.event_type),
                payload: event_payload_to_wire(event.payload),
            })
            .collect(),
        next_event_id: page.next_event_id,
        tasks: page.tasks.into_iter().map(task_to_wire).collect(),
    }
}

fn proof_to_wire(proof: ManifestProof) -> McpSourceProof {
    match proof {
        ManifestProof::LocalBytes => McpSourceProof::LocalBytes,
        ManifestProof::Catalog {
            index_digest,
            declared_manifest_digest,
            signature,
        } => McpSourceProof::Catalog {
            index_digest,
            declared_manifest_digest,
            signature: signature.map(|signature| McpSignatureProof {
                algorithm: signature.algorithm,
                signing_identity: signature.signing_identity,
                present: true,
            }),
        },
    }
}

fn publisher_to_wire(manifest: &Manifest) -> McpPublisher {
    McpPublisher {
        id: manifest.publisher.id.clone(),
        name: manifest.publisher.name.clone(),
        website: manifest.publisher.website.clone(),
        signing_identities: manifest.publisher.signing_identities.clone(),
    }
}

fn distribution_to_wire(distribution: &Distribution) -> McpDistributionContract {
    match distribution {
        Distribution::RemoteHttp => McpDistributionContract::RemoteHttp,
        Distribution::ManualStdio { platforms, .. } => McpDistributionContract::ManualStdio {
            platforms: platforms.iter().copied().map(platform_name).collect(),
        },
        Distribution::Npm {
            package,
            package_version,
            artifacts,
            ..
        } => McpDistributionContract::Npm {
            package: package.clone(),
            package_version: package_version.as_str().to_string(),
            artifacts: artifacts.iter().map(artifact_to_wire).collect(),
        },
        Distribution::PythonWheel {
            package,
            package_version,
            python,
            artifacts,
            ..
        } => McpDistributionContract::PythonWheel {
            package: package.clone(),
            package_version: package_version.as_str().to_string(),
            python: python.clone(),
            artifacts: artifacts.iter().map(artifact_to_wire).collect(),
        },
        Distribution::BinaryArchive {
            archive_format,
            artifacts,
            ..
        } => McpDistributionContract::BinaryArchive {
            archive_format: archive_format_name(*archive_format).to_string(),
            artifacts: artifacts.iter().map(artifact_to_wire).collect(),
        },
        Distribution::Docker { image, digest, .. } => McpDistributionContract::Docker {
            image: image.clone(),
            digest: digest.value.clone(),
        },
        Distribution::GitDev {
            repository,
            commit,
            adapter,
            ..
        } => McpDistributionContract::GitDev {
            repository: repository.clone(),
            commit: commit.clone(),
            adapter: git_adapter_name(*adapter).to_string(),
        },
    }
}

fn artifact_to_wire(artifact: &crate::mcp_platform::manifest::Artifact) -> McpArtifactContract {
    McpArtifactContract {
        platform: platform_name(artifact.platform),
        architecture: architecture_name(artifact.arch),
        origin: artifact.url.clone(),
        digest: artifact.digest.value.clone(),
        size_bytes: artifact.size_bytes,
    }
}

fn transport_to_wire(transport: &Transport) -> McpTransportContract {
    match transport {
        Transport::Stdio {
            startup_timeout_seconds,
        } => McpTransportContract::Stdio {
            startup_timeout_seconds: *startup_timeout_seconds,
        },
        Transport::StreamableHttp {
            url,
            connect_timeout_seconds,
            allowed_redirect_origins,
        } => McpTransportContract::StreamableHttp {
            endpoint: url.clone(),
            connect_timeout_seconds: *connect_timeout_seconds,
            allowed_redirect_origins: allowed_redirect_origins.clone(),
        },
    }
}

fn auth_to_wire(auth: &Auth) -> McpAuthContract {
    match auth {
        Auth::None => McpAuthContract::None,
        Auth::ApiKeyHeader { prefix, .. } => McpAuthContract::ApiKeyHeader {
            credential_required: true,
            prefix_required: prefix.is_some(),
        },
        Auth::Environment { .. } => McpAuthContract::Environment {
            credential_required: true,
        },
        Auth::Oauth2 {
            authorization_url,
            token_url,
            client_registration,
            scopes,
        } => McpAuthContract::Oauth2 {
            authorization_endpoint: authorization_url.clone(),
            token_endpoint: token_url.clone(),
            client_registration: match client_registration {
                OAuthClientRegistration::Dynamic => "dynamic",
                OAuthClientRegistration::UserProvided => "user_provided",
            }
            .to_string(),
            scopes: scopes.clone(),
        },
    }
}

fn health_to_wire(health: &HealthCheck) -> McpHealthContract {
    match health {
        HealthCheck::McpInitialize { timeout_seconds } => McpHealthContract::McpInitialize {
            timeout_seconds: *timeout_seconds,
        },
        HealthCheck::McpListTools { timeout_seconds } => McpHealthContract::McpListTools {
            timeout_seconds: *timeout_seconds,
        },
        HealthCheck::Http {
            path,
            expected_status,
            timeout_seconds,
        } => McpHealthContract::Http {
            relative_endpoint: path.clone(),
            expected_status: *expected_status,
            timeout_seconds: *timeout_seconds,
        },
    }
}

fn permission_to_wire(permission: &crate::mcp_platform::manifest::Permission) -> McpPermission {
    McpPermission {
        id: permission.id.clone(),
        kind: match permission.kind {
            PermissionKind::FilesystemRead => McpPermissionKind::FilesystemRead,
            PermissionKind::FilesystemWrite => McpPermissionKind::FilesystemWrite,
            PermissionKind::Network => McpPermissionKind::Network,
            PermissionKind::Credentials => McpPermissionKind::Credentials,
            PermissionKind::ProcessSpawn => McpPermissionKind::ProcessSpawn,
            PermissionKind::Docker => McpPermissionKind::Docker,
            PermissionKind::HostApplication => McpPermissionKind::HostApplication,
        },
        reason: permission.reason.clone(),
        required: permission.required,
        scope: permission.scope.clone(),
    }
}

fn capability_to_wire(capability: Capability) -> McpCapability {
    match capability {
        Capability::Tools => McpCapability::Tools,
        Capability::Resources => McpCapability::Resources,
        Capability::Prompts => McpCapability::Prompts,
        Capability::Sampling => McpCapability::Sampling,
        Capability::Elicitation => McpCapability::Elicitation,
        Capability::Logging => McpCapability::Logging,
    }
}

fn trust_from_wire(trust: McpTrustTier) -> crate::mcp_platform::TrustTier {
    match trust {
        McpTrustTier::Official => crate::mcp_platform::TrustTier::Official,
        McpTrustTier::Community => crate::mcp_platform::TrustTier::Community,
        McpTrustTier::Local => crate::mcp_platform::TrustTier::Local,
    }
}

fn trust_to_wire(trust: crate::mcp_platform::TrustTier) -> McpTrustTier {
    match trust {
        crate::mcp_platform::TrustTier::Official => McpTrustTier::Official,
        crate::mcp_platform::TrustTier::Community => McpTrustTier::Community,
        crate::mcp_platform::TrustTier::Local => McpTrustTier::Local,
    }
}

fn compatibility_from_wire(
    compatibility: McpCompatibility,
) -> crate::mcp_platform::CatalogCompatibility {
    match compatibility {
        McpCompatibility::Compatible => crate::mcp_platform::CatalogCompatibility::Compatible,
        McpCompatibility::Incompatible => crate::mcp_platform::CatalogCompatibility::Incompatible,
    }
}

fn compatibility_to_wire(
    compatibility: crate::mcp_platform::CatalogCompatibility,
) -> McpCompatibility {
    match compatibility {
        crate::mcp_platform::CatalogCompatibility::Compatible => McpCompatibility::Compatible,
        crate::mcp_platform::CatalogCompatibility::Incompatible => McpCompatibility::Incompatible,
    }
}

fn eligibility_to_wire(value: crate::mcp_platform::service::Eligibility) -> McpEligibility {
    McpEligibility {
        outcome: match value.outcome {
            crate::mcp_platform::service::EligibilityOutcome::Allowed => {
                McpEligibilityOutcome::Allowed
            }
            crate::mcp_platform::service::EligibilityOutcome::Restricted => {
                McpEligibilityOutcome::Restricted
            }
            crate::mcp_platform::service::EligibilityOutcome::Denied => {
                McpEligibilityOutcome::Denied
            }
        },
        reason: match value.reason {
            crate::mcp_platform::service::EligibilityReason::Eligible => {
                McpEligibilityReason::Eligible
            }
            crate::mcp_platform::service::EligibilityReason::ConfirmationRequired => {
                McpEligibilityReason::ConfirmationRequired
            }
            crate::mcp_platform::service::EligibilityReason::PlatformUnsupported => {
                McpEligibilityReason::PlatformUnsupported
            }
            crate::mcp_platform::service::EligibilityReason::RuntimeUnavailable => {
                McpEligibilityReason::RuntimeUnavailable
            }
            crate::mcp_platform::service::EligibilityReason::PolicyDenied => {
                McpEligibilityReason::PolicyDenied
            }
            crate::mcp_platform::service::EligibilityReason::DevelopmentModeRequired => {
                McpEligibilityReason::DevelopmentModeRequired
            }
            crate::mcp_platform::service::EligibilityReason::ExternalCapabilityUnavailable => {
                McpEligibilityReason::ExternalCapabilityUnavailable
            }
        },
        recovery: recovery_to_wire(value.recovery),
    }
}

fn recovery_to_wire(
    value: crate::mcp_platform::service::RecoverySuggestion,
) -> McpRecoverySuggestion {
    match value {
        crate::mcp_platform::service::RecoverySuggestion::None => McpRecoverySuggestion::None,
        crate::mcp_platform::service::RecoverySuggestion::ReviewPermissions => {
            McpRecoverySuggestion::ReviewPermissions
        }
        crate::mcp_platform::service::RecoverySuggestion::ChooseCompatibleRelease => {
            McpRecoverySuggestion::ChooseCompatibleRelease
        }
        crate::mcp_platform::service::RecoverySuggestion::InstallRequiredRuntime => {
            McpRecoverySuggestion::InstallRequiredRuntime
        }
        crate::mcp_platform::service::RecoverySuggestion::EnableDevelopmentMode => {
            McpRecoverySuggestion::EnableDevelopmentMode
        }
        crate::mcp_platform::service::RecoverySuggestion::RestoreVerifiedCache => {
            McpRecoverySuggestion::RestoreVerifiedCache
        }
        crate::mcp_platform::service::RecoverySuggestion::Retry => McpRecoverySuggestion::Retry,
        crate::mcp_platform::service::RecoverySuggestion::RecreatePlan => {
            McpRecoverySuggestion::RecreatePlan
        }
        crate::mcp_platform::service::RecoverySuggestion::ResolveRecovery => {
            McpRecoverySuggestion::ResolveRecovery
        }
        crate::mcp_platform::service::RecoverySuggestion::ContactPolicyAdministrator => {
            McpRecoverySuggestion::ContactPolicyAdministrator
        }
    }
}

fn source_policy_to_wire(
    value: crate::mcp_platform::service::SourcePolicyState,
) -> McpSourcesPolicyState {
    McpSourcesPolicyState {
        sources: value
            .sources
            .into_iter()
            .map(|source| McpSourceState {
                source_id: source.source_id,
                display_name: source.display_name,
                import_kind: governed_import_kind_to_wire(source.import_kind),
                trust_tiers: source.trust_tiers.into_iter().map(trust_to_wire).collect(),
                manifest_count: source.manifest_count,
                last_imported_at_ms: source.last_imported_at_ms,
                cache: McpCatalogCacheMetadata {
                    offline: !source.refreshable,
                    local_persistence_only: matches!(
                        source.import_kind,
                        crate::mcp_platform::manifest::SourceImportKind::LocalPersistence
                    ),
                    newest_verified_at_ms: source.newest_verified_at_ms,
                    freshness: if source.newest_verified_at_ms.is_some() {
                        if source.refreshable {
                            McpCacheFreshness::Fresh
                        } else {
                            McpCacheFreshness::OfflineVerified
                        }
                    } else {
                        McpCacheFreshness::Empty
                    },
                    refresh_state: if source.refreshable {
                        match source.last_refresh_result {
                            Some(crate::mcp_platform::repository::GovernedSourceRefreshResultState::Failed) => {
                                McpRefreshState::Failed
                            }
                            _ => McpRefreshState::Idle,
                        }
                    } else {
                        McpRefreshState::LocalOnly
                    },
                    recovery: if source.refreshable {
                        if matches!(
                            source.last_refresh_result,
                            Some(crate::mcp_platform::repository::GovernedSourceRefreshResultState::Failed)
                        ) {
                            McpRecoverySuggestion::Retry
                        } else {
                            McpRecoverySuggestion::None
                        }
                    } else if source.newest_verified_at_ms.is_some() {
                        McpRecoverySuggestion::None
                    } else {
                        McpRecoverySuggestion::RestoreVerifiedCache
                    },
                },
                compatibility: McpCompatibilitySummary {
                    compatible: source.compatibility.compatible,
                    restricted: source.compatibility.restricted,
                    denied: source.compatibility.denied,
                },
                recovery: recovery_to_wire(source.recovery),
                last_refresh_document_digest: source.last_refresh_document_digest,
                last_refreshed_at_ms: source.last_refreshed_at_ms,
            })
            .collect(),
        policy: McpMachinePolicyState {
            target_platform: value.policy.target_platform,
            target_architecture: value.policy.target_architecture,
            development_mode: value.policy.development_mode,
            docker_allowed: value.policy.docker_allowed,
            recovery: McpRecoverySuggestion::None,
        },
    }
}

fn manual_stdio_sources_to_wire(
    value: crate::mcp_platform::service::ManualStdioSourcesPage,
) -> McpManualStdioSourcesPage {
    McpManualStdioSourcesPage {
        items: value
            .items
            .into_iter()
            .map(|source| McpManualStdioSource {
                source_id: source.source_id,
                display_name: source.display_name,
                publisher_name: source.publisher_name,
                trust_tier: trust_to_wire(source.trust_tier),
                compatibility: if source.compatible {
                    McpCompatibility::Compatible
                } else {
                    McpCompatibility::Incompatible
                },
            })
            .collect(),
        provider: if value.available {
            McpManualStdioProviderState::Available
        } else {
            McpManualStdioProviderState::OperationNotSupported
        },
    }
}

fn distribution_kind(adapter: &str) -> McpDistributionKind {
    match adapter {
        "remote_http" => McpDistributionKind::RemoteHttp,
        "manual_stdio" => McpDistributionKind::ManualStdio,
        "npm" => McpDistributionKind::Npm,
        "python_wheel" => McpDistributionKind::PythonWheel,
        "binary_archive" => McpDistributionKind::BinaryArchive,
        "docker" => McpDistributionKind::Docker,
        "git_dev" => McpDistributionKind::GitDev,
        _ => unreachable!("verified manifests use a closed distribution enum"),
    }
}

fn policy_outcome_to_wire(outcome: PolicyOutcome) -> McpPolicyOutcome {
    match outcome {
        PolicyOutcome::Allow => McpPolicyOutcome::Allow,
        PolicyOutcome::Deny => McpPolicyOutcome::Deny,
        PolicyOutcome::NeedsConfirmation => McpPolicyOutcome::NeedsConfirmation,
    }
}

fn task_operation_to_wire(operation: TaskOperation) -> McpTaskOperation {
    match operation {
        TaskOperation::Register => McpTaskOperation::Register,
        TaskOperation::Install => McpTaskOperation::Install,
        TaskOperation::Update => McpTaskOperation::Update,
        TaskOperation::Repair => McpTaskOperation::Repair,
        TaskOperation::Uninstall => McpTaskOperation::Uninstall,
        TaskOperation::Health => McpTaskOperation::Health,
    }
}

fn task_status_to_wire(status: TaskStatus) -> McpTaskStatus {
    match status {
        TaskStatus::Planned => McpTaskStatus::Planned,
        TaskStatus::AwaitingConfirmation => McpTaskStatus::AwaitingConfirmation,
        TaskStatus::Queued => McpTaskStatus::Queued,
        TaskStatus::Running => McpTaskStatus::Running,
        TaskStatus::Cancelling => McpTaskStatus::Cancelling,
        TaskStatus::Verifying => McpTaskStatus::Verifying,
        TaskStatus::Activating => McpTaskStatus::Activating,
        TaskStatus::RollingBack => McpTaskStatus::RollingBack,
        TaskStatus::Succeeded => McpTaskStatus::Succeeded,
        TaskStatus::Failed => McpTaskStatus::Failed,
        TaskStatus::Cancelled => McpTaskStatus::Cancelled,
        TaskStatus::Interrupted => McpTaskStatus::Interrupted,
        TaskStatus::RecoveryRequired => McpTaskStatus::RecoveryRequired,
    }
}

fn event_type_to_wire(event_type: AuditEventType) -> McpEventType {
    match event_type {
        AuditEventType::TaskCreated => McpEventType::TaskCreated,
        AuditEventType::TaskStatusChanged => McpEventType::TaskStatusChanged,
        AuditEventType::StepStarted => McpEventType::StepStarted,
        AuditEventType::StepCommitted => McpEventType::StepCommitted,
        AuditEventType::RecoveryInterrupted => McpEventType::RecoveryInterrupted,
        AuditEventType::ConfirmationRecorded => McpEventType::ConfirmationRecorded,
        AuditEventType::CancellationRequested => McpEventType::CancellationRequested,
        AuditEventType::RetryQueued => McpEventType::RetryQueued,
    }
}

fn event_payload_to_wire(payload: AuditPayload) -> McpEventPayload {
    match payload {
        AuditPayload::TaskCreated { status } => McpEventPayload::TaskCreated {
            status: task_status_to_wire(status),
        },
        AuditPayload::TaskStatusChanged { from, to } => McpEventPayload::TaskStatusChanged {
            from: task_status_to_wire(from),
            to: task_status_to_wire(to),
        },
        AuditPayload::ConfirmationRecorded {
            from,
            to,
            plan_id,
            plan_digest,
        } => McpEventPayload::ConfirmationRecorded {
            from: task_status_to_wire(from),
            to: task_status_to_wire(to),
            plan_id,
            plan_digest,
        },
        AuditPayload::CancellationRequested { from, to } => {
            McpEventPayload::CancellationRequested {
                from: task_status_to_wire(from),
                to: task_status_to_wire(to),
            }
        }
        AuditPayload::StepStatusChanged { ordinal, from, to } => {
            McpEventPayload::StepStatusChanged {
                ordinal,
                from: task_step_to_wire(from),
                to: task_step_to_wire(to),
            }
        }
        AuditPayload::RecoveryDecision { decision } => McpEventPayload::RecoveryDecision {
            decision: match decision {
                RecoveryDecision::ResumeFromStep { ordinal } => {
                    McpRecoveryDecision::ResumeFromStep { ordinal }
                }
                RecoveryDecision::RollbackFromStep { ordinal } => {
                    McpRecoveryDecision::RollbackFromStep { ordinal }
                }
                RecoveryDecision::RequiresManualRecovery => {
                    McpRecoveryDecision::RequiresManualRecovery
                }
            },
        },
    }
}

fn task_step_to_wire(status: TaskStepStatus) -> McpTaskStepStatus {
    match status {
        TaskStepStatus::NotStarted => McpTaskStepStatus::NotStarted,
        TaskStepStatus::Started => McpTaskStepStatus::Started,
        TaskStepStatus::Committed => McpTaskStepStatus::Committed,
    }
}

fn platform_name(platform: Platform) -> String {
    match platform {
        Platform::Windows => "windows",
        Platform::Macos => "macos",
        Platform::Linux => "linux",
        Platform::Any => "any",
    }
    .to_string()
}

fn architecture_name(architecture: Architecture) -> String {
    match architecture {
        Architecture::X86_64 => "x86_64",
        Architecture::Aarch64 => "aarch64",
        Architecture::Universal => "universal",
        Architecture::Any => "any",
    }
    .to_string()
}

const fn archive_format_name(format: ArchiveFormat) -> &'static str {
    match format {
        ArchiveFormat::Zip => "zip",
        ArchiveFormat::TarGz => "tar.gz",
        ArchiveFormat::TarXz => "tar.xz",
    }
}

const fn git_adapter_name(adapter: GitDevAdapter) -> &'static str {
    match adapter {
        GitDevAdapter::Npm => "npm",
        GitDevAdapter::PythonWheel => "python_wheel",
        GitDevAdapter::BinaryArchive => "binary_archive",
        GitDevAdapter::Docker => "docker",
    }
}

fn managed_page_to_wire(page: crate::mcp_platform::service::ManagedMcpPage) -> McpManagedPage {
    McpManagedPage {
        items: page
            .items
            .into_iter()
            .map(managed_summary_to_wire)
            .collect(),
        next_cursor: page.next_cursor,
    }
}

fn managed_summary_to_wire(
    value: crate::mcp_platform::service::ManagedMcpSummary,
) -> McpManagedSummary {
    let runtime_control =
        runtime_control_binding(&value.managed_mcp_id, value.revision).map(|binding| {
            let lifecycle_idle = value
                .current_task
                .as_ref()
                .is_none_or(|task| task.status.is_terminal());
            McpRuntimeControlCapability {
                binding,
                can_start: matches!(
                    value.runtime,
                    crate::mcp_platform::RuntimeState::Stopped
                        | crate::mcp_platform::RuntimeState::Crashed
                ) && value.registration
                    == crate::mcp_platform::RegistrationState::Registered
                    && !value.recovery_required
                    && lifecycle_idle,
                can_stop: matches!(
                    value.runtime,
                    crate::mcp_platform::RuntimeState::Running
                        | crate::mcp_platform::RuntimeState::Crashed
                ) && value.registration
                    == crate::mcp_platform::RegistrationState::Registered
                    && !value.recovery_required
                    && lifecycle_idle,
            }
        });
    McpManagedSummary {
        managed_mcp_id: value.managed_mcp_id,
        mcp_id: value.mcp_id,
        installation_scope: McpInstallationScope::User,
        registration: registration_to_wire(value.registration),
        installation: installation_to_wire(value.installation),
        runtime: runtime_to_wire(value.runtime),
        health: health_state_to_wire(value.health),
        default_enabled: value.default_enabled,
        revision: value.revision,
        updated_at_ms: value.updated_at_ms,
        distribution_adapter: value.distribution_adapter,
        active_version: value.active_version,
        available_version: value.available_version,
        available_manifest_digest: value.available_manifest_digest,
        current_task: value.current_task.map(task_to_wire),
        recovery_required: value.recovery_required,
        credential_status: value.credential_status.map(credential_status_to_wire),
        external_capability: value.external_capability.map(|status| match status {
            crate::mcp_platform::service::ManagedExternalCapabilityStatus::DockerCliMissing => {
                McpExternalCapabilityStatus::DockerCliMissing
            }
            crate::mcp_platform::service::ManagedExternalCapabilityStatus::DockerDaemonUnverified => {
                McpExternalCapabilityStatus::DockerDaemonUnverified
            }
            crate::mcp_platform::service::ManagedExternalCapabilityStatus::DockerDaemonVerified => {
                McpExternalCapabilityStatus::DockerDaemonVerified
            }
            crate::mcp_platform::service::ManagedExternalCapabilityStatus::DockerDaemonPolicyDenied => {
                McpExternalCapabilityStatus::DockerDaemonPolicyDenied
            }
            crate::mcp_platform::service::ManagedExternalCapabilityStatus::GitMissing => {
                McpExternalCapabilityStatus::GitMissing
            }
            crate::mcp_platform::service::ManagedExternalCapabilityStatus::GitAvailable => {
                McpExternalCapabilityStatus::GitAvailable
            }
        }),
        eligibility: McpManagedEligibility {
            update: value.eligibility.update,
            repair: value.eligibility.repair,
            uninstall: value.eligibility.uninstall,
            reason: managed_eligibility_reason_to_wire(value.eligibility.reason),
            update_reason: managed_eligibility_reason_to_wire(value.eligibility.update_reason),
            repair_reason: managed_eligibility_reason_to_wire(value.eligibility.repair_reason),
            uninstall_reason: managed_eligibility_reason_to_wire(
                value.eligibility.uninstall_reason,
            ),
        },
        next_action: match value.next_action {
            crate::mcp_platform::service::ManagedNextAction::None => McpManagedNextAction::None,
            crate::mcp_platform::service::ManagedNextAction::EnableAfterHealth => {
                McpManagedNextAction::EnableAfterHealth
            }
            crate::mcp_platform::service::ManagedNextAction::Repair => McpManagedNextAction::Repair,
            crate::mcp_platform::service::ManagedNextAction::ResolveRecovery => {
                McpManagedNextAction::ResolveRecovery
            }
            crate::mcp_platform::service::ManagedNextAction::WaitForTask => {
                McpManagedNextAction::WaitForTask
            }
            crate::mcp_platform::service::ManagedNextAction::ResumeTask => {
                McpManagedNextAction::ResumeTask
            }
        },
        phase_capabilities: McpPhaseCapabilities {
            session_enablement: McpPhaseCapability::Available,
            tool_policy: McpPhaseCapability::NotAvailableInThisPhase,
            profiles: McpPhaseCapability::Available,
            model_suggestions: McpPhaseCapability::Available,
        },
        runtime_control,
    }
}

fn runtime_control_binding(managed_mcp_id: &str, revision: i64) -> Option<String> {
    let authority = current_transport_mcp_platform_write_authority()?;
    let mut hasher = Sha256::new();
    hasher.update(b"goose-managed-runtime-control-v1\0");
    hasher.update(authority.binding().as_bytes());
    hasher.update(b"\0");
    hasher.update(managed_mcp_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(revision.to_be_bytes());
    Some(crate::utils::bytes_to_hex(hasher.finalize().as_slice()))
}

fn managed_eligibility_reason_to_wire(
    value: crate::mcp_platform::service::ManagedEligibilityReason,
) -> McpManagedEligibilityReason {
    match value {
        crate::mcp_platform::service::ManagedEligibilityReason::Eligible => {
            McpManagedEligibilityReason::Eligible
        }
        crate::mcp_platform::service::ManagedEligibilityReason::RegistrationOnly => {
            McpManagedEligibilityReason::RegistrationOnly
        }
        crate::mcp_platform::service::ManagedEligibilityReason::TaskRecoveryRequired => {
            McpManagedEligibilityReason::TaskRecoveryRequired
        }
        crate::mcp_platform::service::ManagedEligibilityReason::TaskInProgress => {
            McpManagedEligibilityReason::TaskInProgress
        }
        crate::mcp_platform::service::ManagedEligibilityReason::TaskInterrupted => {
            McpManagedEligibilityReason::TaskInterrupted
        }
        crate::mcp_platform::service::ManagedEligibilityReason::NotInstalled => {
            McpManagedEligibilityReason::NotInstalled
        }
        crate::mcp_platform::service::ManagedEligibilityReason::NoUpdateAvailable => {
            McpManagedEligibilityReason::NoUpdateAvailable
        }
        crate::mcp_platform::service::ManagedEligibilityReason::RuntimeUnavailable => {
            McpManagedEligibilityReason::RuntimeUnavailable
        }
        crate::mcp_platform::service::ManagedEligibilityReason::PolicyDenied => {
            McpManagedEligibilityReason::PolicyDenied
        }
        crate::mcp_platform::service::ManagedEligibilityReason::Incompatible => {
            McpManagedEligibilityReason::Incompatible
        }
    }
}

fn managed_detail_to_wire(
    value: crate::mcp_platform::service::ManagedMcpDetail,
) -> McpManagedDetail {
    McpManagedDetail {
        summary: managed_summary_to_wire(value.summary),
        distribution_adapter: value.distribution_adapter,
        active_manifest_digest: value.active_manifest_digest,
        active_version: value.active_version,
        extension_config_key: value.extension_config_key,
        projection_digest: value.projection_digest,
        latest_health: value.latest_health.map(health_observation_to_wire),
        registration_task: value.registration_task.map(task_to_wire),
        supply_chain: value.supply_chain.map(|evidence| match evidence {
            crate::mcp_platform::service::ManagedSupplyChainSummary::Docker {
                image,
                image_digest,
                adapter_version,
                daemon_version,
                rootless,
                mount_plan_digest,
                created_at_ms,
            } => McpSupplyChainSummary::Docker {
                image,
                image_digest,
                adapter_version,
                daemon_version,
                rootless,
                mount_plan_digest,
                created_at_ms,
            },
            crate::mcp_platform::service::ManagedSupplyChainSummary::GitDev {
                repository_origin,
                commit,
                git_tree_id,
                materialized_tree_digest,
                adapter_version,
                created_at_ms,
            } => McpSupplyChainSummary::GitDev {
                repository_origin,
                commit,
                git_tree_id,
                materialized_tree_digest,
                adapter_version,
                created_at_ms,
            },
        }),
    }
}

fn health_status_to_wire(value: crate::mcp_platform::service::HealthStatus) -> McpHealthStatus {
    McpHealthStatus {
        managed_mcp_id: value.managed_mcp_id,
        state: health_state_to_wire(value.state),
        latest: value.latest.map(health_observation_to_wire),
        credential_status: value.credential_status.map(credential_status_to_wire),
    }
}

fn credential_status_to_wire(
    value: crate::mcp_platform::service::CredentialStatus,
) -> McpCredentialStatus {
    match value {
        crate::mcp_platform::service::CredentialStatus::Unconfigured => {
            McpCredentialStatus::Unconfigured
        }
        crate::mcp_platform::service::CredentialStatus::ReRegistrationRequired => {
            McpCredentialStatus::ReRegistrationRequired
        }
        crate::mcp_platform::service::CredentialStatus::TrustedStateConflict => {
            McpCredentialStatus::TrustedStateConflict
        }
        crate::mcp_platform::service::CredentialStatus::TemporarilyUnavailable => {
            McpCredentialStatus::TemporarilyUnavailable
        }
        crate::mcp_platform::service::CredentialStatus::Ready => McpCredentialStatus::Ready,
    }
}

fn health_observation_to_wire(
    value: crate::mcp_platform::HealthObservationRecord,
) -> McpHealthObservation {
    McpHealthObservation {
        task_id: value.task_id,
        mode: if value.check_type == "runtime" {
            McpHealthCheckMode::Runtime
        } else {
            McpHealthCheckMode::Registration
        },
        result: match value.result_code {
            crate::mcp_platform::HealthResultCode::Healthy => McpHealthResult::Healthy,
            crate::mcp_platform::HealthResultCode::Unhealthy => McpHealthResult::Unhealthy,
            crate::mcp_platform::HealthResultCode::BlockedAuth => McpHealthResult::BlockedAuth,
            crate::mcp_platform::HealthResultCode::Incompatible => McpHealthResult::Incompatible,
            crate::mcp_platform::HealthResultCode::Timeout => McpHealthResult::Timeout,
            crate::mcp_platform::HealthResultCode::Cancelled => McpHealthResult::Cancelled,
        },
        latency_ms: value.latency_ms,
        capabilities_digest: value.capabilities_digest,
        tools_digest: value.tools_digest,
        checked_at_ms: value.checked_at_ms,
        detail_code: match value.detail_code {
            crate::mcp_platform::HealthDetailCode::ProjectionConsistent => {
                McpHealthDetailCode::ProjectionConsistent
            }
            crate::mcp_platform::HealthDetailCode::ProjectionDrift => {
                McpHealthDetailCode::ProjectionDrift
            }
            crate::mcp_platform::HealthDetailCode::CredentialHandleMissing => {
                McpHealthDetailCode::CredentialHandleMissing
            }
            crate::mcp_platform::HealthDetailCode::ExpectedStatus => {
                McpHealthDetailCode::ExpectedStatus
            }
            crate::mcp_platform::HealthDetailCode::UnexpectedStatus => {
                McpHealthDetailCode::UnexpectedStatus
            }
            crate::mcp_platform::HealthDetailCode::McpInitializeSucceeded => {
                McpHealthDetailCode::McpInitializeSucceeded
            }
            crate::mcp_platform::HealthDetailCode::McpInitializeFailed => {
                McpHealthDetailCode::McpInitializeFailed
            }
            crate::mcp_platform::HealthDetailCode::McpListToolsSucceeded => {
                McpHealthDetailCode::McpListToolsSucceeded
            }
            crate::mcp_platform::HealthDetailCode::McpListToolsFailed => {
                McpHealthDetailCode::McpListToolsFailed
            }
            crate::mcp_platform::HealthDetailCode::IncompatibleHealthContract => {
                McpHealthDetailCode::IncompatibleHealthContract
            }
            crate::mcp_platform::HealthDetailCode::CleanupFailed => {
                McpHealthDetailCode::CleanupFailed
            }
            crate::mcp_platform::HealthDetailCode::Timeout => McpHealthDetailCode::Timeout,
            crate::mcp_platform::HealthDetailCode::Cancelled => McpHealthDetailCode::Cancelled,
        },
    }
}

fn registration_from_wire(value: McpRegistrationState) -> crate::mcp_platform::RegistrationState {
    match value {
        McpRegistrationState::Absent => crate::mcp_platform::RegistrationState::Absent,
        McpRegistrationState::Registered => crate::mcp_platform::RegistrationState::Registered,
    }
}
fn registration_to_wire(value: crate::mcp_platform::RegistrationState) -> McpRegistrationState {
    match value {
        crate::mcp_platform::RegistrationState::Absent => McpRegistrationState::Absent,
        crate::mcp_platform::RegistrationState::Registered => McpRegistrationState::Registered,
    }
}
fn installation_from_wire(value: McpInstallationState) -> crate::mcp_platform::InstallationState {
    match value {
        McpInstallationState::NotApplicable => {
            crate::mcp_platform::InstallationState::NotApplicable
        }
        McpInstallationState::NotInstalled => crate::mcp_platform::InstallationState::NotInstalled,
        McpInstallationState::Staged => crate::mcp_platform::InstallationState::Staged,
        McpInstallationState::Installed => crate::mcp_platform::InstallationState::Installed,
        McpInstallationState::UpdateAvailable => {
            crate::mcp_platform::InstallationState::UpdateAvailable
        }
        McpInstallationState::RepairRequired => {
            crate::mcp_platform::InstallationState::RepairRequired
        }
        McpInstallationState::UninstallPending => {
            crate::mcp_platform::InstallationState::UninstallPending
        }
    }
}
fn installation_to_wire(value: crate::mcp_platform::InstallationState) -> McpInstallationState {
    match value {
        crate::mcp_platform::InstallationState::NotApplicable => {
            McpInstallationState::NotApplicable
        }
        crate::mcp_platform::InstallationState::NotInstalled => McpInstallationState::NotInstalled,
        crate::mcp_platform::InstallationState::Staged => McpInstallationState::Staged,
        crate::mcp_platform::InstallationState::Installed => McpInstallationState::Installed,
        crate::mcp_platform::InstallationState::UpdateAvailable => {
            McpInstallationState::UpdateAvailable
        }
        crate::mcp_platform::InstallationState::RepairRequired => {
            McpInstallationState::RepairRequired
        }
        crate::mcp_platform::InstallationState::UninstallPending => {
            McpInstallationState::UninstallPending
        }
    }
}
fn runtime_from_wire(value: McpRuntimeState) -> crate::mcp_platform::RuntimeState {
    match value {
        McpRuntimeState::Stopped => crate::mcp_platform::RuntimeState::Stopped,
        McpRuntimeState::Starting => crate::mcp_platform::RuntimeState::Starting,
        McpRuntimeState::Running => crate::mcp_platform::RuntimeState::Running,
        McpRuntimeState::Stopping => crate::mcp_platform::RuntimeState::Stopping,
        McpRuntimeState::Crashed => crate::mcp_platform::RuntimeState::Crashed,
    }
}
fn runtime_to_wire(value: crate::mcp_platform::RuntimeState) -> McpRuntimeState {
    match value {
        crate::mcp_platform::RuntimeState::Stopped => McpRuntimeState::Stopped,
        crate::mcp_platform::RuntimeState::Starting => McpRuntimeState::Starting,
        crate::mcp_platform::RuntimeState::Running => McpRuntimeState::Running,
        crate::mcp_platform::RuntimeState::Stopping => McpRuntimeState::Stopping,
        crate::mcp_platform::RuntimeState::Crashed => McpRuntimeState::Crashed,
    }
}
fn health_from_wire(value: McpHealthState) -> crate::mcp_platform::HealthState {
    match value {
        McpHealthState::Unknown => crate::mcp_platform::HealthState::Unknown,
        McpHealthState::Checking => crate::mcp_platform::HealthState::Checking,
        McpHealthState::Healthy => crate::mcp_platform::HealthState::Healthy,
        McpHealthState::Degraded => crate::mcp_platform::HealthState::Degraded,
        McpHealthState::Unhealthy => crate::mcp_platform::HealthState::Unhealthy,
        McpHealthState::BlockedAuth => crate::mcp_platform::HealthState::BlockedAuth,
        McpHealthState::Incompatible => crate::mcp_platform::HealthState::Incompatible,
    }
}
fn health_state_to_wire(value: crate::mcp_platform::HealthState) -> McpHealthState {
    match value {
        crate::mcp_platform::HealthState::Unknown => McpHealthState::Unknown,
        crate::mcp_platform::HealthState::Checking => McpHealthState::Checking,
        crate::mcp_platform::HealthState::Healthy => McpHealthState::Healthy,
        crate::mcp_platform::HealthState::Degraded => McpHealthState::Degraded,
        crate::mcp_platform::HealthState::Unhealthy => McpHealthState::Unhealthy,
        crate::mcp_platform::HealthState::BlockedAuth => McpHealthState::BlockedAuth,
        crate::mcp_platform::HealthState::Incompatible => McpHealthState::Incompatible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use crate::agents::extension_manager::tests::RetryableCloseClient;
    use crate::agents::mcp_client::McpClientTrait;
    use crate::agents::platform_extensions::orchestrator::{
        CompletionTestGate, OrchestratorClient,
    };
    use crate::agents::platform_extensions::PlatformExtensionContext;
    use crate::agents::{Agent, AgentConfig, ToolCallContext};
    use crate::config::{Config, PermissionManager};
    use crate::conversation::message::Message;
    use crate::mcp_platform::credential_authority::EnrollmentWriterOwner;
    use crate::mcp_platform::error::MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE;
    use crate::mcp_platform::repository::{
        ActivateManagedInstallation, CreateTask, HttpsManifestProvisionRecord, PutOwnedProjection,
        SqliteMcpPlatformRepository, StageManagedInstallation, StepTransition, TaskTransition,
    };
    use crate::mcp_platform::service::{CredentialStatus, GovernedSourceRefreshFetcher};
    use crate::mcp_platform::task::{
        CompensationDescriptor, RedactedError, RedactedErrorCode, RollbackStatus, StepEvidence,
        TaskOperation, TaskStatus, TaskStepStatus,
    };
    use crate::mcp_platform::{
        parse_manifest, AuthRequirementResolver, ConfigAuthRequirementResolver,
        ConfigProjectionSink, CoreManagedRemoteHttpNetworkPolicy, CoreTransportProjectionAdapter,
        EmptyHostIntegrationAdapter, HealthAdapterResult, HealthCheckAdapter, HealthCheckMode,
        HealthDetailCode, HealthExecution, HealthResultCode, HealthState, InMemoryIntegritySigner,
        InstallationState, LifecyclePorts, ManifestProof, ManifestRecord, McpPlatformError,
        McpPlatformErrorCode, McpPlatformService, McpPlatformServiceOptions, RegistrationEffect,
        RegistrationEffectAdapter, RegistrationEffectEvidence, RegistrationState,
        RemoteHttpNetworkPolicy, SystemClock, TrustTier, UuidGenerator,
    };
    use crate::providers::base::{stream_from_single_message, MessageStream, Provider};
    use crate::scheduler::{ScheduledJob, SchedulerError};
    use crate::scheduler_trait::SchedulerTrait;
    use crate::session::{
        EnabledExtensionsState, ExtensionData, ExtensionState, Session, SessionManager, SessionType,
    };
    use anyhow::Result;
    use async_trait::async_trait;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use goose_providers::conversation::token_usage::{ProviderUsage, Usage};
    use goose_providers::errors::ProviderError;
    const TEST_GATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
    use goose_providers::model::ModelConfig;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn https_manifest_preview_mapping_preserves_security_fields_and_has_no_service_warnings() {
        let preview =
            https_manifest_preview_to_wire(crate::mcp_platform::service::HttpsManifestPreview {
                manifest_id: "manifest".to_string(),
                version: "1.0.0".to_string(),
                redacted_origin: "https://example.test".to_string(),
                raw_digest: "raw".to_string(),
                parsed_digest: "parsed".to_string(),
                redirect_chain_digest: "redirect".to_string(),
                dns_evidence_digest: "dns".to_string(),
            });

        assert_eq!(preview.manifest_id, "manifest");
        assert_eq!(preview.version, "1.0.0");
        assert_eq!(preview.redacted_origin, "https://example.test");
        assert_eq!(preview.raw_digest, "raw");
        assert_eq!(preview.parsed_digest, "parsed");
        assert_eq!(preview.redirect_chain_digest, "redirect");
        assert_eq!(preview.dns_evidence_digest, "dns");
        assert!(preview.warnings.is_empty());
    }

    const ED25519_SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    const IMPORTABLE_MANIFEST: &str =
        include_str!("../../../../../documentation/static/schemas/examples/manual-stdio.json");
    const REMOTE: &str =
        include_str!("../../../../../documentation/static/schemas/examples/remote-http.json");

    fn context() -> RequestContext {
        RequestContext::local_authenticated_client("correlation".to_string())
    }

    fn runtime_test_agent(data_dir: &Path) -> Arc<Agent> {
        Arc::new(Agent::with_config(AgentConfig::new(
            Arc::new(SessionManager::new(data_dir.to_path_buf())),
            Arc::new(PermissionManager::new(data_dir.to_path_buf())),
            None,
            Config::global().get_goose_mode().unwrap_or_default(),
            true,
            GoosePlatform::GooseCli,
        )))
    }

    struct SingleMessageProvider;

    #[async_trait]
    impl Provider for SingleMessageProvider {
        fn get_name(&self) -> &str {
            "single-message-test"
        }

        async fn stream(
            &self,
            _model_config: &ModelConfig,
            _system: &str,
            _messages: &[Message],
            _tools: &[rmcp::model::Tool],
        ) -> std::result::Result<MessageStream, ProviderError> {
            Ok(stream_from_single_message(
                Message::assistant().with_text("old response"),
                ProviderUsage::new("single-message-test".to_string(), Usage::default()),
            ))
        }
    }

    struct CompletionRaceProvider {
        calls: AtomicUsize,
        new_run_entered: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        new_run_release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl CompletionRaceProvider {
        fn new(
            new_run_entered: tokio::sync::oneshot::Sender<()>,
            new_run_release: tokio::sync::oneshot::Receiver<()>,
        ) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                new_run_entered: tokio::sync::Mutex::new(Some(new_run_entered)),
                new_run_release: tokio::sync::Mutex::new(Some(new_run_release)),
            }
        }
    }

    #[async_trait]
    impl Provider for CompletionRaceProvider {
        fn get_name(&self) -> &str {
            "completion-race-test"
        }

        async fn stream(
            &self,
            _model_config: &ModelConfig,
            _system: &str,
            _messages: &[Message],
            _tools: &[rmcp::model::Tool],
        ) -> std::result::Result<MessageStream, ProviderError> {
            let message = Message::assistant().with_text("completion race response");
            let usage = ProviderUsage::new("completion-race-test".to_string(), Usage::default());
            if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
                return Ok(stream_from_single_message(message, usage));
            }

            let entered = self.new_run_entered.lock().await.take();
            let release = self.new_run_release.lock().await.take();
            Ok(Box::pin(futures::stream::once(async move {
                if let Some(entered) = entered {
                    let _ = entered.send(());
                }
                if let Some(release) = release {
                    let _ = tokio::time::timeout(TEST_GATE_TIMEOUT, release)
                        .await
                        .map_err(|_| {
                            ProviderError::RequestFailed(
                                "completion race provider release timed out".to_string(),
                            )
                        })?;
                }
                Ok((Some(message), Some(usage)))
            })))
        }
    }

    async fn transition_task_fixture(
        repository: &Arc<SqliteMcpPlatformRepository>,
        task: &crate::mcp_platform::repository::TaskRecord,
        next_status: TaskStatus,
        now_ms: i64,
    ) -> crate::mcp_platform::repository::TaskRecord {
        repository
            .transition_task(TaskTransition {
                task_id: &task.task_id,
                expected_revision: task.revision,
                next_status,
                actor: "runtime-test",
                now_ms,
                heartbeat_at_ms: Some(now_ms),
                progress: task.progress,
                redacted_error: task.redacted_error.as_ref(),
                rollback_status: task.rollback_status,
                rollback_evidence: task.rollback_evidence.as_ref(),
            })
            .await
            .unwrap()
    }

    async fn prepare_managed_runtime_fixture(
        service: &McpPlatformService,
        repository: &Arc<SqliteMcpPlatformRepository>,
    ) -> (String, crate::agents::ExtensionConfig, i64, bool) {
        let artifact_digest = sha256_hex_for_tests(b"managed-runtime-start-partial");
        let manifest = serde_json::json!({
            "schema_version": 1,
            "id": "org.example.managed-runtime-start",
            "version": "1.0.0",
            "name": "Managed runtime start fixture",
            "description": "Managed runtime start fixture",
            "homepage": "https://example.com/managed-runtime-start",
            "publisher": {"id": "com.example", "name": "Example"},
            "license": {"spdx": "MIT"},
            "capabilities": ["tools"],
            "permissions": [{
                "id": "spawn-server",
                "kind": "process_spawn",
                "reason": "Run the installed MCP process.",
                "required": true,
                "scope": "managed-node-runtime"
            }],
            "distribution": {
                "type": "npm",
                "package": "@example/managed-runtime-start",
                "package_version": "1.0.0",
                "registry": "https://registry.npmjs.org",
                "artifacts": [{
                    "platform": "any",
                    "arch": "any",
                    "url": "https://registry.npmjs.org/@example/managed-runtime-start/-/managed-runtime-start-1.0.0.tgz",
                    "digest": {"algorithm": "sha256", "value": artifact_digest},
                    "size_bytes": 1,
                    "media_type": "application/gzip"
                }],
                "entrypoint": {
                    "executable": "${installation.bin}/managed-runtime-start",
                    "args": [],
                    "environment_keys": []
                }
            },
            "transport": {"type": "stdio", "startup_timeout_seconds": 20},
            "auth": {"type": "none"},
            "health_check": {"type": "mcp_initialize", "timeout_seconds": 20},
            "owned_files": [{
                "root": "installation",
                "path": "bin/managed-runtime-start",
                "kind": "file",
                "remove_on_uninstall": true
            }],
            "uninstall": {"mode": "remove_owned_files_only", "preserve_user_data": true}
        });
        let verified = parse_manifest(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        let manifest_digest = verified.digest().to_string();
        repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Local,
                source_metadata: Default::default(),
                created_at_ms: 1,
            })
            .await
            .unwrap();
        let context = context();
        let review = service
            .plan_create(
                &context,
                PlanCreateInput {
                    intent: PlanIntent::Install { manifest_digest },
                    idempotency_key: "managed-runtime-start-partial".to_string(),
                },
            )
            .await
            .unwrap();
        let plan = repository.get_plan(&review.plan_id).await.unwrap();
        let task_id = "managed-runtime-start-partial-task";
        let task = repository
            .create_task(CreateTask {
                task_id,
                plan_id: &review.plan_id,
                plan_digest: plan.plan.plan_digest(),
                operation: TaskOperation::from(plan.plan.operation()),
                idempotency_key: "managed-runtime-start-partial-task-key",
                actor: context.actor(),
                now_ms: 10,
                adapter_evidence: None,
                rollback_evidence: None,
            })
            .await
            .unwrap();
        let awaiting =
            transition_task_fixture(repository, &task, TaskStatus::AwaitingConfirmation, 11).await;
        let confirmed = repository
            .confirm_task(&task_id, awaiting.revision, "runtime-test", 12)
            .await
            .unwrap();
        let running = repository
            .claim_next_task("runtime-test", 13, 30_000)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(running.task_id, confirmed.task_id);
        let verifying =
            transition_task_fixture(repository, &running, TaskStatus::Verifying, 14).await;
        let activating =
            transition_task_fixture(repository, &verifying, TaskStatus::Activating, 15).await;
        let managed_mcp_id = plan.target.managed_mcp_id.clone().unwrap();
        let adapter_evidence = crate::mcp_platform::AdapterEvidence {
            adapter_id: "runtime-test".to_string(),
            adapter_version: "1".to_string(),
            compatible_for_recovery: true,
            resume_safe: true,
        };
        let verification_evidence = crate::mcp_platform::ArtifactVerificationEvidence {
            source_origin: "https://registry.example.test".to_string(),
            artifact_digest: "11".repeat(32),
            size_bytes: 1,
            adapter_id: "runtime-test".to_string(),
            adapter_version: "1".to_string(),
            platform_selector: "windows-x86_64".to_string(),
            verification_result: crate::mcp_platform::VerificationResult::Verified,
            installed_at_ms: 16,
            artifact_signature:
                crate::mcp_platform::ArtifactSignatureStatus::NotDeclaredByManifestV1,
        };
        repository
            .stage_managed_installation(StageManagedInstallation {
                managed_mcp_id: &managed_mcp_id,
                mcp_id: plan.plan.manifest_id(),
                installation_scope: "user",
                distribution_adapter: &plan.plan.adapter().id,
                manifest_digest: plan.plan.manifest_digest(),
                version: plan.plan.manifest_version(),
                installation_root: "D:\\managed-runtime-test",
                task_id,
                adapter_evidence: &adapter_evidence,
                verification_evidence: &verification_evidence,
                materialized_tree_digest: &"22".repeat(32),
                supply_chain_evidence: None,
                owned_relative_paths: &[],
                now_ms: 16,
            })
            .await
            .unwrap();
        repository
            .mark_managed_runtime_activated(
                &managed_mcp_id,
                plan.plan.manifest_version(),
                task_id,
                17,
            )
            .await
            .unwrap();
        repository
            .activate_managed_installation(ActivateManagedInstallation {
                managed_mcp_id: &managed_mcp_id,
                target_version: plan.plan.manifest_version(),
                task_id,
                now_ms: 18,
            })
            .await
            .unwrap();
        repository
            .add_task_step_with_adapter(
                task_id,
                8,
                "managed-runtime-start-partial:projection",
                &CompensationDescriptor::NoCompensation,
                "connection_projection_repository",
                "1",
                "runtime-test",
                activating.revision,
                19,
            )
            .await
            .unwrap();
        repository
            .transition_task_step(StepTransition {
                task_id,
                ordinal: 8,
                owner_id: "runtime-test",
                expected_task_revision: activating.revision,
                expected_status: TaskStepStatus::NotStarted,
                next_status: TaskStepStatus::Started,
                evidence: None,
                actor: "runtime-test",
                now_ms: 20,
            })
            .await
            .unwrap();
        let projection = repository
            .put_owned_connection_projection(PutOwnedProjection {
                plan_id: &review.plan_id,
                owner_task_id: task_id,
                worker_owner_id: "runtime-test",
                step_ordinal: 8,
                step_token: "managed-runtime-start-partial:projection",
                now_ms: 21,
            })
            .await
            .unwrap();
        let claimed = repository.get_task(task_id).await.unwrap();
        repository
            .transition_task_step(StepTransition {
                task_id,
                ordinal: 8,
                owner_id: "runtime-test",
                expected_task_revision: claimed.revision,
                expected_status: TaskStepStatus::Started,
                next_status: TaskStepStatus::Committed,
                evidence: Some(&StepEvidence::ProjectionPersisted {
                    managed_mcp_id: managed_mcp_id.clone(),
                    link_key: projection.link_key,
                    revision: projection.revision,
                    created: true,
                }),
                actor: "runtime-test",
                now_ms: 22,
            })
            .await
            .unwrap();
        let task = repository.get_task(task_id).await.unwrap();
        transition_task_fixture(repository, &task, TaskStatus::Succeeded, 23).await;

        let inventory = repository
            .get_managed_inventory(&managed_mcp_id)
            .await
            .unwrap();
        let mut ready = inventory.managed.state.clone();
        ready.runtime = crate::mcp_platform::RuntimeState::Stopped;
        ready.health = crate::mcp_platform::HealthState::Healthy;
        let ready = repository
            .update_managed_state(&managed_mcp_id, inventory.managed.revision, &ready, 24)
            .await
            .unwrap();
        (
            managed_mcp_id,
            repository
                .get_connection_projection(&ready.managed_mcp_id)
                .await
                .unwrap()
                .projection
                .to_extension_config(),
            ready.revision,
            ready.state.default_enabled,
        )
    }

    struct ConnectionTestRegistration;

    #[async_trait]
    impl RegistrationEffectAdapter for ConnectionTestRegistration {
        fn adapter_id(&self) -> &'static str {
            "connection_registration"
        }

        fn adapter_version(&self) -> &'static str {
            "1"
        }

        async fn verify(
            &self,
            _effect: &RegistrationEffect,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> crate::mcp_platform::McpPlatformResult<RegistrationEffectEvidence> {
            Ok(RegistrationEffectEvidence {
                adapter_id: self.adapter_id().to_string(),
                adapter_version: self.adapter_version().to_string(),
            })
        }
    }

    struct ConnectionTestHealth;

    struct ConnectionTestRemoteHttpPolicy;

    #[async_trait]
    impl RemoteHttpNetworkPolicy for ConnectionTestRemoteHttpPolicy {
        fn validate_endpoint(&self, endpoint: &str) -> crate::mcp_platform::McpPlatformResult<()> {
            CoreManagedRemoteHttpNetworkPolicy::default().validate_endpoint(endpoint)
        }

        async fn secure_client(
            &self,
            _endpoint: &str,
            _connect_timeout: std::time::Duration,
        ) -> crate::mcp_platform::McpPlatformResult<crate::mcp_platform::ManagedRemoteHttpClient>
        {
            Err(McpPlatformError::new(
                McpPlatformErrorCode::RemoteHttpPolicyUnavailable,
                "connection-test remote HTTP client is unavailable",
            ))
        }

        async fn validate_for_plan(
            &self,
            endpoint: &str,
            _connect_timeout: std::time::Duration,
        ) -> crate::mcp_platform::McpPlatformResult<()> {
            self.validate_endpoint(endpoint)
        }
    }

    #[async_trait]
    impl HealthCheckAdapter for ConnectionTestHealth {
        fn adapter_id(&self) -> &'static str {
            "mcp_health"
        }

        fn adapter_version(&self) -> &'static str {
            "1"
        }

        async fn run(
            &self,
            _execution: HealthExecution,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> crate::mcp_platform::McpPlatformResult<HealthAdapterResult> {
            Ok(HealthAdapterResult {
                result_code: HealthResultCode::Healthy,
                latency_ms: 1,
                capabilities_digest: None,
                tools_digest: None,
                detail_code: HealthDetailCode::McpInitializeSucceeded,
            })
        }
    }

    async fn connection_test_health_failure_diagnostic(
        repository: &Arc<SqliteMcpPlatformRepository>,
        health_task: &crate::mcp_platform::repository::TaskRecord,
    ) -> String {
        let plan = repository.get_plan(&health_task.plan_id).await.unwrap();
        let health_request = repository
            .get_health_task_request(&health_task.task_id)
            .await
            .unwrap();
        let audit_events = repository
            .list_audit_events(&health_task.task_id)
            .await
            .unwrap();
        let task_steps = repository
            .list_task_steps(&health_task.task_id)
            .await
            .unwrap();

        let mut diagnostic = format!(
            "{}; health_request={{managed_mcp_id={}, mode={}}}; plan={{plan_id={}, plan_digest={}, manifest_id={}, manifest_version={}, operation={}, target_mcp_id={}, target_managed_mcp_id={}, target_scope={}, target_version={}}}; audit_count={}, audit=[",
            connection_test_task_failure_diagnostic(health_task),
            health_request.managed_mcp_id,
            health_request.mode.as_str(),
            plan.plan_id,
            plan.plan.plan_digest(),
            plan.plan.manifest_id(),
            plan.plan.manifest_version(),
            plan.plan.operation().as_str(),
            plan.target.mcp_id,
            plan.target.managed_mcp_id.as_deref().unwrap_or("none"),
            plan.target.installation_scope.as_deref().unwrap_or("none"),
            plan.target.version,
            audit_events.len(),
        );
        for (index, event) in audit_events.iter().take(32).enumerate() {
            if index > 0 {
                diagnostic.push_str("; ");
            }
            diagnostic.push_str(&format!(
                "seq={}, type={}, actor={}, at_ms={}, error={}",
                event.sequence,
                event.event_type.as_str(),
                event.actor,
                event.occurred_at_ms,
                redacted_error_diagnostic(event.redacted_error.as_ref()),
            ));
        }
        if audit_events.len() > 32 {
            diagnostic.push_str("; ... truncated");
        }
        diagnostic.push_str(&format!("]; step_count={}, steps=[", task_steps.len()));
        for (index, step) in task_steps.iter().take(32).enumerate() {
            if index > 0 {
                diagnostic.push_str("; ");
            }
            diagnostic.push_str(&format!(
                "ordinal={}, status={}, adapter={}@{}, started_at_ms={}, committed_at_ms={}, compensation={}",
                step.ordinal,
                step.status.as_str(),
                bounded_text(&step.adapter_id),
                bounded_text(&step.adapter_version),
                optional_i64(step.started_at_ms),
                optional_i64(step.committed_at_ms),
                step.compensation_status.as_str(),
            ));
        }
        if task_steps.len() > 32 {
            diagnostic.push_str("; ... truncated");
        }
        diagnostic.push(']');
        diagnostic
    }

    fn connection_test_task_failure_diagnostic(
        task: &crate::mcp_platform::repository::TaskRecord,
    ) -> String {
        format!(
            "task_id={}, status={}, operation={}, error={}, actor={}, owner_id={}, lease_expires_at_ms={}, heartbeat_at_ms={}, revision={}, event_sequence={}, progress={}, step_cursor={}, attempt_count={}, rollback_status={}, updated_at_ms={}",
            task.task_id,
            task.status.as_str(),
            task.operation.as_str(),
            redacted_error_diagnostic(task.redacted_error.as_ref()),
            task.actor,
            task.owner_id.as_deref().unwrap_or("none"),
            optional_i64(task.lease_expires_at_ms),
            optional_i64(task.heartbeat_at_ms),
            task.revision,
            task.event_sequence,
            task.progress,
            task.step_cursor,
            task.attempt_count,
            task.rollback_status.as_str(),
            task.updated_at_ms,
        )
    }

    fn redacted_error_diagnostic(error: Option<&crate::mcp_platform::RedactedError>) -> String {
        let Some(error) = error else {
            return "none".to_string();
        };
        let code = match error.code() {
            crate::mcp_platform::RedactedErrorCode::AdapterFailed => "adapter_failed",
            crate::mcp_platform::RedactedErrorCode::VerificationFailed => "verification_failed",
            crate::mcp_platform::RedactedErrorCode::ActivationFailed => "activation_failed",
            crate::mcp_platform::RedactedErrorCode::RollbackFailed => "rollback_failed",
            crate::mcp_platform::RedactedErrorCode::Cancelled => "cancelled",
            crate::mcp_platform::RedactedErrorCode::Interrupted => "interrupted",
            crate::mcp_platform::RedactedErrorCode::Unknown => "unknown",
        };
        format!("code={code},message={}", bounded_text(error.message()))
    }

    fn bounded_text(value: &str) -> String {
        const MAX_CHARS: usize = 240;
        let mut bounded = value.chars().take(MAX_CHARS).collect::<String>();
        if value.chars().count() > MAX_CHARS {
            bounded.push_str("...");
        }
        bounded
    }

    fn optional_i64(value: Option<i64>) -> String {
        value.map_or_else(|| "none".to_string(), |value| value.to_string())
    }

    async fn prepare_connection_test_remote_http_fixture(
        service: &McpPlatformService,
        repository: &Arc<SqliteMcpPlatformRepository>,
    ) -> (String, i64) {
        let mut manifest: serde_json::Value = serde_json::from_str(REMOTE).unwrap();
        manifest["id"] = serde_json::json!("org.example.connection-test-remote-http");
        manifest["version"] = serde_json::json!("1.0.0");
        manifest["name"] = serde_json::json!("Connection test remote HTTP fixture");
        manifest["description"] = serde_json::json!("Connection test remote HTTP fixture");
        manifest["permissions"] = serde_json::json!([{
            "id": "connection-test-network",
            "kind": "network",
            "reason": "Connect to the connection-test MCP service.",
            "required": true,
            "scope": "https://mcp.example.com"
        }]);
        manifest["auth"] = serde_json::json!({"type": "none"});
        manifest["transport"]["allowed_redirect_origins"] = serde_json::json!([]);
        let verified = parse_manifest(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(matches!(verified.manifest().auth, Auth::None));
        assert!(verified
            .manifest()
            .permissions
            .iter()
            .any(|permission| permission.kind == PermissionKind::Network
                && permission.scope.as_deref() == Some("https://mcp.example.com")));
        assert!(verified
            .manifest()
            .permissions
            .iter()
            .all(|permission| permission.kind != PermissionKind::Credentials));
        assert!(matches!(
            ConfigAuthRequirementResolver.requirement(&verified.manifest().auth),
            Ok(crate::mcp_platform::AuthRequirement::Ready)
        ));
        let manifest_digest = verified.digest().to_string();
        repository
            .save_manifest(&ManifestRecord {
                verified,
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Local,
                source_metadata: Default::default(),
                created_at_ms: 1,
            })
            .await
            .unwrap();

        let context = context();
        let mcp_id = "org.example.connection-test-remote-http";
        let review = service
            .plan_create(
                &context,
                PlanCreateInput {
                    intent: PlanIntent::Register {
                        manifest_digest,
                        installation_scope: InstallationScope::User,
                    },
                    idempotency_key: "connection-test-remote-http-plan".to_string(),
                },
            )
            .await
            .unwrap();
        let task = service
            .install_confirm(
                &context,
                InstallConfirmInput {
                    plan_id: review.plan_id.clone(),
                    plan_digest: review.plan_digest.clone(),
                    decision: UserDecision::Confirm,
                    idempotency_key: "connection-test-remote-http-task".to_string(),
                },
            )
            .await
            .unwrap();
        assert!(service.runner_tick().await.unwrap());
        let task_record = repository.get_task(&task.task_id).await.unwrap();
        assert_eq!(
            task_record.status,
            TaskStatus::Succeeded,
            "registration task failed: {}",
            connection_test_task_failure_diagnostic(&task_record)
        );

        let managed_mcp_id =
            crate::mcp_platform::task_runner::stable_managed_mcp_id(mcp_id, "user");
        let health = service
            .health_run(
                &context,
                HealthRunInput {
                    managed_mcp_id: managed_mcp_id.clone(),
                    mode: HealthCheckMode::Registration,
                    idempotency_key: "connection-test-remote-http-health".to_string(),
                },
            )
            .await
            .unwrap();
        assert!(service.runner_tick().await.unwrap());
        let health_task = repository.get_task(&health.task_id).await.unwrap();
        if health_task.status != TaskStatus::Succeeded {
            let diagnostic =
                connection_test_health_failure_diagnostic(repository, &health_task).await;
            panic!("registration-health task failed: {diagnostic}");
        }
        let inventory = repository
            .get_managed_inventory(&managed_mcp_id)
            .await
            .unwrap();
        assert_eq!(
            inventory.managed.state.installation,
            InstallationState::NotApplicable
        );
        assert_eq!(
            inventory.managed.state.registration,
            RegistrationState::Registered
        );
        assert_eq!(inventory.managed.state.health, HealthState::Healthy);
        assert!(!inventory.managed.state.default_enabled);
        let detail = service
            .managed_get(
                &context,
                ManagedGetInput {
                    managed_mcp_id: managed_mcp_id.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            detail.summary.credential_status,
            Some(CredentialStatus::Ready)
        );
        assert_eq!(
            detail.summary.eligibility.reason,
            crate::mcp_platform::service::ManagedEligibilityReason::RegistrationOnly
        );
        (managed_mcp_id, inventory.managed.revision)
    }

    struct UnusedScheduler;

    struct StaticGovernedSourceRefreshFetcher {
        snapshot: crate::mcp_platform::source_provisioning::FrozenSourceProvisioningSnapshot,
    }

    struct UnusedDistribution;

    #[async_trait]
    impl crate::mcp_platform::DistributionEffectAdapter for UnusedDistribution {
        fn adapter_version(&self) -> &'static str {
            "runtime-test"
        }

        async fn install(
            &self,
            _effect: &crate::mcp_platform::ManagedInstallEffect,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> crate::mcp_platform::McpPlatformResult<crate::mcp_platform::ManagedInstallOutcome>
        {
            unreachable!()
        }

        async fn inspect_installed(
            &self,
            _effect: &crate::mcp_platform::ManagedInstallEffect,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> crate::mcp_platform::McpPlatformResult<crate::mcp_platform::ManagedInstallOutcome>
        {
            unreachable!()
        }

        async fn version_exists(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
        ) -> crate::mcp_platform::McpPlatformResult<bool> {
            unreachable!()
        }

        async fn activate_version(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
            _task_id: &str,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> crate::mcp_platform::McpPlatformResult<()> {
            unreachable!()
        }

        async fn restore_activation(
            &self,
            _managed_mcp_id: &str,
            _previous_version: Option<&str>,
            _task_id: &str,
        ) -> crate::mcp_platform::McpPlatformResult<()> {
            unreachable!()
        }

        async fn active_version(
            &self,
            _managed_mcp_id: &str,
        ) -> crate::mcp_platform::McpPlatformResult<Option<String>> {
            unreachable!()
        }

        async fn remove_version(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> crate::mcp_platform::McpPlatformResult<()> {
            unreachable!()
        }

        async fn quarantine_version(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
            _task_id: &str,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> crate::mcp_platform::McpPlatformResult<String> {
            unreachable!()
        }

        async fn restore_quarantined(
            &self,
            _managed_mcp_id: &str,
            _version: &str,
            _token: &str,
        ) -> crate::mcp_platform::McpPlatformResult<()> {
            unreachable!()
        }

        async fn purge_quarantined(
            &self,
            _token: &str,
            _cancellation: &tokio_util::sync::CancellationToken,
        ) -> crate::mcp_platform::McpPlatformResult<()> {
            unreachable!()
        }
    }

    #[async_trait]
    impl GovernedSourceRefreshFetcher for StaticGovernedSourceRefreshFetcher {
        async fn fetch(
            &self,
            _endpoint: &str,
        ) -> crate::mcp_platform::McpPlatformResult<
            crate::mcp_platform::source_provisioning::FrozenSourceProvisioningSnapshot,
        > {
            Ok(self.snapshot.clone())
        }
    }

    #[async_trait]
    impl SchedulerTrait for UnusedScheduler {
        async fn add_scheduled_job(
            &self,
            _job: ScheduledJob,
            _copy_recipe: bool,
        ) -> Result<(), SchedulerError> {
            unreachable!()
        }

        async fn add_scheduled_job_from_content(
            &self,
            _job: ScheduledJob,
            _recipe_content: &[u8],
        ) -> Result<ScheduledJob, SchedulerError> {
            unreachable!()
        }

        async fn schedule_recipe(
            &self,
            _recipe_path: std::path::PathBuf,
            _cron_schedule: Option<String>,
        ) -> Result<(), SchedulerError> {
            unreachable!()
        }

        async fn list_scheduled_jobs(&self) -> Vec<ScheduledJob> {
            Vec::new()
        }

        async fn remove_scheduled_job(
            &self,
            _id: &str,
            _remove_recipe: bool,
        ) -> Result<(), SchedulerError> {
            unreachable!()
        }

        async fn pause_schedule(&self, _id: &str) -> Result<(), SchedulerError> {
            unreachable!()
        }

        async fn unpause_schedule(&self, _id: &str) -> Result<(), SchedulerError> {
            unreachable!()
        }

        async fn run_now(&self, _id: &str) -> Result<String, SchedulerError> {
            unreachable!()
        }

        async fn sessions(
            &self,
            _sched_id: &str,
            _limit: usize,
        ) -> Result<Vec<(String, Session)>, SchedulerError> {
            unreachable!()
        }

        async fn update_schedule(
            &self,
            _sched_id: &str,
            _new_cron: String,
        ) -> Result<(), SchedulerError> {
            unreachable!()
        }

        async fn kill_running_job(&self, _sched_id: &str) -> Result<(), SchedulerError> {
            unreachable!()
        }

        async fn get_running_job_info(
            &self,
            _sched_id: &str,
        ) -> Result<Option<(String, chrono::DateTime<chrono::Utc>)>, SchedulerError> {
            unreachable!()
        }
    }

    async fn agent(data_dir: &Path, config_dir: &Path) -> GooseAcpAgent {
        let config_file = config_dir.join("config.yaml");
        let secrets_file = config_dir.join("secrets.yaml");
        if !config_file.exists() {
            std::fs::write(&config_file, "").unwrap();
        }
        if !secrets_file.exists() {
            std::fs::write(&secrets_file, "").unwrap();
        }
        let _config = Arc::new(Config::new_with_file_secrets(&config_file, &secrets_file).unwrap());
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_url_with_integrity_signer(
                "sqlite::memory:",
                Arc::new(InMemoryIntegritySigner::new_for_testing([0x71; 32])),
            )
            .await
            .unwrap(),
        );
        let service = Arc::new(
            McpPlatformService::new_trusted(
                repository.clone(),
                Arc::new(SystemClock),
                Arc::new(UuidGenerator),
                McpPlatformServiceOptions::default(),
            )
            .with_enrollment_writer_owner(
                EnrollmentWriterOwner::in_memory_for_testing_default_time(),
            ),
        );
        let trusted_service_cell = Arc::new(tokio::sync::OnceCell::new());
        let _ = trusted_service_cell.set(service.clone());
        let provider_factory: AcpProviderFactory = Arc::new(|_, _, _| {
            Box::pin(async {
                Err(anyhow::anyhow!(
                    "provider is unused in governed import ACP boundary tests"
                ))
            })
        });
        GooseAcpAgent::new_with_trusted_mcp_platform_service_cell(
            GooseAcpAgentOptions {
                provider_factory,
                builtins: Vec::new(),
                data_dir: data_dir.to_path_buf(),
                config_dir: config_dir.to_path_buf(),
                disable_session_naming: true,
                goose_platform: GoosePlatform::GooseCli,
                additional_source_roots: Vec::new(),
                scheduler: Arc::new(UnusedScheduler),
                mcp_platform_service: Some(service),
                mcp_platform_service_cell: None,
            },
            Some(trusted_service_cell),
        )
        .await
        .unwrap()
    }

    async fn agent_with_service(
        data_dir: &Path,
        config_dir: &Path,
        service: Arc<McpPlatformService>,
    ) -> GooseAcpAgent {
        let config_file = config_dir.join("config.yaml");
        let secrets_file = config_dir.join("secrets.yaml");
        if !config_file.exists() {
            std::fs::write(&config_file, "").unwrap();
        }
        if !secrets_file.exists() {
            std::fs::write(&secrets_file, "").unwrap();
        }
        let _config = Arc::new(Config::new_with_file_secrets(&config_file, &secrets_file).unwrap());
        let trusted_service_cell = Arc::new(tokio::sync::OnceCell::new());
        let _ = trusted_service_cell.set(service.clone());
        let provider_factory: AcpProviderFactory = Arc::new(|_, _, _| {
            Box::pin(async {
                Err(anyhow::anyhow!(
                    "provider is unused in governed import ACP boundary tests"
                ))
            })
        });
        GooseAcpAgent::new_with_trusted_mcp_platform_service_cell(
            GooseAcpAgentOptions {
                provider_factory,
                builtins: Vec::new(),
                data_dir: data_dir.to_path_buf(),
                config_dir: config_dir.to_path_buf(),
                disable_session_naming: true,
                goose_platform: GoosePlatform::GooseCli,
                additional_source_roots: Vec::new(),
                scheduler: Arc::new(UnusedScheduler),
                mcp_platform_service: Some(service),
                mcp_platform_service_cell: None,
            },
            Some(trusted_service_cell),
        )
        .await
        .unwrap()
    }

    async fn agent_with_service_and_provider(
        data_dir: &Path,
        config_dir: &Path,
        service: Arc<McpPlatformService>,
        provider: Arc<dyn Provider>,
        factory_provider_ids: Arc<Mutex<Vec<String>>>,
    ) -> GooseAcpAgent {
        let config_file = config_dir.join("config.yaml");
        let secrets_file = config_dir.join("secrets.yaml");
        if !config_file.exists() {
            std::fs::write(&config_file, "").unwrap();
        }
        if !secrets_file.exists() {
            std::fs::write(&secrets_file, "").unwrap();
        }
        let _config = Arc::new(Config::new_with_file_secrets(&config_file, &secrets_file).unwrap());
        let trusted_service_cell = Arc::new(tokio::sync::OnceCell::new());
        let _ = trusted_service_cell.set(service.clone());
        let provider_factory: AcpProviderFactory = Arc::new(move |provider_id, _, _| {
            factory_provider_ids.lock().unwrap().push(provider_id);
            let provider = provider.clone();
            Box::pin(async move { Ok(provider) })
        });
        GooseAcpAgent::new_with_trusted_mcp_platform_service_cell(
            GooseAcpAgentOptions {
                provider_factory,
                builtins: Vec::new(),
                data_dir: data_dir.to_path_buf(),
                config_dir: config_dir.to_path_buf(),
                disable_session_naming: true,
                goose_platform: GoosePlatform::GooseCli,
                additional_source_roots: Vec::new(),
                scheduler: Arc::new(UnusedScheduler),
                mcp_platform_service: Some(service),
                mcp_platform_service_cell: None,
            },
            Some(trusted_service_cell),
        )
        .await
        .unwrap()
    }

    fn manifest_fixture_bytes(mcp_id: &str, version: &str) -> Vec<u8> {
        let mut manifest: serde_json::Value = serde_json::from_str(IMPORTABLE_MANIFEST).unwrap();
        manifest["id"] = serde_json::json!(mcp_id);
        manifest["version"] = serde_json::json!(version);
        manifest["name"] = serde_json::json!(format!("{mcp_id} {version}"));
        manifest["description"] = serde_json::json!("governed import fixture");
        serde_json::to_vec(&manifest).unwrap()
    }

    fn source_root_key(seed: u8) -> crate::verified_source_catalog::SourceRootKeyV1 {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(&signing.verifying_key().to_bytes());
        crate::verified_source_catalog::SourceRootKeyV1 {
            kid: format!("ed25519-spki-sha256:{}", sha256_hex_for_tests(&der)),
            spki_der_b64u: URL_SAFE_NO_PAD.encode(der),
        }
    }

    fn signed_source_directory_bytes(
        source_id: &str,
        source_name: &str,
        release_id: &str,
        mcp_id: &str,
        version: &str,
        seed: u8,
    ) -> Vec<u8> {
        signed_source_directory_bytes_at(
            source_id,
            source_name,
            release_id,
            mcp_id,
            version,
            seed,
            123,
        )
    }

    fn signed_source_directory_bytes_at(
        source_id: &str,
        source_name: &str,
        release_id: &str,
        mcp_id: &str,
        version: &str,
        seed: u8,
        issued_at_ms: i64,
    ) -> Vec<u8> {
        let current = source_root_key(seed);
        let payload = crate::verified_source_catalog::SourceUnsignedPayloadV1 {
            schema_version: 1,
            source_id: source_id.to_string(),
            source_name: source_name.to_string(),
            issued_at_ms,
            root: crate::verified_source_catalog::SourceTrustRootV1 {
                quorum: 1,
                keys: vec![current.clone()],
                previous: None,
            },
            snapshot: crate::verified_source_catalog::SourceSnapshotV1 {
                releases: vec![crate::verified_source_catalog::SourceReleaseV1 {
                    release_id: release_id.to_string(),
                    mcp_id: mcp_id.to_string(),
                    version: version.to_string(),
                    manifest_kind: "manifest".to_string(),
                    platform: "linux".to_string(),
                    architecture: "x86_64".to_string(),
                    variant: "gnu".to_string(),
                }],
                revocations: Vec::new(),
            },
        };
        let canonical_payload = payload.to_canonical_json_bytes();
        let signature = SigningKey::from_bytes(&[seed; 32]).sign(&canonical_payload);
        crate::verified_source_catalog::SourceSignedEnvelopeV1 {
            payload,
            signatures: vec![crate::verified_source_catalog::SourceSignatureV1 {
                kid: current.kid,
                sig_b64u: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            }],
        }
        .to_canonical_json_bytes()
    }

    fn source_provisioning_descriptor_bytes(
        directory_document_bytes: &[u8],
        seed: u8,
        endpoint: &str,
    ) -> Vec<u8> {
        let directory =
            crate::verified_source_catalog::parse_signed_envelope(directory_document_bytes)
                .unwrap();
        let payload = serde_json::json!({
            "format_version": crate::mcp_platform::source_provisioning::SOURCE_PROVISIONING_DESCRIPTOR_FORMAT,
            "refresh": {
                "endpoint": endpoint,
                "transport": "verified_source_bundle_v1",
            },
            "root_digest": directory.trust_anchor().root_digest,
            "source_document_canonical_digest": directory.digests.canonical_digest,
            "source_id": directory.envelope.payload.source_id,
        });
        let canonical_payload = serde_json::to_vec(&payload).unwrap();
        let root = source_root_key(seed);
        let signature = SigningKey::from_bytes(&[seed; 32]).sign(&canonical_payload);
        serde_json::to_vec(&serde_json::json!({
            "payload": payload,
            "signatures": [{
                "kid": root.kid,
                "sig_b64u": URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            }],
        }))
        .unwrap()
    }

    fn governed_source_refresh_snapshot(
        descriptor_bytes: Vec<u8>,
        directory_document_bytes: &[u8],
        manifests: Vec<(&str, Vec<u8>)>,
    ) -> crate::mcp_platform::source_provisioning::FrozenSourceProvisioningSnapshot {
        crate::mcp_platform::source_provisioning::FrozenSourceProvisioningSnapshot {
            descriptor_bytes,
            source_document_bytes: directory_document_bytes.to_vec(),
            manifests: manifests
                .into_iter()
                .map(|(release_id, document_bytes)| {
                    crate::mcp_platform::source_provisioning::FrozenSourceProvisioningManifest {
                        release_id: release_id.to_string(),
                        document_bytes,
                    }
                })
                .collect(),
        }
    }

    fn sha256_hex_for_tests(bytes: &[u8]) -> String {
        use sha2::{Digest as _, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in digest {
            out.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
            out.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
        }
        out
    }

    async fn managed_item_count(agent: &GooseAcpAgent) -> usize {
        let (context, service) = agent.mcp_platform_context_and_service_read_only().await;
        let service = service.expect("managed list service should be available");
        service
            .managed_list(
                &context,
                ManagedListInput {
                    page_size: Some(10),
                    ..ManagedListInput::default()
                },
            )
            .await
            .expect("managed list should succeed")
            .items
            .len()
    }

    async fn catalog_item_count(agent: &GooseAcpAgent) -> usize {
        let (context, service) = agent.mcp_platform_context_and_service_read_only().await;
        let service = service.expect("catalog list service should be available");
        service
            .catalog_list(
                &context,
                CatalogListInput {
                    page_size: Some(10),
                    ..CatalogListInput::default()
                },
            )
            .await
            .expect("catalog list should succeed")
            .items
            .len()
    }

    async fn catalog_page_via_transport(agent: &GooseAcpAgent) -> McpCatalogPage {
        let listed = agent
            .test_dispatch_transport_custom_request_without_authority(
                MCP_CATALOG_LIST_METHOD,
                serde_json::json!({
                    "pageSize": 10,
                }),
            )
            .await
            .expect("catalog list custom request should succeed");
        let listed: McpCatalogListResponse = serde_json::from_value(listed).unwrap();
        let McpPlatformOutcome::Success { value } = listed.outcome else {
            panic!("catalog list via formal ACP boundary should succeed");
        };
        value
    }

    async fn source_policy_via_transport(agent: &GooseAcpAgent) -> McpSourcesPolicyState {
        let listed = agent
            .test_dispatch_transport_custom_request_without_authority(
                MCP_SOURCES_POLICY_GET_METHOD,
                serde_json::json!({}),
            )
            .await
            .expect("source policy custom request should succeed");
        let listed: McpSourcesPolicyGetResponse = serde_json::from_value(listed).unwrap();
        let McpPlatformOutcome::Success { value } = listed.outcome else {
            panic!("source policy via formal ACP boundary should succeed");
        };
        value
    }

    async fn provision_source_via_transport(agent: &GooseAcpAgent, source_dir: &Path) -> String {
        let (_transport_session, authority) = test_transport_session_authority();
        let prepared = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority.clone(),
                MCP_SOURCE_PROVISION_PREPARE_METHOD,
                serde_json::json!({
                    "localDirectory": source_dir.to_string_lossy(),
                }),
            )
            .await
            .unwrap();
        let prepared: McpSourceProvisionPrepareResponse = serde_json::from_value(prepared).unwrap();
        let McpPlatformOutcome::Success { value: prepared } = prepared.outcome else {
            panic!("source provision prepare should succeed through ACP");
        };
        let source_id = prepared.preview.source_id.clone();
        let confirmed = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority,
                MCP_SOURCE_PROVISION_CONFIRM_METHOD,
                serde_json::json!({
                    "provisionId": prepared.provision_id,
                    "confirmationToken": prepared.confirmation_token,
                    "confirm": true,
                }),
            )
            .await
            .unwrap();
        let confirmed: McpSourceProvisionConfirmResponse =
            serde_json::from_value(confirmed).unwrap();
        let McpPlatformOutcome::Success { value: confirmed } = confirmed.outcome else {
            panic!("source provision confirm should succeed through ACP");
        };
        assert_eq!(confirmed.preview.source_id, source_id);
        format!("verified_source_catalog_{source_id}")
    }

    #[tokio::test]
    async fn source_provisioning_transport_prepares_and_confirms_a_frozen_local_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        let source_dir = temp.path().join("source-provisioning");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        let source_id = "source-provisioning-transport";
        let release_id = "release-source-provisioning-transport";
        let mcp_id = "pkg.source.provisioning.transport";
        let endpoint = "https://catalog.example.com/source-provisioning-transport.bundle";
        let directory_document = signed_source_directory_bytes(
            source_id,
            "Provisioning Transport Source",
            release_id,
            mcp_id,
            "1.0.0",
            111,
        );
        std::fs::write(source_dir.join("source.json"), &directory_document).unwrap();
        std::fs::write(
            source_dir.join("provisioning.json"),
            source_provisioning_descriptor_bytes(&directory_document, 111, endpoint),
        )
        .unwrap();
        std::fs::write(
            source_dir.join("manifest.json"),
            manifest_fixture_bytes(mcp_id, "1.0.0"),
        )
        .unwrap();
        let agent = agent(&data_dir, &config_dir).await;
        let (_transport_session, authority) = test_transport_session_authority();

        let prepared = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority.clone(),
                MCP_SOURCE_PROVISION_PREPARE_METHOD,
                serde_json::json!({
                    "localDirectory": source_dir.to_string_lossy(),
                }),
            )
            .await
            .unwrap();
        let prepared: McpSourceProvisionPrepareResponse = serde_json::from_value(prepared).unwrap();
        let McpPlatformOutcome::Success { value: prepared } = prepared.outcome else {
            panic!("source provision prepare should succeed through ACP");
        };
        assert_eq!(prepared.preview.source_id, source_id);
        assert_eq!(prepared.preview.manifest_count, 1);
        assert_eq!(prepared.preview.endpoint_host, "catalog.example.com");
        assert_eq!(catalog_item_count(&agent).await, 0);
        let provision_id = prepared.provision_id.clone();
        let confirmation_token = prepared.confirmation_token.clone();

        std::fs::write(
            source_dir.join("manifest.json"),
            manifest_fixture_bytes(mcp_id, "1.1.0"),
        )
        .unwrap();
        let confirmed = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority.clone(),
                MCP_SOURCE_PROVISION_CONFIRM_METHOD,
                serde_json::json!({
                    "provisionId": provision_id,
                    "confirmationToken": confirmation_token,
                    "confirm": true,
                }),
            )
            .await
            .unwrap();
        let confirmed: McpSourceProvisionConfirmResponse =
            serde_json::from_value(confirmed).unwrap();
        let McpPlatformOutcome::Success { value: confirmed } = confirmed.outcome else {
            panic!("source provision confirm should succeed through ACP");
        };
        assert_eq!(confirmed.preview.source_id, source_id);
        assert_eq!(confirmed.preview.manifest_count, 1);

        let catalog = catalog_page_via_transport(&agent).await;
        assert_eq!(catalog.items.len(), 1);
        assert_eq!(
            catalog.items[0].source_id,
            format!("verified_source_catalog_{source_id}")
        );
        assert_eq!(catalog.items[0].mcp_id, mcp_id);
        assert_eq!(catalog.items[0].version, "1.0.0");
        assert_eq!(managed_item_count(&agent).await, 0);

        let replay = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority,
                MCP_SOURCE_PROVISION_CONFIRM_METHOD,
                serde_json::json!({
                    "provisionId": prepared.provision_id,
                    "confirmationToken": prepared.confirmation_token,
                    "confirm": true,
                }),
            )
            .await
            .unwrap();
        let replay: McpSourceProvisionConfirmResponse = serde_json::from_value(replay).unwrap();
        let McpPlatformOutcome::Error { error } = replay.outcome else {
            panic!("source provision confirmation replay must fail closed");
        };
        assert_eq!(error.code, McpPlatformErrorCodeDto::PlanStale);
    }

    #[tokio::test]
    async fn source_provisioning_transport_confirmation_requires_the_preparing_transport_session() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        let source_dir = temp.path().join("source-provisioning-session-binding");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        let source_id = "source-provisioning-session-binding";
        let release_id = "release-source-provisioning-session-binding";
        let mcp_id = "pkg.source.provisioning.session-binding";
        let endpoint = "https://catalog.example.com/source-provisioning-session-binding.bundle";
        let directory_document = signed_source_directory_bytes(
            source_id,
            "Provisioning Session Binding Source",
            release_id,
            mcp_id,
            "1.0.0",
            146,
        );
        std::fs::write(source_dir.join("source.json"), &directory_document).unwrap();
        std::fs::write(
            source_dir.join("provisioning.json"),
            source_provisioning_descriptor_bytes(&directory_document, 146, endpoint),
        )
        .unwrap();
        std::fs::write(
            source_dir.join("manifest.json"),
            manifest_fixture_bytes(mcp_id, "1.0.0"),
        )
        .unwrap();
        let agent = agent(&data_dir, &config_dir).await;
        let (transport_session_a, authority_a) = test_transport_session_authority();
        let (_transport_session_b, authority_b) = test_transport_session_authority();

        let prepared = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority_a.clone(),
                MCP_SOURCE_PROVISION_PREPARE_METHOD,
                serde_json::json!({
                    "localDirectory": source_dir.to_string_lossy(),
                }),
            )
            .await
            .unwrap();
        let prepared: McpSourceProvisionPrepareResponse = serde_json::from_value(prepared).unwrap();
        let McpPlatformOutcome::Success { value: prepared } = prepared.outcome else {
            panic!("source provision prepare should succeed through ACP");
        };

        let cross_session = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority_b,
                MCP_SOURCE_PROVISION_CONFIRM_METHOD,
                serde_json::json!({
                    "provisionId": prepared.provision_id.clone(),
                    "confirmationToken": prepared.confirmation_token.clone(),
                    "confirm": true,
                }),
            )
            .await
            .unwrap();
        let cross_session: McpSourceProvisionConfirmResponse =
            serde_json::from_value(cross_session).unwrap();
        let McpPlatformOutcome::Error { error } = cross_session.outcome else {
            panic!("another transport session must not confirm the provision");
        };
        assert_eq!(error.code, McpPlatformErrorCodeDto::PlanStale);
        assert_eq!(catalog_item_count(&agent).await, 0);

        let confirmed = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority_a.clone(),
                MCP_SOURCE_PROVISION_CONFIRM_METHOD,
                serde_json::json!({
                    "provisionId": prepared.provision_id.clone(),
                    "confirmationToken": prepared.confirmation_token.clone(),
                    "confirm": true,
                }),
            )
            .await
            .unwrap();
        let confirmed: McpSourceProvisionConfirmResponse =
            serde_json::from_value(confirmed).unwrap();
        assert!(matches!(
            confirmed.outcome,
            McpPlatformOutcome::Success { .. }
        ));
        assert_eq!(catalog_item_count(&agent).await, 1);

        let replay = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority_a,
                MCP_SOURCE_PROVISION_CONFIRM_METHOD,
                serde_json::json!({
                    "provisionId": prepared.provision_id,
                    "confirmationToken": prepared.confirmation_token,
                    "confirm": true,
                }),
            )
            .await
            .unwrap();
        let replay: McpSourceProvisionConfirmResponse = serde_json::from_value(replay).unwrap();
        let McpPlatformOutcome::Error { error } = replay.outcome else {
            panic!("source provision confirmation replay must fail closed");
        };
        assert_eq!(error.code, McpPlatformErrorCodeDto::PlanStale);
        drop(transport_session_a);
    }

    #[tokio::test]
    async fn governed_import_transport_imports_local_manifest_without_installing_any_mcp() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let manifest_path = temp.path().join("manifest.json");
        std::fs::write(
            &manifest_path,
            manifest_fixture_bytes("pkg.local.import", "1.0.0"),
        )
        .unwrap();
        let agent = agent(&data_dir, &config_dir).await;

        let imported = agent
            .test_dispatch_transport_custom_request(
                MCP_GOVERNED_IMPORT_METHOD,
                serde_json::json!({
                    "source": {
                        "type": "local_manifest",
                        "filePath": manifest_path.to_string_lossy(),
                    }
                }),
            )
            .await
            .unwrap();
        let imported: McpGovernedImportResponse = serde_json::from_value(imported).unwrap();
        let McpPlatformOutcome::Success { value } = imported.outcome else {
            panic!("local manifest import should succeed");
        };
        assert_eq!(
            value.source.import_kind,
            McpGovernedImportKind::LocalPersistence
        );
        assert_eq!(
            value.document.document_kind,
            McpGovernedCatalogDocumentKind::Manifest
        );
        assert_eq!(value.entries.len(), 1);
        assert_eq!(catalog_item_count(&agent).await, 1);
        assert_eq!(managed_item_count(&agent).await, 0);
    }

    #[tokio::test]
    async fn governed_import_transport_imports_signed_local_directory() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        let source_dir = temp.path().join("source-alpha");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(
            source_dir.join("source-alpha.json"),
            signed_source_directory_bytes(
                "source-alpha",
                "Alpha Source",
                "release-a",
                "pkg.catalog.alpha",
                "1.0.0",
                7,
            ),
        )
        .unwrap();
        std::fs::write(
            source_dir.join("pkg.catalog.alpha.json"),
            manifest_fixture_bytes("pkg.catalog.alpha", "1.0.0"),
        )
        .unwrap();
        let agent = agent(&data_dir, &config_dir).await;

        let imported = agent
            .test_dispatch_transport_custom_request(
                MCP_GOVERNED_IMPORT_METHOD,
                serde_json::json!({
                    "source": {
                        "type": "local_directory",
                        "directoryPath": source_dir.to_string_lossy(),
                    }
                }),
            )
            .await
            .unwrap();
        let imported: McpGovernedImportResponse = serde_json::from_value(imported).unwrap();
        let McpPlatformOutcome::Success { value } = imported.outcome else {
            panic!("local directory import should succeed");
        };
        assert_eq!(
            value.source.import_kind,
            McpGovernedImportKind::VerifiedSourceCatalog
        );
        assert_eq!(
            value.document.document_kind,
            McpGovernedCatalogDocumentKind::Directory
        );
        assert_eq!(value.entries.len(), 1);
        assert_eq!(catalog_item_count(&agent).await, 1);
        assert_eq!(managed_item_count(&agent).await, 0);
    }

    #[tokio::test]
    async fn governed_import_transport_rejects_refresh_anchored_local_directory_reimport_without_mutation(
    ) {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        let source_dir = temp.path().join("source-anchor-guard");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();

        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_url_with_integrity_signer(
                "sqlite::memory:",
                Arc::new(InMemoryIntegritySigner::new_for_testing([0x86; 32])),
            )
            .await
            .unwrap(),
        );
        let service = Arc::new(
            McpPlatformService::new_trusted(
                repository.clone(),
                Arc::new(SystemClock),
                Arc::new(UuidGenerator),
                McpPlatformServiceOptions::default(),
            )
            .with_enrollment_writer_owner(
                EnrollmentWriterOwner::in_memory_for_testing_default_time(),
            ),
        );
        let agent = agent_with_service(&data_dir, &config_dir, service).await;
        let source_id = "source-anchor-guard";
        let release_id = "release-anchor-guard";
        let mcp_id = "pkg.anchor.guard";
        let initial_directory = signed_source_directory_bytes(
            source_id,
            "Anchor Guard Source",
            release_id,
            mcp_id,
            "1.0.0",
            86,
        );
        let initial_manifest = manifest_fixture_bytes(mcp_id, "1.0.0");
        std::fs::write(source_dir.join("source.json"), &initial_directory).unwrap();
        std::fs::write(
            source_dir.join("provisioning.json"),
            source_provisioning_descriptor_bytes(
                &initial_directory,
                86,
                "https://catalog.example.com/source-anchor-guard.bundle",
            ),
        )
        .unwrap();
        std::fs::write(source_dir.join("manifest.json"), &initial_manifest).unwrap();

        let governed_source_id = provision_source_via_transport(&agent, &source_dir).await;
        let anchor_before = repository
            .get_governed_source_refresh_trust_anchor(&governed_source_id)
            .await
            .unwrap();
        let document_before = repository
            .source_catalog_document_provenance(source_id)
            .await
            .unwrap();
        let entries_before = repository
            .list_governed_catalog_entries()
            .await
            .unwrap()
            .into_iter()
            .map(|entry| {
                (
                    entry.source.source_id,
                    entry.document.document_id,
                    entry.document.document_digest,
                    entry.manifest.verified.digest().to_string(),
                )
            })
            .collect::<Vec<_>>();
        let audits_before = repository
            .list_governed_source_refresh_audits(&governed_source_id)
            .await
            .unwrap();

        std::fs::write(
            source_dir.join("source.json"),
            signed_source_directory_bytes(
                source_id,
                "Anchor Guard Source",
                release_id,
                mcp_id,
                "1.1.0",
                87,
            ),
        )
        .unwrap();
        std::fs::write(
            source_dir.join("manifest.json"),
            manifest_fixture_bytes(mcp_id, "1.1.0"),
        )
        .unwrap();

        let rejected = agent
            .test_dispatch_transport_custom_request(
                MCP_GOVERNED_IMPORT_METHOD,
                serde_json::json!({
                    "source": {
                        "type": "local_directory",
                        "directoryPath": source_dir.to_string_lossy(),
                    }
                }),
            )
            .await
            .unwrap();
        let rejected: McpGovernedImportResponse = serde_json::from_value(rejected).unwrap();
        let McpPlatformOutcome::Error { error } = rejected.outcome else {
            panic!("refresh-anchored local reimport must fail");
        };
        assert_eq!(error.code, McpPlatformErrorCodeDto::OperationNotSupported);

        assert_eq!(
            repository
                .get_governed_source_refresh_trust_anchor(&governed_source_id)
                .await
                .unwrap(),
            anchor_before
        );
        assert_eq!(
            repository
                .source_catalog_document_provenance(source_id)
                .await
                .unwrap(),
            document_before
        );
        assert_eq!(
            repository
                .list_governed_catalog_entries()
                .await
                .unwrap()
                .into_iter()
                .map(|entry| {
                    (
                        entry.source.source_id,
                        entry.document.document_id,
                        entry.document.document_digest,
                        entry.manifest.verified.digest().to_string(),
                    )
                })
                .collect::<Vec<_>>(),
            entries_before
        );
        assert_eq!(
            repository
                .list_governed_source_refresh_audits(&governed_source_id)
                .await
                .unwrap(),
            audits_before
        );
        let listed = catalog_page_via_transport(&agent).await;
        assert_eq!(listed.items.len(), 1);
        assert_eq!(listed.items[0].version, "1.0.0");
    }

    #[tokio::test]
    async fn governed_import_transport_refreshes_catalog_through_formal_acp_list_readback() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let manifest_path = temp.path().join("manifest.json");
        std::fs::write(
            &manifest_path,
            manifest_fixture_bytes("pkg.local.import", "1.0.0"),
        )
        .unwrap();
        let agent = agent(&data_dir, &config_dir).await;

        let imported = agent
            .test_dispatch_transport_custom_request(
                MCP_GOVERNED_IMPORT_METHOD,
                serde_json::json!({
                    "source": {
                        "type": "local_manifest",
                        "filePath": manifest_path.to_string_lossy(),
                    }
                }),
            )
            .await
            .unwrap();
        let imported: McpGovernedImportResponse = serde_json::from_value(imported).unwrap();
        let McpPlatformOutcome::Success { value } = imported.outcome else {
            panic!("local manifest import should succeed");
        };
        assert_eq!(value.entries.len(), 1);

        let listed = catalog_page_via_transport(&agent).await;
        assert_eq!(listed.items.len(), 1);
        assert_eq!(listed.items[0].source_id, value.source.source_id);
        assert_eq!(listed.items[0].mcp_id, "pkg.local.import");
        assert_eq!(listed.items[0].version, "1.0.0");
        assert_eq!(
            listed.items[0].manifest_digest,
            value.entries[0].manifest_digest
        );
        assert_eq!(managed_item_count(&agent).await, 0);
    }

    #[tokio::test]
    async fn governed_source_refresh_transport_updates_catalog_without_installing_any_mcp() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        let source_dir = temp.path().join("source-refresh-transport");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_url_with_integrity_signer(
                "sqlite::memory:",
                Arc::new(InMemoryIntegritySigner::new_for_testing([0x71; 32])),
            )
            .await
            .unwrap(),
        );
        let source_id = "source-refresh-transport";
        let source_display_name = "Refresh Transport Source";
        let release_id = "release-refresh-transport";
        let mcp_id = "pkg.refresh.transport";
        let initial_manifest = manifest_fixture_bytes(mcp_id, "1.0.0");
        let refreshed_manifest = manifest_fixture_bytes(mcp_id, "1.1.0");
        let initial_directory = signed_source_directory_bytes(
            source_id,
            source_display_name,
            release_id,
            mcp_id,
            "1.0.0",
            11,
        );
        let refreshed_directory = signed_source_directory_bytes_at(
            source_id,
            source_display_name,
            release_id,
            mcp_id,
            "1.1.0",
            11,
            124,
        );
        let refreshed_descriptor = source_provisioning_descriptor_bytes(
            &refreshed_directory,
            11,
            "https://catalog.example.com/source-refresh-transport.bundle",
        );
        let refresh_snapshot = governed_source_refresh_snapshot(
            refreshed_descriptor,
            &refreshed_directory,
            vec![(release_id, refreshed_manifest)],
        );
        let service = Arc::new(
            McpPlatformService::new_trusted(
                repository.clone(),
                Arc::new(SystemClock),
                Arc::new(UuidGenerator),
                McpPlatformServiceOptions::default(),
            )
            .with_enrollment_writer_owner(
                EnrollmentWriterOwner::in_memory_for_testing_default_time(),
            )
            .with_governed_source_refresh_fetcher(Arc::new(
                StaticGovernedSourceRefreshFetcher {
                    snapshot: refresh_snapshot,
                },
            )),
        );
        std::fs::write(source_dir.join("source.json"), &initial_directory).unwrap();
        std::fs::write(
            source_dir.join("provisioning.json"),
            source_provisioning_descriptor_bytes(
                &initial_directory,
                11,
                "https://catalog.example.com/source-refresh-transport.bundle",
            ),
        )
        .unwrap();
        std::fs::write(source_dir.join("manifest.json"), &initial_manifest).unwrap();
        let agent = agent_with_service(&data_dir, &config_dir, service.clone()).await;
        let imported_source_id = provision_source_via_transport(&agent, &source_dir).await;

        let refreshed = agent
            .test_dispatch_transport_custom_request(
                MCP_SOURCE_REFRESH_METHOD,
                serde_json::json!({
                    "sourceId": imported_source_id,
                }),
            )
            .await
            .unwrap();
        let refreshed: McpSourceRefreshResponse = serde_json::from_value(refreshed).unwrap();
        let McpPlatformOutcome::Success { value } = refreshed.outcome else {
            panic!("source refresh should succeed");
        };
        assert_eq!(value.manifest_count, 1);
        assert_eq!(value.display_name, source_display_name);

        let listed = catalog_page_via_transport(&agent).await;
        assert_eq!(listed.items.len(), 1);
        assert_eq!(listed.items[0].source_id, imported_source_id);
        assert_eq!(listed.items[0].mcp_id, mcp_id);
        assert_eq!(listed.items[0].version, "1.1.0");

        let policy = source_policy_via_transport(&agent).await;
        let source = policy
            .sources
            .into_iter()
            .find(|source| source.source_id == imported_source_id)
            .unwrap();
        assert_eq!(source.display_name, source_display_name);
        assert_eq!(source.cache.refresh_state, McpRefreshState::Idle);
        assert_eq!(
            source.last_refresh_document_digest.as_deref(),
            Some(value.document_digest.as_str())
        );
        assert_eq!(managed_item_count(&agent).await, 0);
    }

    #[tokio::test]
    async fn governed_import_fail_closed_rejects_empty_remote_and_unauthorized_requests() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let manifest_path = temp.path().join("manifest.json");
        std::fs::write(
            &manifest_path,
            manifest_fixture_bytes("pkg.local.import", "1.0.0"),
        )
        .unwrap();
        let agent = agent(&data_dir, &config_dir).await;

        let empty = agent
            .test_dispatch_transport_custom_request(
                MCP_GOVERNED_IMPORT_METHOD,
                serde_json::json!({
                    "source": {
                        "type": "local_manifest",
                        "filePath": "",
                    }
                }),
            )
            .await
            .unwrap();
        let empty: McpGovernedImportResponse = serde_json::from_value(empty).unwrap();
        let McpPlatformOutcome::Error { error } = empty.outcome else {
            panic!("empty local import must fail closed");
        };
        assert_eq!(error.code, McpPlatformErrorCodeDto::InvalidRequest);

        let remote = agent
            .test_dispatch_transport_custom_request(
                MCP_GOVERNED_IMPORT_METHOD,
                serde_json::json!({
                    "source": {
                        "type": "local_manifest",
                        "filePath": "https://catalog.example.test/pkg.json",
                    }
                }),
            )
            .await
            .unwrap();
        let remote: McpGovernedImportResponse = serde_json::from_value(remote).unwrap();
        let McpPlatformOutcome::Error { error } = remote.outcome else {
            panic!("remote descriptor must fail closed");
        };
        assert_eq!(error.code, McpPlatformErrorCodeDto::InvalidRequest);

        let unauthorized = agent
            .dispatch_custom_request(
                MCP_GOVERNED_IMPORT_METHOD,
                serde_json::json!({
                    "source": {
                        "type": "local_manifest",
                        "filePath": manifest_path.to_string_lossy(),
                    }
                }),
            )
            .await
            .unwrap();
        let unauthorized: McpGovernedImportResponse = serde_json::from_value(unauthorized).unwrap();
        let McpPlatformOutcome::Error { error } = unauthorized.outcome else {
            panic!("raw ACP dispatch must not gain write authority");
        };
        assert_eq!(error.code, McpPlatformErrorCodeDto::PolicyDenied);
        assert_eq!(catalog_item_count(&agent).await, 0);
        assert_eq!(managed_item_count(&agent).await, 0);
    }

    #[tokio::test]
    async fn https_plan_review_requires_the_transport_session_and_redacts_handler_errors() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let agent = agent(&data_dir, &config_dir).await;
        let params = serde_json::json!({
            "provisionId": "provision-secret",
            "expectedManifestDigest": "digest-secret",
            "idempotencyKey": "idempotency-secret",
            "authority": "body-forged-authority",
            "sourceUrl": "https://source-secret.invalid/manifest.json?query=raw-url-secret",
            "confirmationToken": "token-secret",
            "actorId": "actor-secret",
            "sessionId": "session-secret",
            "planId": "plan-secret",
            "proof": "proof-secret",
            "publisher": "publisher-secret",
            "rawBytes": "raw-bytes-secret",
            "internalError": "internal-error-sentinel",
        });
        let handler_params = serde_json::json!({
            "provisionId": "provision-secret",
            "expectedManifestDigest": "digest-secret",
            "idempotencyKey": "idempotency-secret",
        });

        let missing = agent
            .dispatch_custom_request(MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD, params.clone())
            .await
            .expect_err("raw ACP dispatch must not acquire HTTPS write authority");
        assert_eq!(
            missing.code,
            agent_client_protocol::ErrorCode::MethodNotFound
        );
        let missing_wire = serde_json::to_string(&missing).unwrap();
        for secret in [
            "provision-secret",
            "digest-secret",
            "idempotency-secret",
            "body-forged-authority",
            "https://source-secret.invalid/manifest.json?query=raw-url-secret",
            "token-secret",
            "actor-secret",
            "session-secret",
            "plan-secret",
            "proof-secret",
            "publisher-secret",
            "raw-bytes-secret",
            "internal-error-sentinel",
        ] {
            assert!(!missing_wire.contains(secret));
        }

        let (_session_a, authority_a) = test_transport_session_authority();
        let (_session_b, authority_b) = test_transport_session_authority();
        let wrong_session = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority_b,
                MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD,
                handler_params.clone(),
            )
            .await
            .expect("transport dispatch should return an ACP response");
        let wrong_session_wire = serde_json::to_string(&wrong_session).unwrap();
        let wrong_session: McpHttpsProvisionPlanCreateResponse =
            serde_json::from_value(wrong_session).unwrap();
        let McpPlatformOutcome::Error { error } = wrong_session.outcome else {
            panic!("wrong transport session must fail through the handler");
        };
        assert_eq!(error.code, McpPlatformErrorCodeDto::InvalidRequest);
        for secret in [
            "source-secret.invalid",
            "raw-url-secret",
            "token-secret",
            "actor-secret",
            "session-secret",
            "plan-secret",
            "digest-secret",
            "proof-secret",
            "publisher-secret",
            "raw-bytes-secret",
            "internal-error-sentinel",
        ] {
            assert!(!wrong_session_wire.contains(secret));
        }

        let handler_error = agent
            .test_dispatch_transport_custom_request_with_authority(
                authority_a,
                MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD,
                handler_params,
            )
            .await
            .expect("handler errors must remain ACP responses");
        let handler_error_wire = serde_json::to_string(&handler_error).unwrap();
        let handler_error: McpHttpsProvisionPlanCreateResponse =
            serde_json::from_value(handler_error).unwrap();
        let McpPlatformOutcome::Error { error } = handler_error.outcome else {
            panic!("missing provision must fail through the real handler");
        };
        assert_eq!(error.code, McpPlatformErrorCodeDto::InvalidRequest);
        for secret in [
            "source-secret.invalid",
            "raw-url-secret",
            "token-secret",
            "provision-secret",
            "digest-secret",
            "idempotency-secret",
            "body-forged-authority",
            "actor-secret",
            "session-secret",
            "plan-secret",
            "proof-secret",
            "publisher-secret",
            "raw-bytes-secret",
            "internal-error-sentinel",
        ] {
            assert!(!handler_error_wire.contains(secret));
        }
        assert!(!handler_error_wire.contains("internal error"));
    }

    struct HttpsPlanFixture {
        _temp: tempfile::TempDir,
        _session: TransportWriteSession,
        repository: Arc<SqliteMcpPlatformRepository>,
        agent: GooseAcpAgent,
        authority: TransportSessionMcpWriteAuthority,
        binding: String,
        requested_url: String,
        final_url: String,
        parsed_digest: String,
        token: &'static str,
        token_hash: String,
        wrong_actor_token: &'static str,
        wrong_actor_token_hash: String,
        dns_evidence_digest: String,
        requested_url_id: String,
        final_url_id: String,
        redirect_chain_digest: String,
        raw_digest: String,
        raw_manifest_sentinel: String,
        consumed_record: HttpsManifestProvisionRecord,
        wrong_actor_consumed_record: HttpsManifestProvisionRecord,
    }

    async fn setup_https_plan_fixture() -> HttpsPlanFixture {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("agent-data");
        let config_dir = temp.path().join("agent-config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_url_with_integrity_signer(
                "sqlite::memory:",
                Arc::new(InMemoryIntegritySigner::new_for_testing([0x72; 32])),
            )
            .await
            .unwrap(),
        );
        let service = Arc::new(
            McpPlatformService::new_trusted(
                repository.clone(),
                Arc::new(SystemClock),
                Arc::new(UuidGenerator),
                McpPlatformServiceOptions::default(),
            )
            .with_enrollment_writer_owner(
                EnrollmentWriterOwner::in_memory_for_testing_default_time(),
            ),
        );
        let agent = agent_with_service(&data_dir, &config_dir, service).await;
        let (_session, authority) = test_transport_session_authority();
        let binding = authority.binding().to_string();
        let actor = "local_authenticated_client";
        let requested = crate::mcp_platform::https_manifest_policy::ValidatedHttpsUrl::parse(
            "https://catalog.example.test/frozen-manifest.json",
        )
        .unwrap();
        let final_url = crate::mcp_platform::https_manifest_policy::ValidatedHttpsUrl::parse(
            "https://cdn.example.test/frozen-manifest.json",
        )
        .unwrap();
        let requested_url = requested.request_url().as_str().to_string();
        let final_url_string = final_url.request_url().as_str().to_string();
        let mut frozen_manifest: serde_json::Value =
            serde_json::from_slice(&manifest_fixture_bytes("pkg.https.dispatch", "1.0.0")).unwrap();
        frozen_manifest["description"] = serde_json::json!("https-plan-raw-only-sentinel");
        let frozen_bytes = serde_json::to_vec(&frozen_manifest).unwrap();
        assert!(String::from_utf8_lossy(&frozen_bytes).contains("https-plan-raw-only-sentinel"));
        let parsed = parse_manifest(&frozen_bytes).unwrap();
        let token = "https-plan-confirmation-token";
        let token_hash = sha256_hex_for_tests(token.as_bytes());
        let wrong_actor_token = "https-plan-wrong-actor-confirmation-token";
        let wrong_actor_token_hash = sha256_hex_for_tests(wrong_actor_token.as_bytes());
        let raw_digest = sha256_hex_for_tests(&frozen_bytes);
        let parsed_digest = parsed.digest().to_string();
        repository
            .save_manifest(&ManifestRecord {
                verified: parsed.clone(),
                proof: ManifestProof::LocalBytes,
                trust_tier: TrustTier::Local,
                source_metadata: ManifestSourceMetadata {
                    source_ref: SourceRef::HttpsManifestUrl {
                        manifest_url: requested_url.clone(),
                    },
                    import_kind: SourceImportKind::HttpsManifestUrl,
                    ..ManifestSourceMetadata::default()
                },
                created_at_ms: 1,
            })
            .await
            .unwrap();
        let requested_url_id = crate::utils::bytes_to_hex(requested.request_identity_digest());
        let final_url_id = crate::utils::bytes_to_hex(final_url.request_identity_digest());
        let redirect_chain_digest = sha256_hex_for_tests(
            crate::mcp_platform::https_manifest_policy::redirect_chain_evidence_digest_input(&[
                (requested.clone(), 302),
                (final_url.clone(), 200),
            ])
            .as_bytes(),
        );
        let dns_addresses = ["8.8.8.8".parse().unwrap(), "1.1.1.1".parse().unwrap()];
        let dns_evidence_digest = crate::utils::bytes_to_hex(
            crate::mcp_platform::https_manifest_fetcher::digest_addresses_for_test(&dns_addresses),
        );
        let record = HttpsManifestProvisionRecord {
            provision_id: "https-provision-real-chain".to_string(),
            actor: actor.to_string(),
            transport_session_binding: binding.clone(),
            requested_url: Some(requested_url.clone()),
            requested_url_id,
            final_url_id,
            raw_digest,
            parsed_digest: parsed_digest.clone(),
            redirect_chain_digest,
            dns_evidence_digest: Some(dns_evidence_digest.clone()),
            frozen_bytes,
            expires_at_ms: 9_999_999_999,
            status: "saved".to_string(),
            created_at_ms: 1,
        };
        let mut wrong_actor_record = record.clone();
        wrong_actor_record.provision_id = "https-provision-wrong-actor".to_string();
        wrong_actor_record.actor = "different-actor".to_string();
        repository
            .save_https_manifest_provision(record.clone(), &token_hash)
            .await
            .unwrap();
        repository
            .save_https_manifest_provision(wrong_actor_record, &wrong_actor_token_hash)
            .await
            .unwrap();
        repository
            .claim_https_manifest_provision(
                "https-provision-wrong-actor",
                &wrong_actor_token_hash,
                "different-actor",
                &binding,
                2,
            )
            .await
            .unwrap();
        repository
            .consume_https_manifest_provision(
                "https-provision-wrong-actor",
                &wrong_actor_token_hash,
                "different-actor",
                &binding,
                3,
            )
            .await
            .unwrap();
        repository
            .claim_https_manifest_provision(
                "https-provision-real-chain",
                &token_hash,
                actor,
                &binding,
                2,
            )
            .await
            .unwrap();
        repository
            .consume_https_manifest_provision(
                "https-provision-real-chain",
                &token_hash,
                actor,
                &binding,
                3,
            )
            .await
            .unwrap();
        let consumed_record = repository
            .get_consumed_https_manifest_provision("https-provision-real-chain", actor, &binding)
            .await
            .unwrap();
        let wrong_actor_consumed_record = repository
            .get_consumed_https_manifest_provision(
                "https-provision-wrong-actor",
                "different-actor",
                &binding,
            )
            .await
            .unwrap();
        HttpsPlanFixture {
            _temp: temp,
            _session: _session,
            repository,
            agent,
            authority,
            binding,
            requested_url,
            final_url: final_url_string,
            parsed_digest,
            token,
            token_hash,
            wrong_actor_token,
            wrong_actor_token_hash,
            dns_evidence_digest,
            requested_url_id: record.requested_url_id,
            final_url_id: record.final_url_id,
            redirect_chain_digest: record.redirect_chain_digest,
            raw_digest: record.raw_digest,
            raw_manifest_sentinel: "https-plan-raw-only-sentinel".to_string(),
            consumed_record,
            wrong_actor_consumed_record,
        }
    }

    struct HttpsPlanRouteHarness {
        _temp: tempfile::TempDir,
        _session: TransportWriteSession,
        agent: GooseAcpAgent,
        authority: TransportSessionMcpWriteAuthority,
        parsed_digest: String,
        expected_mcp_id: &'static str,
        expected_name: &'static str,
        expected_version: &'static str,
    }

    async fn setup_https_plan_route_harness() -> HttpsPlanRouteHarness {
        let fixture = setup_https_plan_fixture().await;
        let HttpsPlanFixture {
            _temp,
            _session,
            agent,
            authority,
            parsed_digest,
            ..
        } = fixture;
        HttpsPlanRouteHarness {
            _temp,
            _session,
            agent,
            authority,
            parsed_digest,
            expected_mcp_id: "pkg.https.dispatch",
            expected_name: "pkg.https.dispatch 1.0.0",
            expected_version: "1.0.0",
        }
    }

    fn dispatch_https_plan_create_route(
        agent: GooseAcpAgent,
        authority: TransportSessionMcpWriteAuthority,
        parsed_digest: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = serde_json::Value> + Send>> {
        Box::pin(async move {
            let response = agent
                .test_dispatch_transport_custom_request_with_authority(
                    authority,
                    MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD,
                    serde_json::json!({
                        "provisionId": "https-provision-real-chain",
                        "expectedManifestDigest": parsed_digest,
                        "idempotencyKey": "https-plan-idempotency",
                    }),
                )
                .await
                .unwrap();
            response
        })
    }

    #[tokio::test]
    async fn https_plan_create_wire_rejects_forged_authority_without_creating_plan() {
        let fixture = setup_https_plan_fixture().await;
        let idempotency_key = "https-plan-forged-authority";
        let error = fixture
            .agent
            .test_dispatch_transport_custom_request_with_authority(
                fixture.authority,
                MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD,
                serde_json::json!({
                    "provisionId": "https-provision-real-chain",
                    "expectedManifestDigest": fixture.parsed_digest,
                    "idempotencyKey": idempotency_key,
                    "authority": "body-forged-authority",
                }),
            )
            .await
            .expect_err("unknown wire fields must be rejected before the plan handler runs");
        let error_wire = serde_json::to_value(&error).unwrap();
        assert_eq!(error_wire["code"], serde_json::json!(-32603));
        assert_eq!(
            error_wire["data"],
            serde_json::json!("acp_custom_dispatch_failed")
        );
        let serialized_error = serde_json::to_string(&error).unwrap();
        for secret in [
            "body-forged-authority",
            "authority",
            "deny_unknown_fields",
            "https-provision-real-chain",
            "digest",
            "idempotency",
        ] {
            assert!(!serialized_error.contains(secret), "leaked {secret}");
        }
        assert!(fixture
            .repository
            .get_plan_by_idempotency_key(idempotency_key)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn https_plan_create_uses_confirmed_provision_binding_without_leaks() {
        let HttpsPlanRouteHarness {
            _temp,
            _session,
            agent,
            authority,
            parsed_digest,
            expected_mcp_id,
            expected_name,
            expected_version,
        } = setup_https_plan_route_harness().await;
        let planned =
            dispatch_https_plan_create_route(agent, authority, parsed_digest.clone()).await;
        let value = planned["outcome"]["value"].as_object().unwrap();
        assert_eq!(value.len(), 6);
        assert_eq!(
            value.get("mcpId").and_then(|v| v.as_str()),
            Some(expected_mcp_id)
        );
        assert_eq!(
            value.get("name").and_then(|v| v.as_str()),
            Some(expected_name)
        );
        assert_eq!(
            value.get("version").and_then(|v| v.as_str()),
            Some(expected_version)
        );
        assert_eq!(
            value.get("selectedManifestDigest").and_then(|v| v.as_str()),
            Some(parsed_digest.as_str())
        );
        assert!(value
            .get("planId")
            .and_then(|v| v.as_str())
            .is_some_and(|plan_id| !plan_id.is_empty()));
        assert!(value
            .get("planDigest")
            .and_then(|v| v.as_str())
            .is_some_and(|plan_digest| !plan_digest.is_empty()));
        assert!(value.keys().all(|key| matches!(
            key.as_str(),
            "mcpId" | "name" | "version" | "selectedManifestDigest" | "planId" | "planDigest"
        )));
    }

    #[tokio::test]
    async fn https_plan_review_direct_service_preserves_consumed_provision_binding() {
        let fixture = setup_https_plan_fixture().await;
        let (context, service) = with_transport_mcp_platform_write_authority(
            Some(fixture.authority.clone()),
            fixture.agent.mcp_platform_context_and_service(),
        )
        .await;
        let review = service
            .unwrap()
            .https_manifest_plan_review(
                &context,
                HttpsManifestPlanReviewInput {
                    provision_id: "https-provision-real-chain".to_string(),
                    expected_manifest_digest: fixture.parsed_digest.clone(),
                    idempotency_key: "https-plan-direct-service".to_string(),
                },
            )
            .await
            .unwrap();
        assert_eq!(review.plan.manifest_digest(), fixture.parsed_digest);
        assert_eq!(
            review.plan.source_context(),
            review.target.source_context.as_ref()
        );
        assert!(
            matches!(review.plan.source_context(), Some(crate::mcp_platform::repository::ManifestSourceContext::HttpsProvision { binding }) if binding.provision_id == "https-provision-real-chain" && binding.parsed_digest == fixture.parsed_digest && binding.raw_digest == fixture.raw_digest)
        );
        assert_eq!(
            fixture
                .repository
                .get_consumed_https_manifest_provision(
                    "https-provision-real-chain",
                    "local_authenticated_client",
                    &fixture.binding
                )
                .await
                .unwrap(),
            fixture.consumed_record
        );
    }

    #[tokio::test]
    async fn https_plan_create_direct_handler_uses_scoped_transport_authority() {
        let fixture = setup_https_plan_fixture().await;
        let response =
            with_transport_mcp_platform_write_authority(Some(fixture.authority.clone()), async {
                fixture
                    .agent
                    .on_mcp_https_provision_plan_create(McpHttpsProvisionPlanCreateRequest {
                        provision_id: "https-provision-real-chain".to_string(),
                        expected_manifest_digest: fixture.parsed_digest.clone(),
                        idempotency_key: "https-plan-direct-handler".to_string(),
                    })
                    .await
            })
            .await;
        let McpPlatformOutcome::Success { value } = response.outcome else {
            panic!("the direct HTTPS plan handler must succeed under scoped authority");
        };
        assert_eq!(value.selected_manifest_digest, fixture.parsed_digest);
        assert!(!value.plan_id.is_empty());
        assert!(!value.plan_digest.is_empty());
    }

    #[tokio::test]
    async fn https_plan_review_wire_projection_preserves_safe_provenance_only() {
        let fixture = setup_https_plan_fixture().await;
        let (context, service) = with_transport_mcp_platform_write_authority(
            Some(fixture.authority.clone()),
            fixture.agent.mcp_platform_context_and_service(),
        )
        .await;
        let review = service
            .unwrap()
            .https_manifest_plan_review(
                &context,
                HttpsManifestPlanReviewInput {
                    provision_id: "https-provision-real-chain".to_string(),
                    expected_manifest_digest: fixture.parsed_digest.clone(),
                    idempotency_key: "https-plan-wire-projection".to_string(),
                },
            )
            .await
            .unwrap();
        let expected_plan_id = review.plan_id.clone();
        let expected_plan_digest = review.plan_digest.clone();
        let wire = plan_review_to_wire(review);
        let serialized = serde_json::to_string(&wire).unwrap();
        let round_tripped: McpPlanReview = serde_json::from_str(&serialized).unwrap();
        assert_eq!(round_tripped.plan_id, expected_plan_id);
        assert_eq!(round_tripped.plan_digest, expected_plan_digest);
        assert_eq!(
            round_tripped.selected_manifest_digest,
            fixture.parsed_digest
        );
        assert_eq!(round_tripped.mcp_id, "pkg.https.dispatch");
        assert_eq!(round_tripped.name, "pkg.https.dispatch 1.0.0");
        assert_eq!(round_tripped.version, "1.0.0");
        assert_eq!(
            round_tripped.source_provenance.import_kind,
            McpSourceImportKind::HttpsManifestUrl
        );
        assert!(matches!(
            round_tripped.source_provenance.source_ref,
            McpSourceRef::HttpsManifestUrl { .. }
        ));
        assert_eq!(
            serde_json::to_value(&round_tripped.permissions).unwrap(),
            serde_json::to_value(&wire.permissions).unwrap()
        );
        assert_eq!(round_tripped.network_origins, wire.network_origins);
        assert_eq!(
            round_tripped.file_effects.writes_files,
            wire.file_effects.writes_files
        );
        assert_eq!(
            round_tripped.file_effects.removes_files,
            wire.file_effects.removes_files
        );
        assert_eq!(
            round_tripped.file_effects.owned_items,
            wire.file_effects.owned_items
        );
        assert_eq!(
            round_tripped.host_effects.registration_ids,
            wire.host_effects.registration_ids
        );
        assert_eq!(
            round_tripped
                .process_effects
                .process_required_for_connection,
            wire.process_effects.process_required_for_connection
        );
        assert_eq!(
            round_tripped.process_effects.starts_during_confirmation,
            wire.process_effects.starts_during_confirmation
        );
        for secret in [
            fixture.token,
            fixture.token_hash.as_str(),
            fixture.raw_digest.as_str(),
            fixture.raw_manifest_sentinel.as_str(),
        ] {
            assert!(
                !serialized.contains(secret),
                "wire projection leaked {secret}"
            );
        }
    }

    #[tokio::test]
    async fn https_install_confirm_returns_only_task_id_without_leaks() {
        let fixture = setup_https_plan_fixture().await;
        let planned = fixture.agent.test_dispatch_transport_custom_request_with_authority(fixture.authority.clone(), MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD, serde_json::json!({"provisionId":"https-provision-real-chain","expectedManifestDigest":fixture.parsed_digest,"idempotencyKey":"https-plan-idempotency"})).await.unwrap();
        let value = planned["outcome"]["value"].as_object().unwrap();
        let confirmed = fixture.agent.test_dispatch_transport_custom_request_with_authority(fixture.authority, MCP_INSTALL_CONFIRM_METHOD, serde_json::json!({"planId":value["planId"],"planDigest":value["planDigest"],"userDecision":"confirm","idempotencyKey":"https-install-confirm-idempotency"})).await.unwrap();
        let wire = serde_json::to_string(&confirmed).unwrap();
        assert!(wire.contains("taskId"));
        for secret in [
            fixture.requested_url.as_str(),
            fixture.final_url.as_str(),
            fixture.binding.as_str(),
            fixture.requested_url_id.as_str(),
            fixture.final_url_id.as_str(),
            fixture.redirect_chain_digest.as_str(),
            fixture.raw_digest.as_str(),
            "https-install-confirm-idempotency",
            "https-provision-real-chain",
            fixture.token,
            fixture.token_hash.as_str(),
            fixture.wrong_actor_token,
            fixture.wrong_actor_token_hash.as_str(),
            fixture.parsed_digest.as_str(),
            fixture.dns_evidence_digest.as_str(),
            fixture.raw_manifest_sentinel.as_str(),
            "body-forged-authority",
            "proof-secret",
            "publisher-secret",
            "raw-secret",
            "HttpsProvision",
        ] {
            assert!(!wire.contains(secret), "leaked {secret}");
        }
        assert!(confirmed["outcome"]["value"]
            .as_object()
            .unwrap()
            .keys()
            .all(|key| key == "taskId"));
    }

    #[tokio::test]
    async fn https_plan_rejects_wrong_actor_and_session_without_state_changes() {
        let fixture = setup_https_plan_fixture().await;
        let (_wrong_session, wrong_authority) = test_transport_session_authority();
        for (authority, provision_id, idempotency) in [
            (
                fixture.authority,
                "https-provision-wrong-actor",
                "wrong-actor",
            ),
            (
                wrong_authority,
                "https-provision-real-chain",
                "wrong-session",
            ),
        ] {
            let response = fixture.agent.test_dispatch_transport_custom_request_with_authority(authority, MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD, serde_json::json!({"provisionId":provision_id,"expectedManifestDigest":fixture.parsed_digest,"idempotencyKey":idempotency})).await.unwrap();
            let wire = serde_json::to_string(&response).unwrap();
            for secret in [
                fixture.requested_url.as_str(),
                fixture.final_url.as_str(),
                fixture.binding.as_str(),
                fixture.requested_url_id.as_str(),
                fixture.final_url_id.as_str(),
                fixture.redirect_chain_digest.as_str(),
                fixture.raw_digest.as_str(),
                fixture.token,
                fixture.token_hash.as_str(),
                fixture.wrong_actor_token,
                fixture.wrong_actor_token_hash.as_str(),
                fixture.parsed_digest.as_str(),
                fixture.dns_evidence_digest.as_str(),
                fixture.raw_manifest_sentinel.as_str(),
                "https-provision-real-chain",
                "https-provision-wrong-actor",
                "HttpsProvision",
                "body-forged-authority",
                "proof-secret",
                "publisher-secret",
                "raw-secret",
            ] {
                assert!(!wire.contains(secret), "leaked {secret}");
            }
            let response: McpHttpsProvisionPlanCreateResponse =
                serde_json::from_value(response).unwrap();
            let McpPlatformOutcome::Error { error } = response.outcome else {
                panic!("invalid binding must fail through the real plan handler");
            };
            assert_eq!(error.code, McpPlatformErrorCodeDto::InvalidRequest);
            assert!(fixture
                .repository
                .get_plan_by_idempotency_key(idempotency)
                .await
                .unwrap()
                .is_none());
        }
        assert_eq!(
            fixture
                .repository
                .get_consumed_https_manifest_provision(
                    "https-provision-real-chain",
                    "local_authenticated_client",
                    &fixture.binding
                )
                .await
                .unwrap(),
            fixture.consumed_record
        );
        assert_eq!(
            fixture
                .repository
                .get_consumed_https_manifest_provision(
                    "https-provision-wrong-actor",
                    "different-actor",
                    &fixture.binding
                )
                .await
                .unwrap(),
            fixture.wrong_actor_consumed_record
        );
    }

    #[cfg(windows)]
    #[test]
    fn canonicalize_local_import_path_rejects_unc_and_device_namespaces_before_resolution() {
        for raw_path in [
            r"\\server\share\manifest.json",
            r"//server/share/manifest.json",
            r"\\?\C:\catalogs\manifest.json",
            r"\\.\COM1",
        ] {
            let error = canonicalize_local_import_path(
                raw_path,
                "Choose a local manifest file before importing.",
            )
            .expect_err("UNC and device namespace paths must fail closed");
            assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);
            assert_eq!(error.message(), LOCAL_ONLY_GOVERNED_IMPORT_MESSAGE);
        }
    }

    #[cfg(windows)]
    #[test]
    fn canonicalize_local_import_path_accepts_regular_local_absolute_paths() {
        let temp = tempfile::tempdir().unwrap();
        let manifest_path = temp.path().join("manifest.json");
        std::fs::write(&manifest_path, "{}").unwrap();

        let canonical = canonicalize_local_import_path(
            manifest_path.to_str().unwrap(),
            "Choose a local manifest file before importing.",
        )
        .expect("regular local absolute paths should remain available");

        assert_eq!(canonical, std::fs::canonicalize(&manifest_path).unwrap());
    }

    #[test]
    fn conflict_error_codes_keep_their_wire_meaning() {
        let cases = [
            (
                McpPlatformErrorCode::RevisionConflict,
                "revision_conflict",
                "revision_conflict",
            ),
            (
                McpPlatformErrorCode::ProjectionConflict,
                "projection_conflict",
                "projection_conflict",
            ),
        ];

        for (code, expected_code, expected_details_type) in cases {
            let envelope = error_to_wire(
                &context(),
                ErrorContext::default(),
                McpPlatformError::new(code, "test"),
            );
            let serialized = serde_json::to_value(envelope).expect("error envelope serializes");
            assert_eq!(serialized["code"], expected_code);
            assert_eq!(serialized["details"]["type"], expected_details_type);
        }
    }

    #[test]
    fn profile_phase_errors_use_4b_and_formal_profile_methods() {
        let envelope = error_to_wire(
            &context(),
            ErrorContext::profile(MCP_PROFILE_LIST_METHOD),
            McpPlatformError::new(McpPlatformErrorCode::NotImplementedForPhase, "test"),
        );
        let serialized = serde_json::to_value(envelope).expect("error envelope serializes");

        assert_eq!(serialized["code"], "not_implemented_for_phase");
        assert_eq!(serialized["details"]["type"], "phase_unavailable");
        assert_eq!(serialized["details"]["phase"], "4B");
        assert_eq!(serialized["details"]["operation"], MCP_PROFILE_LIST_METHOD);
    }

    #[test]
    fn operation_not_supported_keeps_closed_adapter_contract_even_for_profile_routes() {
        let envelope = error_to_wire(
            &context(),
            ErrorContext::profile(MCP_PROFILE_MODEL_RECOMMEND_METHOD),
            McpPlatformError::new(McpPlatformErrorCode::OperationNotSupported, "test"),
        );
        let serialized = serde_json::to_value(envelope).expect("error envelope serializes");

        assert_eq!(serialized["code"], "operation_not_supported");
        assert_eq!(serialized["details"]["type"], "phase_unavailable");
        assert_eq!(serialized["details"]["phase"], "3C");
        assert_eq!(serialized["details"]["operation"], "lifecycle");
    }

    #[test]
    fn credential_status_mapping_exposes_only_the_five_stable_wire_values() {
        let cases = [
            (CredentialStatus::Unconfigured, "unconfigured"),
            (
                CredentialStatus::ReRegistrationRequired,
                "re_registration_required",
            ),
            (
                CredentialStatus::TrustedStateConflict,
                "trusted_state_conflict",
            ),
            (
                CredentialStatus::TemporarilyUnavailable,
                "temporarily_unavailable",
            ),
            (CredentialStatus::Ready, "ready"),
        ];

        for (status, expected) in cases {
            assert_eq!(
                serde_json::to_value(credential_status_to_wire(status)).unwrap(),
                serde_json::Value::String(expected.to_string())
            );
        }
    }

    #[test]
    fn credential_status_projection_serialization_omits_sensitive_runtime_terms() {
        let profile = profile_to_wire(
            crate::mcp_platform::McpProfile {
                profile_id: "profile_a".to_string(),
                name: "A".to_string(),
                description: "B".to_string(),
                revision: 3,
                archived: false,
                entries: vec![],
                created_at_ms: 1,
                updated_at_ms: 2,
            },
            Some(CredentialStatus::TrustedStateConflict),
        );
        let health = health_status_to_wire(crate::mcp_platform::service::HealthStatus {
            managed_mcp_id: "managed_a".to_string(),
            state: crate::mcp_platform::HealthState::Healthy,
            latest: None,
            credential_status: Some(CredentialStatus::ReRegistrationRequired),
        });

        for serialized in [
            serde_json::to_string(&profile).unwrap(),
            serde_json::to_string(&health).unwrap(),
        ] {
            for forbidden in ["secret", "handle", "digest", "witness", "keyring", "anchor"] {
                assert!(
                    !serialized.contains(forbidden),
                    "projection leaked forbidden token {forbidden}: {serialized}"
                );
            }
        }
    }

    #[test]
    fn platform_error_wire_hides_sensitive_low_level_message_parts() {
        let sensitive_payloads = [
            r"C:\\Users\\test\\app\\server.exe",
            "goose --bootstrap --token=abc123",
            "sha256:deadbeefcafebabe",
        ];
        let envelope = error_to_wire(
            &context(),
            ErrorContext::default(),
            McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "path=/tmp/secret file not found -- token=abc123 hash=sha256:deadbeefcafebabe cmd=goose run",
            ),
        );
        let serialized = serde_json::to_value(envelope).expect("error envelope serializes");
        let serialized = serialized.to_string();

        for payload in sensitive_payloads {
            assert!(
                !serialized.contains(payload),
                "serialized envelope leaked sensitive payload: {payload}: {serialized}"
            );
        }
    }

    #[test]
    fn runtime_control_error_wire_hides_sensitive_low_level_message_parts() {
        let sensitive_payloads = [
            r"C:\\Users\\test\\goose-runtime.exe",
            "runtime-control --token=secret-token",
            "sha256:0123456789abcdef",
        ];
        let envelope = error_to_wire(
            &context(),
            ErrorContext::default(),
            McpPlatformError::new(
                McpPlatformErrorCode::RuntimeControlUnavailable,
                "runtime command failed: C:\\Users\\test\\goose-runtime.exe --token=secret-token hash=sha256:0123456789abcdef",
            ),
        );
        let serialized = serde_json::to_value(envelope).expect("error envelope serializes");
        let serialized = serialized.to_string();

        for payload in sensitive_payloads {
            assert!(
                !serialized.contains(payload),
                "serialized envelope leaked sensitive payload: {payload}: {serialized}"
            );
        }
    }

    #[test]
    fn task_wire_hides_sensitive_adapter_failed_message() {
        let task = crate::mcp_platform::service::TaskRef {
            task_id: "task_1".to_string(),
            operation: TaskOperation::Install,
            status: TaskStatus::Failed,
            progress: 0,
            cancellable: false,
            revision: 2,
            updated_at_ms: 3,
            redacted_error: Some(RedactedError::new(
                RedactedErrorCode::AdapterFailed,
                "failed to start adapter command C:\\Users\\test\\mcp.exe --token=adapter-token hash=abc123",
                std::iter::empty::<&str>(),
            )),
            rollback_status: RollbackStatus::NotRequired,
            rollback_evidence: None,
        };

        let wire = task_to_wire(task);
        let serialized =
            serde_json::to_string(&wire.outcome.error).expect("task outcome serializes");
        assert!(
            !serialized.contains("C:\\Users\\test\\mcp.exe")
                && !serialized.contains("adapter-token")
                && !serialized.contains("abc123"),
            "task outcome leaked sensitive adapter failure details: {serialized}"
        );
    }

    #[test]
    fn integrity_unavailable_preserves_stable_storage_root_message() {
        let envelope = error_to_wire(
            &context(),
            ErrorContext::default(),
            McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE,
            ),
        );

        assert_eq!(envelope.message, MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE);
    }

    #[test]
    fn task_wire_surfaces_stable_storage_root_message() {
        let task = crate::mcp_platform::service::TaskRef {
            task_id: "task_1".to_string(),
            operation: TaskOperation::Install,
            status: TaskStatus::Failed,
            progress: 0,
            cancellable: false,
            revision: 2,
            updated_at_ms: 3,
            redacted_error: Some(RedactedError::new(
                RedactedErrorCode::AdapterFailed,
                MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE,
                std::iter::empty::<&str>(),
            )),
            rollback_status: RollbackStatus::NotRequired,
            rollback_evidence: None,
        };

        let wire = task_to_wire(task);

        assert_eq!(
            wire.outcome
                .error
                .as_ref()
                .map(|error| error.message.as_str()),
            Some(MANAGED_STORAGE_ROOT_UNAVAILABLE_MESSAGE)
        );
    }

    #[tokio::test]
    async fn managed_runtime_stop_preserves_successful_session_record_when_another_close_fails() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let acp = agent(&data_dir, &config_dir).await;
        let session_a = runtime_test_agent(&temp.path().join("session-a"));
        let session_b = runtime_test_agent(&temp.path().join("session-b"));
        session_a
            .extension_manager
            .add_mock_extension(
                "todo".to_string(),
                Arc::new(RetryableCloseClient {
                    fail_next_close: AtomicBool::new(false),
                }),
            )
            .await;
        session_b
            .extension_manager
            .add_mock_extension(
                "todo".to_string(),
                Arc::new(RetryableCloseClient {
                    fail_next_close: AtomicBool::new(true),
                }),
            )
            .await;
        let expected_config = session_a
            .extension_manager
            .get_extension_config("todo")
            .await
            .unwrap();
        acp.register_acp_session("session_a".to_string(), session_a.clone(), HashMap::new())
            .await;
        acp.register_acp_session("session_b".to_string(), session_b.clone(), HashMap::new())
            .await;
        let command = ManagedRuntimeCommand {
            managed_mcp_id: "managed_todo".to_string(),
            extension_key: "todo".to_string(),
            expected_config,
            action: HostRuntimeAction::Stop,
        };

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            AcpManagedRuntimeControlPort { agent: &acp }.control(command.clone()),
        )
        .await
        .expect("managed stop must not deadlock")
        .unwrap_err();
        assert_eq!(
            error.code(),
            crate::mcp_platform::McpPlatformErrorCode::RuntimeControlUnavailable
        );
        assert!(session_a
            .extension_manager
            .get_extension_config("todo")
            .await
            .is_none());
        assert!(session_b
            .extension_manager
            .get_extension_config("todo")
            .await
            .is_some());
        assert_eq!(
            acp.managed_runtime_stopped_session_ids("managed_todo")
                .await,
            HashSet::from(["session_a".to_string()])
        );

        assert!(tokio::time::timeout(
            std::time::Duration::from_secs(5),
            AcpManagedRuntimeControlPort { agent: &acp }.control(command),
        )
        .await
        .expect("retrying managed stop must not deadlock")
        .unwrap());
        assert_eq!(
            acp.managed_runtime_stopped_session_ids("managed_todo")
                .await,
            HashSet::from(["session_a".to_string(), "session_b".to_string()])
        );
    }

    #[tokio::test]
    async fn managed_runtime_record_race_with_session_cleanup_leaves_no_stale_session_id() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let acp = agent(&data_dir, &config_dir).await;
        let session = runtime_test_agent(&temp.path().join("session-a"));
        acp.register_acp_session("session_a".to_string(), session.clone(), HashMap::new())
            .await;

        let record =
            acp.record_stopped_managed_runtime_session("managed_todo", "session_a", &session);
        let cleanup = acp.unregister_acp_session_and_managed_runtime("session_a", &session);
        let (_, _) = tokio::join!(record, cleanup);

        assert!(acp
            .managed_runtime_stopped_session_ids("managed_todo")
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn managed_runtime_start_and_session_cleanup_do_not_deadlock() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let acp = agent(&data_dir, &config_dir).await;
        let session = runtime_test_agent(&temp.path().join("session-a"));
        session
            .extension_manager
            .add_mock_extension(
                "todo".to_string(),
                Arc::new(RetryableCloseClient {
                    fail_next_close: AtomicBool::new(false),
                }),
            )
            .await;
        let expected_config = session
            .extension_manager
            .get_extension_config("todo")
            .await
            .unwrap();
        session
            .extension_manager
            .remove_extension("todo")
            .await
            .unwrap();
        acp.register_acp_session("session_a".to_string(), session.clone(), HashMap::new())
            .await;
        assert!(
            acp.record_stopped_managed_runtime_session("managed_todo", "session_a", &session)
                .await
        );

        let port = AcpManagedRuntimeControlPort { agent: &acp };
        let start = port.control(ManagedRuntimeCommand {
            managed_mcp_id: "managed_todo".to_string(),
            extension_key: "todo".to_string(),
            expected_config,
            action: HostRuntimeAction::Start,
        });
        let cleanup = acp.unregister_acp_session_and_managed_runtime("session_a", &session);
        let (_, cleaned) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(start, cleanup)
        })
        .await
        .expect("managed runtime start and session cleanup must not deadlock");

        assert!(cleaned);
        assert!(acp
            .managed_runtime_stopped_session_ids("managed_todo")
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn managed_runtime_cleanup_does_not_delete_re_registered_session_record() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let acp = agent(&data_dir, &config_dir).await;
        let old_session = runtime_test_agent(&temp.path().join("session-old"));
        let new_session = runtime_test_agent(&temp.path().join("session-new"));
        acp.register_acp_session("session_a".to_string(), old_session.clone(), HashMap::new())
            .await;
        assert!(
            acp.record_stopped_managed_runtime_session("managed_todo", "session_a", &old_session)
                .await
        );

        acp.register_acp_session("session_a".to_string(), new_session.clone(), HashMap::new())
            .await;
        assert!(
            acp.record_stopped_managed_runtime_session("managed_todo", "session_a", &new_session)
                .await
        );

        assert!(
            !acp.unregister_acp_session_and_managed_runtime("session_a", &old_session)
                .await
        );
        assert_eq!(
            acp.managed_runtime_stopped_session_ids("managed_todo")
                .await,
            HashSet::from(["session_a".to_string()])
        );
        assert!(acp.session_is_current("session_a", &new_session).await);
    }

    #[tokio::test]
    async fn managed_runtime_cleanup_removes_only_old_owner_records_after_re_registration() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let acp = agent(&data_dir, &config_dir).await;
        let old_session = runtime_test_agent(&temp.path().join("session-old"));
        let new_session = runtime_test_agent(&temp.path().join("session-new"));

        acp.register_acp_session("session_a".to_string(), new_session.clone(), HashMap::new())
            .await;
        {
            let mut runtimes = acp.managed_runtime_sessions.lock().await;
            runtimes.stopped.insert(
                "managed_todo".to_string(),
                HashMap::from([("session_a".to_string(), old_session.clone())]),
            );
        }

        assert!(
            !acp.unregister_acp_session_and_managed_runtime("session_a", &old_session)
                .await
        );
        assert!(acp.session_is_current("session_a", &new_session).await);
        let runtimes = acp.managed_runtime_sessions.lock().await;
        assert!(runtimes
            .active
            .get("session_a")
            .is_some_and(|agent| Arc::ptr_eq(agent, &new_session)));
        assert!(runtimes.stopped.get("managed_todo").is_none());
    }

    #[tokio::test]
    async fn managed_runtime_delayed_registration_cannot_overwrite_new_owner() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let acp = Arc::new(agent(&data_dir, &config_dir).await);
        let session_id = "session_a".to_string();
        let old_agent = acp
            .agent_manager
            .get_or_create_agent(session_id.clone())
            .await
            .unwrap();
        acp.register_acp_session(session_id.clone(), old_agent.clone(), HashMap::new())
            .await;
        assert!(
            acp.unregister_acp_session_and_managed_runtime(&session_id, &old_agent)
                .await
        );
        assert!(acp
            .agent_manager
            .remove_session_if_current(&session_id, &old_agent)
            .await
            .unwrap());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        acp.install_registration_test_gate(RegistrationTestGate {
            entered: entered_tx,
            release: release_rx,
        })
        .await;

        let old_registration = {
            let acp = acp.clone();
            let old_agent = old_agent.clone();
            let session_id = session_id.clone();
            tokio::spawn(async move {
                acp.register_acp_session(session_id, old_agent, HashMap::new())
                    .await;
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx)
            .await
            .expect("old registration must reach the pre-commit gate")
            .expect("old registration gate must signal");

        let new_agent = acp
            .agent_manager
            .get_or_create_agent(session_id.clone())
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&old_agent, &new_agent));

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            acp.register_acp_session(session_id.clone(), new_agent.clone(), HashMap::new()),
        )
        .await
        .expect("new registration must commit while old registration is paused");
        release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), old_registration)
            .await
            .expect("old registration must finish after release")
            .unwrap();

        assert!(acp.session_is_current(&session_id, &new_agent).await);
        let cached = acp
            .agent_manager
            .get_or_create_agent(session_id.clone())
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&cached, &new_agent));
        let runtimes = acp.managed_runtime_sessions.lock().await;
        assert!(runtimes
            .active
            .get(&session_id)
            .is_some_and(|agent| Arc::ptr_eq(agent, &new_agent)));
        assert!(runtimes
            .stopped
            .values()
            .all(|sessions| !sessions.contains_key(&session_id)));
        drop(runtimes);
        let registrations = acp.session_registrations.lock().await;
        assert!(registrations
            .get(&session_id)
            .is_some_and(|registration| Arc::ptr_eq(&registration.agent, &new_agent)));
    }

    #[tokio::test]
    async fn orchestrator_completion_preserves_re_registered_token_owner() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let acp = Arc::new(agent(&data_dir, &config_dir).await);
        let (new_run_entered_tx, new_run_entered_rx) = tokio::sync::oneshot::channel();
        let (new_run_release_tx, new_run_release_rx) = tokio::sync::oneshot::channel();
        let provider = Arc::new(CompletionRaceProvider::new(
            new_run_entered_tx,
            new_run_release_rx,
        ));
        acp.agent_manager.set_default_provider(provider).await;
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let session = acp
            .agent_manager
            .session_manager()
            .create_session(
                workspace,
                "orchestrator completion race".to_string(),
                SessionType::Acp,
                Config::global().get_goose_mode().unwrap_or_default(),
            )
            .await
            .unwrap();
        let mut extension_data = ExtensionData::new();
        EnabledExtensionsState::new(Vec::new())
            .to_extension_data(&mut extension_data)
            .unwrap();
        acp.agent_manager
            .session_manager()
            .update(&session.id)
            .extension_data(extension_data)
            .recipe(None)
            .apply()
            .await
            .unwrap();
        let session_id = session.id;
        let old_agent = acp
            .agent_manager
            .get_or_create_agent(session_id.clone())
            .await
            .unwrap();
        acp.register_acp_session(session_id.clone(), old_agent.clone(), HashMap::new())
            .await;

        let (old_completion_entered_tx, old_completion_entered_rx) =
            tokio::sync::oneshot::channel();
        let (old_completion_release_tx, old_completion_release_rx) =
            tokio::sync::oneshot::channel();
        let old_client = OrchestratorClient::new_for_test(
            PlatformExtensionContext {
                extension_manager: None,
                session_manager: old_agent.config.session_manager.clone(),
                session: None,
                use_login_shell_path: false,
            },
            acp.agent_manager.clone(),
            Some(CompletionTestGate {
                entered: old_completion_entered_tx,
                release: old_completion_release_rx,
            }),
        )
        .unwrap();
        let old_run = {
            let session_id = session_id.clone();
            tokio::spawn(async move {
                old_client
                    .call_tool(
                        &ToolCallContext::new("parent-session".to_string(), None, None),
                        "send_message",
                        Some(
                            serde_json::json!({
                                "session_id": session_id,
                                "message": "old run"
                            })
                            .as_object()
                            .unwrap()
                            .clone(),
                        ),
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), old_completion_entered_rx)
            .await
            .expect("old run must reach the production completion gate")
            .expect("old completion gate must signal");

        assert!(
            acp.unregister_acp_session_and_managed_runtime(&session_id, &old_agent)
                .await
        );
        assert!(acp
            .agent_manager
            .remove_session_if_current(&session_id, &old_agent)
            .await
            .unwrap());
        let new_agent = acp
            .agent_manager
            .get_or_create_agent(session_id.clone())
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&old_agent, &new_agent));
        acp.register_acp_session(session_id.clone(), new_agent.clone(), HashMap::new())
            .await;

        let new_client = OrchestratorClient::new_for_test(
            PlatformExtensionContext {
                extension_manager: None,
                session_manager: new_agent.config.session_manager.clone(),
                session: None,
                use_login_shell_path: false,
            },
            acp.agent_manager.clone(),
            None,
        )
        .unwrap();
        let new_run = {
            let session_id = session_id.clone();
            tokio::spawn(async move {
                new_client
                    .call_tool(
                        &ToolCallContext::new("parent-session".to_string(), None, None),
                        "send_message",
                        Some(
                            serde_json::json!({
                                "session_id": session_id,
                                "message": "new run"
                            })
                            .as_object()
                            .unwrap()
                            .clone(),
                        ),
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), new_run_entered_rx)
            .await
            .expect("new run must register its token through the orchestrator")
            .expect("new run provider must signal");
        assert!(acp.agent_manager.is_session_busy(&session_id).await);
        assert_eq!(
            acp.agent_manager
                .current_cancel_token_is_cancelled(&session_id)
                .await,
            Some(false)
        );

        old_completion_release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), old_run)
            .await
            .expect("old orchestrator completion must return after release")
            .unwrap()
            .unwrap();

        assert!(acp.session_is_current(&session_id, &new_agent).await);
        assert!(acp.agent_manager.is_session_busy(&session_id).await);
        assert_eq!(
            acp.agent_manager
                .current_cancel_token_is_cancelled(&session_id)
                .await,
            Some(false)
        );
        let runtimes = acp.managed_runtime_sessions.lock().await;
        assert!(runtimes
            .active
            .get(&session_id)
            .is_some_and(|agent| Arc::ptr_eq(agent, &new_agent)));
        assert!(runtimes
            .stopped
            .values()
            .all(|sessions| !sessions.contains_key(&session_id)));
        drop(runtimes);
        new_run_release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), new_run)
            .await
            .expect("new orchestrator completion must return after release")
            .unwrap()
            .unwrap();
        assert!(!acp.agent_manager.is_session_busy(&session_id).await);
    }

    #[tokio::test]
    async fn message_processing_releases_sessions_lock_before_async_handler() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let acp = Arc::new(agent(&data_dir, &config_dir).await);
        let session_agent = runtime_test_agent(&temp.path().join("session"));
        acp.register_acp_session(
            "session_a".to_string(),
            session_agent.clone(),
            HashMap::new(),
        )
        .await;

        let (client_read, server_write) = tokio::io::duplex(64 * 1024);
        let (server_read, client_write) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let _ = agent_client_protocol::Client
                .builder()
                .connect_to(agent_client_protocol::ByteStreams::new(
                    server_write.compat_write(),
                    server_read.compat(),
                ))
                .await;
        });
        let transport = agent_client_protocol::ByteStreams::new(
            client_write.compat_write(),
            client_read.compat(),
        );
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        acp.install_message_content_test_gate(MessageContentTestGate {
            entered: entered_tx,
            release: release_rx,
        })
        .await;

        let test_result = SacpAgent
            .builder()
            .connect_with(transport, async |cx| {
                let processing_agent = acp.clone();
                let processing_session_agent = session_agent.clone();
                let processing = tokio::spawn(async move {
                    processing_agent
                        .handle_message_content(
                            &MessageContent::SystemNotification(SystemNotificationContent {
                                notification_type: SystemNotificationType::InlineMessage,
                                msg: "test".to_string(),
                                data: None,
                            }),
                            &SessionId::new("session_a"),
                            "session_a",
                            None,
                            0,
                            &Role::Assistant,
                            false,
                            &processing_session_agent,
                            &cx,
                        )
                        .await
                });

                tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx)
                    .await
                    .expect("message handler must reach its async wait")
                    .expect("message handler gate must signal");
                let sessions = tokio::time::timeout(
                    std::time::Duration::from_millis(250),
                    acp.sessions.lock(),
                )
                .await
                .expect("sessions lock must not remain held while message handling awaits");
                drop(sessions);
                release_tx.send(()).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(2), processing)
                    .await
                    .expect("message processing must complete after release")
                    .unwrap()?;
                Ok(())
            })
            .await;
        server.abort();
        test_result.unwrap();
    }

    #[tokio::test]
    async fn prompt_loop_old_message_cannot_mutate_re_registered_session() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let acp = Arc::new(agent(&data_dir, &config_dir).await);
        let old_agent = runtime_test_agent(&data_dir);
        let session = old_agent
            .config
            .session_manager
            .create_session(
                workspace,
                "old prompt".to_string(),
                SessionType::Acp,
                Config::global().get_goose_mode().unwrap_or_default(),
            )
            .await
            .unwrap();
        old_agent
            .update_provider(
                Arc::new(SingleMessageProvider),
                ModelConfig::new("test-model"),
                &session.id,
            )
            .await
            .unwrap();
        let new_agent = runtime_test_agent(&temp.path().join("new"));
        acp.register_acp_session(session.id.clone(), old_agent.clone(), HashMap::new())
            .await;

        let (client_read, server_write) = tokio::io::duplex(64 * 1024);
        let (server_read, client_write) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let _ = agent_client_protocol::Client
                .builder()
                .connect_to(agent_client_protocol::ByteStreams::new(
                    server_write.compat_write(),
                    server_read.compat(),
                ))
                .await;
        });
        let transport = agent_client_protocol::ByteStreams::new(
            client_write.compat_write(),
            client_read.compat(),
        );
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        acp.install_message_content_test_gate(MessageContentTestGate {
            entered: entered_tx,
            release: release_rx,
        })
        .await;

        let test_result = SacpAgent
            .builder()
            .connect_with(transport, async |cx| {
                let processing_acp = acp.clone();
                let processing_cx = cx.clone();
                let prompt = PromptRequest::new(
                    SessionId::new(session.id.clone()),
                    vec![ContentBlock::Text(TextContent::new("go".to_string()))],
                );
                let processing =
                    tokio::spawn(
                        async move { processing_acp.on_prompt(&processing_cx, prompt).await },
                    );

                tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx)
                    .await
                    .expect("prompt loop must reach the message handler await")
                    .expect("message handler gate must signal");
                let sessions = tokio::time::timeout(
                    std::time::Duration::from_millis(250),
                    acp.sessions.lock(),
                )
                .await
                .expect("prompt loop must not retain sessions while handling a message");
                drop(sessions);
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    acp.register_acp_session(session.id.clone(), new_agent.clone(), HashMap::new()),
                )
                .await
                .expect("re-registration must complete while old message handling is paused");
                release_tx.send(()).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(2), processing)
                    .await
                    .expect("old prompt loop must finish after release")
                    .unwrap()?;

                assert!(acp.session_is_current(&session.id, &new_agent).await);
                let sessions = acp.sessions.lock().await;
                let current = sessions.get(&session.id).unwrap();
                assert!(current.chain_membership.is_empty());
                assert!(current.tool_requests.is_empty());
                Ok(())
            })
            .await;
        server.abort();
        test_result.unwrap();
    }

    #[tokio::test]
    async fn managed_runtime_start_partial_failure_crashes_service_and_keeps_retryable_session() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &temp.path().join("managed-runtime.db"),
                Arc::new(InMemoryIntegritySigner::new_for_testing([0x42; 32])),
            )
            .await
            .unwrap(),
        );
        let remote_http =
            Arc::new(crate::mcp_platform::CoreManagedRemoteHttpNetworkPolicy::default());
        let service = Arc::new(
            McpPlatformService::new_with_distribution_ports(
                repository.clone(),
                Arc::new(SystemClock),
                Arc::new(UuidGenerator),
                McpPlatformServiceOptions::default(),
                crate::mcp_platform::LifecyclePorts {
                    registration: Arc::new(
                        crate::mcp_platform::SafeRegistrationEffectAdapter::new(
                            remote_http.clone(),
                        ),
                    ),
                    host_integration: Arc::new(crate::mcp_platform::EmptyHostIntegrationAdapter),
                    transport: Arc::new(crate::mcp_platform::CoreTransportProjectionAdapter),
                    auth: Arc::new(crate::mcp_platform::ConfigAuthRequirementResolver),
                    health: Arc::new(crate::mcp_platform::ProductionHealthCheckAdapter::new(
                        remote_http,
                    )),
                    projection_sink: Arc::new(crate::mcp_platform::ConfigProjectionSink::default()),
                },
                Arc::new(UnusedDistribution),
                crate::mcp_platform::RuntimeCapabilitySnapshot {
                    node_available: true,
                    python_major_minor: None,
                },
            )
            .with_enrollment_writer_owner(
                EnrollmentWriterOwner::in_memory_for_testing_default_time(),
            ),
        );
        let (managed_mcp_id, expected_config, revision, default_enabled) =
            prepare_managed_runtime_fixture(&service, &repository).await;
        let acp = agent_with_service(&data_dir, &config_dir, service.clone()).await;
        let session_a = runtime_test_agent(&temp.path().join("session-a"));
        let session_b = runtime_test_agent(&temp.path().join("session-b"));
        let session_a_id = session_a
            .config
            .session_manager
            .create_session(
                temp.path().join("workspace-a"),
                "runtime-a".to_string(),
                SessionType::Acp,
                Config::global().get_goose_mode().unwrap_or_default(),
            )
            .await
            .unwrap()
            .id;
        let session_b_id = session_b
            .config
            .session_manager
            .create_session(
                temp.path().join("workspace-b"),
                "runtime-b".to_string(),
                SessionType::Acp,
                Config::global().get_goose_mode().unwrap_or_default(),
            )
            .await
            .unwrap()
            .id;
        session_a
            .extension_manager
            .add_mock_extension_config_for_test(expected_config.clone())
            .await;
        session_b
            .extension_manager
            .add_mock_extension_config_for_test(expected_config)
            .await;
        acp.register_acp_session(session_a_id.clone(), session_a.clone(), HashMap::new())
            .await;
        acp.register_acp_session(session_b_id.clone(), session_b.clone(), HashMap::new())
            .await;
        acp.managed_runtime_sessions.lock().await.stopped.insert(
            managed_mcp_id.clone(),
            HashMap::from([
                (session_a_id.clone(), session_a),
                (session_b_id.clone(), session_b.clone()),
            ]),
        );
        session_b.fail_next_managed_runtime_start_for_test();

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.managed_runtime_control(
                &context(),
                ManagedRuntimeControlInput {
                    managed_mcp_id: managed_mcp_id.clone(),
                    expected_revision: revision,
                    action: ServiceRuntimeAction::Start,
                },
                &AcpManagedRuntimeControlPort { agent: &acp },
            ),
        )
        .await
        .expect("managed start must not deadlock")
        .unwrap_err();
        assert_eq!(
            error.code(),
            McpPlatformErrorCode::RuntimeControlUnavailable
        );
        assert_eq!(
            acp.managed_runtime_stopped_session_ids(&managed_mcp_id)
                .await,
            HashSet::from([session_b_id])
        );
        assert_eq!(
            repository
                .get_managed_inventory(&managed_mcp_id)
                .await
                .unwrap()
                .managed
                .state
                .runtime,
            crate::mcp_platform::RuntimeState::Crashed
        );
        assert_eq!(
            repository
                .get_managed_inventory(&managed_mcp_id)
                .await
                .unwrap()
                .managed
                .state
                .default_enabled,
            default_enabled
        );

        let retry_revision = repository
            .get_managed_inventory(&managed_mcp_id)
            .await
            .unwrap()
            .managed
            .revision;
        let restarted = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.managed_runtime_control(
                &context(),
                ManagedRuntimeControlInput {
                    managed_mcp_id: managed_mcp_id.clone(),
                    expected_revision: retry_revision,
                    action: ServiceRuntimeAction::Start,
                },
                &AcpManagedRuntimeControlPort { agent: &acp },
            ),
        )
        .await
        .expect("retrying managed start must not deadlock")
        .unwrap();
        assert_eq!(
            restarted.runtime,
            crate::mcp_platform::RuntimeState::Running
        );
        assert_eq!(restarted.default_enabled, default_enabled);
        assert!(acp
            .managed_runtime_stopped_session_ids(&managed_mcp_id)
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn runtime_control_binding_is_transport_and_revision_bound() {
        assert!(runtime_control_binding("managed_a", 4).is_none());

        let (transport_a, authority_a) = test_transport_session_authority();
        let (same_id, other_revision, other_id) =
            with_transport_mcp_platform_write_authority(Some(authority_a), async {
                (
                    runtime_control_binding("managed_a", 4).unwrap(),
                    runtime_control_binding("managed_a", 5).unwrap(),
                    runtime_control_binding("managed_b", 4).unwrap(),
                )
            })
            .await;
        let (_transport_b, authority_b) = test_transport_session_authority();
        let different_transport =
            with_transport_mcp_platform_write_authority(Some(authority_b), async {
                runtime_control_binding("managed_a", 4).unwrap()
            })
            .await;

        assert_ne!(same_id, other_revision);
        assert_ne!(same_id, other_id);
        assert_ne!(same_id, different_transport);
        drop(transport_a);
    }

    #[test]
    fn connection_test_single_flight_and_deadline_are_fail_closed() {
        let permits = connection_test_permits();
        let permit = permits.try_acquire().unwrap();
        assert!(permits.try_acquire().is_err());
        drop(permit);
        assert!(
            remaining_connection_test_time(Instant::now() - Duration::from_millis(1)).is_none()
        );
        assert!(!connection_test_may_send_model(false, &[]));
    }

    #[tokio::test]
    async fn reaper_keeps_the_connection_test_permit_until_cleanup_finishes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(ExtensionManager::new_without_provider(
            temp_dir.path().to_path_buf(),
        ));
        let permits = Arc::new(Semaphore::new(1));
        let permit = permits.clone().try_acquire_owned().unwrap();
        let cleanup_gate = Arc::new(tokio::sync::Notify::new());
        let setup_gate = cleanup_gate.clone();
        let pending_setup = tokio::spawn(async move {
            setup_gate.notified().await;
            Ok(())
        });

        let reaper = spawn_connection_test_reaper(
            manager,
            "not-added".to_string(),
            Some(pending_setup),
            permit,
        );
        tokio::task::yield_now().await;
        assert!(permits.try_acquire().is_err());

        cleanup_gate.notify_one();
        reaper.await.unwrap();
        assert!(permits.try_acquire().is_ok());
    }

    #[test]
    fn connection_test_forbids_unbounded_transport_allows_only_managed_streamable_http() {
        let direct_process =
            crate::agents::ExtensionConfig::stdio("rejected-stdio", "cmd", "stdio test", 1_u64);
        let streamable_http = crate::agents::ExtensionConfig::streamable_http(
            "rejected-streamable-http",
            "https://mcp.example.test",
            "streamable http test",
            1_u64,
        );
        let managed_http = crate::agents::ExtensionConfig::ManagedStreamableHttp {
            name: "rejected-managed-streamable-http".to_string(),
            description: "remote test".to_string(),
            uri: "https://mcp.example.test".to_string(),
            timeout: Some(1),
            bundled: None,
            available_tools: Vec::new(),
        };

        assert!(connection_test_forbids_unbounded_transport(&direct_process));
        assert!(connection_test_forbids_unbounded_transport(
            &streamable_http
        ));
        assert!(!connection_test_forbids_unbounded_transport(&managed_http));
    }

    struct ConnectionTestNoToolCaptureProvider {
        called_with_tools: AtomicUsize,
        completed: AtomicBool,
    }

    impl ConnectionTestNoToolCaptureProvider {
        fn new() -> Self {
            Self {
                called_with_tools: AtomicUsize::new(usize::MAX),
                completed: AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl Provider for ConnectionTestNoToolCaptureProvider {
        fn get_name(&self) -> &str {
            "connection-test-no-tool-capture-provider"
        }

        async fn stream(
            &self,
            _model_config: &ModelConfig,
            _system: &str,
            _messages: &[Message],
            tools: &[rmcp::model::Tool],
        ) -> std::result::Result<MessageStream, ProviderError> {
            self.called_with_tools.store(tools.len(), Ordering::SeqCst);
            self.completed.store(true, Ordering::SeqCst);
            Ok(stream_from_single_message(
                Message::assistant().with_text("ok"),
                ProviderUsage::new(
                    "connection-test-no-tool-capture-provider".to_string(),
                    Usage::default(),
                ),
            ))
        }
    }

    #[test]
    fn connection_test_discovery_then_model_request_does_not_send_tools() {
        let handle = std::thread::Builder::new()
            .name("connection-test-discovery".to_string())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(
                    connection_test_discovery_then_model_request_does_not_send_tools_async(),
                );
            })
            .unwrap();
        if let Err(err) = handle.join() {
            std::panic::resume_unwind(err);
        }
    }

    async fn connection_test_discovery_then_model_request_does_not_send_tools_async() {
        let provider = Arc::new(ConnectionTestNoToolCaptureProvider::new());
        let factory_provider_ids = Arc::new(Mutex::new(Vec::new()));
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join("data");
        let config_dir = temp_dir.path().join("config");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let config_file = config_dir.join("config.yaml");
        let secrets_file = config_dir.join("secrets.yaml");
        std::fs::write(&config_file, "").unwrap();
        std::fs::write(&secrets_file, "").unwrap();
        let config = Arc::new(Config::new_with_file_secrets(&config_file, &secrets_file).unwrap());
        let repository = Arc::new(
            SqliteMcpPlatformRepository::open_path_with_integrity_signer(
                &temp_dir.path().join("connection-test.db"),
                Arc::new(InMemoryIntegritySigner::new_for_testing([0x43; 32])),
            )
            .await
            .unwrap(),
        );
        let service = Arc::new(
            McpPlatformService::new_with_lifecycle_ports(
                repository.clone(),
                Arc::new(SystemClock),
                Arc::new(UuidGenerator),
                McpPlatformServiceOptions::default(),
                LifecyclePorts {
                    registration: Arc::new(ConnectionTestRegistration),
                    host_integration: Arc::new(EmptyHostIntegrationAdapter),
                    transport: Arc::new(CoreTransportProjectionAdapter),
                    auth: Arc::new(ConfigAuthRequirementResolver),
                    health: Arc::new(ConnectionTestHealth),
                    projection_sink: Arc::new(ConfigProjectionSink::with_config(config.clone())),
                },
                Arc::new(ConnectionTestRemoteHttpPolicy),
            )
            .with_enrollment_writer_owner(
                EnrollmentWriterOwner::in_memory_for_testing_default_time(),
            ),
        );
        let (managed_mcp_id, revision) =
            prepare_connection_test_remote_http_fixture(&service, &repository).await;
        let projection_key = format!(
            "managed_mcp_{}",
            managed_mcp_id.strip_prefix("managed_").unwrap()
        );
        assert!(
            crate::config::extensions::get_extension_entry_by_key_with_config(
                config.as_ref(),
                &projection_key,
            )
            .is_some(),
            "connection-test registration projection must use fixture-local Config"
        );
        let enabled = service
            .set_default_enabled(
                &context(),
                SetDefaultEnabledInput {
                    managed_mcp_id: managed_mcp_id.clone(),
                    enabled: true,
                    expected_revision: revision,
                },
            )
            .await
            .unwrap();
        assert!(enabled.default_enabled);
        let profile = service
            .profile_create(
                &context(),
                ProfileCreateInput {
                    name: "Connection test fixture".to_string(),
                    description: "In-process MCP discovery fixture".to_string(),
                    managed_mcp_ids: vec![managed_mcp_id],
                    idempotency_key: "connection-test-profile".to_string(),
                },
            )
            .await
            .unwrap();
        let agent = agent_with_service_and_provider(
            &data_dir,
            &config_dir,
            service,
            provider.clone(),
            factory_provider_ids.clone(),
        )
        .await;
        agent.set_configured_provider_override("anthropic");
        let (_transport_session, authority) = test_transport_session_authority();
        let response = with_transport_mcp_platform_write_authority(Some(authority), async {
            agent
                .on_mcp_profile_connection_test(McpProfileConnectionTestRequest {
                    profile_id: profile.profile_id,
                    provider_id: "anthropic".to_string(),
                    model_id: "claude-sonnet-4-5".to_string(),
                })
                .await
        })
        .await;
        let McpPlatformOutcome::Success { value } = response.outcome else {
            panic!("the profile connection test must succeed");
        };
        let stage_summary = value
            .stages
            .iter()
            .map(|stage| {
                let diagnostic = stage.diagnostic.chars().take(240).collect::<String>();
                format!(
                    "phase={:?}, status={:?}, code={:?}, managed_mcp_id={:?}, diagnostic={diagnostic:?}",
                    stage.phase, stage.status, stage.code, stage.managed_mcp_id
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            value.passed,
            "the complete profile connection test must pass; stages: {stage_summary}"
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
            value.stages.len(),
            expected_phases.len(),
            "expected exactly one stage for each connection-test phase; stages: {stage_summary}"
        );
        assert!(
            value
                .stages
                .iter()
                .zip(expected_phases)
                .all(|(stage, expected_phase)| {
                    stage.phase == expected_phase && stage.status == McpConnectionTestStatus::Passed
                }),
            "expected stages in business order, each exactly once and Passed; stages: {stage_summary}"
        );
        let tool_visibility_stage = value
            .stages
            .iter()
            .find(|stage| stage.phase == McpConnectionTestPhase::ToolVisibility)
            .expect("the exact stage assertion above requires ToolVisibility");
        assert!(tool_visibility_stage
            .diagnostic
            .contains("intentionally sent no MCP tool definitions"));
        assert_eq!(&*factory_provider_ids.lock().unwrap(), &["anthropic"]);
        assert!(provider.completed.load(Ordering::SeqCst));
        assert_eq!(provider.called_with_tools.load(Ordering::SeqCst), 0);
    }
}
