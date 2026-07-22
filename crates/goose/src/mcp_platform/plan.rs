use std::collections::HashMap;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConnectionProjection {
    RemoteHttp {
        name: String,
        description: String,
        uri: String,
        timeout_seconds: Option<u64>,
        auth: Auth,
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

impl ConnectionProjection {
    pub fn to_extension_config(&self) -> ExtensionConfig {
        match self {
            Self::RemoteHttp {
                name,
                description,
                uri,
                timeout_seconds,
                auth,
            } => {
                let (env_keys, headers) = remote_auth_projection(auth);
                ExtensionConfig::StreamableHttp {
                    name: name.clone(),
                    description: description.clone(),
                    uri: uri.clone(),
                    envs: Envs::default(),
                    env_keys,
                    headers,
                    timeout: *timeout_seconds,
                    socket: None,
                    bundled: None,
                    available_tools: Vec::new(),
                }
            }
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
    connection_projection: ConnectionProjection,
    policy: PolicyDecision,
}

impl<'de> Deserialize<'de> for InstallationPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = InstallationPlanWire::deserialize(deserializer)?;
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
            connection_projection: wire.connection_projection,
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
        if digest_serializable(&content)? != self.plan_digest {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored installation plan digest does not match its content",
            ));
        }
        Ok(())
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

fn remote_auth_projection(auth: &Auth) -> (Vec<String>, HashMap<String, String>) {
    match auth {
        Auth::ApiKeyHeader {
            header_name,
            prefix,
            credential_name,
        } => {
            let environment_key = credential_environment_key(credential_name);
            let prefix = prefix.as_deref().unwrap_or_default();
            let separator = if prefix.is_empty() { "" } else { " " };
            let value = format!("{prefix}{separator}${{{environment_key}}}");
            (
                vec![environment_key],
                HashMap::from([(header_name.clone(), value)]),
            )
        }
        Auth::Environment {
            environment_key, ..
        } => (vec![environment_key.clone()], HashMap::new()),
        Auth::None | Auth::Oauth2 { .. } => (Vec::new(), HashMap::new()),
    }
}

fn credential_environment_key(credential_name: &str) -> String {
    credential_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}
