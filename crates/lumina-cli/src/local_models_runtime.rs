use anyhow::{ensure, Context, Result};
use lumina::providers::local_inference::lifecycle::{
    ComponentRole, InstallationApplication, InstalledVersion, LaunchChatTemplate, LaunchConfig,
    NativeChatAdapter, NativeChatRequest, NativeHelperRuntime, RepositoryPath, ResourceCacheType,
    RuntimeExecutionMode, RuntimeParameters,
};
use std::collections::BTreeMap;
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
    let config = cpu_launch(&installed)?;
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

fn cpu_launch(installed: &InstalledVersion) -> Result<LaunchConfig> {
    let manifest = &installed.manifest;
    let workload = manifest
        .plan
        .admission
        .as_ref()
        .context("Installed model has no frozen resource admission")?
        .envelope
        .workload
        .clone();
    let mut component_files: BTreeMap<ComponentRole, Vec<RepositoryPath>> = BTreeMap::new();
    for component in manifest.plan.dependencies.components.values() {
        let paths = component_files.entry(component.role).or_default();
        for id in &component.artifact_ids {
            let path = manifest.installed_files[id].relative_path.clone();
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    let cache_name = |kind| match kind {
        ResourceCacheType::F16 => "f16".to_string(),
        ResourceCacheType::F32 => "f32".to_string(),
    };
    Ok(LaunchConfig {
        installation_id: manifest.installation_id.clone(),
        version_id: manifest.version_id.clone(),
        manifest_digest: installed.manifest_digest.clone(),
        runtime: manifest.plan.runtime.clone(),
        capabilities: manifest.plan.target_capabilities.clone(),
        component_files,
        parameters: RuntimeParameters::LlamaCpp {
            chat_template: LaunchChatTemplate::Embedded,
            context_length: workload.context_tokens,
            gpu_layers: 0,
            cpu_threads: 4,
            kv_key_type: cache_name(workload.kv_key_type),
            kv_value_type: cache_name(workload.kv_value_type),
            batch_size: workload.batch_tokens,
            flash_attention: true,
            workload: Some(workload),
        },
        mode: RuntimeExecutionMode::Helper { port: 0 },
    })
}
