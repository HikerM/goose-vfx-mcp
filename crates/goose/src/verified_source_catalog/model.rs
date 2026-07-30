use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

use super::json::{parse_json_bytes, write_json_string, AstValue, ParserLimits};

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const ROOT_KID_PREFIX: &str = "ed25519-spki-sha256:";

struct RootKeyMaterial {
    verifying_key: VerifyingKey,
    public_key_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSignedEnvelopeV1 {
    pub payload: SourceUnsignedPayloadV1,
    pub signatures: Vec<SourceSignatureV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceUnsignedPayloadV1 {
    pub schema_version: i64,
    pub source_id: String,
    pub source_name: String,
    pub issued_at_ms: i64,
    pub root: SourceTrustRootV1,
    pub snapshot: SourceSnapshotV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTrustRootV1 {
    pub quorum: i64,
    pub keys: Vec<SourceRootKeyV1>,
    pub previous: Option<SourcePreviousTrustRootV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePreviousTrustRootV1 {
    pub quorum: i64,
    pub keys: Vec<SourceRootKeyV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRootKeyV1 {
    pub kid: String,
    pub spki_der_b64u: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshotV1 {
    pub releases: Vec<SourceReleaseV1>,
    pub revocations: Vec<SourceRevocationV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReleaseV1 {
    pub release_id: String,
    pub mcp_id: String,
    pub version: String,
    pub manifest_kind: String,
    pub platform: String,
    pub architecture: String,
    pub variant: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRevocationV1 {
    pub release_id: String,
    pub reason_code: String,
    pub revoked_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSignatureV1 {
    pub kid: String,
    pub sig_b64u: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDigests {
    pub raw_digest: String,
    pub canonical_digest: String,
    pub signed_digest: String,
    pub binding_digest: String,
    pub document_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSourceDocument {
    pub raw_bytes: Vec<u8>,
    pub envelope: SourceSignedEnvelopeV1,
    pub canonical_payload_bytes: Vec<u8>,
    pub canonical_signatures_bytes: Vec<u8>,
    pub canonical_envelope_bytes: Vec<u8>,
    pub digests: SourceDigests,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTrustAnchorV1 {
    pub source_id: String,
    pub root: SourceTrustRootV1,
    pub root_digest: String,
}

pub fn parse_signed_envelope(raw: &[u8]) -> Result<VerifiedSourceDocument> {
    let ast = parse_json_bytes(raw, ParserLimits::default())?;
    let envelope = SourceSignedEnvelopeV1::from_ast(ast)?;
    envelope.verify(raw)
}

pub fn parse_signed_envelope_with_trust_anchor(
    raw: &[u8],
    anchor: &SourceTrustAnchorV1,
) -> Result<VerifiedSourceDocument> {
    let document = parse_signed_envelope(raw)?;
    anchor.verify_document(&document)?;
    Ok(document)
}

impl VerifiedSourceDocument {
    pub fn trust_anchor(&self) -> SourceTrustAnchorV1 {
        SourceTrustAnchorV1 {
            source_id: self.envelope.payload.source_id.clone(),
            root: self.envelope.payload.root.clone(),
            root_digest: self.envelope.payload.root.anchor_digest(),
        }
    }

    pub fn recompute_digests(&self) -> SourceDigests {
        SourceDigests::compute(
            &self.raw_bytes,
            &self.canonical_payload_bytes,
            &self.canonical_signatures_bytes,
            &self.canonical_envelope_bytes,
            &self.envelope.payload.binding_bytes(
                &digest_hex("canonical", &self.canonical_payload_bytes),
                &digest_hex("signed", &self.canonical_signatures_bytes),
            ),
        )
    }
}

impl SourceTrustAnchorV1 {
    pub fn verify_document(&self, document: &VerifiedSourceDocument) -> Result<()> {
        if document.envelope.payload.source_id != self.source_id {
            bail!("Trust anchor source_id does not match candidate document");
        }
        if document.envelope.payload.root != self.root
            || document.envelope.payload.root.anchor_digest() != self.root_digest
        {
            bail!("Trust root rotation is not supported by verified source bundle v1");
        }
        verify_signature_quorum(
            &self.root,
            &document.envelope.signatures,
            &document.canonical_payload_bytes,
        )
    }

    pub fn verify_signed_payload(
        &self,
        signatures: &[SourceSignatureV1],
        canonical_payload_bytes: &[u8],
    ) -> Result<()> {
        verify_signature_quorum(&self.root, signatures, canonical_payload_bytes)
    }
}

impl SourceSignedEnvelopeV1 {
    pub fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "source signed envelope")?;
        let payload = SourceUnsignedPayloadV1::from_ast(take_required(&mut object, "payload")?)?;
        let signatures = take_array(take_required(&mut object, "signatures")?, "signatures")?
            .into_iter()
            .map(SourceSignatureV1::from_ast)
            .collect::<Result<Vec<_>>>()?;
        reject_unknown_fields(object, "source signed envelope")?;
        Ok(Self {
            payload,
            signatures,
        })
    }

    pub fn to_canonical_json_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(b'{');
        write_json_string(&mut out, "payload");
        out.push(b':');
        self.payload.write_canonical_json(&mut out);
        out.push(b',');
        write_json_string(&mut out, "signatures");
        out.push(b':');
        write_signature_array(&mut out, &self.signatures);
        out.push(b'}');
        out
    }

    fn verify(mut self, raw: &[u8]) -> Result<VerifiedSourceDocument> {
        self.payload.validate()?;
        self.validate_signatures()?;

        let canonical_payload_bytes = self.payload.to_canonical_json_bytes();
        verify_signature_quorum(
            &self.payload.root,
            &self.signatures,
            &canonical_payload_bytes,
        )?;

        let canonical_signatures_bytes = signature_array_bytes(&self.signatures);
        let binding_bytes = self.payload.binding_bytes(
            &digest_hex("canonical", &canonical_payload_bytes),
            &digest_hex("signed", &canonical_signatures_bytes),
        );
        let canonical_envelope_bytes = self.to_canonical_json_bytes();
        let digests = SourceDigests::compute(
            raw,
            &canonical_payload_bytes,
            &canonical_signatures_bytes,
            &canonical_envelope_bytes,
            &binding_bytes,
        );
        Ok(VerifiedSourceDocument {
            raw_bytes: raw.to_vec(),
            envelope: self,
            canonical_payload_bytes,
            canonical_signatures_bytes,
            canonical_envelope_bytes,
            digests,
        })
    }

    fn validate_signatures(&mut self) -> Result<()> {
        ensure_sorted_unique(
            self.signatures
                .iter()
                .map(|signature| signature.kid.as_str()),
            "signature kids",
        )?;
        for signature in &self.signatures {
            validate_kid(&signature.kid)?;
            decode_b64u_exact(&signature.sig_b64u, 64, "signature")?;
        }
        if self.signatures.is_empty() {
            bail!("At least one signature is required");
        }
        Ok(())
    }
}

impl SourceUnsignedPayloadV1 {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "source unsigned payload")?;
        let schema_version = take_integer(take_required(&mut object, "schema_version")?)?;
        let source_id = take_string(take_required(&mut object, "source_id")?, "source_id")?;
        let source_name = take_string(take_required(&mut object, "source_name")?, "source_name")?;
        let issued_at_ms = take_integer(take_required(&mut object, "issued_at_ms")?)?;
        let root = SourceTrustRootV1::from_ast(take_required(&mut object, "root")?)?;
        let snapshot = SourceSnapshotV1::from_ast(take_required(&mut object, "snapshot")?)?;
        reject_unknown_fields(object, "source unsigned payload")?;
        Ok(Self {
            schema_version,
            source_id,
            source_name,
            issued_at_ms,
            root,
            snapshot,
        })
    }

    pub fn to_canonical_json_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical_json(&mut out);
        out
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "issued_at_ms");
        out.push(b':');
        out.extend_from_slice(self.issued_at_ms.to_string().as_bytes());
        out.push(b',');
        write_json_string(out, "root");
        out.push(b':');
        self.root.write_canonical_json(out);
        out.push(b',');
        write_json_string(out, "schema_version");
        out.push(b':');
        out.extend_from_slice(self.schema_version.to_string().as_bytes());
        out.push(b',');
        write_json_string(out, "snapshot");
        out.push(b':');
        self.snapshot.write_canonical_json(out);
        out.push(b',');
        write_json_string(out, "source_id");
        out.push(b':');
        write_json_string(out, &self.source_id);
        out.push(b',');
        write_json_string(out, "source_name");
        out.push(b':');
        write_json_string(out, &self.source_name);
        out.push(b'}');
    }

    fn binding_bytes(&self, canonical_digest: &str, signed_digest: &str) -> Vec<u8> {
        let mut current_kids = self
            .root
            .keys
            .iter()
            .map(|key| key.kid.as_str())
            .collect::<Vec<_>>();
        current_kids.sort_unstable();
        let mut previous_kids = self
            .root
            .previous
            .as_ref()
            .map(|previous| {
                let mut kids = previous
                    .keys
                    .iter()
                    .map(|key| key.kid.as_str())
                    .collect::<Vec<_>>();
                kids.sort_unstable();
                kids
            })
            .unwrap_or_default();
        previous_kids.sort_unstable();

        let mut out = Vec::new();
        out.push(b'{');
        write_json_string(&mut out, "canonical_digest");
        out.push(b':');
        write_json_string(&mut out, canonical_digest);
        out.push(b',');
        write_json_string(&mut out, "current_kids");
        out.push(b':');
        write_string_array(&mut out, &current_kids);
        out.push(b',');
        write_json_string(&mut out, "issued_at_ms");
        out.push(b':');
        out.extend_from_slice(self.issued_at_ms.to_string().as_bytes());
        out.push(b',');
        write_json_string(&mut out, "previous_kids");
        out.push(b':');
        write_string_array(&mut out, &previous_kids);
        out.push(b',');
        write_json_string(&mut out, "signed_digest");
        out.push(b':');
        write_json_string(&mut out, signed_digest);
        out.push(b',');
        write_json_string(&mut out, "source_id");
        out.push(b':');
        write_json_string(&mut out, &self.source_id);
        out.push(b'}');
        out
    }

    fn validate(&mut self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("schema_version must be 1");
        }
        validate_non_empty("source_id", &self.source_id)?;
        validate_non_empty("source_name", &self.source_name)?;
        if self.issued_at_ms < 0 {
            bail!("issued_at_ms must be non-negative");
        }
        self.root.validate()?;
        self.snapshot.validate()?;
        let known_release_ids = self
            .snapshot
            .releases
            .iter()
            .map(|release| release.release_id.as_str())
            .collect::<BTreeSet<_>>();
        for revocation in &self.snapshot.revocations {
            if !known_release_ids.contains(revocation.release_id.as_str()) {
                bail!(
                    "Revocation references unknown release_id `{}`",
                    revocation.release_id
                );
            }
        }
        Ok(())
    }
}

impl SourceTrustRootV1 {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "source trust root")?;
        let quorum = take_integer(take_required(&mut object, "quorum")?)?;
        let keys = take_array(take_required(&mut object, "keys")?, "keys")?
            .into_iter()
            .map(SourceRootKeyV1::from_ast)
            .collect::<Result<Vec<_>>>()?;
        let previous = match object.remove("previous") {
            Some(value) => Some(SourcePreviousTrustRootV1::from_ast(value)?),
            None => None,
        };
        reject_unknown_fields(object, "source trust root")?;
        Ok(Self {
            quorum,
            keys,
            previous,
        })
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "keys");
        out.push(b':');
        write_root_key_array(out, &self.keys);
        if let Some(previous) = &self.previous {
            out.push(b',');
            write_json_string(out, "previous");
            out.push(b':');
            previous.write_canonical_json(out);
        }
        out.push(b',');
        write_json_string(out, "quorum");
        out.push(b':');
        out.extend_from_slice(self.quorum.to_string().as_bytes());
        out.push(b'}');
    }

    pub fn anchor_digest(&self) -> String {
        let mut canonical = Vec::new();
        self.write_canonical_json(&mut canonical);
        digest_hex("trust-anchor", &canonical)
    }

    fn validate(&mut self) -> Result<()> {
        if self.quorum <= 0 {
            bail!("Current root quorum must be positive");
        }
        ensure_sorted_unique(
            self.keys.iter().map(|key| key.kid.as_str()),
            "current root kids",
        )?;
        if self.keys.len() < self.quorum as usize {
            bail!("Current root quorum exceeds available keys");
        }
        let current_spki = validate_root_key_set(&self.keys, "current root")?;
        if let Some(previous) = &mut self.previous {
            previous.validate()?;
            for spki in validate_root_key_set(&previous.keys, "previous root")? {
                if current_spki.contains(&spki) {
                    bail!("Root rotation old/new keysets must not overlap by SPKI DER");
                }
            }
        }
        Ok(())
    }
}

impl SourcePreviousTrustRootV1 {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "previous trust root")?;
        let quorum = take_integer(take_required(&mut object, "quorum")?)?;
        let keys = take_array(take_required(&mut object, "keys")?, "keys")?
            .into_iter()
            .map(SourceRootKeyV1::from_ast)
            .collect::<Result<Vec<_>>>()?;
        reject_unknown_fields(object, "previous trust root")?;
        Ok(Self { quorum, keys })
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "keys");
        out.push(b':');
        write_root_key_array(out, &self.keys);
        out.push(b',');
        write_json_string(out, "quorum");
        out.push(b':');
        out.extend_from_slice(self.quorum.to_string().as_bytes());
        out.push(b'}');
    }

    fn validate(&mut self) -> Result<()> {
        if self.quorum <= 0 {
            bail!("Previous root quorum must be positive");
        }
        ensure_sorted_unique(
            self.keys.iter().map(|key| key.kid.as_str()),
            "previous root kids",
        )?;
        if self.keys.len() < self.quorum as usize {
            bail!("Previous root quorum exceeds available keys");
        }
        Ok(())
    }
}

impl SourceRootKeyV1 {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "source root key")?;
        let kid = take_string(take_required(&mut object, "kid")?, "kid")?;
        let spki_der_b64u = take_string(
            take_required(&mut object, "spki_der_b64u")?,
            "spki_der_b64u",
        )?;
        reject_unknown_fields(object, "source root key")?;
        Ok(Self { kid, spki_der_b64u })
    }

    fn validate(&self) -> Result<()> {
        let material = self.key_material()?;
        let expected_kid = expected_root_kid(&self.spki_der_bytes()?);
        if self.kid != expected_kid {
            bail!("root key kid must equal the Ed25519 SPKI SHA-256 fingerprint");
        }
        let _ = material;
        Ok(())
    }

    fn verifying_key(&self) -> Result<VerifyingKey> {
        Ok(self.key_material()?.verifying_key)
    }

    fn key_material(&self) -> Result<RootKeyMaterial> {
        validate_kid(&self.kid)?;
        let bytes = self.spki_der_bytes()?;
        if bytes[..12] != ED25519_SPKI_PREFIX {
            bail!("Ed25519 SPKI DER prefix is invalid");
        }
        let public_key_fingerprint: [u8; 32] = bytes[12..44]
            .try_into()
            .map_err(|_| anyhow!("Ed25519 public key length is invalid"))?;
        let verifying_key = VerifyingKey::from_bytes(&public_key_fingerprint)
            .map_err(|_| anyhow!("Ed25519 public key is invalid"))?;
        Ok(RootKeyMaterial {
            verifying_key,
            public_key_fingerprint,
        })
    }

    fn spki_der_bytes(&self) -> Result<Vec<u8>> {
        decode_b64u_exact(&self.spki_der_b64u, 44, "spki_der_b64u")
    }
}

impl SourceSnapshotV1 {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "source snapshot")?;
        let releases = take_array(take_required(&mut object, "releases")?, "releases")?
            .into_iter()
            .map(SourceReleaseV1::from_ast)
            .collect::<Result<Vec<_>>>()?;
        let revocations = take_array(take_required(&mut object, "revocations")?, "revocations")?
            .into_iter()
            .map(SourceRevocationV1::from_ast)
            .collect::<Result<Vec<_>>>()?;
        reject_unknown_fields(object, "source snapshot")?;
        Ok(Self {
            releases,
            revocations,
        })
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "releases");
        out.push(b':');
        write_release_array(out, &self.releases);
        out.push(b',');
        write_json_string(out, "revocations");
        out.push(b':');
        write_revocation_array(out, &self.revocations);
        out.push(b'}');
    }

    fn validate(&mut self) -> Result<()> {
        self.releases
            .sort_by(|left, right| release_sort_key(left).cmp(&release_sort_key(right)));
        self.revocations.sort_by(|left, right| {
            (&left.release_id, &left.reason_code, left.revoked_at_ms).cmp(&(
                &right.release_id,
                &right.reason_code,
                right.revoked_at_ms,
            ))
        });

        let mut release_keys = BTreeSet::new();
        let mut release_ids = BTreeSet::new();
        for release in &self.releases {
            release.validate()?;
            if !release_ids.insert(release.release_id.clone()) {
                bail!("Duplicate release_id `{}`", release.release_id);
            }
            let key = release_sort_key(release);
            if !release_keys.insert(key) {
                bail!("Duplicate release tuple inside a single source snapshot");
            }
        }

        let mut revoked_ids = BTreeSet::new();
        for revocation in &self.revocations {
            revocation.validate()?;
            if !revoked_ids.insert(revocation.release_id.clone()) {
                bail!(
                    "Duplicate source-scoped revocation for release `{}`",
                    revocation.release_id
                );
            }
        }
        Ok(())
    }
}

impl SourceReleaseV1 {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "source release")?;
        let release_id = take_string(take_required(&mut object, "release_id")?, "release_id")?;
        let mcp_id = take_string(take_required(&mut object, "mcp_id")?, "mcp_id")?;
        let version = take_string(take_required(&mut object, "version")?, "version")?;
        let manifest_kind = take_string(
            take_required(&mut object, "manifest_kind")?,
            "manifest_kind",
        )?;
        let platform = take_string(take_required(&mut object, "platform")?, "platform")?;
        let architecture =
            take_string(take_required(&mut object, "architecture")?, "architecture")?;
        let variant = take_string(take_required(&mut object, "variant")?, "variant")?;
        reject_unknown_fields(object, "source release")?;
        Ok(Self {
            release_id,
            mcp_id,
            version,
            manifest_kind,
            platform,
            architecture,
            variant,
        })
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "architecture");
        out.push(b':');
        write_json_string(out, &self.architecture);
        out.push(b',');
        write_json_string(out, "manifest_kind");
        out.push(b':');
        write_json_string(out, &self.manifest_kind);
        out.push(b',');
        write_json_string(out, "mcp_id");
        out.push(b':');
        write_json_string(out, &self.mcp_id);
        out.push(b',');
        write_json_string(out, "platform");
        out.push(b':');
        write_json_string(out, &self.platform);
        out.push(b',');
        write_json_string(out, "release_id");
        out.push(b':');
        write_json_string(out, &self.release_id);
        out.push(b',');
        write_json_string(out, "variant");
        out.push(b':');
        write_json_string(out, &self.variant);
        out.push(b',');
        write_json_string(out, "version");
        out.push(b':');
        write_json_string(out, &self.version);
        out.push(b'}');
    }

    fn validate(&self) -> Result<()> {
        validate_non_empty("release_id", &self.release_id)?;
        validate_non_empty("mcp_id", &self.mcp_id)?;
        validate_non_empty("version", &self.version)?;
        validate_non_empty("manifest_kind", &self.manifest_kind)?;
        validate_non_empty("platform", &self.platform)?;
        validate_non_empty("architecture", &self.architecture)?;
        validate_non_empty("variant", &self.variant)?;
        Ok(())
    }
}

impl SourceRevocationV1 {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "source revocation")?;
        let release_id = take_string(take_required(&mut object, "release_id")?, "release_id")?;
        let reason_code = take_string(take_required(&mut object, "reason_code")?, "reason_code")?;
        let revoked_at_ms = take_integer(take_required(&mut object, "revoked_at_ms")?)?;
        reject_unknown_fields(object, "source revocation")?;
        Ok(Self {
            release_id,
            reason_code,
            revoked_at_ms,
        })
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "reason_code");
        out.push(b':');
        write_json_string(out, &self.reason_code);
        out.push(b',');
        write_json_string(out, "release_id");
        out.push(b':');
        write_json_string(out, &self.release_id);
        out.push(b',');
        write_json_string(out, "revoked_at_ms");
        out.push(b':');
        out.extend_from_slice(self.revoked_at_ms.to_string().as_bytes());
        out.push(b'}');
    }

    fn validate(&self) -> Result<()> {
        validate_non_empty("release_id", &self.release_id)?;
        validate_non_empty("reason_code", &self.reason_code)?;
        if self.revoked_at_ms < 0 {
            bail!("revoked_at_ms must be non-negative");
        }
        Ok(())
    }
}

impl SourceSignatureV1 {
    fn from_ast(value: AstValue) -> Result<Self> {
        let mut object = take_object(value, "source signature")?;
        let kid = take_string(take_required(&mut object, "kid")?, "kid")?;
        let sig_b64u = take_string(take_required(&mut object, "sig_b64u")?, "sig_b64u")?;
        reject_unknown_fields(object, "source signature")?;
        Ok(Self { kid, sig_b64u })
    }

    fn write_canonical_json(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        write_json_string(out, "kid");
        out.push(b':');
        write_json_string(out, &self.kid);
        out.push(b',');
        write_json_string(out, "sig_b64u");
        out.push(b':');
        write_json_string(out, &self.sig_b64u);
        out.push(b'}');
    }
}

impl SourceDigests {
    fn compute(
        raw_bytes: &[u8],
        canonical_payload_bytes: &[u8],
        canonical_signatures_bytes: &[u8],
        canonical_envelope_bytes: &[u8],
        binding_bytes: &[u8],
    ) -> Self {
        Self {
            raw_digest: digest_hex("raw", raw_bytes),
            canonical_digest: digest_hex("canonical", canonical_payload_bytes),
            signed_digest: digest_hex("signed", canonical_signatures_bytes),
            binding_digest: digest_hex("binding", binding_bytes),
            document_digest: digest_hex("document", canonical_envelope_bytes),
        }
    }
}

fn verify_signature_quorum(
    root: &SourceTrustRootV1,
    signatures: &[SourceSignatureV1],
    canonical_payload_bytes: &[u8],
) -> Result<()> {
    let current_keys = root
        .keys
        .iter()
        .map(|key| Ok((key.kid.as_str(), key.key_material()?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let previous_keys = root
        .previous
        .as_ref()
        .map(|previous| {
            previous
                .keys
                .iter()
                .map(|key| Ok((key.kid.as_str(), key.key_material()?)))
                .collect::<Result<BTreeMap<_, _>>>()
        })
        .transpose()?
        .unwrap_or_default();

    let mut current_valid = BTreeSet::new();
    let mut previous_valid = BTreeSet::new();
    for signature_entry in signatures {
        let bytes = decode_b64u_exact(&signature_entry.sig_b64u, 64, "signature")?;
        let signature = Signature::from_slice(&bytes).map_err(|_| anyhow!("Invalid signature"))?;
        if let Some(key) = current_keys.get(signature_entry.kid.as_str()) {
            key.verifying_key
                .verify_strict(canonical_payload_bytes, &signature)
                .map_err(|_| {
                    anyhow!(
                        "Invalid current-root signature for kid `{}`",
                        signature_entry.kid
                    )
                })?;
            current_valid.insert(key.public_key_fingerprint);
            continue;
        }
        if let Some(key) = previous_keys.get(signature_entry.kid.as_str()) {
            key.verifying_key
                .verify_strict(canonical_payload_bytes, &signature)
                .map_err(|_| {
                    anyhow!(
                        "Invalid previous-root signature for kid `{}`",
                        signature_entry.kid
                    )
                })?;
            previous_valid.insert(key.public_key_fingerprint);
            continue;
        }
        bail!("Unknown kid `{}`", signature_entry.kid);
    }

    if current_valid.len() < root.quorum as usize {
        bail!("Bootstrap/current root quorum is not satisfied");
    }
    if let Some(previous) = &root.previous {
        if previous_valid.len() < previous.quorum as usize {
            bail!("Root rotation requires dual quorum from old and new roots");
        }
    }
    Ok(())
}

fn signature_array_bytes(signatures: &[SourceSignatureV1]) -> Vec<u8> {
    let mut out = Vec::new();
    write_signature_array(&mut out, signatures);
    out
}

fn write_signature_array(out: &mut Vec<u8>, signatures: &[SourceSignatureV1]) {
    out.push(b'[');
    for (index, signature) in signatures.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        signature.write_canonical_json(out);
    }
    out.push(b']');
}

fn write_root_key_array(out: &mut Vec<u8>, keys: &[SourceRootKeyV1]) {
    out.push(b'[');
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.push(b'{');
        write_json_string(out, "kid");
        out.push(b':');
        write_json_string(out, &key.kid);
        out.push(b',');
        write_json_string(out, "spki_der_b64u");
        out.push(b':');
        write_json_string(out, &key.spki_der_b64u);
        out.push(b'}');
    }
    out.push(b']');
}

fn write_release_array(out: &mut Vec<u8>, releases: &[SourceReleaseV1]) {
    out.push(b'[');
    for (index, release) in releases.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        release.write_canonical_json(out);
    }
    out.push(b']');
}

fn write_revocation_array(out: &mut Vec<u8>, revocations: &[SourceRevocationV1]) {
    out.push(b'[');
    for (index, revocation) in revocations.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        revocation.write_canonical_json(out);
    }
    out.push(b']');
}

fn write_string_array(out: &mut Vec<u8>, values: &[&str]) {
    out.push(b'[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        write_json_string(out, value);
    }
    out.push(b']');
}

fn release_sort_key(
    release: &SourceReleaseV1,
) -> (String, String, String, String, String, String, String) {
    (
        release.mcp_id.clone(),
        release.version.clone(),
        release.manifest_kind.clone(),
        release.platform.clone(),
        release.architecture.clone(),
        release.variant.clone(),
        release.release_id.clone(),
    )
}

fn digest_hex(label: &str, payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"verified_source_catalog:");
    hasher.update(label.as_bytes());
    hasher.update([0]);
    hasher.update(payload);
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_b64u_exact(value: &str, expected_len: usize, field: &str) -> Result<Vec<u8>> {
    if value.contains('=') {
        bail!("{field} must use base64url without padding");
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| anyhow!("{field} is not valid base64url"))?;
    if decoded.len() != expected_len {
        bail!("{field} has unexpected decoded length");
    }
    if URL_SAFE_NO_PAD.encode(&decoded) != value {
        bail!("{field} is not canonical base64url");
    }
    Ok(decoded)
}

fn ensure_sorted_unique<'a>(values: impl Iterator<Item = &'a str>, field_name: &str) -> Result<()> {
    let mut previous: Option<&str> = None;
    let mut seen = BTreeSet::new();
    for value in values {
        if let Some(last) = previous {
            if last >= value {
                bail!("{field_name} must be strictly sorted");
            }
        }
        if !seen.insert(value) {
            bail!("{field_name} must be unique");
        }
        previous = Some(value);
    }
    Ok(())
}

fn validate_kid(kid: &str) -> Result<()> {
    if !kid.starts_with(ROOT_KID_PREFIX) {
        bail!("kid must use the ed25519-spki-sha256 prefix");
    }
    let suffix = &kid[ROOT_KID_PREFIX.len()..];
    if suffix.len() != 64 {
        bail!("kid length is invalid");
    }
    if suffix
        .bytes()
        .any(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        bail!("kid must end with a lowercase hexadecimal SHA-256 digest");
    }
    Ok(())
}

fn validate_root_key_set(keys: &[SourceRootKeyV1], field_name: &str) -> Result<BTreeSet<Vec<u8>>> {
    let mut spki = BTreeSet::new();
    for key in keys {
        key.validate()?;
        let bytes = key.spki_der_bytes()?;
        if !spki.insert(bytes) {
            bail!("{field_name} SPKI DER entries must be unique");
        }
    }
    Ok(spki)
}

fn expected_root_kid(spki_der: &[u8]) -> String {
    format!("{ROOT_KID_PREFIX}{}", sha256_hex(spki_der))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    if value.len() > 256 {
        bail!("{field} exceeds size limit");
    }
    if value.chars().any(|ch| ch.is_control()) {
        bail!("{field} must not contain control characters");
    }
    Ok(())
}

fn take_required(object: &mut BTreeMap<String, AstValue>, field: &str) -> Result<AstValue> {
    object
        .remove(field)
        .ok_or_else(|| anyhow!("Missing required field `{field}`"))
}

fn reject_unknown_fields(object: BTreeMap<String, AstValue>, context: &str) -> Result<()> {
    if let Some((field, _)) = object.into_iter().next() {
        bail!("Unknown field `{field}` in {context}");
    }
    Ok(())
}

fn take_object(value: AstValue, context: &str) -> Result<BTreeMap<String, AstValue>> {
    match value {
        AstValue::Object(value) => Ok(value),
        _ => bail!("{context} must be a JSON object"),
    }
}

fn take_array(value: AstValue, context: &str) -> Result<Vec<AstValue>> {
    match value {
        AstValue::Array(value) => Ok(value),
        _ => bail!("{context} must be a JSON array"),
    }
}

fn take_string(value: AstValue, field: &str) -> Result<String> {
    match value {
        AstValue::String(value) => Ok(value),
        _ => bail!("{field} must be a JSON string"),
    }
}

fn take_integer(value: AstValue) -> Result<i64> {
    match value {
        AstValue::Integer(value) => Ok(value),
        _ => bail!("Expected JSON integer"),
    }
}
