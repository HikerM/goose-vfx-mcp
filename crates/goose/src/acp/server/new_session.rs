use crate::acp::custom_requests::GooseExtension;
use crate::acp::server::{meta_string, validate_absolute_cwd, ResultExt};
use crate::agents::ExtensionLoadResult;
use crate::config::{Config, GooseMode};
use crate::mcp_platform::{ConsumedProfileApplication, ProfileApplicationMarker};
use crate::recipe::{Recipe, Settings};
use crate::session::{EnabledExtensionsState, ExtensionData, ExtensionState, Session, SessionType};

use super::GooseAcpAgent;
use agent_client_protocol::schema::v1::{Meta, NewSessionRequest, NewSessionResponse, SessionId};
use agent_client_protocol::{Client, ConnectionTo};
use goose_providers::model::ModelConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::warn;

const PROFILE_APPLICATION_REJECTED: &str =
    "profile application rejected: invalid_profile_application_token";

struct InitialSessionConfig {
    provider: String,
    model_config: ModelConfig,
    extension_data: ExtensionData,
    recipe: Option<Recipe>,
    user_recipe_values: Option<HashMap<String, String>>,
    project_id: Option<String>,
}

struct PreparedRecipe {
    source: Option<(Recipe, PathBuf)>,
    rendered: Option<Recipe>,
    user_recipe_values: Option<HashMap<String, String>>,
}

struct NewSessionCleanupResult {
    session_deleted: bool,
    agent_removed: bool,
}

impl NewSessionCleanupResult {
    fn complete(&self) -> bool {
        self.session_deleted && self.agent_removed
    }

    fn failure_code(&self) -> &'static str {
        match (self.session_deleted, self.agent_removed) {
            (false, false) => "session_and_agent_cleanup_failed",
            (false, true) => "session_cleanup_failed",
            (true, false) => "agent_cleanup_failed",
            (true, true) => "session_cleanup_complete",
        }
    }
}

impl GooseAcpAgent {
    pub(super) async fn handle_new_session(
        &self,
        cx: &ConnectionTo<Client>,
        args: NewSessionRequest,
    ) -> Result<NewSessionResponse, agent_client_protocol::Error> {
        validate_absolute_cwd(&args.cwd)?;
        let config = Config::global();
        let project_id = meta_string(args.meta.as_ref(), "projectId")?;
        let session_type = match meta_string(args.meta.as_ref(), "client")? {
            Some(_) => SessionType::User,
            None => SessionType::Acp,
        };
        let current_mode: GooseMode = config.get_goose_mode().unwrap_or_default();
        let recipe = self.resolve_recipe_from_meta(args.meta.as_ref()).await?;
        let pending_recipe_id = format!("pending_recipe_{}", uuid::Uuid::new_v4());
        let (rendered_recipe, user_recipe_values) = self
            .render_recipe_for_session(cx, &pending_recipe_id, recipe.as_ref())
            .await?;
        let session_name = match recipe.as_ref() {
            Some((recipe, _)) if !recipe.title.trim().is_empty() => recipe.title.clone(),
            _ => "New Chat".to_string(),
        };

        let profile_application = self
            .prepare_profile_application(
                &args,
                rendered_recipe
                    .as_ref()
                    .and_then(|recipe| recipe.extensions.as_deref()),
            )
            .await?;
        let prepared_recipe = PreparedRecipe {
            source: recipe,
            rendered: rendered_recipe,
            user_recipe_values,
        };

        let session = match self
            .session_manager
            .create_session(args.cwd.clone(), session_name, session_type, current_mode)
            .await
        {
            Ok(session) => session,
            Err(error) => {
                if let Some(application) = &profile_application {
                    return Err(self
                        .finalize_failed_profile_application(
                            application,
                            None,
                            "session_create_failed",
                        )
                        .await);
                }
                return Err(agent_client_protocol::Error::internal_error()
                    .data(format!("Failed to create session: {error}")));
            }
        };
        if let Some(application) = &profile_application {
            if self
                .bind_profile_application_marker(&session.id, application)
                .await
                .is_err()
            {
                return Err(self
                    .finalize_failed_profile_application(
                        application,
                        Some(&session.id),
                        "session_binding_failed",
                    )
                    .await);
            }
        }
        match self
            .finish_new_session_setup(
                cx,
                config,
                &session,
                args,
                prepared_recipe,
                project_id,
                profile_application.as_ref(),
            )
            .await
        {
            Ok(response) => {
                if let Some(application) = &profile_application {
                    if let Err(error) = self
                        .finish_profile_application_record(
                            application,
                            Some(&session.id),
                            "created",
                            "session_created",
                        )
                        .await
                    {
                        let _ = error;
                        return Err(self
                            .finalize_failed_profile_application(
                                application,
                                Some(&session.id),
                                "application_record_failed",
                            )
                            .await);
                    }
                }
                Ok(response)
            }
            Err(error) => {
                if let Some(application) = &profile_application {
                    return Err(self
                        .finalize_failed_profile_application(
                            application,
                            Some(&session.id),
                            "session_setup_failed",
                        )
                        .await);
                }
                let _ = self.cleanup_failed_new_session(&session.id).await;
                Err(error)
            }
        }
    }

    async fn finish_new_session_setup(
        &self,
        cx: &ConnectionTo<Client>,
        config: &Config,
        session: &Session,
        args: NewSessionRequest,
        prepared_recipe: PreparedRecipe,
        project_id: Option<String>,
        profile_application: Option<&ConsumedProfileApplication>,
    ) -> Result<NewSessionResponse, agent_client_protocol::Error> {
        let rendered_recipe = self
            .configure_new_session(
                config,
                session,
                args,
                prepared_recipe,
                project_id,
                profile_application,
            )
            .await?;

        let reloaded_session = self.reload_session(&session.id).await?;
        let (agent, extension_results) = self
            .activate_acp_session(cx, &reloaded_session, HashMap::new(), profile_application)
            .await?;
        if let Some(recipe) = &rendered_recipe {
            self.apply_recipe(&agent, recipe).await;
        }

        let reloaded_session = self.reload_session(&session.id).await?;
        let response = self
            .build_new_session_response(&reloaded_session, &extension_results)
            .await?;
        self.notify_session_setup(cx, &reloaded_session).await?;
        Ok(response)
    }

    async fn cleanup_failed_new_session(&self, session_id: &str) -> NewSessionCleanupResult {
        let expected_agent = self.session_agent_if_current(session_id).await;
        let session_deleted = match self.session_manager.delete_session(session_id).await {
            Ok(()) => true,
            Err(error) => {
                warn!(
                    session_id,
                    %error,
                    "Failed to delete session during new-session cleanup"
                );
                false
            }
        };
        let agent_removed = if let Some(expected_agent) = expected_agent {
            self.unregister_acp_session_and_managed_runtime(session_id, &expected_agent)
                .await;
            match self
                .agent_manager
                .remove_session_if_current(session_id, &expected_agent)
                .await
            {
                Ok(_) => true,
                Err(error) => {
                    warn!(
                        session_id,
                        %error,
                        "Failed to remove in-memory agent during new-session cleanup"
                    );
                    false
                }
            }
        } else {
            true
        };
        NewSessionCleanupResult {
            session_deleted,
            agent_removed,
        }
    }

    async fn configure_new_session(
        &self,
        config: &Config,
        session: &Session,
        args: NewSessionRequest,
        prepared_recipe: PreparedRecipe,
        project_id: Option<String>,
        profile_application: Option<&ConsumedProfileApplication>,
    ) -> Result<Option<Recipe>, agent_client_protocol::Error> {
        let recipe_settings = prepared_recipe
            .rendered
            .as_ref()
            .and_then(|recipe| recipe.settings.as_ref());
        let (provider, model_config) = self
            .resolve_provider_and_model(config, args.meta.as_ref(), recipe_settings)
            .await?;

        let goose_extensions = meta_goose_extensions(args.meta.as_ref())?;
        let recipe_extensions = prepared_recipe
            .rendered
            .as_ref()
            .and_then(|recipe| recipe.extensions.as_deref());
        let mut extension_data = self.build_enabled_extensions_data(
            config,
            session,
            args.mcp_servers,
            goose_extensions,
            recipe_extensions,
        )?;
        if let Some(application) = profile_application {
            let managed_names = application
                .managed_extension_names()
                .iter()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>();
            let mut state = EnabledExtensionsState::from_extension_data(&extension_data)
                .unwrap_or_else(|| EnabledExtensionsState::new(Vec::new()));
            state
                .extensions
                .retain(|extension| !managed_names.contains(extension.name().as_str()));
            state
                .to_extension_data(&mut extension_data)
                .internal_err_ctx("Failed to isolate MCP profile extensions")?;
            ProfileApplicationMarker {
                application_id: application.application_id.clone(),
                profile_id: application.profile_id.clone(),
                profile_revision: application.profile_revision,
                merge_policy: "replace_managed_only".to_string(),
                original_session_id: session.id.clone(),
                managed_extension_names: application.managed_extension_names().to_vec(),
            }
            .to_extension_data(&mut extension_data)
            .internal_err_ctx("Failed to record MCP profile application")?;
        }

        self.apply_initial_session_config(
            &session.id,
            InitialSessionConfig {
                provider,
                model_config,
                extension_data,
                recipe: prepared_recipe
                    .source
                    .map(|(recipe, _)| recipe_for_persistence(recipe)),
                user_recipe_values: prepared_recipe.user_recipe_values,
                project_id,
            },
        )
        .await?;

        Ok(prepared_recipe.rendered)
    }

    async fn prepare_profile_application(
        &self,
        args: &NewSessionRequest,
        recipe_extensions: Option<&[crate::agents::ExtensionConfig]>,
    ) -> Result<Option<ConsumedProfileApplication>, agent_client_protocol::Error> {
        let token = meta_string(args.meta.as_ref(), "profileApplicationToken")?;
        let goose_extensions = meta_goose_extensions(args.meta.as_ref())?;
        if token.is_none()
            && args.mcp_servers.is_empty()
            && goose_extensions.as_ref().is_none_or(Vec::is_empty)
            && recipe_extensions.is_none_or(|extensions| extensions.is_empty())
        {
            return Ok(None);
        }
        let (_context, service) = self.mcp_platform_context_and_service_read_only().await;
        let service = service.map_err(profile_application_public_error)?;
        let provenance = service
            .managed_extension_provenance()
            .await
            .map_err(profile_application_public_error)?;
        let goose_extensions = goose_extensions.unwrap_or_default();
        if contains_managed_injection(
            &provenance,
            &goose_extensions,
            &args.mcp_servers,
            recipe_extensions.unwrap_or_default(),
        )? {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("platform-managed MCP configuration must be supplied by a confirmed profile application token"));
        }
        match token {
            Some(token) => {
                let (context, service) = self.mcp_platform_context_and_service().await;
                service
                    .map_err(profile_application_public_error)?
                    .consume_profile_application_token(&context, &token)
                    .await
                    .map(Some)
                    .map_err(profile_application_token_error)
            }
            None => Ok(None),
        }
    }

    async fn finish_profile_application_record(
        &self,
        application: &ConsumedProfileApplication,
        session_id: Option<&str>,
        status: &str,
        detail_code: &str,
    ) -> Result<(), agent_client_protocol::Error> {
        let (_, service) = self.mcp_platform_context_and_service().await;
        service
            .map_err(profile_application_public_error)?
            .finish_profile_application(
                &application.application_id,
                session_id,
                status,
                detail_code,
            )
            .await
            .map_err(profile_application_public_error)
    }

    async fn bind_profile_application_marker(
        &self,
        session_id: &str,
        application: &ConsumedProfileApplication,
    ) -> Result<(), agent_client_protocol::Error> {
        let mut extension_data = ExtensionData::new();
        ProfileApplicationMarker {
            application_id: application.application_id.clone(),
            profile_id: application.profile_id.clone(),
            profile_revision: application.profile_revision,
            merge_policy: "replace_managed_only".to_string(),
            original_session_id: session_id.to_string(),
            managed_extension_names: application.managed_extension_names().to_vec(),
        }
        .to_extension_data(&mut extension_data)
        .internal_err_ctx("Failed to bind MCP profile recovery marker")?;
        self.session_manager
            .update(session_id)
            .trusted_extension_data(extension_data)
            .apply()
            .await
            .internal_err_ctx("Failed to persist MCP profile recovery marker")
    }

    async fn finalize_failed_profile_application(
        &self,
        application: &ConsumedProfileApplication,
        session_id: Option<&str>,
        failure_code: &str,
    ) -> agent_client_protocol::Error {
        if self
            .finish_profile_application_record(application, session_id, "failed", failure_code)
            .await
            .is_err()
        {
            return profile_recovery_error(
                application,
                session_id,
                "application_state_transition_failed",
            );
        }

        let cleanup = match session_id {
            Some(session_id) => self.cleanup_failed_new_session(session_id).await,
            None => NewSessionCleanupResult {
                session_deleted: true,
                agent_removed: true,
            },
        };
        let status = if cleanup.complete() {
            "rolled_back"
        } else {
            "recovery_required"
        };
        if self
            .finish_profile_application_record(
                application,
                session_id,
                status,
                cleanup.failure_code(),
            )
            .await
            .is_err()
        {
            return profile_recovery_error(
                application,
                session_id,
                "compensation_state_transition_failed",
            );
        }
        agent_client_protocol::Error::internal_error().data(format!(
            "profile_application_{status}:{};application_id={};session_id={}",
            cleanup.failure_code(),
            application.application_id,
            session_id.unwrap_or("none")
        ))
    }

    async fn reload_session(
        &self,
        session_id: &str,
    ) -> Result<Session, agent_client_protocol::Error> {
        self.session_manager
            .get_session(session_id, false)
            .await
            .internal_err_ctx("Failed to reload session")
    }

    async fn resolve_provider_and_model(
        &self,
        config: &Config,
        meta: Option<&Meta>,
        recipe_settings: Option<&Settings>,
    ) -> Result<(String, ModelConfig), agent_client_protocol::Error> {
        let recipe_provider = recipe_settings.and_then(|s| s.goose_provider.clone());
        let recipe_model = recipe_settings.and_then(|s| s.goose_model.clone());

        let provider = match recipe_provider {
            Some(provider) => provider,
            None => match meta_string(meta, "provider")? {
                Some(provider) => provider,
                None => {
                    if let Some(model) = recipe_model.as_deref() {
                        let provider = config.get_goose_provider().map_err(|error| {
                            agent_client_protocol::Error::internal_error()
                                .data(format!("Failed to resolve provider: {}", error))
                        })?;
                        let model_config = model_config_from_recipe_settings(&provider, model)?;
                        return Ok((provider, model_config));
                    }

                    return super::resolve_default_provider_model_config(config);
                }
            },
        };

        let model_config = match recipe_model {
            Some(model) => model_config_from_recipe_settings(&provider, &model)?,
            None => super::resolve_provider_default_model_config(&provider).await?,
        };

        Ok((provider, model_config))
    }

    async fn apply_initial_session_config(
        &self,
        session_id: &str,
        config: InitialSessionConfig,
    ) -> Result<(), agent_client_protocol::Error> {
        let mut builder = self
            .session_manager
            .update(session_id)
            .provider_name(config.provider)
            .model_config(config.model_config)
            .trusted_extension_data(config.extension_data);
        if let Some(recipe) = config.recipe {
            builder = builder.recipe(Some(recipe));
        }
        if config.user_recipe_values.is_some() {
            builder = builder.user_recipe_values(config.user_recipe_values);
        }
        if let Some(project_id) = config.project_id {
            builder = builder.project_id(Some(project_id));
        }
        builder
            .apply()
            .await
            .internal_err_ctx("Failed to update session")?;
        Ok(())
    }

    async fn build_new_session_response(
        &self,
        session: &Session,
        extension_results: &[ExtensionLoadResult],
    ) -> Result<NewSessionResponse, agent_client_protocol::Error> {
        let (mode_state, config_options) =
            super::build_session_setup_config(&self.provider_inventory, session).await?;

        let mut response =
            NewSessionResponse::new(SessionId::new(session.id.clone())).modes(mode_state);
        if let Some(co) = config_options {
            response = response.config_options(co);
        }
        response = response.meta(super::session_response_meta(session, extension_results));
        Ok(response)
    }
}

fn mcp_server_name(server: &agent_client_protocol::schema::v1::McpServer) -> &str {
    match server {
        agent_client_protocol::schema::v1::McpServer::Stdio(value) => &value.name,
        agent_client_protocol::schema::v1::McpServer::Http(value) => &value.name,
        agent_client_protocol::schema::v1::McpServer::Sse(value) => &value.name,
        _ => "",
    }
}

fn contains_managed_injection(
    provenance: &[(String, String, crate::mcp_platform::ProfileManagedReference)],
    goose_extensions: &[GooseExtension],
    mcp_servers: &[agent_client_protocol::schema::v1::McpServer],
    recipe_extensions: &[crate::agents::ExtensionConfig],
) -> Result<bool, agent_client_protocol::Error> {
    let managed_names = provenance
        .iter()
        .map(|(_, name, _)| name.as_str())
        .collect::<std::collections::HashSet<_>>();
    if goose_extensions.iter().any(|extension| {
        let name = match extension {
            GooseExtension::Builtin { name, .. } | GooseExtension::Platform { name, .. } => name,
            GooseExtension::Mcp { server, .. } => mcp_server_name(server),
        };
        managed_names.contains(name)
    }) || mcp_servers
        .iter()
        .any(|server| managed_names.contains(mcp_server_name(server)))
        || recipe_extensions
            .iter()
            .any(|extension| managed_names.contains(extension.name().as_str()))
    {
        return Ok(true);
    }
    let mut client_configs =
        super::extensions::goose_extensions_to_configs(goose_extensions.to_vec())?;
    for server in mcp_servers {
        client_configs.push(
            super::mcp_server_to_extension_config(server.clone()).map_err(|_| {
                agent_client_protocol::Error::invalid_params()
                    .data("unable to verify MCP server provenance")
            })?,
        );
    }
    client_configs.extend_from_slice(recipe_extensions);
    for config in client_configs {
        let fingerprint = crate::mcp_platform::extension_source_fingerprint(&config)
            .map_err(profile_application_public_error)?;
        if provenance
            .iter()
            .any(|(_, _, managed)| managed.source_fingerprint == fingerprint)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn recipe_for_persistence(mut recipe: Recipe) -> Recipe {
    recipe.extensions = None;
    recipe
}

fn profile_application_token_error(
    error: crate::mcp_platform::McpPlatformError,
) -> agent_client_protocol::Error {
    match error.code() {
        crate::mcp_platform::McpPlatformErrorCode::InvalidRequest
        | crate::mcp_platform::McpPlatformErrorCode::NotFound
        | crate::mcp_platform::McpPlatformErrorCode::PlanExpired
        | crate::mcp_platform::McpPlatformErrorCode::PlanStale => {
            agent_client_protocol::Error::invalid_params().data(PROFILE_APPLICATION_REJECTED)
        }
        _ => profile_application_public_error(error),
    }
}

pub(super) fn profile_application_public_error(
    error: crate::mcp_platform::McpPlatformError,
) -> agent_client_protocol::Error {
    match error.code() {
        crate::mcp_platform::McpPlatformErrorCode::PolicyDenied => {
            agent_client_protocol::Error::invalid_params()
                .data("profile application rejected: policy_denied")
        }
        _ => agent_client_protocol::Error::internal_error()
            .data("profile application rejected: platform_unavailable"),
    }
}

fn profile_recovery_error(
    application: &ConsumedProfileApplication,
    session_id: Option<&str>,
    outcome: &str,
) -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data(format!(
        "profile_application_recovery_required:{outcome};application_id={};session_id={}",
        application.application_id,
        session_id.unwrap_or("none")
    ))
}

fn model_config_from_recipe_settings(
    provider: &str,
    model: &str,
) -> Result<ModelConfig, agent_client_protocol::Error> {
    crate::model_config::model_config_from_user_config(provider, model)
        .internal_err_ctx("Failed to build model config from recipe settings")
}

fn meta_goose_extensions(
    meta: Option<&Meta>,
) -> Result<Option<Vec<GooseExtension>>, agent_client_protocol::Error> {
    let Some(value) = meta.and_then(|m| m.get("enabledExtensions")) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|e| {
            agent_client_protocol::Error::invalid_params().data(format!("enabledExtensions: {e}"))
        })
}

#[cfg(test)]
mod profile_tests {
    use crate::mcp_platform::{McpPlatformError, McpPlatformErrorCode};
    use agent_client_protocol::schema::v1::{McpServer, McpServerStdio};

    use super::*;

    #[test]
    fn direct_platform_managed_mcp_injection_is_detected() {
        let managed_server =
            McpServer::Stdio(McpServerStdio::new("managed_mcp_owned", "managed-command"));
        let managed_config = super::super::mcp_server_to_extension_config(managed_server).unwrap();
        let mut provenance = (0..10_000)
            .map(|index| {
                let managed_mcp_id = format!("filler-managed-{index:05}");
                let extension_name = format!("filler_managed_mcp_{index:05}");
                (
                    managed_mcp_id.clone(),
                    extension_name.clone(),
                    crate::mcp_platform::ProfileManagedReference {
                        managed_mcp_id,
                        extension_name,
                        projection_digest: format!("{index:064x}"),
                        source_fingerprint: format!("{:064x}", index + 1),
                    },
                )
            })
            .collect::<Vec<_>>();
        provenance.push((
            "managed-id".to_string(),
            "managed_mcp_owned".to_string(),
            crate::mcp_platform::ProfileManagedReference {
                managed_mcp_id: "managed-id".to_string(),
                extension_name: "managed_mcp_owned".to_string(),
                projection_digest: "a".repeat(64),
                source_fingerprint: crate::mcp_platform::extension_source_fingerprint(
                    &managed_config,
                )
                .unwrap(),
            },
        ));
        let ordinary = GooseExtension::Builtin {
            name: "developer".to_string(),
            description: None,
            display_name: None,
            timeout: None,
            bundled: None,
            available_tools: None,
        };
        assert!(!contains_managed_injection(&provenance, &[ordinary], &[], &[]).unwrap());

        let injected = GooseExtension::Platform {
            name: "managed_mcp_owned".to_string(),
            description: None,
            display_name: None,
            bundled: None,
            available_tools: None,
        };
        assert!(contains_managed_injection(&provenance, &[injected], &[], &[]).unwrap());

        let server = McpServer::Stdio(McpServerStdio::new("managed_mcp_owned", "forged-command"));
        assert!(contains_managed_injection(&provenance, &[], &[server], &[]).unwrap());

        let alias = McpServer::Stdio(McpServerStdio::new("renamed", "managed-command"));
        assert!(contains_managed_injection(&provenance, &[], &[alias], &[]).unwrap());

        let mut recipe_alias = managed_config.clone();
        assert!(contains_managed_injection(
            &provenance,
            &[],
            &[],
            std::slice::from_ref(&managed_config)
        )
        .unwrap());
        if let crate::agents::ExtensionConfig::Stdio { name, .. } = &mut recipe_alias {
            *name = "recipe-renamed".to_string();
        }
        let encoded = crate::recipe_deeplink::encode(
            &Recipe::builder()
                .title("Managed alias")
                .description("regression")
                .instructions("test")
                .extensions(vec![recipe_alias])
                .build()
                .unwrap(),
        )
        .unwrap();
        let decoded = crate::recipe_deeplink::decode(&encoded).unwrap();
        assert!(contains_managed_injection(
            &provenance,
            &[],
            &[],
            decoded.extensions.as_deref().unwrap()
        )
        .unwrap());
        assert!(recipe_for_persistence(decoded).extensions.is_none());

        let ordinary_recipe = crate::agents::ExtensionConfig::Builtin {
            name: "developer".to_string(),
            description: String::new(),
            display_name: None,
            timeout: None,
            bundled: Some(true),
            available_tools: Vec::new(),
        };
        assert!(!contains_managed_injection(&provenance, &[], &[], &[ordinary_recipe]).unwrap());
    }

    #[test]
    fn cleanup_failure_never_qualifies_as_complete_rollback() {
        for cleanup in [
            NewSessionCleanupResult {
                session_deleted: false,
                agent_removed: true,
            },
            NewSessionCleanupResult {
                session_deleted: true,
                agent_removed: false,
            },
            NewSessionCleanupResult {
                session_deleted: false,
                agent_removed: false,
            },
        ] {
            assert!(!cleanup.complete());
            assert_ne!(cleanup.failure_code(), "session_cleanup_complete");
        }
    }

    #[test]
    fn profile_application_token_rejections_are_publicly_generic() {
        let expected = profile_application_token_error(McpPlatformError::new(
            McpPlatformErrorCode::NotFound,
            "missing token",
        ))
        .data;
        for error in [
            McpPlatformError::new(McpPlatformErrorCode::NotFound, "missing token"),
            McpPlatformError::new(McpPlatformErrorCode::PlanStale, "replayed token"),
            McpPlatformError::new(McpPlatformErrorCode::PlanExpired, "expired token"),
            McpPlatformError::new(McpPlatformErrorCode::InvalidRequest, "malformed token"),
        ] {
            let data = profile_application_token_error(error).data;
            assert_eq!(data, expected);
            assert_eq!(
                data.as_ref().and_then(serde_json::Value::as_str),
                Some(PROFILE_APPLICATION_REJECTED)
            );
            let text = data.unwrap().as_str().unwrap().to_string();
            assert!(!text.contains("not_found"));
            assert!(!text.contains("plan_stale"));
            assert!(!text.contains("plan_expired"));
            assert!(!text.contains("invalid_request"));
        }
    }

    #[test]
    fn non_token_profile_errors_keep_public_semantics() {
        assert_eq!(
            profile_application_public_error(McpPlatformError::new(
                McpPlatformErrorCode::PolicyDenied,
                "trusted Goose launcher or authenticated transport is required for MCP platform mutations",
            )),
            agent_client_protocol::Error::invalid_params()
                .data("profile application rejected: policy_denied")
        );

        assert_eq!(
            profile_application_token_error(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityUnavailable,
                "integrity store unavailable",
            )),
            agent_client_protocol::Error::internal_error()
                .data("profile application rejected: platform_unavailable")
        );
    }
}
