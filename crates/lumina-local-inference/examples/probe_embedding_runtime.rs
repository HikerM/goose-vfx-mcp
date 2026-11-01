use anyhow::{ensure, Context, Result};
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use lumina_local_inference::lifecycle::*;
use serde::Deserialize;
use std::fmt::Write;
use std::io::Read;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceFixture {
    repository: ModelScopeRepository,
    revision: String,
    file: RepositoryPath,
    sha256: Sha256Digest,
    size_bytes: u64,
    local_path: PathBuf,
}

fn hash_file(path: &Path) -> Result<Sha256Digest> {
    let mut file = std::fs::File::open(path)?;
    let mut state = sha2::Sha256::default();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        sha2::Digest::update(&mut state, &buffer[..count]);
    }
    let mut encoded = String::with_capacity(64);
    for byte in sha2::Digest::finalize(state) {
        write!(&mut encoded, "{byte:02x}")?;
    }
    Sha256Digest::parse(encoded)
}

fn embed(backend: &LlamaBackend, model: &LlamaModel, text: &str) -> Result<(Vec<f64>, usize)> {
    let mut tokens = model.str_to_token(text, AddBos::Always)?;
    if tokens.last() != Some(&model.token_eos()) {
        tokens.push(model.token_eos());
    }
    ensure!(
        !tokens.is_empty() && tokens.len() <= 512,
        "Probe input exceeds context budget"
    );
    let parameters = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(512))
        .with_n_batch(512)
        .with_n_ubatch(512)
        .with_n_threads(4)
        .with_n_threads_batch(4)
        .with_embeddings(true)
        .with_pooling_type(LlamaPoolingType::Last);
    let mut context = model.new_context(backend, parameters)?;
    let mut batch = LlamaBatch::new(tokens.len(), 1);
    batch.add_sequence(&tokens, 0, true)?;
    context.decode(&mut batch)?;
    let raw = context.embeddings_seq_ith(0)?;
    ensure!(
        raw.len() == 1024 && raw.iter().all(|value| value.is_finite()),
        "Invalid actual embedding output"
    );
    let norm = raw
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    ensure!(
        norm.is_finite() && norm > 1e-12,
        "Actual embedding norm is invalid"
    );
    let normalized: Vec<_> = raw.iter().map(|value| f64::from(*value) / norm).collect();
    let normalized_norm = normalized.iter().map(|value| value.powi(2)).sum::<f64>();
    ensure!((normalized_norm - 1.0).abs() < 1e-6, "Normalization failed");
    Ok((normalized, tokens.len()))
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    ensure!(
        arguments.len() == 2,
        "Usage: probe_embedding_runtime source-verification.json output.json"
    );
    let input = std::fs::File::open(&arguments[0])?;
    ensure!(
        input.metadata()?.len() <= 1024 * 1024,
        "Source fixture is too large"
    );
    let fixture: SourceFixture = serde_json::from_reader(input.take(1024 * 1024 + 1))?;
    let provider = ModelScopeProvider::new()?;
    let inventory = provider
        .inspect_at(
            fixture.repository.clone(),
            fixture.revision.clone(),
            fixture.revision.clone(),
            chrono::Utc::now().timestamp(),
            None,
        )
        .await?;
    let source = inventory
        .files
        .get(fixture.file.as_str())
        .context("Asset missing from pinned repository")?;
    ensure!(
        source.size_bytes == Some(fixture.size_bytes)
            && source.sha256.as_ref() == Some(&fixture.sha256),
        "Fixture differs from current pinned source inventory"
    );
    ensure!(
        std::fs::metadata(&fixture.local_path)?.len() == fixture.size_bytes
            && hash_file(&fixture.local_path)? == fixture.sha256,
        "Actual asset differs from pinned source"
    );
    let inspection =
        GgufInspection::inspect_file(&fixture.local_path, GgufReadLimits::for_llama_cpp())?;
    ensure!(
        inspection.architecture() == Some("qwen3")
            && inspection
                .metadata
                .get("qwen3.pooling_type")
                .and_then(GgufValue::as_u64)
                == Some(3),
        "Probe requires observed qwen3 last-token embedding metadata"
    );
    ensure!(
        inspection
            .metadata
            .get("qwen3.embedding_length_out")
            .is_none_or(|value| value.as_u64() == Some(1024)),
        "Locked wrapper requires matching input and embedding output widths"
    );
    let backend = LlamaBackend::init()?;
    let model = LlamaModel::load_from_file(
        &backend,
        &fixture.local_path,
        &LlamaModelParams::default().with_n_gpu_layers(0),
    )?;
    ensure!(
        model.n_embd() == 1024,
        "Unexpected actual model embedding dimension"
    );
    let documents = [
        "Paris is the capital and largest city of France.",
        "To sort a Python list in place, call its sort method or use sorted to create a new list.",
        "光合作用是绿色植物利用阳光把二氧化碳和水转化为有机物并释放氧气的过程。",
        "Saturn is a planet surrounded by prominent rings made of ice and rock.",
        "Bread dough rises because yeast produces carbon dioxide during fermentation.",
        "A bicycle has two wheels and is powered by its rider pedaling.",
    ];
    let queries = [
        ("What is the capital of France?", 0),
        ("How can I arrange a Python list in ascending order?", 1),
        ("植物怎样利用太阳能制造养分？", 2),
    ];
    let started = Instant::now();
    let mut vectors = Vec::new();
    let mut token_counts = Vec::new();
    for document in &documents {
        let (vector, count) = embed(&backend, &model, document)?;
        vectors.push(vector);
        token_counts.push(count);
    }
    let mut cases = Vec::new();
    for (query, expected) in queries {
        let text = format!("Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery: {query}");
        let (vector, token_count) = embed(&backend, &model, &text)?;
        let scores: Vec<_> = vectors
            .iter()
            .map(|document| {
                vector
                    .iter()
                    .zip(document)
                    .map(|(left, right)| left * right)
                    .sum::<f64>()
            })
            .collect();
        let strongest_distractor = scores
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != expected)
            .map(|(_, score)| *score)
            .fold(f64::NEG_INFINITY, f64::max);
        let passed =
            scores.iter().all(|score| score.is_finite()) && scores[expected] > strongest_distractor;
        cases.push(serde_json::json!({"query":query,"expectedDocument":expected,"scores":scores,"passed":passed,"tokenCount":token_count,"vector":vector}));
    }
    let passed = cases.iter().all(|case| case["passed"] == true);
    ensure!(
        hash_file(&fixture.local_path)? == fixture.sha256,
        "Asset changed during actual inference"
    );
    let report = serde_json::json!({
        "observedAt":chrono::Utc::now().to_rfc3339(),"status":if passed {"PASS"} else {"FAIL"},
        "scope":"M0 actual locked-runtime CPU embedding and fixed semantic retrieval probe; not product installation, batched Adapter, GPU, full benchmark or capability READY",
        "repository":fixture.repository,"revision":fixture.revision,"file":fixture.file,
        "modelSha256":fixture.sha256,"sizeBytes":fixture.size_bytes,"headerDigest":inspection.header_digest,
        "snapshotDigest":inventory.snapshot.content_digest()?,"binarySha256":hash_file(&std::env::current_exe()?)?,
        "cargoLockSha256":Sha256Digest::of_bytes(include_bytes!("../../../Cargo.lock")),
        "probeSourceSha256":Sha256Digest::of_bytes(include_bytes!("probe_embedding_runtime.rs")),
        "runtime":{"engine":"locked llama.cpp","execution":"cpu","gpuLayers":0,"threads":4,"context":512,"batch":512,"microBatch":512,"pooling":"last","appendEos":true,"dimension":1024,"normalization":"l2"},
        "elapsedSeconds":started.elapsed().as_secs_f64(),"documents":documents,"documentTokenCounts":token_counts,
        "documentVectors":vectors,"cases":cases
    });
    std::fs::write(&arguments[1], serde_json::to_vec_pretty(&report)?)?;
    ensure!(
        passed,
        "Actual embeddings failed fixed semantic retrieval checks; inspect persisted evidence"
    );
    println!("PASS: actual 1024-dimensional vectors and three fixed semantic retrieval cases");
    Ok(())
}
