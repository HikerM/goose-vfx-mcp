mod manual_stdio;
mod remote_http;

pub use manual_stdio::ManualStdioAdapter;
pub use remote_http::RemoteHttpAdapter;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::manifest::{Distribution, VerifiedManifest};
use super::plan::InstallationPlan;
use super::policy::{evaluate_manifest_policy, PolicyContext};

pub trait DistributionAdapter {
    fn id(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn plan(
        &self,
        manifest: &VerifiedManifest,
        context: &PolicyContext,
    ) -> McpPlatformResult<InstallationPlan>;
}

pub fn plan_for_manifest(
    manifest: &VerifiedManifest,
    context: &PolicyContext,
) -> McpPlatformResult<InstallationPlan> {
    let decision = evaluate_manifest_policy(manifest.manifest(), context);
    if decision.is_denied() {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::PolicyDenied,
            "manifest planning was denied by policy",
        ));
    }

    match manifest.manifest().distribution {
        Distribution::RemoteHttp => RemoteHttpAdapter.plan(manifest, context),
        Distribution::ManualStdio { .. } => ManualStdioAdapter.plan(manifest, context),
        Distribution::Npm { .. }
        | Distribution::PythonWheel { .. }
        | Distribution::BinaryArchive { .. }
        | Distribution::Docker { .. }
        | Distribution::GitDev { .. } => Err(McpPlatformError::new(
            McpPlatformErrorCode::NotImplementedForPhase,
            "distribution planning is not implemented in phase 2A",
        )),
    }
}
