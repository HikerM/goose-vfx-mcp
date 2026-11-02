use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::{
    install_from_modelscope, installation_directory, InstallationStore, LifecycleRepository,
    ModelScopeInstallRequest,
};
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System clock must be after Unix epoch")
        .as_secs() as i64
}

async fn run() -> Result<()> {
    let args = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    ensure!(
        args.len() == 3 && args.iter().all(|path| path.is_absolute()),
        "Usage: install_modelscope_plan <absolute-database> <absolute-managed-root> <absolute-request.json>"
    );
    let repository = Arc::new(LifecycleRepository::open(&args[0])?);
    let store = Arc::new(InstallationStore::open(&args[1])?);
    let mut bytes = Vec::new();
    std::fs::File::open(&args[2])?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 64 * 1024, "Request exceeds input budget");
    let request: ModelScopeInstallRequest = serde_json::from_slice(&bytes)?;
    let before = repository
        .read_job(&request.job_id)?
        .context("Installation job not found")?;
    let active_before = repository.active_installations()?;
    let installed = install_from_modelscope(
        Arc::clone(&repository),
        Arc::clone(&store),
        request.clone(),
        None,
        "modelscope-install-example".into(),
        now,
    )
    .await?;
    let events = repository.events(&request.job_id, 0, 1000)?;
    let file_events = repository.file_transfer_events(&request.job_id, 0, 1000)?;
    let replayed = install_from_modelscope(
        Arc::clone(&repository),
        Arc::clone(&store),
        request.clone(),
        None,
        "modelscope-install-replay".into(),
        now,
    )
    .await?;
    ensure!(
        replayed == installed
            && repository.events(&request.job_id, 0, 1000)? == events
            && repository.file_transfer_events(&request.job_id, 0, 1000)? == file_events,
        "Completed installation replay changed its result or events"
    );
    let manifest_relative = installation_directory(&installed.manifest, true)?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "job_before": before,
            "job_after": repository.read_job(&request.job_id)?,
            "manifest_digest": installed.manifest_digest,
            "manifest_path": store.root().join(manifest_relative.as_str()).join("manifest.json"),
            "installed_version": installed,
            "file_transfers": repository.file_transfers(&request.job_id)?,
            "file_events": file_events,
            "job_events": events,
            "commit_journal": repository.installation_commit(&request.job_id)?,
            "active_installations_before": active_before,
            "active_installations_after": repository.active_installations()?,
            "capability_evidence": repository.evidence(&request.installation_id)?,
            "replay_verified": true,
            "scope": "Public fixed-ModelScope-source network installation and completed replay; no runtime launch, product entry or capability READY acceptance"
        }))?
    );
    Ok(())
}

fn main() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(run())
}
