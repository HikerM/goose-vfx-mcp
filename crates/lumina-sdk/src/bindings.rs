//! In-process uniffi bindings for the Lumina SDK.
//!
//! This is the API surface exposed to Python and Kotlin. It currently focuses
//! on declarative providers: consumers can construct a provider from JSON and
//! stream completions from it.

use std::{future::Future, sync::Arc, sync::OnceLock};

use futures::StreamExt;
use lumina_providers::{
    api_client::{ApiClient, AuthMethod},
    base::{MessageStream, Provider as LuminaProvider},
    conversation::message::Message,
    databricks::DatabricksProvider as LuminaDatabricksProvider,
    databricks_auth::DatabricksAuth,
    declarative::EnvKeyResolver,
    model::ModelConfig,
    openai::OpenAiProviderBuilder,
};

/// Errors surfaced across the uniffi boundary.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum LuminaError {
    #[error("{0}")]
    Generic(String),
}

impl From<anyhow::Error> for LuminaError {
    fn from(error: anyhow::Error) -> Self {
        Self::Generic(error.to_string())
    }
}

impl From<lumina_providers::errors::ProviderError> for LuminaError {
    fn from(error: lumina_providers::errors::ProviderError) -> Self {
        Self::Generic(error.to_string())
    }
}

impl From<serde_json::Error> for LuminaError {
    fn from(error: serde_json::Error) -> Self {
        Self::Generic(error.to_string())
    }
}

/// A text message passed to a provider.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ProviderMessage {
    pub role: MessageRole,
    pub text: String,
}

/// Supported message roles for provider requests and streamed responses.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum MessageRole {
    User,
    Assistant,
}

impl ProviderMessage {
    fn to_lumina_message(&self) -> Message {
        match self.role {
            MessageRole::User => Message::user().with_text(&self.text),
            MessageRole::Assistant => Message::assistant().with_text(&self.text),
        }
    }
}

/// Model selection and optional generation settings for a provider request.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ProviderModelConfig {
    pub model_name: String,
    #[uniffi(default = None)]
    pub context_limit: Option<u64>,
    #[uniffi(default = None)]
    pub temperature: Option<f32>,
    #[uniffi(default = None)]
    pub max_tokens: Option<i32>,
    #[uniffi(default = false)]
    pub toolshim: bool,
    #[uniffi(default = None)]
    pub toolshim_model: Option<String>,
    /// Provider-specific request parameters as a JSON object string.
    #[uniffi(default = None)]
    pub request_params_json: Option<String>,
    #[uniffi(default = None)]
    pub reasoning: Option<bool>,
}

impl ProviderModelConfig {
    fn to_lumina_model_config(&self) -> Result<ModelConfig, LuminaError> {
        let mut config = ModelConfig::new(&self.model_name)
            .with_context_limit(self.context_limit.map(|limit| limit as usize))
            .with_temperature(self.temperature)
            .with_max_tokens(self.max_tokens)
            .with_toolshim(self.toolshim)
            .with_toolshim_model(self.toolshim_model.clone());

        if let Some(request_params_json) = &self.request_params_json {
            let request_params = serde_json::from_str(request_params_json)?;
            config = config.with_merged_request_params(request_params);
        }

        config.reasoning = self.reasoning;
        Ok(config)
    }
}

/// One item yielded by a provider stream.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ProviderStreamChunk {
    /// The concatenated text content in this message chunk, if one was emitted.
    pub text: Option<String>,
    /// Full Lumina message JSON for callers that need non-text content such as tool requests.
    pub message_json: Option<String>,
    /// Provider usage JSON when the provider emits usage metadata.
    pub usage_json: Option<String>,
}

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn runtime() -> Result<&'static tokio::runtime::Runtime, LuminaError> {
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| LuminaError::Generic(error.to_string()))?;

    let _ = RUNTIME.set(runtime);
    Ok(RUNTIME.get().expect("runtime was initialized"))
}

async fn run_on_runtime<T>(
    future: impl Future<Output = T> + Send + 'static,
) -> Result<T, LuminaError>
where
    T: Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    runtime()?.spawn(async move {
        let _ = sender.send(future.await);
    });

    receiver
        .await
        .map_err(|_| LuminaError::Generic("runtime task was cancelled".to_string()))
}

struct ProviderHandle {
    provider: Arc<dyn LuminaProvider>,
}

impl ProviderHandle {
    fn new(provider: Box<dyn LuminaProvider>) -> Self {
        Self {
            provider: Arc::from(provider),
        }
    }

    fn name(&self) -> String {
        self.provider.get_name().to_string()
    }

    async fn stream(
        &self,
        model: ProviderModelConfig,
        system: String,
        messages: Vec<ProviderMessage>,
    ) -> Result<Arc<ProviderStream>, LuminaError> {
        let model = model.to_lumina_model_config()?;
        let messages = messages
            .iter()
            .map(ProviderMessage::to_lumina_message)
            .collect::<Vec<_>>();
        let provider = Arc::clone(&self.provider);
        let stream =
            run_on_runtime(async move { provider.stream(&model, &system, &messages, &[]).await })
                .await??;

        Ok(Arc::new(ProviderStream {
            stream: Arc::new(tokio::sync::Mutex::new(stream)),
        }))
    }
}

/// A Lumina provider backed by one of Lumina's native provider implementations.
#[derive(uniffi::Object)]
pub struct Provider {
    handle: ProviderHandle,
}

impl Provider {
    fn new(provider: Box<dyn LuminaProvider>) -> Arc<Self> {
        Arc::new(Self {
            handle: ProviderHandle::new(provider),
        })
    }
}

#[uniffi::export]
impl Provider {
    pub fn name(&self) -> String {
        self.handle.name()
    }

    /// Start a streaming completion request. Tools are not yet exposed over the
    /// uniffi boundary, so this calls providers with an empty tool list.
    pub async fn stream(
        &self,
        model: ProviderModelConfig,
        system: String,
        messages: Vec<ProviderMessage>,
    ) -> Result<Arc<ProviderStream>, LuminaError> {
        self.handle.stream(model, system, messages).await
    }
}

#[uniffi::export]
pub fn declarative_provider_from_json(json: String) -> Result<Arc<Provider>, LuminaError> {
    let provider = lumina_providers::declarative::from_json(&json, None, EnvKeyResolver {})?;
    Ok(Provider::new(provider))
}

#[uniffi::export]
pub fn openai_default_model() -> String {
    lumina_providers::openai::OPEN_AI_DEFAULT_MODEL.to_string()
}

#[uniffi::export]
pub fn openai_provider(api_key: String) -> Result<Arc<Provider>, LuminaError> {
    let api_client = ApiClient::new_with_tls(
        "https://api.openai.com".to_string(),
        AuthMethod::BearerToken(api_key),
        None,
    )?;
    let provider = OpenAiProviderBuilder::new(api_client).build();
    Ok(Provider::new(Box::new(provider)))
}

#[uniffi::export]
pub fn databricks_default_model() -> String {
    lumina_providers::databricks::DATABRICKS_DEFAULT_MODEL.to_string()
}

#[uniffi::export]
pub fn databricks_provider(host: String, token: String) -> Result<Arc<Provider>, LuminaError> {
    let retry_config = LuminaDatabricksProvider::load_retry_config(|key| std::env::var(key).ok());
    let provider = LuminaDatabricksProvider::new(
        host,
        DatabricksAuth::token(token),
        retry_config,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )?;

    Ok(Provider::new(Box::new(provider)))
}

/// An async iterator over provider stream chunks.
#[derive(uniffi::Object)]
pub struct ProviderStream {
    stream: Arc<tokio::sync::Mutex<MessageStream>>,
}

#[uniffi::export]
impl ProviderStream {
    /// Return the next stream chunk, or `None` when the stream is exhausted.
    pub async fn next(&self) -> Result<Option<ProviderStreamChunk>, LuminaError> {
        let stream = Arc::clone(&self.stream);
        run_on_runtime(async move {
            let mut stream = stream.lock().await;
            let Some((message, usage)) = stream.next().await.transpose()? else {
                return Ok(None);
            };

            let text = message.as_ref().map(Message::as_concat_text);
            let message_json = message.as_ref().map(serde_json::to_string).transpose()?;
            let usage_json = usage.as_ref().map(serde_json::to_string).transpose()?;

            Ok(Some(ProviderStreamChunk {
                text,
                message_json,
                usage_json,
            }))
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_config_rejects_invalid_request_params_json() {
        let config = ProviderModelConfig {
            model_name: "test".to_string(),
            context_limit: None,
            temperature: None,
            max_tokens: None,
            toolshim: false,
            toolshim_model: None,
            request_params_json: Some("not json".to_string()),
            reasoning: None,
        };

        assert!(config.to_lumina_model_config().is_err());
    }

    #[test]
    fn provider_message_converts_user_text() {
        let message = ProviderMessage {
            role: MessageRole::User,
            text: "what is the capital of France?".to_string(),
        }
        .to_lumina_message();

        assert_eq!(message.as_concat_text(), "what is the capital of France?");
    }
}
