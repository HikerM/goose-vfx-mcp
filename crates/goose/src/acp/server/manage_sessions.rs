use super::*;
use crate::mcp_platform::McpPlatformErrorCode;

impl GooseAcpAgent {
    pub(super) async fn on_update_working_dir(
        &self,
        req: UpdateWorkingDirRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        let working_dir = req.working_dir.trim().to_string();
        if working_dir.is_empty() {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("working directory cannot be empty"));
        }
        let path = std::path::PathBuf::from(&working_dir);
        validate_absolute_cwd(&path)?;
        let session_id = &req.session_id;

        let session = self
            .session_manager
            .get_session(session_id, false)
            .await
            .map_err(|_| {
                agent_client_protocol::Error::resource_not_found(Some(session_id.to_string()))
                    .data(format!("Session not found: {}", session_id))
            })?;

        if path == session.working_dir {
            return Ok(EmptyResponse {});
        }

        self.session_manager
            .update(session_id)
            .working_dir(path)
            .apply()
            .await
            .internal_err_ctx("Failed to update session working directory")?;

        let session = self
            .session_manager
            .get_session(session_id, false)
            .await
            .internal_err_ctx("Failed to reload session")?;

        let agent = self.get_session_agent(session_id).await?;
        agent
            .restore_provider_from_session(&session)
            .await
            .internal_err_ctx("Failed to refresh provider from session")?;

        agent
            .extension_manager
            .update_working_dir(&session.working_dir)
            .await;

        Ok(EmptyResponse {})
    }

    pub(super) async fn on_set_session_system_prompt(
        &self,
        req: SetSessionSystemPromptRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        let session_id = req.session_id.trim();
        if session_id.is_empty() {
            return Err(
                agent_client_protocol::Error::invalid_params().data("sessionId cannot be empty")
            );
        }

        let agent = self.get_session_agent(session_id).await?;
        match req.mode {
            SessionSystemPromptMode::Set => {
                if req.text.trim().is_empty() {
                    agent.clear_system_prompt_override().await;
                } else {
                    agent.override_system_prompt(req.text).await;
                }
            }
            SessionSystemPromptMode::Append => {
                let key = req
                    .key
                    .as_deref()
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .ok_or_else(|| {
                        agent_client_protocol::Error::invalid_params()
                            .data("key cannot be empty for append mode")
                    })?;
                if req.text.trim().is_empty() {
                    agent.remove_system_prompt_extra(key).await;
                } else {
                    agent.extend_system_prompt(key.to_string(), req.text).await;
                }
            }
        }

        Ok(EmptyResponse {})
    }

    pub(super) async fn on_delete_session(
        &self,
        req: DeleteSessionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        let expected_agent = self.session_agent_if_current(&req.session_id).await;
        let session = self
            .session_manager
            .get_session(&req.session_id, false)
            .await;
        let profile_marker = session.as_ref().ok().and_then(|session| {
            let raw = session.extension_data.get_extension_state(
                crate::mcp_platform::ProfileApplicationMarker::EXTENSION_NAME,
                crate::mcp_platform::ProfileApplicationMarker::VERSION,
            );
            let marker = <crate::mcp_platform::ProfileApplicationMarker as crate::session::ExtensionState>::from_extension_data(
                &session.extension_data,
            );
            if raw.is_some() && marker.is_none() {
                Some(Err(agent_client_protocol::Error::invalid_params()
                    .data("profile application rejected: invalid_profile_marker")))
            } else {
                marker.map(Ok)
            }
        }).transpose()?;

        let mut profile_cleanup = None;
        let mut profile_service = None;
        if let Some(marker) = &profile_marker {
            let (_, service) = self.mcp_platform_context_and_service().await;
            let service = service.map_err(|error| match error.code() {
                McpPlatformErrorCode::PolicyDenied => {
                    agent_client_protocol::Error::invalid_params()
                        .data("profile application rejected: policy_denied")
                }
                _ => agent_client_protocol::Error::internal_error().data(format!(
                    "profile_application_recovery_required:session_delete_state_unavailable;application_id={};session_id={}",
                    marker.application_id, req.session_id
                )),
            })?;
            let stored = service
                .profile_application_cleanup_state(&req.session_id)
                .await
                .map_err(|_| {
                    agent_client_protocol::Error::internal_error().data(format!(
                        "profile_application_recovery_required:session_delete_state_unavailable;application_id={};session_id={}",
                        marker.application_id, req.session_id
                    ))
                })?;
            if marker.original_session_id != req.session_id {
                return Err(agent_client_protocol::Error::invalid_params()
                    .data("profile application rejected: cleanup_binding_mismatch"));
            }
            if stored
                .as_ref()
                .is_some_and(|(application_id, _)| application_id != &marker.application_id)
            {
                return Err(agent_client_protocol::Error::invalid_params()
                    .data("profile application rejected: cleanup_binding_mismatch"));
            }
            profile_cleanup = Some(
                stored.unwrap_or_else(|| (marker.application_id.clone(), "created".to_string())),
            );
            profile_service = Some(service);
        } else if session.is_err() {
            let (_, service) = self.mcp_platform_context_and_service_read_only().await;
            if let Ok(service) = service {
                match service
                    .profile_application_cleanup_state(&req.session_id)
                    .await
                {
                    Ok(Some(stored)) => {
                        let (_, trusted_service) = self.mcp_platform_context_and_service().await;
                        let trusted_service = trusted_service.map_err(|error| {
                            if error.code() == McpPlatformErrorCode::PolicyDenied {
                                agent_client_protocol::Error::invalid_params()
                                    .data("profile application rejected: policy_denied")
                            } else {
                                agent_client_protocol::Error::internal_error()
                                    .data("session delete state lookup failed")
                            }
                        })?;
                        profile_cleanup = Some(stored);
                        profile_service = Some(trusted_service);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        return Err(agent_client_protocol::Error::internal_error()
                            .data("session delete state lookup failed"));
                    }
                }
            }
        }

        if let Some((application_id, status)) = &profile_cleanup {
            let service = profile_service
                .as_ref()
                .expect("cleanup state requires service");
            if status != "deleted" {
                service
                    .finish_profile_application(
                        application_id,
                        Some(&req.session_id),
                        "recovery_required",
                        "session_delete_started",
                    )
                    .await
                    .map_err(|_| {
                        agent_client_protocol::Error::internal_error().data(format!(
                            "profile_application_recovery_required:session_delete_state_transition_failed;application_id={application_id};session_id={}",
                            req.session_id
                        ))
                    })?;
            }
        }

        if session.is_ok() {
            self.session_manager
                .delete_session(&req.session_id)
                .await
                .map_err(|_| {
                    if let Some((application_id, _)) = &profile_cleanup {
                        agent_client_protocol::Error::internal_error().data(format!(
                            "profile_application_recovery_required:session_delete_failed;application_id={application_id};session_id={}",
                            req.session_id
                        ))
                    } else {
                        agent_client_protocol::Error::internal_error()
                    }
                })?;
        } else if profile_cleanup.is_none() {
            self.session_manager
                .delete_session(&req.session_id)
                .await
                .internal_err()?;
        }
        if let Some(expected_agent) = expected_agent {
            self.unregister_acp_session_and_managed_runtime(&req.session_id, &expected_agent)
                .await;
            if self
                .agent_manager
                .remove_session_if_current(&req.session_id, &expected_agent)
                .await
                .is_err()
            {
                if let Some((application_id, _)) = &profile_cleanup {
                    profile_service
                    .as_ref()
                    .expect("cleanup state requires service")
                    .finish_profile_application(
                        application_id,
                        Some(&req.session_id),
                        "recovery_required",
                        "agent_cleanup_failed",
                    )
                    .await
                    .map_err(|_| {
                        agent_client_protocol::Error::internal_error().data(format!(
                            "profile_application_recovery_required:session_delete_state_transition_failed;application_id={application_id};session_id={}",
                            req.session_id
                        ))
                    })?;
                    return Err(agent_client_protocol::Error::internal_error().data(format!(
                    "profile_application_recovery_required:agent_cleanup_failed;application_id={application_id};session_id={}",
                    req.session_id
                )));
                }
                return Err(agent_client_protocol::Error::internal_error()
                    .data("Failed to remove in-memory agent"));
            }
        }
        if let Some((application_id, status)) = profile_cleanup {
            if status != "deleted" {
                profile_service
                    .expect("cleanup state requires service")
                .finish_profile_application(
                    &application_id,
                    Some(&req.session_id),
                    "deleted",
                    "session_deleted",
                )
                .await
                .map_err(|_| {
                    agent_client_protocol::Error::internal_error().data(format!(
                        "profile_application_recovery_required:session_delete_audit_failed;application_id={application_id};session_id={}",
                        req.session_id
                    ))
                })?;
            }
        }
        Ok(EmptyResponse {})
    }

    pub(super) async fn on_export_session(
        &self,
        req: ExportSessionRequest,
    ) -> Result<ExportSessionResponse, agent_client_protocol::Error> {
        let data = self
            .session_manager
            .export_session(&req.session_id)
            .await
            .internal_err()?;
        Ok(ExportSessionResponse { data })
    }

    pub(super) async fn on_import_session(
        &self,
        req: ImportSessionRequest,
    ) -> Result<ImportSessionResponse, agent_client_protocol::Error> {
        let is_nostr = match req.source {
            SessionImportSource::Auto => is_nostr_session_link(&req.input),
            SessionImportSource::Json => false,
            SessionImportSource::Nostr => true,
        };
        let (data, session_type) = if is_nostr {
            (
                import_nostr_session_json(&req.input).await?,
                Some(SessionType::User),
            )
        } else {
            (req.input, None)
        };

        let session = self
            .session_manager
            .import_session(&data, session_type)
            .await
            .internal_err()?;

        let msg_count = session.message_count as u64;

        Ok(ImportSessionResponse {
            session_id: session.id,
            title: Some(session.name),
            updated_at: Some(session.updated_at.to_rfc3339()),
            message_count: msg_count,
        })
    }

    pub(super) async fn on_share_session_nostr(
        &self,
        req: ShareSessionNostrRequest,
    ) -> Result<ShareSessionNostrResponse, agent_client_protocol::Error> {
        let data = self
            .session_manager
            .export_session(&req.session_id)
            .await
            .internal_err()?;

        let share = publish_session_to_nostr(&data, req.relays).await?;

        Ok(ShareSessionNostrResponse {
            deeplink: share.deeplink,
            nevent: share.nevent,
            event_id: share.event_id,
            relays: share.relays,
        })
    }

    pub(super) async fn on_get_session_info(
        &self,
        req: GetSessionInfoRequest,
    ) -> Result<GetSessionInfoResponse, agent_client_protocol::Error> {
        let session_id = req.session_id.trim();
        if session_id.is_empty() {
            return Err(
                agent_client_protocol::Error::invalid_params().data("sessionId cannot be empty")
            );
        }

        let session = self
            .session_manager
            .get_session(session_id, false)
            .await
            .map_err(|_| {
                agent_client_protocol::Error::resource_not_found(Some(session_id.to_string()))
                    .data(format!("Session not found: {}", session_id))
            })?;

        Ok(GetSessionInfoResponse {
            session: build_session_info(session),
        })
    }

    pub(super) async fn on_truncate_session_conversation(
        &self,
        req: TruncateSessionConversationRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        let session_id = req.session_id.trim();
        if session_id.is_empty() {
            return Err(
                agent_client_protocol::Error::invalid_params().data("sessionId cannot be empty")
            );
        }

        self.session_manager
            .truncate_conversation(session_id, req.truncate_from)
            .await
            .internal_err()?;
        Ok(EmptyResponse {})
    }

    pub(super) async fn on_update_session_project(
        &self,
        req: UpdateSessionProjectRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.session_manager
            .update(&req.session_id)
            .project_id(req.project_id)
            .apply()
            .await
            .internal_err()?;
        Ok(EmptyResponse {})
    }

    pub(super) async fn on_rename_session(
        &self,
        req: RenameSessionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.session_manager
            .update(&req.session_id)
            .user_provided_name(req.title)
            .apply()
            .await
            .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;
        Ok(EmptyResponse {})
    }

    pub(super) async fn on_archive_session(
        &self,
        req: ArchiveSessionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        let expected_agent = self.session_agent_if_current(&req.session_id).await;
        self.session_manager
            .update(&req.session_id)
            .archived_at(Some(chrono::Utc::now()))
            .apply()
            .await
            .internal_err()?;
        if let Some(expected_agent) = expected_agent {
            self.unregister_acp_session_and_managed_runtime(&req.session_id, &expected_agent)
                .await;
            self.agent_manager
                .remove_session_if_current(&req.session_id, &expected_agent)
                .await
                .internal_err_ctx("Failed to remove in-memory agent")?;
        }
        Ok(EmptyResponse {})
    }

    pub(super) async fn on_unarchive_session(
        &self,
        req: UnarchiveSessionRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        self.session_manager
            .update(&req.session_id)
            .archived_at(None)
            .apply()
            .await
            .internal_err()?;
        Ok(EmptyResponse {})
    }
}

fn is_nostr_session_link(input: &str) -> bool {
    input.trim_start().starts_with("goose://sessions/nostr")
}

#[cfg(feature = "nostr")]
async fn import_nostr_session_json(deeplink: &str) -> Result<String, agent_client_protocol::Error> {
    crate::session::nostr_share::import_session_json_from_deeplink(deeplink)
        .await
        .invalid_params_err()
}

#[cfg(not(feature = "nostr"))]
async fn import_nostr_session_json(
    _deeplink: &str,
) -> Result<String, agent_client_protocol::Error> {
    Err(agent_client_protocol::Error::invalid_params()
        .data("Nostr session import is not available in this build"))
}

#[cfg(feature = "nostr")]
async fn publish_session_to_nostr(
    data: &str,
    relays: Vec<String>,
) -> Result<NostrSessionShare, agent_client_protocol::Error> {
    let relays = crate::session::nostr_share::resolve_relays(relays, Config::global());
    let share = crate::session::nostr_share::publish_session_json(data, relays)
        .await
        .internal_err()?;
    Ok(NostrSessionShare {
        deeplink: share.deeplink,
        nevent: share.nevent,
        event_id: share.event_id,
        relays: share.relays,
    })
}

#[cfg(not(feature = "nostr"))]
async fn publish_session_to_nostr(
    _data: &str,
    _relays: Vec<String>,
) -> Result<NostrSessionShare, agent_client_protocol::Error> {
    Err(agent_client_protocol::Error::invalid_params()
        .data("Nostr session sharing is not available in this build"))
}

struct NostrSessionShare {
    deeplink: String,
    nevent: String,
    event_id: String,
    relays: Vec<String>,
}
