use anyhow::{ensure, Result};
use lumina_local_inference::lifecycle::*;

#[tokio::main]
async fn main() -> Result<()> {
    let mut arguments: Vec<_> = std::env::args().skip(1).collect();
    ensure!(arguments.len()>=3,"Usage: inspect_modelscope_component repository revision source-path [source-path ... | --candidate]");
    let auto = arguments
        .last()
        .is_some_and(|argument| argument == "--candidate");
    if auto {
        arguments.pop();
    }
    let provider = ModelScopeProvider::new()?;
    let source = provider
        .inspect_repository(
            ModelScopeRepository::parse(&arguments[0])?,
            &arguments[1],
            None,
        )
        .await?;
    let paths = arguments[2..]
        .iter()
        .map(RepositoryPath::parse)
        .collect::<Result<Vec<_>>>()?;
    let component = if auto {
        ensure!(
            paths.len() == 1,
            "Candidate inspection requires exactly one selected path"
        );
        let candidate = discover_gguf_variant_candidates(&source.inventory)?
            .into_iter()
            .find(|candidate| {
                candidate
                    .files
                    .iter()
                    .any(|file| file.path.as_str() == paths[0].as_str())
            })
            .ok_or_else(|| anyhow::anyhow!("Selected source file has no candidate"))?;
        provider
            .inspect_variant_candidate(
                &source.inventory,
                &candidate.id,
                &paths[0],
                ComponentRole::Main,
                GgufReadLimits::default(),
                None,
            )
            .await?
    } else {
        provider
            .inspect_selected_component(
                &source.inventory,
                &paths,
                ComponentRole::Main,
                GgufReadLimits::default(),
                None,
            )
            .await?
    };
    println!(
        "{}",
        serde_json::to_string(
            &serde_json::json!({"component":component,"scope":"Actual structural GGUF component only; no full remote weight verification, installation or runtime acceptance"})
        )?
    );
    Ok(())
}
