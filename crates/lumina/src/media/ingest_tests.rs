use super::*;
use std::io::Cursor;
use std::path::PathBuf;

use rmcp::model::{CallToolRequestParams, CallToolResult};

use crate::session::{SessionManager, SessionType};

struct Fixture {
    manager: SessionManager,
    session_id: String,
    data_dir: PathBuf,
    _temp: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("lumina-media-ingest-")
            .tempdir_in("E:/Codex/temp")
            .unwrap();
        let data_dir = temp.path().join("data");
        let manager = SessionManager::new(data_dir.clone());
        let session = manager
            .create_session(
                temp.path().to_owned(),
                "Media producer test".into(),
                SessionType::User,
                Default::default(),
            )
            .await
            .unwrap();
        Self {
            manager,
            session_id: session.id,
            data_dir,
            _temp: temp,
        }
    }

    async fn persist(&self, request: &ToolRequest, response: &mut Message) {
        persist_tool_media(self.manager.storage(), &self.session_id, request, response).await;
    }

    async fn hydrate(&self, response: Message) -> Message {
        let mut conversation = Conversation::new_unvalidated([response]);
        hydrate_tool_media(self.manager.storage(), &self.session_id, &mut conversation).await;
        conversation.messages_mut().remove(0)
    }

    async fn finish(self) {
        self.manager.storage().pool().await.unwrap().close().await;
    }
}

fn request(id: &str) -> ToolRequest {
    ToolRequest {
        id: id.into(),
        tool_call: Ok(
            CallToolRequestParams::new("camera__capture").with_arguments(
                serde_json::Map::from_iter([("subject".into(), json!("red apple"))]),
            ),
        ),
        metadata: None,
        tool_meta: Some(json!({"lumina_extension": "camera", "other.toolMetadata": true})),
    }
}

fn image(jpeg: bool, color: [u8; 3]) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(3, 2, image::Rgb(color));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(
            &mut bytes,
            if jpeg {
                image::ImageOutputFormat::Jpeg(90)
            } else {
                image::ImageOutputFormat::Png
            },
        )
        .unwrap();
    bytes.into_inner()
}

fn wav() -> Vec<u8> {
    let data: Vec<_> = (0..800_u16).flat_map(u16::to_le_bytes).collect();
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8000_u32.to_le_bytes());
    bytes.extend_from_slice(&16000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend(data);
    bytes
}

fn mixed_response(call_id: &str) -> Message {
    let png: Content = serde_json::from_value(json!({
        "type": "image", "mimeType": "image/png", "data": STANDARD.encode(image(false, [255, 0, 0])),
        "annotations": {"audience": ["assistant", "user"], "priority": 0.7, "lastModified": "2026-11-12T00:00:00Z"},
        "_meta": {"vendor.caption": "red apple", "vendor.nested": {"preserved": true}}
    })).unwrap();
    let audio: Content = serde_json::from_value(json!({
        "type": "audio", "mimeType": "audio/wav", "data": STANDARD.encode(wav()),
        "annotations": {"audience": ["assistant"], "priority": 0.6}
    }))
    .unwrap();
    let mut result = CallToolResult::success(vec![
        Content::text("Before media"),
        png,
        audio,
        Content::image(STANDARD.encode(image(true, [0, 255, 0])), "image/jpeg"),
        Content::text("After media"),
    ]);
    result.meta = Some(Meta(serde_json::Map::from_iter([(
        "vendor.result".into(),
        json!(42),
    )])));
    Message::user()
        .with_generated_id()
        .with_tool_response(call_id, Ok(result))
}

fn result(message: &Message) -> &CallToolResult {
    let MessageContent::ToolResponse(response) = &message.content[0] else {
        panic!("Expected tool response")
    };
    response.tool_result.as_ref().unwrap()
}

fn result_mut(message: &mut Message) -> &mut CallToolResult {
    let MessageContent::ToolResponse(response) = &mut message.content[0] else {
        panic!("Expected tool response")
    };
    response.tool_result.as_mut().unwrap()
}

fn link(message: &Message, index: usize) -> &RawResource {
    let RawContent::ResourceLink(link) = &result(message).content[index].raw else {
        panic!("Expected persisted resource link")
    };
    link
}

#[cfg(windows)]
#[tokio::test]
async fn real_session_roundtrip_persists_refs_and_hydrates_only_provider_copy() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture.manager.storage().media_output_directory(),
        fixture.data_dir.join("outputs")
    );
    let request = request("call-roundtrip");
    let original = mixed_response(&request.id);
    let mut published = original.clone();
    fixture.persist(&request, &mut published).await;
    assert_eq!(result(&published).content.len(), 5);
    assert_eq!(result(&published).meta, result(&original).meta);
    for index in 1..4 {
        let resource = link(&published, index);
        let id = result_id(&fixture.session_id, &request.id, index);
        assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 8);
        assert_eq!(resource.uri, format!("{URI_PREFIX}{id}"));
        assert_eq!(
            resource.meta.as_ref().unwrap().0[RESULT_META],
            json!({"schemaVersion":1,"sessionId":fixture.session_id,"resultId":id})
        );
        assert_eq!(
            result(&published).content[index].annotations,
            result(&original).content[index].annotations
        );
        let store = MediaResultStore::new(fixture.manager.storage().pool().await.unwrap().clone());
        let stored = store
            .read(&fixture.session_id, &id)
            .await
            .unwrap()
            .unwrap()
            .result;
        assert_eq!(resource.name, stored.file_name);
        assert_eq!(resource.mime_type.as_ref(), Some(&stored.mime));
        assert_eq!(resource.size.map(u64::from), Some(stored.bytes));
        let MediaSource::Mcp {
            server_id,
            tool_name,
            tool_call_id,
            input_refs,
        } = stored.source
        else {
            panic!("Expected real MCP provenance")
        };
        assert_eq!(
            (
                server_id.as_str(),
                tool_name.as_str(),
                tool_call_id.as_str()
            ),
            ("camera", "capture", request.id.as_str())
        );
        assert_eq!(input_refs.len(), 2);
        assert_eq!(
            input_refs[1],
            MediaInputRef::ContentDigest {
                sha256: stored.sha256
            }
        );
    }
    let request_message = Message::assistant()
        .with_generated_id()
        .with_content(MessageContent::ToolRequest(request));
    fixture
        .manager
        .add_message(&fixture.session_id, &request_message)
        .await
        .unwrap();
    fixture
        .manager
        .add_message(&fixture.session_id, &published)
        .await
        .unwrap();
    fixture
        .manager
        .storage()
        .pool()
        .await
        .unwrap()
        .close()
        .await;

    let restarted = SessionManager::new(fixture.data_dir.clone());
    let persisted = restarted
        .get_session(&fixture.session_id, true)
        .await
        .unwrap()
        .conversation
        .unwrap();
    let serialized = serde_json::to_string(&persisted).unwrap();
    assert!(serialized.contains(URI_PREFIX));
    assert!(!serialized.contains(&STANDARD.encode(wav())));
    assert!(!serialized.contains(&STANDARD.encode(image(false, [255, 0, 0]))));
    let before = serialized;
    let mut provider_copy = persisted.clone();
    hydrate_tool_media(restarted.storage(), &fixture.session_id, &mut provider_copy).await;
    assert_eq!(
        result(provider_copy.messages().last().unwrap()).content,
        result(&original).content
    );
    assert_eq!(serde_json::to_string(&persisted).unwrap(), before);
    let reread = restarted
        .get_session(&fixture.session_id, true)
        .await
        .unwrap()
        .conversation
        .unwrap();
    assert_eq!(serde_json::to_string(&reread).unwrap(), before);
    restarted.storage().pool().await.unwrap().close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn duplicate_events_reuse_results_and_changed_bytes_cannot_replace_them() {
    let fixture = Fixture::new().await;
    let request = request("call-repeat");
    let mut first = mixed_response(&request.id);
    let mut duplicate = first.clone();
    fixture.persist(&request, &mut first).await;
    fixture.persist(&request, &mut duplicate).await;
    assert_eq!(first, duplicate);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_results_v1")
        .fetch_one(fixture.manager.storage().pool().await.unwrap())
        .await
        .unwrap();
    assert_eq!(count, 3);

    let mut changed = mixed_response(&request.id);
    let RawContent::Image(image_content) = &mut result_mut(&mut changed).content[1].raw else {
        panic!()
    };
    image_content.data = STANDARD.encode(image(false, [0, 0, 255]));
    fixture.persist(&request, &mut changed).await;
    assert!(matches!(
        &result(&changed).content[1].raw,
        RawContent::Image(_)
    ));
    assert_eq!(
        result(&changed).meta.as_ref().unwrap().0[PERSISTENCE_META]["status"],
        "partial"
    );
    assert_ne!(result(&changed).is_error, Some(true));
    let hydrated = fixture.hydrate(first).await;
    assert_eq!(
        result(&hydrated).content,
        result(&mixed_response(&request.id)).content
    );
    fixture.finish().await;
}

#[cfg(windows)]
#[tokio::test]
async fn cross_session_unknown_and_foreign_links_never_hydrate() {
    let fixture = Fixture::new().await;
    let request = request("call-scope");
    let mut published = mixed_response(&request.id);
    fixture.persist(&request, &mut published).await;
    let mut foreign_session = Conversation::new_unvalidated([published.clone()]);
    hydrate_tool_media(
        fixture.manager.storage(),
        "another-session",
        &mut foreign_session,
    )
    .await;
    let rejected = foreign_session.messages().last().unwrap();
    assert!(link(rejected, 1)
        .description
        .as_ref()
        .unwrap()
        .contains("invalid_reference"));

    let mut unknown = published.clone();
    let MessageContent::ToolResponse(response) = &mut unknown.content[0] else {
        panic!()
    };
    response.id = "unknown-call".into();
    let RawContent::ResourceLink(resource) =
        &mut response.tool_result.as_mut().unwrap().content[1].raw
    else {
        panic!()
    };
    let id = result_id(&fixture.session_id, "unknown-call", 1);
    resource.uri = format!("{URI_PREFIX}{id}");
    resource.meta.as_mut().unwrap().0[RESULT_META]["resultId"] = json!(id);
    let unknown = fixture.hydrate(unknown).await;
    assert!(link(&unknown, 1)
        .description
        .as_ref()
        .unwrap()
        .contains("missing_or_inaccessible"));

    for uri in [
        "file:///E:/private.png",
        "https://example.invalid/media.png",
        "lumina-media://result/not-a-uuid",
    ] {
        let mut response = published.clone();
        let RawContent::ResourceLink(resource) = &mut result_mut(&mut response).content[1].raw
        else {
            panic!()
        };
        resource.uri = uri.into();
        let response = fixture.hydrate(response).await;
        assert_eq!(link(&response, 1).uri, uri);
        if uri.starts_with("lumina-media:") {
            assert!(link(&response, 1)
                .description
                .as_ref()
                .unwrap()
                .contains("invalid_reference"));
        }
    }
    fixture.finish().await;
}

#[cfg(windows)]
#[tokio::test]
async fn missing_tampered_and_oversized_refs_stay_explicitly_unavailable() {
    let fixture = Fixture::new().await;
    for mode in ["missing", "tampered", "oversized"] {
        let request = request(&format!("call-{mode}"));
        let mut response = mixed_response(&request.id);
        fixture.persist(&request, &mut response).await;
        let id = result_id(&fixture.session_id, &request.id, 1);
        let store = MediaResultStore::new(fixture.manager.storage().pool().await.unwrap().clone());
        let stored = store
            .read(&fixture.session_id, &id)
            .await
            .unwrap()
            .unwrap()
            .result;
        let path = fixture
            .manager
            .storage()
            .media_output_directory()
            .join(stored.relative_path);
        match mode {
            "missing" => std::fs::remove_file(path).unwrap(),
            "tampered" => std::fs::write(path, b"changed").unwrap(),
            _ => {
                let RawContent::ResourceLink(resource) =
                    &mut result_mut(&mut response).content[1].raw
                else {
                    panic!()
                };
                resource.size = Some(MAX_INLINE_BYTES as u32 + 1);
            }
        }
        let response = fixture.hydrate(response).await;
        assert!(link(&response, 1)
            .description
            .as_ref()
            .unwrap()
            .contains("unavailable"));
        assert_ne!(result(&response).is_error, Some(true));
    }
    fixture.finish().await;
}

#[cfg(windows)]
#[tokio::test]
async fn failed_saving_preserves_inline_tool_success_and_safe_error_metadata() {
    let fixture = Fixture::new().await;
    std::fs::write(
        fixture.manager.storage().media_output_directory(),
        b"not a directory",
    )
    .unwrap();
    let request = request("call-save-failure");
    let original = mixed_response(&request.id);
    let mut response = original.clone();
    fixture.persist(&request, &mut response).await;
    assert_eq!(
        &result(&response).content[..5],
        result(&original).content.as_slice()
    );
    assert_eq!(result(&response).is_error, result(&original).is_error);
    let metadata = &result(&response).meta.as_ref().unwrap().0;
    assert_eq!(metadata["vendor.result"], 42);
    assert_eq!(metadata[PERSISTENCE_META]["status"], "failed");
    assert_eq!(
        metadata[PERSISTENCE_META]["failures"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let serialized = serde_json::to_string(&response).unwrap();
    assert!(!serialized.contains("E:"));
    assert!(!serialized.contains("not a directory"));
    assert_eq!(result(&response).content.len(), 6);
    fixture.finish().await;
}

#[cfg(windows)]
#[tokio::test]
async fn unsupported_sources_mime_and_failed_tool_calls_are_unchanged() {
    let fixture = Fixture::new().await;
    let mut request = request("call-unowned");
    request.tool_meta = None;
    request.tool_call.as_mut().unwrap().name = "capture".into();
    let mut response = mixed_response(&request.id);
    let original = response.clone();
    fixture.persist(&request, &mut response).await;
    assert_eq!(response, original);

    request.tool_call.as_mut().unwrap().name = "camera__capture".into();
    result_mut(&mut response).is_error = Some(true);
    let original = response.clone();
    fixture.persist(&request, &mut response).await;
    assert_eq!(response, original);

    let mut response = Message::user().with_tool_response(
        &request.id,
        Ok(CallToolResult::success(vec![Content::image(
            "untouched",
            "image/webp",
        )])),
    );
    let original = response.clone();
    fixture.persist(&request, &mut response).await;
    assert_eq!(response, original);
    let mut empty = Conversation::empty();
    hydrate_tool_media(fixture.manager.storage(), &fixture.session_id, &mut empty).await;
    assert!(empty.messages().is_empty());
    fixture.finish().await;
}

#[cfg(windows)]
#[tokio::test]
async fn newest_tool_media_is_hydrated_before_older_history_consumes_the_budget() {
    let fixture = Fixture::new().await;
    let request = request("call-current-screenshot");
    let original = mixed_response(&request.id);
    let mut current = original.clone();
    fixture.persist(&request, &mut current).await;
    let mut history = Vec::new();
    for index in 0..MAX_REQUEST_REFERENCES {
        history.push(Message::user().with_tool_response(
            format!("old-call-{index}"),
            Ok(CallToolResult::success(vec![
                result(&current).content[1].clone(),
            ])),
        ));
    }
    history.push(current.clone());
    let mut conversation = Conversation::new_unvalidated(history);
    hydrate_tool_media(
        fixture.manager.storage(),
        &fixture.session_id,
        &mut conversation,
    )
    .await;
    assert_eq!(
        result(conversation.messages().last().unwrap()).content,
        result(&original).content
    );
    assert!(matches!(
        &result(&current).content[1].raw,
        RawContent::ResourceLink(_)
    ));
    fixture.finish().await;
}

#[test]
fn inline_decode_checks_budget_mime_and_invalid_base64_before_publication() {
    let png = STANDARD.encode(image(false, [1, 2, 3]));
    assert_eq!(
        decode_inline("image/png", &png, &mut 1).unwrap_err(),
        "request_limit"
    );
    let mut budget = MAX_REQUEST_BYTES;
    assert_eq!(
        decode_inline("audio/wav", &png, &mut budget).unwrap_err(),
        "invalid_content"
    );
    assert_eq!(
        decode_inline("image/png", "%%%not-base64", &mut budget).unwrap_err(),
        "invalid_content"
    );
    let mut remaining = MAX_REQUEST_BYTES;
    let bytes = decode_inline("image/png", &png, &mut remaining).unwrap();
    assert_eq!(remaining, MAX_REQUEST_BYTES - bytes.len());
    assert_eq!(bytes, image(false, [1, 2, 3]));
}
