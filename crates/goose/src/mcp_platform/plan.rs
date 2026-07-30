use serde::de::Error as _;
use serde::{Deserialize, Serialize};

use crate::agents::extension::Envs;
use crate::agents::ExtensionConfig;

use super::domain::TrustTier;
use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::manifest::{
    digest_serializable, Architecture, ArchiveFormat, ArtifactCapacity, ArtifactCapacityContract,
    Auth, Distribution, DockerMount, GitDevAdapter, HealthCheck, Manifest, Platform, Sha256Digest,
    VerifiedManifest,
};
use super::policy::{PlanOperation, PolicyDecision, PolicyReasonCode};
use super::repository::ManifestSourceContext;
use url::Url;

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
pub struct ManagedCapacityBudget {
    /// Digest of the signed manifest metadata from which the contracts came.
    pub metadata_digest: String,
    pub operation: PlanOperation,
    pub artifact_capacities: Vec<ArtifactCapacityBinding>,
    pub required_peak_bytes: u64,
}

/// A capacity contract bound to the exact manifest artifact acquired by a plan.
///
/// All fields are manifest metadata and are deliberately non-secret. The binding
/// prevents a plan digest from being used as the only authority for capacity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCapacityBinding {
    pub artifact_url: String,
    pub artifact_digest: Sha256Digest,
    pub platform: Platform,
    pub arch: Architecture,
    pub capacity: ArtifactCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedCapacityContractStatus {
    Known { required_peak_bytes: u64 },
    Unknown,
}

/// Runtime facts selected by the execution layer, never from stored plan data.
///
/// Construct this from the host target and the adapter dispatch that is about to
/// run. It must not be reconstructed from an [`InstallationPlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TrustedPlanSelectionContext {
    platform: Platform,
    arch: Architecture,
    operation: PlanOperation,
    adapter: TrustedPlanAdapter,
}

impl TrustedPlanSelectionContext {
    pub(crate) const fn new(
        platform: Platform,
        arch: Architecture,
        operation: PlanOperation,
        adapter: TrustedPlanAdapter,
    ) -> Self {
        Self {
            platform,
            arch,
            operation,
            adapter,
        }
    }
}

/// The adapter selected by trusted execution dispatch, rather than plan JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustedPlanAdapter {
    RemoteHttp,
    ManualStdio,
    Npm,
    PythonWheel,
    BinaryArchive,
    Docker,
    GitDev,
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

pub fn decode_projection_config(value: &str) -> McpPlatformResult<ConnectionProjection> {
    serde_json::from_str(value).map_err(|_| projection_integrity_error())
}

pub fn encode_projection_config(projection: &ConnectionProjection) -> McpPlatformResult<String> {
    serde_json::to_string(projection).map_err(|_| projection_integrity_error())
}

pub fn projection_config_digest(projection: &ConnectionProjection) -> McpPlatformResult<String> {
    digest_serializable(projection).map_err(|_| projection_integrity_error())
}

pub fn decode_verified_projection_config(
    value: &str,
    expected_digest: &str,
) -> McpPlatformResult<ConnectionProjection> {
    let projection = decode_projection_config(value)?;
    if projection_config_digest(&projection)? != expected_digest {
        return Err(projection_integrity_error());
    }
    Ok(projection)
}

fn projection_integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "managed MCP connection projection failed integrity validation",
    )
}

impl ConnectionProjection {
    /// Managed local projections must be converted to a launcher-owned transport on Windows.
    /// The secure launcher is not available yet, so these projections cannot enter a runtime.
    pub(crate) const fn requires_windows_secure_launcher(&self) -> bool {
        cfg!(windows)
            && matches!(
                self,
                Self::ManagedStdio { .. } | Self::ManagedDockerStdio { .. }
            )
    }

    pub(crate) fn require_runtime_transport(&self) -> McpPlatformResult<()> {
        if self.requires_windows_secure_launcher() {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::RuntimeControlUnavailable,
                "managed local MCP runtime requires the Windows secure launcher",
            ));
        }
        Ok(())
    }

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capacity_budget: Option<ManagedCapacityBudget>,
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
    #[serde(default)]
    source_context: Option<ManifestSourceContext>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallationPlanWire {
    manifest_id: String,
    manifest_version: String,
    manifest_digest: String,
    plan_digest: String,
    #[serde(default)]
    capacity_budget: Option<ManagedCapacityBudget>,
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
    source_context: Option<ManifestSourceContext>,
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
            capacity_budget: wire.capacity_budget,
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
            source_context: wire.source_context,
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
        verify_planned_acquisition(manifest, &adapter, operation, &steps)?;
        let capacity_budget =
            managed_capacity_budget(manifest, &manifest_digest, operation, &steps)?;
        let content = PlanDigestContent {
            manifest_id: &manifest.id,
            manifest_version: manifest.version.as_str(),
            manifest_digest: &manifest_digest,
            capacity_budget: capacity_budget.as_ref(),
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
            source_context: None,
        };
        let plan_digest = digest_serializable(&content)?;

        Ok(Self {
            manifest_id: manifest.id.clone(),
            manifest_version: manifest.version.as_str().to_string(),
            manifest_digest,
            plan_digest,
            capacity_budget,
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
            source_context: None,
        })
    }

    pub(crate) fn bind_source_context(
        &mut self,
        source_context: ManifestSourceContext,
    ) -> McpPlatformResult<()> {
        self.source_context = Some(source_context);
        self.plan_digest = self.compute_plan_digest()?;
        Ok(())
    }

    pub(crate) fn source_context(&self) -> Option<&ManifestSourceContext> {
        self.source_context.as_ref()
    }

    fn compute_plan_digest(&self) -> McpPlatformResult<String> {
        digest_serializable(&PlanDigestContent {
            manifest_id: &self.manifest_id,
            manifest_version: &self.manifest_version,
            manifest_digest: &self.manifest_digest,
            capacity_budget: self.capacity_budget.as_ref(),
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
            source_context: self.source_context.as_ref(),
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

    /// A missing budget is only a wire-format fact, never an execution decision.
    /// Use [`Self::verify_trusted_acquisition_contract`] before storage preflight
    /// or execution to determine whether it is trusted Unknown or a replan error.
    pub(crate) fn capacity_budget(&self) -> Option<&ManagedCapacityBudget> {
        self.capacity_budget.as_ref()
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
        self.verify_content_integrity()?;
        verify_acquisition_shape_for_adapter(&self.adapter, self.operation, &self.steps)?;
        if self.capacity_budget.is_none() && has_windows_managed_step(&self.steps) {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored Windows managed plan has no capacity budget and requires replanning",
            ));
        }
        Ok(())
    }

    /// Recomputes the complete acquisition contract from independently trusted
    /// runtime selection facts and the currently verified manifest. Call this
    /// immediately before any storage preflight or execution side effect.
    ///
    /// A plan digest proves only that the stored fields agree with each other;
    /// this method binds those fields to the manifest that was independently
    /// verified for the current operation.
    pub(crate) fn verify_trusted_acquisition_contract(
        &self,
        manifest: &VerifiedManifest,
        selection: TrustedPlanSelectionContext,
    ) -> McpPlatformResult<ManagedCapacityContractStatus> {
        self.verify_content_integrity()?;
        verify_acquisition_shape_for_adapter(&self.adapter, self.operation, &self.steps)?;
        if self.manifest_id != manifest.manifest().id
            || self.manifest_version != manifest.manifest().version.as_str()
            || self.manifest_digest != manifest.digest()
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored installation plan does not match the verified manifest",
            ));
        }

        if self.operation != selection.operation
            || self.adapter != trusted_adapter_identity(selection.adapter)
        {
            return Err(capacity_replan_error());
        }

        let expected = trusted_acquisition(manifest.manifest(), selection)?;
        let actual = acquisition_steps(&self.steps);
        match expected {
            Some(expected) if actual.len() != 1 || actual[0] != &expected => {
                Err(capacity_replan_error())
            }
            None if !actual.is_empty() => Err(capacity_replan_error()),
            Some(expected @ PlanStep::AcquireManagedDistribution { .. }) => match (
                trusted_capacity_budget(
                    manifest.manifest(),
                    manifest.digest(),
                    selection.operation,
                    &expected,
                )?,
                self.capacity_budget.as_ref(),
            ) {
                (Some(expected), Some(actual)) if actual == &expected => {
                    Ok(ManagedCapacityContractStatus::Known {
                        required_peak_bytes: actual.required_peak_bytes,
                    })
                }
                (None, None) => Ok(ManagedCapacityContractStatus::Unknown),
                _ => Err(capacity_replan_error()),
            },
            Some(_) | None if self.capacity_budget.is_none() => {
                Ok(ManagedCapacityContractStatus::Unknown)
            }
            Some(_) | None => Err(capacity_replan_error()),
        }
    }

    fn verify_content_integrity(&self) -> McpPlatformResult<()> {
        use super::error::{McpPlatformError, McpPlatformErrorCode};

        if self.default_enabled {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored installation plan violates the disabled-by-default invariant",
            ));
        }
        self.verify_capacity_budget()?;
        let content = PlanDigestContent {
            manifest_id: &self.manifest_id,
            manifest_version: &self.manifest_version,
            manifest_digest: &self.manifest_digest,
            capacity_budget: self.capacity_budget.as_ref(),
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
            source_context: self.source_context.as_ref(),
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

    fn verify_capacity_budget(&self) -> McpPlatformResult<()> {
        let Some(capacity_budget) = &self.capacity_budget else {
            return Ok(());
        };
        if capacity_budget.metadata_digest != self.manifest_digest
            || capacity_budget.operation != self.operation
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored installation plan capacity budget does not match plan metadata",
            ));
        }

        let managed_steps = managed_acquisition_steps(&self.steps);
        verify_acquisition_shape_for_adapter(&self.adapter, self.operation, &self.steps)?;
        if managed_steps.len() != 1
            || capacity_budget.artifact_capacities.len() != 1
            || capacity_budget.artifact_capacities.iter().any(|binding| {
                binding.platform != Platform::Windows || binding.capacity.validate().is_err()
            })
            || managed_steps.iter().any(|step| {
                !matches!(
                    step,
                    PlanStep::AcquireManagedDistribution {
                        platform: Platform::Windows,
                        ..
                    }
                )
            })
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored installation plan capacity budget does not match managed artifacts",
            ));
        }

        if !managed_step_matches_binding(managed_steps[0], &capacity_budget.artifact_capacities[0])
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored installation plan capacity binding does not match managed artifacts",
            ));
        }

        let required_peak_bytes = capacity_budget
            .artifact_capacities
            .iter()
            .try_fold(0_u64, |total, binding| {
                binding.capacity.validate().map_err(|_| {
                    McpPlatformError::new(
                        McpPlatformErrorCode::IntegrityError,
                        "stored installation plan contains an invalid capacity contract",
                    )
                })?;
                total.checked_add(binding.capacity.required_peak_bytes().map_err(|_| {
                    McpPlatformError::new(
                        McpPlatformErrorCode::IntegrityError,
                        "stored installation plan contains an invalid capacity contract",
                    )
                })?)
                .ok_or_else(|| {
                    McpPlatformError::new(
                        McpPlatformErrorCode::IntegrityError,
                        "stored installation plan capacity budget exceeds supported storage accounting",
                    )
                })
            })?;
        if capacity_budget.required_peak_bytes != required_peak_bytes {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "stored installation plan capacity budget peak does not match artifact contracts",
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

fn managed_capacity_budget(
    manifest: &Manifest,
    metadata_digest: &str,
    operation: PlanOperation,
    steps: &[PlanStep],
) -> McpPlatformResult<Option<ManagedCapacityBudget>> {
    let managed_steps = managed_acquisition_steps(steps);
    if managed_steps.is_empty() {
        return Ok(None);
    }
    verify_at_most_one_acquisition(steps)?;

    let step = managed_steps[0];
    let artifact = manifest.distribution.artifacts().iter().find(|artifact| {
        matches!(
            step,
            PlanStep::AcquireManagedDistribution {
                artifact_url,
                artifact_digest,
                platform,
                arch,
                ..
            } if artifact.url == *artifact_url
                && artifact.digest == *artifact_digest
                && artifact.platform == *platform
                && artifact.arch == *arch
        )
    });
    if artifact.is_none() {
        return Err(capacity_replan_error());
    }
    trusted_capacity_budget(manifest, metadata_digest, operation, step)
}

fn trusted_managed_acquisition(
    manifest: &Manifest,
    selection: TrustedPlanSelectionContext,
) -> McpPlatformResult<Option<PlanStep>> {
    let (kind, package_name, package_version, archive_format) = match &manifest.distribution {
        Distribution::Npm {
            package,
            package_version,
            ..
        } if selection.adapter == TrustedPlanAdapter::Npm => (
            ManagedDistributionKind::Npm,
            Some(package.clone()),
            package_version.as_str().to_string(),
            None,
        ),
        Distribution::PythonWheel {
            package,
            package_version,
            ..
        } if selection.adapter == TrustedPlanAdapter::PythonWheel => (
            ManagedDistributionKind::PythonWheel,
            Some(package.clone()),
            package_version.as_str().to_string(),
            None,
        ),
        Distribution::BinaryArchive { archive_format, .. }
            if selection.adapter == TrustedPlanAdapter::BinaryArchive =>
        {
            (
                ManagedDistributionKind::BinaryArchive,
                None,
                manifest.version.as_str().to_string(),
                Some(*archive_format),
            )
        }
        Distribution::RemoteHttp if selection.adapter == TrustedPlanAdapter::RemoteHttp => {
            return Ok(None);
        }
        Distribution::ManualStdio { .. }
            if selection.adapter == TrustedPlanAdapter::ManualStdio =>
        {
            return Ok(None);
        }
        Distribution::Docker { .. } if selection.adapter == TrustedPlanAdapter::Docker => {
            return Ok(None);
        }
        Distribution::GitDev { .. } if selection.adapter == TrustedPlanAdapter::GitDev => {
            return Ok(None);
        }
        _ => return Err(capacity_replan_error()),
    };

    if !matches!(
        selection.operation,
        PlanOperation::Install | PlanOperation::Update | PlanOperation::Repair
    ) {
        return Err(capacity_replan_error());
    }
    let artifact = trusted_selected_artifact(manifest.distribution.artifacts(), selection)?;
    let source = Url::parse(&artifact.url).map_err(|_| capacity_replan_error())?;
    let source_origin = trusted_https_origin(&source)?;
    Ok(Some(PlanStep::AcquireManagedDistribution {
        kind,
        source_origin,
        artifact_url: artifact.url.clone(),
        artifact_digest: artifact.digest.clone(),
        expected_size_bytes: artifact.size_bytes,
        platform: artifact.platform,
        arch: artifact.arch,
        archive_format,
        package_name,
        package_version,
        dependency_policy: ManagedDependencyPolicy::DenyImplicit,
    }))
}

/// Returns the single acquisition authority that trusted dispatch selected.
///
/// The result deliberately covers every acquisition variant. Callers must
/// compare the complete acquisition set, rather than look for a matching
/// managed step, because the runner dispatches the first acquisition it sees.
fn trusted_acquisition(
    manifest: &Manifest,
    selection: TrustedPlanSelectionContext,
) -> McpPlatformResult<Option<PlanStep>> {
    if selection.operation == PlanOperation::Uninstall {
        return Ok(None);
    }
    match &manifest.distribution {
        Distribution::Npm { .. }
        | Distribution::PythonWheel { .. }
        | Distribution::BinaryArchive { .. } => trusted_managed_acquisition(manifest, selection),
        Distribution::RemoteHttp if selection.adapter == TrustedPlanAdapter::RemoteHttp => {
            if selection.operation == PlanOperation::Register {
                Ok(None)
            } else {
                Err(capacity_replan_error())
            }
        }
        Distribution::ManualStdio { .. }
            if selection.adapter == TrustedPlanAdapter::ManualStdio =>
        {
            if selection.operation == PlanOperation::Register {
                Ok(None)
            } else {
                Err(capacity_replan_error())
            }
        }
        Distribution::Docker {
            image,
            digest,
            mounts,
            ..
        } if selection.adapter == TrustedPlanAdapter::Docker => {
            require_managed_operation(selection.operation)?;
            Ok(Some(PlanStep::AcquireDockerDistribution {
                image: image.clone(),
                digest: digest.clone(),
                mounts: mounts.clone(),
                mount_plan_digest: digest_serializable(mounts)
                    .map_err(|_| capacity_replan_error())?,
            }))
        }
        Distribution::GitDev {
            repository,
            commit,
            subdirectory,
            adapter,
            ..
        } if selection.adapter == TrustedPlanAdapter::GitDev => {
            require_managed_operation(selection.operation)?;
            super::manifest::validate_git_repository(repository)
                .map_err(|_| capacity_replan_error())?;
            let url = Url::parse(repository).map_err(|_| capacity_replan_error())?;
            let host = url.host_str().ok_or_else(capacity_replan_error)?;
            let repository_origin = match url.port() {
                Some(port) => format!("https://{host}:{port}"),
                None => format!("https://{host}"),
            };
            let acquisition_digest = digest_serializable(&GitAcquisitionAuthority {
                repository,
                commit,
                subdirectory,
                adapter: *adapter,
            })
            .map_err(|_| capacity_replan_error())?;
            Ok(Some(PlanStep::AcquireGitDevDistribution {
                repository_origin,
                repository: repository.clone(),
                commit: commit.clone(),
                subdirectory: subdirectory.clone(),
                underlying_adapter: *adapter,
                acquisition_digest,
            }))
        }
        _ => Err(capacity_replan_error()),
    }
}

#[derive(Serialize)]
struct GitAcquisitionAuthority<'a> {
    repository: &'a str,
    commit: &'a str,
    subdirectory: &'a Option<String>,
    adapter: GitDevAdapter,
}

fn require_managed_operation(operation: PlanOperation) -> McpPlatformResult<()> {
    if matches!(
        operation,
        PlanOperation::Install | PlanOperation::Update | PlanOperation::Repair
    ) {
        Ok(())
    } else {
        Err(capacity_replan_error())
    }
}

fn verify_planned_acquisition(
    manifest: &Manifest,
    adapter: &AdapterIdentity,
    operation: PlanOperation,
    steps: &[PlanStep],
) -> McpPlatformResult<()> {
    verify_at_most_one_acquisition(steps)?;
    let trusted_adapter =
        trusted_adapter_from_identity(adapter).ok_or_else(capacity_replan_error)?;
    let selection = match acquisition_steps(steps).first() {
        Some(PlanStep::AcquireManagedDistribution { platform, arch, .. }) => {
            TrustedPlanSelectionContext::new(*platform, *arch, operation, trusted_adapter)
        }
        _ => TrustedPlanSelectionContext::new(
            Platform::Any,
            Architecture::Any,
            operation,
            trusted_adapter,
        ),
    };
    let expected = trusted_acquisition(manifest, selection)?;
    let actual = acquisition_steps(steps);
    if expected
        .as_ref()
        .is_some_and(|expected| actual.as_slice() != [expected])
        || expected.is_none() && !actual.is_empty()
    {
        return Err(capacity_replan_error());
    }
    Ok(())
}

fn verify_acquisition_shape_for_adapter(
    adapter: &AdapterIdentity,
    operation: PlanOperation,
    steps: &[PlanStep],
) -> McpPlatformResult<()> {
    verify_at_most_one_acquisition(steps)?;
    let trusted_adapter =
        trusted_adapter_from_identity(adapter).ok_or_else(capacity_replan_error)?;
    let actual = acquisition_steps(steps);
    let expected = match (trusted_adapter, operation) {
        (_, PlanOperation::Uninstall) => None,
        (
            TrustedPlanAdapter::RemoteHttp | TrustedPlanAdapter::ManualStdio,
            PlanOperation::Register,
        ) => None,
        (
            TrustedPlanAdapter::Npm
            | TrustedPlanAdapter::PythonWheel
            | TrustedPlanAdapter::BinaryArchive,
            PlanOperation::Install | PlanOperation::Update | PlanOperation::Repair,
        ) => Some("managed"),
        (
            TrustedPlanAdapter::Docker,
            PlanOperation::Install | PlanOperation::Update | PlanOperation::Repair,
        ) => Some("docker"),
        (
            TrustedPlanAdapter::GitDev,
            PlanOperation::Install | PlanOperation::Update | PlanOperation::Repair,
        ) => Some("git"),
        _ => return Err(capacity_replan_error()),
    };
    let matches_expected = match (expected, actual.as_slice()) {
        (None, []) => true,
        (Some("managed"), [step]) => matches!(step, PlanStep::AcquireManagedDistribution { .. }),
        (Some("docker"), [step]) => matches!(step, PlanStep::AcquireDockerDistribution { .. }),
        (Some("git"), [step]) => matches!(step, PlanStep::AcquireGitDevDistribution { .. }),
        _ => false,
    };
    if matches_expected {
        Ok(())
    } else {
        Err(capacity_replan_error())
    }
}

fn trusted_adapter_from_identity(adapter: &AdapterIdentity) -> Option<TrustedPlanAdapter> {
    if adapter.version != "1" {
        return None;
    }
    match adapter.id.as_str() {
        "remote_http" => Some(TrustedPlanAdapter::RemoteHttp),
        "manual_stdio" => Some(TrustedPlanAdapter::ManualStdio),
        "npm" => Some(TrustedPlanAdapter::Npm),
        "python_wheel" => Some(TrustedPlanAdapter::PythonWheel),
        "binary_archive" => Some(TrustedPlanAdapter::BinaryArchive),
        "docker" => Some(TrustedPlanAdapter::Docker),
        "git_dev" => Some(TrustedPlanAdapter::GitDev),
        _ => None,
    }
}

fn trusted_selected_artifact(
    artifacts: &[super::manifest::Artifact],
    selection: TrustedPlanSelectionContext,
) -> McpPlatformResult<&super::manifest::Artifact> {
    let mut matches = artifacts
        .iter()
        .filter_map(|artifact| {
            let platform = if artifact.platform == selection.platform {
                2
            } else if artifact.platform == Platform::Any {
                1
            } else {
                return None;
            };
            let arch = if artifact.arch == selection.arch {
                2
            } else if matches!(artifact.arch, Architecture::Any | Architecture::Universal) {
                1
            } else {
                return None;
            };
            Some((platform + arch, artifact))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(specificity, _)| std::cmp::Reverse(*specificity));
    let Some((specificity, selected)) = matches.first().copied() else {
        return Err(capacity_replan_error());
    };
    if matches
        .get(1)
        .is_some_and(|(other, _)| *other == specificity)
    {
        return Err(capacity_replan_error());
    }
    Ok(selected)
}

fn trusted_https_origin(url: &Url) -> McpPlatformResult<String> {
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(capacity_replan_error());
    }
    let host = url.host_str().ok_or_else(capacity_replan_error)?;
    Ok(match url.port() {
        Some(port) => format!("https://{host}:{port}"),
        None => format!("https://{host}"),
    })
}

fn trusted_capacity_budget(
    manifest: &Manifest,
    metadata_digest: &str,
    operation: PlanOperation,
    expected: &PlanStep,
) -> McpPlatformResult<Option<ManagedCapacityBudget>> {
    let PlanStep::AcquireManagedDistribution {
        artifact_url,
        artifact_digest,
        platform,
        arch,
        ..
    } = expected
    else {
        return Ok(None);
    };
    let matches = manifest
        .distribution
        .artifacts()
        .iter()
        .filter(|artifact| {
            artifact.url == *artifact_url
                && artifact.digest == *artifact_digest
                && artifact.platform == *platform
                && artifact.arch == *arch
        })
        .collect::<Vec<_>>();
    let [artifact] = matches.as_slice() else {
        return Err(capacity_replan_error());
    };
    let ArtifactCapacityContract::Known(capacity) = artifact.capacity_contract() else {
        return if artifact.platform == Platform::Windows {
            Err(capacity_replan_error())
        } else {
            Ok(None)
        };
    };
    let required_peak_bytes = capacity
        .required_peak_bytes()
        .map_err(|_| capacity_replan_error())?;
    Ok(Some(ManagedCapacityBudget {
        metadata_digest: metadata_digest.to_string(),
        operation,
        artifact_capacities: vec![ArtifactCapacityBinding {
            artifact_url: artifact.url.clone(),
            artifact_digest: artifact.digest.clone(),
            platform: artifact.platform,
            arch: artifact.arch,
            capacity,
        }],
        required_peak_bytes,
    }))
}

fn managed_acquisition_steps(steps: &[PlanStep]) -> Vec<&PlanStep> {
    steps
        .iter()
        .filter(|step| matches!(step, PlanStep::AcquireManagedDistribution { .. }))
        .collect()
}

fn acquisition_steps(steps: &[PlanStep]) -> Vec<&PlanStep> {
    steps
        .iter()
        .filter(|step| {
            matches!(
                step,
                PlanStep::AcquireManagedDistribution { .. }
                    | PlanStep::AcquireDockerDistribution { .. }
                    | PlanStep::AcquireGitDevDistribution { .. }
            )
        })
        .collect()
}

fn verify_at_most_one_acquisition(steps: &[PlanStep]) -> McpPlatformResult<()> {
    if acquisition_steps(steps).len() > 1 {
        return Err(McpPlatformError::new(
            McpPlatformErrorCode::IntegrityError,
            "stored installation plan has multiple acquisitions and requires replanning",
        ));
    }
    Ok(())
}

fn trusted_adapter_identity(adapter: TrustedPlanAdapter) -> AdapterIdentity {
    let id = match adapter {
        TrustedPlanAdapter::RemoteHttp => "remote_http",
        TrustedPlanAdapter::ManualStdio => "manual_stdio",
        TrustedPlanAdapter::Npm => "npm",
        TrustedPlanAdapter::PythonWheel => "python_wheel",
        TrustedPlanAdapter::BinaryArchive => "binary_archive",
        TrustedPlanAdapter::Docker => "docker",
        TrustedPlanAdapter::GitDev => "git_dev",
    };
    AdapterIdentity {
        id: id.to_string(),
        version: "1".to_string(),
    }
}

fn capacity_replan_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "stored installation plan capacity contract requires replanning",
    )
}

fn has_windows_managed_step(steps: &[PlanStep]) -> bool {
    steps.iter().any(|step| {
        matches!(
            step,
            PlanStep::AcquireManagedDistribution {
                platform: Platform::Windows,
                ..
            }
        )
    })
}

fn managed_step_matches_binding(step: &PlanStep, binding: &ArtifactCapacityBinding) -> bool {
    matches!(
        step,
        PlanStep::AcquireManagedDistribution {
            artifact_url,
            artifact_digest,
            platform,
            arch,
            ..
        } if artifact_url == &binding.artifact_url
            && artifact_digest == &binding.artifact_digest
            && platform == &binding.platform
            && arch == &binding.arch
    )
}

#[derive(Serialize)]
struct PlanDigestContent<'a> {
    manifest_id: &'a str,
    manifest_version: &'a str,
    manifest_digest: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    capacity_budget: Option<&'a ManagedCapacityBudget>,
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
    source_context: Option<&'a ManifestSourceContext>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_platform::manifest::parse_manifest;
    use crate::mcp_platform::policy::{PolicyOutcome, PolicyReason};

    fn managed_manifest(with_capacity: bool) -> crate::mcp_platform::manifest::VerifiedManifest {
        managed_manifest_for("npm", with_capacity)
    }

    fn managed_manifest_for(
        adapter: &str,
        with_capacity: bool,
    ) -> crate::mcp_platform::manifest::VerifiedManifest {
        let capacity = with_capacity.then(|| {
            serde_json::json!({
                "download_size_bytes": 10,
                "materialized_size_bytes": 20,
                "workspace_size_bytes": 30,
                "rollback_extra_bytes": 40
            })
        });
        let distribution = match adapter {
            "npm" => serde_json::json!({
                "type": "npm", "package": "example-package", "package_version": "1.0.0",
                "registry": "https://example.com",
                "artifacts": [{
                    "platform": "windows", "arch": "x86_64", "url": "https://example.com/package",
                    "digest": { "algorithm": "sha256", "value": "a".repeat(64) },
                    "capacity": capacity
                }],
                "entrypoint": { "executable": "bin/example" }
            }),
            "python_wheel" => serde_json::json!({
                "type": "python_wheel", "package": "example_package", "package_version": "1.0.0",
                "python": ">=3.9", "index": "https://example.com",
                "artifacts": [{
                    "platform": "windows", "arch": "x86_64", "url": "https://example.com/package",
                    "digest": { "algorithm": "sha256", "value": "a".repeat(64) },
                    "capacity": capacity
                }],
                "entrypoint": { "executable": "bin/example" }
            }),
            "binary_archive" => serde_json::json!({
                "type": "binary_archive", "archive_format": "zip",
                "artifacts": [{
                    "platform": "windows", "arch": "x86_64", "url": "https://example.com/package",
                    "digest": { "algorithm": "sha256", "value": "a".repeat(64) },
                    "capacity": capacity
                }],
                "entrypoint": { "executable": "bin/example" }
            }),
            _ => unreachable!(),
        };
        let mut value = serde_json::json!({
            "schema_version": 1,
            "id": "test.managed",
            "version": "1.0.0",
            "name": "Test managed manifest",
            "description": "Managed capacity planning test.",
            "publisher": { "id": "test.publisher", "name": "Test Publisher" },
            "license": { "spdx": "MIT" },
            "capabilities": ["tools"],
            "permissions": [],
            "distribution": distribution,
            "transport": { "type": "stdio" },
            "auth": { "type": "none" },
            "health_check": { "type": "mcp_initialize", "timeout_seconds": 1 },
            "owned_files": [],
            "uninstall": { "mode": "remove_owned_files_only", "preserve_user_data": true }
        });
        if !with_capacity {
            value["distribution"]["artifacts"][0]
                .as_object_mut()
                .unwrap()
                .remove("capacity");
        }
        parse_manifest(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn recompute_plan_digest(plan: &mut InstallationPlan) {
        let content = PlanDigestContent {
            manifest_id: &plan.manifest_id,
            manifest_version: &plan.manifest_version,
            manifest_digest: &plan.manifest_digest,
            capacity_budget: plan.capacity_budget.as_ref(),
            adapter: &plan.adapter,
            trust_tier: plan.trust_tier,
            operation: plan.operation,
            steps: &plan.steps,
            effects: &plan.effects,
            warnings: &plan.warnings,
            required_confirmations: &plan.required_confirmations,
            default_enabled: plan.default_enabled,
            connection_projection: &plan.connection_projection,
            policy: &plan.policy,
            source_context: plan.source_context.as_ref(),
        };
        plan.plan_digest = digest_serializable(&content).unwrap();
    }

    fn recompute_wire_digest(wire: serde_json::Value) -> serde_json::Value {
        let wire: InstallationPlanWire = serde_json::from_value(wire).unwrap();
        let mut plan = InstallationPlan {
            manifest_id: wire.manifest_id,
            manifest_version: wire.manifest_version,
            manifest_digest: wire.manifest_digest,
            plan_digest: wire.plan_digest,
            capacity_budget: wire.capacity_budget,
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
            source_context: wire.source_context,
        };
        recompute_plan_digest(&mut plan);
        serde_json::to_value(plan).unwrap()
    }

    fn two_windows_manifest() -> crate::mcp_platform::manifest::VerifiedManifest {
        let mut value = serde_json::to_value(managed_manifest(true).manifest()).unwrap();
        value["distribution"]["artifacts"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "platform": "windows", "arch": "aarch64", "url": "https://example.com/package-arm",
                "digest": { "algorithm": "sha256", "value": "c".repeat(64) },
                "capacity": {
                    "download_size_bytes": 11,
                    "materialized_size_bytes": 22,
                    "workspace_size_bytes": 33,
                    "rollback_extra_bytes": 44
                }
            }));
        parse_manifest(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn cross_platform_manifest() -> crate::mcp_platform::manifest::VerifiedManifest {
        let mut value = serde_json::to_value(managed_manifest(true).manifest()).unwrap();
        let artifacts = value["distribution"]["artifacts"].as_array_mut().unwrap();
        for (platform, suffix, digest) in [
            ("linux", "linux", "b".repeat(64)),
            ("any", "any", "c".repeat(64)),
        ] {
            artifacts.push(serde_json::json!({
                "platform": platform,
                "arch": "x86_64",
                "url": format!("https://example.com/package-{suffix}"),
                "digest": { "algorithm": "sha256", "value": digest }
            }));
        }
        parse_manifest(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn remote_manifest() -> crate::mcp_platform::manifest::VerifiedManifest {
        let mut value = serde_json::to_value(managed_manifest(true).manifest()).unwrap();
        value["distribution"] = serde_json::json!({ "type": "remote_http" });
        value["transport"] = serde_json::json!({
            "type": "streamable_http", "url": "https://example.com/mcp"
        });
        parse_manifest(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn remote_plan(verified: &crate::mcp_platform::manifest::VerifiedManifest) -> InstallationPlan {
        InstallationPlan::new(
            verified.manifest(),
            verified.digest().to_string(),
            AdapterIdentity {
                id: "remote_http".to_string(),
                version: "1".to_string(),
            },
            TrustTier::Official,
            PlanOperation::Register,
            vec![PlanStep::RegisterRemote {
                endpoint: "https://example.com/mcp".to_string(),
                auth: Auth::None,
                health_check: verified.manifest().health_check.clone(),
                permission_ids: Vec::new(),
            }],
            EffectSummary {
                registers_connection: true,
                downloads_artifacts: false,
                writes_files: false,
                removes_files: false,
                requires_process_spawn: false,
                network_origins: vec!["https://example.com".to_string()],
                permission_ids: Vec::new(),
            },
            vec![PlanWarning::RegistrationOnly],
            Vec::new(),
            ConnectionProjection::RemoteHttp {
                name: "Test remote manifest".to_string(),
                description: "Remote capacity planning control.".to_string(),
                uri: "https://example.com/mcp".to_string(),
                timeout_seconds: None,
            },
            PolicyDecision {
                outcome: PolicyOutcome::Allow,
                reasons: Vec::new(),
            },
        )
        .unwrap()
    }

    fn manual_manifest() -> crate::mcp_platform::manifest::VerifiedManifest {
        let mut value = serde_json::to_value(managed_manifest(true).manifest()).unwrap();
        value["distribution"] = serde_json::json!({
            "type": "manual_stdio", "entrypoint": { "executable": "server" }
        });
        parse_manifest(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn manual_plan(verified: &crate::mcp_platform::manifest::VerifiedManifest) -> InstallationPlan {
        InstallationPlan::new(
            verified.manifest(),
            verified.digest().to_string(),
            AdapterIdentity {
                id: "manual_stdio".to_string(),
                version: "1".to_string(),
            },
            TrustTier::Official,
            PlanOperation::Register,
            vec![PlanStep::RegisterStdio {
                executable: "server".to_string(),
                args: Vec::new(),
                environment_keys: Vec::new(),
                cwd: None,
                startup_timeout_seconds: None,
                health_check: verified.manifest().health_check.clone(),
                permission_ids: Vec::new(),
            }],
            EffectSummary {
                registers_connection: true,
                downloads_artifacts: false,
                writes_files: false,
                removes_files: false,
                requires_process_spawn: true,
                network_origins: Vec::new(),
                permission_ids: Vec::new(),
            },
            vec![PlanWarning::RegistrationOnly],
            Vec::new(),
            ConnectionProjection::ManualStdio {
                name: "Manual test".to_string(),
                description: "Manual registration test.".to_string(),
                executable: "server".to_string(),
                args: Vec::new(),
                environment_keys: Vec::new(),
                cwd: None,
                timeout_seconds: None,
            },
            PolicyDecision {
                outcome: PolicyOutcome::Allow,
                reasons: Vec::new(),
            },
        )
        .unwrap()
    }

    fn external_manifest(
        distribution: serde_json::Value,
        permissions: serde_json::Value,
    ) -> crate::mcp_platform::manifest::VerifiedManifest {
        let mut value = serde_json::to_value(managed_manifest(true).manifest()).unwrap();
        value["distribution"] = distribution;
        value["permissions"] = permissions;
        parse_manifest(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn docker_manifest() -> crate::mcp_platform::manifest::VerifiedManifest {
        external_manifest(
            serde_json::json!({
                "type": "docker",
                "image": "registry.example/team/server",
                "digest": { "algorithm": "sha256", "value": "d".repeat(64) },
                "entrypoint": { "executable": "/server" }
            }),
            serde_json::json!([
                { "id": "docker", "kind": "docker", "reason": "test", "required": true },
                { "id": "spawn", "kind": "process_spawn", "reason": "test", "required": true },
                { "id": "network", "kind": "network", "reason": "test", "required": true }
            ]),
        )
    }

    fn git_manifest() -> crate::mcp_platform::manifest::VerifiedManifest {
        external_manifest(
            serde_json::json!({
                "type": "git_dev",
                "repository": "https://source.example/team/server",
                "commit": "a".repeat(40),
                "adapter": "npm",
                "entrypoint": { "executable": "bin/server" }
            }),
            serde_json::json!([
                { "id": "spawn", "kind": "process_spawn", "reason": "test", "required": true },
                { "id": "network", "kind": "network", "reason": "test", "required": true }
            ]),
        )
    }

    fn external_plan(
        verified: &crate::mcp_platform::manifest::VerifiedManifest,
    ) -> InstallationPlan {
        let selection = trusted_context_for(verified, Platform::Windows, Architecture::X86_64);
        let step = trusted_acquisition(verified.manifest(), selection)
            .unwrap()
            .unwrap();
        InstallationPlan::new(
            verified.manifest(),
            verified.digest().to_string(),
            trusted_adapter_identity(selection.adapter),
            TrustTier::Official,
            selection.operation,
            vec![step],
            EffectSummary {
                registers_connection: true,
                downloads_artifacts: true,
                writes_files: true,
                removes_files: false,
                requires_process_spawn: true,
                network_origins: Vec::new(),
                permission_ids: Vec::new(),
            },
            vec![PlanWarning::DefaultDisabled],
            Vec::new(),
            ConnectionProjection::ManagedStdio {
                name: "External test".to_string(),
                description: "External acquisition test.".to_string(),
                executable: "server-owned-runtime".to_string(),
                args: Vec::new(),
                environment_keys: Vec::new(),
                cwd: None,
                timeout_seconds: None,
            },
            PolicyDecision {
                outcome: PolicyOutcome::Allow,
                reasons: Vec::new(),
            },
        )
        .unwrap()
    }

    fn add_second_windows_artifact(
        plan: &mut InstallationPlan,
        verified: &crate::mcp_platform::manifest::VerifiedManifest,
    ) {
        let artifact = &verified.manifest().distribution.artifacts()[1];
        let PlanStep::AcquireManagedDistribution {
            kind,
            source_origin,
            expected_size_bytes,
            archive_format,
            package_name,
            package_version,
            dependency_policy,
            ..
        } = plan.steps[0].clone()
        else {
            unreachable!()
        };
        plan.steps.push(PlanStep::AcquireManagedDistribution {
            kind,
            source_origin,
            artifact_url: artifact.url.clone(),
            artifact_digest: artifact.digest.clone(),
            expected_size_bytes,
            platform: artifact.platform,
            arch: artifact.arch,
            archive_format,
            package_name,
            package_version,
            dependency_policy,
        });
        plan.capacity_budget = managed_capacity_budget(
            verified.manifest(),
            verified.digest(),
            plan.operation,
            &plan.steps,
        )
        .unwrap();
        recompute_plan_digest(plan);
    }

    fn install_plan(
        verified: &crate::mcp_platform::manifest::VerifiedManifest,
    ) -> InstallationPlan {
        install_plan_for(
            verified,
            trusted_context_for(verified, Platform::Windows, Architecture::X86_64),
        )
    }

    fn install_plan_for(
        verified: &crate::mcp_platform::manifest::VerifiedManifest,
        selection: TrustedPlanSelectionContext,
    ) -> InstallationPlan {
        let step = trusted_managed_acquisition(verified.manifest(), selection)
            .unwrap()
            .unwrap();
        InstallationPlan::new(
            verified.manifest(),
            verified.digest().to_string(),
            trusted_adapter_identity(selection.adapter),
            TrustTier::Official,
            selection.operation,
            vec![step],
            EffectSummary {
                registers_connection: true,
                downloads_artifacts: true,
                writes_files: true,
                removes_files: false,
                requires_process_spawn: true,
                network_origins: vec!["https://example.com".to_string()],
                permission_ids: Vec::new(),
            },
            vec![PlanWarning::DefaultDisabled],
            Vec::new(),
            ConnectionProjection::ManagedStdio {
                name: "Test managed manifest".to_string(),
                description: "Managed capacity planning test.".to_string(),
                executable: "bin/example".to_string(),
                args: Vec::new(),
                environment_keys: Vec::new(),
                cwd: None,
                timeout_seconds: None,
            },
            PolicyDecision {
                outcome: PolicyOutcome::Allow,
                reasons: Vec::<PolicyReason>::new(),
            },
        )
        .unwrap()
    }

    fn trusted_context_for(
        verified: &crate::mcp_platform::manifest::VerifiedManifest,
        platform: Platform,
        arch: Architecture,
    ) -> TrustedPlanSelectionContext {
        let adapter = match &verified.manifest().distribution {
            Distribution::Npm { .. } => TrustedPlanAdapter::Npm,
            Distribution::PythonWheel { .. } => TrustedPlanAdapter::PythonWheel,
            Distribution::BinaryArchive { .. } => TrustedPlanAdapter::BinaryArchive,
            Distribution::RemoteHttp => TrustedPlanAdapter::RemoteHttp,
            Distribution::ManualStdio { .. } => TrustedPlanAdapter::ManualStdio,
            Distribution::Docker { .. } => TrustedPlanAdapter::Docker,
            Distribution::GitDev { .. } => TrustedPlanAdapter::GitDev,
        };
        let operation = match &verified.manifest().distribution {
            Distribution::RemoteHttp | Distribution::ManualStdio { .. } => PlanOperation::Register,
            _ => PlanOperation::Install,
        };
        TrustedPlanSelectionContext::new(platform, arch, operation, adapter)
    }

    #[test]
    fn known_capacity_is_bound_into_the_plan_digest() {
        let verified = managed_manifest(true);
        let plan = install_plan(&verified);
        let budget = plan.capacity_budget().unwrap();
        assert_eq!(budget.metadata_digest, verified.digest());
        assert_eq!(budget.operation, PlanOperation::Install);
        assert_eq!(budget.required_peak_bytes, 100);

        let mut encoded = serde_json::to_value(&plan).unwrap();
        assert!(encoded.get("capacity_budget").is_some());
        encoded["capacity_budget"]["required_peak_bytes"] = serde_json::json!(101);
        assert!(serde_json::from_value::<InstallationPlan>(encoded).is_err());
    }

    #[test]
    fn wire_tampering_cannot_downgrade_trusted_windows_selection_to_unknown() {
        let verified = cross_platform_manifest();
        let plan = install_plan(&verified);
        let selection = trusted_context_for(&verified, Platform::Windows, Architecture::X86_64);

        for artifact in &verified.manifest().distribution.artifacts()[1..] {
            let mut wire = serde_json::to_value(&plan).unwrap();
            wire["steps"][0]["artifact_url"] = serde_json::json!(artifact.url);
            wire["steps"][0]["artifact_digest"] = serde_json::to_value(&artifact.digest).unwrap();
            wire["steps"][0]["platform"] = serde_json::to_value(artifact.platform).unwrap();
            wire["steps"][0]["arch"] = serde_json::to_value(artifact.arch).unwrap();
            wire.as_object_mut().unwrap().remove("capacity_budget");

            let decoded: InstallationPlan =
                serde_json::from_value(recompute_wire_digest(wire)).unwrap();
            let error = decoded
                .verify_trusted_acquisition_contract(&verified, selection)
                .unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
            assert!(!error.to_string().contains("example.com"));
        }
    }

    #[test]
    fn wire_tampering_of_capacity_metadata_or_operation_requires_replanning() {
        let verified = managed_manifest(true);
        let plan = install_plan(&verified);
        let selection = trusted_context_for(&verified, Platform::Windows, Architecture::X86_64);

        let mut capacity = serde_json::to_value(&plan).unwrap();
        capacity["capacity_budget"]["artifact_capacities"][0]["capacity"]["download_size_bytes"] =
            serde_json::json!(11);
        capacity["capacity_budget"]["required_peak_bytes"] = serde_json::json!(101);
        let decoded: InstallationPlan =
            serde_json::from_value(recompute_wire_digest(capacity)).unwrap();
        assert!(decoded
            .verify_trusted_acquisition_contract(&verified, selection)
            .is_err());

        let mut operation = serde_json::to_value(&plan).unwrap();
        operation["operation"] = serde_json::json!("update");
        operation["capacity_budget"]["operation"] = serde_json::json!("update");
        let decoded: InstallationPlan =
            serde_json::from_value(recompute_wire_digest(operation)).unwrap();
        assert!(decoded
            .verify_trusted_acquisition_contract(&verified, selection)
            .is_err());
    }

    #[test]
    fn wire_identity_tampering_with_a_recomputed_digest_reaches_and_fails_trusted_preflight() {
        let verified = managed_manifest(true);
        let plan = install_plan(&verified);
        let selection = trusted_context_for(&verified, Platform::Windows, Architecture::X86_64);

        let mut url = serde_json::to_value(&plan).unwrap();
        url["steps"][0]["artifact_url"] = serde_json::json!("https://invalid.example/artifact");
        url["capacity_budget"]["artifact_capacities"][0]["artifact_url"] =
            serde_json::json!("https://invalid.example/artifact");
        let decoded: InstallationPlan = serde_json::from_value(recompute_wire_digest(url)).unwrap();
        assert!(decoded
            .verify_trusted_acquisition_contract(&verified, selection)
            .is_err());

        let mut digest = serde_json::to_value(&plan).unwrap();
        digest["steps"][0]["artifact_digest"]["value"] = serde_json::json!("b".repeat(64));
        digest["capacity_budget"]["artifact_capacities"][0]["artifact_digest"]["value"] =
            serde_json::json!("b".repeat(64));
        let decoded: InstallationPlan =
            serde_json::from_value(recompute_wire_digest(digest)).unwrap();
        assert!(decoded
            .verify_trusted_acquisition_contract(&verified, selection)
            .is_err());

        let mut arch = serde_json::to_value(&plan).unwrap();
        arch["steps"][0]["arch"] = serde_json::json!("aarch64");
        arch["capacity_budget"]["artifact_capacities"][0]["arch"] = serde_json::json!("aarch64");
        let decoded: InstallationPlan =
            serde_json::from_value(recompute_wire_digest(arch)).unwrap();
        assert!(decoded
            .verify_trusted_acquisition_contract(&verified, selection)
            .is_err());

        let mut duplicate = serde_json::to_value(&plan).unwrap();
        let step = duplicate["steps"][0].clone();
        duplicate["steps"].as_array_mut().unwrap().push(step);
        assert!(
            serde_json::from_value::<InstallationPlan>(recompute_wire_digest(duplicate)).is_err()
        );

        let mut inserted = serde_json::to_value(&plan).unwrap();
        let binding = inserted["capacity_budget"]["artifact_capacities"][0].clone();
        inserted["capacity_budget"]["artifact_capacities"]
            .as_array_mut()
            .unwrap()
            .push(binding);
        inserted["capacity_budget"]["required_peak_bytes"] = serde_json::json!(200);
        assert!(
            serde_json::from_value::<InstallationPlan>(recompute_wire_digest(inserted)).is_err()
        );

        let mut overflow = serde_json::to_value(&plan).unwrap();
        overflow["capacity_budget"]["artifact_capacities"][0]["capacity"]["download_size_bytes"] =
            serde_json::json!(u64::MAX);
        overflow["capacity_budget"]["required_peak_bytes"] = serde_json::json!(u64::MAX);
        assert!(
            serde_json::from_value::<InstallationPlan>(recompute_wire_digest(overflow)).is_err()
        );
    }

    #[test]
    fn reordered_or_inserted_managed_acquisitions_are_rejected_from_wire() {
        let verified = cross_platform_manifest();
        let plan = install_plan(&verified);
        let artifacts = verified.manifest().distribution.artifacts();
        let mut wire = serde_json::to_value(&plan).unwrap();
        wire.as_object_mut().unwrap().remove("capacity_budget");

        let first = &artifacts[1];
        wire["steps"][0]["artifact_url"] = serde_json::json!(first.url);
        wire["steps"][0]["artifact_digest"] = serde_json::to_value(&first.digest).unwrap();
        wire["steps"][0]["platform"] = serde_json::to_value(first.platform).unwrap();
        wire["steps"][0]["arch"] = serde_json::to_value(first.arch).unwrap();

        let second = &artifacts[2];
        let mut inserted = wire["steps"][0].clone();
        inserted["artifact_url"] = serde_json::json!(second.url);
        inserted["artifact_digest"] = serde_json::to_value(&second.digest).unwrap();
        inserted["platform"] = serde_json::to_value(second.platform).unwrap();
        inserted["arch"] = serde_json::to_value(second.arch).unwrap();
        wire["steps"].as_array_mut().unwrap().push(inserted);

        assert!(
            serde_json::from_value::<InstallationPlan>(recompute_wire_digest(wire.clone()))
                .is_err()
        );
        wire["steps"].as_array_mut().unwrap().reverse();
        assert!(serde_json::from_value::<InstallationPlan>(recompute_wire_digest(wire)).is_err());
    }

    #[test]
    fn recomputed_digest_does_not_bypass_capacity_budget_invariants() {
        let plan = install_plan(&managed_manifest(true));

        let mut metadata = plan.clone();
        metadata.capacity_budget.as_mut().unwrap().metadata_digest = "b".repeat(64);
        recompute_plan_digest(&mut metadata);
        assert_eq!(
            metadata.verify_integrity().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );

        let mut operation = plan.clone();
        operation.operation = PlanOperation::Uninstall;
        recompute_plan_digest(&mut operation);
        assert_eq!(
            operation.verify_integrity().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );

        let mut capacities = plan.clone();
        capacities
            .capacity_budget
            .as_mut()
            .unwrap()
            .artifact_capacities
            .clear();
        recompute_plan_digest(&mut capacities);
        assert_eq!(
            capacities.verify_integrity().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );

        let mut peak = plan;
        peak.capacity_budget.as_mut().unwrap().required_peak_bytes = 99;
        recompute_plan_digest(&mut peak);
        assert_eq!(
            peak.verify_integrity().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    #[test]
    fn non_windows_managed_artifacts_have_unknown_capacity() {
        for platform in ["linux", "any"] {
            let mut manifest = serde_json::to_value(managed_manifest(true).manifest()).unwrap();
            manifest["distribution"]["artifacts"][0]["platform"] = serde_json::json!(platform);
            let verified = parse_manifest(&serde_json::to_vec(&manifest).unwrap()).unwrap();
            let trusted_platform = if platform == "linux" {
                Platform::Linux
            } else {
                Platform::Any
            };
            let plan = install_plan_for(
                &verified,
                trusted_context_for(&verified, trusted_platform, Architecture::X86_64),
            );
            assert!(plan.capacity_budget().is_none());
        }
    }

    #[test]
    fn explicit_windows_legacy_capacity_requires_replanning() {
        let verified = managed_manifest(false);
        let plan = install_plan(&verified);
        assert!(plan.capacity_budget().is_none());
        assert!(serde_json::to_value(&plan)
            .unwrap()
            .get("capacity_budget")
            .is_none());
        assert_eq!(
            plan.verify_trusted_acquisition_contract(
                &verified,
                trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
            )
            .unwrap_err()
            .code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    #[test]
    fn manifest_aware_capacity_contract_binds_each_windows_artifact() {
        for adapter in ["npm", "python_wheel", "binary_archive"] {
            let verified = managed_manifest_for(adapter, true);
            let plan = install_plan(&verified);
            assert_eq!(
                plan.verify_trusted_acquisition_contract(
                    &verified,
                    trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
                )
                .unwrap(),
                ManagedCapacityContractStatus::Known {
                    required_peak_bytes: 100
                }
            );
        }
    }

    #[test]
    fn deleted_windows_capacity_budget_requires_replanning_even_with_a_new_digest() {
        let verified = managed_manifest(true);
        let mut plan = install_plan(&verified);
        plan.capacity_budget = None;
        recompute_plan_digest(&mut plan);

        assert_eq!(
            plan.verify_integrity().unwrap_err().code(),
            McpPlatformErrorCode::IntegrityError
        );
        assert_eq!(
            plan.verify_trusted_acquisition_contract(
                &verified,
                trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
            )
            .unwrap_err()
            .code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    #[test]
    fn manifest_aware_capacity_contract_rejects_recomputed_budget_tampering() {
        let verified = managed_manifest(true);
        let plan = install_plan(&verified);

        let mut same_peak = plan.clone();
        same_peak
            .capacity_budget
            .as_mut()
            .unwrap()
            .artifact_capacities[0]
            .capacity = ArtifactCapacity {
            download_size_bytes: 1,
            materialized_size_bytes: 1,
            workspace_size_bytes: 1,
            rollback_extra_bytes: 97,
        };
        recompute_plan_digest(&mut same_peak);
        assert_eq!(
            same_peak
                .verify_trusted_acquisition_contract(
                    &verified,
                    trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
                )
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::IntegrityError
        );

        let identity_mutations: [fn(&mut ArtifactCapacityBinding); 4] = [
            |binding: &mut ArtifactCapacityBinding| binding.artifact_url.push_str("-changed"),
            |binding: &mut ArtifactCapacityBinding| binding.artifact_digest.value = "b".repeat(64),
            |binding: &mut ArtifactCapacityBinding| binding.platform = Platform::Linux,
            |binding: &mut ArtifactCapacityBinding| binding.arch = Architecture::Aarch64,
        ];
        for mutate in identity_mutations {
            let mut altered = plan.clone();
            mutate(
                &mut altered
                    .capacity_budget
                    .as_mut()
                    .unwrap()
                    .artifact_capacities[0],
            );
            recompute_plan_digest(&mut altered);
            assert_eq!(
                altered
                    .verify_trusted_acquisition_contract(
                        &verified,
                        trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
                    )
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::IntegrityError
            );
        }

        let capacity_mutations: [fn(&mut ArtifactCapacity); 4] = [
            |capacity: &mut ArtifactCapacity| capacity.download_size_bytes = 11,
            |capacity: &mut ArtifactCapacity| capacity.materialized_size_bytes = 21,
            |capacity: &mut ArtifactCapacity| capacity.workspace_size_bytes = 31,
            |capacity: &mut ArtifactCapacity| capacity.rollback_extra_bytes = 41,
        ];
        for mutate in capacity_mutations {
            let mut altered = plan.clone();
            mutate(
                &mut altered
                    .capacity_budget
                    .as_mut()
                    .unwrap()
                    .artifact_capacities[0]
                    .capacity,
            );
            altered
                .capacity_budget
                .as_mut()
                .unwrap()
                .required_peak_bytes = 101;
            recompute_plan_digest(&mut altered);
            assert_eq!(
                altered
                    .verify_trusted_acquisition_contract(
                        &verified,
                        trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
                    )
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::IntegrityError
            );
        }

        let mut overflow = plan;
        overflow
            .capacity_budget
            .as_mut()
            .unwrap()
            .artifact_capacities[0]
            .capacity
            .download_size_bytes = u64::MAX;
        recompute_plan_digest(&mut overflow);
        let error = overflow
            .verify_trusted_acquisition_contract(
                &verified,
                trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
        assert!(!error.to_string().contains("example.com"));
    }

    #[test]
    fn manifest_aware_capacity_contract_rejects_inserted_deleted_or_repeated_bindings() {
        let verified = managed_manifest(true);
        let plan = install_plan(&verified);

        let mut deleted = plan.clone();
        deleted
            .capacity_budget
            .as_mut()
            .unwrap()
            .artifact_capacities
            .clear();
        recompute_plan_digest(&mut deleted);
        assert!(deleted
            .verify_trusted_acquisition_contract(
                &verified,
                trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
            )
            .is_err());

        let mut inserted = plan.clone();
        let binding = inserted
            .capacity_budget
            .as_ref()
            .unwrap()
            .artifact_capacities[0]
            .clone();
        inserted
            .capacity_budget
            .as_mut()
            .unwrap()
            .artifact_capacities
            .push(binding);
        inserted
            .capacity_budget
            .as_mut()
            .unwrap()
            .required_peak_bytes = 200;
        recompute_plan_digest(&mut inserted);
        assert!(inserted
            .verify_trusted_acquisition_contract(
                &verified,
                trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
            )
            .is_err());
    }

    #[test]
    fn multiple_managed_acquisitions_require_replanning() {
        let verified = two_windows_manifest();
        let mut plan = install_plan(&verified);
        let extra = trusted_managed_acquisition(
            verified.manifest(),
            trusted_context_for(&verified, Platform::Windows, Architecture::Aarch64),
        )
        .unwrap()
        .unwrap();
        plan.steps.push(extra);
        recompute_plan_digest(&mut plan);
        assert_eq!(
            plan.verify_trusted_acquisition_contract(
                &verified,
                trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
            )
            .unwrap_err()
            .code(),
            McpPlatformErrorCode::IntegrityError
        );
    }

    #[test]
    fn trusted_contract_rejects_complete_acquisition_wire_tampering() {
        let managed = managed_manifest(true);
        let managed_plan = install_plan(&managed);
        let docker = docker_manifest();
        let docker_plan = external_plan(&docker);
        let git = git_manifest();
        let git_plan = external_plan(&git);

        for injected in [
            serde_json::to_value(&docker_plan).unwrap()["steps"][0].clone(),
            serde_json::to_value(&git_plan).unwrap()["steps"][0].clone(),
        ] {
            let mut wire = serde_json::to_value(&managed_plan).unwrap();
            wire["steps"]
                .as_array_mut()
                .unwrap()
                .insert(0, injected.clone());
            assert!(
                serde_json::from_value::<InstallationPlan>(recompute_wire_digest(wire)).is_err()
            );

            let mut wire = serde_json::to_value(&managed_plan).unwrap();
            wire["steps"].as_array_mut().unwrap().push(injected);
            assert!(
                serde_json::from_value::<InstallationPlan>(recompute_wire_digest(wire)).is_err()
            );
        }

        let managed_mutations: [fn(&mut serde_json::Value); 11] = [
            |step| step["kind"] = serde_json::json!("python_wheel"),
            |step| step["source_origin"] = serde_json::json!("https://attacker.example"),
            |step| step["artifact_url"] = serde_json::json!("https://attacker.example/package"),
            |step| step["artifact_digest"]["value"] = serde_json::json!("b".repeat(64)),
            |step| step["expected_size_bytes"] = serde_json::json!(1),
            |step| step["platform"] = serde_json::json!("linux"),
            |step| step["arch"] = serde_json::json!("aarch64"),
            |step| step["archive_format"] = serde_json::json!("tar.gz"),
            |step| step["package_name"] = serde_json::json!("attacker-package"),
            |step| step["package_version"] = serde_json::json!("9.9.9"),
            |step| step["dependency_policy"] = serde_json::json!("allow_implicit"),
        ];
        for mutate in managed_mutations {
            let mut wire = serde_json::to_value(&managed_plan).unwrap();
            mutate(&mut wire["steps"][0]);
            let decoded = serde_json::from_value::<InstallationPlan>(recompute_wire_digest(wire));
            match decoded {
                Ok(plan) => assert!(plan
                    .verify_trusted_acquisition_contract(
                        &managed,
                        trusted_context_for(&managed, Platform::Windows, Architecture::X86_64),
                    )
                    .is_err()),
                Err(error) => assert!(!error.to_string().contains("attacker")),
            }
        }

        for (verified, plan, secret) in [
            (docker, docker_plan, "registry.example/team/server"),
            (git, git_plan, "source.example/team/server"),
        ] {
            let selection = trusted_context_for(&verified, Platform::Windows, Architecture::X86_64);
            assert_eq!(
                plan.verify_trusted_acquisition_contract(&verified, selection)
                    .unwrap(),
                ManagedCapacityContractStatus::Unknown
            );

            let mut duplicate = serde_json::to_value(&plan).unwrap();
            let step = duplicate["steps"][0].clone();
            duplicate["steps"].as_array_mut().unwrap().push(step);
            assert!(
                serde_json::from_value::<InstallationPlan>(recompute_wire_digest(duplicate))
                    .is_err()
            );

            let mut changed = serde_json::to_value(&plan).unwrap();
            let step = &mut changed["steps"][0];
            if step["type"] == "acquire_docker_distribution" {
                step["image"] = serde_json::json!("registry.example/attacker/image");
                step["digest"]["value"] = serde_json::json!("e".repeat(64));
                step["mounts"] = serde_json::json!([{ "source_permission": "docker", "target": "/tmp", "read_only": true }]);
                step["mount_plan_digest"] = serde_json::json!("f".repeat(64));
            } else {
                step["repository_origin"] = serde_json::json!("https://attacker.example");
                step["repository"] = serde_json::json!("https://attacker.example/team/server");
                step["commit"] = serde_json::json!("b".repeat(40));
                step["subdirectory"] = serde_json::json!("attacker");
                step["underlying_adapter"] = serde_json::json!("python_wheel");
                step["acquisition_digest"] = serde_json::json!("c".repeat(64));
            }
            let decoded: InstallationPlan =
                serde_json::from_value(recompute_wire_digest(changed)).unwrap();
            let error = decoded
                .verify_trusted_acquisition_contract(&verified, selection)
                .unwrap_err();
            assert_eq!(error.code(), McpPlatformErrorCode::IntegrityError);
            assert!(!error.to_string().contains(secret));
            assert!(!error.to_string().contains("attacker"));
        }

        let remote = remote_manifest();
        let remote_plan = remote_plan(&remote);
        let mut remote_wire = serde_json::to_value(&remote_plan).unwrap();
        remote_wire["steps"].as_array_mut().unwrap().insert(
            0,
            serde_json::to_value(&external_plan(&docker_manifest())).unwrap()["steps"][0].clone(),
        );
        assert!(
            serde_json::from_value::<InstallationPlan>(recompute_wire_digest(remote_wire)).is_err()
        );

        let manual = manual_manifest();
        let manual_plan = manual_plan(&manual);
        assert_eq!(
            manual_plan
                .verify_trusted_acquisition_contract(
                    &manual,
                    trusted_context_for(&manual, Platform::Windows, Architecture::X86_64),
                )
                .unwrap(),
            ManagedCapacityContractStatus::Unknown
        );
        let mut manual_wire = serde_json::to_value(&manual_plan).unwrap();
        manual_wire["steps"].as_array_mut().unwrap().push(
            serde_json::to_value(&external_plan(&git_manifest())).unwrap()["steps"][0].clone(),
        );
        assert!(
            serde_json::from_value::<InstallationPlan>(recompute_wire_digest(manual_wire)).is_err()
        );
    }

    #[test]
    fn non_windows_and_remote_plans_remain_manifest_aware_unknown() {
        for platform in ["linux", "any"] {
            let mut manifest = serde_json::to_value(managed_manifest(true).manifest()).unwrap();
            manifest["distribution"]["artifacts"][0]["platform"] = serde_json::json!(platform);
            let verified = parse_manifest(&serde_json::to_vec(&manifest).unwrap()).unwrap();
            let trusted_platform = if platform == "linux" {
                Platform::Linux
            } else {
                Platform::Any
            };
            let plan = install_plan_for(
                &verified,
                trusted_context_for(&verified, trusted_platform, Architecture::X86_64),
            );
            assert_eq!(
                plan.verify_trusted_acquisition_contract(
                    &verified,
                    trusted_context_for(&verified, trusted_platform, Architecture::X86_64),
                )
                .unwrap(),
                ManagedCapacityContractStatus::Unknown
            );
        }

        let verified = remote_manifest();
        let plan = remote_plan(&verified);
        assert_eq!(
            plan.verify_trusted_acquisition_contract(
                &verified,
                trusted_context_for(&verified, Platform::Windows, Architecture::X86_64),
            )
            .unwrap(),
            ManagedCapacityContractStatus::Unknown
        );
    }

    #[test]
    fn windows_secure_launcher_gate_blocks_only_managed_local_projections() {
        let managed = ConnectionProjection::ManagedStdio {
            name: "managed".to_string(),
            description: "managed".to_string(),
            executable: "managed.exe".to_string(),
            args: Vec::new(),
            environment_keys: Vec::new(),
            cwd: None,
            timeout_seconds: None,
        };
        let docker = ConnectionProjection::ManagedDockerStdio {
            name: "managed-docker".to_string(),
            description: "managed docker".to_string(),
            executable: "docker".to_string(),
            args: Vec::new(),
            cwd: None,
            timeout_seconds: None,
        };
        let remote = ConnectionProjection::RemoteHttp {
            name: "remote".to_string(),
            description: "remote".to_string(),
            uri: "https://example.invalid/mcp".to_string(),
            timeout_seconds: None,
        };

        assert_eq!(managed.requires_windows_secure_launcher(), cfg!(windows));
        assert_eq!(docker.requires_windows_secure_launcher(), cfg!(windows));
        assert!(!remote.requires_windows_secure_launcher());
        assert!(remote.require_runtime_transport().is_ok());

        #[cfg(windows)]
        for projection in [&managed, &docker] {
            let error = projection.require_runtime_transport().unwrap_err();
            assert_eq!(
                error.code(),
                McpPlatformErrorCode::RuntimeControlUnavailable
            );
        }
    }
}
