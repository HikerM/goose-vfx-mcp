use std::time::{SystemTime, UNIX_EPOCH};

use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::manifest::{ManifestProof, VerifiedManifest};
use crate::mcp_platform::TrustTier;

use super::dto::ManualStdioSource;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub trait IdGenerator: Send + Sync {
    fn next_id(&self, prefix: &str) -> String;
}

pub trait ManualStdioProvider: Send + Sync {
    fn list_sources(&self) -> McpPlatformResult<Vec<ManualStdioSource>>;
    fn resolve(&self, source_id: &str) -> McpPlatformResult<ResolvedManualStdioSource>;
}

pub trait RemoteHttpNetworkPolicy: Send + Sync {
    fn validate_endpoint(&self, endpoint: &str) -> McpPlatformResult<()>;
}

#[derive(Debug, Default)]
pub struct UnavailableRemoteHttpNetworkPolicy;

impl RemoteHttpNetworkPolicy for UnavailableRemoteHttpNetworkPolicy {
    fn validate_endpoint(&self, _endpoint: &str) -> McpPlatformResult<()> {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::RemoteHttpPolicyUnavailable,
            "no verified remote HTTP connection policy is configured",
        ))
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedManualStdioSource {
    pub verified: VerifiedManifest,
    pub proof: ManifestProof,
    pub trust_tier: TrustTier,
}

#[derive(Debug, Default)]
pub struct UnsupportedManualStdioProvider;

impl ManualStdioProvider for UnsupportedManualStdioProvider {
    fn list_sources(&self) -> McpPlatformResult<Vec<ManualStdioSource>> {
        Ok(Vec::new())
    }

    fn resolve(&self, _source_id: &str) -> McpPlatformResult<ResolvedManualStdioSource> {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::ManualStdioProviderUnavailable,
            "no safe manual stdio provider is configured",
        ))
    }
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or_default()
    }
}

#[derive(Debug, Default)]
pub struct UuidGenerator;

impl IdGenerator for UuidGenerator {
    fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}_{}", uuid::Uuid::now_v7())
    }
}
