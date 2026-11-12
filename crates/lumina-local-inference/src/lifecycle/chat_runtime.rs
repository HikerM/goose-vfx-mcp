use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{header, redirect, Client, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use super::native_runtime::{NativeHelperRuntime, NativeRequestGate};
use super::{
    LaunchChatTemplate, LaunchConfig, ModelCapability, ResourceWorkload, RuntimeExecutionMode,
    RuntimeParameters, Sha256Digest,
};

const TEXT_LIMIT: usize = 64 * 1024;
const TOTAL_TEXT_LIMIT: usize = 512 * 1024;
const REQUEST_LIMIT: usize = 16 * 1024 * 1024;
const RESPONSE_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeChatRequest {
    pub messages: Vec<NativeChatMessage>,
    #[serde(default)]
    pub tools: Vec<NativeChatTool>,
    pub max_output_tokens: u32,
    #[serde(default)]
    pub temperature: f64,
    #[serde(default = "default_seed")]
    pub seed: u32,
}

fn default_seed() -> u32 {
    42
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeChatMessage {
    System {
        content: String,
    },
    User {
        content: Vec<NativeChatContent>,
    },
    Assistant {
        content: Option<String>,
        #[serde(default)]
        tool_calls: Vec<NativeToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeChatContent {
    Text { text: String },
    ImagePng { bytes: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeChatTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeToolCall {
    pub id: String,
    pub function: NativeFunctionCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFunctionCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeChatFinishReason {
    Stop,
    Length,
    ToolCalls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeChatUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeChatResponse {
    pub id: String,
    pub content: Option<String>,
    pub tool_calls: Vec<NativeToolCall>,
    pub finish_reason: NativeChatFinishReason,
    pub usage: NativeChatUsage,
    pub launch_digest: Sha256Digest,
    pub request_digest: Sha256Digest,
    pub image_digests: Vec<Sha256Digest>,
}

impl NativeChatRequest {
    pub fn text(prompt: impl Into<String>, max_output_tokens: u32) -> Self {
        Self {
            messages: vec![NativeChatMessage::User {
                content: vec![NativeChatContent::Text {
                    text: prompt.into(),
                }],
            }],
            tools: vec![],
            max_output_tokens,
            temperature: 0.0,
            seed: default_seed(),
        }
    }

    pub fn add_png(&mut self, bytes: Vec<u8>) -> Result<()> {
        let Some(NativeChatMessage::User { content }) = self.messages.last_mut() else {
            anyhow::bail!("PNG input requires a final user message");
        };
        content.push(NativeChatContent::ImagePng { bytes });
        Ok(())
    }

    /// Local validation only; this never creates a helper or sends a request.
    pub fn validate(&self, launch: &LaunchConfig) -> Result<()> {
        self.freeze(launch, "validation-only").map(|_| ())
    }

    fn freeze(&self, launch: &LaunchConfig, model_alias: &str) -> Result<FrozenRequest> {
        let workload = chat_workload(launch)?;
        ensure!(
            !self.messages.is_empty() && self.messages.len() <= 128,
            "Chat requires 1 to 128 messages"
        );
        ensure!(
            self.max_output_tokens > 0 && self.max_output_tokens < workload.context_tokens,
            "Output budget must leave room for a nonempty input prompt"
        );
        ensure!(
            self.temperature.is_finite() && (0.0..=2.0).contains(&self.temperature),
            "Invalid sampling temperature"
        );
        ensure!(self.tools.len() <= 32, "Chat tool count exceeds 32");
        for message in &self.messages {
            if let NativeChatMessage::User { content } = message {
                ensure!(
                    !content.is_empty() && content.len() <= 32,
                    "User message has an invalid number of content parts"
                );
            }
        }
        let mut text_bytes = 0usize;
        let mut tool_names = BTreeSet::new();
        let mut tools = Vec::new();
        for tool in &self.tools {
            ensure!(
                valid_name(&tool.name, 64) && tool_names.insert(tool.name.clone()),
                "Invalid or duplicate tool name"
            );
            count_text(&tool.description, &mut text_bytes, true)?;
            ensure!(
                tool.parameters.get("type").and_then(Value::as_str) == Some("object"),
                "Tool schema must describe an object"
            );
            count_json(&tool.parameters, &mut text_bytes)?;
            tools.push(json!({"type":"function","function":{"name":tool.name,"description":tool.description,"parameters":tool.parameters}}));
        }
        let images: Vec<&[u8]> = self
            .messages
            .iter()
            .flat_map(|message| match message {
                NativeChatMessage::User { content } => content
                    .iter()
                    .filter_map(|item| match item {
                        NativeChatContent::ImagePng { bytes } => Some(bytes.as_slice()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => vec![],
            })
            .collect();
        let frozen_images = if images.is_empty() {
            vec![]
        } else {
            ensure!(
                launch.capabilities.contains(&ModelCapability::Vision),
                "Launch does not admit vision requests"
            );
            workload
                .vision
                .as_ref()
                .context("Launch has no frozen vision input budget")?
                .inspect_images(&images)?
        };
        let mut image_index = 0usize;
        let mut pending = BTreeSet::new();
        let mut historical_ids = BTreeSet::new();
        let mut messages = Vec::new();
        let mut tools_used = !self.tools.is_empty();
        let mut conversation_started = false;
        for message in &self.messages {
            if !matches!(message, NativeChatMessage::Tool { .. }) {
                ensure!(
                    pending.is_empty(),
                    "Every assistant tool call needs a result before the conversation continues"
                );
            }
            let encoded = match message {
                NativeChatMessage::System { content } => {
                    ensure!(
                        !conversation_started,
                        "System messages must precede the conversation"
                    );
                    count_text(content, &mut text_bytes, false)?;
                    json!({"role":"system","content":content})
                }
                NativeChatMessage::User { content } => {
                    conversation_started = true;
                    ensure!(
                        !content.is_empty() && content.len() <= 32,
                        "User message has an invalid number of content parts"
                    );
                    let mut parts = Vec::new();
                    for item in content {
                        match item {
                            NativeChatContent::Text { text } => {
                                count_text(text, &mut text_bytes, false)?;
                                parts.push(json!({"type":"text","text":text}));
                            }
                            NativeChatContent::ImagePng { .. } => {
                                let image = &frozen_images[image_index];
                                image_index += 1;
                                parts.push(json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{}", STANDARD.encode(image.bytes()))}}));
                            }
                        }
                    }
                    json!({"role":"user","content":parts})
                }
                NativeChatMessage::Assistant {
                    content,
                    tool_calls,
                } => {
                    ensure!(
                        conversation_started && tool_calls.len() <= 16,
                        "Assistant history is missing a preceding user or exceeds the tool limit"
                    );
                    if let Some(content) = content {
                        count_text(content, &mut text_bytes, true)?;
                    }
                    ensure!(
                        content.as_ref().is_some_and(|text| !text.trim().is_empty())
                            || !tool_calls.is_empty(),
                        "Assistant history is empty"
                    );
                    let mut encoded_calls = Vec::new();
                    for call in tool_calls {
                        validate_call(call, &tool_names, &mut historical_ids, &mut text_bytes)?;
                        pending.insert(call.id.clone());
                        encoded_calls.push(json!({"id":call.id,"type":"function","function":{"name":call.function.name,"arguments":serde_json::to_string(&call.function.arguments)?}}));
                    }
                    tools_used |= !tool_calls.is_empty();
                    let mut encoded = json!({"role":"assistant","content":content});
                    if !encoded_calls.is_empty() {
                        encoded["tool_calls"] = Value::Array(encoded_calls);
                    }
                    encoded
                }
                NativeChatMessage::Tool {
                    tool_call_id,
                    content,
                } => {
                    tools_used = true;
                    ensure!(
                        pending.remove(tool_call_id),
                        "Tool result has no unique pending assistant call"
                    );
                    count_text(content, &mut text_bytes, false)?;
                    json!({"role":"tool","tool_call_id":tool_call_id,"content":content})
                }
            };
            messages.push(encoded);
        }
        ensure!(
            pending.is_empty(),
            "Conversation ends with unresolved tool calls"
        );
        ensure!(
            matches!(
                self.messages.last(),
                Some(NativeChatMessage::User { .. } | NativeChatMessage::Tool { .. })
            ),
            "Generation requires a final user message or complete tool results"
        );
        ensure!(
            !tools_used || launch.capabilities.contains(&ModelCapability::Tools),
            "Launch does not admit tool calls"
        );
        let mut body = json!({"model":model_alias,"messages":messages,"max_tokens":self.max_output_tokens,
            "temperature":self.temperature,"seed":self.seed,"stream":false,"n":1,"cache_prompt":false,
            "chat_template_kwargs":{"enable_thinking":false}});
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
            body["tool_choice"] = json!("auto");
            body["parallel_tool_calls"] = json!(false);
        }
        let bytes = serde_json::to_vec(&body)?;
        ensure!(
            bytes.len() <= REQUEST_LIMIT,
            "Serialized chat request exceeds its byte budget"
        );
        Ok(FrozenRequest {
            digest: Sha256Digest::of_bytes(&bytes),
            bytes,
            image_digests: frozen_images
                .iter()
                .map(|image| image.sha256().clone())
                .collect(),
            tool_names,
            historical_ids,
            max_output_tokens: self.max_output_tokens,
        })
    }
}

fn chat_workload(launch: &LaunchConfig) -> Result<&ResourceWorkload> {
    ensure!(
        launch.capabilities.contains(&ModelCapability::Text),
        "Launch does not admit text chat"
    );
    let RuntimeParameters::LlamaCpp {
        workload: Some(workload),
        context_length,
        chat_template,
        ..
    } = &launch.parameters
    else {
        anyhow::bail!("Chat requires a complete frozen llama.cpp workload");
    };
    ensure!(
        *context_length == workload.context_tokens
            && workload.context_tokens > 0
            && !matches!(chat_template, LaunchChatTemplate::NotApplicable),
        "Chat context or template differs from its workload"
    );
    Ok(workload)
}

fn valid_name(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn count_text(value: &str, total: &mut usize, allow_empty: bool) -> Result<()> {
    ensure!(
        (allow_empty || !value.trim().is_empty()) && value.len() <= TEXT_LIMIT,
        "Chat text is empty or exceeds 64 KiB"
    );
    *total += value.len();
    ensure!(
        *total <= TOTAL_TEXT_LIMIT,
        "Conversation text and tool JSON exceed 512 KiB"
    );
    Ok(())
}

fn count_json(value: &Value, total: &mut usize) -> Result<()> {
    ensure!(
        value.is_object(),
        "Tool schema and arguments must be JSON objects"
    );
    let mut pending = vec![(value, 0usize)];
    let mut nodes = 0usize;
    let mut string_bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        ensure!(
            depth <= 16 && nodes <= 4096,
            "Tool JSON nesting or node budget exceeded"
        );
        match value {
            Value::Array(values) => {
                ensure!(
                    values.len() + pending.len() + nodes <= 4096,
                    "Tool JSON node budget exceeded"
                );
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                ensure!(
                    values.len() + pending.len() + nodes <= 4096,
                    "Tool JSON node budget exceeded"
                );
                string_bytes += values.keys().map(String::len).sum::<usize>();
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            Value::String(value) => string_bytes += value.len(),
            _ => {}
        }
        ensure!(
            string_bytes <= TEXT_LIMIT,
            "Tool JSON strings exceed 64 KiB"
        );
    }
    count_text(&serde_json::to_string(value)?, total, true)
}

fn validate_call(
    call: &NativeToolCall,
    tools: &BTreeSet<String>,
    ids: &mut BTreeSet<String>,
    total: &mut usize,
) -> Result<()> {
    ensure!(
        valid_name(&call.id, 128) && ids.insert(call.id.clone()),
        "Tool call ID is invalid or duplicated"
    );
    ensure!(
        tools.contains(&call.function.name),
        "Tool call names an undefined function"
    );
    count_json(&call.function.arguments, total)
}

pub struct NativeChatAdapter<'a> {
    _owner: &'a NativeHelperRuntime,
    connection: ChatConnection,
}

impl<'a> NativeChatAdapter<'a> {
    pub fn new(owner: &'a NativeHelperRuntime) -> Result<Self> {
        ensure!(
            owner.child_id().is_some(),
            "Native helper has already stopped"
        );
        Ok(Self {
            _owner: owner,
            connection: ChatConnection::new(
                owner.actual_config(),
                owner.endpoint(),
                owner.bearer_token(),
                owner.model_alias(),
                owner.request_gate(),
            )?,
        })
    }

    /// Dropping this future cancels transport. It does not prove the native
    /// computation has stopped; the borrowed owner retains process and assets.
    pub async fn complete(
        &self,
        request: &NativeChatRequest,
        timeout: Duration,
    ) -> Result<NativeChatResponse> {
        self.connection.complete(request, timeout).await
    }
}

struct ChatConnection {
    config: LaunchConfig,
    endpoint: Url,
    model_alias: String,
    client: Client,
    request_gate: Arc<NativeRequestGate>,
}

struct FrozenRequest {
    bytes: Vec<u8>,
    digest: Sha256Digest,
    image_digests: Vec<Sha256Digest>,
    tool_names: BTreeSet<String>,
    historical_ids: BTreeSet<String>,
    max_output_tokens: u32,
}

impl ChatConnection {
    fn new(
        config: &LaunchConfig,
        endpoint: &Url,
        token: &str,
        model_alias: &str,
        request_gate: Arc<NativeRequestGate>,
    ) -> Result<Self> {
        chat_workload(config)?;
        let RuntimeExecutionMode::Helper { port } = config.mode else {
            anyhow::bail!("Chat requires a managed helper");
        };
        let ip: std::net::IpAddr = endpoint
            .host_str()
            .context("Helper endpoint has no host")?
            .trim_matches(['[', ']'])
            .parse()?;
        ensure!(
            endpoint.scheme() == "http"
                && ip.is_loopback()
                && port > 0
                && endpoint.port_or_known_default() == Some(port)
                && endpoint.path() == "/"
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "Chat endpoint differs from its loopback binding"
        );
        ensure!(
            !token.trim().is_empty() && !model_alias.trim().is_empty(),
            "Helper instance identity is incomplete"
        );
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {token}"))?;
        authorization.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json"),
        );
        let client = Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .default_headers(headers)
            .build()?;
        Ok(Self {
            config: config.clone(),
            endpoint: endpoint.clone(),
            model_alias: model_alias.into(),
            client,
            request_gate,
        })
    }

    async fn complete(
        &self,
        request: &NativeChatRequest,
        timeout: Duration,
    ) -> Result<NativeChatResponse> {
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(180),
            "Chat operation deadline must be within 180 seconds"
        );
        tokio::time::timeout(timeout, async {
            let mut operation = self.request_gate.acquire().await?;
            let frozen = request.freeze(&self.config, &self.model_alias)?;
            operation.sending();
            let preflight: InputTokens = self
                .post("v1/chat/completions/input_tokens", &frozen.bytes, timeout)
                .await?;
            operation.finished();
            ensure!(
                preflight.object == "response.input_tokens" && preflight.input_tokens > 0,
                "Invalid native input token preflight response"
            );
            let workload = chat_workload(&self.config)?;
            let total = preflight
                .input_tokens
                .checked_add(u64::from(frozen.max_output_tokens))
                .context("Chat token budget overflow")?;
            ensure!(
                total <= u64::from(workload.context_tokens),
                "Complete chat prompt and output exceed the frozen context"
            );
            if !frozen.image_digests.is_empty() {
                workload
                    .validate_vision_prompt(preflight.input_tokens, frozen.max_output_tokens)?;
            }
            operation.sending();
            let response: Completion = self
                .post("v1/chat/completions", &frozen.bytes, timeout)
                .await?;
            operation.finished();
            self.decode(response, preflight.input_tokens, frozen)
        })
        .await?
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        bytes: &[u8],
        timeout: Duration,
    ) -> Result<T> {
        let mut response = self
            .client
            .post(self.endpoint.join(path)?)
            .body(bytes.to_vec())
            .timeout(timeout)
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "Native chat helper returned HTTP {}",
            response.status().as_u16()
        );
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        ensure!(
            content_type
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json")),
            "Native chat helper returned non-JSON content"
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= RESPONSE_LIMIT as u64),
            "Native chat response exceeds 1 MiB"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                chunk.len() <= RESPONSE_LIMIT.saturating_sub(body.len()),
                "Native chat response exceeds 1 MiB"
            );
            body.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&body)?)
    }

    fn decode(
        &self,
        response: Completion,
        input_tokens: u64,
        frozen: FrozenRequest,
    ) -> Result<NativeChatResponse> {
        ensure!(
            response.object == "chat.completion"
                && response.model == self.model_alias
                && !response.id.is_empty()
                && response.id.len() <= 256
                && response.choices.len() == 1,
            "Native chat response has a different model or incomplete completion envelope"
        );
        let usage = &response.usage;
        ensure!(
            usage.prompt_tokens == input_tokens
                && usage.completion_tokens > 0
                && usage.completion_tokens <= u64::from(frozen.max_output_tokens)
                && usage.prompt_tokens.checked_add(usage.completion_tokens)
                    == Some(usage.total_tokens),
            "Native chat usage differs from preflight or output budget"
        );
        let choice = response.choices.into_iter().next().unwrap();
        ensure!(
            choice.index == 0 && choice.message.role == "assistant",
            "Native chat response has an invalid assistant choice"
        );
        let mut text_bytes = 0;
        if let Some(content) = &choice.message.content {
            count_text(content, &mut text_bytes, true)?;
        }
        ensure!(
            choice.message.tool_calls.len() <= 16,
            "Native chat response exceeds the tool-call limit"
        );
        let mut tool_calls = Vec::new();
        let mut ids = frozen.historical_ids;
        for raw in choice.message.tool_calls {
            ensure!(
                raw.kind == "function" && raw.function.arguments.len() <= TEXT_LIMIT,
                "Native tool call has invalid type or oversized arguments"
            );
            let call = NativeToolCall {
                id: raw.id,
                function: NativeFunctionCall {
                    name: raw.function.name,
                    arguments: serde_json::from_str(&raw.function.arguments)?,
                },
            };
            validate_call(&call, &frozen.tool_names, &mut ids, &mut text_bytes)?;
            tool_calls.push(call);
        }
        match choice.finish_reason {
            NativeChatFinishReason::ToolCalls => {
                ensure!(!tool_calls.is_empty(), "Tool completion has no tool calls")
            }
            NativeChatFinishReason::Stop | NativeChatFinishReason::Length => ensure!(
                tool_calls.is_empty()
                    && choice
                        .message
                        .content
                        .as_ref()
                        .is_some_and(|content| !content.trim().is_empty()),
                "Text completion is empty or has incomplete tool calls"
            ),
        }
        Ok(NativeChatResponse {
            id: response.id,
            content: choice.message.content,
            tool_calls,
            finish_reason: choice.finish_reason,
            usage: response.usage,
            launch_digest: self.config.digest()?,
            request_digest: frozen.digest,
            image_digests: frozen.image_digests,
        })
    }
}

#[derive(Deserialize)]
struct InputTokens {
    object: String,
    input_tokens: u64,
}

#[derive(Deserialize)]
struct Completion {
    id: String,
    object: String,
    model: String,
    choices: Vec<Choice>,
    usage: NativeChatUsage,
}

#[derive(Deserialize)]
struct Choice {
    index: u32,
    message: AssistantMessage,
    finish_reason: NativeChatFinishReason,
}

#[derive(Deserialize)]
struct AssistantMessage {
    role: String,
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<RawToolCall>,
}

#[derive(Deserialize)]
struct RawToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: RawFunctionCall,
}

#[derive(Deserialize)]
struct RawFunctionCall {
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{baseline_pairing_runtime, ResourceCacheType, VisionResourceWorkload};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 96, 96, 96, 0, 0, 0,
        4, 0, 1, 246, 23, 56, 85, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    fn config(port: u16, vision: bool, tools: bool) -> LaunchConfig {
        let mut capabilities = BTreeSet::from([ModelCapability::Text]);
        if vision {
            capabilities.insert(ModelCapability::Vision);
        }
        if tools {
            capabilities.insert(ModelCapability::Tools);
        }
        LaunchConfig {
            installation_id: "unit-chat".into(),
            version_id: "v1".into(),
            manifest_digest: Sha256Digest::of_bytes(b"unit-only synthetic manifest"),
            runtime: baseline_pairing_runtime().unwrap(),
            capabilities,
            component_files: BTreeMap::new(),
            parameters: RuntimeParameters::LlamaCpp {
                chat_template: LaunchChatTemplate::Embedded,
                context_length: 4096,
                gpu_layers: 0,
                cpu_threads: 4,
                kv_key_type: "f16".into(),
                kv_value_type: "f16".into(),
                batch_size: 512,
                flash_attention: true,
                workload: Some(ResourceWorkload {
                    context_tokens: 4096,
                    batch_tokens: 512,
                    parallel_sequences: 1,
                    recurrent_rollback_slots: 0,
                    kv_key_type: ResourceCacheType::F16,
                    kv_value_type: ResourceCacheType::F16,
                    prompt_cache_bytes: 0,
                    context_checkpoint_slots: 0,
                    vision: vision.then(VisionResourceWorkload::measured_qwen35_cpu),
                    vector: None,
                }),
            },
            mode: RuntimeExecutionMode::Helper { port },
        }
    }

    struct Reply {
        status: u16,
        body: Value,
        delay: Duration,
    }
    impl Reply {
        fn json(body: Value) -> Self {
            Self {
                status: 200,
                body,
                delay: Duration::ZERO,
            }
        }
    }

    type CapturedRequests = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    struct Mock {
        connection: ChatConnection,
        requests: CapturedRequests,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl Mock {
        fn new(
            vision: bool,
            tools: bool,
            handler: impl Fn(&str, &Value) -> Reply + Send + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let endpoint = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
            // This private test transport cannot construct a public managed adapter.
            let connection = ChatConnection::new(
                &config(port, vision, tools),
                &endpoint,
                "unit-secret",
                "unit-model",
                Arc::default(),
            )
            .unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => panic!("mock accept failed: {error}"),
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let header_end = loop {
                        let mut chunk = [0; 8192];
                        let count = stream.read(&mut chunk).unwrap();
                        assert!(count > 0);
                        bytes.extend_from_slice(&chunk[..count]);
                        if let Some(offset) =
                            bytes.windows(4).position(|value| value == b"\r\n\r\n")
                        {
                            break offset + 4;
                        }
                    };
                    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                    assert!(headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer unit-secret"));
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    while bytes.len() < header_end + length {
                        let mut chunk = [0; 8192];
                        let count = stream.read(&mut chunk).unwrap();
                        assert!(count > 0);
                        bytes.extend_from_slice(&chunk[..count]);
                    }
                    let path = headers
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .to_string();
                    let body = bytes[header_end..header_end + length].to_vec();
                    captured.lock().unwrap().push((path.clone(), body.clone()));
                    let reply = handler(&path, &serde_json::from_slice(&body).unwrap());
                    thread::sleep(reply.delay);
                    let body = reply.body.to_string();
                    let _ = write!(stream, "HTTP/1.1 {} Unit\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.status, body.len(), body);
                }
            });
            Self {
                connection,
                requests,
                stop,
                worker: Some(worker),
            }
        }
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            let joined = self.worker.take().unwrap().join();
            if !thread::panicking() {
                joined.unwrap();
            }
        }
    }

    fn response() -> Value {
        json!({"id":"completion-unit","object":"chat.completion","model":"unit-model",
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"4"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":2,"total_tokens":102}})
    }

    fn ordinary(path: &str, _: &Value) -> Reply {
        Reply::json(if path.ends_with("input_tokens") {
            json!({"object":"response.input_tokens","input_tokens":100})
        } else {
            response()
        })
    }

    fn tools_request() -> NativeChatRequest {
        let mut request = NativeChatRequest::text("Look up the weather", 64);
        request.tools.push(NativeChatTool { name: "weather".into(), description: "Return weather".into(),
            parameters: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}) });
        request.messages.push(NativeChatMessage::Assistant {
            content: None,
            tool_calls: vec![NativeToolCall {
                id: "call_previous".into(),
                function: NativeFunctionCall {
                    name: "weather".into(),
                    arguments: json!({"city":"Paris"}),
                },
            }],
        });
        request.messages.push(NativeChatMessage::Tool {
            tool_call_id: "call_previous".into(),
            content: "Sunny".into(),
        });
        request
    }

    #[tokio::test]
    async fn adapters_for_one_owner_share_capacity_before_image_preflight() {
        let mock = Mock::new(false, false, ordinary);
        let second = ChatConnection::new(
            &mock.connection.config,
            &mock.connection.endpoint,
            "unit-secret",
            "unit-model",
            Arc::clone(&mock.connection.request_gate),
        )
        .unwrap();
        let held = mock.connection.request_gate.acquire().await.unwrap();
        let request = NativeChatRequest::text("2+2", 64);
        assert!(second
            .complete(&request, Duration::from_millis(30))
            .await
            .is_err());
        assert!(mock.requests.lock().unwrap().is_empty());
        drop(held);
        let (first, second) = tokio::join!(
            mock.connection.complete(&request, Duration::from_secs(2)),
            second.complete(&request, Duration::from_secs(2))
        );
        first.unwrap();
        second.unwrap();
        let paths: Vec<_> = mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|(path, _)| path.clone())
            .collect();
        assert_eq!(
            paths,
            [
                "/v1/chat/completions/input_tokens",
                "/v1/chat/completions",
                "/v1/chat/completions/input_tokens",
                "/v1/chat/completions"
            ]
        );
    }

    #[tokio::test]
    async fn frozen_body_is_identical_for_native_preflight_and_generation() {
        let mock = Mock::new(false, false, ordinary);
        let result = mock
            .connection
            .complete(&NativeChatRequest::text("2+2", 64), Duration::from_secs(2))
            .await
            .unwrap();
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].0, "/v1/chat/completions/input_tokens");
        assert_eq!(requests[1].0, "/v1/chat/completions");
        assert_eq!(requests[0].1, requests[1].1);
        assert_eq!(
            result.request_digest,
            Sha256Digest::of_bytes(&requests[0].1)
        );
        assert_eq!(
            result.launch_digest,
            mock.connection.config.digest().unwrap()
        );
        let body: Value = serde_json::from_slice(&requests[0].1).unwrap();
        assert_eq!(body["cache_prompt"], false);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(body["stream"], false);
        assert_eq!(result.content.as_deref(), Some("4"));
    }

    #[tokio::test]
    async fn excessive_zero_overflow_or_wrong_preflight_never_generates() {
        for body in [
            json!({"object":"response.input_tokens","input_tokens":4090}),
            json!({"object":"response.input_tokens","input_tokens":0}),
            json!({"object":"response.input_tokens","input_tokens":u64::MAX}),
            json!({"object":"chat.completion","input_tokens":100}),
        ] {
            let mock = Mock::new(false, false, move |path, _| {
                assert!(path.ends_with("input_tokens"));
                Reply::json(body.clone())
            });
            assert!(mock
                .connection
                .complete(&NativeChatRequest::text("2+2", 64), Duration::from_secs(2))
                .await
                .is_err());
            assert_eq!(mock.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn actual_png_bytes_are_frozen_and_hashed_in_the_only_request_body() {
        let mock = Mock::new(true, false, ordinary);
        let mut request = NativeChatRequest::text("Describe this", 64);
        request.add_png(PNG.to_vec()).unwrap();
        let result = mock
            .connection
            .complete(&request, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(result.image_digests, vec![Sha256Digest::of_bytes(PNG)]);
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests[0].1, requests[1].1);
        let body: Value = serde_json::from_slice(&requests[0].1).unwrap();
        let url = body["messages"][0]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap();
        assert_eq!(
            STANDARD
                .decode(url.strip_prefix("data:image/png;base64,").unwrap())
                .unwrap(),
            PNG
        );
    }

    #[tokio::test]
    async fn historical_multi_image_non_png_and_unadmitted_vision_never_reach_http() {
        for case in 0..3 {
            let mock = Mock::new(case != 2, false, |_, _| panic!("invalid request sent HTTP"));
            let mut request = NativeChatRequest::text("first", 64);
            request
                .add_png(if case == 1 {
                    b"not a PNG".to_vec()
                } else {
                    PNG.to_vec()
                })
                .unwrap();
            if case == 0 {
                request.messages.push(NativeChatMessage::Assistant {
                    content: Some("first answer".into()),
                    tool_calls: vec![],
                });
                request.messages.push(NativeChatMessage::User {
                    content: vec![NativeChatContent::ImagePng {
                        bytes: PNG.to_vec(),
                    }],
                });
            }
            assert!(mock
                .connection
                .complete(&request, Duration::from_secs(2))
                .await
                .is_err());
            assert!(mock.requests.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn tool_history_uses_function_json_and_resolved_call_ids() {
        let mock = Mock::new(false, true, |path, body| {
            assert_eq!(
                body["messages"][1]["tool_calls"][0]["function"]["arguments"],
                "{\"city\":\"Paris\"}"
            );
            assert_eq!(body["messages"][2]["tool_call_id"], "call_previous");
            if path.ends_with("input_tokens") {
                return ordinary(path, body);
            }
            let mut result = response();
            result["choices"][0]["finish_reason"] = json!("tool_calls");
            result["choices"][0]["message"] = json!({"role":"assistant","content":null,
                "tool_calls":[{"id":"call_next","type":"function","function":{"name":"weather","arguments":"{\"city\":\"London\"}"}}]});
            Reply::json(result)
        });
        let result = mock
            .connection
            .complete(&tools_request(), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(result.finish_reason, NativeChatFinishReason::ToolCalls);
        assert_eq!(
            result.tool_calls[0].function.arguments,
            json!({"city":"London"})
        );
        assert_eq!(result.tool_calls[0].id, "call_next");
    }

    #[tokio::test]
    async fn invalid_tool_history_capability_json_and_text_budgets_send_no_http() {
        for case in 0..8 {
            let mock = Mock::new(false, case != 0, |_, _| panic!("invalid request sent HTTP"));
            let mut request = tools_request();
            match case {
                0 => {}
                1 => {
                    request.messages.pop();
                }
                2 => request.messages.push(NativeChatMessage::Tool {
                    tool_call_id: "call_previous".into(),
                    content: "duplicate".into(),
                }),
                3 => {
                    if let NativeChatMessage::Tool { tool_call_id, .. } =
                        request.messages.last_mut().unwrap()
                    {
                        *tool_call_id = "missing".into();
                    }
                }
                4 => {
                    request.tools[0].parameters =
                        json!({"type":"object","extra":"x".repeat(TEXT_LIMIT+1)})
                }
                5 => request.temperature = f64::NAN,
                6 => request.max_output_tokens = 0,
                7 => request.messages.push(NativeChatMessage::User {
                    content: vec![NativeChatContent::Text {
                        text: "x".repeat(TEXT_LIMIT + 1),
                    }],
                }),
                _ => unreachable!(),
            }
            assert!(
                mock.connection
                    .complete(&request, Duration::from_secs(2))
                    .await
                    .is_err(),
                "case {case}"
            );
            assert!(mock.requests.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn completion_model_usage_choice_and_tool_integrity_are_checked() {
        for case in 0..11 {
            let mock = Mock::new(false, true, move |path, body| {
                if path.ends_with("input_tokens") {
                    return ordinary(path, body);
                }
                let mut result = response();
                match case {
                    0 => result["model"] = json!("wrong"),
                    1 => result["usage"]["prompt_tokens"] = json!(99),
                    2 => result["usage"]["completion_tokens"] = json!(0),
                    3 => result["usage"]["total_tokens"] = json!(101),
                    4 => result["choices"] = json!([]),
                    5 => result["choices"][0]["index"] = json!(1),
                    6 => result["choices"][0]["message"]["role"] = json!("user"),
                    7 => result["choices"][0]["finish_reason"] = json!("tool_calls"),
                    8 => result["choices"][0]["message"]["content"] = Value::Null,
                    9 => {
                        result["choices"][0]["finish_reason"] = json!("tool_calls");
                        result["choices"][0]["message"]["tool_calls"] = json!([{"id":"call_previous","type":"function","function":{"name":"weather","arguments":"{}"}}]);
                    }
                    10 => {
                        result["choices"][0]["finish_reason"] = json!("tool_calls");
                        result["choices"][0]["message"]["tool_calls"] = json!([{"id":"new_call","type":"function","function":{"name":"weather","arguments":"{incomplete"}}]);
                    }
                    _ => unreachable!(),
                }
                Reply::json(result)
            });
            assert!(
                mock.connection
                    .complete(&tools_request(), Duration::from_secs(2))
                    .await
                    .is_err(),
                "case {case}"
            );
        }
    }

    #[tokio::test]
    async fn native_errors_and_oversized_responses_are_not_generations() {
        for oversized in [false, true] {
            let mock = Mock::new(false, false, move |_, _| {
                if oversized {
                    Reply::json(json!({"oversized":"x".repeat(RESPONSE_LIMIT)}))
                } else {
                    Reply {
                        status: 503,
                        body: json!({"error":"unavailable"}),
                        delay: Duration::ZERO,
                    }
                }
            });
            assert!(mock
                .connection
                .complete(
                    &NativeChatRequest::text("hello", 64),
                    Duration::from_secs(2)
                )
                .await
                .is_err());
            assert_eq!(mock.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn timeout_or_dropped_future_does_not_send_a_generation_after_preflight() {
        for external_cancel in [false, true] {
            let mock = Mock::new(false, false, |path, body| {
                let mut reply = ordinary(path, body);
                reply.delay = Duration::from_millis(350);
                reply
            });
            let request = NativeChatRequest::text("hello", 64);
            if external_cancel {
                assert!(tokio::time::timeout(
                    Duration::from_millis(150),
                    mock.connection.complete(&request, Duration::from_secs(2))
                )
                .await
                .is_err());
            } else {
                assert!(mock
                    .connection
                    .complete(&request, Duration::from_millis(150))
                    .await
                    .is_err());
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            assert_eq!(mock.requests.lock().unwrap().len(), 1);
            let retry = mock
                .connection
                .complete(&request, Duration::from_secs(2))
                .await
                .unwrap_err();
            assert!(retry.to_string().contains("stop and restart"));
            assert_eq!(mock.requests.lock().unwrap().len(), 1);
        }
    }
}
