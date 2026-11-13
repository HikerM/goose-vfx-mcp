use std::io::Write;

use anyhow::{ensure, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rmcp::model::{Content, Meta, RawAudioContent, RawContent, RawImageContent, RawResource};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::conversation::message::{Message, MessageContent, ToolRequest};
use crate::conversation::Conversation;
use crate::session::session_manager::SessionStorage;

use super::{
    files, BeginMediaPublish, MediaAssociation, MediaInputRef, MediaKind, MediaResultRef,
    MediaResultStore, MediaSource, PublishMediaRequest, MAX_MEDIA_READ_BYTES,
};

const MAX_INLINE_BYTES: usize = 32 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_REQUEST_REFERENCES: usize = 64;
const RESULT_META: &str = "lumina.mediaResult";
const PERSISTENCE_META: &str = "lumina.mediaPersistence";
const URI_PREFIX: &str = "lumina-media://result/";

fn result_id(session_id: &str, call_id: &str, index: usize) -> String {
    let mut hash = Sha256::new();
    for part in ["lumina-mcp-media-v1", session_id, call_id] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.update((index as u64).to_le_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finalize()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn source(request: &ToolRequest) -> Option<MediaSource> {
    let call = request.tool_call.as_ref().ok()?;
    let (server_id, tool_name) = if let Some(owner) = request
        .tool_meta
        .as_ref()
        .and_then(|meta| meta.get("lumina_extension"))
        .and_then(|owner| owner.as_str())
        .filter(|owner| !owner.trim().is_empty())
    {
        (
            owner.to_owned(),
            call.name
                .strip_prefix(&format!("{owner}__"))
                .unwrap_or(&call.name)
                .to_owned(),
        )
    } else {
        let (owner, tool) = call.name.split_once("__")?;
        if owner.is_empty() || tool.is_empty() {
            return None;
        }
        (
            crate::config::extensions::name_to_key(owner),
            tool.to_owned(),
        )
    };
    Some(MediaSource::Mcp {
        server_id,
        tool_name,
        tool_call_id: request.id.clone(),
        input_refs: vec![MediaInputRef::ContentDigest {
            sha256: files::request_digest(&call.arguments).ok()?,
        }],
    })
}

fn inline(content: &Content) -> Option<(MediaKind, &str, &str)> {
    match &content.raw {
        RawContent::Image(image)
            if matches!(image.mime_type.as_str(), "image/png" | "image/jpeg") =>
        {
            Some((MediaKind::Image, &image.mime_type, &image.data))
        }
        RawContent::Audio(audio) if audio.mime_type == "audio/wav" => {
            Some((MediaKind::Audio, &audio.mime_type, &audio.data))
        }
        _ => None,
    }
}

fn decode_inline(
    mime: &str,
    encoded: &str,
    remaining: &mut usize,
) -> Result<Vec<u8>, &'static str> {
    if encoded.len() > MAX_INLINE_BYTES.div_ceil(3) * 4 {
        return Err("inline_limit");
    }
    let estimate = (encoded.len() / 4 * 3).saturating_sub(
        encoded
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'=')
            .count(),
    );
    if estimate > MAX_INLINE_BYTES {
        return Err("inline_limit");
    }
    if estimate > *remaining {
        return Err("request_limit");
    }
    *remaining -= estimate;
    let bytes = STANDARD.decode(encoded).map_err(|_| "invalid_content")?;
    let signature_matches = match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8"),
        "audio/wav" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE"),
        _ => false,
    };
    if bytes.is_empty() || bytes.len() > estimate || !signature_matches {
        return Err("invalid_content");
    }
    Ok(bytes)
}

async fn publish_inline(
    storage: &SessionStorage,
    session_id: &str,
    call_id: &str,
    index: usize,
    kind: MediaKind,
    mut source: MediaSource,
    bytes: Vec<u8>,
) -> Result<MediaResultRef> {
    let store = MediaResultStore::new(storage.pool().await?.clone());
    let root = store
        .register_output_root(&storage.media_output_directory())
        .await?;
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let MediaSource::Mcp { input_refs, .. } = &mut source else {
        anyhow::bail!("Expected an MCP source")
    };
    input_refs.push(MediaInputRef::ContentDigest { sha256: digest });
    let request = PublishMediaRequest {
        result_id: result_id(session_id, call_id, index),
        session_id: session_id.to_owned(),
        association: MediaAssociation::ToolCall {
            tool_call_id: call_id.to_owned(),
        },
        job_id: format!("tool:{}", files::request_digest(&call_id)?),
        root_id: root.root_id,
        kind,
        source,
    };
    match store.begin_publish(request).await? {
        BeginMediaPublish::Existing(result) => Ok(result),
        BeginMediaPublish::Staging(mut staged) => {
            let staged = tokio::task::spawn_blocking(move || -> Result<_> {
                staged.write_all(&bytes)?;
                Ok(staged)
            })
            .await??;
            store.publish(staged).await
        }
    }
}

#[cfg(feature = "local-inference")]
pub(crate) fn native_result_link(result: &MediaResultRef) -> Content {
    let mut link = resource(&Content::text(""), result);
    if let RawContent::ResourceLink(resource) = &mut link.raw {
        resource
            .meta
            .get_or_insert_default()
            .0
            .insert("lumina.localModelResult".into(), json!(true));
    }
    link
}

fn resource(content: &Content, result: &MediaResultRef) -> Content {
    let mut meta = match &content.raw {
        RawContent::Image(image) => image.meta.clone().unwrap_or_default(),
        _ => Meta::default(),
    };
    meta.0.insert(
        RESULT_META.into(),
        json!({"schemaVersion": 1, "sessionId": result.session_id, "resultId": result.result_id}),
    );
    Content::new(
        RawContent::ResourceLink(RawResource {
            uri: format!("{URI_PREFIX}{}", result.result_id),
            name: result.file_name.clone(),
            title: None,
            description: Some("Saved tool media".into()),
            mime_type: Some(result.mime.clone()),
            size: Some(result.bytes as u32),
            icons: None,
            meta: Some(meta),
        }),
        content.annotations.clone(),
    )
}

/// Converts successful inline tool media before the same message reaches live
/// clients and SQLite. Storage failure never changes the tool execution status.
pub(crate) async fn persist_tool_media(
    storage: &SessionStorage,
    session_id: &str,
    request: &ToolRequest,
    response: &mut Message,
) {
    let Some(source) = source(request) else {
        return;
    };
    let mut remaining = MAX_REQUEST_BYTES;
    let mut references = 0;
    for part in &mut response.content {
        let MessageContent::ToolResponse(response) = part else {
            continue;
        };
        if response.id != request.id {
            continue;
        }
        let Ok(result) = &mut response.tool_result else {
            continue;
        };
        if result.is_error == Some(true) {
            continue;
        }
        let mut failures = Vec::new();
        let mut saved = 0;
        for (index, content) in result.content.iter_mut().enumerate() {
            let Some((kind, mime, encoded)) = inline(content) else {
                continue;
            };
            references += 1;
            let decoded = if references > MAX_REQUEST_REFERENCES {
                Err("request_limit")
            } else {
                decode_inline(mime, encoded, &mut remaining)
            };
            let published = match decoded {
                Ok(bytes) => publish_inline(
                    storage,
                    session_id,
                    &request.id,
                    index,
                    kind,
                    source.clone(),
                    bytes,
                )
                .await
                .map_err(|_| "save_failed"),
                Err(code) => Err(code),
            };
            match published {
                Ok(published) if published.mime == mime => {
                    *content = resource(content, &published);
                    saved += 1;
                }
                Ok(_) => failures.push(json!({"contentIndex": index, "code": "invalid_content"})),
                Err(code) => failures.push(json!({"contentIndex": index, "code": code})),
            }
        }
        if !failures.is_empty() {
            result.meta.get_or_insert_default().0.insert(
                PERSISTENCE_META.into(),
                json!({"schemaVersion": 1, "status": if saved == 0 { "failed" } else { "partial" }, "failures": failures}),
            );
            let mut warning = Content::text("The tool completed, but some media could not be saved. The original inline content has been retained; no saved reference was issued for that content.");
            if let RawContent::Text(text) = &mut warning.raw {
                text.meta = Some(Meta(serde_json::Map::from_iter([(
                    PERSISTENCE_META.into(),
                    json!({"schemaVersion": 1, "status": "save_failed"}),
                )])));
            }
            if !result.content.iter().any(|content| {
                matches!(&content.raw, RawContent::Text(text) if text.meta.as_ref().is_some_and(|meta| meta.0.get(PERSISTENCE_META).and_then(|value| value.get("status")).and_then(|value| value.as_str()) == Some("save_failed")))
            }) {
                result.content.push(warning);
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reference {
    schema_version: u32,
    session_id: String,
    result_id: String,
}

fn reference(
    link: &RawResource,
    session_id: &str,
    call_id: &str,
    index: usize,
) -> Option<Reference> {
    let marker: Reference =
        serde_json::from_value(link.meta.as_ref()?.0.get(RESULT_META)?.clone()).ok()?;
    let canonical = uuid::Uuid::parse_str(&marker.result_id).ok()?.to_string();
    (marker.schema_version == 1
        && marker.session_id == session_id
        && marker.result_id == canonical
        && (is_local_model_link(link) || marker.result_id == result_id(session_id, call_id, index))
        && link.uri == format!("{URI_PREFIX}{canonical}"))
    .then_some(marker)
}

fn is_local_model_link(link: &RawResource) -> bool {
    link.meta
        .as_ref()
        .and_then(|meta| meta.0.get("lumina.localModelResult"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

fn unavailable(link: &mut RawResource, code: &str) {
    link.description = Some(format!(
        "Media content is unavailable ({code}). The saved reference is retained; no replacement was generated."
    ));
    link.meta.get_or_insert_default().0.insert(
        "lumina.mediaAvailability".into(),
        json!({"schemaVersion": 1, "status": "unavailable", "code": code}),
    );
}

async fn hydrate(
    store: &MediaResultStore,
    session_id: &str,
    call_id: &str,
    marker: Reference,
    link: &RawResource,
) -> Result<Option<RawContent>> {
    let mut content = store.open_content(session_id, &marker.result_id).await?;
    let result = content.result();
    ensure!(
        Some(result.bytes) == link.size.map(u64::from)
            && link.mime_type.as_ref() == Some(&result.mime)
            && link.name == result.file_name
            && result.association
                == MediaAssociation::ToolCall {
                    tool_call_id: call_id.to_owned()
                }
            && match &result.source {
                MediaSource::LocalModel { .. } => is_local_model_link(link),
                MediaSource::Mcp { tool_call_id, .. } =>
                    !is_local_model_link(link) && tool_call_id == call_id,
            },
        "Media reference does not match its verified tool result"
    );
    let kind = result.kind;
    let mime_type = result.mime.clone();
    if kind == MediaKind::Video {
        return Ok(None);
    }
    ensure!(
        result.bytes <= MAX_INLINE_BYTES as u64,
        "Inline media exceeds the request limit"
    );
    ensure!(
        matches!(
            (kind, mime_type.as_str()),
            (MediaKind::Image, "image/png" | "image/jpeg") | (MediaKind::Audio, "audio/wav")
        ),
        "Unsupported inline media reference"
    );
    let length = usize::try_from(result.bytes)?;
    let data = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut bytes = Vec::with_capacity(length);
        while bytes.len() < length {
            bytes.extend(content.read_range(
                bytes.len() as u64,
                (length - bytes.len()).min(MAX_MEDIA_READ_BYTES),
            )?);
        }
        Ok(STANDARD.encode(bytes))
    })
    .await??;
    match kind {
        MediaKind::Image => {
            let mut meta = link.meta.clone().unwrap_or_default();
            meta.0.remove(RESULT_META);
            Ok(Some(RawContent::Image(RawImageContent {
                data,
                mime_type,
                meta: (!meta.0.is_empty()).then_some(meta),
            })))
        }
        MediaKind::Audio => Ok(Some(RawContent::Audio(RawAudioContent { data, mime_type }))),
        MediaKind::Video => unreachable!("Video stays a verified resource link"),
    }
}

/// Hydrates only the provider's temporary conversation. The durable conversation
/// and its live/replayed notifications continue to contain the same references.
pub(crate) async fn hydrate_tool_media(
    storage: &SessionStorage,
    session_id: &str,
    conversation: &mut Conversation,
) {
    let mut store = None;
    let mut remaining = MAX_REQUEST_BYTES;
    let mut references = 0;
    // Older history must not consume the budget before the current screenshot.
    for message in conversation.messages_mut().iter_mut().rev() {
        for part in message.content.iter_mut().rev() {
            let MessageContent::ToolResponse(response) = part else {
                continue;
            };
            let Ok(result) = &mut response.tool_result else {
                continue;
            };
            for (index, content) in result.content.iter_mut().enumerate().rev() {
                let RawContent::ResourceLink(link) = &mut content.raw else {
                    continue;
                };
                if !link.uri.starts_with("lumina-media:") {
                    continue;
                }
                references += 1;
                let Some(marker) = reference(link, session_id, &response.id, index) else {
                    unavailable(link, "invalid_reference");
                    continue;
                };
                let length = if is_local_model_link(link)
                    && link.mime_type.as_deref() == Some("video/x-msvideo")
                {
                    0
                } else {
                    link.size.map(|size| size as usize).unwrap_or(usize::MAX)
                };
                if references > MAX_REQUEST_REFERENCES
                    || length > MAX_INLINE_BYTES
                    || length > remaining
                {
                    unavailable(link, "request_limit");
                    continue;
                }
                remaining -= length;
                if store.is_none() {
                    store = storage
                        .pool()
                        .await
                        .ok()
                        .map(|pool| MediaResultStore::new(pool.clone()));
                }
                let Some(store) = &store else {
                    unavailable(link, "storage_unavailable");
                    continue;
                };
                match hydrate(store, session_id, &response.id, marker, link).await {
                    Ok(Some(raw)) => content.raw = raw,
                    Ok(None) => {}
                    Err(_) => unavailable(link, "missing_or_inaccessible"),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod tests;
