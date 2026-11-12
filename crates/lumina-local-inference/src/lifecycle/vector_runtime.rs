mod binding;
mod http;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::Duration;

use super::native_runtime::{NativeHelperRuntime, NativeRequestGuard};
use super::{LaunchConfig, ModelCapability, ModelManifest, Sha256Digest};
use http::VectorConnection;

pub use binding::VectorRuntimeProfile;

pub fn validate_vector_inputs(query: Option<&str>, inputs: &[String]) -> Result<()> {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorTokenUsage {
    pub prompt_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingBatch {
    pub launch_digest: Sha256Digest,
    pub vectors: Vec<Vec<f64>>,
    pub usage: VectorTokenUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RerankScoreSemantics {
    RawClassifierLogit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RerankScore {
    pub index: usize,
    pub relevance_score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RerankBatch {
    pub launch_digest: Sha256Digest,
    pub scores: Vec<RerankScore>,
    pub semantics: RerankScoreSemantics,
    pub usage: VectorTokenUsage,
}

pub struct NativeEmbeddingAdapter<'a> {
    _owner: Option<&'a NativeHelperRuntime>,
    connection: VectorConnection,
}

pub struct NativeRerankAdapter<'a> {
    _owner: Option<&'a NativeHelperRuntime>,
    connection: VectorConnection,
}

impl<'a> NativeEmbeddingAdapter<'a> {
    pub fn new(
        owner: &'a NativeHelperRuntime,
        manifest: &ModelManifest,
        timeout: Duration,
    ) -> Result<Self> {
        Ok(Self {
            _owner: Some(owner),
            connection: VectorConnection::from_owner(
                owner,
                manifest,
                ModelCapability::Embedding,
                timeout,
            )?,
        })
    }

    /// Connects to an independently managed diagnostic helper. This adapter owns
    /// neither its process nor its asset/resource guards; its gate is local to
    /// this adapter and cannot coordinate other connections to that helper.
    pub fn diagnostic(
        launch: &LaunchConfig,
        manifest: &ModelManifest,
        endpoint: &str,
        bearer_token: &str,
        timeout: Duration,
    ) -> Result<Self> {
        Ok(Self {
            _owner: None,
            connection: VectorConnection::diagnostic(
                launch,
                manifest,
                ModelCapability::Embedding,
                endpoint,
                bearer_token,
                timeout,
            )?,
        })
    }

    pub fn helper_arguments(&self) -> Vec<String> {
        self.connection.helper_arguments()
    }

    pub fn model_alias(&self) -> &str {
        &self.connection.model_alias
    }

    /// Cancellation during HTTP invalidates the shared helper request gate until
    /// its owner stops and restarts the process.
    pub async fn embed(&self, inputs: &[String]) -> Result<EmbeddingBatch> {
        self.connection.validate_inputs(None, inputs)?;
        tokio::time::timeout(self.connection.timeout, async {
            let mut operation = self.connection.request_gate.acquire().await?;
            self.embed_inner(inputs, &mut operation).await
        })
        .await?
    }

    async fn embed_inner(
        &self,
        inputs: &[String],
        operation: &mut NativeRequestGuard<'_>,
    ) -> Result<EmbeddingBatch> {
        let mut expected_tokens = 0u64;
        for input in inputs {
            expected_tokens += self
                .connection
                .token_count(input, true, true, operation)
                .await?;
        }
        let response: EmbeddingResponse = self.connection.post(
            "v1/embeddings",
            &serde_json::json!({"input": inputs, "encoding_format": "float", "embd_normalize": 2}),
            operation,
        ).await?;
        self.connection.validate_response(
            &response.model,
            &response.object,
            &response.usage,
            expected_tokens,
        )?;
        ensure!(
            response.data.len() == inputs.len(),
            "Embedding result count differs from its input batch"
        );
        let mut indices = BTreeSet::new();
        let mut vectors = vec![Vec::new(); inputs.len()];
        for item in response.data {
            ensure!(
                item.object == "embedding"
                    && item.index < inputs.len()
                    && indices.insert(item.index),
                "Embedding response has an invalid object or duplicate/out-of-range index"
            );
            ensure!(
                item.embedding.len() == 1024,
                "Embedding dimension differs from the bound Qwen profile"
            );
            ensure!(
                item.embedding.iter().all(|value| value.is_finite()),
                "Embedding contains a non-finite value"
            );
            let norm_squared: f64 = item.embedding.iter().map(|value| value * value).sum();
            ensure!(
                norm_squared.is_finite() && (norm_squared - 1.0).abs() <= 0.0001,
                "Embedding is not L2 normalized as required by the bound profile"
            );
            vectors[item.index] = item.embedding;
        }
        Ok(EmbeddingBatch {
            launch_digest: self.connection.launch_digest.clone(),
            vectors,
            usage: response.usage,
        })
    }
}

impl<'a> NativeRerankAdapter<'a> {
    pub fn new(
        owner: &'a NativeHelperRuntime,
        manifest: &ModelManifest,
        timeout: Duration,
    ) -> Result<Self> {
        Ok(Self {
            _owner: Some(owner),
            connection: VectorConnection::from_owner(
                owner,
                manifest,
                ModelCapability::Reranking,
                timeout,
            )?,
        })
    }

    /// Connects to an independently managed diagnostic helper. This adapter owns
    /// neither its process nor its asset/resource guards; its gate is local to
    /// this adapter and cannot coordinate other connections to that helper.
    pub fn diagnostic(
        launch: &LaunchConfig,
        manifest: &ModelManifest,
        endpoint: &str,
        bearer_token: &str,
        timeout: Duration,
    ) -> Result<Self> {
        Ok(Self {
            _owner: None,
            connection: VectorConnection::diagnostic(
                launch,
                manifest,
                ModelCapability::Reranking,
                endpoint,
                bearer_token,
                timeout,
            )?,
        })
    }

    pub fn helper_arguments(&self) -> Vec<String> {
        self.connection.helper_arguments()
    }

    pub fn model_alias(&self) -> &str {
        &self.connection.model_alias
    }

    /// Cancellation during HTTP invalidates the shared helper request gate until
    /// its owner stops and restarts the process.
    pub async fn rerank(&self, query: &str, documents: &[String]) -> Result<RerankBatch> {
        self.connection.validate_inputs(Some(query), documents)?;
        tokio::time::timeout(self.connection.timeout, async {
            let mut operation = self.connection.request_gate.acquire().await?;
            self.rerank_inner(query, documents, &mut operation).await
        })
        .await?
    }

    async fn rerank_inner(
        &self,
        query: &str,
        documents: &[String],
        operation: &mut NativeRequestGuard<'_>,
    ) -> Result<RerankBatch> {
        let query_tokens = self
            .connection
            .token_count(query, false, false, operation)
            .await?;
        let mut expected_tokens = 0u64;
        for document in documents {
            let document_tokens = self
                .connection
                .token_count(document, false, false, operation)
                .await?;
            // This content-bound BGE has no rerank template: the pinned helper
            // adds BOS + query + EOS + document + EOS, with add_sep=false.
            let pair_tokens = query_tokens + document_tokens + 3;
            ensure!(
                pair_tokens <= 512,
                "Rerank query/document pair exceeds the frozen 512-token capacity"
            );
            expected_tokens += pair_tokens;
        }
        let response: RerankResponse = self.connection.post(
            "v1/rerank",
            &serde_json::json!({"query": query, "documents": documents, "top_n": documents.len()}),
            operation,
        ).await?;
        self.connection.validate_response(
            &response.model,
            &response.object,
            &response.usage,
            expected_tokens,
        )?;
        ensure!(
            response.results.len() == documents.len(),
            "Rerank result count differs from its document batch"
        );
        let mut indices = BTreeSet::new();
        let mut previous = f64::INFINITY;
        for item in &response.results {
            ensure!(
                item.index < documents.len() && indices.insert(item.index),
                "Rerank response has a duplicate or out-of-range index"
            );
            ensure!(
                item.relevance_score.is_finite() && item.relevance_score <= previous,
                "Rerank scores must be finite and sorted in descending relevance order"
            );
            // This pinned helper emits the sentinel when no rank embedding is available.
            ensure!(
                item.relevance_score != -1_000_000.0,
                "Native reranker failed to produce a rank embedding"
            );
            previous = item.relevance_score;
        }
        Ok(RerankBatch {
            launch_digest: self.connection.launch_digest.clone(),
            scores: response.results,
            semantics: RerankScoreSemantics::RawClassifierLogit,
            usage: response.usage,
        })
    }
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    model: String,
    object: String,
    data: Vec<EmbeddingItem>,
    usage: VectorTokenUsage,
}

#[derive(Deserialize)]
struct EmbeddingItem {
    index: usize,
    object: String,
    embedding: Vec<f64>,
}

#[derive(Deserialize)]
struct RerankResponse {
    model: String,
    object: String,
    results: Vec<RerankScore>,
    usage: VectorTokenUsage,
}
