use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use lumina::verified_source_catalog::{
    parse_signed_envelope, parse_signed_envelope_with_trust_anchor,
    strict_json::{parse_json_bytes, ParserLimits},
    SourcePreviousTrustRootV1, SourceReleaseV1, SourceRevocationV1, SourceRootKeyV1,
    SourceSignatureV1, SourceSignedEnvelopeV1, SourceSnapshotV1, SourceTrustRootV1,
    SourceUnsignedPayloadV1,
};
use sha2::{Digest, Sha256};

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[test]
fn bootstrap_self_quorum_verifies() {
    let current_a = root_key(11);
    let current_b = root_key(12);
    let payload = basic_payload(
        "source-bootstrap",
        SourceTrustRootV1 {
            quorum: 2,
            keys: vec![current_a.clone(), current_b.clone()],
            previous: None,
        },
        vec![],
    );
    let raw = envelope_json(
        payload,
        &[
            (current_a.kid.as_str(), signing_key(11)),
            (current_b.kid.as_str(), signing_key(12)),
        ],
    );

    let verified = parse_signed_envelope(&raw).unwrap();
    assert_eq!(verified.envelope.payload.source_id, "source-bootstrap");
}

#[test]
fn rotation_requires_dual_quorum() {
    let new_root = root_key(21);
    let old_a = root_key(31);
    let old_b = root_key(32);
    let payload = basic_payload(
        "source-rotation",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![new_root.clone()],
            previous: Some(SourcePreviousTrustRootV1 {
                quorum: 2,
                keys: vec![old_a.clone(), old_b.clone()],
            }),
        },
        vec![],
    );
    let raw = envelope_json(
        payload,
        &[
            (new_root.kid.as_str(), signing_key(21)),
            (old_a.kid.as_str(), signing_key(31)),
            (old_b.kid.as_str(), signing_key(32)),
        ],
    );

    let verified = parse_signed_envelope(&raw).unwrap();
    assert_eq!(verified.envelope.payload.root.previous.unwrap().quorum, 2);
}

#[test]
fn frozen_trust_anchor_accepts_the_original_root_and_rejects_a_self_signed_rotation() {
    let trusted_root = root_key(35);
    let bootstrap = basic_payload(
        "source-frozen-anchor",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![trusted_root.clone()],
            previous: None,
        },
        vec![],
    );
    let bootstrap_raw = envelope_json(bootstrap, &[(trusted_root.kid.as_str(), signing_key(35))]);
    let anchor = parse_signed_envelope(&bootstrap_raw)
        .unwrap()
        .trust_anchor();

    let mut same_root = basic_payload(
        "source-frozen-anchor",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![trusted_root.clone()],
            previous: None,
        },
        vec![],
    );
    same_root.issued_at_ms = 124;
    let same_root_raw = envelope_json(same_root, &[(trusted_root.kid.as_str(), signing_key(35))]);
    assert!(parse_signed_envelope_with_trust_anchor(&same_root_raw, &anchor).is_ok());

    let rotated_root = root_key(36);
    let mut self_signed_rotation = basic_payload(
        "source-frozen-anchor",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![rotated_root.clone()],
            previous: None,
        },
        vec![],
    );
    self_signed_rotation.issued_at_ms = 125;
    let self_signed_rotation_raw = envelope_json(
        self_signed_rotation,
        &[(rotated_root.kid.as_str(), signing_key(36))],
    );

    let error = parse_signed_envelope_with_trust_anchor(&self_signed_rotation_raw, &anchor)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Trust root rotation is not supported"));
}

#[test]
fn duplicate_key_bom_float_null_and_unknown_kid_are_rejected() {
    let bom = concat!(
        "\u{feff}",
        r#"{"payload":{"schema_version":1,"source_id":"x","source_name":"x","issued_at_ms":1,"root":{"quorum":1,"keys":[{"kid":"a","spki_der_b64u":"MCowBQYDK2VwAyEAeHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHg"}],"previous":{"keys":[],"quorum":0}},"snapshot":{"releases":[],"revocations":[]}},"signatures":[{"kid":"a","sig_b64u":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}"#
    );
    assert!(parse_signed_envelope(bom.as_bytes()).is_err());

    let duplicate_key = br#"{"payload":{"schema_version":1,"source_id":"a","source_id":"b","source_name":"x","issued_at_ms":1,"root":{"quorum":1,"keys":[],"previous":{"keys":[],"quorum":0}},"snapshot":{"releases":[],"revocations":[]}},"signatures":[]}"#;
    assert!(parse_signed_envelope(duplicate_key).is_err());

    let float_number = br#"{"payload":{"schema_version":1,"source_id":"a","source_name":"x","issued_at_ms":1.5,"root":{"quorum":1,"keys":[],"previous":{"keys":[],"quorum":0}},"snapshot":{"releases":[],"revocations":[]}},"signatures":[]}"#;
    assert!(parse_signed_envelope(float_number).is_err());

    let null_value = br#"{"payload":{"schema_version":1,"source_id":"a","source_name":"x","issued_at_ms":1,"root":{"quorum":1,"keys":[],"previous":null},"snapshot":{"releases":[],"revocations":[]}},"signatures":[]}"#;
    assert!(parse_signed_envelope(null_value).is_err());

    let payload = basic_payload(
        "source-rogue",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![root_key(41)],
            previous: None,
        },
        vec![],
    );
    let raw = envelope_json(payload, &[("rogue", signing_key(99))]);
    assert!(parse_signed_envelope(&raw).is_err());
}

#[test]
fn bootstrap_rejects_same_spki_with_different_kids() {
    let trusted = root_key(61);
    let alias = SourceRootKeyV1 {
        kid: root_key(62).kid,
        spki_der_b64u: trusted.spki_der_b64u.clone(),
    };
    let payload = basic_payload(
        "source-same-spki-bootstrap",
        SourceTrustRootV1 {
            quorum: 2,
            keys: vec![trusted.clone(), alias.clone()],
            previous: None,
        },
        vec![],
    );
    let raw = envelope_json(
        payload,
        &[
            (trusted.kid.as_str(), signing_key(61)),
            (alias.kid.as_str(), signing_key(61)),
        ],
    );
    let err = parse_signed_envelope(&raw).unwrap_err().to_string();
    assert!(err.contains("root key kid must equal the Ed25519 SPKI SHA-256 fingerprint"));
}

#[test]
fn rotation_rejects_spki_alias_before_overlap_when_kid_mismatches() {
    let current = root_key(71);
    let previous_alias = SourceRootKeyV1 {
        kid: root_key(72).kid,
        spki_der_b64u: current.spki_der_b64u.clone(),
    };
    let payload = basic_payload(
        "source-same-spki-rotation",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![current.clone()],
            previous: Some(SourcePreviousTrustRootV1 {
                quorum: 1,
                keys: vec![previous_alias.clone()],
            }),
        },
        vec![],
    );
    let raw = envelope_json(
        payload,
        &[
            (current.kid.as_str(), signing_key(71)),
            (previous_alias.kid.as_str(), signing_key(71)),
        ],
    );
    let err = parse_signed_envelope(&raw).unwrap_err().to_string();
    assert!(err.contains("root key kid must equal the Ed25519 SPKI SHA-256 fingerprint"));
}

#[test]
fn rotation_rejects_old_new_spki_overlap_with_same_valid_key() {
    let current = root_key(73);
    let payload = basic_payload(
        "source-same-spki-rotation",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![current.clone()],
            previous: Some(SourcePreviousTrustRootV1 {
                quorum: 1,
                keys: vec![current.clone()],
            }),
        },
        vec![],
    );
    let raw = envelope_json(payload, &[(current.kid.as_str(), signing_key(73))]);

    let err = parse_signed_envelope(&raw).unwrap_err().to_string();
    assert!(err.contains("Root rotation old/new keysets must not overlap by SPKI DER"));
}

#[test]
fn previous_none_canonical_round_trip_verifies() {
    let current = root_key(81);
    let payload = basic_payload(
        "source-round-trip",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![current.clone()],
            previous: None,
        },
        vec![],
    );
    let canonical_payload = payload.to_canonical_json_bytes();
    let signature = signing_key(81).sign(&canonical_payload);
    let canonical = SourceSignedEnvelopeV1 {
        payload,
        signatures: vec![SourceSignatureV1 {
            kid: current.kid.clone(),
            sig_b64u: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        }],
    }
    .to_canonical_json_bytes();
    let canonical_text = String::from_utf8(canonical.clone()).unwrap();
    assert!(!canonical_text.contains("\"previous\""));

    let verified = parse_signed_envelope(&canonical).unwrap();
    assert!(verified.envelope.payload.root.previous.is_none());
    assert_eq!(verified.canonical_envelope_bytes, canonical);
}

#[test]
fn offline_recomputation_is_stable() {
    let current = root_key(51);
    let payload = basic_payload(
        "source-digest",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![current.clone()],
            previous: None,
        },
        vec![SourceRevocationV1 {
            release_id: "release-a".to_string(),
            reason_code: "withdrawn".to_string(),
            revoked_at_ms: 500,
        }],
    );
    let raw = envelope_json(payload, &[(current.kid.as_str(), signing_key(51))]);

    let verified = parse_signed_envelope(&raw).unwrap();
    assert_eq!(verified.recompute_digests(), verified.digests);
}

#[test]
fn malformed_spki_and_signature_encodings_are_rejected() {
    let valid = root_key(91);
    let invalid_length = basic_payload(
        "source-invalid-spki-len",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![SourceRootKeyV1 {
                kid: valid.kid.clone(),
                spki_der_b64u: "AA".to_string(),
            }],
            previous: None,
        },
        vec![],
    );
    let invalid_length_raw =
        envelope_json(invalid_length, &[(valid.kid.as_str(), signing_key(91))]);
    assert!(parse_signed_envelope(&invalid_length_raw).is_err());

    let mut bad_prefix_der = vec![0u8; 44];
    bad_prefix_der[12..44].copy_from_slice(&signing_key(92).verifying_key().to_bytes());
    let bad_prefix = basic_payload(
        "source-invalid-spki-prefix",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![SourceRootKeyV1 {
                kid: valid.kid.clone(),
                spki_der_b64u: URL_SAFE_NO_PAD.encode(bad_prefix_der),
            }],
            previous: None,
        },
        vec![],
    );
    let bad_prefix_raw = envelope_json(bad_prefix, &[(valid.kid.as_str(), signing_key(92))]);
    assert!(parse_signed_envelope(&bad_prefix_raw).is_err());

    let current = root_key(93);
    let payload = basic_payload(
        "source-invalid-signature",
        SourceTrustRootV1 {
            quorum: 1,
            keys: vec![current.clone()],
            previous: None,
        },
        vec![],
    );
    let bad_signature = SourceSignedEnvelopeV1 {
        payload,
        signatures: vec![SourceSignatureV1 {
            kid: current.kid.clone(),
            sig_b64u: "not_base64url!".to_string(),
        }],
    }
    .to_canonical_json_bytes();
    assert!(parse_signed_envelope(&bad_signature).is_err());

    let short_signature = SourceSignedEnvelopeV1 {
        payload: basic_payload(
            "source-short-signature",
            SourceTrustRootV1 {
                quorum: 1,
                keys: vec![current],
                previous: None,
            },
            vec![],
        ),
        signatures: vec![SourceSignatureV1 {
            kid: root_key(94).kid,
            sig_b64u: URL_SAFE_NO_PAD.encode([0u8; 32]),
        }],
    }
    .to_canonical_json_bytes();
    assert!(parse_signed_envelope(&short_signature).is_err());
}

#[test]
fn strict_parser_enforces_depth_array_string_and_integer_limits() {
    let too_deep = format!("{}0{}", "[".repeat(18), "]".repeat(18));
    assert!(parse_json_bytes(too_deep.as_bytes(), ParserLimits::default()).is_err());

    let too_many_items = format!(
        "[{}]",
        std::iter::repeat("0")
            .take(ParserLimits::default().max_array_items + 1)
            .collect::<Vec<_>>()
            .join(",")
    );
    assert!(parse_json_bytes(too_many_items.as_bytes(), ParserLimits::default()).is_err());

    let too_long_string = format!(
        "\"{}\"",
        "a".repeat(ParserLimits::default().max_string_bytes + 1)
    );
    assert!(parse_json_bytes(too_long_string.as_bytes(), ParserLimits::default()).is_err());

    assert!(parse_json_bytes(b"9223372036854775808", ParserLimits::default()).is_err());
}

fn basic_payload(
    source_id: &str,
    root: SourceTrustRootV1,
    revocations: Vec<SourceRevocationV1>,
) -> SourceUnsignedPayloadV1 {
    SourceUnsignedPayloadV1 {
        schema_version: 1,
        source_id: source_id.to_string(),
        source_name: format!("{source_id}-name"),
        issued_at_ms: 123,
        root,
        snapshot: SourceSnapshotV1 {
            releases: vec![SourceReleaseV1 {
                release_id: "release-a".to_string(),
                mcp_id: "pkg/example".to_string(),
                version: "1.0.0".to_string(),
                manifest_kind: "manifest".to_string(),
                platform: "linux".to_string(),
                architecture: "x86_64".to_string(),
                variant: "gnu".to_string(),
            }],
            revocations,
        },
    }
}

fn root_key(seed: u8) -> SourceRootKeyV1 {
    let signing = signing_key(seed);
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(&signing.verifying_key().to_bytes());
    SourceRootKeyV1 {
        kid: format!("ed25519-spki-sha256:{}", sha256_hex(&der)),
        spki_der_b64u: URL_SAFE_NO_PAD.encode(der),
    }
}

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn envelope_json(payload: SourceUnsignedPayloadV1, signers: &[(&str, SigningKey)]) -> Vec<u8> {
    let canonical_payload = payload.to_canonical_json_bytes();
    let mut signatures = signers
        .iter()
        .map(|(kid, key)| {
            let signature = key.sign(&canonical_payload);
            SourceSignatureV1 {
                kid: (*kid).to_string(),
                sig_b64u: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
            }
        })
        .collect::<Vec<_>>();
    signatures.sort_by(|left, right| left.kid.cmp(&right.kid));
    SourceSignedEnvelopeV1 {
        payload,
        signatures,
    }
    .to_canonical_json_bytes()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        out.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    out
}
