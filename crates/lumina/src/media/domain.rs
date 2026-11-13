use std::fmt;
use std::path::PathBuf;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

pub const MAX_MEDIA_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_MEDIA_READ_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "associationKind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MediaAssociation {
    Message { message_id: String },
    ToolCall { tool_call_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MediaInputRef {
    Message { message_id: String },
    MediaResult { result_id: String },
    ContentDigest { sha256: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaRuntimeBuild {
    pub adapter_id: String,
    pub engine_revision: String,
    pub binary_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MediaSource {
    LocalModel {
        installation_id: String,
        version_id: String,
        runtime: MediaRuntimeBuild,
        generation_config_sha256: String,
        input_refs: Vec<MediaInputRef>,
    },
    Mcp {
        server_id: String,
        tool_name: String,
        tool_call_id: String,
        input_refs: Vec<MediaInputRef>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOwnership {
    ApplicationGenerated,
    ApplicationImported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum MediaSpec {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaOutputRoot {
    pub root_id: String,
    pub path: PathBuf,
    pub identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishMediaRequest {
    pub result_id: String,
    pub session_id: String,
    pub job_id: String,
    pub root_id: String,
    pub kind: MediaKind,
    pub association: MediaAssociation,
    pub source: MediaSource,
}

impl PublishMediaRequest {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&self.result_id)?.to_string() == self.result_id,
            "Media result ID must be a canonical UUID"
        );
        validate_id(&self.session_id)?;
        validate_id(&self.job_id)?;
        validate_id(&self.root_id)?;
        match &self.association {
            MediaAssociation::Message { message_id } => validate_text(message_id)?,
            MediaAssociation::ToolCall { tool_call_id } => validate_text(tool_call_id)?,
        }
        let inputs = match &self.source {
            MediaSource::LocalModel {
                installation_id,
                version_id,
                runtime,
                generation_config_sha256,
                input_refs,
            } => {
                for value in [
                    installation_id,
                    version_id,
                    &runtime.adapter_id,
                    &runtime.engine_revision,
                ] {
                    validate_text(value)?;
                }
                validate_digest(&runtime.binary_sha256)?;
                validate_digest(generation_config_sha256)?;
                input_refs
            }
            MediaSource::Mcp {
                server_id,
                tool_name,
                tool_call_id,
                input_refs,
            } => {
                for value in [server_id, tool_name, tool_call_id] {
                    validate_text(value)?;
                }
                if let MediaAssociation::ToolCall {
                    tool_call_id: association,
                } = &self.association
                {
                    ensure!(
                        association == tool_call_id,
                        "MCP result belongs to a different tool call"
                    );
                }
                input_refs
            }
        };
        ensure!(inputs.len() <= 128, "Too many media input references");
        for input in inputs {
            match input {
                MediaInputRef::Message { message_id } => validate_text(message_id)?,
                MediaInputRef::MediaResult { result_id } => {
                    uuid::Uuid::parse_str(result_id)?;
                }
                MediaInputRef::ContentDigest { sha256 } => validate_digest(sha256)?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaResultRef {
    pub schema_version: u32,
    pub result_id: String,
    pub session_id: String,
    pub association: MediaAssociation,
    pub job_id: String,
    pub kind: MediaKind,
    pub mime: String,
    pub file_name: String,
    pub bytes: u64,
    pub sha256: String,
    /// Unix milliseconds, fixed when this publication is first reserved.
    pub created_at: i64,
    pub root_id: String,
    pub relative_path: String,
    pub spec: MediaSpec,
    pub ownership: MediaOwnership,
    pub source: MediaSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaAvailability {
    Available,
    Publishing,
    Missing,
    AccessDenied,
    Tampered,
    UnsafePath,
    UnsupportedFormat,
    IoUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaResultStatus {
    pub result: MediaResultRef,
    pub availability: MediaAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRecoveryOutcome {
    pub result_id: String,
    pub availability: MediaAvailability,
}

#[derive(Debug)]
pub struct MediaAccessError {
    pub availability: MediaAvailability,
    message: &'static str,
}

impl MediaAccessError {
    pub(crate) fn new(availability: MediaAvailability, message: &'static str) -> Self {
        Self {
            availability,
            message,
        }
    }

    pub(crate) fn io(error: std::io::Error) -> Self {
        let availability = match error.kind() {
            std::io::ErrorKind::NotFound => MediaAvailability::Missing,
            std::io::ErrorKind::PermissionDenied => MediaAvailability::AccessDenied,
            _ if matches!(error.raw_os_error(), Some(32 | 33)) => MediaAvailability::AccessDenied,
            _ => MediaAvailability::IoUnavailable,
        };
        Self::new(availability, "Media file access failed")
    }
}

impl fmt::Display for MediaAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for MediaAccessError {}

pub(crate) fn validate_id(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 96 && !value.chars().any(char::is_control),
        "Invalid media scope identifier"
    );
    Ok(())
}

fn validate_text(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control),
        "Invalid media provenance identifier"
    );
    Ok(())
}

pub(crate) fn validate_digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Invalid media content digest"
    );
    Ok(())
}

pub(crate) fn segment(value: &str) -> String {
    // Fixed UTF-8 hex encoding is reversible and injective even on case-insensitive
    // Windows volumes. Logical IDs remain unchanged in the persisted record.
    let mut encoded = String::from("id-");
    for byte in value.bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}
