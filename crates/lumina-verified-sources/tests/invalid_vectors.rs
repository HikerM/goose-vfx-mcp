mod common;

use lumina_verified_sources::{parse_and_verify_envelope, verify_chain, ErrorCode, State};

use common::{
    active_release, bootstrap, parse_verified, revocation, root, root_key, rotation,
    signed_envelope, snapshot, state, MemoryContext,
};

#[test]
fn parser_and_schema_rejections_cover_major_invalid_inputs() {
    let (root_key_a, signing_key_a) = root_key(21);
    let valid = signed_envelope(
        bootstrap(
            "parser/source",
            root(&[root_key_a.clone()], 1),
            state(&[active_release(
                "release/base",
                "pkg/base",
                "1.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&root_key_a, &signing_key_a)],
    )
    .to_canonical_json_bytes()
    .unwrap();
    let valid_text = String::from_utf8(valid.clone()).unwrap();

    let cases = vec![
        (
            [vec![0xEF, 0xBB, 0xBF], valid.clone()].concat(),
            ErrorCode::Utf8BomNotAllowed,
        ),
        (
            br#"{"payload":{"doc_kind":"bootstrap","doc_kind":"bootstrap","parent_document_digest":null,"root":{"keys":[],"quorum":1},"sequence":0,"source_id":"a","state":{"releases":[]},"wire_version":"v2"},"signatures":[]}"#.to_vec(),
            ErrorCode::DuplicateKey,
        ),
        (
            vec![b'{', 0xFF, b'}'],
            ErrorCode::InvalidUtf8,
        ),
        (
            valid_text
                .replacen("\"sequence\":0", "\"sequence\":01", 1)
                .into_bytes(),
            ErrorCode::InvalidNumber,
        ),
        (
            valid_text
                .replacen("\"sequence\":0", "\"sequence\":1.0", 1)
                .into_bytes(),
            ErrorCode::InvalidNumber,
        ),
        (
            valid_text
                .replacen(
                    "\"wire_version\":\"v2\"",
                    "\"wire_version\":\"v2\",\"extra\":1",
                    1,
                )
                .into_bytes(),
            ErrorCode::UnknownField,
        ),
        (
            valid_text
                .replacen(",\"wire_version\":\"v2\"", "", 1)
                .into_bytes(),
            ErrorCode::MissingField,
        ),
        (
            valid_text
                .replacen("\"source_id\":\"parser/source\"", "\"source_id\":\"源/source\"", 1)
                .into_bytes(),
            ErrorCode::InvalidAscii,
        ),
        (
            valid_text
                .replacen("\"wire_version\":\"v2\"", "\"wire_version\":\"v1\"", 1)
                .into_bytes(),
            ErrorCode::InvalidWireVersion,
        ),
        (
            valid_text
                .replacen("\"doc_kind\":\"bootstrap\"", "\"doc_kind\":\"legacy\"", 1)
                .into_bytes(),
            ErrorCode::InvalidDocKind,
        ),
    ];

    for (raw, expected) in cases {
        assert_eq!(parse_and_verify_envelope(&raw).unwrap_err(), expected);
    }
}

#[test]
fn array_order_ids_spki_signature_and_quorum_fail_closed() {
    let (a, a_signing) = root_key(31);
    let (b, _) = root_key(32);
    let (first, second) = if a.kid < b.kid { (&b, &a) } else { (&a, &b) };
    let signature = signed_envelope(
        bootstrap("ordered/source", root(&[a.clone()], 1), state(&[])),
        &[(&a, &a_signing)],
    )
    .signatures[0]
        .sig_b64u
        .clone();

    let unsorted_root = format!(
        "{{\"payload\":{{\"doc_kind\":\"bootstrap\",\"parent_document_digest\":null,\"root\":{{\"keys\":[{{\"kid\":\"{}\",\"spki_der_b64u\":\"{}\"}},{{\"kid\":\"{}\",\"spki_der_b64u\":\"{}\"}}],\"quorum\":1}},\"sequence\":0,\"source_id\":\"ordered/source\",\"state\":{{\"releases\":[]}},\"wire_version\":\"v2\"}},\"signatures\":[{{\"kid\":\"{}\",\"sig_b64u\":\"{}\"}}]}}",
        first.kid, first.spki_der_b64u, second.kid, second.spki_der_b64u, a.kid, signature
    );
    assert_eq!(
        parse_and_verify_envelope(unsorted_root.as_bytes()).unwrap_err(),
        ErrorCode::UnsortedArray
    );

    let duplicate_signature = format!(
        "{{\"payload\":{{\"doc_kind\":\"bootstrap\",\"parent_document_digest\":null,\"root\":{{\"keys\":[{{\"kid\":\"{}\",\"spki_der_b64u\":\"{}\"}}],\"quorum\":1}},\"sequence\":0,\"source_id\":\"dup/source\",\"state\":{{\"releases\":[]}},\"wire_version\":\"v2\"}},\"signatures\":[{{\"kid\":\"{}\",\"sig_b64u\":\"{}\"}},{{\"kid\":\"{}\",\"sig_b64u\":\"{}\"}}]}}",
        a.kid, a.spki_der_b64u, a.kid, signature, a.kid, signature
    );
    assert_eq!(
        parse_and_verify_envelope(duplicate_signature.as_bytes()).unwrap_err(),
        ErrorCode::DuplicateArrayEntry
    );

    let valid_text = canonical_text(signed_envelope(
        bootstrap(
            "ids/source",
            root(&[a.clone()], 1),
            state(&[active_release(
                "release/base",
                "pkg/base",
                "1.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&a, &a_signing)],
    ));
    let bad_source = valid_text
        .replacen(
            "\"source_id\":\"ids/source\"",
            "\"source_id\":\"bad//source\"",
            1,
        )
        .into_bytes();
    assert_eq!(
        parse_and_verify_envelope(&bad_source).unwrap_err(),
        ErrorCode::InvalidSourceId
    );

    let bad_kid = valid_with_replaced_kid(
        &a,
        &a_signing,
        "ed25519-spki-sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    );
    assert_eq!(
        parse_and_verify_envelope(&bad_kid).unwrap_err(),
        ErrorCode::InvalidKid
    );

    let bad_quorum = valid_text
        .replacen("\"quorum\":1", "\"quorum\":0", 1)
        .into_bytes();
    assert_eq!(
        parse_and_verify_envelope(&bad_quorum).unwrap_err(),
        ErrorCode::InvalidQuorum
    );

    let bad_spki = String::from_utf8(
        signed_envelope(
            bootstrap("spki/source", root(&[a.clone()], 1), state(&[])),
            &[(&a, &a_signing)],
        )
        .to_canonical_json_bytes()
        .unwrap(),
    )
    .unwrap()
    .replacen(
        &a.spki_der_b64u,
        "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!",
        1,
    )
    .into_bytes();
    assert_eq!(
        parse_and_verify_envelope(&bad_spki).unwrap_err(),
        ErrorCode::InvalidBase64
    );

    let bad_signature = String::from_utf8(
        signed_envelope(
            bootstrap("sig/source", root(&[a.clone()], 1), state(&[])),
            &[(&a, &a_signing)],
        )
        .to_canonical_json_bytes()
        .unwrap(),
    )
    .unwrap()
    .replacen("\"sig_b64u\":\"", "\"sig_b64u\":\"not_base64url!", 1)
    .into_bytes();
    assert_eq!(
        parse_and_verify_envelope(&bad_signature).unwrap_err(),
        ErrorCode::InvalidSignature
    );

    let bad_release = valid_text
        .replacen(
            "\"release_id\":\"release/base\"",
            "\"release_id\":\"bad//release\"",
            1,
        )
        .into_bytes();
    assert_eq!(
        parse_and_verify_envelope(&bad_release).unwrap_err(),
        ErrorCode::InvalidReleaseId
    );

    let bad_mcp = valid_text
        .replacen("\"mcp_id\":\"pkg/base\"", "\"mcp_id\":\"Pkg/base\"", 1)
        .into_bytes();
    assert_eq!(
        parse_and_verify_envelope(&bad_mcp).unwrap_err(),
        ErrorCode::InvalidMcpId
    );

    let bad_version = valid_text
        .replacen("\"version\":\"1.0.0\"", "\"version\":\"1.\"", 1)
        .into_bytes();
    assert_eq!(
        parse_and_verify_envelope(&bad_version).unwrap_err(),
        ErrorCode::InvalidVersion
    );
}

#[test]
fn chain_fail_close_cases_cover_parent_gap_fork_and_transition_violations() {
    let (old_a, old_a_signing) = root_key(41);
    let (old_b, old_b_signing) = root_key(42);
    let (new_a, new_a_signing) = root_key(43);

    let bootstrap_verified = parse_verified(signed_envelope(
        bootstrap(
            "chain/source",
            root(&[old_a.clone(), old_b.clone()], 2),
            state(&[active_release(
                "release/a",
                "pkg/a",
                "1.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));
    let mut context = MemoryContext::default();

    let missing_parent = parse_verified(signed_envelope(
        snapshot(
            "chain/source",
            1,
            bootstrap_verified.digests.document,
            root(&[old_a.clone(), old_b.clone()], 2),
            state(&[active_release(
                "release/a",
                "pkg/a",
                "1.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));
    assert_eq!(
        verify_chain(&context, &missing_parent).unwrap_err(),
        ErrorCode::MissingParent
    );

    context.store(&bootstrap_verified);
    let wrong_parent_digest = parse_verified(signed_envelope(
        snapshot(
            "chain/source",
            1,
            bootstrap_verified.digests.signature_set,
            root(&[old_a.clone(), old_b.clone()], 2),
            state(&[active_release(
                "release/a",
                "pkg/a",
                "1.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));
    assert_eq!(
        verify_chain(&context, &wrong_parent_digest).unwrap_err(),
        ErrorCode::ParentDigestMismatch
    );

    let snapshot_verified = parse_verified(signed_envelope(
        snapshot(
            "chain/source",
            1,
            bootstrap_verified.digests.document,
            root(&[old_a.clone(), old_b.clone()], 2),
            state(&[
                active_release(
                    "release/a",
                    "pkg/a",
                    "1.0.0",
                    "manifest",
                    "linux",
                    "x86_64",
                    "gnu",
                ),
                active_release(
                    "release/b",
                    "pkg/b",
                    "1.0.0",
                    "manifest",
                    "linux",
                    "x86_64",
                    "gnu",
                ),
            ]),
        ),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));
    context.store(&snapshot_verified);

    let same_seq_fork = parse_verified(signed_envelope(
        snapshot(
            "chain/source",
            1,
            bootstrap_verified.digests.document,
            root(&[old_a.clone(), old_b.clone()], 2),
            state(&[active_release(
                "release/a",
                "pkg/a",
                "1.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));
    assert_eq!(
        verify_chain(&context, &same_seq_fork).unwrap_err(),
        ErrorCode::SequenceFork
    );

    let bad_rotation = parse_verified(signed_envelope(
        rotation(
            "chain/source",
            2,
            snapshot_verified.digests.document,
            root(&[old_a.clone(), old_b.clone()], 2),
            root(&[old_a.clone(), new_a.clone()], 2),
            state(&[
                active_release(
                    "release/a",
                    "pkg/a",
                    "1.0.0",
                    "manifest",
                    "linux",
                    "x86_64",
                    "gnu",
                ),
                active_release(
                    "release/b",
                    "pkg/b",
                    "1.0.0",
                    "manifest",
                    "linux",
                    "x86_64",
                    "gnu",
                ),
            ]),
        ),
        &[
            (&old_a, &old_a_signing),
            (&old_b, &old_b_signing),
            (&new_a, &new_a_signing),
        ],
    ));
    assert_eq!(
        verify_chain(&context, &bad_rotation).unwrap_err(),
        ErrorCode::TransitionViolation
    );

    let bad_revocation = parse_verified(signed_envelope(
        revocation(
            "chain/source",
            2,
            snapshot_verified.digests.document,
            root(&[old_a.clone(), old_b.clone()], 2),
            state(&[
                active_release(
                    "release/a",
                    "pkg/a",
                    "1.0.0",
                    "manifest",
                    "linux",
                    "x86_64",
                    "gnu",
                ),
                active_release(
                    "release/c",
                    "pkg/c",
                    "1.0.0",
                    "manifest",
                    "linux",
                    "x86_64",
                    "gnu",
                ),
            ]),
        ),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));
    assert_eq!(
        verify_chain(&context, &bad_revocation).unwrap_err(),
        ErrorCode::TransitionViolation
    );
}

#[test]
fn chain_rejects_lookup_fork_bootstrap_conflict_and_rotation_without_old_root_quorum() {
    let (old_a, old_a_signing) = root_key(51);
    let (old_b, old_b_signing) = root_key(52);
    let (new_a, new_a_signing) = root_key(53);

    let bootstrap_verified = parse_verified(signed_envelope(
        bootstrap(
            "fork/source",
            root(&[old_a.clone(), old_b.clone()], 2),
            state(&[active_release(
                "release/base",
                "pkg/base",
                "1.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));

    let conflicting_bootstrap = parse_verified(signed_envelope(
        bootstrap(
            "fork/source",
            root(&[new_a.clone()], 1),
            state(&[active_release(
                "release/other",
                "pkg/other",
                "2.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&new_a, &new_a_signing)],
    ));

    let snapshot_verified = parse_verified(signed_envelope(
        snapshot(
            "fork/source",
            1,
            bootstrap_verified.digests.document,
            root(&[old_a.clone(), old_b.clone()], 2),
            state(&[active_release(
                "release/base",
                "pkg/base",
                "1.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));

    let rotation_verified = parse_verified(signed_envelope(
        rotation(
            "fork/source",
            2,
            snapshot_verified.digests.document,
            root(&[old_a.clone(), old_b.clone()], 2),
            root(&[new_a.clone()], 1),
            state(&[active_release(
                "release/base",
                "pkg/base",
                "1.0.0",
                "manifest",
                "linux",
                "x86_64",
                "gnu",
            )]),
        ),
        &[(&new_a, &new_a_signing)],
    ));

    let mut forked_context = MemoryContext::default();
    forked_context.store(&bootstrap_verified);
    forked_context.store(&conflicting_bootstrap);
    assert_eq!(
        verify_chain(&forked_context, &snapshot_verified).unwrap_err(),
        ErrorCode::LookupFork
    );

    let mut bootstrap_context = MemoryContext::default();
    bootstrap_context.store(&bootstrap_verified);
    assert_eq!(
        verify_chain(&bootstrap_context, &bootstrap_verified).unwrap_err(),
        ErrorCode::BootstrapConflict
    );

    let mut rotation_context = MemoryContext::default();
    rotation_context.store(&bootstrap_verified);
    rotation_context.store(&snapshot_verified);
    assert_eq!(
        verify_chain(&rotation_context, &rotation_verified).unwrap_err(),
        ErrorCode::SignatureQuorumNotMet
    );
}

#[test]
fn canonical_serialization_rejects_invalid_intrinsic_shapes() {
    let (old_a, old_a_signing) = root_key(61);
    let (old_b, _) = root_key(62);

    let bootstrap_doc = bootstrap(
        "serialize/source",
        root(&[old_a.clone()], 1),
        state(&[active_release(
            "release/base",
            "pkg/base",
            "1.0.0",
            "manifest",
            "linux",
            "x86_64",
            "gnu",
        )]),
    );
    let bootstrap_verified = parse_verified(signed_envelope(
        bootstrap_doc.clone(),
        &[(&old_a, &old_a_signing)],
    ));

    let mut missing_prior_root = rotation(
        "serialize/source",
        1,
        bootstrap_verified.digests.document,
        bootstrap_doc.root.clone(),
        root(&[old_a.clone()], 1),
        bootstrap_doc.state.clone(),
    );
    missing_prior_root.prior_root = None;
    assert_eq!(
        missing_prior_root.to_canonical_json_bytes().unwrap_err(),
        ErrorCode::MissingField
    );

    let mut snapshot_with_prior_root = snapshot(
        "serialize/source",
        1,
        bootstrap_verified.digests.document,
        bootstrap_doc.root.clone(),
        bootstrap_doc.state.clone(),
    );
    snapshot_with_prior_root.prior_root = Some(bootstrap_doc.root.clone());
    assert_eq!(
        snapshot_with_prior_root
            .to_canonical_json_bytes()
            .unwrap_err(),
        ErrorCode::UnknownField
    );

    let mut revocation_with_prior_root = revocation(
        "serialize/source",
        1,
        bootstrap_verified.digests.document,
        bootstrap_doc.root.clone(),
        bootstrap_doc.state.clone(),
    );
    revocation_with_prior_root.prior_root = Some(bootstrap_doc.root.clone());
    assert_eq!(
        revocation_with_prior_root
            .to_canonical_json_bytes()
            .unwrap_err(),
        ErrorCode::UnknownField
    );

    let mut invalid_root = root(&[old_a.clone(), old_b.clone()], 2);
    invalid_root.quorum = 3;
    assert_eq!(
        invalid_root.to_canonical_json_bytes().unwrap_err(),
        ErrorCode::InvalidQuorum
    );

    let invalid_state = State {
        releases: vec![active_release(
            "bad//release",
            "pkg/base",
            "1.0.0",
            "manifest",
            "linux",
            "x86_64",
            "gnu",
        )],
    };
    assert_eq!(
        invalid_state.to_canonical_json_bytes().unwrap_err(),
        ErrorCode::InvalidReleaseId
    );

    let mut invalid_envelope = signed_envelope(bootstrap_doc, &[(&old_a, &old_a_signing)]);
    invalid_envelope.payload.source_id = "bad//source".to_string();
    assert_eq!(
        invalid_envelope.to_canonical_json_bytes().unwrap_err(),
        ErrorCode::InvalidSourceId
    );
}

fn valid_with_replaced_kid(
    root_key: &lumina_verified_sources::RootKey,
    signing_key: &ed25519_dalek::SigningKey,
    replacement_kid: &str,
) -> Vec<u8> {
    let bytes = signed_envelope(
        bootstrap("kid/source", root(&[root_key.clone()], 1), state(&[])),
        &[(root_key, signing_key)],
    )
    .to_canonical_json_bytes()
    .unwrap();
    String::from_utf8(bytes)
        .unwrap()
        .replacen(&root_key.kid, replacement_kid, 1)
        .into_bytes()
}

fn canonical_text(envelope: lumina_verified_sources::Envelope) -> String {
    String::from_utf8(envelope.to_canonical_json_bytes().unwrap()).unwrap()
}
