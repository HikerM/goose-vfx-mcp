use serde::de::Error as _;
use serde::{Deserialize, Serialize};

use crate::agents::extension::Envs;
use crate::agents::ExtensionConfig;

use super::domain::TrustTier;
use super::error::McpPlatformResult;
use super::manifest::{
    digest_serializable, Architecture, ArchiveFormat, Auth, DockerMount, GitDevAdapter,
    HealthCheck, Manifest, Platform, Sha256Digest,
};
use super::policy::{PlanOperation, PolicyDecision, PolicyReasonCode};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterIdentity {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanStep {
    RegisterRemote {
        endpoint: String,
        auth: Auth,
        health_check: HealthCheck,
        permission_ids: Vec<String>,
    },
    RegisterStdio {
        executable: String,
        args: Vec<String>,
        environment_keys: Vec<String>,
        cwd: Option<String>,
        startup_timeout_seconds: Option<u64>,
        health_check: HealthCheck,
        permission_ids: Vec<String>,
    },
    AcquireManagedDistribution {
        kind: ManagedDistributionKind,
        source_origin: String,
        artifact_url: String,
        artifact_digest: Sha256Digest,
        expected_size_bytes: Option<u64>,
        platform: Platform,
        arch: Architecture,
        archive_format: Option<ArchiveFormat>,
        package_name: Option<String>,
        package_version: String,
        dependency_policy: ManagedDependencyPolicy,
    },
    AcquireDockerDistribution {
        image: String,
        digest: Sha256Digest,
        mounts: Vec<DockerMount>,
        mount_plan_digest: String,
    },
    AcquireGitDevDistribution {
        repository_origin: String,
        repository: String,
        commit: String,
        subdirectory: Option<String>,
        underlying_adapter: GitDevAdapter,
        acquisition_digest: String,
    },
    RemoveManagedInstallation {
        managed_mcp_id: String,
        version: String,
        preserve_user_data: bool,
        ownership_only: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedDistributionKind {
    Npm,
    PythonWheel,
    BinaryArchive,
    Docker,
    GitDev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedDependencyPolicy {
    DenyImplicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectSummary {
    pub registers_connection: bool,
    pub downloads_artifacts: bool,
    pub writes_files: bool,
    pub removes_files: bool,
    pub requires_process_spawn: bool,
    pub network_origins: Vec<String>,
    pub permission_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequiredConfirmation {
    Policy { reason_code: PolicyReasonCode },
    Permission { permission_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanWarning {
    DefaultDisabled,
    RegistrationOnly,
    ManagedArtifactDownload,
    ExistingVersionRetainedUntilCommit,
    RemovesOwnedFilesOnly,
    ImmutableContainerImage,
    WritableContainerMount,
    DevelopmentSourcePinnedCommitNoBuild,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConnectionProjection {
    RemoteHttp {
        name: String,
        description: String,
        uri: String,
        timeout_seconds: Option<u64>,
    },
    ManualStdio {
        name: String,
        description: String,
        executable: String,
        args: Vec<String>,
        environment_keys: Vec<String>,
        cwd: Option<String>,
        timeout_seconds: Option<u64>,
    },
    ManagedStdio {
        name: String,
        description: String,
        executable: String,
        args: Vec<String>,
        environment_keys: Vec<String>,
        cwd: Option<String>,
        timeout_seconds: Option<u64>,
    },
    ManagedDockerStdio {
        name: String,
        description: String,
        executable: String,
        args: Vec<String>,
        cwd: Option<String>,
        timeout_seconds: Option<u64>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ConnectionProjectionWire {
    RemoteHttp {
        name: String,
        description: String,
        uri: String,
        timeout_seconds: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<Auth>,
    },
    ManualStdio {
        name: String,
        description: String,
        executable: String,
        args: Vec<String>,
        environment_keys: Vec<String>,
        cwd: Option<String>,
        timeout_seconds: Option<u64>,
    },
    ManagedStdio {
        name: String,
        description: String,
        executable: String,
        args: Vec<String>,
        environment_keys: Vec<String>,
        cwd: Option<String>,
        timeout_seconds: Option<u64>,
    },
    ManagedDockerStdio {
        name: String,
        description: String,
        executable: String,
        args: Vec<String>,
        cwd: Option<String>,
        timeout_seconds: Option<u64>,
    },
}

impl ConnectionProjectionWire {
    fn has_unsupported_legacy_auth(&self) -> bool {
        match self {
            Self::RemoteHttp {
                auth: Some(auth), ..
            } => !matches!(auth, Auth::None),
            _ => false,
        }
    }

    fn into_projection(self) -> ConnectionProjection {
        match self {
            Self::RemoteHttp {
                name,
                description,
                uri,
                timeout_seconds,
                ..
            } => ConnectionProjection::RemoteHttp {
                name,
                description,
                uri,
                timeout_seconds,
            },
            Self::ManualStdio {
                name,
                description,
                executable,
                args,
                environment_keys,
                cwd,
                timeout_seconds,
            } => ConnectionProjection::ManualStdio {
                name,
                description,
                executable,
                args,
                environment_keys,
                cwd,
                timeout_seconds,
            },
            Self::ManagedStdio {
                name,
                description,
                executable,
                args,
                environment_keys,
                cwd,
                timeout_seconds,
            } => ConnectionProjection::ManagedStdio {
                name,
                description,
                executable,
                args,
                environment_keys,
                cwd,
                timeout_seconds,
            },
            Self::ManagedDockerStdio {
                name,
                description,
                executable,
                args,
                cwd,
                timeout_seconds,
            } => ConnectionProjection::ManagedDockerStdio {
                name,
                description,
                executable,
                args,
                cwd,
                timeout_seconds,
            },
        }
    }

    fn legacy_none_from_projection(projection: &ConnectionProjection) -> Option<Self> {
        match projection {
            ConnectionProjection::RemoteHttp {
                name,
                description,
                uri,
                timeout_seconds,
            } => Some(Self::RemoteHttp {
                name: name.clone(),
                description: description.clone(),
                uri: uri.clone(),
                timeout_seconds: *timeout_seconds,
                auth: Some(Auth::None),
            }),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for ConnectionProjection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ConnectionProjectionWire::deserialize(deserializer)?;
        if wire.has_unsupported_legacy_auth() {
            return Err(D::Error::custom(
                "legacy managed remote authentication requires replanning",
            ));
        }
        Ok(Self::from_wire(wire))
    }
}

impl ConnectionProjection {
    fn from_wire(wire: ConnectionProjectionWire) -> Self {
        wire.into_projection()
    }
}

impl ConnectionProjection {
    pub fn to_extension_config(&self) -> ExtensionConfig {
        match self {
            Self::RemoteHttp {
                name,
                description,
                uri,
                timeout_seconds,
            } => ExtensionConfig::ManagedStreamableHttp {
                name: name.clone(),
                description: description.clone(),
                uri: uri.clone(),
                timeout: *timeout_seconds,
                bundled: None,
                available_tools: Vec::new(),
            },
            Self::ManualStdio {
                name,
                description,
                executable,
                args,
                environment_keys,
                cwd,
                timeout_seconds,
            } => ExtensionConfig::Stdio {
                name: name.clone(),
                description: description.clone(),
                cmd: executable.clone(),
                args: args.clone(),
                envs: Envs::default(),
                env_keys: environment_keys.clone(),
                timeout: *timeout_seconds,
                cwd: cwd.clone(),
                bundled: None,
                available_tools: Vec::new(),
            },
            Self::ManagedStdio {
                name,
                description,
                executable,
                args,
                environment_keys,
                cwd,
                timeout_seconds,
            } => ExtensionConfig::Stdio {
                name: name.clone(),
                description: description.clone(),
                cmd: executable.clone(),
                args: args.clone(),
                envs: Envs::default(),
                env_keys: environment_keys.clone(),
                timeout: *timeout_seconds,
                cwd: cwd.clone(),
                bundled: None,
                available_tools: Vec::new(),
            },
            Self::ManagedDockerStdio {
                name,
                description,
                executable,
                args,
                cwd,
                timeout_seconds,
            } => ExtensionConfig::Stdio {
                name: name.clone(),
                description: description.clone(),
                cmd: executable.clone(),
                args: args.clone(),
                envs: Envs::managed_docker(),
                env_keys: Vec::new(),
                timeout: *timeout_seconds,
                cwd: cwd.clone(),
                bundled: None,
                available_tools: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationPlan {
    manifest_id: String,
    manifest_version: String,
    manifest_digest: String,
    plan_digest: String,
    adapter: AdapterIdentity,
    trust_tier: TrustTier,
    operation: PlanOperation,
    steps: Vec<PlanStep>,
    effects: EffectSummary,
    warnings: Vec<PlanWarning>,
    required_confirmations: Vec<RequiredConfirmation>,
    default_enabled: bool,
    connection_projection: ConnectionProjection,
    policy: PolicyDecision,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallationPlanWire {
    manifest_id: String,
    manifest_version: String,
    manifest_digest: String,
    plan_digest: String,
    adapter: AdapterIdentity,
    trust_tier: TrustTier,
    operation: PlanOperation,
    steps: Vec<PlanStep>,
    effects: EffectSummary,
    warnings: Vec<PlanWarning>,
    required_confirmations: Vec<RequiredConfirmation>,
    default_enabled: bool,
    connection_projection: ConnectionProjectionWire,
    policy: PolicyDecision,
}

impl<'de> Deserialize<'de> for InstallationPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = InstallationPlanWire::deserialize(deserializer)?;
        if wire.connection_projection.has_unsupported_legacy_auth() {
            return Err(D::Error::custom(
                "legacy managed remote authentication requires replanning",
            ));
        }
        let plan = Self {
            manifest_id: wire.manifest_id,
            manifest_version: wire.manifest_version,
            manifest_digest: wire.manifest_digest,
            plan_digest: wire.plan_digest,
            adapter: wire.adapter,
            trust_tier: wire.trust_tier,
            operation: wire.operation,
            steps: wire.steps,
            effects: wire.effects,
            warnings: wire.warnings,
            required_confirmations: wire.required_confirmations,
            default_enabled: wire.default_enabled,
            connection_projection: wire.connection_projection.into_projection(),
            policy: wire.policy,
        };
        plan.verify_integrity()
            .map_err(|_| D::Error::custom("installation plan integrity validation failed"))?;
        Ok(plan)
    }
}

impl InstallationPlan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        manifest: &Manifest,
        manifest_digest: String,
        adapter: AdapterIdentity,
        trust_tier: TrustTier,
        operation: PlanOperation,
        steps: Vec<PlanStep>,
        effects: EffectSummary,
        warnings: Vec<PlanWarning>,
        required_confirmations: Vec<RequiredConfirmation>,
        connection_projection: ConnectionProjection,
        policy: PolicyDecision,
    ) -> McpPlatformResult<Self> {
        let content = PlanDigestContent {
            manifest_id: &manifest.id,
            manifest_version: manifest.version.as_str(),
            manifest_digest: &manifest_digest,
            adapter: &adapter,
            trust_tier,
            operation,
            steps: &steps,
            effects: &effects,
            warnings: &warnings,
            required_confirmations: &required_confirmations,
            default_enabled: false,
            connection_projection: &connection_projection,
            policy: &policy,
        };
        let plan_digest = digest_serializable(&content)?;

        Ok(Self {
            manifest_id: manifest.id.clone(),
            manifest_version: manifest.version.as_str().to_string(),
            manifest_digest,
            plan_digest,
            adapter,
            trust_tier,
            operation,
            steps,
            effects,
            warnings,
            required_confirmations,
            default_enabled: false,
            connection_projection,
            policy,
        })
    }

    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }

    pub fn manifest_id(&self) -> &str {
        &self.manifest_id
    }

    pub fn manifest_version(&self) -> &str {
        &self.manifest_version
    }

    pub fn plan_digest(&self) -> &str {
        &self.plan_digest
    }

    pub fn adapter(&self) -> &AdapterIdentity {
        &self.adapter
    }

    pub const fn trust_tier(&self) -> TrustTier {
        self.trust_tier
    }

    pub const fn operation(&self) -> PlanOperation {
        self.operation
    }

    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    pub fn effects(&self) -> &EffectSummary {
        &self.effects
    }

    pub fn warnings(&self) -> &[PlanWarning] {
        &self.warnings
    }

    pub fn required_confirmations(&self) -> &[RequiredConfirmation] {
        &self.required_confirmations
    }

    pub const fn default_enabled(&self) -> bool {
        self.default_enabled
    }

    pub fn connection_projection(&self) -> &ConnectionProjection {
        &self.connection_projection
    }

    pub fn policy(&self) -> &PolicyDecision {
        &self.policy
    }

    pub(crate) fn verify_integrity(&self) -> McpPlatformResult<()> {
        use super::error::{McpPlatformError, McpPlatformErrorCode};

        if self.default_enabled {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored installation plan violates the disabled-by-default invariant",
            ));
        }
        let content = PlanDigestContent {
            manifest_id: &self.manifest_id,
            manifest_version: &self.manifest_version,
            manifest_digest: &self.manifest_digest,
            adapter: &self.adapter,
            trust_tier: self.trust_tier,
            operation: self.operation,
            steps: &self.steps,
            effects: &self.effects,
            warnings: &self.warnings,
            required_confirmations: &self.required_confirmations,
            default_enabled: self.default_enabled,
            connection_projection: &self.connection_projection,
            policy: &self.policy,
        };
        if digest_serializable(&content)? != self.plan_digest
            && !self.legacy_none_digest_matches()?
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored installation plan digest does not match its content",
            ));
        }
        Ok(())
    }

    fn legacy_none_digest_matches(&self) -> McpPlatformResult<bool> {
        let Some(connection_projection) =
            ConnectionProjectionWire::legacy_none_from_projection(&self.connection_projection)
        else {
            return Ok(false);
        };
        let content = LegacyPlanDigestContent {
            manifest_id: &self.manifest_id,
            manifest_version: &self.manifest_version,
            manifest_digest: &self.manifest_digest,
            adapter: &self.adapter,
            trust_tier: self.trust_tier,
            operation: self.operation,
            steps: &self.steps,
            effects: &self.effects,
            warnings: &self.warnings,
            required_confirmations: &self.required_confirmations,
            default_enabled: self.default_enabled,
            connection_projection: &connection_projection,
            policy: &self.policy,
        };
        Ok(digest_serializable(&content)? == self.plan_digest)
    }
}

#[derive(Serialize)]
struct PlanDigestContent<'a> {
    manifest_id: &'a str,
    manifest_version: &'a str,
    manifest_digest: &'a str,
    adapter: &'a AdapterIdentity,
    trust_tier: TrustTier,
    operation: PlanOperation,
    steps: &'a [PlanStep],
    effects: &'a EffectSummary,
    warnings: &'a [PlanWarning],
    required_confirmations: &'a [RequiredConfirmation],
    default_enabled: bool,
    connection_projection: &'a ConnectionProjection,
    policy: &'a PolicyDecision,
}

#[derive(Serialize)]
struct LegacyPlanDigestContent<'a> {
    manifest_id: &'a str,
    manifest_version: &'a str,
    manifest_digest: &'a str,
    adapter: &'a AdapterIdentity,
    trust_tier: TrustTier,
    operation: PlanOperation,
    steps: &'a [PlanStep],
    effects: &'a EffectSummary,
    warnings: &'a [PlanWarning],
    required_confirmations: &'a [RequiredConfirmation],
    default_enabled: bool,
    connection_projection: &'a ConnectionProjectionWire,
    policy: &'a PolicyDecision,
}
