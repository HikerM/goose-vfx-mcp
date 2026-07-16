use std::collections::HashMap;

use serde::Serialize;

use crate::agents::extension::Envs;
use crate::agents::ExtensionConfig;

use super::domain::TrustTier;
use super::error::McpPlatformResult;
use super::manifest::{digest_serializable, Auth, HealthCheck, Manifest};
use super::policy::{PlanOperation, PolicyDecision, PolicyReasonCode};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterIdentity {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequiredConfirmation {
    Policy { reason_code: PolicyReasonCode },
    Permission { permission_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanWarning {
    DefaultDisabled,
    RegistrationOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationPlan {
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
