use anyhow::{ensure, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{
    partial_error, scoped_auth_headers, status_error, transport_error, ModelScopeProvider,
};
use crate::lifecycle::ModelScopeRepository;

const CATALOG_URL: &str = "https://modelscope.cn/openapi/v1/models";
const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_CATALOG_WINDOW: u64 = 3000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSearchKind {
    Name,
    Series,
    Architecture,
    Quantization,
    Component,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSearchQuery {
    pub text: String,
    pub kind: ModelSearchKind,
}

impl ModelSearchQuery {
    pub fn terms(&self) -> Result<Vec<String>> {
        ensure!(
            self.text.len() <= 512 && !self.text.chars().any(char::is_control),
            "Invalid model search query"
        );
        let normalized = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        ensure!(!normalized.is_empty(), "Model search query is empty");
        let without_format = normalized
            .split_whitespace()
            .filter(|term| !term.eq_ignore_ascii_case("gguf"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut queries = if normalized
            .split_whitespace()
            .any(|term| term.eq_ignore_ascii_case("gguf"))
        {
            vec![normalized]
        } else {
            vec![format!("{normalized} GGUF"), normalized]
        };
        if !without_format.is_empty() && !queries.contains(&without_format) {
            queries.push(without_format);
        }
        ensure!(
            queries.iter().all(|query| query.len() <= 512),
            "Expanded model search query exceeds its byte limit"
        );
        Ok(queries)
    }

    pub fn exact_repository(&self) -> Option<ModelScopeRepository> {
        (self.kind == ModelSearchKind::Name)
            .then(|| ModelScopeRepository::parse(self.text.trim()).ok())
            .flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPageRequest {
    pub query: String,
    pub page_number: u32,
    pub page_size: u32,
}

impl CatalogPageRequest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.query.trim().is_empty()
                && self.query.len() <= 512
                && !self.query.chars().any(char::is_control),
            "Invalid catalog search query"
        );
        ensure!(
            self.page_number > 0
                && (1..=50).contains(&self.page_size)
                && u64::from(self.page_number) * u64::from(self.page_size) <= MAX_CATALOG_WINDOW,
            "ModelScope catalog pagination is outside its bounded window"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub repository: ModelScopeRepository,
    pub display_name: Option<String>,
    pub downloads: Option<u64>,
    pub private: Option<bool>,
    pub gated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelScopeCatalogPage {
    pub request: CatalogPageRequest,
    pub models: Vec<CatalogModel>,
    pub total_count: u64,
}

impl ModelScopeCatalogPage {
    pub fn has_more(&self) -> bool {
        u64::from(self.request.page_number) * u64::from(self.request.page_size) < self.total_count
    }

    pub fn next_request(&self) -> Option<CatalogPageRequest> {
        if !self.has_more() {
            return None;
        }
        let next = CatalogPageRequest {
            page_number: self.request.page_number.checked_add(1)?,
            ..self.request.clone()
        };
        next.validate().ok().map(|()| next)
    }
}

#[derive(Deserialize)]
struct CatalogResponse {
    success: bool,
    data: Option<CatalogData>,
}

#[derive(Deserialize)]
struct CatalogData {
    models: Vec<CatalogWireModel>,
    total_count: u64,
    page_number: u32,
    page_size: u32,
}

#[derive(Deserialize)]
struct CatalogWireModel {
    id: ModelScopeRepository,
    display_name: Option<String>,
    downloads: Option<u64>,
    private: Option<bool>,
    gated: Option<bool>,
}

pub fn parse_modelscope_catalog_page(
    body: &[u8],
    request: CatalogPageRequest,
) -> Result<ModelScopeCatalogPage> {
    request.validate()?;
    ensure!(body.len() <= MAX_CATALOG_BYTES, partial_error());
    let response: CatalogResponse = serde_json::from_slice(body).map_err(|_| partial_error())?;
    ensure!(response.success, partial_error());
    let data = response.data.ok_or_else(partial_error)?;
    ensure!(
        data.page_number == request.page_number && data.page_size == request.page_size,
        partial_error()
    );
    let offset = u64::from(request.page_number - 1) * u64::from(request.page_size);
    let expected = data
        .total_count
        .saturating_sub(offset)
        .min(u64::from(request.page_size));
    ensure!(data.models.len() as u64 == expected, partial_error());
    let mut seen = BTreeSet::new();
    let mut models = Vec::with_capacity(data.models.len());
    for model in data.models {
        ensure!(seen.insert(model.id.clone()), partial_error());
        models.push(CatalogModel {
            repository: model.id,
            display_name: model.display_name,
            downloads: model.downloads,
            private: model.private,
            gated: model.gated,
        });
    }
    Ok(ModelScopeCatalogPage {
        request,
        models,
        total_count: data.total_count,
    })
}

impl ModelScopeProvider {
    pub async fn search_catalog_page(
        &self,
        request: CatalogPageRequest,
        token: Option<&str>,
    ) -> Result<ModelScopeCatalogPage> {
        request.validate()?;
        let mut url = reqwest::Url::parse(CATALOG_URL)?;
        url.query_pairs_mut()
            .append_pair("search", &request.query)
            .append_pair("sort", "downloads")
            .append_pair("page_number", &request.page_number.to_string())
            .append_pair("page_size", &request.page_size.to_string());
        let response = self
            .client
            .get(url)
            .headers(scoped_auth_headers(token)?)
            .send()
            .await
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(status_error(response.status().as_u16()).into());
        }
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= MAX_CATALOG_BYTES as u64),
            partial_error()
        );
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport_error)?;
            ensure!(
                chunk.len() <= MAX_CATALOG_BYTES.saturating_sub(body.len()),
                partial_error()
            );
            body.extend_from_slice(&chunk);
        }
        parse_modelscope_catalog_page(&body, request)
    }
}
