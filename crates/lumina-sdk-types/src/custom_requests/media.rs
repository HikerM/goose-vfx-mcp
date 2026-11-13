use agent_client_protocol::{JsonRpcRequest, JsonRpcResponse};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MEDIA_READ_METHOD: &str = "_lumina/unstable/media/read";
pub const MEDIA_ACCESS_METHOD: &str = "_lumina/unstable/media/access";
pub const MEDIA_RELEASE_METHOD: &str = "_lumina/unstable/media/release";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaAccessPurposeDto {
    #[default]
    Preview,
    Open,
    Reveal,
    CopyPath,
    SaveAs,
}

#[derive(
    Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcRequest,
)]
#[request(method = "_lumina/unstable/media/access", response = MediaAccessResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaAccessRequest {
    pub session_id: String,
    pub result_id: String,
    pub purpose: MediaAccessPurposeDto,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaAccessResponse {
    pub ticket: String,
    pub expires_in_ms: u64,
}

impl std::fmt::Debug for MediaAccessResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaAccessResponse")
            .field("ticket", &"[redacted]")
            .field("expires_in_ms", &self.expires_in_ms)
            .finish()
    }
}

#[derive(Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method = "_lumina/unstable/media/release", response = MediaReleaseResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaReleaseRequest {
    pub ticket: String,
}

impl std::fmt::Debug for MediaReleaseRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MediaReleaseRequest")
            .field("ticket", &"[redacted]")
            .finish()
    }
}

#[derive(
    Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcResponse,
)]
#[serde(deny_unknown_fields)]
pub struct MediaReleaseResponse {}

#[derive(
    Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcRequest,
)]
#[request(method = "_lumina/unstable/media/read", response = MediaReadResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaReadRequest {
    pub session_id: String,
    /// Canonical UUID assigned when the result was reserved for publication.
    pub result_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum MediaReadResponse {
    Found {
        result: Box<MediaResultDto>,
        availability: MediaAvailabilityDto,
    },
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaKindDto {
    Image,
    Audio,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "associationKind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MediaAssociationDto {
    Message { message_id: String },
    ToolCall { tool_call_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MediaInputRefDto {
    Message { message_id: String },
    MediaResult { result_id: String },
    ContentDigest { sha256: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaRuntimeBuildDto {
    pub adapter_id: String,
    pub engine_revision: String,
    pub binary_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MediaSourceDto {
    LocalModel {
        installation_id: String,
        version_id: String,
        runtime: MediaRuntimeBuildDto,
        generation_config_sha256: String,
        input_refs: Vec<MediaInputRefDto>,
    },
    Mcp {
        server_id: String,
        tool_name: String,
        tool_call_id: String,
        input_refs: Vec<MediaInputRefDto>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaOwnershipDto {
    ApplicationGenerated,
    ApplicationImported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MediaSpecDto {
    Image {
        width: u32,
        height: u32,
    },
    Audio {
        duration_micros: u64,
        sample_rate: u32,
        channels: u16,
        sample_frames: u64,
    },
    Video {
        width: u32,
        height: u32,
        duration_micros: u64,
        frame_count: u64,
        fps_numerator: u32,
        fps_denominator: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaResultDto {
    pub schema_version: u32,
    pub result_id: String,
    pub session_id: String,
    pub association: MediaAssociationDto,
    pub job_id: String,
    pub kind: MediaKindDto,
    pub mime: String,
    pub file_name: String,
    pub bytes: u64,
    pub sha256: String,
    /// Unix milliseconds, fixed when publication was first reserved.
    pub created_at: i64,
    pub root_id: String,
    /// Relative to the registered output root; this is not an openable URL.
    pub relative_path: String,
    pub spec: MediaSpecDto,
    pub ownership: MediaOwnershipDto,
    pub source: MediaSourceDto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaAvailabilityDto {
    Available,
    Publishing,
    Missing,
    AccessDenied,
    Tampered,
    UnsafePath,
    UnsupportedFormat,
    IoUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::JsonRpcMessage;
    use serde_json::json;

    #[test]
    fn media_read_request_has_exact_method_and_only_logical_identifiers() {
        let value =
            json!({"sessionId": "session-1", "resultId": "12345678-1234-4234-8234-123456789abc"});
        let request: MediaReadRequest = serde_json::from_value(value.clone()).unwrap();
        let message = request.to_untyped_message().unwrap();
        assert_eq!(message.method(), MEDIA_READ_METHOD);
        assert_eq!(message.params(), &value);
        assert!(serde_json::from_value::<MediaReadRequest>(json!({})).is_err());
        assert!(serde_json::from_value::<MediaReadRequest>(json!({
            "sessionId": "session-1", "resultId": "result-1", "path": "C:/private/output.wav"
        }))
        .is_err());
    }

    #[test]
    fn media_read_not_found_is_explicit_and_contains_no_result_metadata() {
        assert_eq!(
            serde_json::to_value(MediaReadResponse::NotFound).unwrap(),
            json!({"status": "notFound"})
        );
        assert!(serde_json::from_value::<MediaReadResponse>(json!({"status": "found"})).is_err());
        assert_eq!(
            serde_json::from_value::<MediaReadResponse>(json!({"status": "notFound"})).unwrap(),
            MediaReadResponse::NotFound
        );
    }

    #[test]
    fn media_access_requires_explicit_scope_and_known_purpose() {
        for purpose in ["preview", "save_as", "open", "reveal", "copy_path"] {
            let value = json!({"sessionId":"session-1","resultId":"result-1","purpose":purpose});
            let request: MediaAccessRequest = serde_json::from_value(value.clone()).unwrap();
            let message = request.to_untyped_message().unwrap();
            assert_eq!(message.method(), MEDIA_ACCESS_METHOD);
            assert_eq!(message.params(), &value);
        }
        for invalid in [
            json!({"sessionId":"session-1","resultId":"result-1"}),
            json!({"sessionId":"session-1","resultId":"result-1","purpose":"fetch_remote"}),
            json!({"sessionId":"session-1","resultId":"result-1","purpose":"preview","path":"D:/any.wav"}),
        ] {
            assert!(serde_json::from_value::<MediaAccessRequest>(invalid).is_err());
        }
        let response = MediaAccessResponse {
            ticket: "private-capability".into(),
            expires_in_ms: 300_000,
        };
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({"ticket":"private-capability","expiresInMs":300000})
        );
        assert!(!format!("{response:?}").contains("private-capability"));
    }

    #[test]
    fn media_release_is_ticket_only_and_has_an_empty_response() {
        let request = MediaReleaseRequest {
            ticket: "private-capability".into(),
        };
        assert!(!format!("{request:?}").contains("private-capability"));
        let message = request.to_untyped_message().unwrap();
        assert_eq!(message.method(), MEDIA_RELEASE_METHOD);
        assert_eq!(message.params(), &json!({"ticket":"private-capability"}));
        assert!(serde_json::from_value::<MediaReleaseRequest>(
            json!({"ticket":"value","ownerId":"other"})
        )
        .is_err());
        assert_eq!(
            serde_json::to_value(MediaReleaseResponse {}).unwrap(),
            json!({})
        );
    }
}
