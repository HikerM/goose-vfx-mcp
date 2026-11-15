use anyhow::Result;
use lumina_providers::local_inference::lifecycle::{Sha256Digest, TaskPublicationContext};

pub(super) fn project_reuse_context(
    task: &super::project_task_reuse::PreparedProjectTask,
) -> Result<TaskPublicationContext> {
    // Bind the host work absent from the native engine identity. Test-only sources
    // and the resource-profile registry are excluded so registering measured
    // evidence cannot change the implementation it measured.
    macro_rules! sources {
        ($($path:literal),+ $(,)?) => {
            [$(($path, Sha256Digest::of_bytes(include_str!($path).replace("\r\n", "\n").as_bytes()))),+]
        };
    }
    let sources = sources!(
        "task_publication.rs",
        "task_service.rs",
        "project_task_reuse.rs",
        "project_payload.rs",
        "task_configuration.rs",
        "task_inputs.rs",
        "task_output.rs",
        "project_assets.rs",
        "domain.rs",
        "store.rs",
        "files.rs",
        "formats.rs",
        "avi_audio.rs",
        "../project/assets.rs",
        "../../../../Cargo.lock",
    );
    let tier = task.payload_tier()?;
    let limits = [
        tier.source_assets_limit(),
        tier.record_limit(),
        tier.output_archive_limit(),
    ];
    Ok(TaskPublicationContext::ProjectParameterReuseV2 {
        implementation_sources_digest: Sha256Digest::of_serializable(&(sources, limits))?,
        payload_tier: tier,
    })
}
