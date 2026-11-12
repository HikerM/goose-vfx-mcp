use anyhow::{ensure, Context, Result};
use lumina::providers::local_inference::lifecycle::{
    validate_vector_admission, validate_vector_inputs, InstallationApplication, LaunchConfig,
    ModelCapability, NativeChatAdapter, NativeChatRequest, NativeEmbeddingAdapter,
    NativeHelperRuntime, NativeRerankAdapter,
};
use serde::Deserialize;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

pub(crate) struct InstalledGeneration {
    pub version_id: String,
    pub engine: PathBuf,
    pub prompt: Option<String>,
    pub request: Option<PathBuf>,
    pub image: Option<PathBuf>,
    pub max_tokens: u32,
    pub timeout_secs: u64,
}

pub(crate) struct InstalledVectorRequest {
    pub version_id: String,
    pub engine: PathBuf,
    pub request: PathBuf,
    pub timeout_secs: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingRequest {
    input: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RerankRequest {
    query: String,
    documents: Vec<String>,
}

pub(crate) async fn vector_installed(
    application: &InstallationApplication,
    arguments: InstalledVectorRequest,
    capability: ModelCapability,
) -> Result<()> {
    ensure!(
        (1..=180).contains(&arguments.timeout_secs),
        "Runtime timeout must be between 1 and 180 seconds"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(&arguments.request)?
        .take(512 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 512 * 1024,
        "Vector request JSON exceeds 512 KiB"
    );
    let (query, inputs) = match capability {
        ModelCapability::Embedding => {
            let request: EmbeddingRequest = serde_json::from_slice(&bytes)?;
            (None, request.input)
        }
        ModelCapability::Reranking => {
            let request: RerankRequest = serde_json::from_slice(&bytes)?;
            (Some(request.query), request.documents)
        }
        _ => anyhow::bail!("This command requires embedding or reranking"),
    };
    validate_vector_inputs(query.as_deref(), &inputs)?;
    let service = application.service().await?;
    let installed = service
        .repository()
        .installed_version(&arguments.version_id)?
        .context("Model version is not installed")?;
    let admission = installed
        .manifest
        .plan
        .admission
        .as_ref()
        .context("Installed vector model has no frozen resource admission")?;
    let profile = validate_vector_admission(admission)?;
    ensure!(
        profile.capability() == capability,
        "Installed model does not support this vector operation"
    );
    let config = LaunchConfig::for_measured_cpu_installation(&installed)?;
    let logs = application
        .root()
        .join("runtime-runs")
        .join(format!("{:032x}", rand::random::<u128>()));
    let timeout = Duration::from_secs(arguments.timeout_secs);
    let mut runtime = NativeHelperRuntime::start(
        service.repository(),
        service.store(),
        config,
        &arguments.engine,
        &logs,
        timeout,
    )
    .await?;
    let operation = async {
        if let Some(query) = &query {
            let adapter = NativeRerankAdapter::new(&runtime, &installed.manifest, timeout)?;
            Ok(serde_json::to_value(adapter.rerank(query, &inputs).await?)?)
        } else {
            let adapter = NativeEmbeddingAdapter::new(&runtime, &installed.manifest, timeout)?;
            Ok(serde_json::to_value(adapter.embed(&inputs).await?)?)
        }
    };
    let completion: Result<serde_json::Value> = tokio::select! {
        result = operation => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            Err(anyhow::anyhow!("Installed vector operation was cancelled"))
        }
    };
    let stopped = runtime.stop();
    let response = completion?;
    let status = stopped?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "response": response,
            "launch": runtime.actual_config(),
            "helper_stopped": true,
            "helper_exit_code": status.code(),
            "stdout_log": runtime.stdout_path(),
            "stderr_log": runtime.stderr_path(),
        }))?
    );
    Ok(())
}

pub(crate) async fn generate_installed(
    application: &InstallationApplication,
    arguments: InstalledGeneration,
) -> Result<()> {
    ensure!(
        (1..=180).contains(&arguments.timeout_secs),
        "Runtime timeout must be between 1 and 180 seconds"
    );
    let service = application.service().await?;
    let installed = service
        .repository()
        .installed_version(&arguments.version_id)?
        .context("Model version is not installed")?;
    let config = LaunchConfig::for_measured_cpu_installation(&installed)?;
    let mut request = if let Some(path) = arguments.request {
        let mut bytes = Vec::new();
        const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
        std::fs::File::open(path)?
            .take(MAX_REQUEST_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_REQUEST_BYTES,
            "Chat request exceeds 16 MiB"
        );
        serde_json::from_slice::<NativeChatRequest>(&bytes)?
    } else {
        NativeChatRequest::text(
            arguments
                .prompt
                .context("Generation requires a prompt or request")?,
            arguments.max_tokens,
        )
    };
    if let Some(image) = &arguments.image {
        let vision = installed
            .manifest
            .plan
            .admission
            .as_ref()
            .and_then(|admission| admission.envelope.workload.vision.as_ref())
            .context("Installed version has no bounded vision profile")?;
        let mut bytes = Vec::new();
        std::fs::File::open(image)?
            .take(vision.max_input_bytes + 1)
            .read_to_end(&mut bytes)?;
        vision.inspect_images(&[&bytes])?;
        request.add_png(bytes)?;
    }
    request.validate(&config)?;
    let logs = application
        .root()
        .join("runtime-runs")
        .join(format!("{:032x}", rand::random::<u128>()));
    let timeout = Duration::from_secs(arguments.timeout_secs);
    let mut runtime = NativeHelperRuntime::start(
        service.repository(),
        service.store(),
        config,
        &arguments.engine,
        &logs,
        timeout,
    )
    .await?;
    let completion = {
        let adapter = NativeChatAdapter::new(&runtime)?;
        tokio::select! {
            result = adapter.complete(&request, timeout) => result,
            signal = tokio::signal::ctrl_c() => {
                signal?;
                Err(anyhow::anyhow!("Installed model generation was cancelled"))
            }
        }
    };
    let stopped = runtime.stop();
    let response = completion?;
    let status = stopped?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "response": response,
            "launch": runtime.actual_config(),
            "helper_stopped": true,
            "helper_exit_code": status.code(),
            "stdout_log": runtime.stdout_path(),
            "stderr_log": runtime.stderr_path(),
        }))?
    );
    Ok(())
}
