use std::collections::BTreeMap;

use super::domain::TrustTier;
use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::manifest::{
    parse_manifest, Architecture, Distribution, ManifestProof, Platform, VerifiedManifest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatibilityTarget {
    pub platform: Platform,
    pub arch: Architecture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogCompatibility {
    Compatible,
    Incompatible,
}

#[derive(Debug, Clone, Default)]
pub struct CatalogFilter {
    pub query: Option<String>,
    pub trust_tiers: Vec<TrustTier>,
    pub compatibility: Option<CatalogCompatibility>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogInsertOutcome {
    Inserted,
    IdempotentReplay,
}

#[derive(Debug, Clone)]
pub struct CatalogEntry {
    source: String,
    trust_tier: TrustTier,
    proof: ManifestProof,
    verified: VerifiedManifest,
}

impl CatalogEntry {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn id(&self) -> &str {
        &self.verified.manifest().id
    }

    pub fn version(&self) -> &str {
        self.verified.manifest().version.as_str()
    }

    pub const fn trust_tier(&self) -> TrustTier {
        self.trust_tier
    }

    pub fn digest(&self) -> &str {
        self.verified.digest()
    }

    pub fn proof(&self) -> &ManifestProof {
        &self.proof
    }

    pub fn verified_manifest(&self) -> &VerifiedManifest {
        &self.verified
    }

    pub fn compatibility(&self, target: CompatibilityTarget) -> CatalogCompatibility {
        compatibility(&self.verified, target)
    }
}

#[derive(Debug, Default)]
pub struct VerifiedManifestCollection {
    entries: BTreeMap<(String, String, String), CatalogEntry>,
}

impl VerifiedManifestCollection {
    pub fn insert(
        &mut self,
        source: impl Into<String>,
        trust_tier: TrustTier,
        proof: ManifestProof,
        bytes: &[u8],
    ) -> McpPlatformResult<CatalogInsertOutcome> {
        let source = source.into();
        let verified = parse_manifest(bytes)?;
        if let ManifestProof::Catalog {
            declared_manifest_digest,
            ..
        } = &proof
        {
            if declared_manifest_digest != verified.digest() {
                return Err(McpPlatformError::new(
                    McpPlatformErrorCode::InvalidDigest,
                    "catalog manifest digest does not match canonical manifest content",
                ));
            }
        }

        let key = (
            source.clone(),
            verified.manifest().id.clone(),
            verified.manifest().version.as_str().to_string(),
        );
        if let Some(existing) = self.entries.get(&key) {
            return if existing.digest() == verified.digest() {
                Ok(CatalogInsertOutcome::IdempotentReplay)
            } else {
                Err(McpPlatformError::new(
                    McpPlatformErrorCode::ManifestConflict,
                    "catalog identity already has different manifest content",
                ))
            };
        }

        self.entries.insert(
            key,
            CatalogEntry {
                source,
                trust_tier,
                proof,
                verified,
            },
        );
        Ok(CatalogInsertOutcome::Inserted)
    }

    pub fn detail(
        &self,
        source: &str,
        id: &str,
        version: &str,
    ) -> McpPlatformResult<&CatalogEntry> {
        self.entries
            .get(&(source.to_string(), id.to_string(), version.to_string()))
            .ok_or_else(|| {
                McpPlatformError::new(
                    McpPlatformErrorCode::NotFound,
                    "catalog manifest was not found",
                )
            })
    }

    pub fn list(&self, filter: &CatalogFilter, target: CompatibilityTarget) -> Vec<&CatalogEntry> {
        let normalized_query = filter.query.as_deref().map(str::to_ascii_lowercase);
        self.entries
            .values()
            .filter(|entry| {
                filter.trust_tiers.is_empty() || filter.trust_tiers.contains(&entry.trust_tier)
            })
            .filter(|entry| {
                normalized_query.as_ref().is_none_or(|query| {
                    let manifest = entry.verified.manifest();
                    manifest.id.to_ascii_lowercase().contains(query)
                        || manifest.name.to_ascii_lowercase().contains(query)
                        || manifest.description.to_ascii_lowercase().contains(query)
                })
            })
            .filter(|entry| {
                filter
                    .compatibility
                    .is_none_or(|expected| entry.compatibility(target) == expected)
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn compatibility(verified: &VerifiedManifest, target: CompatibilityTarget) -> CatalogCompatibility {
    match &verified.manifest().distribution {
        Distribution::ManualStdio { platforms, .. } => {
            if platforms.is_empty()
                || platforms.contains(&Platform::Any)
                || platforms.contains(&target.platform)
            {
                CatalogCompatibility::Compatible
            } else {
                CatalogCompatibility::Incompatible
            }
        }
        Distribution::Npm { artifacts, .. }
        | Distribution::PythonWheel { artifacts, .. }
        | Distribution::BinaryArchive { artifacts, .. } => {
            if artifacts.iter().any(|artifact| {
                (artifact.platform == Platform::Any || artifact.platform == target.platform)
                    && (artifact.arch == Architecture::Any || artifact.arch == target.arch)
            }) {
                CatalogCompatibility::Compatible
            } else {
                CatalogCompatibility::Incompatible
            }
        }
        Distribution::RemoteHttp | Distribution::Docker { .. } | Distribution::GitDev { .. } => {
            CatalogCompatibility::Compatible
        }
    }
}
