//! Fail-closed data contract for a future official runtime catalog.
//!
//! This module deliberately has no network, cache, activation, command, or
//! environment surface. A future bootstrapper must supply cryptographically
//! verified evidence before it can construct this contract.

use std::collections::BTreeSet;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::manifest::ExactVersion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mcp_platform) enum OfficialRuntimeFamily {
    Node,
    Python,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp_platform) struct RuntimeTarget {
    pub platform: String,
    pub architecture: String,
    pub abi: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp_platform) struct RuntimeArtifactContract {
    pub family: OfficialRuntimeFamily,
    pub version: ExactVersion,
    pub target: RuntimeTarget,
    pub sha256: String,
    pub length_bytes: u64,
    pub immutable_origin_id: String,
    pub provenance_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp_platform) struct RootTrustPolicy {
    pub root_key_ids: BTreeSet<String>,
    pub threshold: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp_platform) struct VerifiedRootRotationProof {
    pub prior_root_catalog_digest: String,
    pub next_root_key_ids: BTreeSet<String>,
    pub verified_signer_key_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp_platform) struct RevocationProof {
    pub catalog_identity: String,
    pub revocation_digest: String,
    pub checked_at_ms: u64,
    pub valid_until_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp_platform) struct RuntimeCatalogContract {
    pub canonical_identity: String,
    pub canonical_digest: String,
    pub expires_at_ms: u64,
    pub trust_policy: RootTrustPolicy,
    pub root_rotation: Option<VerifiedRootRotationProof>,
    pub revocation_proof: Option<RevocationProof>,
    pub artifacts: Vec<RuntimeArtifactContract>,
}

impl RuntimeCatalogContract {
    /// Validates already-authenticated catalog evidence. Any missing proof is
    /// rejected rather than deferred to a downloader or activation phase.
    pub(in crate::mcp_platform) fn validate(&self, now_ms: u64) -> McpPlatformResult<()> {
        if !valid_identity(&self.canonical_identity)
            || !sha256(&self.canonical_digest)
            || self.expires_at_ms <= now_ms
            || self.artifacts.is_empty()
        {
            return Err(contract_error());
        }
        self.validate_trust()?;
        self.validate_revocation(now_ms)?;
        for artifact in &self.artifacts {
            if !valid_target(&artifact.target)
                || !sha256(&artifact.sha256)
                || artifact.length_bytes == 0
                || !valid_identity(&artifact.immutable_origin_id)
                || !sha256(&artifact.provenance_digest)
            {
                return Err(contract_error());
            }
        }
        Ok(())
    }

    fn validate_trust(&self) -> McpPlatformResult<()> {
        let policy = &self.trust_policy;
        if policy.root_key_ids.is_empty()
            || policy.threshold == 0
            || policy.threshold > policy.root_key_ids.len()
            || policy.root_key_ids.iter().any(|id| !valid_identity(id))
        {
            return Err(contract_error());
        }
        let Some(rotation) = &self.root_rotation else {
            return Err(contract_error());
        };
        if !sha256(&rotation.prior_root_catalog_digest)
            || rotation.next_root_key_ids != policy.root_key_ids
            || rotation.verified_signer_key_ids.len() < policy.threshold
            || !rotation
                .verified_signer_key_ids
                .iter()
                .all(|key| rotation.next_root_key_ids.contains(key))
        {
            return Err(contract_error());
        }
        Ok(())
    }

    fn validate_revocation(&self, now_ms: u64) -> McpPlatformResult<()> {
        let Some(proof) = &self.revocation_proof else {
            return Err(contract_error());
        };
        if proof.catalog_identity != self.canonical_identity
            || !sha256(&proof.revocation_digest)
            || proof.checked_at_ms > now_ms
            || proof.valid_until_ms <= now_ms
        {
            return Err(contract_error());
        }
        Ok(())
    }
}

fn valid_target(value: &RuntimeTarget) -> bool {
    [&value.platform, &value.architecture, &value.abi]
        .into_iter()
        .all(|part| valid_identity(part))
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

const fn contract_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityUnavailable,
        "runtime catalog contract evidence is incomplete or invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> String {
        "a".repeat(64)
    }

    fn valid_catalog() -> RuntimeCatalogContract {
        let keys = BTreeSet::from(["root-a".to_string(), "root-b".to_string()]);
        RuntimeCatalogContract {
            canonical_identity: "lumina.runtime-catalog.stable".to_string(),
            canonical_digest: digest(),
            expires_at_ms: 200,
            trust_policy: RootTrustPolicy {
                root_key_ids: keys.clone(),
                threshold: 2,
            },
            root_rotation: Some(VerifiedRootRotationProof {
                prior_root_catalog_digest: digest(),
                next_root_key_ids: keys.clone(),
                verified_signer_key_ids: keys,
            }),
            revocation_proof: Some(RevocationProof {
                catalog_identity: "lumina.runtime-catalog.stable".to_string(),
                revocation_digest: digest(),
                checked_at_ms: 100,
                valid_until_ms: 150,
            }),
            artifacts: vec![RuntimeArtifactContract {
                family: OfficialRuntimeFamily::Node,
                version: ExactVersion::parse("22.1.0").unwrap(),
                target: RuntimeTarget {
                    platform: "windows".to_string(),
                    architecture: "x86_64".to_string(),
                    abi: "msvc".to_string(),
                },
                sha256: digest(),
                length_bytes: 1,
                immutable_origin_id: "node-v22.1.0-win-x64".to_string(),
                provenance_digest: digest(),
            }],
        }
    }

    #[test]
    fn catalog_contract_rejects_missing_artifact_evidence_and_expiry() {
        let mut catalog = valid_catalog();
        catalog.artifacts[0].sha256.clear();
        assert!(catalog.validate(120).is_err());
        let mut catalog = valid_catalog();
        catalog.artifacts[0].length_bytes = 0;
        assert!(catalog.validate(120).is_err());
        let mut catalog = valid_catalog();
        catalog.artifacts[0].immutable_origin_id.clear();
        assert!(catalog.validate(120).is_err());
        assert!(valid_catalog().validate(200).is_err());
    }

    #[test]
    fn catalog_contract_requires_current_revocation_and_threshold_rotation_proof() {
        let mut catalog = valid_catalog();
        catalog.revocation_proof = None;
        assert!(catalog.validate(120).is_err());
        let mut catalog = valid_catalog();
        catalog
            .root_rotation
            .as_mut()
            .unwrap()
            .verified_signer_key_ids = BTreeSet::from(["root-a".to_string()]);
        assert!(catalog.validate(120).is_err());
        let mut catalog = valid_catalog();
        catalog.root_rotation = None;
        assert!(catalog.validate(120).is_err());
    }
}
