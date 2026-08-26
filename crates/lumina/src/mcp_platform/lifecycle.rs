use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::agents::ExtensionConfig;
use crate::config::extensions::ExtensionEntry;

use super::credential_authority::CredentialReadinessBinding;
use super::credential_runtime_gate::{auth_reference, CredentialRuntimeSnapshot};
use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::managed_remote::{CoreManagedRemoteHttpNetworkPolicy, RemoteHttpNetworkPolicy};
use super::manifest::{Auth, Distribution, HealthCheck, Manifest, Transport};
use super::plan::ConnectionProjection;
use super::repository::{ProjectionSinkAtomicProofKind, ProjectionWitnessV2};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRequirementEvidence {
    pub ready: bool,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRequirementSnapshot {
    pub requirement: AuthRequirement,
    pub evidence: AuthRequirementEvidence,
}

pub trait AuthRequirementResolver: Send + Sync {
    fn requirement(&self, auth: &Auth) -> McpPlatformResult<AuthRequirement>;

    fn snapshot(&self, auth: &Auth) -> McpPlatformResult<AuthRequirementSnapshot> {
        let requirement = self.requirement(auth)?;
        let evidence = auth_requirement_evidence(requirement.clone(), None)?;
        Ok(AuthRequirementSnapshot {
            requirement,
            evidence,
        })
    }

    fn evidence(&self, auth: &Auth) -> McpPlatformResult<AuthRequirementEvidence> {
        Ok(self.snapshot(auth)?.evidence)
    }

    fn runtime_snapshot(
        &self,
        auth: &Auth,
        _binding: &CredentialReadinessBinding,
    ) -> McpPlatformResult<CredentialRuntimeSnapshot> {
        let snapshot = self.snapshot(auth)?;
        let AuthRequirementSnapshot {
            requirement,
            evidence,
        } = snapshot;
        Ok(CredentialRuntimeSnapshot::from_legacy_snapshot(
            requirement,
            evidence,
            auth_reference(auth),
        ))
    }

    fn verify_runtime_binding(
        &self,
        auth: &Auth,
        binding: &CredentialReadinessBinding,
        _ttl: Duration,
    ) -> McpPlatformResult<CredentialRuntimeSnapshot> {
        let snapshot = self.runtime_snapshot(auth, binding)?;
        if snapshot.is_ready() {
            Ok(snapshot)
        } else {
            Err(snapshot.status().readiness_error())
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionSinkAtomicProof {
    adapter_id: String,
    adapter_version: String,
    runtime_id: String,
    observed_state_digest: String,
    target_state_digest: String,
    kind: ProjectionSinkAtomicProofKind,
    attestation: String,
}

impl ProjectionSinkAtomicProof {
    pub(crate) fn new(
        adapter_id: String,
        adapter_version: String,
        runtime_id: String,
        observed_state_digest: String,
        target_state_digest: String,
        kind: ProjectionSinkAtomicProofKind,
        attestation: String,
    ) -> McpPlatformResult<Self> {
        if adapter_id.is_empty()
            || adapter_version.is_empty()
            || runtime_id.is_empty()
            || observed_state_digest.len() != 64
            || target_state_digest.len() != 64
            || attestation.len() != 64
        {
            return Err(super::error::McpPlatformError::new(
                McpPlatformErrorCode::IntegrityError,
                "projection sink atomic proof is malformed",
            ));
        }
        Ok(Self {
            adapter_id,
            adapter_version,
            runtime_id,
            observed_state_digest,
            target_state_digest,
            kind,
            attestation,
        })
    }

    pub(crate) fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    pub(crate) fn adapter_version(&self) -> &str {
        &self.adapter_version
    }

    pub(crate) fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    pub(crate) fn observed_state_digest(&self) -> &str {
        &self.observed_state_digest
    }

    pub(crate) fn target_state_digest(&self) -> &str {
        &self.target_state_digest
    }

    pub(crate) const fn kind(&self) -> ProjectionSinkAtomicProofKind {
        self.kind
    }

    pub(crate) fn attestation(&self) -> &str {
        &self.attestation
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionProofSession {
    sink_identity: String,
    secret: Arc<[u8; 32]>,
}

impl ProjectionProofSession {
    pub(crate) fn new(sink_identity: String) -> Self {
        let mut secret = [0_u8; 32];
        rand::rng().fill(&mut secret);
        Self {
            sink_identity,
            secret: Arc::new(secret),
        }
    }

    pub(crate) fn attest(
        &self,
        witness: &ProjectionWitnessV2,
        adapter_id: &str,
        adapter_version: &str,
        kind: ProjectionSinkAtomicProofKind,
        observed_state_digest: &str,
        target_state_digest: &str,
    ) -> String {
        crate::utils::bytes_to_hex(projection_proof_mac(
            self.secret.as_ref(),
            &projection_proof_payload(
                &self.sink_identity,
                witness,
                adapter_id,
                adapter_version,
                kind,
                observed_state_digest,
                target_state_digest,
            ),
        ))
    }

    pub(crate) fn validates(
        &self,
        witness: &ProjectionWitnessV2,
        sink_identity: &str,
        proof: &ProjectionSinkAtomicProof,
    ) -> bool {
        if self.sink_identity != sink_identity || witness.sink_identity != sink_identity {
            return false;
        }
        let expected = self.attest(
            witness,
            proof.adapter_id(),
            proof.adapter_version(),
            proof.kind(),
            proof.observed_state_digest(),
            proof.target_state_digest(),
        );
        constant_time_eq(expected.as_bytes(), proof.attestation().as_bytes())
    }
}

#[derive(Debug, Clone)]
pub struct ProjectionCommitPlan {
    expected: Option<ExtensionEntry>,
    witness: ProjectionWitnessV2,
    target_state_digest: String,
    proof_session: ProjectionProofSession,
}

impl ProjectionCommitPlan {
    pub(crate) fn new(
        expected: Option<ExtensionEntry>,
        witness: ProjectionWitnessV2,
        proof_session: ProjectionProofSession,
    ) -> McpPlatformResult<Self> {
        let expected_observed_digest = match expected.as_ref() {
            Some(entry) => observed_projection_digest(
                &witness.sink_identity,
                &witness.runtime_id,
                Some(entry),
            )?,
            None if !witness.desired_enabled => {
                observed_projection_digest(&witness.sink_identity, &witness.runtime_id, None)?
            }
            None => return Err(integrity_error()),
        };
        if expected_observed_digest != witness.observed_state_digest {
            return Err(integrity_error());
        }
        Ok(Self {
            expected,
            witness,
            target_state_digest: expected_observed_digest,
            proof_session,
        })
    }

    pub(crate) fn confirm_existing(
        expected: Option<ExtensionEntry>,
        witness: ProjectionWitnessV2,
        proof_session: ProjectionProofSession,
    ) -> McpPlatformResult<Self> {
        let target_state_digest = match expected.as_ref() {
            Some(entry) => observed_projection_digest(
                &witness.sink_identity,
                &witness.runtime_id,
                Some(entry),
            )?,
            None => observed_projection_digest(&witness.sink_identity, &witness.runtime_id, None)?,
        };
        Ok(Self {
            expected,
            witness,
            target_state_digest,
            proof_session,
        })
    }

    pub(crate) fn key(&self) -> &str {
        &self.witness.runtime_id
    }

    pub(crate) fn expected(&self) -> Option<&ExtensionEntry> {
        self.expected.as_ref()
    }

    pub(crate) fn expected_current(&self) -> Option<ExtensionEntry> {
        self.expected.as_ref().map(|entry| ExtensionEntry {
            enabled: self.witness.previous_enabled,
            config: entry.config.clone(),
        })
    }

    pub(crate) fn desired_enabled(&self) -> bool {
        self.witness.desired_enabled
    }

    pub(crate) fn sink_identity(&self) -> &str {
        &self.witness.sink_identity
    }

    pub(crate) fn target_state_digest(&self) -> &str {
        &self.target_state_digest
    }

    pub(crate) fn runtime_id(&self) -> &str {
        &self.witness.runtime_id
    }

    fn atomic_proof(
        &self,
        adapter_id: &str,
        adapter_version: &str,
        kind: ProjectionSinkAtomicProofKind,
        observed_state_digest: String,
    ) -> McpPlatformResult<ProjectionSinkAtomicProof> {
        if observed_state_digest != self.target_state_digest {
            return Err(projection_conflict());
        }
        let attestation = self.proof_session.attest(
            &self.witness,
            adapter_id,
            adapter_version,
            kind,
            &observed_state_digest,
            &self.target_state_digest,
        );
        ProjectionSinkAtomicProof::new(
            adapter_id.to_string(),
            adapter_version.to_string(),
            self.runtime_id().to_string(),
            observed_state_digest,
            self.target_state_digest.clone(),
            kind,
            attestation,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_only_issue_atomic_proof(
        &self,
        adapter_id: &str,
        adapter_version: &str,
        kind: ProjectionSinkAtomicProofKind,
        observed_state_digest: String,
    ) -> McpPlatformResult<ProjectionSinkAtomicProof> {
        self.atomic_proof(adapter_id, adapter_version, kind, observed_state_digest)
    }
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
    async fn commit_enabled_projection(
        &self,
        _plan: &ProjectionCommitPlan,
    ) -> McpPlatformResult<ProjectionSinkAtomicProof> {
        Err(projection_conflict())
    }
    async fn confirm_target_state(
        &self,
        _plan: &ProjectionCommitPlan,
    ) -> McpPlatformResult<Option<ProjectionSinkAtomicProof>> {
        Ok(None)
    }
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
        projection.require_runtime_transport()?;
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

#[cfg(test)]
mod projection_transport_tests {
    use super::*;

    fn managed_stdio() -> ConnectionProjection {
        ConnectionProjection::ManagedStdio {
            name: "managed".to_string(),
            description: "managed".to_string(),
            executable: "managed.exe".to_string(),
            args: Vec::new(),
            environment_keys: Vec::new(),
            cwd: None,
            timeout_seconds: None,
        }
    }

    #[test]
    fn windows_managed_local_projection_never_reaches_the_extension_config_seam() {
        let adapter = CoreTransportProjectionAdapter;
        let remote = ConnectionProjection::RemoteHttp {
            name: "remote".to_string(),
            description: "remote".to_string(),
            uri: "https://example.invalid/mcp".to_string(),
            timeout_seconds: None,
        };
        let manual = ConnectionProjection::ManualStdio {
            name: "manual".to_string(),
            description: "manual".to_string(),
            executable: "manual.exe".to_string(),
            args: Vec::new(),
            environment_keys: Vec::new(),
            cwd: None,
            timeout_seconds: None,
        };

        assert!(adapter.extension_config(&remote, "remote").is_ok());
        assert!(adapter.extension_config(&manual, "manual").is_ok());

        #[cfg(windows)]
        {
            let error = adapter
                .extension_config(&managed_stdio(), "managed")
                .unwrap_err();
            assert_eq!(
                error.code(),
                McpPlatformErrorCode::RuntimeControlUnavailable
            );
        }
    }
}

#[derive(Debug, Default)]
pub struct ConfigAuthRequirementResolver;

impl AuthRequirementResolver for ConfigAuthRequirementResolver {
    fn requirement(&self, auth: &Auth) -> McpPlatformResult<AuthRequirement> {
        Ok(self.snapshot(auth)?.requirement)
    }

    fn snapshot(&self, auth: &Auth) -> McpPlatformResult<AuthRequirementSnapshot> {
        config_auth_snapshot_with(auth, |key| crate::config::Config::global().get(key, true))
    }
}

fn config_auth_snapshot_with<E>(
    auth: &Auth,
    read_credential: impl FnOnce(&str) -> Result<serde_json::Value, E>,
) -> McpPlatformResult<AuthRequirementSnapshot> {
    let (requirement, credential_value) = match auth {
        Auth::None => (AuthRequirement::Ready, None),
        Auth::ApiKeyHeader {
            credential_name, ..
        }
        | Auth::Environment {
            credential_name, ..
        } => {
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
            match read_credential(&key) {
                Ok(value) => match value.as_str().filter(|value| !value.is_empty()) {
                    Some(value) => (AuthRequirement::Ready, Some(value.to_owned())),
                    None => (
                        AuthRequirement::MissingOpaqueHandle {
                            credential_name: credential_name.clone(),
                        },
                        None,
                    ),
                },
                Err(_) => (
                    AuthRequirement::MissingOpaqueHandle {
                        credential_name: credential_name.clone(),
                    },
                    None,
                ),
            }
        }
        Auth::Oauth2 { .. } => (
            AuthRequirement::MissingOpaqueHandle {
                credential_name: "oauth2".to_string(),
            },
            None,
        ),
    };
    let evidence = auth_requirement_evidence(requirement.clone(), credential_value.as_deref())?;
    Ok(AuthRequirementSnapshot {
        requirement,
        evidence,
    })
}

fn auth_requirement_evidence(
    requirement: AuthRequirement,
    credential_value: Option<&str>,
) -> McpPlatformResult<AuthRequirementEvidence> {
    let ready = matches!(requirement, AuthRequirement::Ready);
    let requirement_code = match &requirement {
        AuthRequirement::Ready => "ready",
        AuthRequirement::MissingOpaqueHandle { .. } => "missing_opaque_handle",
    };
    let credential_name = match &requirement {
        AuthRequirement::Ready => None,
        AuthRequirement::MissingOpaqueHandle { credential_name } => Some(credential_name.as_str()),
    };
    let bytes = serde_json::to_vec(&(
        "managed-auth-evidence-v1",
        requirement_code,
        credential_name,
        credential_value,
    ))
    .map_err(|_| {
        McpPlatformError::new(
            McpPlatformErrorCode::SerializationFailed,
            "failed to derive managed authentication evidence",
        )
    })?;
    Ok(AuthRequirementEvidence {
        ready,
        evidence_digest: crate::utils::bytes_to_hex(Sha256::digest(bytes)),
    })
}

#[cfg(test)]
mod auth_snapshot_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn api_key_auth() -> Auth {
        Auth::ApiKeyHeader {
            header_name: "Authorization".to_string(),
            prefix: None,
            credential_name: "profile-token".to_string(),
        }
    }

    #[test]
    fn config_auth_snapshot_reads_once_and_keeps_readiness_and_evidence_consistent() {
        let reads = AtomicUsize::new(0);
        let snapshot = config_auth_snapshot_with(&api_key_auth(), |_| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok::<_, ()>(serde_json::Value::String("rotated-secret".to_string()))
        })
        .unwrap();

        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot.requirement, AuthRequirement::Ready);
        assert!(snapshot.evidence.ready);
        assert_eq!(snapshot.evidence.evidence_digest.len(), 64);
    }

    #[test]
    fn config_auth_snapshot_fails_closed_on_read_and_parse_errors() {
        let read_error = config_auth_snapshot_with(&api_key_auth(), |_| {
            Err::<serde_json::Value, _>("read failed")
        })
        .unwrap();
        assert!(!read_error.evidence.ready);
        assert!(matches!(
            read_error.requirement,
            AuthRequirement::MissingOpaqueHandle { .. }
        ));

        let parse_error = config_auth_snapshot_with(&api_key_auth(), |_| {
            Ok::<_, ()>(serde_json::json!({"unexpected": "shape"}))
        })
        .unwrap();
        assert!(!parse_error.evidence.ready);
        assert!(matches!(
            parse_error.requirement,
            AuthRequirement::MissingOpaqueHandle { .. }
        ));
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

    async fn commit_enabled_projection(
        &self,
        plan: &ProjectionCommitPlan,
    ) -> McpPlatformResult<ProjectionSinkAtomicProof> {
        match plan.expected() {
            Some(target) => {
                let expected = plan.expected_current().ok_or_else(integrity_error)?;
                crate::config::extensions::try_replace_managed_extension_at_key_with_config(
                    self.config(),
                    plan.key(),
                    &expected,
                    target.clone(),
                )
                .map_err(|_| projection_conflict())?;
                let observed_state_digest =
                    observed_projection_digest(plan.sink_identity(), plan.key(), Some(target))?;
                plan.atomic_proof(
                    self.adapter_id(),
                    self.adapter_version(),
                    ProjectionSinkAtomicProofKind::CompareAndSwapWrite,
                    observed_state_digest.clone(),
                )
            }
            None => {
                if plan.desired_enabled() {
                    return Err(projection_conflict());
                }
                crate::config::extensions::try_confirm_managed_extension_at_key_with_config(
                    self.config(),
                    plan.key(),
                    None,
                )
                .map_err(|_| projection_conflict())?;
                let observed_state_digest =
                    observed_projection_digest(plan.sink_identity(), plan.key(), None)?;
                plan.atomic_proof(
                    self.adapter_id(),
                    self.adapter_version(),
                    ProjectionSinkAtomicProofKind::NoopCompareAndSwap,
                    observed_state_digest.clone(),
                )
            }
        }
    }

    async fn confirm_target_state(
        &self,
        plan: &ProjectionCommitPlan,
    ) -> McpPlatformResult<Option<ProjectionSinkAtomicProof>> {
        match plan.expected() {
            Some(target) => {
                crate::config::extensions::try_confirm_managed_extension_at_key_with_config(
                    self.config(),
                    plan.key(),
                    Some(target),
                )
                .map_err(|_| projection_conflict())?;
                let observed_state_digest =
                    observed_projection_digest(plan.sink_identity(), plan.key(), Some(target))?;
                Ok(Some(plan.atomic_proof(
                    self.adapter_id(),
                    self.adapter_version(),
                    ProjectionSinkAtomicProofKind::NoopCompareAndSwap,
                    observed_state_digest.clone(),
                )?))
            }
            None => {
                if plan.desired_enabled() {
                    return Err(projection_conflict());
                }
                crate::config::extensions::try_confirm_managed_extension_at_key_with_config(
                    self.config(),
                    plan.key(),
                    None,
                )
                .map_err(|_| projection_conflict())?;
                let observed_state_digest =
                    observed_projection_digest(plan.sink_identity(), plan.key(), None)?;
                Ok(Some(plan.atomic_proof(
                    self.adapter_id(),
                    self.adapter_version(),
                    ProjectionSinkAtomicProofKind::NoopCompareAndSwap,
                    observed_state_digest.clone(),
                )?))
            }
        }
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

fn projection_proof_payload(
    sink_identity: &str,
    witness: &ProjectionWitnessV2,
    adapter_id: &str,
    adapter_version: &str,
    kind: ProjectionSinkAtomicProofKind,
    observed_state_digest: &str,
    target_state_digest: &str,
) -> Vec<u8> {
    canonical_fields(&[
        b"projection-sink-atomic-proof-v2",
        sink_identity.as_bytes(),
        witness.domain.as_bytes(),
        witness.version.to_string().as_bytes(),
        witness.repository.provider_id.as_bytes(),
        witness.repository.instance_id.as_bytes(),
        witness.repository.path_binding.as_bytes(),
        witness.repository.key_epoch.to_string().as_bytes(),
        witness.sink_identity.as_bytes(),
        witness.runtime_id.as_bytes(),
        witness.projection_digest.as_bytes(),
        witness.mutation_id.to_string().as_bytes(),
        witness.managed_mcp_id.as_bytes(),
        witness.expected_revision.to_string().as_bytes(),
        bool_flag(witness.previous_enabled),
        bool_flag(witness.desired_enabled),
        witness.observed_state_digest.as_bytes(),
        adapter_id.as_bytes(),
        adapter_version.as_bytes(),
        kind.as_str().as_bytes(),
        observed_state_digest.as_bytes(),
        target_state_digest.as_bytes(),
    ])
}

fn projection_proof_mac(secret: &[u8; 32], payload: &[u8]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;
    let mut normalized = [0_u8; BLOCK_BYTES];
    normalized[..secret.len()].copy_from_slice(secret);
    let mut inner_pad = [0x36_u8; BLOCK_BYTES];
    let mut outer_pad = [0x5c_u8; BLOCK_BYTES];
    for index in 0..BLOCK_BYTES {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(payload);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn canonical_fields(fields: &[&[u8]]) -> Vec<u8> {
    let mut payload = Vec::new();
    for field in fields {
        payload.extend_from_slice(&(field.len() as u64).to_be_bytes());
        payload.extend_from_slice(field);
    }
    payload
}

fn bool_flag(value: bool) -> &'static [u8] {
    if value {
        b"1"
    } else {
        b"0"
    }
}

const fn projection_failed() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RepositoryUnavailable,
        "managed extension projection could not be persisted",
    )
}

pub(crate) fn observed_projection_digest(
    sink_identity: &str,
    key: &str,
    entry: Option<&ExtensionEntry>,
) -> McpPlatformResult<String> {
    let payload = match entry {
        Some(entry) => serde_json::to_vec(&(
            "projection-observed-state-v1",
            sink_identity,
            key,
            true,
            entry,
        )),
        None => serde_json::to_vec(&("projection-observed-state-v1", sink_identity, key, false)),
    }
    .map_err(|_| integrity_error())?;
    Ok(crate::utils::bytes_to_hex(Sha256::digest(payload)))
}
