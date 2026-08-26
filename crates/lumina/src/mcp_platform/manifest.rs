use std::collections::BTreeSet;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use url::Url;

use super::domain::TrustTier;
use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};

const MANIFEST_SCHEMA: &str =
    include_str!("../../../../documentation/static/schemas/mcp-package-manifest-v1.schema.json");
const MANIFEST_V2_CONTRACT_SCHEMA: &str =
    include_str!("../../../../documentation/static/schemas/mcp-package-manifest-v2.schema.json");
const TEMPLATE_VARIABLES: &[&str] = &[
    "installation.root",
    "installation.bin",
    "host.user_config",
    "user.workspace",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExactVersion(String);

impl ExactVersion {
    pub fn parse(value: &str) -> McpPlatformResult<Self> {
        if is_exact_semver(value) {
            Ok(Self(value.to_string()))
        } else {
            Err(error(
                McpPlatformErrorCode::VersionNotExact,
                "manifest versions must be exact semantic versions",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ExactVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ExactVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(|_| D::Error::custom("version must be exact semantic version"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u8,
    pub id: String,
    pub version: ExactVersion,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    pub publisher: Publisher,
    pub license: License,
    pub capabilities: Vec<Capability>,
    pub permissions: Vec<Permission>,
    pub distribution: Distribution,
    pub transport: Transport,
    pub auth: Auth,
    pub health_check: HealthCheck,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_integrations: Vec<HostIntegration>,
    pub owned_files: Vec<OwnedFile>,
    pub uninstall: Uninstall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signing_identities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct License {
    pub spdx: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub notice_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Tools,
    Resources,
    Prompts,
    Sampling,
    Elicitation,
    Logging,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permission {
    pub id: String,
    pub kind: PermissionKind,
    pub reason: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    FilesystemRead,
    FilesystemWrite,
    Network,
    Credentials,
    ProcessSpawn,
    Docker,
    HostApplication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Distribution {
    RemoteHttp,
    ManualStdio {
        entrypoint: Entrypoint,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        platforms: Vec<Platform>,
    },
    Npm {
        package: String,
        package_version: ExactVersion,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        registry: Option<String>,
        artifacts: Vec<Artifact>,
        entrypoint: Entrypoint,
    },
    PythonWheel {
        package: String,
        package_version: ExactVersion,
        python: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<String>,
        artifacts: Vec<Artifact>,
        entrypoint: Entrypoint,
    },
    BinaryArchive {
        archive_format: ArchiveFormat,
        #[serde(default, skip_serializing_if = "is_zero_u8")]
        strip_components: u8,
        artifacts: Vec<Artifact>,
        entrypoint: Entrypoint,
    },
    Docker {
        image: String,
        digest: Sha256Digest,
        entrypoint: Entrypoint,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mounts: Vec<DockerMount>,
    },
    GitDev {
        repository: String,
        commit: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subdirectory: Option<String>,
        adapter: GitDevAdapter,
        entrypoint: Entrypoint,
    },
}

impl Distribution {
    pub const fn adapter_id(&self) -> &'static str {
        match self {
            Self::RemoteHttp => "remote_http",
            Self::ManualStdio { .. } => "manual_stdio",
            Self::Npm { .. } => "npm",
            Self::PythonWheel { .. } => "python_wheel",
            Self::BinaryArchive { .. } => "binary_archive",
            Self::Docker { .. } => "docker",
            Self::GitDev { .. } => "git_dev",
        }
    }

    pub fn artifacts(&self) -> &[Artifact] {
        match self {
            Self::Npm { artifacts, .. }
            | Self::PythonWheel { artifacts, .. }
            | Self::BinaryArchive { artifacts, .. } => artifacts,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveFormat {
    #[serde(rename = "zip")]
    Zip,
    #[serde(rename = "tar.gz")]
    TarGz,
    #[serde(rename = "tar.xz")]
    TarXz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitDevAdapter {
    Npm,
    PythonWheel,
    BinaryArchive,
    Docker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub platform: Platform,
    pub arch: Architecture,
    pub url: String,
    pub digest: Sha256Digest,
    /// The signed storage contract used for Windows managed-local preflight.
    ///
    /// Older manifests did not carry this field.  They deliberately remain
    /// readable, but callers must treat their capacity as unknown rather than
    /// inferring it from the legacy download-size hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<ArtifactCapacity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCapacity {
    pub download_size_bytes: u64,
    pub materialized_size_bytes: u64,
    pub workspace_size_bytes: u64,
    pub rollback_extra_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactCapacityContract {
    Unknown,
    Known(ArtifactCapacity),
}

impl Artifact {
    pub fn capacity_contract(&self) -> ArtifactCapacityContract {
        match (&self.platform, &self.capacity) {
            (Platform::Windows, Some(capacity)) => {
                ArtifactCapacityContract::Known(capacity.clone())
            }
            (_, None) => ArtifactCapacityContract::Unknown,
            _ => ArtifactCapacityContract::Unknown,
        }
    }
}

impl ArtifactCapacity {
    pub fn required_peak_bytes(&self) -> McpPlatformResult<u64> {
        [
            self.download_size_bytes,
            self.materialized_size_bytes,
            self.workspace_size_bytes,
            self.rollback_extra_bytes,
        ]
        .into_iter()
        .try_fold(0_u64, |total, size| total.checked_add(size))
        .ok_or_else(|| {
            error(
                McpPlatformErrorCode::InvalidManifest,
                "managed artifact capacity contract exceeds supported storage accounting",
            )
        })
    }

    pub(crate) fn validate(&self) -> McpPlatformResult<()> {
        if [
            self.download_size_bytes,
            self.materialized_size_bytes,
            self.workspace_size_bytes,
            self.rollback_extra_bytes,
        ]
        .into_iter()
        .any(|size| size == 0)
        {
            return Err(error(
                McpPlatformErrorCode::InvalidManifest,
                "managed artifact capacity contract values must be nonzero",
            ));
        }
        self.required_peak_bytes().map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sha256Digest {
    pub algorithm: Sha256Algorithm,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sha256Algorithm {
    #[serde(rename = "sha256")]
    Sha256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    Macos,
    Linux,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
    Universal,
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entrypoint {
    pub executable: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerMount {
    pub source_permission: String,
    pub target: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transport {
    Stdio {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        startup_timeout_seconds: Option<u64>,
    },
    StreamableHttp {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connect_timeout_seconds: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allowed_redirect_origins: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Auth {
    None,
    ApiKeyHeader {
        header_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        credential_name: String,
    },
    Environment {
        environment_key: String,
        credential_name: String,
    },
    Oauth2 {
        authorization_url: String,
        token_url: String,
        client_registration: OAuthClientRegistration,
        scopes: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CredentialEnrollmentSchema {
    pub schema_id: &'static str,
    pub fields: Vec<CredentialFieldSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CredentialFieldSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub input_kind: CredentialFieldInputKind,
    pub required: bool,
    pub secret: bool,
    pub help: &'static str,
    pub validation: CredentialFieldValidation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialFieldInputKind {
    SecretText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CredentialFieldValidation {
    pub min_length: usize,
    pub max_length: usize,
    pub reject_control_characters: bool,
}

pub(crate) fn trusted_credential_enrollment_schema(
    auth: &Auth,
) -> McpPlatformResult<CredentialEnrollmentSchema> {
    let secret_field = |schema_id, label, help| CredentialEnrollmentSchema {
        schema_id,
        fields: vec![CredentialFieldSpec {
            id: "secret",
            label,
            input_kind: CredentialFieldInputKind::SecretText,
            required: true,
            secret: true,
            help,
            validation: CredentialFieldValidation {
                min_length: 1,
                max_length: 8192,
                reject_control_characters: true,
            },
        }],
    };
    match auth {
        Auth::None => Err(error(
            McpPlatformErrorCode::InvalidTransition,
            "credential enrollment is unavailable for auth type none",
        )),
        Auth::ApiKeyHeader {
            header_name,
            prefix,
            ..
        } if header_name.eq_ignore_ascii_case("authorization")
            && prefix.as_deref() == Some("Bearer ") =>
        {
            Ok(secret_field(
                "bearer_token",
                "Bearer token",
                "Provide the bearer token value without the Bearer prefix.",
            ))
        }
        Auth::ApiKeyHeader { .. } => Ok(secret_field(
            "static_header_secret",
            "Header secret",
            "Provide the secret value required by the trusted package manifest.",
        )),
        Auth::Environment { .. } => Ok(secret_field(
            "static_env_secret",
            "Credential secret",
            "Provide the secret value required by the trusted package manifest.",
        )),
        Auth::Oauth2 { .. } => Err(error(
            McpPlatformErrorCode::OperationNotSupported,
            "credential enrollment for oauth2 is temporarily unavailable",
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthClientRegistration {
    Dynamic,
    UserProvided,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HealthCheck {
    McpInitialize {
        timeout_seconds: u64,
    },
    McpListTools {
        timeout_seconds: u64,
    },
    Http {
        path: String,
        expected_status: u16,
        timeout_seconds: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostIntegration {
    pub id: String,
    pub host: Host,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_name: Option<String>,
    pub supported_versions: Vec<String>,
    pub platforms: Vec<Platform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection: Option<HostDetection>,
    pub actions: Vec<HostAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Host {
    UnrealEngine,
    Houdini,
    Nuke,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostDetection {
    pub strategy: HostDetectionStrategy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostDetectionStrategy {
    KnownLocations,
    Registry,
    BundleIdentifier,
    UserSelected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostAction {
    CopyFile {
        source: String,
        target: String,
    },
    WriteConfigFragment {
        target: String,
        format: ConfigFragmentFormat,
        fragment: serde_json::Map<String, Value>,
    },
    RegisterPlugin {
        registration_id: String,
        target: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFragmentFormat {
    Json,
    Toml,
    Yaml,
    Ini,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedFile {
    pub root: OwnedRoot,
    pub path: String,
    pub kind: OwnedFileKind,
    pub remove_on_uninstall: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnedRoot {
    Installation,
    Data,
    Config,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnedFileKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Uninstall {
    pub mode: UninstallMode,
    pub preserve_user_data: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_host_integrations: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UninstallMode {
    #[serde(rename = "remove_owned_files_only")]
    RemoveOwnedFilesOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceImportKind {
    #[default]
    LocalPersistence,
    VerifiedSourceCatalog,
    HttpsManifestUrl,
    EnterpriseDirectory,
}

impl SourceImportKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalPersistence => "local_persistence",
            Self::VerifiedSourceCatalog => "verified_source_catalog",
            Self::HttpsManifestUrl => "https_manifest_url",
            Self::EnterpriseDirectory => "enterprise_directory",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceRef {
    LocalPersistence,
    VerifiedSourceCatalog {
        source_id: String,
    },
    HttpsManifestUrl {
        manifest_url: String,
    },
    EnterpriseDirectory {
        directory_id: String,
        entry_id: String,
    },
}

impl Default for SourceRef {
    fn default() -> Self {
        Self::LocalPersistence
    }
}

impl SourceRef {
    pub const fn import_kind(&self) -> SourceImportKind {
        match self {
            Self::LocalPersistence => SourceImportKind::LocalPersistence,
            Self::VerifiedSourceCatalog { .. } => SourceImportKind::VerifiedSourceCatalog,
            Self::HttpsManifestUrl { .. } => SourceImportKind::HttpsManifestUrl,
            Self::EnterpriseDirectory { .. } => SourceImportKind::EnterpriseDirectory,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateChannel {
    Default,
    Named { name: String },
}

impl Default for UpdateChannel {
    fn default() -> Self {
        Self::Default
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedSourceDocumentRef {
    pub source_id: String,
    pub document_digest: String,
    pub canonical_digest: String,
    pub signed_digest: String,
    pub binding_digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature_kids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_source_document: Option<VerifiedSourceDocumentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSourceMetadata {
    #[serde(default)]
    pub source_ref: SourceRef,
    #[serde(default)]
    pub import_kind: SourceImportKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    #[serde(default)]
    pub origin_provenance: OriginProvenance,
    #[serde(default)]
    pub update_channel: UpdateChannel,
}

impl ManifestSourceMetadata {
    pub fn local_persistence() -> Self {
        Self {
            source_ref: SourceRef::LocalPersistence,
            import_kind: SourceImportKind::LocalPersistence,
            release_id: None,
            origin_provenance: OriginProvenance::default(),
            update_channel: UpdateChannel::Default,
        }
    }

    pub fn validate(&self) -> McpPlatformResult<()> {
        if self.import_kind != self.source_ref.import_kind() {
            return Err(error(
                McpPlatformErrorCode::InvalidManifest,
                "manifest source metadata import_kind must match source_ref",
            ));
        }
        if let Some(release_id) = &self.release_id {
            validate_metadata_token("release_id", release_id)?;
        }
        match &self.update_channel {
            UpdateChannel::Default => {}
            UpdateChannel::Named { name } => validate_metadata_token("update_channel", name)?,
        }
        match &self.source_ref {
            SourceRef::LocalPersistence => {
                if self.release_id.is_some()
                    || self.origin_provenance.verified_source_document.is_some()
                {
                    return Err(error(
                        McpPlatformErrorCode::InvalidManifest,
                        "local persistence manifests cannot claim source release provenance",
                    ));
                }
            }
            SourceRef::VerifiedSourceCatalog { source_id } => {
                validate_metadata_token("source_id", source_id)?;
                let Some(document) = self.origin_provenance.verified_source_document.as_ref()
                else {
                    return Err(error(
                        McpPlatformErrorCode::InvalidManifest,
                        "verified source manifests require verified source document provenance",
                    ));
                };
                document.validate(source_id)?;
            }
            SourceRef::HttpsManifestUrl { manifest_url } => {
                validate_https_url(manifest_url)?;
                if self.origin_provenance.verified_source_document.is_some() {
                    return Err(error(
                        McpPlatformErrorCode::InvalidManifest,
                        "https manifest metadata cannot claim verified source catalog evidence",
                    ));
                }
            }
            SourceRef::EnterpriseDirectory {
                directory_id,
                entry_id,
            } => {
                validate_metadata_token("directory_id", directory_id)?;
                validate_metadata_token("entry_id", entry_id)?;
                if self.origin_provenance.verified_source_document.is_some() {
                    return Err(error(
                        McpPlatformErrorCode::InvalidManifest,
                        "enterprise directory metadata cannot claim verified source catalog evidence",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate_trust_tier(&self, trust_tier: TrustTier) -> McpPlatformResult<()> {
        self.validate()?;
        if let Some(expected) = trust_tier_for_source_ref(&self.source_ref) {
            if trust_tier != expected {
                return Err(error(
                    McpPlatformErrorCode::InvalidManifest,
                    "manifest trust tier does not match its source provenance",
                ));
            }
        }
        Ok(())
    }
}

pub(crate) const fn trust_tier_for_source_ref(source_ref: &SourceRef) -> Option<TrustTier> {
    match source_ref {
        SourceRef::VerifiedSourceCatalog { .. } => Some(TrustTier::Official),
        SourceRef::HttpsManifestUrl { .. } | SourceRef::EnterpriseDirectory { .. } => {
            Some(TrustTier::Local)
        }
        // A persisted/local manifest is user-provided material. Legacy rows that omitted
        // provenance are decoded as this same local policy and cannot retain a historical
        // higher tier.
        SourceRef::LocalPersistence => Some(TrustTier::Local),
    }
}

impl VerifiedSourceDocumentRef {
    fn validate(&self, expected_source_id: &str) -> McpPlatformResult<()> {
        validate_metadata_token("source_id", &self.source_id)?;
        if self.source_id != expected_source_id {
            return Err(error(
                McpPlatformErrorCode::InvalidManifest,
                "verified source document provenance must match source_ref",
            ));
        }
        for (field, value) in [
            ("document_digest", self.document_digest.as_str()),
            ("canonical_digest", self.canonical_digest.as_str()),
            ("signed_digest", self.signed_digest.as_str()),
            ("binding_digest", self.binding_digest.as_str()),
        ] {
            if !is_lower_hex_64(value) {
                return Err(error(
                    McpPlatformErrorCode::InvalidDigest,
                    "manifest provenance digests must be lowercase SHA-256 values",
                ));
            }
            let _ = field;
        }
        if self.signature_kids.is_empty() {
            return Err(error(
                McpPlatformErrorCode::InvalidManifest,
                "verified source document provenance requires at least one signature kid",
            ));
        }
        for kid in &self.signature_kids {
            validate_metadata_token("signature_kid", kid)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManifestProof {
    LocalBytes,
    Catalog {
        index_digest: String,
        declared_manifest_digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<SignatureEvidence>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureEvidence {
    pub algorithm: String,
    pub signing_identity: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedManifest {
    manifest: Manifest,
    digest: String,
    canonical_json: Vec<u8>,
}

impl VerifiedManifest {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical_json
    }
}

/// Parses a manifest, validates the v1 schema, and computes a canonical SHA-256 digest.
///
/// Canonicalization serializes the strongly typed manifest, omits absent and
/// schema-default values, sorts every object key lexicographically, preserves
/// array order, and emits JSON without insignificant whitespace. Signature
/// evidence is external to the manifest and is not verified by this phase.
pub fn parse_manifest(bytes: &[u8]) -> McpPlatformResult<VerifiedManifest> {
    let raw: Value = serde_json::from_slice(bytes).map_err(|_| {
        error(
            McpPlatformErrorCode::InvalidJson,
            "manifest must be valid JSON",
        )
    })?;

    preflight(&raw)?;

    let schema: Value = serde_json::from_str(MANIFEST_SCHEMA).map_err(|_| {
        error(
            McpPlatformErrorCode::SerializationFailed,
            "embedded manifest schema could not be loaded",
        )
    })?;
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(|_| {
            error(
                McpPlatformErrorCode::SerializationFailed,
                "embedded manifest schema could not be compiled",
            )
        })?;
    if !validator.is_valid(&raw) {
        return Err(error(
            McpPlatformErrorCode::InvalidManifest,
            "manifest does not satisfy the v1 schema",
        ));
    }

    let manifest: Manifest = serde_json::from_value(raw).map_err(|_| {
        error(
            McpPlatformErrorCode::InvalidManifest,
            "manifest fields could not be decoded",
        )
    })?;
    validate_semantics(&manifest)?;

    let normalized = serde_json::to_value(&manifest).map_err(|_| {
        error(
            McpPlatformErrorCode::SerializationFailed,
            "manifest could not be canonicalized",
        )
    })?;
    let canonical_json = canonical_json(&normalized)?;
    let digest = sha256_hex(&canonical_json);

    Ok(VerifiedManifest {
        manifest,
        digest,
        canonical_json,
    })
}

/// Validates only the isolated v2 runtime-contract shape.
///
/// This function intentionally returns no installable manifest and is not
/// exported from `mcp_platform`; v2 cannot reach planning in this phase.
pub(crate) fn parse_manifest_v2_contract(bytes: &[u8]) -> McpPlatformResult<()> {
    let raw: Value = serde_json::from_slice(bytes).map_err(|_| {
        error(
            McpPlatformErrorCode::InvalidJson,
            "runtime manifest contract must be valid JSON",
        )
    })?;
    if raw.get("schema_version").and_then(Value::as_u64) != Some(2) {
        return Err(error(
            McpPlatformErrorCode::UnsupportedSchema,
            "only manifest schema version 2 is valid for the runtime contract",
        ));
    }
    let schema: Value = serde_json::from_str(MANIFEST_V2_CONTRACT_SCHEMA).map_err(|_| {
        error(
            McpPlatformErrorCode::SerializationFailed,
            "embedded v2 runtime manifest schema could not be loaded",
        )
    })?;
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(|_| {
            error(
                McpPlatformErrorCode::SerializationFailed,
                "embedded v2 runtime manifest schema could not be compiled",
            )
        })?;
    if !validator.is_valid(&raw) {
        return Err(error(
            McpPlatformErrorCode::InvalidManifest,
            "runtime manifest contract does not satisfy the v2 schema",
        ));
    }
    Ok(())
}

pub(crate) fn digest_serializable<T: Serialize>(value: &T) -> McpPlatformResult<String> {
    let value = serde_json::to_value(value).map_err(|_| {
        error(
            McpPlatformErrorCode::SerializationFailed,
            "value could not be canonicalized",
        )
    })?;
    Ok(sha256_hex(&canonical_json(&value)?))
}

fn validate_semantics(manifest: &Manifest) -> McpPlatformResult<()> {
    let expected_transport = match manifest.distribution {
        Distribution::RemoteHttp => Some("streamable_http"),
        Distribution::ManualStdio { .. } => Some("stdio"),
        _ => None,
    };
    if matches!(
        (expected_transport, &manifest.transport),
        (Some("streamable_http"), Transport::Stdio { .. })
            | (Some("stdio"), Transport::StreamableHttp { .. })
    ) {
        return Err(error(
            McpPlatformErrorCode::TransportMismatch,
            "distribution and transport are incompatible",
        ));
    }

    let mut selectors = BTreeSet::new();
    for artifact in manifest.distribution.artifacts() {
        if !selectors.insert((artifact.platform, artifact.arch)) {
            return Err(error(
                McpPlatformErrorCode::DuplicateSelector,
                "artifact platform and architecture selectors must be unique",
            ));
        }
        if let Some(capacity) = &artifact.capacity {
            capacity.validate()?;
        }
    }

    match &manifest.distribution {
        Distribution::Docker {
            image,
            entrypoint,
            mounts,
            ..
        } => {
            require_distribution_permissions(
                manifest,
                &[
                    PermissionKind::Docker,
                    PermissionKind::ProcessSpawn,
                    PermissionKind::Network,
                ],
            )?;
            validate_docker_distribution(manifest, image, entrypoint, mounts)?;
        }
        Distribution::GitDev {
            repository,
            commit,
            subdirectory,
            ..
        } => {
            require_distribution_permissions(
                manifest,
                &[PermissionKind::ProcessSpawn, PermissionKind::Network],
            )?;
            validate_git_repository(repository)?;
            if commit.len() != 40
                || !commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(error(
                    McpPlatformErrorCode::CommitUnavailable,
                    "git development commit must be exactly 40 lowercase hexadecimal characters",
                ));
            }
            if let Some(value) = subdirectory {
                validate_git_subdirectory(value)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn require_distribution_permissions(
    manifest: &Manifest,
    required: &[PermissionKind],
) -> McpPlatformResult<()> {
    if required.iter().all(|kind| {
        manifest
            .permissions
            .iter()
            .any(|permission| permission.kind == *kind && permission.required)
    }) {
        Ok(())
    } else {
        Err(error(
            McpPlatformErrorCode::InvalidManifest,
            "external distribution is missing a required closed permission declaration",
        ))
    }
}

fn validate_docker_distribution(
    manifest: &Manifest,
    image: &str,
    entrypoint: &Entrypoint,
    mounts: &[DockerMount],
) -> McpPlatformResult<()> {
    let components = image.split('/').collect::<Vec<_>>();
    let registry = components.first().copied().unwrap_or_default();
    let (registry_host, registry_port) = match registry.split_once(':') {
        Some((host, port)) if !host.contains(':') => (host, Some(port)),
        Some(_) => ("", None),
        None => (registry, None),
    };
    let valid_registry_host = registry_host == "localhost"
        || (registry_host.contains('.')
            && registry_host.split('.').all(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            }));
    let valid_registry_port = registry_port.is_none_or(|port| {
        !port.starts_with('0') && port.parse::<u16>().is_ok_and(|value| value != 0)
    });
    let valid_repository_component = |component: &&str| {
        !component.is_empty()
            && *component != "."
            && *component != ".."
            && component.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            })
    };
    if components.len() < 2
        || !valid_registry_host
        || !valid_registry_port
        || !components.iter().skip(1).all(valid_repository_component)
        || image.contains(['@', '?', '#', '\\', '\0'])
        || components
            .last()
            .is_some_and(|name| name.contains(':') || *name == "latest")
    {
        return Err(error(
            McpPlatformErrorCode::InvalidManifest,
            "docker image must be a canonical registry and repository without a tag",
        ));
    }
    let normalized_entrypoint = normalize_container_path(&entrypoint.executable).map_err(|_| {
        error(
            McpPlatformErrorCode::InvalidManifest,
            "docker entrypoint must be a normalized absolute container path",
        )
    })?;
    if entrypoint.executable.is_empty()
        || entrypoint.args.iter().any(|value| value.contains('\0'))
        || entrypoint
            .environment_keys
            .iter()
            .any(|value| value.contains('\0'))
    {
        return Err(error(
            McpPlatformErrorCode::InvalidManifest,
            "docker entrypoint contains an invalid value",
        ));
    }
    if !entrypoint.environment_keys.is_empty() {
        return Err(error(
            McpPlatformErrorCode::OperationNotSupported,
            "docker environment keys require an opaque value provider unavailable in manifest v1",
        ));
    }
    let permissions = manifest
        .permissions
        .iter()
        .map(|permission| (permission.id.as_str(), permission.kind))
        .collect::<std::collections::HashMap<_, _>>();
    let mut targets = BTreeSet::new();
    for mount in mounts {
        let target = normalize_container_path(&mount.target)?;
        let folded = target.to_ascii_lowercase();
        if !targets.insert(folded)
            || sensitive_container_target(&target)
            || target == normalized_entrypoint
        {
            return Err(error(
                McpPlatformErrorCode::MountPermissionDenied,
                "docker mount target violates container path policy",
            ));
        }
        let Some(kind) = permissions.get(mount.source_permission.as_str()) else {
            return Err(error(
                McpPlatformErrorCode::MountPermissionDenied,
                "docker mount references an undeclared permission",
            ));
        };
        let allowed = matches!(
            kind,
            PermissionKind::FilesystemRead | PermissionKind::FilesystemWrite
        ) && (mount.read_only || *kind == PermissionKind::FilesystemWrite);
        if !allowed {
            return Err(error(
                McpPlatformErrorCode::MountPermissionDenied,
                "writable docker mounts require filesystem_write permission",
            ));
        }
    }
    Ok(())
}

pub fn normalize_container_path(value: &str) -> McpPlatformResult<String> {
    if !value.starts_with('/')
        || value.contains(['\\', ',', '\0'])
        || value.contains("//")
        || value.chars().any(char::is_control)
    {
        return Err(error(
            McpPlatformErrorCode::MountPermissionDenied,
            "docker mount target must be a normalized absolute container path",
        ));
    }
    let components = value.split('/').skip(1).collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| component.is_empty() || *component == "." || *component == "..")
    {
        return Err(error(
            McpPlatformErrorCode::MountPermissionDenied,
            "docker mount target must be a normalized absolute container path",
        ));
    }
    Ok(format!("/{}", components.join("/")))
}

fn sensitive_container_target(value: &str) -> bool {
    ["/", "/proc", "/sys", "/dev", "/etc", "/run", "/var/run"]
        .iter()
        .any(|root| value == *root || value.starts_with(&format!("{root}/")))
        || value.eq_ignore_ascii_case("/var/run/docker.sock")
}

pub fn validate_git_subdirectory(value: &str) -> McpPlatformResult<()> {
    if value.is_empty()
        || value.starts_with(['/', '\\'])
        || value.contains([':', '\0'])
        || value.contains("//")
        || value
            .split(['/', '\\'])
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(error(
            McpPlatformErrorCode::PathTraversal,
            "git development subdirectory must be a normalized relative path",
        ));
    }
    Ok(())
}

pub fn validate_git_repository(value: &str) -> McpPlatformResult<()> {
    let url = Url::parse(value).map_err(|_| {
        error(
            McpPlatformErrorCode::GitOriginDenied,
            "git development origin must be a canonical public HTTPS repository",
        )
    })?;
    let host = url.host_str().unwrap_or_default();
    let path = url.path();
    let valid_path = !path.is_empty()
        && path != "/"
        && !path.contains("//")
        && !path.contains('%')
        && path.trim_start_matches('/').split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        });
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok()
        || !host.contains('.')
        || !valid_path
    {
        return Err(error(
            McpPlatformErrorCode::GitOriginDenied,
            "git development origin must be a canonical public HTTPS repository",
        ));
    }
    Ok(())
}

fn preflight(raw: &Value) -> McpPlatformResult<()> {
    let schema_version = raw.get("schema_version").and_then(Value::as_u64);
    if schema_version != Some(1) {
        return Err(error(
            McpPlatformErrorCode::UnsupportedSchema,
            "only manifest schema version 1 is supported",
        ));
    }

    if raw.get("runtime").is_some() || raw.get("lock").is_some() {
        return Err(error(
            McpPlatformErrorCode::InvalidManifest,
            "manifest v1 does not permit runtime or lock fields",
        ));
    }

    for pointer in ["/version", "/distribution/package_version"] {
        if let Some(version) = raw.pointer(pointer).and_then(Value::as_str) {
            ExactVersion::parse(version)?;
        }
    }

    if let Some(distribution) = raw.get("distribution") {
        let distribution_type = distribution.get("type").and_then(Value::as_str);
        let transport_type = raw
            .get("transport")
            .and_then(|transport| transport.get("type"))
            .and_then(Value::as_str);
        if matches!(
            (distribution_type, transport_type),
            (Some("remote_http"), Some(transport)) if transport != "streamable_http"
        ) || matches!(
            (distribution_type, transport_type),
            (Some("manual_stdio"), Some(transport)) if transport != "stdio"
        ) {
            return Err(error(
                McpPlatformErrorCode::TransportMismatch,
                "distribution and transport are incompatible",
            ));
        }

        if distribution_type == Some("git_dev") {
            let immutable = distribution
                .get("commit")
                .and_then(Value::as_str)
                .is_some_and(is_lower_hex_40);
            if !immutable {
                return Err(error(
                    McpPlatformErrorCode::CommitUnavailable,
                    "git development distributions require a full commit identifier",
                ));
            }
        }
    }

    inspect_value(raw, None)
}

fn inspect_value(value: &Value, key: Option<&str>) -> McpPlatformResult<()> {
    match value {
        Value::Object(object) => {
            for (child_key, child) in object {
                if child_key == "digest" {
                    validate_digest_object(child)?;
                }
                inspect_value(child, Some(child_key))?;
            }
        }
        Value::Array(values) => {
            for child in values {
                inspect_value(child, key)?;
            }
        }
        Value::String(text) => {
            validate_template_variables(text)?;
            if has_parent_component(text) {
                return Err(error(
                    if key == Some("repository") {
                        McpPlatformErrorCode::GitOriginDenied
                    } else {
                        McpPlatformErrorCode::PathTraversal
                    },
                    "manifest location must not traverse parent directories",
                ));
            }
            if key.is_some_and(is_url_field) {
                if key == Some("repository") {
                    validate_git_repository(text)?;
                } else {
                    validate_https_url(text)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_digest_object(value: &Value) -> McpPlatformResult<()> {
    let valid = value.as_object().is_some_and(|digest| {
        digest.get("algorithm").and_then(Value::as_str) == Some("sha256")
            && digest
                .get("value")
                .and_then(Value::as_str)
                .is_some_and(is_lower_hex_64)
    });
    if valid {
        Ok(())
    } else {
        Err(error(
            McpPlatformErrorCode::InvalidDigest,
            "artifact digests must be lowercase SHA-256 values",
        ))
    }
}

fn validate_https_url(value: &str) -> McpPlatformResult<()> {
    let valid = Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    });
    if valid {
        Ok(())
    } else {
        Err(error(
            McpPlatformErrorCode::UnsafeUrl,
            "manifest network locations must be credential-free HTTPS URLs",
        ))
    }
}

fn validate_template_variables(value: &str) -> McpPlatformResult<()> {
    let mut remaining = value;
    while let Some((_, after_start)) = remaining.split_once("${") {
        let Some((variable, after_end)) = after_start.split_once('}') else {
            return Err(error(
                McpPlatformErrorCode::UnknownTemplateVariable,
                "manifest contains an unsupported template variable",
            ));
        };
        if !TEMPLATE_VARIABLES.contains(&variable) {
            return Err(error(
                McpPlatformErrorCode::UnknownTemplateVariable,
                "manifest contains an unsupported template variable",
            ));
        }
        remaining = after_end;
    }
    Ok(())
}

fn validate_metadata_token(field: &str, value: &str) -> McpPlatformResult<()> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || value.starts_with(' ')
        || value.ends_with(' ')
    {
        return Err(error(
            McpPlatformErrorCode::InvalidManifest,
            "manifest provenance metadata contains an invalid token",
        ));
    }
    let _ = field;
    Ok(())
}

fn is_url_field(key: &str) -> bool {
    matches!(
        key,
        "homepage"
            | "documentation"
            | "website"
            | "url"
            | "registry"
            | "index"
            | "repository"
            | "authorization_url"
            | "token_url"
            | "allowed_redirect_origins"
    )
}

fn has_parent_component(value: &str) -> bool {
    value.split(['/', '\\']).any(|component| component == "..")
}

fn is_exact_semver(value: &str) -> bool {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return false;
    }

    let (without_build, build) = split_once_optional(value, '+');
    if build.is_some_and(|part| !valid_identifiers(part, false)) {
        return false;
    }
    let (core, prerelease) = split_once_optional(without_build, '-');
    if prerelease.is_some_and(|part| !valid_identifiers(part, true)) {
        return false;
    }

    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3 && parts.into_iter().all(valid_core_number)
}

fn split_once_optional(value: &str, separator: char) -> (&str, Option<&str>) {
    match value.split_once(separator) {
        Some((left, right)) => (left, Some(right)),
        None => (value, None),
    }
}

fn valid_core_number(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn valid_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !(reject_numeric_leading_zero
                    && identifier.len() > 1
                    && identifier.bytes().all(|byte| byte.is_ascii_digit())
                    && identifier.starts_with('0'))
        })
}

fn is_lower_hex_40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_json(value: &Value) -> McpPlatformResult<Vec<u8>> {
    let mut bytes = Vec::new();
    write_canonical(value, &mut bytes)?;
    Ok(bytes)
}

fn write_canonical(value: &Value, output: &mut Vec<u8>) -> McpPlatformResult<()> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Number(number) => output.extend_from_slice(number.to_string().as_bytes()),
        Value::String(string) => {
            let encoded = serde_json::to_string(string).map_err(|_| {
                error(
                    McpPlatformErrorCode::SerializationFailed,
                    "string could not be canonicalized",
                )
            })?;
            output.extend_from_slice(encoded.as_bytes());
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, item) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_canonical(item, output)?;
            }
            output.push(b']');
        }
        Value::Object(object) => {
            output.push(b'{');
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_canonical(&Value::String(key.clone()), output)?;
                output.push(b':');
                write_canonical(&object[key], output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

const fn is_false(value: &bool) -> bool {
    !*value
}

const fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

const fn error(code: McpPlatformErrorCode, message: &'static str) -> McpPlatformError {
    McpPlatformError::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2_runtime_contract() -> serde_json::Value {
        let digest = "a".repeat(64);
        serde_json::json!({
            "schema_version": 2,
            "id": "runtime-contract",
            "runtime": {
                "family": "node", "version": "22.1.0", "platform": "windows",
                "architecture": "x86_64", "abi": "msvc", "sha256": digest,
                "length_bytes": 1, "immutable_origin_id": "node-v22.1.0-win-x64",
                "provenance_digest": "b".repeat(64)
            },
            "lock": {
                "catalog_identity": "lumina.runtime-catalog.stable",
                "catalog_digest": "c".repeat(64), "expires_at_ms": 1,
                "revocation_digest": "d".repeat(64), "root_key_ids": ["root-a"],
                "root_threshold": 1
            }
        })
    }

    fn valid_v1_manifest() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "id": "test.manifest",
            "version": "1.0.0",
            "name": "Test manifest",
            "description": "A complete v1 manifest for schema-gate regression tests.",
            "publisher": { "id": "test.publisher", "name": "Test Publisher" },
            "license": { "spdx": "MIT" },
            "capabilities": ["tools"],
            "permissions": [],
            "distribution": { "type": "remote_http" },
            "transport": { "type": "streamable_http", "url": "https://example.com/mcp" },
            "auth": { "type": "none" },
            "health_check": { "type": "mcp_initialize", "timeout_seconds": 1 },
            "owned_files": [],
            "uninstall": { "mode": "remove_owned_files_only", "preserve_user_data": true }
        })
    }

    fn managed_v1_manifest(adapter: &str) -> serde_json::Value {
        let artifact = serde_json::json!({
            "platform": "windows",
            "arch": "x86_64",
            "url": "https://example.com/package",
            "digest": { "algorithm": "sha256", "value": "a".repeat(64) },
            "capacity": {
                "download_size_bytes": 10,
                "materialized_size_bytes": 20,
                "workspace_size_bytes": 30,
                "rollback_extra_bytes": 40
            }
        });
        let distribution = match adapter {
            "npm" => serde_json::json!({
                "type": "npm", "package": "example-package", "package_version": "1.0.0",
                "registry": "https://example.com", "artifacts": [artifact],
                "entrypoint": { "executable": "bin/example" }
            }),
            "python_wheel" => serde_json::json!({
                "type": "python_wheel", "package": "example_package", "package_version": "1.0.0",
                "python": ">=3.9", "index": "https://example.com", "artifacts": [artifact],
                "entrypoint": { "executable": "bin/example" }
            }),
            "binary_archive" => serde_json::json!({
                "type": "binary_archive", "archive_format": "zip", "artifacts": [artifact],
                "entrypoint": { "executable": "bin/example" }
            }),
            _ => unreachable!(),
        };
        let mut manifest = valid_v1_manifest();
        manifest["distribution"] = distribution;
        manifest["transport"] = serde_json::json!({ "type": "stdio" });
        manifest
    }

    #[test]
    fn managed_local_artifacts_accept_signed_capacity_contracts() {
        for adapter in ["npm", "python_wheel", "binary_archive"] {
            let verified =
                parse_manifest(&serde_json::to_vec(&managed_v1_manifest(adapter)).unwrap())
                    .unwrap();
            let artifact = &verified.manifest().distribution.artifacts()[0];
            assert_eq!(
                artifact
                    .capacity
                    .as_ref()
                    .unwrap()
                    .required_peak_bytes()
                    .unwrap(),
                100
            );
            assert!(std::str::from_utf8(verified.canonical_json())
                .unwrap()
                .contains("\"capacity\""));
        }
    }

    #[test]
    fn legacy_managed_artifact_capacity_is_unknown_and_never_uses_size_hint() {
        let mut legacy = managed_v1_manifest("npm");
        legacy["distribution"]["artifacts"][0]
            .as_object_mut()
            .unwrap()
            .remove("capacity");
        legacy["distribution"]["artifacts"][0]["size_bytes"] = serde_json::json!(99);
        let legacy = parse_manifest(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(
            legacy.manifest().distribution.artifacts()[0].capacity_contract(),
            ArtifactCapacityContract::Unknown
        );

        let known =
            parse_manifest(&serde_json::to_vec(&managed_v1_manifest("npm")).unwrap()).unwrap();
        assert_ne!(legacy.digest(), known.digest());
    }

    #[test]
    fn managed_capacity_rejects_zero_and_peak_overflow() {
        let mut zero = managed_v1_manifest("npm");
        zero["distribution"]["artifacts"][0]["capacity"]["workspace_size_bytes"] =
            serde_json::json!(0);
        assert_eq!(
            parse_manifest(&serde_json::to_vec(&zero).unwrap())
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::InvalidManifest
        );

        let mut overflow = managed_v1_manifest("npm");
        overflow["distribution"]["artifacts"][0]["capacity"] = serde_json::json!({
            "download_size_bytes": u64::MAX,
            "materialized_size_bytes": 1,
            "workspace_size_bytes": 1,
            "rollback_extra_bytes": 1
        });
        assert_eq!(
            parse_manifest(&serde_json::to_vec(&overflow).unwrap())
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::InvalidManifest
        );
    }

    #[test]
    fn managed_capacity_schema_requires_each_nonzero_field() {
        for field in [
            "download_size_bytes",
            "materialized_size_bytes",
            "workspace_size_bytes",
            "rollback_extra_bytes",
        ] {
            let mut missing = managed_v1_manifest("npm");
            missing["distribution"]["artifacts"][0]["capacity"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert_eq!(
                parse_manifest(&serde_json::to_vec(&missing).unwrap())
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::InvalidManifest
            );

            let mut zero = managed_v1_manifest("npm");
            zero["distribution"]["artifacts"][0]["capacity"][field] = serde_json::json!(0);
            assert_eq!(
                parse_manifest(&serde_json::to_vec(&zero).unwrap())
                    .unwrap_err()
                    .code(),
                McpPlatformErrorCode::InvalidManifest
            );
        }
    }

    #[test]
    fn each_capacity_input_changes_the_manifest_digest() {
        let baseline =
            parse_manifest(&serde_json::to_vec(&managed_v1_manifest("npm")).unwrap()).unwrap();
        for field in [
            "download_size_bytes",
            "materialized_size_bytes",
            "workspace_size_bytes",
            "rollback_extra_bytes",
        ] {
            let mut changed = managed_v1_manifest("npm");
            changed["distribution"]["artifacts"][0]["capacity"][field] = serde_json::json!(41);
            let changed = parse_manifest(&serde_json::to_vec(&changed).unwrap()).unwrap();
            assert_ne!(baseline.digest(), changed.digest());
        }
    }

    #[test]
    fn capacity_is_known_only_for_explicit_windows_artifacts() {
        for platform in ["linux", "any"] {
            let mut manifest = managed_v1_manifest("npm");
            manifest["distribution"]["artifacts"][0]["platform"] = serde_json::json!(platform);
            let verified = parse_manifest(&serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert_eq!(
                verified.manifest().distribution.artifacts()[0].capacity_contract(),
                ArtifactCapacityContract::Unknown
            );
        }

        let remote = parse_manifest(&serde_json::to_vec(&valid_v1_manifest()).unwrap()).unwrap();
        assert!(remote.manifest().distribution.artifacts().is_empty());
    }

    #[test]
    fn v1_rejects_runtime_and_lock_fields_and_v2_rejects_unknown_fields() {
        let v1 = valid_v1_manifest();
        assert!(parse_manifest(&serde_json::to_vec(&v1).unwrap()).is_ok());

        for field in ["runtime", "lock"] {
            let mut invalid = v1.clone();
            invalid[field] = serde_json::json!({});
            let bytes = serde_json::to_vec(&invalid).unwrap();
            assert_eq!(
                parse_manifest(&bytes).unwrap_err().code(),
                McpPlatformErrorCode::InvalidManifest
            );
        }

        let valid = serde_json::to_vec(&v2_runtime_contract()).unwrap();
        assert!(parse_manifest_v2_contract(&valid).is_ok());
        let mut unknown = v2_runtime_contract();
        unknown["unexpected"] = serde_json::json!(true);
        let unknown = serde_json::to_vec(&unknown).unwrap();
        assert_eq!(
            parse_manifest_v2_contract(&unknown).unwrap_err().code(),
            McpPlatformErrorCode::InvalidManifest
        );

        for field in ["runtime", "lock"] {
            let mut unknown = v2_runtime_contract();
            unknown[field]["unexpected"] = serde_json::json!(true);
            let unknown = serde_json::to_vec(&unknown).unwrap();
            assert_eq!(
                parse_manifest_v2_contract(&unknown).unwrap_err().code(),
                McpPlatformErrorCode::InvalidManifest
            );
        }
    }

    #[test]
    fn semver_parser_rejects_ranges_and_numeric_prerelease_leading_zero() {
        for invalid in ["latest", "^1.2.3", "1.2", "01.2.3", "1.2.3-01"] {
            assert_eq!(
                ExactVersion::parse(invalid).unwrap_err().code(),
                McpPlatformErrorCode::VersionNotExact
            );
        }
        assert!(ExactVersion::parse("1.2.3-alpha.1+build.7").is_ok());
    }

    #[test]
    fn trusted_enrollment_schema_allowlist_rejects_none_and_oauth2() {
        assert_eq!(
            trusted_credential_enrollment_schema(&Auth::None)
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::InvalidTransition
        );
        assert_eq!(
            trusted_credential_enrollment_schema(&Auth::Oauth2 {
                authorization_url: "https://example.com/auth".to_string(),
                token_url: "https://example.com/token".to_string(),
                client_registration: OAuthClientRegistration::Dynamic,
                scopes: vec!["scope.read".to_string()],
            })
            .unwrap_err()
            .code(),
            McpPlatformErrorCode::OperationNotSupported
        );
    }

    #[test]
    fn trusted_enrollment_schema_allowlist_maps_only_supported_secret_shapes() {
        let bearer = trusted_credential_enrollment_schema(&Auth::ApiKeyHeader {
            header_name: "Authorization".to_string(),
            prefix: Some("Bearer ".to_string()),
            credential_name: "legacy-bearer".to_string(),
        })
        .unwrap();
        assert_eq!(bearer.schema_id, "bearer_token");
        assert_eq!(bearer.fields[0].id, "secret");

        let header = trusted_credential_enrollment_schema(&Auth::ApiKeyHeader {
            header_name: "X-Api-Key".to_string(),
            prefix: None,
            credential_name: "legacy-header".to_string(),
        })
        .unwrap();
        assert_eq!(header.schema_id, "static_header_secret");

        let env = trusted_credential_enrollment_schema(&Auth::Environment {
            environment_key: "AUTH_TOKEN".to_string(),
            credential_name: "legacy-env".to_string(),
        })
        .unwrap();
        assert_eq!(env.schema_id, "static_env_secret");
        assert_eq!(env.fields[0].validation.max_length, 8192);
    }

    #[test]
    fn manifest_source_metadata_defaults_legacy_payloads_to_local_persistence() {
        let metadata: ManifestSourceMetadata =
            serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(metadata, ManifestSourceMetadata::local_persistence());
    }

    #[test]
    fn manifest_source_metadata_rejects_unknown_fields() {
        let error = serde_json::from_value::<ManifestSourceMetadata>(serde_json::json!({
            "source_ref": { "kind": "local_persistence" },
            "unexpected": true,
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn manifest_source_metadata_rejects_source_kind_and_digest_mismatches() {
        let mut metadata = ManifestSourceMetadata {
            source_ref: SourceRef::VerifiedSourceCatalog {
                source_id: "catalog-source".to_string(),
            },
            import_kind: SourceImportKind::HttpsManifestUrl,
            release_id: None,
            origin_provenance: OriginProvenance {
                verified_source_document: Some(VerifiedSourceDocumentRef {
                    source_id: "catalog-source".to_string(),
                    document_digest: "a".repeat(64),
                    canonical_digest: "b".repeat(64),
                    signed_digest: "c".repeat(64),
                    binding_digest: "d".repeat(64),
                    signature_kids: vec!["kid-1".to_string()],
                }),
            },
            update_channel: UpdateChannel::Default,
        };
        assert_eq!(
            metadata.validate().unwrap_err().code(),
            McpPlatformErrorCode::InvalidManifest
        );

        metadata.import_kind = SourceImportKind::VerifiedSourceCatalog;
        metadata
            .origin_provenance
            .verified_source_document
            .as_mut()
            .unwrap()
            .canonical_digest = "not-a-digest".to_string();
        assert_eq!(
            metadata.validate().unwrap_err().code(),
            McpPlatformErrorCode::InvalidDigest
        );
    }

    #[test]
    fn remote_manifest_url_is_serializable_but_not_verified_catalog_provenance() {
        let metadata = ManifestSourceMetadata {
            source_ref: SourceRef::HttpsManifestUrl {
                manifest_url: "https://example.com/mcp-manifest.json".to_string(),
            },
            import_kind: SourceImportKind::HttpsManifestUrl,
            release_id: None,
            origin_provenance: OriginProvenance::default(),
            update_channel: UpdateChannel::Default,
        };
        metadata.validate().unwrap();
        let encoded = serde_json::to_value(&metadata).unwrap();
        assert_eq!(encoded["source_ref"]["kind"], "https_manifest_url");
        assert!(encoded["origin_provenance"]["verified_source_document"].is_null());
        assert_ne!(
            metadata.source_ref,
            SourceRef::VerifiedSourceCatalog {
                source_id: "example.com".to_string(),
            }
        );
    }

    #[test]
    fn https_manifest_url_cannot_claim_official_trust() {
        let metadata = ManifestSourceMetadata {
            source_ref: SourceRef::HttpsManifestUrl {
                manifest_url: "https://example.com/mcp-manifest.json".to_string(),
            },
            import_kind: SourceImportKind::HttpsManifestUrl,
            ..ManifestSourceMetadata::local_persistence()
        };
        assert_eq!(
            metadata
                .validate_trust_tier(TrustTier::Official)
                .unwrap_err()
                .code(),
            McpPlatformErrorCode::InvalidManifest
        );
        metadata.validate_trust_tier(TrustTier::Local).unwrap();
    }

    #[test]
    fn manifest_source_metadata_accepts_verified_source_catalog_provenance() {
        let metadata = ManifestSourceMetadata {
            source_ref: SourceRef::VerifiedSourceCatalog {
                source_id: "catalog-source".to_string(),
            },
            import_kind: SourceImportKind::VerifiedSourceCatalog,
            release_id: Some("release-2026-07".to_string()),
            origin_provenance: OriginProvenance {
                verified_source_document: Some(VerifiedSourceDocumentRef {
                    source_id: "catalog-source".to_string(),
                    document_digest: "a".repeat(64),
                    canonical_digest: "b".repeat(64),
                    signed_digest: "c".repeat(64),
                    binding_digest: "d".repeat(64),
                    signature_kids: vec!["kid-1".to_string()],
                }),
            },
            update_channel: UpdateChannel::Named {
                name: "stable".to_string(),
            },
        };
        metadata.validate().unwrap();
    }

    #[test]
    fn manifest_source_metadata_rejects_catalog_claims_without_provenance() {
        let metadata = ManifestSourceMetadata {
            source_ref: SourceRef::VerifiedSourceCatalog {
                source_id: "catalog-source".to_string(),
            },
            import_kind: SourceImportKind::VerifiedSourceCatalog,
            release_id: Some("release-1".to_string()),
            origin_provenance: OriginProvenance::default(),
            update_channel: UpdateChannel::Default,
        };
        assert_eq!(
            metadata.validate().unwrap_err().code(),
            McpPlatformErrorCode::InvalidManifest
        );
    }
}
