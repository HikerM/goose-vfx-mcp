use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::{
    install_from_local_cache, installation_directory, CacheInstallRequest, InstallationStore,
    LifecycleRepository,
};
use serde_json::json;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System clock must be after Unix epoch")
        .as_secs() as i64
}

fn main() -> Result<()> {
    let args = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    ensure!(
        args.len() == 3,
        "Usage: install_cached_plan <database> <managed-root> <request.json>"
    );
    ensure!(
        args.iter().all(|path| path.is_absolute()),
        "Validation paths must be absolute"
    );
    let repository = LifecycleRepository::open(&args[0])?;
    let store = InstallationStore::open(&args[1])?;
    let request: CacheInstallRequest = serde_json::from_reader(std::fs::File::open(&args[2])?)?;
    let before = repository
        .read_job(&request.job_id)?
        .context("Installation job not found")?;
    let installed =
        install_from_local_cache(&repository, &store, &request, "cache-install-example", now)?;
    let events = repository.events(&request.job_id, 0, 1000)?;
    let replayed =
        install_from_local_cache(&repository, &store, &request, "cache-install-replay", now)?;
    ensure!(
        replayed == installed && repository.events(&request.job_id, 0, 1000)? == events,
        "Completed installation replay changed its result or job events"
    );
    let manifest_relative = installation_directory(&installed.manifest, true)?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "job_before":before,
            "job_after":repository.read_job(&request.job_id)?,
            "manifest_digest":installed.manifest_digest,
            "manifest_path":store.root().join(manifest_relative.as_str()).join("manifest.json"),
            "installed_version":installed,
            "file_transfers":repository.file_transfers(&request.job_id)?,
            "commit_journal":repository.installation_commit(&request.job_id)?,
            "active_installations":repository.active_installations()?,
            "capability_evidence":repository.evidence(&request.installation_id)?,
            "replay_verified":true,
            "scope":"Pinned-source-checksum local cache import and filesystem installation only; no network transfer, runtime launch or capability READY acceptance"
        }))?
    );
    Ok(())
}
