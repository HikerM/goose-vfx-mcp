use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use sha2::{Digest as _, Sha256};
use url::Url;

use crate::mcp_platform::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use crate::mcp_platform::manifest::parse_manifest;
use crate::mcp_platform::repository::{
    GovernedSourceRefreshTransportKind, GovernedSourceTrustBasis,
};
use crate::verified_source_catalog::strict_json::{
    parse_json_bytes, write_json_string, AstValue, ParserLimits,
};
use crate::verified_source_catalog::{SourceSignatureV1, VerifiedSourceDocument};

pub(crate) const SOURCE_PROVISIONING_DESCRIPTOR_FORMAT: &str = "goose-source-provisioning-v1";
pub(crate) const SOURCE_PROVISIONING_CONFIRMATION_TTL_MS: i64 = 5 * 60 * 1000;

const MAX_DESCRIPTOR_BYTES: usize = 256 * 1024;
const MAX_DESCRIPTOR_STRING_BYTES: usize = 4096;
const MAX_DESCRIPTOR_SIGNATURES: usize = 32;
const MAX_SOURCE_PROVISIONING_MANIFESTS: usize = 512;
const MAX_SOURCE_PROVISIONING_STAGES: usize = 16;
const MAX_SOURCE_PROVISIONING_STAGES_PER_ACTOR: usize = 4;
const MAX_SOURCE_PROVISIONING_STAGED_BYTES: usize = 32 * 1024 * 1024;
const SNAPSHOT_DIGEST_DOMAIN: &[u8] = b"goose-source-provisioning-snapshot-v1\0";
const DESCRIPTOR_DIGEST_DOMAIN: &[u8] = b"goose-source-provisioning-descriptor-v1\0";
const TRANSPORT_SESSION_BINDING_DIGEST_DOMAIN: &[u8] =
    b"goose-source-provisioning-transport-session-binding-v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrozenSourceProvisioningManifest {
    pub(crate) release_id: String,
    pub(crate) document_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrozenSourceProvisioningSnapshot {
    pub(crate) descriptor_bytes: Vec<u8>,
    pub(crate) source_document_bytes: Vec<u8>,
    pub(crate) manifests: Vec<FrozenSourceProvisioningManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceProvisioningDescriptor {
    pub(crate) source_id: String,
    pub(crate) source_document_canonical_digest: String,
    pub(crate) root_digest: String,
    pub(crate) transport_kind: GovernedSourceRefreshTransportKind,
    pub(crate) endpoint: String,
    pub(crate) endpoint_host: String,
    pub(crate) descriptor_digest: String,
    signatures: Vec<SourceSignatureV1>,
    canonical_payload_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedSourceProvisioningSnapshot {
    pub(crate) source_document: VerifiedSourceDocument,
    pub(crate) descriptor: SourceProvisioningDescriptor,
    pub(crate) manifest_count: u32,
    pub(crate) snapshot_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceProvisioningPreview {
    pub(crate) source_id: String,
    pub(crate) display_name: String,
    pub(crate) root_digest: String,
    pub(crate) document_digest: String,
    pub(crate) endpoint_host: String,
    pub(crate) transport_kind: GovernedSourceRefreshTransportKind,
    pub(crate) manifest_count: u32,
    pub(crate) warnings: Vec<String>,
    pub(crate) trust_basis: GovernedSourceTrustBasis,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceProvisioningStage {
    pub(crate) provision_id: String,
    pub(crate) actor: String,
    pub(crate) correlation_id: String,
    transport_session_binding_digest: String,
    pub(crate) expires_at_ms: i64,
    pub(crate) snapshot_digest: String,
    pub(crate) frozen: FrozenSourceProvisioningSnapshot,
    pub(crate) preview: SourceProvisioningPreview,
    confirmation_hash: String,
    in_flight: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceProvisioningConfirmation {
    pub(crate) provision_id: String,
    pub(crate) confirmation_token: String,
    pub(crate) expires_at_ms: i64,
}

#[derive(Default)]
pub(crate) struct SourceProvisioningCoordinator {
    stages: Mutex<HashMap<String, SourceProvisioningStage>>,
}

impl SourceProvisioningCoordinator {
    pub(crate) fn stage(
        &self,
        provision_id: String,
        confirmation_token: String,
        actor: &str,
        transport_session_binding: &str,
        frozen: FrozenSourceProvisioningSnapshot,
        parsed: &ParsedSourceProvisioningSnapshot,
        now_ms: i64,
    ) -> McpPlatformResult<(SourceProvisioningStage, SourceProvisioningConfirmation)> {
        if provision_id.is_empty()
            || provision_id.len() > 256
            || transport_session_binding.is_empty()
            || transport_session_binding.len() > 256
        {
            return Err(source_provisioning_invalid());
        }
        if confirmation_token.len() > 512
            || !confirmation_token.starts_with("source_provisioning_confirmation_")
        {
            return Err(source_provisioning_invalid());
        }
        let expires_at_ms = now_ms
            .checked_add(SOURCE_PROVISIONING_CONFIRMATION_TTL_MS)
            .ok_or_else(source_provisioning_temporarily_unavailable)?;
        let confirmation_hash = sha256_hex(confirmation_token.as_bytes());
        let preview = preview_from_parsed(parsed);
        let stage = SourceProvisioningStage {
            provision_id: provision_id.clone(),
            actor: actor.to_string(),
            // ACP gives every RPC a new request correlation. The provision id is the stable,
            // server-issued correlation for this two-step workflow and is also checked below.
            correlation_id: provision_id.clone(),
            transport_session_binding_digest: transport_session_binding_digest(
                transport_session_binding,
            ),
            expires_at_ms,
            snapshot_digest: parsed.snapshot_digest.clone(),
            frozen,
            preview,
            confirmation_hash,
            in_flight: false,
        };
        let mut stages = self
            .stages
            .lock()
            .map_err(|_| source_provisioning_temporarily_unavailable())?;
        purge_expired_stages(&mut stages, now_ms);
        let staged_bytes = frozen_snapshot_bytes(&stage.frozen)
            .ok_or_else(source_provisioning_temporarily_unavailable)?;
        if staged_bytes > MAX_SOURCE_PROVISIONING_STAGED_BYTES
            || stages.contains_key(&provision_id)
            || stages.len() >= MAX_SOURCE_PROVISIONING_STAGES
            || stages
                .values()
                .filter(|candidate| candidate.actor == actor)
                .count()
                >= MAX_SOURCE_PROVISIONING_STAGES_PER_ACTOR
            || stages
                .values()
                .try_fold(0usize, |total, candidate| {
                    total.checked_add(frozen_snapshot_bytes(&candidate.frozen)?)
                })
                .and_then(|total| total.checked_add(staged_bytes))
                .is_none_or(|total| total > MAX_SOURCE_PROVISIONING_STAGED_BYTES)
        {
            return Err(source_provisioning_temporarily_unavailable());
        }
        stages.insert(provision_id.clone(), stage.clone());
        Ok((
            stage,
            SourceProvisioningConfirmation {
                provision_id,
                confirmation_token,
                expires_at_ms,
            },
        ))
    }

    pub(crate) fn claim(
        &self,
        provision_id: &str,
        confirmation_hash: &str,
        actor: &str,
        transport_session_binding: &str,
        now_ms: i64,
    ) -> McpPlatformResult<SourceProvisioningStage> {
        let mut stages = self
            .stages
            .lock()
            .map_err(|_| source_provisioning_temporarily_unavailable())?;
        let requested_stage_expired = stages
            .get(provision_id)
            .is_some_and(|stage| stage.expires_at_ms <= now_ms);
        purge_expired_stages(&mut stages, now_ms);
        if requested_stage_expired {
            return Err(source_provisioning_expired());
        }
        let stage = stages
            .get_mut(provision_id)
            .ok_or_else(source_provisioning_stale)?;
        if stage.expires_at_ms <= now_ms {
            stages.remove(provision_id);
            return Err(source_provisioning_expired());
        }
        if stage.in_flight
            || stage.confirmation_hash != confirmation_hash
            || stage.actor != actor
            || stage.correlation_id != provision_id
            || stage.transport_session_binding_digest
                != transport_session_binding_digest(transport_session_binding)
        {
            return Err(source_provisioning_stale());
        }
        stage.in_flight = true;
        Ok(stage.clone())
    }

    pub(crate) fn finish(&self, provision_id: &str, confirmation_hash: &str, committed: bool) {
        let Ok(mut stages) = self.stages.lock() else {
            return;
        };
        let should_remove = committed
            && stages
                .get(provision_id)
                .is_some_and(|stage| stage.confirmation_hash == confirmation_hash);
        if should_remove {
            stages.remove(provision_id);
        } else if let Some(stage) = stages.get_mut(provision_id) {
            if stage.confirmation_hash == confirmation_hash {
                stage.in_flight = false;
            }
        }
    }
}

fn purge_expired_stages(stages: &mut HashMap<String, SourceProvisioningStage>, now_ms: i64) {
    stages.retain(|_, candidate| candidate.expires_at_ms > now_ms);
}

fn frozen_snapshot_bytes(frozen: &FrozenSourceProvisioningSnapshot) -> Option<usize> {
    frozen
        .descriptor_bytes
        .len()
        .checked_add(frozen.source_document_bytes.len())
        .and_then(|total| {
            frozen.manifests.iter().try_fold(total, |total, manifest| {
                total
                    .checked_add(manifest.release_id.len())
                    .and_then(|total| total.checked_add(manifest.document_bytes.len()))
            })
        })
}

pub(crate) fn parse_source_provisioning_snapshot(
    frozen: &FrozenSourceProvisioningSnapshot,
) -> McpPlatformResult<ParsedSourceProvisioningSnapshot> {
    parse_source_provisioning_snapshot_inner(frozen).map_err(|_| source_provisioning_invalid())
}

pub(crate) fn parse_source_provisioning_descriptor_unverified(
    bytes: &[u8],
) -> McpPlatformResult<SourceProvisioningDescriptor> {
    parse_source_provisioning_descriptor_unverified_inner(bytes)
        .map_err(|_| source_provisioning_invalid())
}

pub(crate) fn source_provisioning_snapshot_digest(
    frozen: &FrozenSourceProvisioningSnapshot,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SNAPSHOT_DIGEST_DOMAIN);
    hash_bytes(&mut hasher, &frozen.descriptor_bytes);
    hash_bytes(&mut hasher, &frozen.source_document_bytes);
    let mut manifests = frozen.manifests.iter().collect::<Vec<_>>();
    manifests.sort_by(|left, right| left.release_id.cmp(&right.release_id));
    for manifest in manifests {
        hash_bytes(&mut hasher, manifest.release_id.as_bytes());
        hash_bytes(&mut hasher, &manifest.document_bytes);
    }
    crate::utils::bytes_to_hex(hasher.finalize())
}

fn parse_source_provisioning_snapshot_inner(
    frozen: &FrozenSourceProvisioningSnapshot,
) -> Result<ParsedSourceProvisioningSnapshot> {
    if frozen.manifests.len() > MAX_SOURCE_PROVISIONING_MANIFESTS {
        bail!("too many source provisioning manifests");
    }
    let source_document =
        crate::verified_source_catalog::parse_signed_envelope(&frozen.source_document_bytes)?;
    let descriptor =
        parse_source_provisioning_descriptor_unverified_inner(&frozen.descriptor_bytes)?;
    validate_descriptor_against_source_document(&descriptor, &source_document)?;

    let revoked_release_ids = source_document
        .envelope
        .payload
        .snapshot
        .revocations
        .iter()
        .map(|revocation| revocation.release_id.as_str())
        .collect::<BTreeSet<_>>();
    let active_releases = source_document
        .envelope
        .payload
        .snapshot
        .releases
        .iter()
        .filter(|release| !revoked_release_ids.contains(release.release_id.as_str()))
        .collect::<Vec<_>>();
    let mut manifests_by_release = BTreeMap::new();
    for manifest in &frozen.manifests {
        if manifest.release_id.is_empty()
            || manifests_by_release
                .insert(manifest.release_id.as_str(), manifest)
                .is_some()
        {
            bail!("source provisioning manifests are not uniquely bound to releases");
        }
    }
    if manifests_by_release.len() != active_releases.len() {
        bail!("source provisioning manifest set does not match signed active releases");
    }
    for release in &active_releases {
        let manifest = manifests_by_release
            .get(release.release_id.as_str())
            .ok_or_else(|| anyhow!("missing source provisioning manifest"))?;
        let verified = parse_manifest(&manifest.document_bytes)
            .map_err(|_| anyhow!("invalid source provisioning manifest"))?;
        if verified.manifest().id != release.mcp_id
            || verified.manifest().version.as_str() != release.version
        {
            bail!("source provisioning manifest does not match signed release");
        }
    }
    let manifest_count = active_releases
        .len()
        .try_into()
        .map_err(|_| anyhow!("source provisioning manifest count overflow"))?;
    Ok(ParsedSourceProvisioningSnapshot {
        source_document,
        descriptor,
        manifest_count,
        snapshot_digest: source_provisioning_snapshot_digest(frozen),
    })
}

fn parse_source_provisioning_descriptor_unverified_inner(
    bytes: &[u8],
) -> Result<SourceProvisioningDescriptor> {
    let ast = parse_json_bytes(
        bytes,
        ParserLimits {
            max_bytes: MAX_DESCRIPTOR_BYTES,
            max_depth: 8,
            max_total_fields: 64,
            max_array_items: MAX_DESCRIPTOR_SIGNATURES,
            max_string_bytes: MAX_DESCRIPTOR_STRING_BYTES,
        },
    )?;
    let mut envelope = take_object(ast, "source provisioning descriptor")?;
    let payload = take_required(&mut envelope, "payload")?;
    let signatures = take_array(take_required(&mut envelope, "signatures")?, "signatures")?
        .into_iter()
        .map(parse_signature)
        .collect::<Result<Vec<_>>>()?;
    reject_unknown_fields(envelope, "source provisioning descriptor")?;
    if signatures.is_empty() || signatures.len() > MAX_DESCRIPTOR_SIGNATURES {
        bail!("source provisioning descriptor requires a bounded signature set");
    }
    let mut sorted_kids = BTreeSet::new();
    let mut previous_kid = None;
    for signature in &signatures {
        validate_text(&signature.kid, 256, "signature kid")?;
        validate_text(&signature.sig_b64u, 256, "signature")?;
        if previous_kid
            .as_deref()
            .is_some_and(|previous| previous >= signature.kid.as_str())
            || !sorted_kids.insert(signature.kid.as_str())
        {
            bail!("source provisioning descriptor signatures must be sorted and unique");
        }
        previous_kid = Some(signature.kid.clone());
    }

    let mut payload = take_object(payload, "source provisioning descriptor payload")?;
    let format_version = take_string(
        take_required(&mut payload, "format_version")?,
        "format_version",
    )?;
    let refresh = take_required(&mut payload, "refresh")?;
    let root_digest = take_string(take_required(&mut payload, "root_digest")?, "root_digest")?;
    let source_document_canonical_digest = take_string(
        take_required(&mut payload, "source_document_canonical_digest")?,
        "source_document_canonical_digest",
    )?;
    let source_id = take_string(take_required(&mut payload, "source_id")?, "source_id")?;
    reject_unknown_fields(payload, "source provisioning descriptor payload")?;
    if format_version != SOURCE_PROVISIONING_DESCRIPTOR_FORMAT {
        bail!("unsupported source provisioning descriptor format");
    }
    validate_text(&source_id, 256, "source_id")?;
    validate_hex_digest(
        &source_document_canonical_digest,
        "source_document_canonical_digest",
    )?;
    validate_hex_digest(&root_digest, "root_digest")?;

    let mut refresh = take_object(refresh, "source provisioning refresh")?;
    let endpoint = take_string(take_required(&mut refresh, "endpoint")?, "endpoint")?;
    let transport = take_string(take_required(&mut refresh, "transport")?, "transport")?;
    reject_unknown_fields(refresh, "source provisioning refresh")?;
    let transport_kind = match transport.as_str() {
        "verified_source_bundle_v1" => GovernedSourceRefreshTransportKind::VerifiedSourceBundleV1,
        _ => bail!("unsupported source provisioning refresh transport"),
    };
    let (endpoint, endpoint_host) = canonical_https_endpoint(&endpoint)?;
    let canonical_payload_bytes = canonical_descriptor_payload(
        &source_id,
        &source_document_canonical_digest,
        &root_digest,
        transport_kind,
        &endpoint,
    );
    let canonical_envelope = canonical_descriptor_envelope(&canonical_payload_bytes, &signatures);
    let mut hasher = Sha256::new();
    hasher.update(DESCRIPTOR_DIGEST_DOMAIN);
    hash_bytes(&mut hasher, &canonical_envelope);
    let descriptor_digest = crate::utils::bytes_to_hex(hasher.finalize());
    Ok(SourceProvisioningDescriptor {
        source_id,
        source_document_canonical_digest,
        root_digest,
        transport_kind,
        endpoint,
        endpoint_host,
        descriptor_digest,
        signatures,
        canonical_payload_bytes,
    })
}

fn validate_descriptor_against_source_document(
    descriptor: &SourceProvisioningDescriptor,
    source_document: &VerifiedSourceDocument,
) -> Result<()> {
    let anchor = source_document.trust_anchor();
    if descriptor.source_id != source_document.envelope.payload.source_id
        || descriptor.source_document_canonical_digest != source_document.digests.canonical_digest
        || descriptor.root_digest != anchor.root_digest
    {
        bail!("source provisioning descriptor is not bound to the signed source document");
    }
    anchor.verify_signed_payload(&descriptor.signatures, &descriptor.canonical_payload_bytes)?;
    Ok(())
}

fn preview_from_parsed(parsed: &ParsedSourceProvisioningSnapshot) -> SourceProvisioningPreview {
    SourceProvisioningPreview {
        source_id: parsed.descriptor.source_id.clone(),
        display_name: parsed.source_document.envelope.payload.source_name.clone(),
        root_digest: parsed.descriptor.root_digest.clone(),
        document_digest: parsed.source_document.digests.document_digest.clone(),
        endpoint_host: parsed.descriptor.endpoint_host.clone(),
        transport_kind: parsed.descriptor.transport_kind,
        manifest_count: parsed.manifest_count,
        warnings: vec![
            "Trust is established by an explicit user pin; this source is not official or enterprise trusted."
                .to_string(),
        ],
        trust_basis: GovernedSourceTrustBasis::UserPin,
    }
}

fn take_object(value: AstValue, context: &str) -> Result<BTreeMap<String, AstValue>> {
    match value {
        AstValue::Object(object) => Ok(object),
        _ => bail!("{context} must be an object"),
    }
}

fn take_array(value: AstValue, context: &str) -> Result<Vec<AstValue>> {
    match value {
        AstValue::Array(items) => Ok(items),
        _ => bail!("{context} must be an array"),
    }
}

fn take_string(value: AstValue, context: &str) -> Result<String> {
    match value {
        AstValue::String(value) => Ok(value),
        _ => bail!("{context} must be a string"),
    }
}

fn take_required(object: &mut BTreeMap<String, AstValue>, field: &str) -> Result<AstValue> {
    object
        .remove(field)
        .ok_or_else(|| anyhow!("missing required field `{field}`"))
}

fn reject_unknown_fields(object: BTreeMap<String, AstValue>, context: &str) -> Result<()> {
    if object.is_empty() {
        Ok(())
    } else {
        bail!("{context} contains unknown fields")
    }
}

fn parse_signature(value: AstValue) -> Result<SourceSignatureV1> {
    let mut object = take_object(value, "source provisioning signature")?;
    let kid = take_string(take_required(&mut object, "kid")?, "signature kid")?;
    let sig_b64u = take_string(take_required(&mut object, "sig_b64u")?, "signature")?;
    reject_unknown_fields(object, "source provisioning signature")?;
    Ok(SourceSignatureV1 { kid, sig_b64u })
}

fn canonical_descriptor_payload(
    source_id: &str,
    source_document_canonical_digest: &str,
    root_digest: &str,
    transport_kind: GovernedSourceRefreshTransportKind,
    endpoint: &str,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(b'{');
    write_json_string(&mut out, "format_version");
    out.push(b':');
    write_json_string(&mut out, SOURCE_PROVISIONING_DESCRIPTOR_FORMAT);
    out.push(b',');
    write_json_string(&mut out, "refresh");
    out.extend_from_slice(br#":{"#);
    write_json_string(&mut out, "endpoint");
    out.push(b':');
    write_json_string(&mut out, endpoint);
    out.push(b',');
    write_json_string(&mut out, "transport");
    out.push(b':');
    write_json_string(&mut out, transport_kind.as_str());
    out.push(b'}');
    out.push(b',');
    write_json_string(&mut out, "root_digest");
    out.push(b':');
    write_json_string(&mut out, root_digest);
    out.push(b',');
    write_json_string(&mut out, "source_document_canonical_digest");
    out.push(b':');
    write_json_string(&mut out, source_document_canonical_digest);
    out.push(b',');
    write_json_string(&mut out, "source_id");
    out.push(b':');
    write_json_string(&mut out, source_id);
    out.push(b'}');
    out
}

fn canonical_descriptor_envelope(payload: &[u8], signatures: &[SourceSignatureV1]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(br#"{"payload":"#);
    out.extend_from_slice(payload);
    out.extend_from_slice(b",\"signatures\":[");
    for (index, signature) in signatures.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.push(b'{');
        write_json_string(&mut out, "kid");
        out.push(b':');
        write_json_string(&mut out, &signature.kid);
        out.push(b',');
        write_json_string(&mut out, "sig_b64u");
        out.push(b':');
        write_json_string(&mut out, &signature.sig_b64u);
        out.push(b'}');
    }
    out.extend_from_slice(b"]}");
    out
}

fn canonical_https_endpoint(value: &str) -> Result<(String, String)> {
    validate_text(value, 2048, "endpoint")?;
    let url = Url::parse(value)?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("source provisioning endpoint has no host"))?;
    let path = url.path();
    let encoded_path = path.to_ascii_lowercase();
    let local_namespace = [
        ".localhost",
        ".local",
        ".internal",
        ".localdomain",
        ".lan",
        ".home.arpa",
        ".test",
        ".invalid",
        ".example",
        ".onion",
    ];
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
        || value.trim() != value
        || host.is_empty()
        || host.ends_with('.')
        || !host.contains('.')
        || host.eq_ignore_ascii_case("localhost")
        || local_namespace
            .iter()
            .any(|suffix| host == suffix.trim_start_matches('.') || host.ends_with(suffix))
        || host.starts_with("xn--")
        || host.contains(".xn--")
        || path.contains('\\')
        || encoded_path.contains("%2e")
        || !path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~')
        })
    {
        bail!("source provisioning endpoint is not a canonical public HTTPS endpoint");
    }
    if host.parse::<IpAddr>().is_ok() {
        bail!("source provisioning endpoint must use a DNS host");
    }
    let canonical = url.to_string();
    if canonical != value {
        bail!("source provisioning endpoint is not canonical");
    }
    Ok((canonical, host.to_ascii_lowercase()))
}

fn validate_text(value: &str, max_bytes: usize, field: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.chars().any(|character| character.is_control())
    {
        bail!("{field} is invalid");
    }
    Ok(())
}

fn validate_hex_digest(value: &str, field: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("{field} must be a lower-case SHA-256 digest");
    }
    Ok(())
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn sha256_hex(value: &[u8]) -> String {
    crate::utils::bytes_to_hex(Sha256::digest(value))
}

fn transport_session_binding_digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(TRANSPORT_SESSION_BINDING_DIGEST_DOMAIN);
    hash_bytes(&mut hasher, value.as_bytes());
    crate::utils::bytes_to_hex(hasher.finalize())
}

const fn source_provisioning_invalid() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::InvalidRequest,
        "source provisioning descriptor or frozen source snapshot is invalid",
    )
}

const fn source_provisioning_stale() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PlanStale,
        "source provisioning confirmation is stale or already consumed",
    )
}

const fn source_provisioning_expired() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::PlanExpired,
        "source provisioning confirmation expired",
    )
}

const fn source_provisioning_temporarily_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RepositoryUnavailable,
        "source provisioning confirmation state is temporarily unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    const ED25519_SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn source_root_key(seed: u8) -> crate::verified_source_catalog::SourceRootKeyV1 {
        let signing_key = signing_key(seed);
        let mut spki_der = ED25519_SPKI_PREFIX.to_vec();
        spki_der.extend_from_slice(&signing_key.verifying_key().to_bytes());
        crate::verified_source_catalog::SourceRootKeyV1 {
            kid: format!(
                "ed25519-spki-sha256:{}",
                crate::utils::bytes_to_hex(Sha256::digest(&spki_der))
            ),
            spki_der_b64u: URL_SAFE_NO_PAD.encode(spki_der),
        }
    }

    fn signed_source_document(seed: u8) -> Vec<u8> {
        let root_key = source_root_key(seed);
        let payload = crate::verified_source_catalog::SourceUnsignedPayloadV1 {
            schema_version: 1,
            source_id: "test-source".to_string(),
            source_name: "Test source".to_string(),
            issued_at_ms: 100,
            root: crate::verified_source_catalog::SourceTrustRootV1 {
                quorum: 1,
                keys: vec![root_key.clone()],
                previous: None,
            },
            snapshot: crate::verified_source_catalog::SourceSnapshotV1 {
                releases: vec![crate::verified_source_catalog::SourceReleaseV1 {
                    release_id: "release-1".to_string(),
                    mcp_id: "pkg.test.source".to_string(),
                    version: "1.0.0".to_string(),
                    manifest_kind: "manifest".to_string(),
                    platform: "windows".to_string(),
                    architecture: "x86_64".to_string(),
                    variant: "msvc".to_string(),
                }],
                revocations: Vec::new(),
            },
        };
        let canonical_payload = payload.to_canonical_json_bytes();
        let signature = signing_key(seed).sign(&canonical_payload);
        crate::verified_source_catalog::SourceSignedEnvelopeV1 {
            payload,
            signatures: vec![SourceSignatureV1 {
                kid: root_key.kid,
                sig_b64u: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            }],
        }
        .to_canonical_json_bytes()
    }

    fn manifest_bytes() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "id": "pkg.test.source",
            "version": "1.0.0",
            "name": "Test source MCP",
            "description": "Source provisioning test fixture",
            "homepage": "https://example.test/pkg.test.source",
            "publisher": {"id": "com.example", "name": "Example"},
            "license": {"spdx": "MIT"},
            "capabilities": ["tools"],
            "permissions": [],
            "distribution": {"type": "remote_http"},
            "transport": {
                "type": "streamable_http",
                "url": "https://mcp.example.test/pkg.test.source"
            },
            "auth": {"type": "none"},
            "health_check": {"type": "mcp_initialize", "timeout_seconds": 20},
            "owned_files": [],
            "uninstall": {
                "mode": "remove_owned_files_only",
                "preserve_user_data": true
            }
        }))
        .unwrap()
    }

    fn signed_descriptor(
        source_document: &VerifiedSourceDocument,
        seed: u8,
        source_id: &str,
        root_digest: &str,
        endpoint: &str,
    ) -> Vec<u8> {
        let canonical_payload = canonical_descriptor_payload(
            source_id,
            &source_document.digests.canonical_digest,
            root_digest,
            GovernedSourceRefreshTransportKind::VerifiedSourceBundleV1,
            endpoint,
        );
        let signature = signing_key(seed).sign(&canonical_payload);
        let root_key = source_root_key(seed);
        canonical_descriptor_envelope(
            &canonical_payload,
            &[SourceSignatureV1 {
                kid: root_key.kid,
                sig_b64u: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            }],
        )
    }

    fn frozen_snapshot(seed: u8) -> FrozenSourceProvisioningSnapshot {
        let source_document_bytes = signed_source_document(seed);
        let source_document =
            crate::verified_source_catalog::parse_signed_envelope(&source_document_bytes).unwrap();
        FrozenSourceProvisioningSnapshot {
            descriptor_bytes: signed_descriptor(
                &source_document,
                seed,
                "test-source",
                &source_document.trust_anchor().root_digest,
                "https://updates.example.com/test-source.bundle",
            ),
            source_document_bytes,
            manifests: vec![FrozenSourceProvisioningManifest {
                release_id: "release-1".to_string(),
                document_bytes: manifest_bytes(),
            }],
        }
    }

    #[test]
    fn snapshot_requires_root_signed_descriptor_binding() {
        let frozen = frozen_snapshot(41);
        let parsed = parse_source_provisioning_snapshot(&frozen).unwrap();
        assert_eq!(parsed.descriptor.endpoint_host, "updates.example.com");
        assert_eq!(parsed.manifest_count, 1);

        let mut wrong_source = frozen.clone();
        wrong_source.descriptor_bytes = signed_descriptor(
            &parsed.source_document,
            41,
            "other-source",
            &parsed.descriptor.root_digest,
            "https://updates.example.com/test-source.bundle",
        );
        let error = parse_source_provisioning_snapshot(&wrong_source).unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);

        let mut bad_endpoint = frozen;
        bad_endpoint.descriptor_bytes = signed_descriptor(
            &parsed.source_document,
            41,
            "test-source",
            &parsed.descriptor.root_digest,
            "https://127.0.0.1/test-source.bundle",
        );
        let error = parse_source_provisioning_snapshot(&bad_endpoint).unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);

        let mut query_endpoint = frozen_snapshot(41);
        query_endpoint.descriptor_bytes = signed_descriptor(
            &parsed.source_document,
            41,
            "test-source",
            &parsed.descriptor.root_digest,
            "https://updates.example.com/test-source.bundle?redirect=https://127.0.0.1/",
        );
        let error = parse_source_provisioning_snapshot(&query_endpoint).unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);

        let mut local_namespace = frozen_snapshot(41);
        local_namespace.descriptor_bytes = signed_descriptor(
            &parsed.source_document,
            41,
            "test-source",
            &parsed.descriptor.root_digest,
            "https://updates.internal/test-source.bundle",
        );
        let error = parse_source_provisioning_snapshot(&local_namespace).unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::InvalidRequest);
    }

    #[test]
    fn confirmation_is_actor_and_transport_session_bound_expires_and_cannot_be_replayed() {
        let frozen = frozen_snapshot(42);
        let parsed = parse_source_provisioning_snapshot(&frozen).unwrap();
        let coordinator = SourceProvisioningCoordinator::default();
        let token = "source_provisioning_confirmation_one_time_token";
        let transport_session_a = "test-transport-session-a";
        let (stage, confirmation) = coordinator
            .stage(
                "source_provisioning_test".to_string(),
                token.to_string(),
                "actor-a",
                transport_session_a,
                frozen,
                &parsed,
                1_000,
            )
            .unwrap();
        assert_eq!(stage.snapshot_digest, parsed.snapshot_digest);
        assert_ne!(stage.confirmation_hash, token);
        assert_eq!(
            stage.confirmation_hash,
            crate::utils::bytes_to_hex(Sha256::digest(token.as_bytes()))
        );

        let error = coordinator
            .claim(
                &confirmation.provision_id,
                &stage.confirmation_hash,
                "actor-b",
                transport_session_a,
                1_001,
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::PlanStale);

        let error = coordinator
            .claim(
                &confirmation.provision_id,
                &stage.confirmation_hash,
                "actor-a",
                "test-transport-session-b",
                1_001,
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::PlanStale);

        coordinator
            .claim(
                &confirmation.provision_id,
                &stage.confirmation_hash,
                "actor-a",
                transport_session_a,
                1_001,
            )
            .unwrap();
        coordinator.finish(&confirmation.provision_id, &stage.confirmation_hash, true);
        let error = coordinator
            .claim(
                &confirmation.provision_id,
                &stage.confirmation_hash,
                "actor-a",
                transport_session_a,
                1_002,
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::PlanStale);

        let (_, expiring_confirmation) = coordinator
            .stage(
                "source_provisioning_expiring".to_string(),
                "source_provisioning_confirmation_expiring".to_string(),
                "actor-a",
                transport_session_a,
                frozen_snapshot(43),
                &parse_source_provisioning_snapshot(&frozen_snapshot(43)).unwrap(),
                5_000,
            )
            .unwrap();
        let error = coordinator
            .claim(
                &expiring_confirmation.provision_id,
                &crate::utils::bytes_to_hex(Sha256::digest(
                    expiring_confirmation.confirmation_token.as_bytes(),
                )),
                "actor-a",
                transport_session_a,
                expiring_confirmation.expires_at_ms,
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::PlanExpired);
    }

    #[test]
    fn coordinator_bounds_pending_snapshots_and_reclaims_expired_stages() {
        let frozen = frozen_snapshot(44);
        let parsed = parse_source_provisioning_snapshot(&frozen).unwrap();
        let now_ms = 1_000;

        let global_capacity = SourceProvisioningCoordinator::default();
        for index in 0..MAX_SOURCE_PROVISIONING_STAGES {
            global_capacity
                .stage(
                    format!("source_provisioning_global_{index}"),
                    format!("source_provisioning_confirmation_global_{index}"),
                    &format!("actor-{index}"),
                    &format!("transport-session-{index}"),
                    frozen.clone(),
                    &parsed,
                    now_ms,
                )
                .unwrap();
        }
        let error = global_capacity
            .stage(
                "source_provisioning_global_overflow".to_string(),
                "source_provisioning_confirmation_global_overflow".to_string(),
                "actor-overflow",
                "transport-session-overflow",
                frozen.clone(),
                &parsed,
                now_ms,
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);

        let per_actor_capacity = SourceProvisioningCoordinator::default();
        for index in 0..MAX_SOURCE_PROVISIONING_STAGES_PER_ACTOR {
            per_actor_capacity
                .stage(
                    format!("source_provisioning_actor_{index}"),
                    format!("source_provisioning_confirmation_actor_{index}"),
                    "actor-a",
                    "transport-session-a",
                    frozen.clone(),
                    &parsed,
                    now_ms,
                )
                .unwrap();
        }
        let error = per_actor_capacity
            .stage(
                "source_provisioning_actor_overflow".to_string(),
                "source_provisioning_confirmation_actor_overflow".to_string(),
                "actor-a",
                "transport-session-a",
                frozen.clone(),
                &parsed,
                now_ms,
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
        per_actor_capacity
            .stage(
                "source_provisioning_after_expiry".to_string(),
                "source_provisioning_confirmation_after_expiry".to_string(),
                "actor-a",
                "transport-session-a",
                frozen.clone(),
                &parsed,
                now_ms + SOURCE_PROVISIONING_CONFIRMATION_TTL_MS + 1,
            )
            .unwrap();

        let mut oversized = frozen_snapshot(45);
        oversized.descriptor_bytes = vec![0; MAX_SOURCE_PROVISIONING_STAGED_BYTES + 1];
        let error = SourceProvisioningCoordinator::default()
            .stage(
                "source_provisioning_oversized".to_string(),
                "source_provisioning_confirmation_oversized".to_string(),
                "actor-a",
                "transport-session-a",
                oversized,
                &parsed,
                now_ms,
            )
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::RepositoryUnavailable);
    }
}
