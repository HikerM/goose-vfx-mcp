use url::Url;

use super::DistributionAdapter;
use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::manifest::{
    Artifact, Distribution, Entrypoint, Transport, VerifiedManifest,
};
use crate::mcp_platform::plan::{
    AdapterIdentity, ConnectionProjection, EffectSummary, InstallationPlan,
    ManagedDependencyPolicy, ManagedDistributionKind, PlanStep, PlanWarning, RequiredConfirmation,
};
use crate::mcp_platform::policy::{
    evaluate_manifest_policy, PlanOperation, PolicyContext, PolicyOutcome,
};

const ADAPTER_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, Default)]
pub struct NpmAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct PythonWheelAdapter;

#[derive(Debug, Clone, Copy, Default)]
pub struct BinaryArchiveAdapter;

impl DistributionAdapter for NpmAdapter {
    fn id(&self) -> &'static str {
        "npm"
    }
    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn plan(
        &self,
        verified: &VerifiedManifest,
        context: &PolicyContext,
    ) -> McpPlatformResult<InstallationPlan> {
        let Distribution::Npm {
            package,
            package_version,
            registry,
            artifacts,
            entrypoint,
        } = &verified.manifest().distribution
        else {
            return Err(unknown_adapter());
        };
        let registry = registry.as_deref().ok_or_else(unsafe_distribution)?;
        if !context.node_available {
            return Err(runtime_incompatible());
        }
        validate_logical_bin_entrypoint(entrypoint)?;
        let artifact = selected_artifact(artifacts, context)?;
        require_same_origin(registry, &artifact.url)?;
        managed_plan(
            self.id(),
            verified,
            context,
            ManagedDistributionKind::Npm,
            Some(package.clone()),
            package_version.as_str(),
            artifact,
            entrypoint,
            None,
        )
    }
}

impl DistributionAdapter for PythonWheelAdapter {
    fn id(&self) -> &'static str {
        "python_wheel"
    }
    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn plan(
        &self,
        verified: &VerifiedManifest,
        context: &PolicyContext,
    ) -> McpPlatformResult<InstallationPlan> {
        let Distribution::PythonWheel {
            package,
            package_version,
            python,
            artifacts,
            entrypoint,
            ..
        } = &verified.manifest().distribution
        else {
            return Err(unknown_adapter());
        };
        let required_minor = python
            .strip_prefix(">=3.")
            .and_then(|value| value.parse::<u8>().ok())
            .ok_or_else(runtime_incompatible)?;
        if context
            .python_major_minor
            .is_none_or(|(major, minor)| major != 3 || minor < required_minor)
        {
            return Err(runtime_incompatible());
        }
        validate_logical_bin_entrypoint(entrypoint)?;
        let artifact = selected_artifact(artifacts, context)?;
        validate_wheel_filename(&artifact.url, package, package_version.as_str(), context)?;
        managed_plan(
            self.id(),
            verified,
            context,
            ManagedDistributionKind::PythonWheel,
            Some(package.clone()),
            package_version.as_str(),
            artifact,
            entrypoint,
            None,
        )
    }
}

impl DistributionAdapter for BinaryArchiveAdapter {
    fn id(&self) -> &'static str {
        "binary_archive"
    }
    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn plan(
        &self,
        verified: &VerifiedManifest,
        context: &PolicyContext,
    ) -> McpPlatformResult<InstallationPlan> {
        let Distribution::BinaryArchive {
            archive_format,
            artifacts,
            entrypoint,
            ..
        } = &verified.manifest().distribution
        else {
            return Err(unknown_adapter());
        };
        let artifact = selected_artifact(artifacts, context)?;
        managed_plan(
            self.id(),
            verified,
            context,
            ManagedDistributionKind::BinaryArchive,
            None,
            verified.manifest().version.as_str(),
            artifact,
            entrypoint,
            Some(*archive_format),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn managed_plan(
    adapter_id: &str,
    verified: &VerifiedManifest,
    context: &PolicyContext,
    kind: ManagedDistributionKind,
    package_name: Option<String>,
    package_version: &str,
    artifact: &Artifact,
    entrypoint: &Entrypoint,
    archive_format: Option<crate::mcp_platform::manifest::ArchiveFormat>,
) -> McpPlatformResult<InstallationPlan> {
    if !verified.manifest().host_integrations.is_empty() {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::OperationNotSupported,
            "managed package host integration is not supported in this phase",
        ));
    }
    if !matches!(
        context.operation,
        PlanOperation::Install | PlanOperation::Update | PlanOperation::Repair
    ) {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::OperationNotSupported,
            "managed distribution operation is not supported",
        ));
    }
    validate_relative_entrypoint(entrypoint)?;
    let source = Url::parse(&artifact.url).map_err(|_| unsafe_distribution())?;
    if source.scheme() != "https"
        || source.host_str().is_none()
        || !source.username().is_empty()
        || source.password().is_some()
    {
        return Err(unsafe_distribution());
    }
    let policy = evaluate_manifest_policy(verified.manifest(), context);
    if policy.is_denied() {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::PolicyDenied,
            "manifest planning was denied by policy",
        ));
    }
    let permission_ids = verified
        .manifest()
        .permissions
        .iter()
        .map(|p| p.id.clone())
        .collect::<Vec<_>>();
    let step = PlanStep::AcquireManagedDistribution {
        kind,
        source_origin: origin(&source)?,
        artifact_url: artifact.url.clone(),
        artifact_digest: artifact.digest.clone(),
        expected_size_bytes: artifact.size_bytes,
        platform: artifact.platform,
        arch: artifact.arch,
        archive_format,
        package_name,
        package_version: package_version.to_string(),
        dependency_policy: ManagedDependencyPolicy::DenyImplicit,
    };
    let required_confirmations = confirmations(verified.manifest(), &policy);
    let Transport::Stdio {
        startup_timeout_seconds,
    } = &verified.manifest().transport
    else {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::TransportMismatch,
            "managed packages require stdio transport",
        ));
    };
    let projection = ConnectionProjection::ManagedStdio {
        name: verified.manifest().name.clone(),
        description: verified.manifest().description.clone(),
        executable: entrypoint.executable.clone(),
        args: entrypoint.args.clone(),
        environment_keys: entrypoint.environment_keys.clone(),
        cwd: entrypoint.cwd.clone(),
        timeout_seconds: *startup_timeout_seconds,
    };
    InstallationPlan::new(
        verified.manifest(),
        verified.digest().to_string(),
        AdapterIdentity {
            id: adapter_id.to_string(),
            version: ADAPTER_VERSION.to_string(),
        },
        context.trust_tier,
        context.operation,
        vec![step],
        EffectSummary {
            registers_connection: true,
            downloads_artifacts: true,
            writes_files: true,
            removes_files: matches!(
                context.operation,
                PlanOperation::Update | PlanOperation::Repair
            ),
            requires_process_spawn: true,
            network_origins: vec![origin(&source)?],
            permission_ids,
        },
        vec![
            PlanWarning::DefaultDisabled,
            PlanWarning::ManagedArtifactDownload,
        ],
        required_confirmations,
        projection,
        policy,
    )
}

fn selected_artifact<'a>(
    artifacts: &'a [Artifact],
    context: &PolicyContext,
) -> McpPlatformResult<&'a Artifact> {
    let mut matches = artifacts
        .iter()
        .filter_map(|artifact| {
            let platform = if artifact.platform == context.platform {
                2
            } else if artifact.platform == crate::mcp_platform::manifest::Platform::Any {
                1
            } else {
                return None;
            };
            let arch = if artifact.arch == context.arch {
                2
            } else if matches!(
                artifact.arch,
                crate::mcp_platform::manifest::Architecture::Any
                    | crate::mcp_platform::manifest::Architecture::Universal
            ) {
                1
            } else {
                return None;
            };
            Some((platform + arch, artifact))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(specificity, _)| std::cmp::Reverse(*specificity));
    let Some((specificity, selected)) = matches.first().copied() else {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::PolicyDenied,
            "no compatible managed artifact exists",
        ));
    };
    if matches
        .get(1)
        .is_some_and(|(other, _)| *other == specificity)
    {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::DuplicateSelector,
            "managed artifact selector is ambiguous",
        ));
    }
    Ok(selected)
}

fn validate_wheel_filename(
    url: &str,
    project: &str,
    version: &str,
    context: &PolicyContext,
) -> McpPlatformResult<()> {
    let parsed = Url::parse(url).map_err(|_| unsafe_distribution())?;
    let filename = parsed
        .path_segments()
        .and_then(Iterator::last)
        .ok_or_else(unsafe_distribution)?;
    if !filename.ends_with(".whl") || filename.contains("/") {
        return Err(unsafe_distribution());
    }
    let stem = filename
        .strip_suffix(".whl")
        .ok_or_else(unsafe_distribution)?;
    let parts = stem.split('-').collect::<Vec<_>>();
    if parts.len() < 5 {
        return Err(unsafe_distribution());
    }
    let normalized = |value: &str| value.replace(['-', '.'], "_").to_ascii_lowercase();
    if normalized(parts[0]) != normalized(project) || parts[1] != version {
        return Err(unsafe_distribution());
    }
    let python_tag = parts[parts.len() - 3];
    let abi_tag = parts[parts.len() - 2];
    let platform_tag = parts[parts.len() - 1];
    let (major, minor) = context
        .python_major_minor
        .ok_or_else(runtime_incompatible)?;
    let compatible = python_tag.split('.').any(|tag| {
        if tag == "py3" {
            return major == 3 && abi_tag == "none";
        }
        let exact = format!("cp{major}{minor}");
        if tag == exact {
            return abi_tag == exact || abi_tag == "abi3" || abi_tag == "none";
        }
        if let Some(version) = tag
            .strip_prefix("cp")
            .and_then(|value| value.parse::<u16>().ok())
        {
            return abi_tag == "abi3" && version <= u16::from(major) * 100 + u16::from(minor);
        }
        false
    });
    if !compatible || !platform_tag_compatible(platform_tag, context) {
        return Err(runtime_incompatible());
    }
    Ok(())
}

fn platform_tag_compatible(tag: &str, context: &PolicyContext) -> bool {
    if tag.split('.').any(|value| value == "any") {
        return true;
    }
    tag.split('.')
        .any(|value| match (context.platform, context.arch) {
            (
                crate::mcp_platform::manifest::Platform::Windows,
                crate::mcp_platform::manifest::Architecture::X86_64,
            ) => value == "win_amd64",
            (
                crate::mcp_platform::manifest::Platform::Windows,
                crate::mcp_platform::manifest::Architecture::Aarch64,
            ) => value == "win_arm64",
            (
                crate::mcp_platform::manifest::Platform::Linux,
                crate::mcp_platform::manifest::Architecture::X86_64,
            ) => value == "linux_x86_64" || (value.contains("linux") && value.ends_with("_x86_64")),
            (
                crate::mcp_platform::manifest::Platform::Linux,
                crate::mcp_platform::manifest::Architecture::Aarch64,
            ) => {
                value == "linux_aarch64" || (value.contains("linux") && value.ends_with("_aarch64"))
            }
            (
                crate::mcp_platform::manifest::Platform::Macos,
                crate::mcp_platform::manifest::Architecture::X86_64,
            ) => {
                value.starts_with("macosx_")
                    && (value.ends_with("_x86_64") || value.ends_with("_universal2"))
            }
            (
                crate::mcp_platform::manifest::Platform::Macos,
                crate::mcp_platform::manifest::Architecture::Aarch64,
            ) => {
                value.starts_with("macosx_")
                    && (value.ends_with("_arm64") || value.ends_with("_universal2"))
            }
            _ => false,
        })
}

fn validate_relative_entrypoint(entrypoint: &Entrypoint) -> McpPlatformResult<()> {
    for value in std::iter::once(entrypoint.executable.as_str())
        .chain(entrypoint.cwd.iter().map(String::as_str))
    {
        let stripped = value
            .strip_prefix("${installation.root}/")
            .or_else(|| value.strip_prefix("${installation.bin}/"))
            .unwrap_or(value);
        if stripped.is_empty()
            || stripped.starts_with(['/', '\\'])
            || stripped.contains(':')
            || stripped.split(['/', '\\']).any(|part| part == "..")
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::PathTraversal,
                "managed entrypoint must stay inside the installation root",
            ));
        }
    }
    Ok(())
}

fn validate_logical_bin_entrypoint(entrypoint: &Entrypoint) -> McpPlatformResult<()> {
    let Some(name) = entrypoint.executable.strip_prefix("${installation.bin}/") else {
        return Err(runtime_incompatible());
    };
    if name.is_empty() || name.contains(['/', '\\', ':']) || name == "." || name == ".." {
        return Err(runtime_incompatible());
    }
    Ok(())
}

fn require_same_origin(registry: &str, artifact: &str) -> McpPlatformResult<()> {
    let registry = Url::parse(registry).map_err(|_| unsafe_distribution())?;
    let artifact = Url::parse(artifact).map_err(|_| unsafe_distribution())?;
    if origin(&registry)? != origin(&artifact)? {
        return Err(unsafe_distribution());
    }
    Ok(())
}

fn origin(url: &Url) -> McpPlatformResult<String> {
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(unsafe_distribution());
    }
    let host = url.host_str().ok_or_else(unsafe_distribution)?;
    Ok(match url.port() {
        Some(port) => format!("https://{host}:{port}"),
        None => format!("https://{host}"),
    })
}

fn confirmations(
    manifest: &crate::mcp_platform::manifest::Manifest,
    policy: &crate::mcp_platform::policy::PolicyDecision,
) -> Vec<RequiredConfirmation> {
    let mut values = if policy.outcome == PolicyOutcome::NeedsConfirmation {
        policy
            .reasons
            .iter()
            .map(|r| RequiredConfirmation::Policy {
                reason_code: r.code,
            })
            .collect()
    } else {
        Vec::new()
    };
    values.extend(manifest.permissions.iter().filter(|p| p.required).map(|p| {
        RequiredConfirmation::Permission {
            permission_id: p.id.clone(),
        }
    }));
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
        "managed distribution is outside the approved supply-chain policy",
    )
}
const fn runtime_incompatible() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::AdapterIncompatible,
        "managed distribution is incompatible with the server runtime capability",
    )
}
