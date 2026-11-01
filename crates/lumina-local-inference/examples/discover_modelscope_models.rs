use anyhow::{ensure, Result};
use lumina_local_inference::lifecycle::{
    ModelDiscoveryRequest, ModelScopeProvider, ModelSearchKind, ModelSearchQuery,
};

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 4,
        "Usage: discover_modelscope_models query limit page-budget verification-limit"
    );
    let response = ModelScopeProvider::new()?
        .discover_models(
            ModelDiscoveryRequest {
                query: ModelSearchQuery {
                    text: arguments[0].clone(),
                    kind: ModelSearchKind::Name,
                },
                limit: arguments[1].parse()?,
                page_budget: arguments[2].parse()?,
                verification_limit: arguments[3].parse()?,
            },
            None,
        )
        .await?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}
