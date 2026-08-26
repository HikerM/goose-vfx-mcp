use super::*;
use crate::providers::inventory::ensure_refresh_identity_current;

fn safe_dispatch_error() -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data("acp_dispatch_failed")
}

impl HandleDispatchFrom<Client> for LuminaAcpHandler {
    fn describe_chain(&self) -> impl std::fmt::Debug {
        "lumina-acp"
    }

    fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        cx: ConnectionTo<Client>,
    ) -> impl std::future::Future<Output = Result<Handled<Dispatch>, agent_client_protocol::Error>> + Send
    {
        let agent = self.agent.clone();
        let transport_mcp_platform_write_session =
            self.transport_mcp_platform_write_session.clone();

        // The MatchDispatchFrom chain produces an ~85KB async state machine.
        // Box::pin moves it to the heap so it doesn't overflow the tokio worker stack.
        Box::pin(async move {
            // Capture the connection handle so handlers can lazily activate
            // sessions that exist on disk but were never activated via
            // new_session/load_session on this connection. Set-once per
            // connection; the result is ignored on later requests.
            let _ = agent.client_cx.set(cx.clone());

            // InitializeRequest runs inline: it sets connection-scoped state
            // (client fs/terminal capabilities) that later handlers read with
            // defaults, so a pipelined NewSessionRequest must not race ahead of it.
            MatchDispatchFrom::new(message, &cx)
                .if_request(
                    |req: InitializeRequest, responder: Responder<InitializeResponse>| async {
                        responder.respond_with_result(agent.on_initialize(req).await.map_err(|_| safe_dispatch_error()))
                    },
                )
                .await
                .if_request(
                    |_req: AuthenticateRequest, responder: Responder<AuthenticateResponse>| async {
                        responder.respond(AuthenticateResponse::new())
                    },
                )
                .await
                .if_request(
                    |req: NewSessionRequest, responder: Responder<NewSessionResponse>| async {
                        let agent = agent.clone();
                        let cx_clone = cx.clone();
                        let transport_mcp_platform_write_session =
                            transport_mcp_platform_write_session.clone();
                        cx.spawn(async move {
                            let result = match transport_mcp_platform_write_session {
                                Some(session) => {
                                    with_transport_mcp_platform_write_authority(
                                        Some(session.authority()),
                                        agent.on_new_session(&cx_clone, req),
                                    )
                                    .await
                                }
                                None => agent.on_new_session(&cx_clone, req).await,
                            };
                            responder.respond_with_result(result.map_err(|_| safe_dispatch_error()))?;
                            Ok(())
                        })?;
                        Ok(())
                    },
                )
                .await
                .if_request(
                    |req: LoadSessionRequest, responder: Responder<LoadSessionResponse>| async {
                        let agent = agent.clone();
                        let cx_clone = cx.clone();
                        cx.spawn(async move {
                            match agent.on_load_session(&cx_clone, req).await {
                                Ok(response) => {
                                    responder.respond(response)?;
                                }
                                Err(_) => {
                                    tracing::error!(event = "acp_load_session_failed");
                                    responder.respond_with_error(safe_dispatch_error())?;
                                }
                            }
                            Ok(())
                        })?;
                        Ok(())
                    },
                )
                .await
                .if_request(
                    |req: PromptRequest, responder: Responder<PromptResponse>| async {
                        let agent = agent.clone();
                        let cx_clone = cx.clone();
                        cx.spawn(async move {
                            match agent.on_prompt(&cx_clone, req).await {
                                Ok(response) => {
                                    responder.respond(response)?;
                                }
                                Err(_) => {
                                    responder.respond_with_error(safe_dispatch_error())?;
                                }
                            }
                            Ok(())
                        })?;
                        Ok(())
                    },
                )
                .await
                .if_notification(|notif: CancelNotification| async {
                    let agent = agent.clone();
                    agent.on_cancel(notif).await?;
                    Ok(())
                })
                .await
                // set_config_option (SACP 11) and set_mode; custom _lumina/* in otherwise.
                .if_request({
                    let agent = agent.clone();
                    let cx = cx.clone();
                    |req: SetSessionConfigOptionRequest, responder: Responder<SetSessionConfigOptionResponse>| async move {
                        let cx_spawn = cx.clone();
                        cx.spawn(async move {
                            let cx = cx_spawn;
                            let value_id = req.value.as_value_id()
                                .ok_or_else(|| agent_client_protocol::Error::invalid_params().data("Expected a value ID"))?
                                .clone();
                            let session_id = req.session_id.clone();
                            let config_id = req.config_id.0.to_string();
                            let t_handler = std::time::Instant::now();
                            match config_id.as_ref() {
                                "provider" => {
                                    Config::global().invalidate_secrets_cache();
                                    match agent.update_provider(&session_id.0, &value_id.0, None, None, None).await {
                                        Ok(_) => {}
                                        Err(_) => { responder.respond_with_error(safe_dispatch_error())?; return Ok(()); }
                                    }
                                }
                                "mode" => {
                                    match agent.on_set_mode(&session_id.0, &value_id.0).await {
                                        Ok(_) => {}
                                        Err(_) => { responder.respond_with_error(safe_dispatch_error())?; return Ok(()); }
                                    }
                                }
                                "model" => {
                                    match agent.on_set_model(&session_id.0, &value_id.0).await {
                                        Ok(_) => {}
                                        Err(_) => { responder.respond_with_error(safe_dispatch_error())?; return Ok(()); }
                                    }
                                }
                                "thinking_effort" => {
                                    match agent.on_set_thinking_effort(&session_id.0, &value_id.0).await {
                                        Ok(_) => {}
                                        Err(_) => { responder.respond_with_error(safe_dispatch_error())?; return Ok(()); }
                                    }
                                }
                                _ => {
                                    responder.respond_with_error(
                                        agent_client_protocol::Error::invalid_params().data("invalid_config_option")
                                    )?;
                                    return Ok(());
                                }
                            }
                            // Respond immediately using the current provider inventory snapshot.
                            let (notification, config_options) = agent.build_config_update(&session_id).await?;
                            cx.send_notification(notification)?;
                            responder.respond(SetSessionConfigOptionResponse::new(config_options))?;

                            let maybe_refresh = if config_id == "provider" {
                                let provider_id = value_id.0.to_string();
                                agent
                                    .provider_inventory
                                    .plan_refresh_jobs(std::slice::from_ref(&provider_id))
                                    .await
                                    .ok()
                                    .and_then(|plan| {
                                        plan.started
                                            .into_iter()
                                            .find(|job| job.provider_id == provider_id)
                                    })
                            } else {
                                None
                            };
                            if let Some(refresh_job) = maybe_refresh {
                                let agent_bg = agent.clone();
                                let cx_bg = cx.clone();
                                let session_id_bg = session_id.clone();
                                tokio::spawn(async move {
                                    let refresh_identity = refresh_job.identity;
                                    let refresh_provider_id = refresh_job.provider_id;
                                    let mut refresh_guard =
                                        agent_bg.provider_inventory.refresh_guard(&refresh_identity);
                                    let provider_result: Result<Arc<dyn Provider>> =
                                        AssertUnwindSafe(async {
                                            let session_agent =
                                                agent_bg.get_session_agent(&session_id_bg.0).await?;
                                            let provider = session_agent
                                                .provider()
                                                .await
                                                .map_err(|_| {
                                                    anyhow::anyhow!(
                                                        "provider_inventory_provider_unavailable"
                                                    )
                                                })?;
                                            let provider_name = provider.get_name().to_string();
                                            if provider_name != refresh_provider_id {
                                                return Err(anyhow::anyhow!(
                                                    "provider changed before inventory refresh completed"
                                                ));
                                            }
                                            Ok(provider)
                                        })
                                        .catch_unwind()
                                .await
                                .map_err(|_| {
                                    anyhow::anyhow!("provider inventory refresh task panicked")
                                })
                                .and_then(|result| result);

                                let fetch_result = match provider_result {
                                    Ok(provider) => {
                                        match ensure_refresh_identity_current(
                                            &refresh_provider_id,
                                            &refresh_identity,
                                        )
                                        .await
                                        {
                                            Ok(()) => match AssertUnwindSafe(
                                                provider.fetch_recommended_models(
                                                    crate::model_config::global_toolshim(),
                                                ),
                                            )
                                            .catch_unwind()
                                            .await
                                            {
                                                Ok(Ok(models)) => Ok(models),
                                                Ok(Err(_)) => Err(anyhow::anyhow!(
                                                    "provider_inventory_fetch_failed"
                                                )),
                                                Err(_) => Err(anyhow::anyhow!(
                                                    "provider inventory refresh task panicked"
                                                )),
                                            },
                                            Err(_) => Err(anyhow::anyhow!(
                                                "provider_inventory_identity_check_failed"
                                            )),
                                        }
                                    }
                                    Err(_) => Err(anyhow::anyhow!(
                                        "provider_inventory_provider_setup_failed"
                                    )),
                                };

                                match fetch_result {
                                    Ok(models) => match agent_bg
                                        .provider_inventory
                                        .store_refreshed_models_for_identity(
                                            &refresh_identity,
                                            &models,
                                        )
                                        .await
                                    {
                                        Ok(()) => {
                                            refresh_guard.complete();
                                            match agent_bg.build_config_update(&session_id_bg).await
                                            {
                                                Ok((fresh_notification, _)) => {
                                                    let _ = cx_bg
                                                        .send_notification(fresh_notification);
                                                }
                                                Err(_) => warn!(event = "acp_provider_inventory_update_failed"),
                                            }
                                        }
                                        Err(_) => warn!(event = "acp_provider_inventory_store_failed"),
                                    },
                                    Err(_) => {
                                        let error_message = "provider_inventory_refresh_failed".to_string();
                                        match agent_bg
                                            .provider_inventory
                                            .store_refresh_error_for_identity(
                                                &refresh_identity,
                                                error_message.clone(),
                                            )
                                            .await
                                        {
                                            Ok(()) => refresh_guard.complete(),
                                            Err(_) => warn!(event = "acp_provider_inventory_error_store_failed"),
                                        }
                                        warn!(event = "acp_provider_inventory_refresh_failed");
                                    }
                                }
                                });
                            }

                            debug!(target: "perf", ms = t_handler.elapsed().as_millis() as u64, "acp_set_config_option_completed");
                            Ok(())
                        })?;
                        Ok(())
                    }
                })
                .await
                .if_request({
                    let agent = agent.clone();
                    let cx = cx.clone();
                    |req: SetSessionModeRequest, responder: Responder<SetSessionModeResponse>| async move {
                        let cx_spawn = cx.clone();
                        cx.spawn(async move {
                            let cx = cx_spawn;
                            let session_id = req.session_id.clone();
                            let mode_id = req.mode_id.clone();
                            match agent.on_set_mode(&session_id.0, &mode_id.0).await {
                                Ok(resp) => {
                                    // Notify before responding so clients see the mode update before block_task unblocks.
                                    cx.send_notification(SessionNotification::new(
                                        session_id,
                                        SessionUpdate::CurrentModeUpdate(
                                            CurrentModeUpdate::new(mode_id),
                                        ),
                                    ))?;
                                    responder.respond(resp)?;
                                }
                                Err(_) => {
                                    responder.respond_with_error(safe_dispatch_error())?;
                                }
                            }
                            Ok(())
                        })?;
                        Ok(())
                    }
                })
                .await
                .if_request({
                    let agent = agent.clone();
                    let cx = cx.clone();
                    |req: ListSessionsRequest, responder: Responder<ListSessionsResponse>| async move {
                        cx.spawn(async move {
                            match agent.on_list_sessions(req).await {
                                Ok(response) => responder.respond(response)?,
                                Err(_) => responder.respond_with_error(safe_dispatch_error())?,
                            }
                            Ok(())
                        })?;
                        Ok(())
                    }
                })
                .await
                .if_request({
                    let agent = agent.clone();
                    let cx = cx.clone();
                    |req: CloseSessionRequest, responder: Responder<CloseSessionResponse>| async move {
                        cx.spawn(async move {
                            responder.respond_with_result(
                                agent
                                    .on_close_session(&req.session_id.0)
                                    .await
                                    .map_err(|_| safe_dispatch_error()),
                            )?;
                            Ok(())
                        })?;
                        Ok(())
                    }
                })
                .await
                .if_request({
                    let agent = agent.clone();
                    let cx = cx.clone();
                    |req: ForkSessionRequest, responder: Responder<ForkSessionResponse>| async move {
                        let cx_spawn = cx.clone();
                        cx.spawn(async move {
                            responder.respond_with_result(
                                agent
                                    .on_fork_session(&cx_spawn, req)
                                    .await
                                    .map_err(|_| safe_dispatch_error()),
                            )?;
                            Ok(())
                        })?;
                        Ok(())
                    }
                })
                .await
                .otherwise({
                    let agent = agent.clone();
                    let cx = cx.clone();
                    let transport_mcp_platform_write_session =
                        transport_mcp_platform_write_session;
                    |message: Dispatch| async move {
                        match message {
                            Dispatch::Request(req, responder) => {
                                cx.spawn(async move {
                                    let result = match transport_mcp_platform_write_session {
                                        Some(session) => agent
                                            .dispatch_transport_custom_request(
                                                session.authority(),
                                                &req.method,
                                                req.params,
                                            )
                                            .await,
                                        None => {
                                            agent.dispatch_custom_request(&req.method, req.params).await
                                        }
                                    };
                                    match result {
                                        Ok(json) => responder.respond(json)?,
                                        Err(error) => responder.respond_with_error(error)?,
                                    }
                                    Ok(())
                                })?;
                                Ok(())
                            }
                            Dispatch::Response(result, router) => {
                                debug!(event = "acp_response_routed", ok = result.is_ok());
                                router.respond_with_result(result)?;
                                Ok(())
                            }
                            Dispatch::Notification(_) => {
                                debug!(event = "acp_unhandled_notification");
                                Ok(())
                            }
                        }
                    }
                })
                .await
                .map(|()| Handled::Yes)
        })
    }
}
