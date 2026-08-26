//! Crate-private, compiled syntax contracts for future MCP package documents.
//!
//! Keeping this layer compiled preserves structural validation while every returned value
//! remains untrusted syntax data. It has no runtime ingress or effect wiring: do not
//! public re-export it or connect it to HTTP, ACP, UI, CLI, persistence, managed MCP,
//! installation, download, signature or authorization decisions, process, or network code.

use std::collections::{BTreeSet, HashSet};

use anyhow::{bail, Result};
use serde::{de, Deserialize, Deserializer};

pub(crate) const MANIFEST_DOCUMENT_TYPE: &str = "lumina.mcp.package-manifest";
pub(crate) const AUTHORIZATION_DOCUMENT_TYPE: &str = "lumina.mcp.install-authorization";
pub(crate) const ADAPTER_CONTRACT_DOCUMENT_TYPE: &str = "lumina.mcp.adapter-capability-contract";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UntrustedDocument {
    PackageManifest(PackageManifestV3),
    InstallAuthorization(InstallAuthorizationEnvelopeV1),
    AdapterCapabilityContract(AdapterCapabilityContract),
}

/// Parses only structural syntax. A returned value has no trust, authorization, or
/// execution meaning.
pub(crate) fn parse_untrusted_document(input: &str) -> Result<UntrustedDocument> {
    if input.trim().is_empty() {
        bail!("MCP contract input is empty");
    }
    reject_duplicate_keys(input)?;
    let header: DocumentHeader = serde_json::from_str(input)?;
    match (header.document_type.as_str(), header.schema_version) {
        (MANIFEST_DOCUMENT_TYPE, 3) => {
            let document: PackageManifestV3 = serde_json::from_str(input)?;
            document.validate()?;
            Ok(UntrustedDocument::PackageManifest(document))
        }
        (AUTHORIZATION_DOCUMENT_TYPE, 1) => {
            let document: InstallAuthorizationEnvelopeV1 = serde_json::from_str(input)?;
            document.validate()?;
            Ok(UntrustedDocument::InstallAuthorization(document))
        }
        (ADAPTER_CONTRACT_DOCUMENT_TYPE, 1) => {
            let document: AdapterCapabilityContract = serde_json::from_str(input)?;
            document.validate()?;
            Ok(UntrustedDocument::AdapterCapabilityContract(document))
        }
        _ => bail!("unsupported MCP contract document type or schema version"),
    }
}

#[derive(Debug, Deserialize)]
struct DocumentHeader {
    document_type: String,
    schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageManifestV3 {
    pub(crate) document_type: String,
    pub(crate) schema_version: u32,
    pub(crate) package: PackageIdentity,
    pub(crate) adapter: AdapterRequirement,
    pub(crate) transport: TransportProtocol,
    pub(crate) entrypoint: TypedEntrypoint,
    pub(crate) platform_variants: Vec<PlatformVariant>,
    pub(crate) closure: ArtifactClosure,
    pub(crate) permissions: Vec<String>,
    pub(crate) config_schema: ConfigSchemaDescriptor,
    pub(crate) owned_resource_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageIdentity {
    pub(crate) id: String,
    pub(crate) version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdapterRequirement {
    pub(crate) id: String,
    pub(crate) version_range: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransportProtocol {
    pub(crate) transport: String,
    pub(crate) protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TypedEntrypoint {
    pub(crate) kind: String,
    pub(crate) immutable_reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlatformVariant {
    pub(crate) selector: String,
    pub(crate) node_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactClosure {
    pub(crate) nodes: Vec<ClosureNode>,
    pub(crate) edges: Vec<ClosureEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClosureNode {
    pub(crate) id: String,
    pub(crate) role: String,
    pub(crate) sha256: String,
    pub(crate) length_bytes: u64,
    pub(crate) origin_identity: String,
    pub(crate) selector: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClosureEdge {
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfigSchemaDescriptor {
    pub(crate) kind: String,
    pub(crate) schema_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallAuthorizationEnvelopeV1 {
    pub(crate) document_type: String,
    pub(crate) schema_version: u32,
    pub(crate) authorization_id: String,
    pub(crate) issuer: String,
    pub(crate) tenant: String,
    pub(crate) manifest_digest: String,
    pub(crate) closure_digest: String,
    pub(crate) artifact_bindings: Vec<ArtifactBinding>,
    pub(crate) adapter_contract: ExactAdapterContract,
    pub(crate) platform: String,
    pub(crate) mode: AuthorizationMode,
    pub(crate) issued_at_ms: u64,
    pub(crate) not_before_ms: u64,
    pub(crate) expires_at_ms: u64,
    pub(crate) revocation: RevocationSyntax,
    pub(crate) offline: OfflineSyntax,
    pub(crate) signature: SignatureMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactBinding {
    pub(crate) artifact_id: String,
    pub(crate) sha256: String,
    pub(crate) length_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExactAdapterContract {
    pub(crate) id: String,
    pub(crate) version: String,
    pub(crate) digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuthorizationMode {
    OfflineBound,
    OnlineBound,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevocationSyntax {
    pub(crate) reference_digest: String,
    pub(crate) checked_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OfflineSyntax {
    pub(crate) allowed: bool,
    pub(crate) maximum_age_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SignatureMetadata {
    pub(crate) algorithm: String,
    pub(crate) key_id: String,
    pub(crate) signature_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdapterCapabilityContract {
    pub(crate) document_type: String,
    pub(crate) schema_version: u32,
    pub(crate) capability_id: String,
    pub(crate) contract_version: String,
    pub(crate) contract_digest: String,
    pub(crate) supported_manifest_schema_versions: Vec<u32>,
    pub(crate) operations: Vec<AdapterOperation>,
    pub(crate) entrypoint_kinds: Vec<String>,
    pub(crate) transport_protocols: Vec<TransportProtocolRange>,
    pub(crate) platform_requirements: Vec<PlatformRequirement>,
    pub(crate) permission_kinds: Vec<String>,
    pub(crate) config_kinds: Vec<String>,
    pub(crate) dependency_resolution: DependencyResolution,
    pub(crate) trusted_launcher: TrustedLauncherIdentity,
    pub(crate) recovery: RecoveryBounds,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdapterOperation {
    pub(crate) id: String,
    pub(crate) request_kind: String,
    pub(crate) response_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransportProtocolRange {
    pub(crate) transport: String,
    pub(crate) protocol_range: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlatformRequirement {
    pub(crate) selector: String,
    pub(crate) runtime: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DependencyResolution {
    ClosedLockOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrustedLauncherIdentity {
    pub(crate) identity: String,
    pub(crate) digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryBounds {
    pub(crate) rollback_supported: bool,
    pub(crate) maximum_attempts: u32,
}

impl PackageManifestV3 {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.document_type != MANIFEST_DOCUMENT_TYPE
            || self.schema_version != 3
            || !valid_id(&self.package.id)
            || !valid_version(&self.package.version)
            || !valid_id(&self.adapter.id)
            || !valid_version_range(&self.adapter.version_range)
            || !valid_id(&self.transport.transport)
            || !valid_id(&self.transport.protocol)
            || !valid_entrypoint(&self.entrypoint)
            || !valid_id(&self.config_schema.kind)
            || !valid_digest(&self.config_schema.schema_digest)
            || self.closure.nodes.is_empty()
        {
            bail!("invalid untrusted package manifest shape");
        }
        unique_ids(&self.permissions)?;
        unique_ids(&self.owned_resource_ids)?;
        validate_closure(&self.closure)?;
        validate_variants(&self.platform_variants, &self.closure)?;
        Ok(())
    }
}

impl InstallAuthorizationEnvelopeV1 {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.document_type != AUTHORIZATION_DOCUMENT_TYPE
            || self.schema_version != 1
            || !valid_id(&self.authorization_id)
            || !valid_id(&self.issuer)
            || !valid_id(&self.tenant)
            || !valid_digest(&self.manifest_digest)
            || !valid_digest(&self.closure_digest)
            || !valid_id(&self.adapter_contract.id)
            || !valid_version(&self.adapter_contract.version)
            || !valid_digest(&self.adapter_contract.digest)
            || !valid_selector(&self.platform)
            || self.issued_at_ms > self.not_before_ms
            || self.not_before_ms > self.expires_at_ms
            || !valid_digest(&self.revocation.reference_digest)
            || self.offline.maximum_age_ms == 0
            || !valid_id(&self.signature.algorithm)
            || !valid_id(&self.signature.key_id)
            || !valid_digest(&self.signature.signature_digest)
            || self.artifact_bindings.is_empty()
        {
            bail!("invalid untrusted authorization envelope shape");
        }
        let artifact_ids: Vec<_> = self
            .artifact_bindings
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect();
        unique_ids(&artifact_ids)?;
        if self.artifact_bindings.iter().any(|artifact| {
            !valid_id(&artifact.artifact_id)
                || !valid_digest(&artifact.sha256)
                || artifact.length_bytes == 0
        }) {
            bail!("invalid untrusted authorization artifact binding");
        }
        Ok(())
    }
}

impl AdapterCapabilityContract {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.document_type != ADAPTER_CONTRACT_DOCUMENT_TYPE
            || self.schema_version != 1
            || !valid_id(&self.capability_id)
            || !valid_version(&self.contract_version)
            || !valid_digest(&self.contract_digest)
            || self.supported_manifest_schema_versions.is_empty()
            || self.operations.is_empty()
            || self.entrypoint_kinds.is_empty()
            || self.transport_protocols.is_empty()
            || self.platform_requirements.is_empty()
            || self.permission_kinds.is_empty()
            || self.config_kinds.is_empty()
            || !valid_id(&self.trusted_launcher.identity)
            || !valid_digest(&self.trusted_launcher.digest)
            || self.recovery.maximum_attempts == 0
        {
            bail!("invalid untrusted adapter capability contract shape");
        }
        let operation_ids: Vec<_> = self.operations.iter().map(|item| item.id.clone()).collect();
        let selector_ids: Vec<_> = self
            .platform_requirements
            .iter()
            .map(|item| item.selector.clone())
            .collect();
        unique_ids(&operation_ids)?;
        unique_ids(&self.entrypoint_kinds)?;
        unique_ids(&self.permission_kinds)?;
        unique_ids(&self.config_kinds)?;
        unique_u32(&self.supported_manifest_schema_versions)?;
        unique_ids(&selector_ids)?;
        unique_transport_protocols(&self.transport_protocols)?;
        unique_platform_requirements(&self.platform_requirements)?;
        if self.operations.iter().any(|operation| {
            !valid_id(&operation.id)
                || !valid_id(&operation.request_kind)
                || !valid_id(&operation.response_kind)
        }) || self.entrypoint_kinds.iter().any(|kind| !valid_id(kind))
            || self.permission_kinds.iter().any(|kind| !valid_id(kind))
            || self.config_kinds.iter().any(|kind| !valid_id(kind))
            || self.transport_protocols.iter().any(|item| {
                !valid_id(&item.transport) || !valid_version_range(&item.protocol_range)
            })
            || self
                .platform_requirements
                .iter()
                .any(|item| !valid_selector(&item.selector) || !valid_id(&item.runtime))
        {
            bail!("invalid untrusted adapter capability contract member");
        }
        Ok(())
    }
}

fn validate_variants(variants: &[PlatformVariant], closure: &ArtifactClosure) -> Result<()> {
    if variants.is_empty() {
        bail!("package manifest requires platform variants");
    }
    let selectors: Vec<_> = variants
        .iter()
        .map(|variant| variant.selector.clone())
        .collect();
    unique_ids(&selectors)?;
    let closure_node_selectors: std::collections::BTreeMap<_, _> = closure
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.selector.as_str()))
        .collect();
    if variants.iter().any(|variant| {
        !valid_selector(&variant.selector)
            || variant.node_ids.is_empty()
            || variant.node_ids.iter().any(|node| {
                !valid_id(node)
                    || closure_node_selectors
                        .get(node.as_str())
                        .map(|selector| *selector)
                        != Some(variant.selector.as_str())
            })
    }) {
        bail!("invalid or ambiguous platform variant");
    }
    for variant in variants {
        unique_ids(&variant.node_ids)?;
    }
    if closure.nodes.iter().any(|node| {
        !variants.iter().any(|variant| {
            variant.selector == node.selector && variant.node_ids.iter().any(|id| id == &node.id)
        })
    }) {
        bail!("closure node is not owned by a matching platform variant");
    }
    Ok(())
}

fn validate_closure(closure: &ArtifactClosure) -> Result<()> {
    let node_ids: Vec<_> = closure.nodes.iter().map(|node| node.id.clone()).collect();
    unique_ids(&node_ids)?;
    if closure.nodes.iter().any(|node| {
        !valid_id(&node.id)
            || !valid_id(&node.role)
            || !valid_digest(&node.sha256)
            || node.length_bytes == 0
            || !valid_id(&node.origin_identity)
            || !valid_selector(&node.selector)
    }) {
        bail!("invalid unlocked closure node");
    }
    let known: BTreeSet<_> = node_ids.into_iter().collect();
    if closure.edges.iter().any(|edge| {
        !valid_id(&edge.from)
            || !valid_id(&edge.to)
            || !known.contains(&edge.from)
            || !known.contains(&edge.to)
    }) {
        bail!("closure edge is malformed or dangling");
    }
    let edge_pairs: HashSet<_> = closure
        .edges
        .iter()
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect();
    if edge_pairs.len() != closure.edges.len() {
        bail!("closure edges must be unique by from/to pair");
    }
    let mut adjacency = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for edge in &closure.edges {
        adjacency.entry(&edge.from).or_default().push(&edge.to);
    }
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for node in &closure.nodes {
        if has_cycle(&node.id, &adjacency, &mut visiting, &mut visited) {
            bail!("closure graph contains a cycle");
        }
    }
    Ok(())
}

fn has_cycle<'a>(
    node: &'a str,
    adjacency: &std::collections::BTreeMap<&'a str, Vec<&'a str>>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
) -> bool {
    if visited.contains(node) {
        return false;
    }
    if !visiting.insert(node) {
        return true;
    }
    let cycle = adjacency.get(node).is_some_and(|next| {
        next.iter()
            .any(|next| has_cycle(next, adjacency, visiting, visited))
    });
    visiting.remove(node);
    visited.insert(node);
    cycle
}

fn valid_entrypoint(entrypoint: &TypedEntrypoint) -> bool {
    matches!(
        entrypoint.kind.as_str(),
        "module" | "library" | "descriptor"
    ) && valid_relative_reference(&entrypoint.immutable_reference)
}

fn valid_relative_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('/')
        && !value.contains('\\')
        && value.split('/').all(|part| valid_id(part))
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_version(value: &str) -> bool {
    (1..=128).contains(&value.len()) && semver::Version::parse(value).is_ok()
}

fn valid_version_range(value: &str) -> bool {
    (1..=128).contains(&value.len()) && semver::VersionReq::parse(value).is_ok()
}

fn valid_selector(value: &str) -> bool {
    valid_id(value) && !value.contains('*')
}

fn unique_ids(values: &[String]) -> Result<()> {
    if values.iter().any(|value| !valid_id(value))
        || values.iter().collect::<HashSet<_>>().len() != values.len()
    {
        bail!("identifier list must contain unique valid identifiers");
    }
    Ok(())
}

fn unique_u32(values: &[u32]) -> Result<()> {
    if values.iter().any(|value| *value == 0)
        || values.iter().collect::<HashSet<_>>().len() != values.len()
    {
        bail!("numeric list must contain unique values");
    }
    Ok(())
}

fn unique_transport_protocols(values: &[TransportProtocolRange]) -> Result<()> {
    let pairs: HashSet<_> = values
        .iter()
        .map(|value| (value.transport.as_str(), value.protocol_range.as_str()))
        .collect();
    if pairs.len() != values.len() {
        bail!("transport protocol ranges must be unique");
    }
    Ok(())
}

fn unique_platform_requirements(values: &[PlatformRequirement]) -> Result<()> {
    let pairs: HashSet<_> = values
        .iter()
        .map(|value| (value.selector.as_str(), value.runtime.as_str()))
        .collect();
    if pairs.len() != values.len() {
        bail!("platform requirements must be unique");
    }
    Ok(())
}

fn reject_duplicate_keys(input: &str) -> Result<()> {
    let mut deserializer = serde_json::Deserializer::from_str(input);
    DuplicateChecked::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(())
}

struct DuplicateChecked;

impl<'de> Deserialize<'de> for DuplicateChecked {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DuplicateKeyVisitor)
    }
}

struct DuplicateKeyVisitor;

impl<'de> de::Visitor<'de> for DuplicateKeyVisitor {
    type Value = DuplicateChecked;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("valid JSON without duplicate object keys")
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateChecked)
    }
    fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateChecked)
    }
    fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateChecked)
    }
    fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateChecked)
    }
    fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateChecked)
    }
    fn visit_string<E>(self, _: String) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateChecked)
    }
    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateChecked)
    }
    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateChecked)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        while sequence.next_element::<DuplicateChecked>()?.is_some() {}
        Ok(DuplicateChecked)
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        while let Some((key, _)) = map.next_entry::<String, DuplicateChecked>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
        }
        Ok(DuplicateChecked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> String {
        "a".repeat(64)
    }

    fn manifest_json() -> String {
        format!(
            r#"{{"document_type":"{MANIFEST_DOCUMENT_TYPE}","schema_version":3,"package":{{"id":"demo.package","version":"1.0.0"}},"adapter":{{"id":"demo.adapter","version_range":"^1.0"}},"transport":{{"transport":"stdio","protocol":"mcp"}},"entrypoint":{{"kind":"module","immutable_reference":"bundle/main"}},"platform_variants":[{{"selector":"linux-x64","node_ids":["main"]}}],"closure":{{"nodes":[{{"id":"main","role":"entry","sha256":"{}","length_bytes":1,"origin_identity":"origin","selector":"linux-x64"}}],"edges":[]}},"permissions":["network"],"config_schema":{{"kind":"json","schema_digest":"{}"}},"owned_resource_ids":["resource"]}}"#,
            digest(),
            digest()
        )
    }

    fn envelope_json() -> String {
        format!(
            r#"{{"document_type":"{AUTHORIZATION_DOCUMENT_TYPE}","schema_version":1,"authorization_id":"auth","issuer":"issuer","tenant":"tenant","manifest_digest":"{}","closure_digest":"{}","artifact_bindings":[{{"artifact_id":"main","sha256":"{}","length_bytes":1}}],"adapter_contract":{{"id":"demo.adapter","version":"1.0.0","digest":"{}"}},"platform":"linux-x64","mode":"offline_bound","issued_at_ms":1,"not_before_ms":1,"expires_at_ms":1,"revocation":{{"reference_digest":"{}","checked_at_ms":0}},"offline":{{"allowed":true,"maximum_age_ms":1}},"signature":{{"algorithm":"ed25519","key_id":"key","signature_digest":"{}"}}}}"#,
            digest(),
            digest(),
            digest(),
            digest(),
            digest(),
            digest()
        )
    }

    fn contract_json() -> String {
        format!(
            r#"{{"document_type":"{ADAPTER_CONTRACT_DOCUMENT_TYPE}","schema_version":1,"capability_id":"demo.adapter","contract_version":"1.0.0","contract_digest":"{}","supported_manifest_schema_versions":[3],"operations":[{{"id":"inspect","request_kind":"manifest","response_kind":"result"}}],"entrypoint_kinds":["module"],"transport_protocols":[{{"transport":"stdio","protocol_range":"^1.0"}}],"platform_requirements":[{{"selector":"linux-x64","runtime":"node"}}],"permission_kinds":["network"],"config_kinds":["json"],"dependency_resolution":"closed_lock_only","trusted_launcher":{{"identity":"launcher","digest":"{}"}},"recovery":{{"rollback_supported":true,"maximum_attempts":1}}}}"#,
            digest(),
            digest()
        )
    }

    #[test]
    fn documents_parse_only_as_untrusted_syntax() {
        assert!(matches!(
            parse_untrusted_document(&manifest_json()).unwrap(),
            UntrustedDocument::PackageManifest(_)
        ));
        assert!(matches!(
            parse_untrusted_document(&envelope_json()).unwrap(),
            UntrustedDocument::InstallAuthorization(_)
        ));
        assert!(matches!(
            parse_untrusted_document(&contract_json()).unwrap(),
            UntrustedDocument::AdapterCapabilityContract(_)
        ));
        let UntrustedDocument::InstallAuthorization(envelope) =
            parse_untrusted_document(&envelope_json()).unwrap()
        else {
            unreachable!();
        };
        assert_eq!(envelope.authorization_id, "auth");
    }

    #[test]
    fn duplicate_and_unknown_keys_are_rejected_at_every_level() {
        for document in [
            manifest_json().replacen(
                "\"schema_version\":3",
                "\"schema_version\":3,\"schema_version\":3",
                1,
            ),
            manifest_json().replace(
                "\"role\":\"entry\"",
                "\"role\":\"entry\",\"role\":\"entry\"",
            ),
            manifest_json().replace(
                "{\"selector\":\"linux-x64\",\"node_ids\":[\"main\"]}",
                "{\"selector\":\"linux-x64\",\"selector\":\"linux-x64\",\"node_ids\":[\"main\"]}",
            ),
            envelope_json().replace(
                "\"length_bytes\":1",
                "\"length_bytes\":1,\"length_bytes\":1",
            ),
            contract_json().replace(
                "\"id\":\"inspect\"",
                "\"id\":\"inspect\",\"id\":\"inspect\"",
            ),
            manifest_json().replace(
                "\"owned_resource_ids\"",
                "\"shell\":\"bad\",\"owned_resource_ids\"",
            ),
            manifest_json().replacen(
                &format!("\"document_type\":\"{MANIFEST_DOCUMENT_TYPE}\""),
                &format!(
                    "\"document_type\":\"{MANIFEST_DOCUMENT_TYPE}\",\"doc\\u0075ment_type\":\"{MANIFEST_DOCUMENT_TYPE}\""
                ),
                1,
            ),
        ] {
            assert!(parse_untrusted_document(&document).is_err());
        }
    }

    #[test]
    fn malformed_json_and_header_extras_are_rejected_before_or_by_full_document_parsing() {
        for document in [
            "not-json".to_string(),
            format!("{} {{}}", manifest_json()),
            manifest_json().replacen(
                "\"schema_version\":3",
                "\"schema_version\":3,\"header_extra\":true",
                1,
            ),
        ] {
            assert!(parse_untrusted_document(&document).is_err());
        }
    }

    #[test]
    fn manifest_shape_failures_are_rejected() {
        for document in [
            manifest_json().replace(&digest(), &"A".repeat(64)),
            manifest_json().replace("\"length_bytes\":1", "\"length_bytes\":0"),
            manifest_json().replace("\"permissions\":[\"network\"]", "\"permissions\":[\"network\",\"network\"]"),
            manifest_json().replace("\"edges\":[]", "\"edges\":[{\"from\":\"main\",\"to\":\"missing\"}]"),
            manifest_json().replace("\"edges\":[]", "\"edges\":[{\"from\":\"main\",\"to\":\"main\"}]"),
            manifest_json().replace(
                "\"node_ids\":[\"main\"]",
                "\"node_ids\":[\"main\",\"main\"]",
            ),
            manifest_json().replace("\"platform_variants\":[{\"selector\":\"linux-x64\",\"node_ids\":[\"main\"]}]", "\"platform_variants\":[{\"selector\":\"linux-x64\",\"node_ids\":[\"main\"]},{\"selector\":\"linux-x64\",\"node_ids\":[\"main\"]}]"),
            manifest_json().replace(
                "\"selector\":\"linux-x64\",\"node_ids\":[\"main\"]",
                "\"selector\":\"windows-x64\",\"node_ids\":[\"main\"]",
            ),
        ] {
            assert!(parse_untrusted_document(&document).is_err());
        }
        let mut empty_closure: serde_json::Value = serde_json::from_str(&manifest_json()).unwrap();
        empty_closure["closure"]["nodes"] = serde_json::json!([]);
        assert!(parse_untrusted_document(&empty_closure.to_string()).is_err());

        let mut isolated_node: serde_json::Value = serde_json::from_str(&manifest_json()).unwrap();
        isolated_node["closure"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "secondary",
                "role": "library",
                "sha256": digest(),
                "length_bytes": 1,
                "origin_identity": "origin",
                "selector": "linux-x64"
            }));
        assert!(parse_untrusted_document(&isolated_node.to_string()).is_err());

        let mut duplicate_edges: serde_json::Value =
            serde_json::from_str(&manifest_json()).unwrap();
        duplicate_edges["closure"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "secondary",
                "role": "library",
                "sha256": digest(),
                "length_bytes": 1,
                "origin_identity": "origin",
                "selector": "linux-x64"
            }));
        duplicate_edges["platform_variants"][0]["node_ids"] =
            serde_json::json!(["main", "secondary"]);
        duplicate_edges["closure"]["edges"] = serde_json::json!([
            {"from": "main", "to": "secondary"},
            {"from": "main", "to": "secondary"}
        ]);
        assert!(parse_untrusted_document(&duplicate_edges.to_string()).is_err());
    }

    #[test]
    fn schema_bounded_versions_and_required_artifacts_are_rejected_when_invalid() {
        let overlong_version = format!("1.0.0-{}", "a".repeat(123));
        let overlong_range = format!("^1.0.0-{}", "a".repeat(122));
        for document in [
            manifest_json().replace("\"version\":\"1.0.0\"", &format!("\"version\":\"{overlong_version}\"")),
            manifest_json().replace("\"version_range\":\"^1.0\"", &format!("\"version_range\":\"{overlong_range}\"")),
            envelope_json().replace("\"artifact_bindings\":[{\"artifact_id\":\"main\",\"sha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"length_bytes\":1}]", "\"artifact_bindings\":[]"),
        ] {
            assert!(parse_untrusted_document(&document).is_err());
        }
    }

    #[test]
    fn parser_fail_closes_semver_syntax_not_fully_expressed_by_schema() {
        for document in [
            manifest_json().replace("\"version\":\"1.0.0\"", "\"version\":\"1.0\""),
            manifest_json().replace("\"version\":\"1.0.0\"", "\"version\":\"01.0.0\""),
            envelope_json().replace("\"version\":\"1.0.0\"", "\"version\":\"1.0.0-01\""),
            manifest_json().replace(
                "\"version_range\":\"^1.0\"",
                "\"version_range\":\"not-a-range\"",
            ),
            contract_json().replace("\"protocol_range\":\"^1.0\"", "\"protocol_range\":\"1..0\""),
        ] {
            assert!(parse_untrusted_document(&document).is_err());
        }
    }

    #[test]
    fn parser_retains_cross_document_semantics_schema_cannot_express() {
        let duplicate_artifact_id = envelope_json().replace(
            "\"artifact_bindings\":[{\"artifact_id\":\"main\",\"sha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"length_bytes\":1}]",
            "\"artifact_bindings\":[{\"artifact_id\":\"main\",\"sha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"length_bytes\":1},{\"artifact_id\":\"main\",\"sha256\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"length_bytes\":2}]",
        );
        let out_of_order_timestamps = envelope_json().replace(
            "\"issued_at_ms\":1,\"not_before_ms\":1,\"expires_at_ms\":1",
            "\"issued_at_ms\":2,\"not_before_ms\":1,\"expires_at_ms\":3",
        );
        let duplicate_platform_selector = contract_json().replace(
            "\"platform_requirements\":[{\"selector\":\"linux-x64\",\"runtime\":\"node\"}]",
            "\"platform_requirements\":[{\"selector\":\"linux-x64\",\"runtime\":\"node\"},{\"selector\":\"linux-x64\",\"runtime\":\"deno\"}]",
        );
        let mut duplicate_variant_selector: serde_json::Value =
            serde_json::from_str(&manifest_json()).unwrap();
        duplicate_variant_selector["closure"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "secondary",
                "role": "library",
                "sha256": digest(),
                "length_bytes": 1,
                "origin_identity": "origin",
                "selector": "linux-x64"
            }));
        duplicate_variant_selector["platform_variants"] = serde_json::json!([
            {"selector": "linux-x64", "node_ids": ["main"]},
            {"selector": "linux-x64", "node_ids": ["secondary"]}
        ]);

        for document in [
            duplicate_artifact_id,
            out_of_order_timestamps,
            duplicate_platform_selector,
            duplicate_variant_selector.to_string(),
        ] {
            assert!(parse_untrusted_document(&document).is_err());
        }
    }

    #[test]
    fn adapter_contract_schema_constraints_are_rejected_when_invalid() {
        for document in [
            contract_json().replace("\"supported_manifest_schema_versions\":[3]", "\"supported_manifest_schema_versions\":[0]"),
            contract_json().replace(
                "\"transport_protocols\":[{\"transport\":\"stdio\",\"protocol_range\":\"^1.0\"}]",
                "\"transport_protocols\":[{\"transport\":\"stdio\",\"protocol_range\":\"^1.0\"},{\"transport\":\"stdio\",\"protocol_range\":\"^1.0\"}]",
            ),
            contract_json().replace(
                "\"platform_requirements\":[{\"selector\":\"linux-x64\",\"runtime\":\"node\"}]",
                "\"platform_requirements\":[{\"selector\":\"linux-x64\",\"runtime\":\"node\"},{\"selector\":\"linux-x64\",\"runtime\":\"node\"}]",
            ),
        ] {
            assert!(parse_untrusted_document(&document).is_err());
        }
    }

    #[test]
    fn envelope_timestamps_and_signature_are_syntax_without_authorization() {
        let syntactically_old = envelope_json().replace(
            "\"issued_at_ms\":1,\"not_before_ms\":1,\"expires_at_ms\":1",
            "\"issued_at_ms\":0,\"not_before_ms\":0,\"expires_at_ms\":0",
        );
        assert!(parse_untrusted_document(&syntactically_old).is_ok());
    }

    #[test]
    fn legacy_document_types_are_not_accepted() {
        assert!(parse_untrusted_document(
            &manifest_json().replace(MANIFEST_DOCUMENT_TYPE, "lumina.mcp.package-manifest-v2")
        )
        .is_err());
        assert!(parse_untrusted_document(
            "{\"document_type\":\"lumina.mcp.package-manifest\",\"schema_version\":2}"
        )
        .is_err());
    }
}
