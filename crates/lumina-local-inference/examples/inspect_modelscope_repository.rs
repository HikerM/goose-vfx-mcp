use anyhow::{ensure, Result};
use lumina_local_inference::lifecycle::{
    observe_document_tasks, ModelScopeProvider, ModelScopeRepository, RepositoryPath,
};

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        (2..=18).contains(&arguments.len()),
        "Usage: inspect_modelscope_repository repository revision [source-document-path ...]"
    );
    let provider = ModelScopeProvider::new()?;
    let inspection = provider
        .inspect_repository(
            ModelScopeRepository::parse(&arguments[0])?,
            &arguments[1],
            None,
        )
        .await?;
    inspection.validate()?;
    let mut documents = Vec::new();
    for argument in arguments.iter().skip(2) {
        let document = provider
            .read_source_document(
                &inspection.inventory,
                &RepositoryPath::parse(argument)?,
                None,
            )
            .await?;
        documents.push(observe_document_tasks(&document));
    }
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "inspection": inspection,
            "document_observations": documents,
            "scope": "Source inventory and document claims only; no component or runtime acceptance",
        }))?
    );
    Ok(())
}
