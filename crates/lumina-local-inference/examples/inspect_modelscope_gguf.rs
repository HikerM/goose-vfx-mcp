use anyhow::{ensure, Result};
use lumina_local_inference::lifecycle::{
    GgufInspection, GgufReadLimits, ModelScopeProvider, ModelScopeRepository, RepositoryPath,
};
use std::path::Path;

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        (3..=4).contains(&arguments.len()),
        "Usage: inspect_modelscope_gguf repository revision source-path [local-file]"
    );
    let provider = ModelScopeProvider::new()?;
    let source = provider
        .inspect_repository(
            ModelScopeRepository::parse(&arguments[0])?,
            &arguments[1],
            None,
        )
        .await?;
    let remote = provider
        .inspect_remote_gguf(
            &source.inventory,
            &RepositoryPath::parse(&arguments[2])?,
            GgufReadLimits::default(),
            None,
        )
        .await?;
    let local_header_matches = if let Some(path) = arguments.get(3) {
        let local = GgufInspection::inspect_file(Path::new(path), GgufReadLimits::default())?;
        ensure!(
            local == remote.inspection,
            "Remote and local GGUF headers differ"
        );
        Some(true)
    } else {
        None
    };
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "remote": remote,
            "local_header_matches": local_header_matches,
            "scope": "Pinned remote GGUF metadata only; declared source file hash is not a full-download verification or runtime capability",
        }))?
    );
    Ok(())
}
