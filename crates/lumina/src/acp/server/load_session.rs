use super::*;

fn sid_short(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

fn replay_audience_annotations(audience: &[Role]) -> Annotations {
    Annotations::new().audience(
        audience
            .iter()
            .map(|role| match role {
                Role::Assistant => agent_client_protocol::schema::v1::Role::Assistant,
                Role::User => agent_client_protocol::schema::v1::Role::User,
            })
            .collect::<Vec<_>>(),
    )
}

fn send_replay_content_chunk(
    cx: &ConnectionTo<Client>,
    session_id: &SessionId,
    message: &Message,
    content: ContentBlock,
) -> std::result::Result<(), agent_client_protocol::Error> {
    let chunk = ContentChunk::new(content).meta(replay_message_meta(message));
    let update = match message.role {
        Role::User => SessionUpdate::UserMessageChunk(chunk),
        Role::Assistant => SessionUpdate::AgentMessageChunk(chunk),
    };
    cx.send_notification(SessionNotification::new(session_id.clone(), update))
}

fn replay_conversation_to_client(
    cx: &ConnectionTo<Client>,
    session: &Session,
    supports_lumina_custom_notifications: bool,
) -> Result<HashMap<String, crate::conversation::message::ToolRequest>, agent_client_protocol::Error>
{
    let session_id = SessionId::new(session.id.clone());
    let sid = sid_short(session_id.0.as_ref());

    let messages = session
        .conversation
        .as_ref()
        .map(|c| c.messages().to_vec())
        .unwrap_or_default();
    debug!(
        target: "perf",
        sid = %sid,
        messages = messages.len(),
            "perf: load_session messages loaded"
    );

    let mut replay_tool_requests =
        HashMap::<String, crate::conversation::message::ToolRequest>::new();

    for message in &messages {
        if !message.metadata.user_visible {
            continue;
        }

        for content_item in &message.content {
            match content_item {
                MessageContent::Text(text) => {
                    let mut tc = TextContent::new(text.text.clone());
                    if let Some(audience) = text.audience() {
                        tc = tc.annotations(replay_audience_annotations(audience));
                    }
                    send_replay_content_chunk(cx, &session_id, message, ContentBlock::Text(tc))?;
                }
                MessageContent::Image(image) => {
                    let mut image_content =
                        ImageContent::new(image.data.clone(), image.mime_type.clone());
                    if let Some(audience) = image.audience() {
                        image_content =
                            image_content.annotations(replay_audience_annotations(audience));
                    }
                    send_replay_content_chunk(
                        cx,
                        &session_id,
                        message,
                        ContentBlock::Image(image_content),
                    )?;
                }
                MessageContent::ToolRequest(tool_request) => {
                    replay_tool_requests.insert(tool_request.id.clone(), tool_request.clone());

                    let pending_tool_call = pending_tool_call_from_request(tool_request);
                    let mut meta = pending_tool_call.identity_meta;
                    if let Some(chain_summary) = tool_request.persisted_chain_summary() {
                        meta = with_tool_chain_summary_meta(
                            meta,
                            &chain_summary.summary,
                            chain_summary.count,
                        );
                    }
                    let tool_call = pending_tool_call
                        .tool_call
                        .meta(merge_replay_message_meta(meta, message));

                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::ToolCall(tool_call),
                    ))?;
                }
                MessageContent::ToolResponse(tool_response) => {
                    let status = match &tool_response.tool_result {
                        Ok(result) if result.is_error == Some(true) => ToolCallStatus::Failed,
                        Ok(_) => ToolCallStatus::Completed,
                        Err(_) => ToolCallStatus::Failed,
                    };

                    let mut fields = ToolCallUpdateFields::new().status(status);
                    if let Some(raw_output) = extract_tool_raw_output(&tool_response.tool_result) {
                        fields = fields.raw_output(raw_output);
                    }
                    if !tool_response
                        .tool_result
                        .as_ref()
                        .is_ok_and(|r| r.is_acp_aware())
                    {
                        let content = build_tool_call_content(&tool_response.tool_result);
                        fields = fields.content(content);

                        let locations =
                            extract_locations_from_meta(tool_response).unwrap_or_else(|| {
                                if let Some(tool_request) =
                                    replay_tool_requests.get(&tool_response.id)
                                {
                                    extract_tool_locations(tool_request, tool_response)
                                } else {
                                    Vec::new()
                                }
                            });
                        if !locations.is_empty() {
                            fields = fields.locations(locations);
                        }
                    }

                    let update =
                        ToolCallUpdate::new(ToolCallId::new(tool_response.id.clone()), fields)
                            .meta(merge_replay_message_meta(
                                extract_tool_call_update_meta(tool_response),
                                message,
                            ));
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::ToolCallUpdate(update),
                    ))?;
                }
                MessageContent::Thinking(thinking) => {
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::AgentThoughtChunk(
                            ContentChunk::new(ContentBlock::Text(TextContent::new(
                                thinking.thinking.clone(),
                            )))
                            .meta(replay_message_meta(message)),
                        ),
                    ))?;
                }
                MessageContent::SystemNotification(_) => {}
                _ => {}
            }
        }

        if supports_lumina_custom_notifications {
            if let Some(usage) = &message.metadata.usage {
                cx.send_notification(LuminaSessionNotification {
                    session_id: session.id.clone(),
                    update: LuminaSessionUpdate::MessageUsage(message_usage_update(
                        message.id.clone(),
                        usage,
                    )),
                })?;
            }
        }
    }

    Ok(replay_tool_requests)
}

impl LuminaAcpAgent {
    pub(super) async fn handle_load_session(
        &self,
        cx: &ConnectionTo<Client>,
        args: LoadSessionRequest,
    ) -> Result<LoadSessionResponse, agent_client_protocol::Error> {
        debug!(?args, "load session request");
        validate_absolute_cwd(&args.cwd)?;

        let session_id_str = args.session_id.0.to_string();
        let sid = sid_short(&session_id_str);
        let t_start = std::time::Instant::now();

        let mut session = self
            .session_manager
            .get_session(&session_id_str, true)
            .await
            .map_err(|_| {
                agent_client_protocol::Error::resource_not_found(Some(session_id_str.clone()))
                    .data(format!("Session not found: {}", session_id_str))
            })?;

        session = self
            .prepare_session_for_activation(session, args.cwd.clone(), args.mcp_servers, true)
            .await?;

        let replay_tool_requests = replay_conversation_to_client(
            cx,
            &session,
            self.supports_lumina_custom_notifications(),
        )?;
        let (agent, extension_results) = self.prepare_acp_session_agent(cx, &session, None).await?;
        self.apply_session_recipe(&agent, &session).await?;
        self.register_acp_session(session_id_str.clone(), agent.clone(), replay_tool_requests)
            .await;

        session = self
            .session_manager
            .get_session(&session_id_str, true)
            .await
            .internal_err_ctx("Failed to reload session")?;

        agent
            .extension_manager
            .update_working_dir(&session.working_dir)
            .await;

        let (mode_state, config_options) =
            build_session_setup_config(&self.provider_inventory, &session).await?;

        self.notify_session_setup(cx, &session).await?;

        let mut response = LoadSessionResponse::new().modes(mode_state);
        if let Some(co) = config_options {
            response = response.config_options(co);
        }

        response = response.meta(session_response_meta(&session, &extension_results));

        debug!(
            target: "perf",
            sid = %sid,
            ms = t_start.elapsed().as_millis() as u64,
            "perf: load_session_refactor done"
        );
        self.closed_session_ids.lock().await.remove(&session_id_str);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::time::Duration;

    fn mixed_media_result() -> Value {
        json!({
            "content": [
                {"type": "text", "text": "Generated media follows"},
                {
                    "type": "audio",
                    "data": "UklGRgQAAABXQVZF",
                    "mimeType": "audio/wav",
                    "annotations": {
                        "audience": ["user", "assistant"],
                        "priority": 0.75,
                        "lastModified": "2026-11-12T08:00:00.123Z"
                    }
                },
                {"type": "image", "data": "aW1hZ2U=", "mimeType": "image/png"},
                {
                    "type": "resource_link",
                    "uri": "mcp://generated/video/clip-1",
                    "name": "clip-1.mp4",
                    "title": "Generated video",
                    "description": "Video with its original metadata",
                    "mimeType": "video/mp4",
                    "size": 4294967295_u32,
                    "annotations": {
                        "audience": ["user"],
                        "priority": 1.0,
                        "lastModified": "2026-11-12T08:00:01Z"
                    },
                    "_meta": {
                        "producer": {"id": "video-tool", "revision": 3},
                        "labels": ["preview", "original"],
                        "available": true
                    }
                },
                {
                    "type": "resource",
                    "resource": {
                        "uri": "mcp://generated/transcript/clip-1",
                        "mimeType": "text/plain",
                        "text": "Transcript"
                    }
                },
                {"type": "text", "text": "End of result"}
            ],
            "structuredContent": {"mediaCount": 2},
            "isError": false
        })
    }

    fn expected_media_content() -> Value {
        Value::Array(
            mixed_media_result()["content"]
                .as_array()
                .unwrap()
                .iter()
                .map(|content| json!({"type": "content", "content": content}))
                .collect(),
        )
    }

    #[test]
    fn media_tool_content_preserves_mixed_order_and_supported_fields() {
        let result: CallToolResult = serde_json::from_value(mixed_media_result()).unwrap();
        let content = build_tool_call_content(&Ok(result));
        assert_eq!(
            serde_json::to_value(content).unwrap(),
            expected_media_content()
        );
    }

    #[test]
    fn media_tool_content_keeps_absent_optional_fields_absent() {
        let blocks = json!([
            {"type": "audio", "data": "AAEC", "mimeType": "audio/mpeg"},
            {"type": "resource_link", "uri": "https://example.invalid/a.wav", "name": "a.wav"}
        ]);
        let result: CallToolResult = serde_json::from_value(json!({"content": blocks})).unwrap();
        let content = build_tool_call_content(&Ok(result));
        assert_eq!(
            serde_json::to_value(content).unwrap(),
            json!([
                {"type": "content", "content": {"type": "audio", "data": "AAEC", "mimeType": "audio/mpeg"}},
                {"type": "content", "content": {"type": "resource_link", "uri": "https://example.invalid/a.wav", "name": "a.wav"}}
            ])
        );
    }

    #[test]
    fn media_tool_content_preserves_error_result_media_without_inventing_transport_error_content() {
        let mut result: CallToolResult = serde_json::from_value(mixed_media_result()).unwrap();
        result.is_error = Some(true);
        assert_eq!(
            serde_json::to_value(build_tool_call_content(&Ok(result))).unwrap(),
            expected_media_content()
        );
        assert!(
            build_tool_call_content(&Err(rmcp::model::ErrorData::internal_error(
                "Tool transport failed",
                None,
            )))
            .is_empty()
        );
    }

    #[tokio::test]
    async fn media_tool_content_is_identical_in_live_and_persisted_replay_notifications() {
        let directory = tempfile::tempdir().unwrap();
        let server = crate::acp::server_factory::AcpServer::new(
            crate::acp::server_factory::AcpServerFactoryConfig {
                builtins: Vec::new(),
                data_dir: directory.path().join("data"),
                config_dir: directory.path().join("config"),
                lumina_platform: LuminaPlatform::LuminaCli,
                additional_source_roots: Vec::new(),
            },
        );
        let acp_agent = server.create_agent().await.unwrap();
        let session = acp_agent
            .session_manager
            .create_session(
                directory.path().to_path_buf(),
                "Media forwarding regression".to_string(),
                SessionType::Acp,
                LuminaMode::default(),
            )
            .await
            .unwrap();
        let runtime_agent = Arc::new(Agent::with_config(AgentConfig::new(
            acp_agent.session_manager.clone(),
            Arc::new(PermissionManager::new(directory.path().join("config"))),
            None,
            LuminaMode::default(),
            true,
            LuminaPlatform::LuminaCli,
        )));
        acp_agent
            .register_acp_session(session.id.clone(), runtime_agent.clone(), HashMap::new())
            .await;
        let result = serde_json::from_value(mixed_media_result()).unwrap();
        let message = Message::assistant().with_tool_response("media-call-1", Ok(result));
        let MessageContent::ToolResponse(tool_response) = message.content[0].clone() else {
            panic!("expected a tool response");
        };
        acp_agent
            .session_manager
            .add_message(&session.id, &message)
            .await
            .unwrap();
        let persisted_session = acp_agent
            .session_manager
            .get_session(&session.id, true)
            .await
            .unwrap();

        let (sender, mut output) = tokio::sync::mpsc::unbounded_channel::<String>();
        let outgoing = futures::sink::unfold(sender, async move |sender, line| {
            sender.send(line).map_err(std::io::Error::other)?;
            Ok::<_, std::io::Error>(sender)
        });
        let transport = Lines::new(
            outgoing,
            futures::stream::pending::<std::io::Result<String>>(),
        );
        let connection = SacpAgent
            .builder()
            .connect_with(transport, async move |cx| {
                let session_id = SessionId::new(session.id.clone());
                acp_agent
                    .handle_tool_response(
                        &tool_response,
                        &session_id,
                        &session.id,
                        message.id.as_deref(),
                        &runtime_agent,
                        &cx,
                    )
                    .await?;
                replay_conversation_to_client(&cx, &persisted_session, false)?;

                let mut notifications = Vec::new();
                for _ in 0..2 {
                    let line = output
                        .recv()
                        .await
                        .expect("ACP notification must be written");
                    let notification: Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(notification["method"], "session/update");
                    assert_eq!(notification["params"]["sessionId"], session.id);
                    let update = &notification["params"]["update"];
                    assert_eq!(update["sessionUpdate"], "tool_call_update");
                    assert_eq!(update["toolCallId"], "media-call-1");
                    assert_eq!(update["status"], "completed");
                    assert_eq!(update["content"], expected_media_content());
                    assert_eq!(update["rawOutput"], json!({"mediaCount": 2}));
                    notifications.push(notification);
                }
                assert_eq!(
                    notifications[0]["params"]["update"]["content"],
                    notifications[1]["params"]["update"]["content"]
                );
                Ok(())
            });
        tokio::time::timeout(Duration::from_secs(10), connection)
            .await
            .expect("live and replay notifications must both finish")
            .unwrap();
        server.shutdown().await;
    }
}
