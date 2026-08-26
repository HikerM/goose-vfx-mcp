//! `mcp_platform` only re-exports public-safe profile views.
//!
//! ```rust
//! use lumina::mcp_platform::{
//!     McpProfile, ProfileApplicationTokenView, ProfileApplyConfirmationView, ProfileEntry,
//! };
//!
//! let _profile = McpProfile {
//!     profile_id: "profile".to_string(),
//!     name: "Example".to_string(),
//!     description: "safe public view".to_string(),
//!     revision: 1,
//!     archived: false,
//!     entries: vec![ProfileEntry {
//!         managed_mcp_id: "managed".to_string(),
//!         ordinal: 0,
//!     }],
//!     created_at_ms: 1,
//!     updated_at_ms: 1,
//! };
//! let _confirmation = ProfileApplyConfirmationView {
//!     confirmation_token: "profile_apply_confirmation_token".to_string(),
//! };
//! let _token = ProfileApplicationTokenView {
//!     token: "profile_application_token".to_string(),
//!     plan_id: "plan".to_string(),
//!     profile_id: "profile".to_string(),
//!     profile_revision: 1,
//!     expires_at_ms: 1,
//! };
//! ```
//!
//! ```compile_fail
//! use lumina::mcp_platform::{McpProfile, ProfileApplyConfirmationView};
//!
//! let _profile = McpProfile {
//!     profile_id: "profile".to_string(),
//!     name: "Example".to_string(),
//!     description: "safe public view".to_string(),
//!     revision: 1,
//!     archived: false,
//!     entries: Vec::new(),
//!     created_at_ms: 1,
//!     updated_at_ms: 1,
//! };
//! let _confirmation = ProfileApplyConfirmationView {
//!     confirmation_token: "profile_apply_confirmation_token".to_string(),
//! };
//! let _ = lumina::mcp_platform::extension_source_fingerprint;
//! let _: lumina::mcp_platform::profile::StoredProfileApplyPlan;
//! ```
//!
//! ```compile_fail
//! use lumina::mcp_platform::{
//!     ProfileApplicationTokenView, ProfileApplyConfirmationView, ProfileApplyPlanView,
//! };
//!
//! let _ = ProfileApplyConfirmationView {
//!     confirmation_token: "profile_apply_confirmation_token".to_string(),
//!     confirmation_hash: "hidden".to_string(),
//! };
//! let _ = ProfileApplicationTokenView {
//!     token: "profile_application_token".to_string(),
//!     plan_id: "plan".to_string(),
//!     profile_id: "profile".to_string(),
//!     profile_revision: 1,
//!     expires_at_ms: 1,
//!     token_hash: "hidden".to_string(),
//! };
//! let _ = ProfileApplyPlanView {
//!     plan_id: "plan".to_string(),
//!     profile_id: "profile".to_string(),
//!     profile_revision: 1,
//!     merge_policy: "replace_managed_only".to_string(),
//!     entries: Vec::new(),
//!     expires_at_ms: 1,
//!     confirmation: ProfileApplyConfirmationView {
//!         confirmation_token: "profile_apply_confirmation_token".to_string(),
//!     },
//!     internal_plan_digest: "hidden".to_string(),
//! };
//! ```

/// Kept crate-private and compiled so future contract syntax remains checked without
/// creating a public or runtime integration boundary.
pub(crate) mod adapter_capability_contract;
pub mod adapters;
pub mod catalog;
pub(crate) mod credential_authority;
pub(crate) mod credential_enrollment;
pub(crate) mod credential_runtime_gate;
pub mod domain;
pub(crate) mod effect_fence;
pub mod error;
pub(crate) mod external_distribution;
pub(crate) mod fenced_projection_sink;
pub mod health;
pub(crate) mod https_manifest_fetcher;
pub(crate) mod https_manifest_policy;
pub(crate) mod intake;
pub(crate) mod intake_inspection;
pub(crate) mod intake_inspection_network;
pub(crate) mod intake_remote_inspection;
pub mod lifecycle;
pub mod managed_distribution;
pub mod managed_remote;
pub mod manifest;
pub mod plan;
pub mod policy;
mod profile;
mod projection_runtime;
#[cfg(test)]
mod regression_mcp_platform_3a;
#[cfg(test)]
mod regression_mcp_platform_3b;
#[cfg(test)]
mod regression_mcp_platform_3c;
#[cfg(test)]
mod regression_mcp_platform_4a;
#[cfg(test)]
mod regression_mcp_platform_4b;
#[cfg(all(test, feature = "rustls-tls"))]
mod regression_mcp_platform_4c;
#[cfg(test)]
mod regression_mcp_platform_i2a;
#[cfg(test)]
mod regression_mcp_platform_i2b;
#[cfg(all(test, feature = "rustls-tls"))]
pub(crate) mod remote_https_fixture;
#[cfg(all(test, feature = "rustls-tls"))]
pub(crate) mod remote_https_test_policy;
pub mod repository;
pub(crate) mod runtime_catalog_contract;
pub(crate) mod runtime_control;
pub(crate) mod runtime_fenced_sink_registry;
pub mod service;
pub(crate) mod source_provisioning;
pub(crate) mod storage_domain;
pub mod task;
pub(crate) mod task_runner;
/// Kept crate-private and compiled so future contract syntax remains checked without
/// creating a public or runtime integration boundary.
pub(crate) mod v3_contract;

pub use catalog::{
    CatalogCompatibility, CatalogEntry, CatalogFilter, CatalogInsertOutcome, CompatibilityTarget,
    VerifiedManifestCollection,
};
pub use domain::*;
pub use error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
pub use external_distribution::SupplyChainEvidence;
pub use health::{run_bounded_health_session, ProductionHealthCheckAdapter};
#[cfg(feature = "integration-test-support")]
pub use https_manifest_fetcher::{
    diagnose_integration_https_manifest_fetch, new_integration_https_manifest_fetcher,
    IntegrationHttpsManifestFetchStage,
};
pub use lifecycle::*;
pub use managed_distribution::*;
pub use managed_remote::*;
pub use manifest::{
    parse_manifest, Distribution, Manifest, ManifestProof, ManifestSourceMetadata,
    OriginProvenance, SignatureEvidence, SourceImportKind, SourceRef, UpdateChannel,
    VerifiedManifest, VerifiedSourceDocumentRef,
};
pub use plan::*;
pub use policy::*;
pub(crate) use profile::{
    extension_source_fingerprint, profile_apply_plan_snapshot_digest, profile_auth_evidence_digest,
    profile_policy_evidence_digest, ChangeProfile, ConsumedProfileApplication,
    ProfileApplicationMarker, ProfileCredentialReference, ProfileManagedReference, SaveProfile,
    SaveProfileApplicationToken, SaveProfileApplyConfirmation, SaveProfileApplyPlan,
    StoredProfileApplyConfirmation, StoredProfileApplyPlan, StoredProfileManagedSnapshot,
};
pub use profile::{
    McpProfile, ModelRecommendationCandidate, ProfileApplicationTokenView,
    ProfileApplyConfirmationView, ProfileApplyEntryView, ProfileApplyPlanView, ProfileDraft,
    ProfileDraftCandidate, ProfileEntry, ProfileRevision,
};
pub use repository::*;
pub(crate) use repository::{InMemoryIntegritySigner, IntegritySigner};
pub use service::*;
pub use task::*;
