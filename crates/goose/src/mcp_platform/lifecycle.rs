use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::agents::ExtensionConfig;
use crate::config::extensions::ExtensionEntry;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::managed_remote::{CoreManagedRemoteHttpNetworkPolicy, RemoteHttpNetworkPolicy};
use super::manifest::{Auth, Distribution, HealthCheck, Manifest, Transport};
use super::plan::ConnectionProjection;
use super::{HealthResultCode, InstallationPlan};

pub const CONNECTION_ADAPTER_VERSION: &str = "1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectSpawnDescriptor {
    pub executable: String,
    pub argv: Vec<String>,
    pub environment_keys: Vec<String>,
    pub working_directory: Option<String>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistrationEffect {
    RemoteHttp {
        endpoint: String,
        allowed_redirect_origins: Vec<String>,
        timeout_seconds: Option<u64>,
    },
    ManualStdio {
        spawn: DirectSpawnDescriptor,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationEffectEvidence {
    pub adapter_id: String,
    pub adapter_version: String,
}

#[async_trait]
pub trait RegistrationEffectAdapter: Send + Sync {
    fn adapter_id(&self) -> &'static str;
    fn adapter_version(&self) -> &'static str;
    async fn verify(
        &self,
        effect: &RegistrationEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<RegistrationEffectEvidence>;
}

pub trait HostIntegrationAdapter: Send + Sync {
    fn is_empty(&self, manifest: &Manifest) -> bool;
}

pub trait TransportProjectionAdapter: Send + Sync {
    fn adapter_id(&self) -> &'static str;
    fn adapter_version(&self) -> &'static str;
    fn registration_effect(
        &self,
        manifest: &Manifest,
        plan: &InstallationPlan,
    ) -> McpPlatformResult<RegistrationEffect>;

    fn extension_config(
        &self,
        projection: &ConnectionProjection,
        stable_key: &str,
    ) -> McpPlatformResult<ExtensionConfig>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthRequirement {
    Ready,
    MissingOpaqueHandle { credential_name: String },
}

pub trait AuthRequirementResolver: Send + Sync {
    fn requirement(&self, auth: &Auth) -> McpPlatformResult<AuthRequirement>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct HealthExecution {
    pub effect: RegistrationEffect,
    pub check: HealthCheck,
    pub projection_config: ExtensionConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthAdapterResult {
    pub result_code: HealthResultCode,
    pub latency_ms: i64,
    pub capabilities_digest: Option<String>,
    pub tools_digest: Option<String>,
    pub detail_code: super::HealthDetailCode,
}

#[async_trait]
pub trait HealthCheckAdapter: Send + Sync {
    fn adapter_id(&self) -> &'static str;
    fn adapter_version(&self) -> &'static str;
    async fn run(
        &self,
        execution: HealthExecution,
        cancellation: CancellationToken,
    ) -> McpPlatformResult<HealthAdapterResult>;
}

#[async_trait]
pub trait HealthCheckSession: Send + Sync {
    async fn open(&self) -> McpPlatformResult<()>;
    async fn initialize(&self) -> McpPlatformResult<()>;
    async fn list_tools(&self) -> McpPlatformResult<Option<String>>;
    async fn close(&self) -> McpPlatformResult<()>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionSnapshot {
    pub entry: ExtensionEntry,
    pub created: bool,
}

#[async_trait]
pub trait ProjectionSink: Send + Sync {
    fn adapter_id(&self) -> &'static str;
    fn adapter_version(&self) -> &'static str;
    async fn put_disabled(
        &self,
        key: &str,
        config: ExtensionConfig,
    ) -> McpPlatformResult<ProjectionSnapshot>;
    async fn get(&self, key: &str) -> McpPlatformResult<Option<ProjectionSnapshot>>;
    async fn set_enabled(&self, key: &str, enabled: bool) -> McpPlatformResult<ProjectionSnapshot>;
    async fn remove_owned(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
    ) -> McpPlatformResult<bool>;
    async fn replace_owned_disabled(
        &self,
        _key: &str,
        _expected: &ProjectionSnapshot,
        _config: ExtensionConfig,
    ) -> McpPlatformResult<ProjectionSnapshot> {
        Err(projection_conflict())
    }
    async fn replace_owned(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
        config: ExtensionConfig,
        enabled: bool,
    ) -> McpPlatformResult<ProjectionSnapshot> {
        let snapshot = self.replace_owned_disabled(key, expected, config).await?;
        if enabled {
            self.set_enabled(key, true).await
        } else {
            Ok(snapshot)
        }
    }
}

pub struct SafeRegistrationEffectAdapter {
    remote_http: Arc<dyn RemoteHttpNetworkPolicy>,
}

impl Default for SafeRegistrationEffectAdapter {
    fn default() -> Self {
        Self::new(Arc::new(CoreManagedRemoteHttpNetworkPolicy::default()))
    }
}

impl SafeRegistrationEffectAdapter {
    pub fn new(remote_http: Arc<dyn RemoteHttpNetworkPolicy>) -> Self {
        Self { remote_http }
    }
}

#[async_trait]
impl RegistrationEffectAdapter for SafeRegistrationEffectAdapter {
    fn adapter_id(&self) -> &'static str {
        "connection_registration"
    }

    fn adapter_version(&self) -> &'static str {
        CONNECTION_ADAPTER_VERSION
    }

    async fn verify(
        &self,
        effect: &RegistrationEffect,
        cancellation: &CancellationToken,
    ) -> McpPlatformResult<RegistrationEffectEvidence> {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        match effect {
            RegistrationEffect::RemoteHttp {
                endpoint,
                allowed_redirect_origins,
                timeout_seconds,
            } => {
                if !allowed_redirect_origins.is_empty() {
                    return Err(unsafe_effect());
                }
                self.remote_http
                    .secure_client(endpoint, Duration::from_secs(timeout_seconds.unwrap_or(30)))
                    .await?;
            }
            RegistrationEffect::ManualStdio { spawn } => {
                if spawn.executable.is_empty() || spawn.executable.contains('\0') {
                    return Err(unsafe_effect());
                }
                if spawn.argv.iter().any(|argument| argument.contains('\0')) {
                    return Err(unsafe_effect());
                }
            }
        }
        Ok(RegistrationEffectEvidence {
            adapter_id: self.adapter_id().to_string(),
            adapter_version: self.adapter_version().to_string(),
        })
    }
}

#[derive(Debug, Default)]
pub struct EmptyHostIntegrationAdapter;

impl HostIntegrationAdapter for EmptyHostIntegrationAdapter {
    fn is_empty(&self, manifest: &Manifest) -> bool {
        manifest.host_integrations.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct CoreTransportProjectionAdapter;

impl TransportProjectionAdapter for CoreTransportProjectionAdapter {
    fn adapter_id(&self) -> &'static str {
        "core_transport_projection"
    }
    fn adapter_version(&self) -> &'static str {
        "1"
    }
    fn registration_effect(
        &self,
        manifest: &Manifest,
        plan: &InstallationPlan,
    ) -> McpPlatformResult<RegistrationEffect> {
        plan.verify_integrity()?;
        if plan.manifest_id() != manifest.id || plan.manifest_version() != manifest.version.as_str()
        {
            return Err(integrity_error());
        }
        match (&manifest.distribution, &manifest.transport) {
            (
                Distribution::RemoteHttp,
                Transport::StreamableHttp {
                    url,
                    connect_timeout_seconds,
                    allowed_redirect_origins,
                },
            ) => {
                let endpoint = Url::parse(url).map_err(|_| unsafe_effect())?;
                if endpoint.scheme() != "https"
                    || endpoint.host_str().is_none()
                    || !allowed_redirect_origins.is_empty()
                {
                    return Err(unsafe_effect());
                }
                Ok(RegistrationEffect::RemoteHttp {
                    endpoint: url.clone(),
                    allowed_redirect_origins: Vec::new(),
                    timeout_seconds: *connect_timeout_seconds,
                })
            }
            (
                Distribution::ManualStdio { entrypoint, .. },
                Transport::Stdio {
                    startup_timeout_seconds,
                },
            ) => Ok(RegistrationEffect::ManualStdio {
                spawn: DirectSpawnDescriptor {
                    executable: entrypoint.executable.clone(),
                    argv: entrypoint.args.clone(),
                    environment_keys: entrypoint.environment_keys.clone(),
                    working_directory: entrypoint.cwd.clone(),
                    timeout_seconds: *startup_timeout_seconds,
                },
            }),
            (
                Distribution::Npm { entrypoint, .. }
                | Distribution::PythonWheel { entrypoint, .. }
                | Distribution::BinaryArchive { entrypoint, .. },
                Transport::Stdio {
                    startup_timeout_seconds,
                },
            ) => Ok(RegistrationEffect::ManualStdio {
                spawn: DirectSpawnDescriptor {
                    executable: entrypoint.executable.clone(),
                    argv: entrypoint.args.clone(),
                    environment_keys: entrypoint.environment_keys.clone(),
                    working_directory: entrypoint.cwd.clone(),
                    timeout_seconds: *startup_timeout_seconds,
                },
            }),
            _ => Err(integrity_error()),
        }
    }

    fn extension_config(
        &self,
        projection: &ConnectionProjection,
        stable_key: &str,
    ) -> McpPlatformResult<ExtensionConfig> {
        if stable_key.is_empty() {
            return Err(integrity_error());
        }
        let config = match projection {
            ConnectionProjection::RemoteHttp {
                description,
                uri,
                timeout_seconds,
                ..
            } => {
                let mut config = ConnectionProjection::RemoteHttp {
                    name: stable_key.to_string(),
                    description: description.clone(),
                    uri: uri.clone(),
                    timeout_seconds: *timeout_seconds,
                }
                .to_extension_config();
                if let ExtensionConfig::ManagedStreamableHttp { name, .. } = &mut config {
                    *name = stable_key.to_string();
                }
                config
            }
            ConnectionProjection::ManualStdio {
                description,
                executable,
                args,
                environment_keys,
                cwd,
                timeout_seconds,
                ..
            } => ConnectionProjection::ManualStdio {
                name: stable_key.to_string(),
                description: description.clone(),
                executable: executable.clone(),
                args: args.clone(),
                environment_keys: environment_keys.clone(),
                cwd: cwd.clone(),
                timeout_seconds: *timeout_seconds,
            }
            .to_extension_config(),
            ConnectionProjection::ManagedStdio {
                name: _,
                description,
                executable,
                args,
                environment_keys,
                cwd,
                timeout_seconds,
            } => ConnectionProjection::ManagedStdio {
                name: stable_key.to_string(),
                description: description.clone(),
                executable: executable.clone(),
                args: args.clone(),
                environment_keys: environment_keys.clone(),
                cwd: cwd.clone(),
                timeout_seconds: *timeout_seconds,
            }
            .to_extension_config(),
            ConnectionProjection::ManagedDockerStdio {
                name: _,
                description,
                executable,
                args,
                cwd,
                timeout_seconds,
            } => ConnectionProjection::ManagedDockerStdio {
                name: stable_key.to_string(),
                description: description.clone(),
                executable: executable.clone(),
                args: args.clone(),
                cwd: cwd.clone(),
                timeout_seconds: *timeout_seconds,
            }
            .to_extension_config(),
        };
        Ok(config)
    }
}

#[derive(Debug, Default)]
pub struct ConfigAuthRequirementResolver;

impl AuthRequirementResolver for ConfigAuthRequirementResolver {
    fn requirement(&self, auth: &Auth) -> McpPlatformResult<AuthRequirement> {
        let credential_name = match auth {
            Auth::None => return Ok(AuthRequirement::Ready),
            Auth::ApiKeyHeader {
                credential_name, ..
            }
            | Auth::Environment {
                credential_name, ..
            } => credential_name,
            Auth::Oauth2 { .. } => {
                return Ok(AuthRequirement::MissingOpaqueHandle {
                    credential_name: "oauth2".to_string(),
                })
            }
        };
        let key = credential_name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        match crate::config::Config::global().get(&key, true) {
            Ok(value) if value.as_str().is_some_and(|value| !value.is_empty()) => {
                Ok(AuthRequirement::Ready)
            }
            _ => Ok(AuthRequirement::MissingOpaqueHandle {
                credential_name: credential_name.clone(),
            }),
        }
    }
}

#[derive(Default)]
pub struct ConfigProjectionSink {
    config: Option<Arc<crate::config::Config>>,
}

impl ConfigProjectionSink {
    pub fn with_config(config: Arc<crate::config::Config>) -> Self {
        Self {
            config: Some(config),
        }
    }

    fn config(&self) -> &crate::config::Config {
        match self.config.as_deref() {
            Some(config) => config,
            None => crate::config::Config::global(),
        }
    }
}

#[async_trait]
impl ProjectionSink for ConfigProjectionSink {
    fn adapter_id(&self) -> &'static str {
        "extension_config_sink"
    }
    fn adapter_version(&self) -> &'static str {
        "1"
    }
    async fn put_disabled(
        &self,
        key: &str,
        config: ExtensionConfig,
    ) -> McpPlatformResult<ProjectionSnapshot> {
        let entry = ExtensionEntry {
            enabled: false,
            config,
        };
        let created =
            match crate::config::extensions::try_create_managed_extension_at_key_with_config(
                self.config(),
                key,
                entry.clone(),
            ) {
                Ok(crate::config::extensions::ManagedExtensionCreateOutcome::Created) => true,
                Ok(crate::config::extensions::ManagedExtensionCreateOutcome::IdempotentReplay) => {
                    false
                }
                Err(_) => {
                    let Some(existing) =
                        crate::config::extensions::get_extension_entry_by_key_with_config(
                            self.config(),
                            key,
                        )
                    else {
                        return Err(projection_conflict());
                    };
                    if !key.starts_with("managed_mcp_")
                        || !matches!(&existing.config, ExtensionConfig::StreamableHttp { .. })
                        || !matches!(&entry.config, ExtensionConfig::ManagedStreamableHttp { .. })
                    {
                        return Err(projection_conflict());
                    }
                    crate::config::extensions::try_replace_managed_extension_at_key_with_config(
                        self.config(),
                        key,
                        &existing,
                        entry.clone(),
                    )
                    .map_err(|_| projection_conflict())?;
                    false
                }
            };
        Ok(ProjectionSnapshot { entry, created })
    }

    async fn get(&self, key: &str) -> McpPlatformResult<Option<ProjectionSnapshot>> {
        Ok(
            crate::config::extensions::get_extension_entry_by_key_with_config(self.config(), key)
                .map(|entry| ProjectionSnapshot {
                    entry,
                    created: false,
                }),
        )
    }

    async fn set_enabled(&self, key: &str, enabled: bool) -> McpPlatformResult<ProjectionSnapshot> {
        if !crate::config::extensions::try_set_platform_extension_enabled_with_config(
            self.config(),
            key,
            enabled,
        )
        .map_err(|_| projection_failed())?
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::NotFound,
                "managed extension projection was not found",
            ));
        }
        self.get(key).await?.ok_or_else(|| {
            McpPlatformError::new(
                McpPlatformErrorCode::NotFound,
                "managed extension projection was not found",
            )
        })
    }

    async fn remove_owned(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
    ) -> McpPlatformResult<bool> {
        let Some(existing) =
            crate::config::extensions::get_extension_entry_by_key_with_config(self.config(), key)
        else {
            return Ok(false);
        };
        if existing.config != expected.entry.config || existing.enabled != expected.entry.enabled {
            return Err(projection_conflict());
        }
        crate::config::extensions::try_remove_platform_extension_with_config(self.config(), key)
            .map_err(|_| projection_failed())?;
        Ok(true)
    }

    async fn replace_owned_disabled(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
        config: ExtensionConfig,
    ) -> McpPlatformResult<ProjectionSnapshot> {
        let entry = ExtensionEntry {
            enabled: false,
            config,
        };
        crate::config::extensions::try_replace_managed_extension_at_key_with_config(
            self.config(),
            key,
            &expected.entry,
            entry.clone(),
        )
        .map_err(|_| projection_conflict())?;
        Ok(ProjectionSnapshot {
            entry,
            created: false,
        })
    }

    async fn replace_owned(
        &self,
        key: &str,
        expected: &ProjectionSnapshot,
        config: ExtensionConfig,
        enabled: bool,
    ) -> McpPlatformResult<ProjectionSnapshot> {
        let entry = ExtensionEntry { enabled, config };
        crate::config::extensions::try_replace_managed_extension_at_key_with_config(
            self.config(),
            key,
            &expected.entry,
            entry.clone(),
        )
        .map_err(|_| projection_conflict())?;
        Ok(ProjectionSnapshot {
            entry,
            created: false,
        })
    }
}

#[derive(Clone)]
pub struct LifecyclePorts {
    pub registration: Arc<dyn RegistrationEffectAdapter>,
    pub host_integration: Arc<dyn HostIntegrationAdapter>,
    pub transport: Arc<dyn TransportProjectionAdapter>,
    pub auth: Arc<dyn AuthRequirementResolver>,
    pub health: Arc<dyn HealthCheckAdapter>,
    pub projection_sink: Arc<dyn ProjectionSink>,
}

const fn integrity_error() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::IntegrityError,
        "MCP lifecycle input failed immutable integrity validation",
    )
}

const fn unsafe_effect() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PolicyDenied,
        "MCP lifecycle effect is outside the approved typed boundary",
    )
}

const fn cancelled() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidTransition,
        "MCP lifecycle operation was cancelled",
    )
}

const fn projection_conflict() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::ProjectionConflict,
        "managed extension projection conflicts with an existing user extension",
    )
}

const fn projection_failed() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RepositoryUnavailable,
        "managed extension projection could not be persisted",
    )
}
