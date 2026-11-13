use agent_client_protocol::{JsonRpcRequest, JsonRpcResponse};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const NATIVE_TASK_LIST_METHOD: &str = "_lumina/unstable/local-inference/tasks/list";
pub const NATIVE_TASK_DESCRIBE_METHOD: &str = "_lumina/unstable/local-inference/tasks/describe";

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NativeTaskIntentDto {
    #[default]
    TextToImage,
    TextToVideo,
    TextToVideoWithAudio,
    FirstFrameToVideoWithAudio,
    SpeechSynthesis,
    Transcription,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTaskIdentityDto {
    pub installation_id: String,
    pub version_id: String,
    pub manifest_digest: String,
    pub runtime_digest: String,
    pub component_binding_digest: String,
    pub task_variant: String,
    pub parameter_schema_id: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method="_lumina/unstable/local-inference/tasks/list", response=NativeTaskListResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTaskListRequest {
    pub session_id: String,
    pub intent: NativeTaskIntentDto,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, JsonRpcRequest)]
#[request(method="_lumina/unstable/local-inference/tasks/describe", response=NativeTaskDescribeResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTaskDescribeRequest {
    pub session_id: String,
    pub identity: NativeTaskIdentityDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTaskListResponse {
    pub tasks: Vec<NativeTaskDescriptionDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, JsonRpcResponse)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeTaskDescribeResponse {
    Found { task: Box<NativeTaskDescriptionDto> },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTaskDescriptionDto {
    pub identity: NativeTaskIdentityDto,
    pub intent: NativeTaskIntentDto,
    pub descriptor: NativeTaskParameterContractDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "family", content = "parameters", rename_all = "snake_case")]
pub enum NativeTaskParameterContractDto {
    Generation(Box<NativeGenerationDescriptorDto>),
    PocketTts(Box<NativePocketTtsDescriptorDto>),
    Qwen3Asr(Box<NativeQwen3AsrDescriptorDto>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NativeParameterSupportDto {
    NativeSupported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGenerationFieldDto {
    pub value_type: String,
    pub support: NativeParameterSupportDto,
    pub default: Option<Value>,
    pub default_source: Option<String>,
    pub unit: Option<String>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub step: Option<f64>,
    pub enum_values: Vec<Value>,
    pub constraints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeAudioFieldDto {
    pub value_type: String,
    pub required: bool,
    pub default: Option<Value>,
    pub native_option: Option<String>,
    pub constraints: Vec<String>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGenerationDescriptorDto {
    pub schema_version: u32,
    pub profile: String,
    pub fields: BTreeMap<String, NativeGenerationFieldDto>,
    pub execution_status: String,
    pub evidence: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativePocketTtsDescriptorDto {
    pub fields: BTreeMap<String, NativeAudioFieldDto>,
    pub unsupported: BTreeMap<String, String>,
    pub model_language: String,
    pub output_sample_rate: u32,
    pub evidence: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeQwen3AsrDescriptorDto {
    pub fields: BTreeMap<String, NativeAudioFieldDto>,
    pub unsupported: BTreeMap<String, String>,
    pub recognition_languages: Vec<String>,
    pub evidence: String,
    pub limitations: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::JsonRpcMessage;
    use serde_json::json;

    #[test]
    fn task_routes_require_explicit_session_intent_or_complete_identity() {
        assert!(NativeTaskListRequest::matches_method(
            NATIVE_TASK_LIST_METHOD
        ));
        assert!(NativeTaskDescribeRequest::matches_method(
            NATIVE_TASK_DESCRIBE_METHOD
        ));
        assert!(serde_json::from_value::<NativeTaskListRequest>(json!({"sessionId":"s"})).is_err());
        assert!(serde_json::from_value::<NativeTaskListRequest>(
            json!({"intent":"speech_synthesis"})
        )
        .is_err());
        assert!(serde_json::from_value::<NativeTaskDescribeRequest>(
            json!({"sessionId":"s","identity":{"versionId":"v"}})
        )
        .is_err());
        assert!(serde_json::from_value::<NativeTaskListRequest>(
            json!({"sessionId":"s","intent":"speech_synthesis","path":"file"})
        )
        .is_err());
        assert_ne!(
            serde_json::to_value(NativeTaskIntentDto::TextToVideoWithAudio).unwrap(),
            serde_json::to_value(NativeTaskIntentDto::FirstFrameToVideoWithAudio).unwrap()
        );
    }
}
