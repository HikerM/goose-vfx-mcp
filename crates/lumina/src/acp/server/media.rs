use super::{Agent, LuminaAcpAgent};
use crate::acp::custom_requests::{
    MediaAccessPurposeDto, MediaAccessRequest, MediaAccessResponse, MediaAssociationDto,
    MediaAvailabilityDto, MediaInputRefDto, MediaKindDto, MediaOwnershipDto, MediaReadRequest,
    MediaReadResponse, MediaReleaseRequest, MediaReleaseResponse, MediaResultDto,
    MediaRuntimeBuildDto, MediaSourceDto, MediaSpecDto,
};
use crate::media as domain;
use std::sync::Arc;

fn inactive_media_session() -> agent_client_protocol::Error {
    agent_client_protocol::Error::resource_not_found(None).data("media_session_not_active")
}

fn media_read_failed() -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data("media_read_failed")
}

impl LuminaAcpAgent {
    pub(super) async fn on_media_access(
        &self,
        req: MediaAccessRequest,
    ) -> Result<MediaAccessResponse, agent_client_protocol::Error> {
        let expected = self.media_session_agent(&req.session_id).await?;
        if !uuid::Uuid::parse_str(&req.result_id).is_ok_and(|id| id.to_string() == req.result_id) {
            return Err(
                agent_client_protocol::Error::invalid_params().data("invalid_media_result_id")
            );
        }
        let unavailable = || {
            agent_client_protocol::Error::resource_not_found(None).data("media_access_unavailable")
        };
        let pool = self
            .session_manager
            .storage()
            .pool()
            .await
            .map_err(|_| unavailable())?
            .clone();
        let content = domain::MediaResultStore::new(pool)
            .open_content(&req.session_id, &req.result_id)
            .await;
        // Keep both guards through issue so close/replacement cannot interleave
        // between the final session check and registering the capability.
        let sessions = self.sessions.lock().await;
        let closed = self.closed_session_ids.lock().await;
        if closed.contains(&req.session_id)
            || !sessions
                .get(&req.session_id)
                .is_some_and(|session| Arc::ptr_eq(&session.agent, &expected))
        {
            return Err(inactive_media_session());
        }
        let ticket = self
            .media_access_owner
            .issue(
                content.map_err(|_| unavailable())?,
                &req.session_id,
                &req.result_id,
                req.purpose.into(),
            )
            .map_err(|_| unavailable())?;
        Ok(MediaAccessResponse {
            ticket: ticket.ticket,
            expires_in_ms: ticket.expires_in_ms,
        })
    }

    pub(super) async fn on_media_release(
        &self,
        req: MediaReleaseRequest,
    ) -> Result<MediaReleaseResponse, agent_client_protocol::Error> {
        self.media_access_owner.release(&req.ticket);
        Ok(MediaReleaseResponse {})
    }

    async fn media_session_agent(
        &self,
        session_id: &str,
    ) -> Result<Arc<Agent>, agent_client_protocol::Error> {
        let agent = self
            .session_agent_if_current(session_id)
            .await
            .ok_or_else(inactive_media_session)?;
        if self.closed_session_ids.lock().await.contains(session_id) {
            return Err(inactive_media_session());
        }
        Ok(agent)
    }

    async fn require_current_media_session(
        &self,
        session_id: &str,
        expected: &Arc<Agent>,
    ) -> Result<(), agent_client_protocol::Error> {
        let current = self.media_session_agent(session_id).await?;
        if !Arc::ptr_eq(&current, expected) {
            return Err(inactive_media_session());
        }
        Ok(())
    }

    pub(super) async fn on_media_read(
        &self,
        req: MediaReadRequest,
    ) -> Result<MediaReadResponse, agent_client_protocol::Error> {
        // get_session_agent would implicitly activate a session found on disk.
        let expected = self.media_session_agent(&req.session_id).await?;
        if !uuid::Uuid::parse_str(&req.result_id).is_ok_and(|id| id.to_string() == req.result_id) {
            return Err(
                agent_client_protocol::Error::invalid_params().data("invalid_media_result_id")
            );
        }
        let pool = self
            .session_manager
            .storage()
            .pool()
            .await
            .map_err(|_| media_read_failed())?
            .clone();
        let result = domain::MediaResultStore::new(pool)
            .read(&req.session_id, &req.result_id)
            .await;
        self.require_current_media_session(&req.session_id, &expected)
            .await?;
        match result.map_err(|_| media_read_failed())? {
            Some(status) => Ok(MediaReadResponse::Found {
                result: Box::new(status.result.into()),
                availability: status.availability.into(),
            }),
            None => Ok(MediaReadResponse::NotFound),
        }
    }
}

impl From<MediaAccessPurposeDto> for domain::MediaAccessPurpose {
    fn from(value: MediaAccessPurposeDto) -> Self {
        match value {
            MediaAccessPurposeDto::Preview => Self::Preview,
            MediaAccessPurposeDto::Open => Self::Open,
            MediaAccessPurposeDto::Reveal => Self::Reveal,
            MediaAccessPurposeDto::CopyPath => Self::CopyPath,
            MediaAccessPurposeDto::SaveAs => Self::SaveAs,
        }
    }
}

impl From<domain::MediaResultRef> for MediaResultDto {
    fn from(result: domain::MediaResultRef) -> Self {
        Self {
            schema_version: result.schema_version,
            result_id: result.result_id,
            session_id: result.session_id,
            association: result.association.into(),
            job_id: result.job_id,
            kind: result.kind.into(),
            mime: result.mime,
            file_name: result.file_name,
            bytes: result.bytes,
            sha256: result.sha256,
            created_at: result.created_at,
            root_id: result.root_id,
            relative_path: result.relative_path,
            spec: result.spec.into(),
            ownership: result.ownership.into(),
            source: result.source.into(),
        }
    }
}

impl From<domain::MediaKind> for MediaKindDto {
    fn from(value: domain::MediaKind) -> Self {
        match value {
            domain::MediaKind::Image => Self::Image,
            domain::MediaKind::Audio => Self::Audio,
            domain::MediaKind::Video => Self::Video,
        }
    }
}

impl From<domain::MediaAssociation> for MediaAssociationDto {
    fn from(value: domain::MediaAssociation) -> Self {
        match value {
            domain::MediaAssociation::Message { message_id } => Self::Message { message_id },
            domain::MediaAssociation::ToolCall { tool_call_id } => Self::ToolCall { tool_call_id },
        }
    }
}

impl From<domain::MediaInputRef> for MediaInputRefDto {
    fn from(value: domain::MediaInputRef) -> Self {
        match value {
            domain::MediaInputRef::Message { message_id } => Self::Message { message_id },
            domain::MediaInputRef::MediaResult { result_id } => Self::MediaResult { result_id },
            domain::MediaInputRef::ContentDigest { sha256 } => Self::ContentDigest { sha256 },
        }
    }
}

impl From<domain::MediaRuntimeBuild> for MediaRuntimeBuildDto {
    fn from(value: domain::MediaRuntimeBuild) -> Self {
        Self {
            adapter_id: value.adapter_id,
            engine_revision: value.engine_revision,
            binary_sha256: value.binary_sha256,
        }
    }
}

impl From<domain::MediaSource> for MediaSourceDto {
    fn from(value: domain::MediaSource) -> Self {
        match value {
            domain::MediaSource::LocalModel {
                installation_id,
                version_id,
                runtime,
                generation_config_sha256,
                input_refs,
            } => Self::LocalModel {
                installation_id,
                version_id,
                runtime: runtime.into(),
                generation_config_sha256,
                input_refs: input_refs.into_iter().map(Into::into).collect(),
            },
            domain::MediaSource::Mcp {
                server_id,
                tool_name,
                tool_call_id,
                input_refs,
            } => Self::Mcp {
                server_id,
                tool_name,
                tool_call_id,
                input_refs: input_refs.into_iter().map(Into::into).collect(),
            },
        }
    }
}

impl From<domain::MediaOwnership> for MediaOwnershipDto {
    fn from(value: domain::MediaOwnership) -> Self {
        match value {
            domain::MediaOwnership::ApplicationGenerated => Self::ApplicationGenerated,
            domain::MediaOwnership::ApplicationImported => Self::ApplicationImported,
        }
    }
}

impl From<domain::MediaSpec> for MediaSpecDto {
    fn from(value: domain::MediaSpec) -> Self {
        match value {
            domain::MediaSpec::Image { width, height } => Self::Image { width, height },
            domain::MediaSpec::Audio {
                duration_micros,
                sample_rate,
                channels,
                sample_frames,
            } => Self::Audio {
                duration_micros,
                sample_rate,
                channels,
                sample_frames,
            },
            domain::MediaSpec::Video {
                width,
                height,
                duration_micros,
                frame_count,
                fps_numerator,
                fps_denominator,
            } => Self::Video {
                width,
                height,
                duration_micros,
                frame_count,
                fps_numerator,
                fps_denominator,
            },
        }
    }
}

impl From<domain::MediaAvailability> for MediaAvailabilityDto {
    fn from(value: domain::MediaAvailability) -> Self {
        match value {
            domain::MediaAvailability::Available => Self::Available,
            domain::MediaAvailability::Publishing => Self::Publishing,
            domain::MediaAvailability::Missing => Self::Missing,
            domain::MediaAvailability::AccessDenied => Self::AccessDenied,
            domain::MediaAvailability::Tampered => Self::Tampered,
            domain::MediaAvailability::UnsafePath => Self::UnsafePath,
            domain::MediaAvailability::UnsupportedFormat => Self::UnsupportedFormat,
            domain::MediaAvailability::IoUnavailable => Self::IoUnavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::server_factory::{AcpServer, AcpServerFactoryConfig};
    use crate::agents::{AgentConfig, LuminaPlatform};
    use crate::config::{permission::PermissionManager, LuminaMode};
    use crate::session::SessionType;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    #[cfg(windows)]
    use std::io::{Cursor, Write};

    const RESULT_ID: &str = "12345678-1234-4234-8234-123456789abc";

    fn sample_result(kind: domain::MediaKind, spec: domain::MediaSpec) -> domain::MediaResultRef {
        domain::MediaResultRef {
            schema_version: 1,
            result_id: RESULT_ID.into(),
            session_id: "session-1".into(),
            association: domain::MediaAssociation::Message {
                message_id: "message-1".into(),
            },
            job_id: "job-1".into(),
            kind,
            mime: "audio/wav".into(),
            file_name: "result.wav".into(),
            bytes: 32044,
            sha256: "a".repeat(64),
            created_at: 1_794_470_400_123,
            root_id: "root-1".into(),
            relative_path: "session/job/result.wav".into(),
            spec,
            ownership: domain::MediaOwnership::ApplicationGenerated,
            source: domain::MediaSource::LocalModel {
                installation_id: "install-1".into(),
                version_id: "version-1".into(),
                runtime: domain::MediaRuntimeBuild {
                    adapter_id: "audio-native".into(),
                    engine_revision: "revision-1".into(),
                    binary_sha256: "b".repeat(64),
                },
                generation_config_sha256: "c".repeat(64),
                input_refs: vec![
                    domain::MediaInputRef::Message {
                        message_id: "input-1".into(),
                    },
                    domain::MediaInputRef::MediaResult {
                        result_id: "87654321-1234-4234-8234-123456789abc".into(),
                    },
                    domain::MediaInputRef::ContentDigest {
                        sha256: "d".repeat(64),
                    },
                ],
            },
        }
    }

    #[test]
    fn media_read_mapping_preserves_all_fields_specs_and_provenance() {
        let profiles = [
            (
                domain::MediaKind::Image,
                domain::MediaSpec::Image {
                    width: 640,
                    height: 480,
                },
            ),
            (
                domain::MediaKind::Audio,
                domain::MediaSpec::Audio {
                    duration_micros: 1_000_000,
                    sample_rate: 16_000,
                    channels: 1,
                    sample_frames: 16_000,
                },
            ),
            (
                domain::MediaKind::Video,
                domain::MediaSpec::Video {
                    width: 256,
                    height: 256,
                    duration_micros: 2_125_000,
                    frame_count: 17,
                    fps_numerator: 8,
                    fps_denominator: 1,
                },
            ),
        ];
        for (kind, spec) in profiles {
            let mut original = sample_result(kind, spec);
            for mcp in [false, true] {
                if mcp {
                    original.association = domain::MediaAssociation::ToolCall {
                        tool_call_id: "tool-call-1".into(),
                    };
                    original.ownership = domain::MediaOwnership::ApplicationImported;
                    original.source = domain::MediaSource::Mcp {
                        server_id: "server-1".into(),
                        tool_name: "generate".into(),
                        tool_call_id: "tool-call-1".into(),
                        input_refs: vec![domain::MediaInputRef::Message {
                            message_id: "input-2".into(),
                        }],
                    };
                }
                let expected = serde_json::to_value(&original).unwrap();
                let dto: MediaResultDto = original.clone().into();
                let actual = serde_json::to_value(&dto).unwrap();
                assert_eq!(actual, expected);
                assert_eq!(actual["createdAt"], json!(1_794_470_400_123_i64));
                assert!(actual.get("path").is_none());
                assert!(actual.get("url").is_none());
                assert_eq!(
                    serde_json::from_value::<MediaResultDto>(actual).unwrap(),
                    dto
                );
            }
        }
    }

    #[test]
    fn media_read_mapping_preserves_every_availability() {
        for status in [
            domain::MediaAvailability::Available,
            domain::MediaAvailability::Publishing,
            domain::MediaAvailability::Missing,
            domain::MediaAvailability::AccessDenied,
            domain::MediaAvailability::Tampered,
            domain::MediaAvailability::UnsafePath,
            domain::MediaAvailability::UnsupportedFormat,
            domain::MediaAvailability::IoUnavailable,
        ] {
            assert_eq!(
                serde_json::to_value(MediaAvailabilityDto::from(status)).unwrap(),
                serde_json::to_value(status).unwrap()
            );
        }
    }

    struct Fixture {
        server: AcpServer,
        acp: Arc<LuminaAcpAgent>,
        session_id: String,
        store: domain::MediaResultStore,
        directory: tempfile::TempDir,
    }

    impl Fixture {
        async fn new() -> Self {
            #[cfg(windows)]
            let directory = {
                let base = std::path::PathBuf::from("E:/Codex/temp/lumina-acp-media-tests");
                std::fs::create_dir_all(&base).unwrap();
                tempfile::Builder::new()
                    .prefix("acp-")
                    .tempdir_in(std::fs::canonicalize(base).unwrap())
                    .unwrap()
            };
            #[cfg(not(windows))]
            let directory = tempfile::tempdir().unwrap();
            let server = AcpServer::new(AcpServerFactoryConfig {
                builtins: Vec::new(),
                data_dir: directory.path().join("data"),
                config_dir: directory.path().join("config"),
                lumina_platform: LuminaPlatform::LuminaCli,
                additional_source_roots: Vec::new(),
            });
            let acp = server.create_agent().await.unwrap();
            let session = acp
                .session_manager
                .create_session(
                    directory.path().to_path_buf(),
                    "Media read test".into(),
                    SessionType::Acp,
                    LuminaMode::default(),
                )
                .await
                .unwrap();
            let store = domain::MediaResultStore::new(
                acp.session_manager.storage().pool().await.unwrap().clone(),
            );
            store.initialize().await.unwrap();
            Self {
                directory,
                server,
                acp,
                session_id: session.id,
                store,
            }
        }

        async fn activate(&self, session_id: &str) -> Arc<Agent> {
            let agent = Arc::new(Agent::with_config(AgentConfig::new(
                self.acp.session_manager.clone(),
                Arc::new(PermissionManager::new(self.directory.path().join("config"))),
                None,
                LuminaMode::default(),
                true,
                LuminaPlatform::LuminaCli,
            )));
            self.acp
                .register_acp_session(session_id.into(), agent.clone(), HashMap::new())
                .await;
            agent
        }

        fn request(&self, result_id: &str) -> MediaReadRequest {
            MediaReadRequest {
                session_id: self.session_id.clone(),
                result_id: result_id.into(),
            }
        }

        #[cfg(windows)]
        async fn publish_image(&self) -> domain::MediaResultRef {
            let root = self
                .store
                .register_output_root(&self.directory.path().join("outputs"))
                .await
                .unwrap();
            let request = domain::PublishMediaRequest {
                result_id: RESULT_ID.into(),
                session_id: self.session_id.clone(),
                job_id: "job-1".into(),
                root_id: root.root_id,
                kind: domain::MediaKind::Image,
                association: domain::MediaAssociation::ToolCall {
                    tool_call_id: "call-1".into(),
                },
                source: domain::MediaSource::Mcp {
                    server_id: "server-1".into(),
                    tool_name: "generate_image".into(),
                    tool_call_id: "call-1".into(),
                    input_refs: vec![],
                },
            };
            let domain::BeginMediaPublish::Staging(mut staged) =
                self.store.begin_publish(request).await.unwrap()
            else {
                panic!("expected a new publication");
            };
            let image = image::RgbImage::from_pixel(3, 2, image::Rgb([255, 0, 0]));
            let mut bytes = Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(image)
                .write_to(&mut bytes, image::ImageOutputFormat::Png)
                .unwrap();
            staged.write_all(bytes.get_ref()).unwrap();
            self.store.publish(staged).await.unwrap()
        }
    }

    fn assert_inactive(error: agent_client_protocol::Error) {
        assert_eq!(error.data, Some(json!("media_session_not_active")));
    }

    #[tokio::test]
    async fn media_read_rejects_disk_only_session_without_activating_it() {
        let fixture = Fixture::new().await;
        let outgoing = futures::sink::unfold((), async |(), _: String| Ok::<_, std::io::Error>(()));
        let transport = agent_client_protocol::Lines::new(
            outgoing,
            futures::stream::pending::<std::io::Result<String>>(),
        );
        let connection =
            agent_client_protocol::Agent
                .builder()
                .connect_with(transport, async |cx| {
                    assert!(fixture.acp.client_cx.set(cx).is_ok());
                    assert_inactive(
                        fixture
                            .acp
                            .on_media_read(fixture.request(RESULT_ID))
                            .await
                            .unwrap_err(),
                    );
                    assert!(fixture
                        .acp
                        .session_agent_if_current(&fixture.session_id)
                        .await
                        .is_none());
                    Ok(())
                });
        tokio::time::timeout(std::time::Duration::from_secs(10), connection)
            .await
            .unwrap()
            .unwrap();
        fixture.server.shutdown().await;
    }

    #[tokio::test]
    async fn media_read_rejects_closed_session_and_replaced_agent() {
        let fixture = Fixture::new().await;
        let original = fixture.activate(&fixture.session_id).await;
        let replacement = fixture.activate(&fixture.session_id).await;
        assert!(!Arc::ptr_eq(&replacement, &original));
        assert_inactive(
            fixture
                .acp
                .require_current_media_session(&fixture.session_id, &original)
                .await
                .unwrap_err(),
        );
        assert_eq!(
            fixture
                .acp
                .on_media_read(fixture.request(RESULT_ID))
                .await
                .unwrap(),
            MediaReadResponse::NotFound
        );
        fixture
            .acp
            .on_close_session(&fixture.session_id)
            .await
            .unwrap();
        assert_inactive(
            fixture
                .acp
                .on_media_read(fixture.request(RESULT_ID))
                .await
                .unwrap_err(),
        );
        assert_inactive(
            fixture
                .acp
                .require_current_media_session(&fixture.session_id, &replacement)
                .await
                .unwrap_err(),
        );
        fixture.server.shutdown().await;
    }

    #[tokio::test]
    async fn media_read_rejects_other_connection_activation() {
        let fixture = Fixture::new().await;
        fixture.activate(&fixture.session_id).await;
        let other_connection = fixture.server.create_agent().await.unwrap();
        assert_inactive(
            other_connection
                .on_media_read(fixture.request(RESULT_ID))
                .await
                .unwrap_err(),
        );
        assert!(other_connection
            .session_agent_if_current(&fixture.session_id)
            .await
            .is_none());
        fixture.server.shutdown().await;
    }

    #[tokio::test]
    async fn media_read_has_safe_unknown_invalid_and_storage_error_responses() {
        let fixture = Fixture::new().await;
        fixture.activate(&fixture.session_id).await;
        assert_eq!(
            fixture
                .acp
                .on_media_read(fixture.request(RESULT_ID))
                .await
                .unwrap(),
            MediaReadResponse::NotFound
        );
        let invalid = fixture
            .acp
            .on_media_read(fixture.request("C:/private/output.wav"))
            .await
            .unwrap_err();
        assert_eq!(invalid.data, Some(json!("invalid_media_result_id")));
        sqlx::query("DROP TABLE media_results_v1")
            .execute(fixture.acp.session_manager.storage().pool().await.unwrap())
            .await
            .unwrap();
        let failed = fixture
            .acp
            .on_media_read(fixture.request(RESULT_ID))
            .await
            .unwrap_err();
        assert_eq!(failed.data, Some(json!("media_read_failed")));
        let rendered = serde_json::to_string(&failed).unwrap();
        assert!(!rendered.contains("media_results_v1"));
        assert!(!rendered.contains(&fixture.directory.path().to_string_lossy().to_string()));
        fixture.server.shutdown().await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn media_read_scopes_published_metadata_and_missing_status_to_its_session() {
        let fixture = Fixture::new().await;
        fixture.activate(&fixture.session_id).await;
        let published = fixture.publish_image().await;
        let expected = MediaReadResponse::Found {
            result: Box::new(published.clone().into()),
            availability: MediaAvailabilityDto::Available,
        };
        let response = fixture
            .acp
            .on_media_read(fixture.request(RESULT_ID))
            .await
            .unwrap();
        assert_eq!(response, expected);
        let wire: Value = serde_json::to_value(&response).unwrap();
        assert_eq!(wire["result"]["createdAt"], json!(published.created_at));
        assert!(wire["result"].get("path").is_none());

        let other = fixture
            .acp
            .session_manager
            .create_session(
                fixture.directory.path().to_path_buf(),
                "Other media session".into(),
                SessionType::Acp,
                LuminaMode::default(),
            )
            .await
            .unwrap();
        fixture.activate(&other.id).await;
        let request = MediaReadRequest {
            session_id: other.id,
            result_id: published.result_id.clone(),
        };
        assert_eq!(
            fixture.acp.on_media_read(request).await.unwrap(),
            MediaReadResponse::NotFound
        );

        let content = fixture
            .store
            .open_content(&fixture.session_id, RESULT_ID)
            .await
            .unwrap();
        let path = content.path().to_path_buf();
        drop(content);
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            fixture
                .acp
                .on_media_read(fixture.request(RESULT_ID))
                .await
                .unwrap(),
            MediaReadResponse::Found {
                result: Box::new(published.into()),
                availability: MediaAvailabilityDto::Missing,
            }
        );
        fixture.server.shutdown().await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn media_access_stays_with_the_current_connection_and_close_revokes_it() {
        let fixture = Fixture::new().await;
        let published = fixture.publish_image().await;
        let request = MediaAccessRequest {
            session_id: fixture.session_id.clone(),
            result_id: published.result_id.clone(),
            purpose: MediaAccessPurposeDto::Preview,
        };
        assert_inactive(
            fixture
                .acp
                .on_media_access(request.clone())
                .await
                .unwrap_err(),
        );
        assert!(fixture
            .acp
            .session_agent_if_current(&fixture.session_id)
            .await
            .is_none());
        fixture.activate(&fixture.session_id).await;
        let response = fixture.acp.on_media_access(request.clone()).await.unwrap();
        let store = fixture.server.media_access_store();
        let access = domain::MediaAccessRequest {
            ticket: &response.ticket,
            session_id: &fixture.session_id,
            result_id: &published.result_id,
            purpose: domain::MediaAccessPurpose::Preview,
        };
        let other = fixture.server.create_agent().await.unwrap();
        assert_inactive(other.on_media_access(request).await.unwrap_err());
        other
            .on_media_release(MediaReleaseRequest {
                ticket: response.ticket.clone(),
            })
            .await
            .unwrap();
        let lease = store.claim_content(&access).unwrap();
        fixture
            .acp
            .on_close_session(&fixture.session_id)
            .await
            .unwrap();
        assert!(store.claim_content(&access).is_err());
        assert_eq!(lease.read_range(0, 8).unwrap(), b"\x89PNG\r\n\x1a\n");
        fixture
            .acp
            .on_media_release(MediaReleaseRequest {
                ticket: response.ticket.clone(),
            })
            .await
            .unwrap();
        fixture
            .acp
            .on_media_release(MediaReleaseRequest {
                ticket: response.ticket,
            })
            .await
            .unwrap();
        drop(lease);
        fixture.server.shutdown().await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn media_access_release_disconnect_and_shutdown_revoke_without_reactivation() {
        let fixture = Fixture::new().await;
        let result = fixture.publish_image().await;
        fixture.activate(&fixture.session_id).await;
        let request = MediaAccessRequest {
            session_id: fixture.session_id.clone(),
            result_id: result.result_id.clone(),
            purpose: MediaAccessPurposeDto::SaveAs,
        };
        let store = fixture.server.media_access_store();
        let response = fixture.acp.on_media_access(request.clone()).await.unwrap();
        fixture
            .acp
            .on_media_release(MediaReleaseRequest {
                ticket: response.ticket.clone(),
            })
            .await
            .unwrap();
        assert!(store
            .claim_content(&domain::MediaAccessRequest {
                ticket: &response.ticket,
                session_id: &fixture.session_id,
                result_id: &result.result_id,
                purpose: domain::MediaAccessPurpose::SaveAs
            })
            .is_err());
        let response = fixture.acp.on_media_access(request.clone()).await.unwrap();
        let disconnect = fixture.acp.media_access_owner.disconnect_guard();
        drop(disconnect);
        assert!(store
            .claim_content(&domain::MediaAccessRequest {
                ticket: &response.ticket,
                session_id: &fixture.session_id,
                result_id: &result.result_id,
                purpose: domain::MediaAccessPurpose::SaveAs
            })
            .is_err());
        assert!(fixture.acp.on_media_access(request).await.is_err());
        fixture.server.shutdown().await;
        assert!(store
            .create_owner()
            .issue(
                fixture
                    .store
                    .open_content(&fixture.session_id, &result.result_id)
                    .await
                    .unwrap(),
                &fixture.session_id,
                &result.result_id,
                domain::MediaAccessPurpose::Preview,
            )
            .is_err());
    }
}
