mod common;

use lumina_verified_sources::{verify_chain, SequenceDisposition};

use common::{
    active_release, assert_accepted, bootstrap, parse_verified, revocation, revoked_release, root,
    root_key, rotation, signed_envelope, snapshot, state, MemoryContext,
};

#[test]
fn canonical_bytes_and_full_chain_golden_vectors_hold() {
    let (old_a, old_a_signing) = root_key(1);
    let (old_b, old_b_signing) = root_key(2);
    let (new_a, new_a_signing) = root_key(3);
    let (new_b, new_b_signing) = root_key(4);

    let bootstrap_state = state(&[active_release(
        "release/core",
        "pkg/core",
        "1.0.0",
        "manifest",
        "linux",
        "x86_64",
        "gnu",
    )]);
    let bootstrap_doc = bootstrap(
        "verified/source",
        root(&[old_a.clone(), old_b.clone()], 2),
        bootstrap_state.clone(),
    );
    let bootstrap_verified = parse_verified(signed_envelope(
        bootstrap_doc.clone(),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));
    let canonical_root_keys = &bootstrap_doc.root.keys;
    let expected_bootstrap_payload = format!(
        "{{\"doc_kind\":\"bootstrap\",\"parent_document_digest\":null,\"root\":{{\"keys\":[{{\"kid\":\"{}\",\"spki_der_b64u\":\"{}\"}},{{\"kid\":\"{}\",\"spki_der_b64u\":\"{}\"}}],\"quorum\":2}},\"sequence\":0,\"source_id\":\"verified/source\",\"state\":{{\"releases\":[{{\"architecture\":\"x86_64\",\"manifest_kind\":\"manifest\",\"mcp_id\":\"pkg/core\",\"platform\":\"linux\",\"release_id\":\"release/core\",\"variant\":\"gnu\",\"version\":\"1.0.0\"}}]}},\"wire_version\":\"v2\"}}",
        canonical_root_keys[0].kid,
        canonical_root_keys[0].spki_der_b64u,
        canonical_root_keys[1].kid,
        canonical_root_keys[1].spki_der_b64u
    );
    assert_eq!(
        String::from_utf8(bootstrap_verified.canonical_payload.clone()).unwrap(),
        expected_bootstrap_payload
    );

    let mut context = MemoryContext::default();
    assert_accepted(
        verify_chain(&context, &bootstrap_verified)
            .unwrap()
            .disposition,
    );
    context.store(&bootstrap_verified);

    let snapshot_state = state(&[
        active_release(
            "release/core",
            "pkg/core",
            "1.0.0",
            "manifest",
            "linux",
            "x86_64",
            "gnu",
        ),
        active_release(
            "release/extra",
            "pkg/extra",
            "2.0.0",
            "manifest",
            "linux",
            "x86_64",
            "gnu",
        ),
    ]);
    let snapshot_verified = parse_verified(signed_envelope(
        snapshot(
            "verified/source",
            1,
            bootstrap_verified.digests.document,
            root(&[old_a.clone(), old_b.clone()], 2),
            snapshot_state.clone(),
        ),
        &[(&old_a, &old_a_signing), (&old_b, &old_b_signing)],
    ));
    assert_accepted(
        verify_chain(&context, &snapshot_verified)
            .unwrap()
            .disposition,
    );
    context.store(&snapshot_verified);

    let rotation_verified = parse_verified(signed_envelope(
        rotation(
            "verified/source",
            2,
            snapshot_verified.digests.document,
            root(&[old_a.clone(), old_b.clone()], 2),
            root(&[new_a.clone(), new_b.clone()], 2),
            snapshot_state.clone(),
        ),
        &[
            (&old_a, &old_a_signing),
            (&old_b, &old_b_signing),
            (&new_a, &new_a_signing),
            (&new_b, &new_b_signing),
        ],
    ));
    assert_accepted(
        verify_chain(&context, &rotation_verified)
            .unwrap()
            .disposition,
    );
    context.store(&rotation_verified);

    let revocation_state = state(&[
        revoked_release(
            "release/core",
            "pkg/core",
            "1.0.0",
            "manifest",
            "linux",
            "x86_64",
            "gnu",
            "security",
            3,
        ),
        active_release(
            "release/extra",
            "pkg/extra",
            "2.0.0",
            "manifest",
            "linux",
            "x86_64",
            "gnu",
        ),
    ]);
    let revocation_verified = parse_verified(signed_envelope(
        revocation(
            "verified/source",
            3,
            rotation_verified.digests.document,
            root(&[new_a.clone(), new_b.clone()], 2),
            revocation_state,
        ),
        &[(&new_a, &new_a_signing), (&new_b, &new_b_signing)],
    ));
    assert_eq!(
        verify_chain(&context, &revocation_verified)
            .unwrap()
            .disposition,
        SequenceDisposition::Accepted
    );
}

#[test]
fn equivalent_signature_variants_do_not_change_document_identity() {
    let (sig_a, sig_a_signing) = root_key(11);
    let (sig_b, sig_b_signing) = root_key(12);

    let bootstrap_verified = parse_verified(signed_envelope(
        bootstrap(
            "variants/source",
            root(&[sig_a.clone(), sig_b.clone()], 1),
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
        &[(&sig_a, &sig_a_signing)],
    ));
    let mut context = MemoryContext::default();
    context.store(&bootstrap_verified);

    let payload = snapshot(
        "variants/source",
        1,
        bootstrap_verified.digests.document,
        root(&[sig_a.clone(), sig_b.clone()], 1),
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
    let variant_a = parse_verified(signed_envelope(
        payload.clone(),
        &[(&sig_a, &sig_a_signing)],
    ));
    assert_accepted(verify_chain(&context, &variant_a).unwrap().disposition);
    context.store(&variant_a);

    let variant_b = parse_verified(signed_envelope(payload, &[(&sig_b, &sig_b_signing)]));
    let outcome = verify_chain(&context, &variant_b).unwrap();
    assert_eq!(outcome.disposition, SequenceDisposition::EquivalentVariant);
    assert_eq!(variant_a.digests.document, variant_b.digests.document);
    assert_ne!(
        variant_a.digests.signature_set,
        variant_b.digests.signature_set
    );

    let duplicate = verify_chain(&context, &variant_a).unwrap();
    assert_eq!(duplicate.disposition, SequenceDisposition::Duplicate);
}
