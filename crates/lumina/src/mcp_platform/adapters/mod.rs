mod external;
mod managed;
mod manual_stdio;
mod remote_http;

pub use external::{DockerAdapter, GitDevDistributionAdapter};
pub use managed::{BinaryArchiveAdapter, NpmAdapter, PythonWheelAdapter};
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
        let code =
            if decision.reasons.iter().any(|reason| {
                reason.code == super::policy::PolicyReasonCode::DevelopmentModeRequired
            }) {
                McpPlatformErrorCode::DevelopmentModeRequired
            } else {
                McpPlatformErrorCode::PolicyDenied
            };
        return Err(McpPlatformError::new(
            code,
            "manifest planning was denied by policy",
        ));
    }

    match manifest.manifest().distribution {
        Distribution::RemoteHttp => RemoteHttpAdapter.plan(manifest, context),
        Distribution::ManualStdio { .. } => ManualStdioAdapter.plan(manifest, context),
        Distribution::Npm { .. } if context.operation != super::policy::PlanOperation::Register => {
            NpmAdapter.plan(manifest, context)
        }
        Distribution::PythonWheel { .. }
            if context.operation != super::policy::PlanOperation::Register =>
        {
            PythonWheelAdapter.plan(manifest, context)
        }
        Distribution::BinaryArchive { .. }
            if context.operation != super::policy::PlanOperation::Register =>
        {
            BinaryArchiveAdapter.plan(manifest, context)
        }
        Distribution::Npm { .. }
        | Distribution::PythonWheel { .. }
        | Distribution::BinaryArchive { .. } => Err(McpPlatformError::new(
            McpPlatformErrorCode::NotImplementedForPhase,
            "managed distribution registration is unavailable; create an install plan",
        )),
        Distribution::Docker { .. } => DockerAdapter.plan(manifest, context),
        Distribution::GitDev { .. } => GitDevDistributionAdapter.plan(manifest, context),
    }
}
