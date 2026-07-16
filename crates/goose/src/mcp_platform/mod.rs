pub mod adapters;
pub mod catalog;
pub mod domain;
pub mod error;
pub mod manifest;
pub mod plan;
pub mod policy;
pub mod repository;
pub mod task;

pub use catalog::{
    CatalogCompatibility, CatalogEntry, CatalogFilter, CatalogInsertOutcome, CompatibilityTarget,
    VerifiedManifestCollection,
};
pub use domain::*;
pub use error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
pub use manifest::{
    parse_manifest, Distribution, Manifest, ManifestProof, SignatureEvidence, VerifiedManifest,
};
pub use plan::*;
pub use policy::*;
pub use repository::*;
pub use task::*;
