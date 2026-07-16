use super::DistributionAdapter;
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::manifest::{Distribution, Transport, VerifiedManifest};
use crate::mcp_platform::plan::{
    AdapterIdentity, ConnectionProjection, EffectSummary, InstallationPlan, PlanStep, PlanWarning,
    RequiredConfirmation,
};
use crate::mcp_platform::policy::{
    evaluate_manifest_policy, PlanOperation, PolicyContext, PolicyOutcome,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ManualStdioAdapter;

impl DistributionAdapter for ManualStdioAdapter {
    fn id(&self) -> &'static str {
        "manual_stdio"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn plan(
        &self,
        verified: &VerifiedManifest,
        context: &PolicyContext,
    ) -> McpPlatformResult<InstallationPlan> {
        if context.operation != PlanOperation::Register {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::OperationNotSupported,
                "manual stdio supports registration plans in phase 2A",
            ));
        }

        let manifest = verified.manifest();
        let Distribution::ManualStdio { entrypoint, .. } = &manifest.distribution else {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::UnknownAdapter,
                "adapter does not match the manifest distribution",
            ));
        };
        let Transport::Stdio {
            startup_timeout_seconds,
        } = &manifest.transport
        else {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::TransportMismatch,
                "manual stdio requires stdio transport",
            ));
        };

        let policy = evaluate_manifest_policy(manifest, context);
        if policy.is_denied() {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::PolicyDenied,
                "manifest planning was denied by policy",
            ));
        }

        let permission_ids = manifest
            .permissions
            .iter()
            .map(|permission| permission.id.clone())
            .collect::<Vec<_>>();
        let required_confirmations = confirmations(manifest, &policy);
        let step = PlanStep::RegisterStdio {
            executable: entrypoint.executable.clone(),
            args: entrypoint.args.clone(),
            environment_keys: entrypoint.environment_keys.clone(),
            cwd: entrypoint.cwd.clone(),
            startup_timeout_seconds: *startup_timeout_seconds,
            health_check: manifest.health_check.clone(),
            permission_ids: permission_ids.clone(),
        };
        let effects = EffectSummary {
            registers_connection: true,
            downloads_artifacts: false,
            writes_files: false,
            removes_files: false,
            requires_process_spawn: true,
            network_origins: Vec::new(),
            permission_ids,
        };
        let projection = ConnectionProjection::ManualStdio {
            name: manifest.name.clone(),
            description: manifest.description.clone(),
            executable: entrypoint.executable.clone(),
            args: entrypoint.args.clone(),
            environment_keys: entrypoint.environment_keys.clone(),
            cwd: entrypoint.cwd.clone(),
            timeout_seconds: *startup_timeout_seconds,
        };

        InstallationPlan::new(
            manifest,
            verified.digest().to_string(),
            AdapterIdentity {
                id: self.id().to_string(),
                version: self.version().to_string(),
            },
            context.trust_tier,
            context.operation,
            vec![step],
            effects,
            vec![PlanWarning::DefaultDisabled, PlanWarning::RegistrationOnly],
            required_confirmations,
            projection,
            policy,
        )
    }
}

fn confirmations(
    manifest: &crate::mcp_platform::manifest::Manifest,
    policy: &crate::mcp_platform::policy::PolicyDecision,
) -> Vec<RequiredConfirmation> {
    let mut confirmations = if policy.outcome == PolicyOutcome::NeedsConfirmation {
        policy
            .reasons
            .iter()
            .map(|reason| RequiredConfirmation::Policy {
                reason_code: reason.code,
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    confirmations.extend(
        manifest
            .permissions
            .iter()
            .filter(|permission| permission.required)
            .map(|permission| RequiredConfirmation::Permission {
                permission_id: permission.id.clone(),
            }),
    );
    confirmations
}
