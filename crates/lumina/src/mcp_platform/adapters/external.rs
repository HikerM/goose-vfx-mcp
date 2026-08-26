use serde::Serialize;
use url::Url;

use super::DistributionAdapter;
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::manifest::{
    digest_serializable, Distribution, GitDevAdapter, Transport, VerifiedManifest,
};
use crate::mcp_platform::plan::{
    AdapterIdentity, ConnectionProjection, EffectSummary, InstallationPlan, PlanStep, PlanWarning,
    RequiredConfirmation,
};
use crate::mcp_platform::policy::{
    evaluate_manifest_policy, PlanOperation, PolicyContext, PolicyOutcome,
};

const ADAPTER_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, Default)]
pub struct DockerAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct GitDevDistributionAdapter;

impl DistributionAdapter for DockerAdapter {
    fn id(&self) -> &'static str {
        "docker"
    }

    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn plan(
        &self,
        verified: &VerifiedManifest,
        context: &PolicyContext,
    ) -> McpPlatformResult<InstallationPlan> {
        let Distribution::Docker {
            image,
            digest,
            entrypoint,
            mounts,
        } = &verified.manifest().distribution
        else {
            return Err(unknown_adapter());
        };
        ensure_managed_operation(context.operation)?;
        ensure_empty_hosts(verified)?;
        ensure_stdio(verified)?;
        if !context.docker_policy_allowed {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::DaemonPolicyDenied,
                "Docker daemon use is denied by server policy",
            ));
        }
        if !context.docker_available {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::DockerUnavailable,
                "Docker capability is unavailable",
            ));
        }
        if !mounts.is_empty() {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::OperationNotSupported,
                "Docker mounts require an opaque filesystem grant lifecycle unavailable in manifest v1",
            ));
        }
        let policy = approved_policy(verified, context)?;
        let mount_plan_digest = digest_serializable(mounts)?;
        let registry = image.split('/').next().ok_or_else(unsafe_distribution)?;
        let mut warnings = vec![
            PlanWarning::DefaultDisabled,
            PlanWarning::ImmutableContainerImage,
        ];
        if mounts.iter().any(|mount| !mount.read_only) {
            warnings.push(PlanWarning::WritableContainerMount);
        }
        InstallationPlan::new(
            verified.manifest(),
            verified.digest().to_string(),
            AdapterIdentity {
                id: self.id().to_string(),
                version: ADAPTER_VERSION.to_string(),
            },
            context.trust_tier,
            context.operation,
            vec![PlanStep::AcquireDockerDistribution {
                image: image.clone(),
                digest: digest.clone(),
                mounts: mounts.clone(),
                mount_plan_digest,
            }],
            EffectSummary {
                registers_connection: true,
                downloads_artifacts: true,
                writes_files: true,
                removes_files: matches!(
                    context.operation,
                    PlanOperation::Update | PlanOperation::Repair
                ),
                requires_process_spawn: true,
                network_origins: vec![format!("https://{registry}")],
                permission_ids: verified
                    .manifest()
                    .permissions
                    .iter()
                    .map(|permission| permission.id.clone())
                    .collect(),
            },
            warnings,
            confirmations(verified, &policy),
            ConnectionProjection::ManagedDockerStdio {
                name: verified.manifest().name.clone(),
                description: verified.manifest().description.clone(),
                executable: "server-owned-docker".to_string(),
                args: entrypoint.args.clone(),
                cwd: None,
                timeout_seconds: stdio_timeout(verified)?,
            },
            policy,
        )
    }
}

impl DistributionAdapter for GitDevDistributionAdapter {
    fn id(&self) -> &'static str {
        "git_dev"
    }

    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn plan(
        &self,
        verified: &VerifiedManifest,
        context: &PolicyContext,
    ) -> McpPlatformResult<InstallationPlan> {
        let Distribution::GitDev {
            repository,
            commit,
            subdirectory,
            adapter,
            entrypoint,
        } = &verified.manifest().distribution
        else {
            return Err(unknown_adapter());
        };
        ensure_managed_operation(context.operation)?;
        ensure_empty_hosts(verified)?;
        ensure_stdio(verified)?;
        if !context.git_available {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::GitUnavailable,
                "Git capability is unavailable",
            ));
        }
        if *adapter == GitDevAdapter::Docker {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::OperationNotSupported,
                "git development Docker requires an immutable image digest that manifest v1 cannot express",
            ));
        }
        let policy = approved_policy(verified, context)?;
        let parsed = Url::parse(repository).map_err(|_| git_origin_denied())?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !canonical_repository_path(&parsed)
            || denied_git_host(parsed.host_str().unwrap_or_default())
        {
            return Err(git_origin_denied());
        }
        let repository_origin = match parsed.port() {
            Some(port) => format!("https://{}:{port}", parsed.host_str().unwrap_or_default()),
            None => format!("https://{}", parsed.host_str().unwrap_or_default()),
        };
        let acquisition_digest = digest_serializable(&GitAcquisitionAuthority {
            repository,
            commit,
            subdirectory,
            adapter: *adapter,
        })?;
        InstallationPlan::new(
            verified.manifest(),
            verified.digest().to_string(),
            AdapterIdentity {
                id: self.id().to_string(),
                version: ADAPTER_VERSION.to_string(),
            },
            context.trust_tier,
            context.operation,
            vec![PlanStep::AcquireGitDevDistribution {
                repository_origin: repository_origin.clone(),
                repository: repository.clone(),
                commit: commit.clone(),
                subdirectory: subdirectory.clone(),
                underlying_adapter: *adapter,
                acquisition_digest,
            }],
            EffectSummary {
                registers_connection: true,
                downloads_artifacts: true,
                writes_files: true,
                removes_files: matches!(
                    context.operation,
                    PlanOperation::Update | PlanOperation::Repair
                ),
                requires_process_spawn: true,
                network_origins: vec![repository_origin],
                permission_ids: verified
                    .manifest()
                    .permissions
                    .iter()
                    .map(|permission| permission.id.clone())
                    .collect(),
            },
            vec![
                PlanWarning::DefaultDisabled,
                PlanWarning::DevelopmentSourcePinnedCommitNoBuild,
            ],
            confirmations(verified, &policy),
            ConnectionProjection::ManagedStdio {
                name: verified.manifest().name.clone(),
                description: verified.manifest().description.clone(),
                executable: "server-owned-gitdev-runtime".to_string(),
                args: entrypoint.args.clone(),
                environment_keys: entrypoint.environment_keys.clone(),
                cwd: None,
                timeout_seconds: stdio_timeout(verified)?,
            },
            policy,
        )
    }
}

#[derive(Serialize)]
struct GitAcquisitionAuthority<'a> {
    repository: &'a str,
    commit: &'a str,
    subdirectory: &'a Option<String>,
    adapter: GitDevAdapter,
}

fn ensure_managed_operation(operation: PlanOperation) -> McpPlatformResult<()> {
    if matches!(
        operation,
        PlanOperation::Install | PlanOperation::Update | PlanOperation::Repair
    ) {
        Ok(())
    } else {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::OperationNotSupported,
            "external managed distribution operation is not supported",
        ))
    }
}

fn ensure_empty_hosts(verified: &VerifiedManifest) -> McpPlatformResult<()> {
    if verified.manifest().host_integrations.is_empty() {
        Ok(())
    } else {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::OperationNotSupported,
            "managed package host integration is not supported in this phase",
        ))
    }
}

fn ensure_stdio(verified: &VerifiedManifest) -> McpPlatformResult<()> {
    if matches!(verified.manifest().transport, Transport::Stdio { .. }) {
        Ok(())
    } else {
        Err(McpPlatformError::new(
            McpPlatformErrorCode::TransportMismatch,
            "external managed distributions require stdio transport",
        ))
    }
}

fn stdio_timeout(verified: &VerifiedManifest) -> McpPlatformResult<Option<u64>> {
    match verified.manifest().transport {
        Transport::Stdio {
            startup_timeout_seconds,
        } => Ok(startup_timeout_seconds),
        _ => Err(unsafe_distribution()),
    }
}

fn approved_policy(
    verified: &VerifiedManifest,
    context: &PolicyContext,
) -> McpPlatformResult<crate::mcp_platform::policy::PolicyDecision> {
    let policy = evaluate_manifest_policy(verified.manifest(), context);
    if policy.is_denied() {
        let code = if policy.reasons.iter().any(|reason| {
            reason.code == crate::mcp_platform::policy::PolicyReasonCode::DevelopmentModeRequired
        }) {
            McpPlatformErrorCode::DevelopmentModeRequired
        } else {
            McpPlatformErrorCode::PolicyDenied
        };
        Err(McpPlatformError::new(
            code,
            "manifest planning was denied by policy",
        ))
    } else {
        Ok(policy)
    }
}

fn canonical_repository_path(url: &Url) -> bool {
    let path = url.path();
    if path == "/" || path.is_empty() || path.contains("//") || path.contains('%') {
        return false;
    }
    let components = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    !components.is_empty()
        && components.iter().all(|component| {
            !component.is_empty()
                && *component != "."
                && *component != ".."
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}

fn denied_git_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok()
}

fn confirmations(
    verified: &VerifiedManifest,
    policy: &crate::mcp_platform::policy::PolicyDecision,
) -> Vec<RequiredConfirmation> {
    let mut values = if policy.outcome == PolicyOutcome::NeedsConfirmation {
        policy
            .reasons
            .iter()
            .map(|reason| RequiredConfirmation::Policy {
                reason_code: reason.code,
            })
            .collect()
    } else {
        Vec::new()
    };
    values.extend(
        verified
            .manifest()
            .permissions
            .iter()
            .filter(|permission| permission.required)
            .map(|permission| RequiredConfirmation::Permission {
                permission_id: permission.id.clone(),
            }),
    );
    values
}

const fn unknown_adapter() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::UnknownAdapter,
        "adapter does not match the manifest distribution",
    )
}

const fn unsafe_distribution() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PolicyDenied,
        "external distribution is outside the approved supply-chain policy",
    )
}

const fn git_origin_denied() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::GitOriginDenied,
        "Git origin is outside the HTTPS origin policy",
    )
}
