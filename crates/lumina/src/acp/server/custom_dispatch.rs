use super::*;
use lumina_acp_macros::custom_methods;

fn safe_custom_dispatch_error(error: agent_client_protocol::Error) -> agent_client_protocol::Error {
    agent_client_protocol::Error::new(i32::from(error.code), "Lumina custom request failed")
        .data("acp_custom_dispatch_failed")
}

fn safe_custom_request_params_error() -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params().data("invalid_custom_request_params")
}

fn unavailable_custom_route_error() -> agent_client_protocol::Error {
    agent_client_protocol::Error::method_not_found().data("custom_route_not_available")
}

fn log_custom_request_failure(is_error: bool) {
    if is_error {
        tracing::error!(event = "acp_custom_request_failed");
    }
}

pub const UNAVAILABLE_CUSTOM_ROUTE_METHODS: &[&str] = &[
    MCP_SOURCE_ADAPTERS_LIST_METHOD,
    MCP_PROJECTION_RECOVERY_RESOLVE_METHOD,
];

pub fn is_unavailable_custom_route(method: &str) -> bool {
    UNAVAILABLE_CUSTOM_ROUTE_METHODS.contains(&method)
}

fn requires_mcp_platform_write_authority(method: &str) -> bool {
    [
        MCP_GOVERNED_IMPORT_METHOD,
        MCP_SOURCE_PROVISION_PREPARE_METHOD,
        MCP_SOURCE_PROVISION_CONFIRM_METHOD,
        MCP_SOURCE_REFRESH_METHOD,
        MCP_MANUAL_PLAN_CREATE_METHOD,
        MCP_PLAN_CREATE_METHOD,
        MCP_HTTPS_MANIFEST_PREPARE_METHOD,
        MCP_HTTPS_MANIFEST_CONFIRM_METHOD,
        MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD,
        MCP_INSTALL_CONFIRM_METHOD,
    ]
    .contains(&method)
}

pub fn available_custom_method_schemas(
    generator: &mut schemars::SchemaGenerator,
) -> Vec<crate::custom_requests::CustomMethodSchema> {
    LuminaAcpAgent::custom_method_schemas(generator)
        .into_iter()
        .filter(|schema| !is_unavailable_custom_route(&schema.method))
        .collect()
}

#[custom_methods]
impl LuminaAcpAgent {
    pub async fn dispatch_custom_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, agent_client_protocol::Error> {
        self.dispatch_custom_request_scoped(None, method, params)
            .await
    }

    pub(super) async fn dispatch_transport_custom_request(
        &self,
        authority: TransportSessionMcpWriteAuthority,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, agent_client_protocol::Error> {
        self.dispatch_custom_request_scoped(Some(authority), method, params)
            .await
    }

    async fn dispatch_custom_request_scoped(
        &self,
        authority: Option<TransportSessionMcpWriteAuthority>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, agent_client_protocol::Error> {
        let result = with_transport_mcp_platform_write_authority(authority, async {
            if requires_mcp_platform_write_authority(method)
                && current_transport_mcp_platform_write_authority().is_none()
            {
                return Err(unavailable_custom_route_error());
            }
            if is_unavailable_custom_route(method) {
                return Err(unavailable_custom_route_error());
            }

            if <SaveRecipeRequest as agent_client_protocol::JsonRpcMessage>::matches_method(method)
            {
                let req = recipe::deserialize_save_recipe_request(params)
                    .map_err(|_| safe_custom_request_params_error())?;
                let result = self
                    .on_save_recipe(req)
                    .await
                    .map_err(safe_custom_dispatch_error)?;
                return serde_json::to_value(&result).map_err(|_| {
                    safe_custom_dispatch_error(agent_client_protocol::Error::internal_error())
                });
            }

            let result = self.handle_custom_request(method, params).await;
            result.map_err(safe_custom_dispatch_error)
        })
        .await;

        log_custom_request_failure(result.is_err());

        result
    }

    #[custom_method(AddSessionExtensionRequest)]
    async fn dispatch_add_session_extension(
        &self,
        req: AddSessionExtensionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_add_session_extension(req).await
    }

    #[custom_method(McpTaskGetRequest)]
    async fn dispatch_mcp_task_get(
        &self,
        req: McpTaskGetRequest,
    ) -> Result<McpTaskGetResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_task_get(req).await)
    }

    #[custom_method(McpHttpsManifestPrepareRequest)]
    async fn dispatch_mcp_https_manifest_prepare(
        &self,
        req: McpHttpsManifestPrepareRequest,
    ) -> Result<McpHttpsManifestPrepareResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_https_manifest_prepare(req).await)
    }

    #[custom_method(McpHttpsManifestConfirmRequest)]
    async fn dispatch_mcp_https_manifest_confirm(
        &self,
        req: McpHttpsManifestConfirmRequest,
    ) -> Result<McpHttpsManifestConfirmResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_https_manifest_confirm(req).await)
    }

    #[custom_method(McpHttpsProvisionPlanCreateRequest)]
    async fn dispatch_mcp_https_provision_plan_create(
        &self,
        req: McpHttpsProvisionPlanCreateRequest,
    ) -> Result<McpHttpsProvisionPlanCreateResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_https_provision_plan_create(req).await)
    }

    #[custom_method(McpInstallConfirmRequest)]
    async fn dispatch_mcp_install_confirm(
        &self,
        req: McpInstallConfirmRequest,
    ) -> Result<McpInstallConfirmResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_install_confirm(req).await)
    }

    #[custom_method(McpTaskCancelRequest)]
    async fn dispatch_mcp_task_cancel(
        &self,
        req: McpTaskCancelRequest,
    ) -> Result<McpTaskCancelResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_task_cancel(req).await)
    }

    #[custom_method(McpTaskRetryRequest)]
    async fn dispatch_mcp_task_retry(
        &self,
        req: McpTaskRetryRequest,
    ) -> Result<McpTaskRetryResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_task_retry(req).await)
    }

    #[custom_method(McpEventsResumeRequest)]
    async fn dispatch_mcp_events_resume(
        &self,
        req: McpEventsResumeRequest,
    ) -> Result<McpEventsResumeResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_events_resume(req).await)
    }

    #[custom_method(McpListRequest)]
    async fn dispatch_mcp_list(
        &self,
        req: McpListRequest,
    ) -> Result<McpListResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_list(req).await)
    }

    #[custom_method(McpGetRequest)]
    async fn dispatch_mcp_get(
        &self,
        req: McpGetRequest,
    ) -> Result<McpGetResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_get(req).await)
    }

    #[custom_method(McpCatalogListRequest)]
    async fn dispatch_mcp_catalog_list(
        &self,
        req: McpCatalogListRequest,
    ) -> Result<McpCatalogListResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_catalog_list(req).await)
    }

    #[custom_method(McpCatalogDetailRequest)]
    async fn dispatch_mcp_catalog_detail(
        &self,
        req: McpCatalogDetailRequest,
    ) -> Result<McpCatalogDetailResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_catalog_detail(req).await)
    }

    #[custom_method(McpSourcesPolicyGetRequest)]
    async fn dispatch_mcp_sources_policy_get(
        &self,
        req: McpSourcesPolicyGetRequest,
    ) -> Result<McpSourcesPolicyGetResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_sources_policy_get(req).await)
    }

    #[custom_method(McpSourceRefreshRequest)]
    async fn dispatch_mcp_source_refresh(
        &self,
        req: McpSourceRefreshRequest,
    ) -> Result<McpSourceRefreshResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_source_refresh(req).await)
    }

    #[custom_method(McpSourceProvisionPrepareRequest)]
    async fn dispatch_mcp_source_provision_prepare(
        &self,
        req: McpSourceProvisionPrepareRequest,
    ) -> Result<McpSourceProvisionPrepareResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_source_provision_prepare(req).await)
    }

    #[custom_method(McpSourceProvisionConfirmRequest)]
    async fn dispatch_mcp_source_provision_confirm(
        &self,
        req: McpSourceProvisionConfirmRequest,
    ) -> Result<McpSourceProvisionConfirmResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_source_provision_confirm(req).await)
    }

    #[custom_method(McpGovernedImportRequest)]
    async fn dispatch_mcp_governed_import(
        &self,
        req: McpGovernedImportRequest,
    ) -> Result<McpGovernedImportResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_governed_import(req).await)
    }

    #[custom_method(McpManualStdioSourcesListRequest)]
    async fn dispatch_mcp_manual_stdio_sources_list(
        &self,
        req: McpManualStdioSourcesListRequest,
    ) -> Result<McpManualStdioSourcesListResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_manual_stdio_sources_list(req).await)
    }

    #[custom_method(McpManualPlanCreateRequest)]
    async fn dispatch_mcp_manual_plan_create(
        &self,
        req: McpManualPlanCreateRequest,
    ) -> Result<McpManualPlanCreateResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_manual_plan_create(req).await)
    }

    #[custom_method(McpPlanCreateRequest)]
    async fn dispatch_mcp_plan_create(
        &self,
        req: McpPlanCreateRequest,
    ) -> Result<McpPlanCreateResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_plan_create(req).await)
    }

    #[custom_method(McpHealthRunRequest)]
    async fn dispatch_mcp_health_run(
        &self,
        req: McpHealthRunRequest,
    ) -> Result<McpHealthRunResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_health_run(req).await)
    }

    #[custom_method(McpHealthGetRequest)]
    async fn dispatch_mcp_health_get(
        &self,
        req: McpHealthGetRequest,
    ) -> Result<McpHealthGetResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_health_get(req).await)
    }

    #[custom_method(McpSetDefaultEnabledRequest)]
    async fn dispatch_mcp_set_default_enabled(
        &self,
        req: McpSetDefaultEnabledRequest,
    ) -> Result<McpSetDefaultEnabledResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_set_default_enabled(req).await)
    }

    #[custom_method(McpRuntimeControlRequest)]
    async fn dispatch_mcp_runtime_control(
        &self,
        req: McpRuntimeControlRequest,
    ) -> Result<McpRuntimeControlResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_runtime_control(req).await)
    }

    #[custom_method(McpProfileListRequest)]
    async fn dispatch_mcp_profile_list(
        &self,
        req: McpProfileListRequest,
    ) -> Result<McpProfileListResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_list(req).await)
    }

    #[custom_method(McpProfileGetRequest)]
    async fn dispatch_mcp_profile_get(
        &self,
        req: McpProfileGetRequest,
    ) -> Result<McpProfileGetResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_get(req).await)
    }

    #[custom_method(McpProfileCreateRequest)]
    async fn dispatch_mcp_profile_create(
        &self,
        req: McpProfileCreateRequest,
    ) -> Result<McpProfileCreateResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_create(req).await)
    }

    #[custom_method(McpProfileUpdateRequest)]
    async fn dispatch_mcp_profile_update(
        &self,
        req: McpProfileUpdateRequest,
    ) -> Result<McpProfileUpdateResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_update(req).await)
    }

    #[custom_method(McpProfileRestoreRequest)]
    async fn dispatch_mcp_profile_restore(
        &self,
        req: McpProfileRestoreRequest,
    ) -> Result<McpProfileRestoreResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_restore(req).await)
    }

    #[custom_method(McpProfileArchiveRequest)]
    async fn dispatch_mcp_profile_archive(
        &self,
        req: McpProfileArchiveRequest,
    ) -> Result<McpProfileArchiveResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_archive(req).await)
    }

    #[custom_method(McpProfileDraftCreateRequest)]
    async fn dispatch_mcp_profile_draft_create(
        &self,
        req: McpProfileDraftCreateRequest,
    ) -> Result<McpProfileDraftCreateResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_draft_create(req).await)
    }

    #[custom_method(McpProfileModelRecommendRequest)]
    async fn dispatch_mcp_profile_model_recommend(
        &self,
        req: McpProfileModelRecommendRequest,
    ) -> Result<McpProfileModelRecommendResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_model_recommend(req).await)
    }

    #[custom_method(McpProfileConnectionTestRequest)]
    async fn dispatch_mcp_profile_connection_test(
        &self,
        req: McpProfileConnectionTestRequest,
    ) -> Result<McpProfileConnectionTestResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_connection_test(req).await)
    }

    #[custom_method(McpProfileApplyPlanCreateRequest)]
    async fn dispatch_mcp_profile_apply_plan_create(
        &self,
        req: McpProfileApplyPlanCreateRequest,
    ) -> Result<McpProfileApplyPlanCreateResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_apply_plan_create(req).await)
    }

    #[custom_method(McpProfileApplyConfirmRequest)]
    async fn dispatch_mcp_profile_apply_confirm(
        &self,
        req: McpProfileApplyConfirmRequest,
    ) -> Result<McpProfileApplyConfirmResponse, agent_client_protocol::Error> {
        Ok(self.on_mcp_profile_apply_confirm(req).await)
    }

    #[custom_method(RemoveSessionExtensionRequest)]
    async fn dispatch_remove_session_extension(
        &self,
        req: RemoveSessionExtensionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_remove_session_extension(req).await
    }

    #[custom_method(GetToolsRequest)]
    async fn dispatch_get_tools(
        &self,
        req: GetToolsRequest,
    ) -> Result<GetToolsResponse, agent_client_protocol::Error> {
        self.on_get_tools(req).await
    }

    #[custom_method(SetToolPermissionsRequest)]
    async fn dispatch_set_tool_permissions(
        &self,
        req: SetToolPermissionsRequest,
    ) -> Result<SetToolPermissionsResponse, agent_client_protocol::Error> {
        self.on_set_tool_permissions(req).await
    }

    #[custom_method(LuminaToolCallRequest)]
    async fn dispatch_call_tool(
        &self,
        req: LuminaToolCallRequest,
    ) -> Result<LuminaToolCallResponse, agent_client_protocol::Error> {
        self.on_call_tool(req).await
    }

    #[custom_method(ReadResourceRequest)]
    async fn dispatch_read_resource(
        &self,
        req: ReadResourceRequest,
    ) -> Result<ReadResourceResponse, agent_client_protocol::Error> {
        self.on_read_resource(req).await
    }

    #[custom_method(AppsListRequest)]
    async fn dispatch_list_apps(
        &self,
        req: AppsListRequest,
    ) -> Result<AppsListResponse, agent_client_protocol::Error> {
        self.on_list_apps(req).await
    }

    #[custom_method(AppsExportRequest)]
    async fn dispatch_export_app(
        &self,
        req: AppsExportRequest,
    ) -> Result<AppsExportResponse, agent_client_protocol::Error> {
        self.on_export_app(req).await
    }

    #[custom_method(AppsImportRequest)]
    async fn dispatch_import_app(
        &self,
        req: AppsImportRequest,
    ) -> Result<AppsImportResponse, agent_client_protocol::Error> {
        self.on_import_app(req).await
    }

    #[custom_method(AppsDeleteRequest)]
    async fn dispatch_delete_app(
        &self,
        req: AppsDeleteRequest,
    ) -> Result<AppsDeleteResponse, agent_client_protocol::Error> {
        self.on_delete_app(req).await
    }

    #[custom_method(UpdateWorkingDirRequest)]
    async fn dispatch_update_working_dir(
        &self,
        req: UpdateWorkingDirRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_update_working_dir(req).await
    }

    #[custom_method(SetSessionSystemPromptRequest)]
    async fn dispatch_set_session_system_prompt(
        &self,
        req: SetSessionSystemPromptRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_set_session_system_prompt(req).await
    }

    #[custom_method(SteerSessionRequest)]
    async fn dispatch_steer_session(
        &self,
        req: SteerSessionRequest,
    ) -> Result<SteerSessionResponse, agent_client_protocol::Error> {
        self.on_steer_session(req).await
    }

    #[custom_method(DiagnosticsGetRequest)]
    async fn dispatch_get_diagnostics(
        &self,
        req: DiagnosticsGetRequest,
    ) -> Result<DiagnosticsGetResponse, agent_client_protocol::Error> {
        self.on_get_diagnostics(req).await
    }

    #[custom_method(ListPromptsRequest)]
    async fn dispatch_list_prompts(
        &self,
        req: ListPromptsRequest,
    ) -> Result<ListPromptsResponse, agent_client_protocol::Error> {
        self.on_list_prompts(req).await
    }

    #[custom_method(GetPromptRequest)]
    async fn dispatch_get_prompt(
        &self,
        req: GetPromptRequest,
    ) -> Result<GetPromptResponse, agent_client_protocol::Error> {
        self.on_get_prompt(req).await
    }

    #[custom_method(SavePromptRequest)]
    async fn dispatch_save_prompt(
        &self,
        req: SavePromptRequest,
    ) -> Result<PromptOperationResponse, agent_client_protocol::Error> {
        self.on_save_prompt(req).await
    }

    #[custom_method(ResetPromptRequest)]
    async fn dispatch_reset_prompt(
        &self,
        req: ResetPromptRequest,
    ) -> Result<PromptOperationResponse, agent_client_protocol::Error> {
        self.on_reset_prompt(req).await
    }

    #[custom_method(DeleteSessionRequest)]
    async fn dispatch_delete_session(
        &self,
        req: DeleteSessionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_delete_session(req).await
    }

    #[custom_method(GetConfigExtensionsRequest)]
    async fn dispatch_get_config_extensions(
        &self,
    ) -> Result<GetConfigExtensionsResponse, agent_client_protocol::Error> {
        self.on_get_config_extensions().await
    }

    #[custom_method(GetAvailableExtensionsRequest)]
    async fn dispatch_get_available_extensions(
        &self,
    ) -> Result<GetAvailableExtensionsResponse, agent_client_protocol::Error> {
        self.on_get_available_extensions().await
    }

    #[custom_method(AddConfigExtensionRequest)]
    async fn dispatch_add_config_extension(
        &self,
        req: AddConfigExtensionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_add_config_extension(req).await
    }

    #[custom_method(RemoveConfigExtensionRequest)]
    async fn dispatch_remove_config_extension(
        &self,
        req: RemoveConfigExtensionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_remove_config_extension(req).await
    }

    #[custom_method(SetConfigExtensionEnabledRequest)]
    async fn dispatch_set_config_extension_enabled(
        &self,
        req: SetConfigExtensionEnabledRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_set_config_extension_enabled(req).await
    }

    #[custom_method(GetSessionExtensionsRequest)]
    async fn dispatch_get_session_extensions(
        &self,
        req: GetSessionExtensionsRequest,
    ) -> Result<GetSessionExtensionsResponse, agent_client_protocol::Error> {
        self.on_get_session_extensions(req).await
    }

    #[custom_method(ListProvidersRequest)]
    async fn dispatch_list_providers(
        &self,
        req: ListProvidersRequest,
    ) -> Result<ListProvidersResponse, agent_client_protocol::Error> {
        self.on_list_providers(req).await
    }

    #[custom_method(ProviderSupportedModelsListRequest)]
    async fn dispatch_list_provider_supported_models(
        &self,
        req: ProviderSupportedModelsListRequest,
    ) -> Result<ProviderSupportedModelsListResponse, agent_client_protocol::Error> {
        self.on_list_provider_supported_models(req).await
    }

    #[custom_method(ProviderCatalogListRequest)]
    async fn dispatch_list_provider_catalog(
        &self,
        req: ProviderCatalogListRequest,
    ) -> Result<ProviderCatalogListResponse, agent_client_protocol::Error> {
        self.on_list_provider_catalog(req).await
    }

    #[custom_method(ProviderSetupCatalogListRequest)]
    async fn dispatch_list_provider_setup_catalog(
        &self,
        req: ProviderSetupCatalogListRequest,
    ) -> Result<ProviderSetupCatalogListResponse, agent_client_protocol::Error> {
        self.on_list_provider_setup_catalog(req).await
    }

    #[custom_method(ProviderCatalogTemplateRequest)]
    async fn dispatch_get_provider_catalog_template(
        &self,
        req: ProviderCatalogTemplateRequest,
    ) -> Result<ProviderCatalogTemplateResponse, agent_client_protocol::Error> {
        self.on_get_provider_catalog_template(req).await
    }

    #[custom_method(CustomProviderCreateRequest)]
    async fn dispatch_create_custom_provider(
        &self,
        req: CustomProviderCreateRequest,
    ) -> Result<CustomProviderCreateResponse, agent_client_protocol::Error> {
        self.on_create_custom_provider(req).await
    }

    #[custom_method(CustomProviderReadRequest)]
    async fn dispatch_read_custom_provider(
        &self,
        req: CustomProviderReadRequest,
    ) -> Result<CustomProviderReadResponse, agent_client_protocol::Error> {
        self.on_read_custom_provider(req).await
    }

    #[custom_method(CustomProviderUpdateRequest)]
    async fn dispatch_update_custom_provider(
        &self,
        req: CustomProviderUpdateRequest,
    ) -> Result<CustomProviderUpdateResponse, agent_client_protocol::Error> {
        self.on_update_custom_provider(req).await
    }

    #[custom_method(CustomProviderDeleteRequest)]
    async fn dispatch_delete_custom_provider(
        &self,
        req: CustomProviderDeleteRequest,
    ) -> Result<CustomProviderDeleteResponse, agent_client_protocol::Error> {
        self.on_delete_custom_provider(req).await
    }

    #[custom_method(RefreshProviderInventoryRequest)]
    async fn dispatch_refresh_provider_inventory(
        &self,
        req: RefreshProviderInventoryRequest,
    ) -> Result<RefreshProviderInventoryResponse, agent_client_protocol::Error> {
        self.on_refresh_provider_inventory(req).await
    }

    #[custom_method(ProviderConfigReadRequest)]
    async fn dispatch_read_provider_config(
        &self,
        req: ProviderConfigReadRequest,
    ) -> Result<ProviderConfigReadResponse, agent_client_protocol::Error> {
        self.on_read_provider_config(req).await
    }

    #[custom_method(ProviderConfigStatusRequest)]
    async fn dispatch_provider_config_status(
        &self,
        req: ProviderConfigStatusRequest,
    ) -> Result<ProviderConfigStatusResponse, agent_client_protocol::Error> {
        self.on_provider_config_status(req).await
    }

    #[custom_method(ProviderConfigSaveRequest)]
    async fn dispatch_save_provider_config(
        &self,
        req: ProviderConfigSaveRequest,
    ) -> Result<ProviderConfigChangeResponse, agent_client_protocol::Error> {
        self.on_save_provider_config(req).await
    }

    #[custom_method(ProviderConfigDeleteRequest)]
    async fn dispatch_delete_provider_config(
        &self,
        req: ProviderConfigDeleteRequest,
    ) -> Result<ProviderConfigChangeResponse, agent_client_protocol::Error> {
        self.on_delete_provider_config(req).await
    }

    #[custom_method(ProviderConfigAuthenticateRequest)]
    async fn dispatch_authenticate_provider_config(
        &self,
        req: ProviderConfigAuthenticateRequest,
    ) -> Result<ProviderConfigChangeResponse, agent_client_protocol::Error> {
        self.on_authenticate_provider_config(req).await
    }

    #[custom_method(ProviderSecretsListRequest)]
    async fn dispatch_list_provider_secrets(
        &self,
        req: ProviderSecretsListRequest,
    ) -> Result<ProviderSecretsListResponse, agent_client_protocol::Error> {
        self.on_list_provider_secrets(req).await
    }

    #[custom_method(ProviderSecretDeleteRequest)]
    async fn dispatch_delete_provider_secret(
        &self,
        req: ProviderSecretDeleteRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_delete_provider_secret(req).await
    }

    #[custom_method(CanonicalModelInfoRequest)]
    async fn dispatch_canonical_model_info(
        &self,
        req: CanonicalModelInfoRequest,
    ) -> Result<CanonicalModelInfoResponse, agent_client_protocol::Error> {
        self.on_canonical_model_info(req).await
    }

    #[custom_method(PreferencesReadRequest)]
    async fn dispatch_preferences_read(
        &self,
        req: PreferencesReadRequest,
    ) -> Result<PreferencesReadResponse, agent_client_protocol::Error> {
        self.on_preferences_read(req).await
    }

    #[custom_method(PreferencesSaveRequest)]
    async fn dispatch_preferences_save(
        &self,
        req: PreferencesSaveRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_preferences_save(req).await
    }

    #[custom_method(PreferencesRemoveRequest)]
    async fn dispatch_preferences_remove(
        &self,
        req: PreferencesRemoveRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_preferences_remove(req).await
    }

    #[custom_method(ConfigReadRequest)]
    async fn dispatch_config_read(
        &self,
        req: ConfigReadRequest,
    ) -> Result<ConfigReadResponse, agent_client_protocol::Error> {
        self.on_config_read(req).await
    }

    #[custom_method(ConfigUpsertRequest)]
    async fn dispatch_config_upsert(
        &self,
        req: ConfigUpsertRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_config_upsert(req).await
    }

    #[custom_method(ConfigRemoveRequest)]
    async fn dispatch_config_remove(
        &self,
        req: ConfigRemoveRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_config_remove(req).await
    }

    #[custom_method(ConfigReadAllRequest)]
    async fn dispatch_config_read_all(
        &self,
        req: ConfigReadAllRequest,
    ) -> Result<ConfigReadAllResponse, agent_client_protocol::Error> {
        self.on_config_read_all(req).await
    }

    #[custom_method(DefaultsReadRequest)]
    async fn dispatch_defaults_read(
        &self,
        req: DefaultsReadRequest,
    ) -> Result<DefaultsReadResponse, agent_client_protocol::Error> {
        self.on_defaults_read(req).await
    }

    #[custom_method(DefaultsSaveRequest)]
    async fn dispatch_defaults_save(
        &self,
        req: DefaultsSaveRequest,
    ) -> Result<DefaultsReadResponse, agent_client_protocol::Error> {
        self.on_defaults_save(req).await
    }

    #[custom_method(DefaultsClearRequest)]
    async fn dispatch_defaults_clear(
        &self,
        req: DefaultsClearRequest,
    ) -> Result<DefaultsReadResponse, agent_client_protocol::Error> {
        self.on_defaults_clear(req).await
    }

    #[custom_method(OnboardingImportScanRequest)]
    async fn dispatch_onboarding_import_scan(
        &self,
        req: OnboardingImportScanRequest,
    ) -> Result<OnboardingImportScanResponse, agent_client_protocol::Error> {
        self.on_onboarding_import_scan(req).await
    }

    #[custom_method(OnboardingImportApplyRequest)]
    async fn dispatch_onboarding_import_apply(
        &self,
        req: OnboardingImportApplyRequest,
    ) -> Result<OnboardingImportApplyResponse, agent_client_protocol::Error> {
        self.on_onboarding_import_apply(req).await
    }

    #[custom_method(ExportSessionRequest)]
    async fn dispatch_export_session(
        &self,
        req: ExportSessionRequest,
    ) -> Result<ExportSessionResponse, agent_client_protocol::Error> {
        self.on_export_session(req).await
    }

    #[custom_method(ImportSessionRequest)]
    async fn dispatch_import_session(
        &self,
        req: ImportSessionRequest,
    ) -> Result<ImportSessionResponse, agent_client_protocol::Error> {
        self.on_import_session(req).await
    }

    #[custom_method(EncodeRecipeRequest)]
    async fn dispatch_encode_recipe(
        &self,
        req: EncodeRecipeRequest,
    ) -> Result<EncodeRecipeResponse, agent_client_protocol::Error> {
        self.on_encode_recipe(req).await
    }

    #[custom_method(DecodeRecipeRequest)]
    async fn dispatch_decode_recipe(
        &self,
        req: DecodeRecipeRequest,
    ) -> Result<DecodeRecipeResponse, agent_client_protocol::Error> {
        self.on_decode_recipe(req).await
    }

    #[custom_method(ScanRecipeRequest)]
    async fn dispatch_scan_recipe(
        &self,
        req: ScanRecipeRequest,
    ) -> Result<ScanRecipeResponse, agent_client_protocol::Error> {
        self.on_scan_recipe(req).await
    }

    #[custom_method(ListRecipesRequest)]
    async fn dispatch_list_recipes(
        &self,
        req: ListRecipesRequest,
    ) -> Result<ListRecipesResponse, agent_client_protocol::Error> {
        self.on_list_recipes(req).await
    }

    #[custom_method(DeleteRecipeRequest)]
    async fn dispatch_delete_recipe(
        &self,
        req: DeleteRecipeRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_delete_recipe(req).await
    }

    #[custom_method(ScheduleRecipeRequest)]
    async fn dispatch_schedule_recipe(
        &self,
        req: ScheduleRecipeRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_schedule_recipe(req).await
    }

    #[custom_method(SetRecipeSlashCommandRequest)]
    async fn dispatch_set_recipe_slash_command(
        &self,
        req: SetRecipeSlashCommandRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_set_recipe_slash_command(req).await
    }

    #[custom_method(SaveRecipeRequest)]
    async fn dispatch_save_recipe(
        &self,
        req: SaveRecipeRequest,
    ) -> Result<SaveRecipeResponse, agent_client_protocol::Error> {
        self.on_save_recipe(req).await
    }

    #[custom_method(ParseRecipeRequest)]
    async fn dispatch_parse_recipe(
        &self,
        req: ParseRecipeRequest,
    ) -> Result<ParseRecipeResponse, agent_client_protocol::Error> {
        self.on_parse_recipe(req).await
    }

    #[custom_method(RecipeToYamlRequest)]
    async fn dispatch_recipe_to_yaml(
        &self,
        req: RecipeToYamlRequest,
    ) -> Result<RecipeToYamlResponse, agent_client_protocol::Error> {
        self.on_recipe_to_yaml(req).await
    }

    #[custom_method(ListSchedulesRequest)]
    async fn dispatch_list_schedules(
        &self,
        req: ListSchedulesRequest,
    ) -> Result<ListSchedulesResponse, agent_client_protocol::Error> {
        self.on_list_schedules(req).await
    }

    #[custom_method(ListScheduleSessionsRequest)]
    async fn dispatch_list_schedule_sessions(
        &self,
        req: ListScheduleSessionsRequest,
    ) -> Result<ListScheduleSessionsResponse, agent_client_protocol::Error> {
        self.on_list_schedule_sessions(req).await
    }

    #[custom_method(CreateScheduleRequest)]
    async fn dispatch_create_schedule(
        &self,
        req: CreateScheduleRequest,
    ) -> Result<CreateScheduleResponse, agent_client_protocol::Error> {
        self.on_create_schedule(req).await
    }

    #[custom_method(DeleteScheduleRequest)]
    async fn dispatch_delete_schedule(
        &self,
        req: DeleteScheduleRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_delete_schedule(req).await
    }

    #[custom_method(PauseScheduleRequest)]
    async fn dispatch_pause_schedule(
        &self,
        req: PauseScheduleRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_pause_schedule(req).await
    }

    #[custom_method(UnpauseScheduleRequest)]
    async fn dispatch_unpause_schedule(
        &self,
        req: UnpauseScheduleRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_unpause_schedule(req).await
    }

    #[custom_method(UpdateScheduleRequest)]
    async fn dispatch_update_schedule(
        &self,
        req: UpdateScheduleRequest,
    ) -> Result<UpdateScheduleResponse, agent_client_protocol::Error> {
        self.on_update_schedule(req).await
    }

    #[custom_method(RunScheduleNowRequest)]
    async fn dispatch_run_schedule_now(
        &self,
        req: RunScheduleNowRequest,
    ) -> Result<RunScheduleNowResponse, agent_client_protocol::Error> {
        self.on_run_schedule_now(req).await
    }

    #[custom_method(KillRunningJobRequest)]
    async fn dispatch_kill_running_job(
        &self,
        req: KillRunningJobRequest,
    ) -> Result<KillRunningJobResponse, agent_client_protocol::Error> {
        self.on_kill_running_job(req).await
    }

    #[custom_method(InspectRunningJobRequest)]
    async fn dispatch_inspect_running_job(
        &self,
        req: InspectRunningJobRequest,
    ) -> Result<InspectRunningJobResponse, agent_client_protocol::Error> {
        self.on_inspect_running_job(req).await
    }

    #[custom_method(GetSessionInfoRequest)]
    async fn dispatch_get_session_info(
        &self,
        req: GetSessionInfoRequest,
    ) -> Result<GetSessionInfoResponse, agent_client_protocol::Error> {
        self.on_get_session_info(req).await
    }

    #[custom_method(TruncateSessionConversationRequest)]
    async fn dispatch_truncate_session_conversation(
        &self,
        req: TruncateSessionConversationRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_truncate_session_conversation(req).await
    }

    #[custom_method(UpdateSessionProjectRequest)]
    async fn dispatch_update_session_project(
        &self,
        req: UpdateSessionProjectRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_update_session_project(req).await
    }

    #[custom_method(RenameSessionRequest)]
    async fn dispatch_rename_session(
        &self,
        req: RenameSessionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_rename_session(req).await
    }

    #[custom_method(ArchiveSessionRequest)]
    async fn dispatch_archive_session(
        &self,
        req: ArchiveSessionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_archive_session(req).await
    }

    #[custom_method(UnarchiveSessionRequest)]
    async fn dispatch_unarchive_session(
        &self,
        req: UnarchiveSessionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_unarchive_session(req).await
    }

    #[custom_method(CreateSourceRequest)]
    async fn dispatch_create_source(
        &self,
        req: CreateSourceRequest,
    ) -> Result<CreateSourceResponse, agent_client_protocol::Error> {
        self.on_create_source(req).await
    }

    #[custom_method(ListSourcesRequest)]
    async fn dispatch_list_sources(
        &self,
        req: ListSourcesRequest,
    ) -> Result<ListSourcesResponse, agent_client_protocol::Error> {
        self.on_list_sources(req).await
    }

    #[custom_method(ListAgentMentionsRequest)]
    async fn dispatch_list_agent_mentions(
        &self,
        req: ListAgentMentionsRequest,
    ) -> Result<ListAgentMentionsResponse, agent_client_protocol::Error> {
        self.on_list_agent_mentions(req).await
    }

    #[custom_method(ListSlashCommandsRequest)]
    async fn dispatch_list_slash_commands(
        &self,
        req: ListSlashCommandsRequest,
    ) -> Result<ListSlashCommandsResponse, agent_client_protocol::Error> {
        self.on_list_slash_commands(req).await
    }

    #[custom_method(UpdateSourceRequest)]
    async fn dispatch_update_source(
        &self,
        req: UpdateSourceRequest,
    ) -> Result<UpdateSourceResponse, agent_client_protocol::Error> {
        self.on_update_source(req).await
    }

    #[custom_method(DeleteSourceRequest)]
    async fn dispatch_delete_source(
        &self,
        req: DeleteSourceRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_delete_source(req).await
    }

    #[custom_method(ExportSourceRequest)]
    async fn dispatch_export_source(
        &self,
        req: ExportSourceRequest,
    ) -> Result<ExportSourceResponse, agent_client_protocol::Error> {
        self.on_export_source(req).await
    }

    #[custom_method(ImportSourcesRequest)]
    async fn dispatch_import_sources(
        &self,
        req: ImportSourcesRequest,
    ) -> Result<ImportSourcesResponse, agent_client_protocol::Error> {
        self.on_import_sources(req).await
    }

    #[custom_method(DictationTranscribeRequest)]
    async fn dispatch_dictation_transcribe(
        &self,
        req: DictationTranscribeRequest,
    ) -> Result<DictationTranscribeResponse, agent_client_protocol::Error> {
        self.on_dictation_transcribe(req).await
    }

    #[custom_method(DictationConfigRequest)]
    async fn dispatch_dictation_config(
        &self,
        _req: DictationConfigRequest,
    ) -> Result<DictationConfigResponse, agent_client_protocol::Error> {
        self.on_dictation_config(_req).await
    }

    #[custom_method(DictationSecretSaveRequest)]
    async fn dispatch_dictation_secret_save(
        &self,
        req: DictationSecretSaveRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_dictation_secret_save(req).await
    }

    #[custom_method(DictationSecretDeleteRequest)]
    async fn dispatch_dictation_secret_delete(
        &self,
        req: DictationSecretDeleteRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_dictation_secret_delete(req).await
    }

    #[custom_method(DictationModelsListRequest)]
    async fn dispatch_dictation_models_list(
        &self,
        _req: DictationModelsListRequest,
    ) -> Result<DictationModelsListResponse, agent_client_protocol::Error> {
        self.on_dictation_models_list(_req).await
    }

    #[custom_method(DictationModelDownloadRequest)]
    async fn dispatch_dictation_model_download(
        &self,
        _req: DictationModelDownloadRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_dictation_model_download(_req).await
    }

    #[custom_method(DictationModelDownloadProgressRequest)]
    async fn dispatch_dictation_model_download_progress(
        &self,
        _req: DictationModelDownloadProgressRequest,
    ) -> Result<DictationModelDownloadProgressResponse, agent_client_protocol::Error> {
        self.on_dictation_model_download_progress(_req).await
    }

    #[custom_method(DictationModelCancelRequest)]
    async fn dispatch_dictation_model_cancel(
        &self,
        _req: DictationModelCancelRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_dictation_model_cancel(_req).await
    }

    #[custom_method(DictationModelDeleteRequest)]
    async fn dispatch_dictation_model_delete(
        &self,
        _req: DictationModelDeleteRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_dictation_model_delete(_req).await
    }

    #[custom_method(DictationModelSelectRequest)]
    async fn dispatch_dictation_model_select(
        &self,
        req: DictationModelSelectRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_dictation_model_select(req).await
    }

    #[custom_method(LocalInferenceModelsListRequest)]
    async fn dispatch_local_inference_models_list(
        &self,
        req: LocalInferenceModelsListRequest,
    ) -> Result<LocalInferenceModelsListResponse, agent_client_protocol::Error> {
        self.on_local_inference_models_list(req).await
    }

    #[custom_method(LocalInferenceModelDownloadRequest)]
    async fn dispatch_local_inference_model_download(
        &self,
        req: LocalInferenceModelDownloadRequest,
    ) -> Result<LocalInferenceModelDownloadResponse, agent_client_protocol::Error> {
        self.on_local_inference_model_download(req).await
    }

    #[custom_method(LocalInferenceModelDownloadProgressRequest)]
    async fn dispatch_local_inference_model_download_progress(
        &self,
        req: LocalInferenceModelDownloadProgressRequest,
    ) -> Result<LocalInferenceModelDownloadProgressResponse, agent_client_protocol::Error> {
        self.on_local_inference_model_download_progress(req).await
    }

    #[custom_method(LocalInferenceModelDownloadCancelRequest)]
    async fn dispatch_local_inference_model_download_cancel(
        &self,
        req: LocalInferenceModelDownloadCancelRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_local_inference_model_download_cancel(req).await
    }

    #[custom_method(LocalInferenceModelDeleteRequest)]
    async fn dispatch_local_inference_model_delete(
        &self,
        req: LocalInferenceModelDeleteRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_local_inference_model_delete(req).await
    }

    #[custom_method(LocalInferenceModelEvictRequest)]
    async fn dispatch_local_inference_model_evict(
        &self,
        req: LocalInferenceModelEvictRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.on_local_inference_model_evict(req).await
    }

    #[custom_method(LocalInferenceModelSettingsReadRequest)]
    async fn dispatch_local_inference_model_settings_read(
        &self,
        req: LocalInferenceModelSettingsReadRequest,
    ) -> Result<LocalInferenceModelSettingsReadResponse, agent_client_protocol::Error> {
        self.on_local_inference_model_settings_read(req).await
    }

    #[custom_method(LocalInferenceModelSettingsUpdateRequest)]
    async fn dispatch_local_inference_model_settings_update(
        &self,
        req: LocalInferenceModelSettingsUpdateRequest,
    ) -> Result<LocalInferenceModelSettingsUpdateResponse, agent_client_protocol::Error> {
        self.on_local_inference_model_settings_update(req).await
    }

    #[custom_method(LocalInferenceHuggingFaceSearchRequest)]
    async fn dispatch_local_inference_huggingface_search(
        &self,
        req: LocalInferenceHuggingFaceSearchRequest,
    ) -> Result<LocalInferenceHuggingFaceSearchResponse, agent_client_protocol::Error> {
        self.on_local_inference_huggingface_search(req).await
    }

    #[custom_method(LocalInferenceHuggingFaceRepoVariantsRequest)]
    async fn dispatch_local_inference_huggingface_repo_variants(
        &self,
        req: LocalInferenceHuggingFaceRepoVariantsRequest,
    ) -> Result<LocalInferenceHuggingFaceRepoVariantsResponse, agent_client_protocol::Error> {
        self.on_local_inference_huggingface_repo_variants(req).await
    }

    #[custom_method(LocalInferenceBuiltinChatTemplatesListRequest)]
    async fn dispatch_local_inference_builtin_chat_templates_list(
        &self,
        req: LocalInferenceBuiltinChatTemplatesListRequest,
    ) -> Result<LocalInferenceBuiltinChatTemplatesListResponse, agent_client_protocol::Error> {
        self.on_local_inference_builtin_chat_templates_list(req)
            .await
    }

    #[custom_method(ProjectOpenRequest)]
    async fn dispatch_project_open(
        &self,
        req: ProjectOpenRequest,
    ) -> Result<ProjectOpenResponse, agent_client_protocol::Error> {
        self.on_project_open(req).await
    }

    #[custom_method(ProjectListRequest)]
    async fn dispatch_project_list(
        &self,
        req: ProjectListRequest,
    ) -> Result<ProjectListResponse, agent_client_protocol::Error> {
        self.on_project_list(req).await
    }

    #[custom_method(ProjectSnapshotRequest)]
    async fn dispatch_project_snapshot(
        &self,
        req: ProjectSnapshotRequest,
    ) -> Result<ProjectSnapshotResponse, agent_client_protocol::Error> {
        self.on_project_snapshot(req).await
    }

    #[custom_method(ProjectWorkItemCreateRequest)]
    async fn dispatch_project_work_item_create(
        &self,
        req: ProjectWorkItemCreateRequest,
    ) -> Result<ProjectWorkItemResponse, agent_client_protocol::Error> {
        self.on_project_work_item_create(req).await
    }

    #[custom_method(ProjectWorkItemCompleteRequest)]
    async fn dispatch_project_work_item_complete(
        &self,
        req: ProjectWorkItemCompleteRequest,
    ) -> Result<ProjectWorkItemResponse, agent_client_protocol::Error> {
        self.on_project_work_item_complete(req).await
    }

    #[custom_method(ProjectRunStartRequest)]
    async fn dispatch_project_run_start(
        &self,
        req: ProjectRunStartRequest,
    ) -> Result<ProjectRunResponse, agent_client_protocol::Error> {
        self.on_project_run_start(req).await
    }

    #[custom_method(ProjectRunPhaseRequest)]
    async fn dispatch_project_run_phase(
        &self,
        req: ProjectRunPhaseRequest,
    ) -> Result<ProjectRunResponse, agent_client_protocol::Error> {
        self.on_project_run_phase(req).await
    }

    #[custom_method(ProjectRunFinishRequest)]
    async fn dispatch_project_run_finish(
        &self,
        req: ProjectRunFinishRequest,
    ) -> Result<ProjectRunResponse, agent_client_protocol::Error> {
        self.on_project_run_finish(req).await
    }

    #[custom_method(ProjectCheckpointCreateRequest)]
    async fn dispatch_project_checkpoint_create(
        &self,
        req: ProjectCheckpointCreateRequest,
    ) -> Result<ProjectCheckpointResponse, agent_client_protocol::Error> {
        self.on_project_checkpoint_create(req).await
    }

    #[custom_method(ProjectChangeSetGetRequest)]
    async fn dispatch_project_change_set_get(
        &self,
        req: ProjectChangeSetGetRequest,
    ) -> Result<ProjectChangeSetResponse, agent_client_protocol::Error> {
        self.on_project_change_set_get(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::JsonRpcMessage;
    use std::fmt;
    use std::sync::{Arc, Mutex};
    use tracing::field::Visit;
    use tracing::{Event, Subscriber};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::Layer;

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<String>>>);

    impl<S> Layer<S> for CapturedLogs
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            let mut visitor = CapturedFields::default();
            event.record(&mut visitor);
            self.0.lock().unwrap().push(visitor.0);
        }
    }

    #[derive(Default)]
    struct CapturedFields(String);

    impl Visit for CapturedFields {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push_str(field.name());
            self.0.push('=');
            self.0.push_str(value);
            self.0.push(';');
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
            self.0.push_str(field.name());
            self.0.push('=');
            self.0.push_str(&format!("{value:?}"));
            self.0.push(';');
        }
    }

    fn assert_redacted(error: agent_client_protocol::Error, secrets: &[&str]) {
        let display = error.to_string();
        let debug = format!("{error:?}");
        let serialized = serde_json::to_string(&error).unwrap();

        for secret in secrets {
            assert!(!display.contains(secret));
            assert!(!debug.contains(secret));
            assert!(!serialized.contains(secret));
        }
    }

    #[test]
    fn mcp_center_routes_are_registered_and_transport_protected() {
        const METHOD_SECRET: &str = "acp-method-secret";
        const ID_SECRET: &str = "acp-id-secret";
        const PAGE_SIZE_SECRET: &str = "acp-page-size-secret";
        const PARAMS_SECRET: &str = "acp-params-secret";
        let params = serde_json::json!({
            "pageSize": PAGE_SIZE_SECRET,
            "query": PARAMS_SECRET,
        });

        assert_eq!(
            UNAVAILABLE_CUSTOM_ROUTE_METHODS,
            &[
                MCP_SOURCE_ADAPTERS_LIST_METHOD,
                MCP_PROJECTION_RECOVERY_RESOLVE_METHOD,
            ]
        );
        assert!(McpGovernedImportRequest::matches_method(
            MCP_GOVERNED_IMPORT_METHOD
        ));
        assert!(McpSourceProvisionPrepareRequest::matches_method(
            MCP_SOURCE_PROVISION_PREPARE_METHOD
        ));
        assert!(McpSourceProvisionConfirmRequest::matches_method(
            MCP_SOURCE_PROVISION_CONFIRM_METHOD
        ));
        assert!(McpCatalogListRequest::matches_method(
            MCP_CATALOG_LIST_METHOD
        ));
        assert!(McpCatalogDetailRequest::matches_method(
            MCP_CATALOG_DETAIL_METHOD
        ));
        assert!(McpSourcesPolicyGetRequest::matches_method(
            MCP_SOURCES_POLICY_GET_METHOD
        ));
        assert!(McpSourceRefreshRequest::matches_method(
            MCP_SOURCE_REFRESH_METHOD
        ));
        assert!(McpSourceAdaptersListRequest::matches_method(
            MCP_SOURCE_ADAPTERS_LIST_METHOD
        ));
        assert!(McpManualStdioSourcesListRequest::matches_method(
            MCP_MANUAL_STDIO_SOURCES_LIST_METHOD
        ));
        assert!(McpManualPlanCreateRequest::matches_method(
            MCP_MANUAL_PLAN_CREATE_METHOD
        ));
        assert!(McpPlanCreateRequest::matches_method(MCP_PLAN_CREATE_METHOD));
        assert!(McpInstallConfirmRequest::matches_method(
            MCP_INSTALL_CONFIRM_METHOD
        ));
        assert!(McpProjectionRecoveryResolveRequest::matches_method(
            MCP_PROJECTION_RECOVERY_RESOLVE_METHOD
        ));
        for method in [
            MCP_GOVERNED_IMPORT_METHOD,
            MCP_SOURCE_PROVISION_PREPARE_METHOD,
            MCP_SOURCE_PROVISION_CONFIRM_METHOD,
            MCP_SOURCE_REFRESH_METHOD,
            MCP_MANUAL_PLAN_CREATE_METHOD,
            MCP_PLAN_CREATE_METHOD,
            MCP_HTTPS_MANIFEST_PREPARE_METHOD,
            MCP_HTTPS_MANIFEST_CONFIRM_METHOD,
            MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD,
            MCP_INSTALL_CONFIRM_METHOD,
        ] {
            assert!(requires_mcp_platform_write_authority(method));
        }
        for method in [
            MCP_CATALOG_LIST_METHOD,
            MCP_CATALOG_DETAIL_METHOD,
            MCP_SOURCES_POLICY_GET_METHOD,
            MCP_MANUAL_STDIO_SOURCES_LIST_METHOD,
            MCP_PROJECTION_RECOVERY_RESOLVE_METHOD,
        ] {
            assert!(!requires_mcp_platform_write_authority(method));
        }
        assert!(!is_unavailable_custom_route(METHOD_SECRET));
        assert_eq!(params["pageSize"], PAGE_SIZE_SECRET);
        let conversion: std::result::Result<McpCatalogListRequest, _> =
            serde_json::from_value(params);
        assert!(conversion.is_err());
        assert_redacted(
            conversion
                .map_err(|_| safe_custom_request_params_error())
                .unwrap_err(),
            &[METHOD_SECRET, ID_SECRET, PAGE_SIZE_SECRET, PARAMS_SECRET],
        );
        assert_redacted(
            unavailable_custom_route_error(),
            &[METHOD_SECRET, ID_SECRET, PAGE_SIZE_SECRET, PARAMS_SECRET],
        );

        let mut generator = schemars::SchemaGenerator::default();
        let schemas = available_custom_method_schemas(&mut generator);
        for method in [
            MCP_CATALOG_LIST_METHOD,
            MCP_SOURCES_POLICY_GET_METHOD,
            MCP_MANUAL_STDIO_SOURCES_LIST_METHOD,
            MCP_MANUAL_PLAN_CREATE_METHOD,
            MCP_PLAN_CREATE_METHOD,
            MCP_INSTALL_CONFIRM_METHOD,
        ] {
            assert!(schemas.iter().any(|schema| schema.method == method));
        }
    }

    #[test]
    fn https_provision_plan_route_is_strict_and_transport_bound() {
        assert!(McpHttpsProvisionPlanCreateRequest::matches_method(
            MCP_HTTPS_PROVISION_PLAN_CREATE_METHOD
        ));
        let params = serde_json::json!({
            "provisionId": "provision",
            "expectedManifestDigest": "digest",
            "idempotencyKey": "request",
            "actor": "forged",
            "sourceUrl": "https://attacker.invalid/manifest.json"
        });
        assert!(serde_json::from_value::<McpHttpsProvisionPlanCreateRequest>(params).is_err());
    }

    #[test]
    fn custom_dispatch_logs_only_a_fixed_event_and_leaves_standard_methods_unmatched() {
        const SECRET: &str = "acp-tracing-secret";
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::registry().with(logs.clone());

        tracing::subscriber::with_default(subscriber, || {
            log_custom_request_failure(true);
        });

        let captured = logs.0.lock().unwrap().join("\n");
        assert!(captured.contains("acp_custom_request_failed"));
        assert!(!captured.contains(SECRET));
        assert!(!is_unavailable_custom_route("initialize"));
        assert!(!is_unavailable_custom_route("session/cancel"));
    }
}
