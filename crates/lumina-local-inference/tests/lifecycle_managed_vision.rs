use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::{
    managed_vision_status, run_managed_vision_validation, CapabilityStatus, InstallationStore,
    LifecycleRepository, ModelCapability,
};
use std::{path::PathBuf, time::Duration};

#[tokio::test]
#[ignore = "Runs the frozen vision suite on a real installed model and projector"]
async fn managed_vision_suite_checks_changed_images_and_multiturn_on_the_registered_pair(
) -> Result<()> {
    let root =
        PathBuf::from(std::env::var_os("LUMINA_VISION_STORE").context("Set LUMINA_VISION_STORE")?);
    let version = std::env::var("LUMINA_VISION_VERSION")?;
    let engine = PathBuf::from(
        std::env::var_os("LUMINA_VISION_ENGINE").context("Set LUMINA_VISION_ENGINE")?,
    );
    let db = root.join("models.db");
    ensure!(
        db.is_file() && root.is_absolute() && engine.is_absolute(),
        "Expected an existing managed installation and engine"
    );
    let store = InstallationStore::open(&root)?;
    let result = run_managed_vision_validation(
        &LifecycleRepository::open(&db)?,
        &store,
        &version,
        &engine,
        Duration::from_secs(180),
    )
    .await?;
    println!("{}", serde_json::to_string(&result)?);
    let status =
        managed_vision_status(&LifecycleRepository::open(&db)?, &store, &version, &engine)?;
    ensure!(
        status.evidence.as_ref() == Some(&result.evidence)
            && status.status == result.evidence.status,
        "Reopened vision evidence differs"
    );
    ensure!(
        result.evidence.capability == ModelCapability::Vision
            && result.evidence.status == CapabilityStatus::Ready
            && result.completed == 10
            && result.correct >= 8
            && result.replacements_passed == 4
            && result.dialogs_completed == 3
            && result.dialogs_passed == 3,
        "Vision suite did not meet A43; inspect registered evidence"
    );
    Ok(())
}
