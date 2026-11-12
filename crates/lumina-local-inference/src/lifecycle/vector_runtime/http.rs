use anyhow::{ensure, Context, Result};
use reqwest::{header, redirect, Client, Url};
use serde::{de::DeserializeOwned, Deserialize};
use std::net::IpAddr;
use std::time::Duration;

use super::{VectorRuntimeProfile, VectorTokenUsage};
use crate::lifecycle::{
    LaunchConfig, ModelCapability, ModelManifest, RuntimeExecutionMode, Sha256Digest,
};

const RESPONSE_LIMIT: usize = 1024 * 1024;

pub(super) struct VectorConnection {
    client: Client,
    endpoint: Url,
    profile: VectorRuntimeProfile,
    pub timeout: Duration,
    pub model_alias: String,
    pub launch_digest: Sha256Digest,
}

impl VectorConnection {
    pub fn new(
        launch: &LaunchConfig,
        manifest: &ModelManifest,
        capability: ModelCapability,
        endpoint: &str,
        token: &str,
        timeout: Duration,
    ) -> Result<Self> {
        let profile = VectorRuntimeProfile::from_binding(launch, manifest)?;
        ensure!(
            profile.capability() == capability,
            "Adapter capability differs from the bound vector profile"
        );
        let endpoint = Url::parse(endpoint)?;
        let host = endpoint
            .host_str()
            .context("Vector helper endpoint has no host")?;
        let ip: IpAddr = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .context("Vector helper endpoint must use an explicit loopback IP address")?;
        let RuntimeExecutionMode::Helper { port } = launch.mode else {
            unreachable!()
        };
        ensure!(
            endpoint.scheme() == "http"
                && ip.is_loopback()
                && endpoint.port_or_known_default() == Some(port)
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.path() == "/"
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "Vector helper endpoint differs from the frozen local helper binding"
        );
        ensure!(
            !token.trim().is_empty(),
            "Vector helper requires a bearer credential"
        );
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(180),
            "Vector request deadline must be within 180 seconds"
        );
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {token}"))?;
        authorization.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json"),
        );
        let client = Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .timeout(timeout)
            .default_headers(headers)
            .build()?;
        let launch_digest = launch.digest()?;
        Ok(Self {
            client,
            endpoint,
            profile,
            timeout,
            model_alias: format!("lumina-vector-{}", launch_digest.as_str()),
            launch_digest,
        })
    }

    /// The runtime owner supplies -m from the bound installation and holds its
    /// asset guard. The bearer credential is passed via environment, never argv.
    pub fn helper_arguments(&self) -> Vec<String> {
        let mut args = self.profile.arguments();
        args.extend([
            "--host".into(),
            self.endpoint
                .host_str()
                .unwrap()
                .trim_matches(['[', ']'])
                .into(),
            "--port".into(),
            self.endpoint.port_or_known_default().unwrap().to_string(),
            "--alias".into(),
            self.model_alias.clone(),
        ]);
        args
    }

    pub fn validate_inputs(&self, query: Option<&str>, inputs: &[String]) -> Result<()> {
        ensure!(
            !inputs.is_empty() && inputs.len() <= 16,
            "Vector request requires 1 to 16 input items"
        );
        let mut bytes = 0usize;
        for value in query.into_iter().chain(inputs.iter().map(String::as_str)) {
            ensure!(
                !value.trim().is_empty() && value.len() <= 8192,
                "Vector input must be nonempty and at most 8192 UTF-8 bytes"
            );
            bytes += value.len();
        }
        ensure!(
            bytes <= 65536,
            "Vector request exceeds the 65536-byte input budget"
        );
        Ok(())
    }

    pub async fn token_count(
        &self,
        input: &str,
        add_special: bool,
        parse_special: bool,
    ) -> Result<u64> {
        let response: Tokenized = self
            .post(
                "tokenize",
                &serde_json::json!({
                    "content": input, "add_special": add_special, "parse_special": parse_special,
                    "with_pieces": false,
                }),
            )
            .await?;
        // The pinned helper rejects n_tokens >= n_ctx for causal LAST pooling;
        // BGE's memory-less RANK path accepts a complete 512-token pair.
        let max_tokens = match self.profile {
            VectorRuntimeProfile::Qwen3Embedding06bQ8 => 511,
            VectorRuntimeProfile::BgeRerankerV2M3Q4Km => 512,
        };
        ensure!(
            !response.tokens.is_empty()
                && response.tokens.len() <= max_tokens
                && response.tokens.iter().all(|token| *token >= 0),
            "Tokenized vector input exceeds capacity or contains invalid token IDs"
        );
        Ok(response.tokens.len() as u64)
    }

    pub fn validate_response(
        &self,
        model: &str,
        object: &str,
        usage: &VectorTokenUsage,
        expected_tokens: u64,
    ) -> Result<()> {
        ensure!(
            model == self.model_alias && object == "list",
            "Response does not match the vector helper model/protocol binding"
        );
        ensure!(
            usage.prompt_tokens == expected_tokens && usage.total_tokens == expected_tokens,
            "Helper evaluated token count differs from the complete preflight input"
        );
        Ok(())
    }

    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let mut response = self
            .client
            .post(self.endpoint.join(path)?)
            .json(body)
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "Vector helper returned HTTP {}",
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
            "Vector helper returned a non-JSON content type"
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= RESPONSE_LIMIT as u64),
            "Vector helper response exceeds the byte limit"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                chunk.len() <= RESPONSE_LIMIT.saturating_sub(bytes.len()),
                "Vector helper response exceeds the byte limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[derive(Deserialize)]
struct Tokenized {
    tokens: Vec<i32>,
}
