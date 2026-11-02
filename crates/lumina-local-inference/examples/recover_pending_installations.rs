use anyhow::{ensure, Result};
use lumina_local_inference::lifecycle::*;
use std::path::PathBuf;
use std::sync::Arc;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

async fn run() -> Result<()> {
    let arguments = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    ensure!(
        arguments.len() == 2 && arguments.iter().all(|path| path.is_absolute()),
        "Usage: recover_pending_installations <absolute-database> <absolute-managed-root>"
    );
    let repository = Arc::new(LifecycleRepository::open(&arguments[0])?);
    let store = Arc::new(InstallationStore::open(&arguments[1])?);
    let page = recover_installations_page(
        Arc::clone(&repository),
        Arc::clone(&store),
        RecoveryPageRequest {
            after_id: None,
            limit: 64,
            worker_name: "startup-recovery-diagnostic".into(),
        },
        Arc::new(|| Ok(None)),
        now,
    )
    .await?;
    let mut completed = Vec::new();
    for outcome in &page.outcomes {
        if let RecoveryDisposition::Completed { version_id } = &outcome.disposition {
            let installed = repository
                .installed_version(version_id)?
                .ok_or_else(|| anyhow::anyhow!("Recovered version is missing"))?;
            let events = repository.events(&outcome.job_id, 0, 1000)?;
            let replay = resume_installation(
                Arc::clone(&repository),
                Arc::clone(&store),
                outcome.job_id.clone(),
                None,
                "startup-recovery-replay".into(),
                now,
            )
            .await?;
            ensure!(
                replay == installed && repository.events(&outcome.job_id, 0, 1000)? == events,
                "Recovery replay changed its installation or events"
            );
            completed.push(serde_json::json!({
                "job":repository.read_job(&outcome.job_id)?,
                "input":repository.read_installation_input(&outcome.job_id)?,
                "installed_version":installed,
                "file_transfers":repository.file_transfers(&outcome.job_id)?,
                "file_events":repository.file_transfer_events(&outcome.job_id,0,1000)?,
                "job_events":events,
                "commit_journal":repository.installation_commit(&outcome.job_id)?,
                "capability_evidence":repository.evidence(&replay.manifest.installation_id)?,
                "replay_verified":true
            }));
        }
    }
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "page":page,"completed":completed,"active_installations":repository.active_installations()?,
            "scope":"One bounded recovery page driven only by database and managed root; credentials are absent for this public-source diagnostic. Not application startup wiring, a full queue drain, Runtime launch or capability READY acceptance."
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
