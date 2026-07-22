pub mod adapters;
pub mod catalog;
pub mod domain;
pub mod error;
pub(crate) mod external_distribution;
pub mod health;
pub mod lifecycle;
pub mod managed_distribution;
pub mod managed_remote;
pub mod manifest;
pub mod plan;
pub mod policy;
pub mod repository;
pub mod service;
pub mod task;
pub mod task_runner;

pub use catalog::{
    CatalogCompatibility, CatalogEntry, CatalogFilter, CatalogInsertOutcome, CompatibilityTarget,
    VerifiedManifestCollection,
};
pub use domain::*;
pub use error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
pub use external_distribution::SupplyChainEvidence;
pub use health::{run_bounded_health_session, ProductionHealthCheckAdapter};
pub use lifecycle::*;
pub use managed_distribution::*;
pub use managed_remote::*;
pub use manifest::{
    parse_manifest, Distribution, Manifest, ManifestProof, SignatureEvidence, VerifiedManifest,
};
pub use plan::*;
pub use policy::*;
pub use repository::*;
pub use service::*;
pub use task::*;
pub use task_runner::TaskRunner;
