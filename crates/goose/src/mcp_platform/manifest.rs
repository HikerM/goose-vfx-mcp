use std::collections::BTreeSet;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use url::Url;

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};

const MANIFEST_SCHEMA: &str =
    include_str!("../../../../documentation/static/schemas/mcp-package-manifest-v1.schema.json");
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
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

/// Parses schema-valid manifest bytes and computes a canonical SHA-256 digest.
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
                    McpPlatformErrorCode::ImmutableReferenceRequired,
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
                    McpPlatformErrorCode::PathTraversal,
                    "manifest paths must not traverse parent directories",
                ));
            }
            if key.is_some_and(is_url_field) {
                validate_https_url(text)?;
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
}
