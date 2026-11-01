use anyhow::{ensure, Result};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use tokio::time::Instant;

use super::{
    CatalogModel, CatalogPageRequest, ModelScopeProvider, ModelSearchQuery, ModelSourceError,
    RepositoryInspection,
};
use crate::lifecycle::{LifecycleErrorCode, ModelScopeRepository, RepositorySnapshot};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDiscoveryRequest {
    pub query: ModelSearchQuery,
    pub limit: u16,
    pub page_budget: u16,
    pub verification_limit: u16,
}

impl ModelDiscoveryRequest {
    pub fn validate(&self) -> Result<()> {
        self.query.terms()?;
        ensure!(
            (1..=100).contains(&self.limit),
            "Invalid model discovery limit"
        );
        ensure!(
            (1..=12).contains(&self.page_budget),
            "Invalid model discovery page budget"
        );
        ensure!(
            self.verification_limit <= self.limit && self.verification_limit <= 16,
            "Invalid model discovery verification limit"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSearchMatch {
    ExactRepository,
    ExactName,
    Related,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryDiscoveryFacts {
    pub snapshot: RepositorySnapshot,
    pub file_count: u64,
    pub gguf_filename_count: u64,
    pub portable_gguf_filename_count: u64,
    pub unsupported_path_count: u64,
}

impl RepositoryDiscoveryFacts {
    fn from_inspection(inspection: RepositoryInspection) -> Result<Self> {
        inspection.validate()?;
        let inventory = inspection.inventory;
        let mut gguf_filename_count = 0;
        let mut portable_gguf_filename_count = 0;
        for file in inventory.files.values() {
            if file.path.as_str().to_ascii_lowercase().ends_with(".gguf") {
                gguf_filename_count += 1;
                portable_gguf_filename_count += u64::from(file.path.materializable_path().is_ok());
            }
        }
        Ok(Self {
            unsupported_path_count: inventory.path_diagnostics().len() as u64,
            file_count: inventory.files.len() as u64,
            snapshot: inventory.snapshot,
            gguf_filename_count,
            portable_gguf_filename_count,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RepositoryDiscoveryState {
    Unverified,
    Verified { facts: RepositoryDiscoveryFacts },
    Failed { failure: ModelSourceError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDiscoveryCandidate {
    pub repository: ModelScopeRepository,
    pub matched_as: ModelSearchMatch,
    pub catalog: Option<CatalogModel>,
    pub source: RepositoryDiscoveryState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExactRepositoryLookup {
    NotRequested,
    Verified {
        repository: ModelScopeRepository,
    },
    Failed {
        repository: ModelScopeRepository,
        failure: ModelSourceError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryCoverage {
    Complete,
    Limited,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogQueryTrace {
    pub query: String,
    pub fetched_pages: u32,
    pub total_count: Option<u64>,
    pub next_page: Option<u32>,
    pub complete: bool,
    pub failure: Option<ModelSourceError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDiscoveryResponse {
    pub request: ModelDiscoveryRequest,
    pub coverage: DiscoveryCoverage,
    pub exact_repository: ExactRepositoryLookup,
    pub queries: Vec<CatalogQueryTrace>,
    pub candidates: Vec<ModelDiscoveryCandidate>,
}

fn classify_match(query: &ModelSearchQuery, repository: &ModelScopeRepository) -> ModelSearchMatch {
    if query
        .exact_repository()
        .is_some_and(|requested| requested.as_str().eq_ignore_ascii_case(repository.as_str()))
    {
        ModelSearchMatch::ExactRepository
    } else if repository
        .as_str()
        .rsplit('/')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case(query.text.trim()))
    {
        ModelSearchMatch::ExactName
    } else {
        ModelSearchMatch::Related
    }
}

fn source_failure(error: anyhow::Error) -> ModelSourceError {
    error
        .downcast_ref::<ModelSourceError>()
        .cloned()
        .unwrap_or(ModelSourceError {
            code: LifecycleErrorCode::SourcePartialFailure,
            http_status: None,
        })
}

fn timeout_failure() -> ModelSourceError {
    ModelSourceError {
        code: LifecycleErrorCode::SourceTimeout,
        http_status: None,
    }
}

fn coverage_for(
    queries: &[CatalogQueryTrace],
    candidates: &[ModelDiscoveryCandidate],
    exact_repository: &ExactRepositoryLookup,
    limited: bool,
) -> DiscoveryCoverage {
    let failed = queries.iter().any(|trace| trace.failure.is_some())
        || candidates
            .iter()
            .any(|candidate| matches!(candidate.source, RepositoryDiscoveryState::Failed { .. }))
        || matches!(exact_repository, ExactRepositoryLookup::Failed { failure, .. } if failure.code != LifecycleErrorCode::SourceNotFound);
    let observed = queries.iter().any(|trace| trace.fetched_pages > 0) || !candidates.is_empty();
    if failed && !observed {
        DiscoveryCoverage::Unavailable
    } else if failed {
        DiscoveryCoverage::Partial
    } else if limited || queries.iter().any(|trace| !trace.complete) {
        DiscoveryCoverage::Limited
    } else {
        DiscoveryCoverage::Complete
    }
}

async fn inspect_before(
    provider: &ModelScopeProvider,
    repository: ModelScopeRepository,
    deadline: Instant,
    token: Option<&str>,
) -> std::result::Result<RepositoryDiscoveryFacts, ModelSourceError> {
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(20));
    if remaining.is_zero() {
        return Err(timeout_failure());
    }
    match tokio::time::timeout(
        remaining,
        provider.inspect_repository(repository, "HEAD", token),
    )
    .await
    {
        Ok(Ok(inspection)) => {
            RepositoryDiscoveryFacts::from_inspection(inspection).map_err(source_failure)
        }
        Ok(Err(error)) => Err(source_failure(error)),
        Err(_) => Err(timeout_failure()),
    }
}

impl ModelScopeProvider {
    pub async fn discover_models(
        &self,
        request: ModelDiscoveryRequest,
        token: Option<&str>,
    ) -> Result<ModelDiscoveryResponse> {
        request.validate()?;
        let terms = request.query.terms()?;
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut candidates = BTreeMap::new();
        let exact_repository = if let Some(repository) = request.query.exact_repository() {
            match inspect_before(self, repository.clone(), deadline, token).await {
                Ok(facts) => {
                    candidates.insert(
                        repository.clone(),
                        ModelDiscoveryCandidate {
                            repository: repository.clone(),
                            matched_as: ModelSearchMatch::ExactRepository,
                            catalog: None,
                            source: RepositoryDiscoveryState::Verified { facts },
                        },
                    );
                    ExactRepositoryLookup::Verified { repository }
                }
                Err(failure) => ExactRepositoryLookup::Failed {
                    repository,
                    failure,
                },
            }
        } else {
            ExactRepositoryLookup::NotRequested
        };
        let mut queries = Vec::new();
        let mut used_pages = 0;
        let mut limited = false;
        for term in terms {
            let mut trace = CatalogQueryTrace {
                query: term.clone(),
                fetched_pages: 0,
                total_count: None,
                next_page: Some(1),
                complete: false,
                failure: None,
            };
            let mut page_request = CatalogPageRequest {
                query: term,
                page_number: 1,
                page_size: 50,
            };
            let mut seen = BTreeSet::new();
            loop {
                if used_pages >= request.page_budget
                    || candidates.len() >= usize::from(request.limit)
                {
                    limited = true;
                    break;
                }
                used_pages += 1;
                let remaining = deadline.saturating_duration_since(Instant::now());
                let page = match tokio::time::timeout(
                    remaining,
                    self.search_catalog_page(page_request.clone(), token),
                )
                .await
                {
                    Ok(Ok(page)) => page,
                    Ok(Err(error)) => {
                        trace.failure = Some(source_failure(error));
                        break;
                    }
                    Err(_) => {
                        trace.failure = Some(timeout_failure());
                        break;
                    }
                };
                trace.fetched_pages += 1;
                if trace
                    .total_count
                    .is_some_and(|total| total != page.total_count)
                {
                    trace.failure = Some(ModelSourceError {
                        code: LifecycleErrorCode::SourcePartialFailure,
                        http_status: None,
                    });
                    break;
                }
                trace.total_count = Some(page.total_count);
                let next = page.next_request();
                let has_more = page.has_more();
                for model in page.models {
                    if !seen.insert(model.repository.clone()) {
                        trace.failure = Some(ModelSourceError {
                            code: LifecycleErrorCode::SourcePartialFailure,
                            http_status: None,
                        });
                        break;
                    }
                    if let Some(candidate) = candidates.get_mut(&model.repository) {
                        candidate.catalog = Some(model);
                    } else if candidates.len() < usize::from(request.limit) {
                        candidates.insert(
                            model.repository.clone(),
                            ModelDiscoveryCandidate {
                                matched_as: classify_match(&request.query, &model.repository),
                                repository: model.repository.clone(),
                                catalog: Some(model),
                                source: RepositoryDiscoveryState::Unverified,
                            },
                        );
                    } else {
                        limited = true;
                    }
                }
                if trace.failure.is_some() {
                    break;
                }
                trace.next_page = next.as_ref().map(|next| next.page_number);
                if !has_more {
                    trace.complete = true;
                    break;
                }
                if let Some(next) = next {
                    page_request = next;
                } else {
                    limited = true;
                    break;
                }
            }
            queries.push(trace);
        }
        let mut candidates = candidates.into_values().collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.matched_as
                .cmp(&right.matched_as)
                .then_with(|| left.repository.cmp(&right.repository))
        });
        let selected = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.source == RepositoryDiscoveryState::Unverified)
            .take(usize::from(request.verification_limit))
            .map(|(index, candidate)| (index, candidate.repository.clone()))
            .collect::<Vec<_>>();
        let results = stream::iter(selected)
            .map(|(index, repository)| async move {
                (
                    index,
                    inspect_before(self, repository, deadline, token).await,
                )
            })
            .buffer_unordered(3)
            .collect::<Vec<_>>()
            .await;
        for (index, state) in results {
            candidates[index].source = match state {
                Ok(facts) => RepositoryDiscoveryState::Verified { facts },
                Err(failure) => RepositoryDiscoveryState::Failed { failure },
            };
        }
        let coverage = coverage_for(&queries, &candidates, &exact_repository, limited);
        Ok(ModelDiscoveryResponse {
            request,
            coverage,
            exact_repository,
            queries,
            candidates,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::ModelSearchKind;

    #[test]
    fn exact_matches_preserve_repository_identity_and_never_promote_related_names() {
        let query = ModelSearchQuery {
            text: "owner/real-model".into(),
            kind: ModelSearchKind::Name,
        };
        assert_eq!(
            classify_match(
                &query,
                &ModelScopeRepository::parse("owner/real-model").unwrap()
            ),
            ModelSearchMatch::ExactRepository
        );
        assert_eq!(
            classify_match(
                &query,
                &ModelScopeRepository::parse("other/real-model").unwrap()
            ),
            ModelSearchMatch::Related
        );
        let query = ModelSearchQuery {
            text: "real-model".into(),
            kind: ModelSearchKind::Name,
        };
        assert_eq!(
            classify_match(
                &query,
                &ModelScopeRepository::parse("owner/real-model").unwrap()
            ),
            ModelSearchMatch::ExactName
        );
        assert_eq!(
            classify_match(
                &query,
                &ModelScopeRepository::parse("owner/real-model-GGUF").unwrap()
            ),
            ModelSearchMatch::Related
        );
    }

    #[test]
    fn failed_partial_and_bounded_searches_do_not_become_empty_complete_results() {
        let mut trace = CatalogQueryTrace {
            query: "actual query".into(),
            fetched_pages: 0,
            total_count: None,
            next_page: Some(1),
            complete: false,
            failure: Some(timeout_failure()),
        };
        assert_eq!(
            coverage_for(
                &[trace.clone()],
                &[],
                &ExactRepositoryLookup::NotRequested,
                false
            ),
            DiscoveryCoverage::Unavailable
        );
        trace.fetched_pages = 1;
        assert_eq!(
            coverage_for(
                &[trace.clone()],
                &[],
                &ExactRepositoryLookup::NotRequested,
                false
            ),
            DiscoveryCoverage::Partial
        );
        trace.failure = None;
        assert_eq!(
            coverage_for(
                &[trace.clone()],
                &[],
                &ExactRepositoryLookup::NotRequested,
                false
            ),
            DiscoveryCoverage::Limited
        );
        assert_eq!(
            coverage_for(
                &[trace.clone()],
                &[],
                &ExactRepositoryLookup::NotRequested,
                true
            ),
            DiscoveryCoverage::Limited
        );
        trace.complete = true;
        trace.total_count = Some(0);
        trace.next_page = None;
        assert_eq!(
            coverage_for(&[trace], &[], &ExactRepositoryLookup::NotRequested, false),
            DiscoveryCoverage::Complete
        );
    }

    #[test]
    fn untyped_source_failures_never_publish_underlying_messages() {
        let failure = source_failure(anyhow::anyhow!("secret remote body"));
        assert_eq!(failure.code, LifecycleErrorCode::SourcePartialFailure);
        assert!(!serde_json::to_string(&failure)
            .unwrap()
            .contains("secret remote body"));
    }
}
