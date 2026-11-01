use anyhow::{ensure, Result};
use lumina_local_inference::lifecycle::{CatalogPageRequest, ModelScopeProvider};

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 3,
        "Usage: inspect_modelscope_catalog query page-number page-size"
    );
    let page = ModelScopeProvider::new()?
        .search_catalog_page(
            CatalogPageRequest {
                query: arguments[0].clone(),
                page_number: arguments[1].parse()?,
                page_size: arguments[2].parse()?,
            },
            None,
        )
        .await?;
    println!("{}", serde_json::to_string(&page)?);
    Ok(())
}
