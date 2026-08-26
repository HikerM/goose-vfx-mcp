use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use super::{
    actual_catalog_hash, apply_catalog, catalog_hash, decode_head_frame, encode_head_frame,
    manifest_hash, params_hash, require_current_catalog, schema_objects, AnchorIdentity,
    AnchorInstanceId, CandidateMembership, CheckpointEventDigest, CheckpointEventKind,
    CheckpointOperation, ContentEvent, ContentRootDigest, FoundationWriteRecordKind,
    FoundationWriteResolution, FrameCorruption, GenesisRecord, Head, HeadCore, HeadLifecycle,
    IntentId, IntentRecord, IntentState, MemberDigest, OperationContentMembership, OperationId,
    PathBindingDigest, PlanBindingDigest, PlanBindingWitness, SuccessorEventDigest, SuccessorHead,
    V2Error, V2Store, V23_PROTOCOL_VERSION, V2_SCHEMA_PARAMS, V2_SCHEMA_VERSION,
};

#[test]
fn head_legal_combinations_validate_and_self_digest_is_strict() {
    let prev = Some(super::HeadDigest::new(seed(200)));
    let prepare = Head::new(inflight_core(CheckpointEventKind::Prepare), prev).unwrap();
    let pending = Head::new(inflight_core(CheckpointEventKind::Pending), prev).unwrap();
    let finalize = Head::new(stable_core(CheckpointEventKind::Finalize), None).unwrap();
    let abort = Head::new(stable_core(CheckpointEventKind::Abort), None).unwrap();

    assert!(prepare.validate().is_ok());
    assert!(pending.validate().is_ok());
    assert!(finalize.validate().is_ok());
    assert!(abort.validate().is_ok());

    let mut corrupted = finalize.clone();
    corrupted.terminal_checkpoint_digest = CheckpointEventDigest::new(seed(201));
    assert!(matches!(
        corrupted.validate(),
        Err(V2Error::CheckpointDigestMismatch)
    ));

    let invalid = HeadCore {
        lifecycle: HeadLifecycle::Stable,
        phase: CheckpointEventKind::Prepare,
        ..stable_core(CheckpointEventKind::Finalize)
    };
    assert!(matches!(
        invalid.validate(),
        Err(V2Error::InvalidHeadCombination)
    ));

    let invalid_generation = HeadCore {
        content_generation: 6,
        ..stable_core(CheckpointEventKind::Finalize)
    };
    assert!(matches!(
        invalid_generation.validate(),
        Err(V2Error::InvalidHeadCombination)
    ));
}

#[test]
fn candidate_membership_is_deterministic_with_monotonic_gaps() {
    let operation_id = OperationId::new(seed(20));
    let first = ContentEvent {
        content_seq: 1,
        payload: b"first".to_vec(),
    };
    let third = ContentEvent {
        content_seq: 3,
        payload: b"third".to_vec(),
    };
    let rows = vec![
        OperationContentMembership {
            operation_id,
            content_seq: 1,
            content_event_digest: first.digest().unwrap(),
        },
        OperationContentMembership {
            operation_id,
            content_seq: 3,
            content_event_digest: third.digest().unwrap(),
        },
    ];

    let membership = CandidateMembership::new(operation_id, rows).unwrap();
    let digest = membership.candidate_membership_digest().unwrap();
    let root_a = membership
        .content_candidate_root(&[first.clone(), third.clone()])
        .unwrap();
    let root_b = membership.content_candidate_root(&[third, first]).unwrap();

    assert_eq!(digest, membership.membership_digest().unwrap());
    assert_eq!(digest, membership.candidate_membership_digest().unwrap());
    assert_eq!(root_a, root_b);
    assert_ne!(
        membership.rows[0].membership_row_digest().unwrap(),
        membership.rows[1].membership_row_digest().unwrap()
    );

    let duplicate = CandidateMembership::new(
        operation_id,
        vec![
            OperationContentMembership {
                operation_id,
                content_seq: 1,
                content_event_digest: ContentEvent {
                    content_seq: 1,
                    payload: b"a".to_vec(),
                }
                .digest()
                .unwrap(),
            },
            OperationContentMembership {
                operation_id,
                content_seq: 1,
                content_event_digest: ContentEvent {
                    content_seq: 1,
                    payload: b"b".to_vec(),
                }
                .digest()
                .unwrap(),
            },
        ],
    );
    assert!(matches!(
        duplicate,
        Err(V2Error::DuplicateContentSeq { content_seq: 1 })
    ));
}

#[test]
fn aborted_operation_cannot_be_reused() {
    let aborted = CheckpointOperation {
        operation_id: OperationId::new(seed(30)),
        operation_generation: 7,
        phase: CheckpointEventKind::Abort,
    };
    let reused = CheckpointOperation {
        operation_id: OperationId::new(seed(30)),
        operation_generation: 8,
        phase: CheckpointEventKind::Prepare,
    };
    assert!(matches!(
        aborted.validate_next_operation(&reused),
        Err(V2Error::AbortedOperationReuse)
    ));
}

#[test]
fn sidecar_codec_truncated_valid_frame_is_incomplete_tail() {
    let head = Head::new(inflight_core(CheckpointEventKind::Pending), None).unwrap();
    let encoded = encode_head_frame(&head).unwrap();
    let decoded = decode_head_frame(&encoded).unwrap();
    assert_eq!(decoded.head, head);

    assert!(matches!(
        decode_head_frame(&encoded[..encoded.len() - 1]),
        Err(V2Error::IncompleteTailFrame {
            expected_len,
            actual_len
        }) if expected_len == encoded.len() as u64 && actual_len == (encoded.len() - 1) as u64
    ));
}

#[test]
fn sidecar_codec_visible_footer_with_forged_length_is_invalid_length() {
    let head = Head::new(inflight_core(CheckpointEventKind::Pending), None).unwrap();
    let encoded = encode_head_frame(&head).unwrap();

    let mut invalid_length = encoded.clone();
    let payload_len = u32::from_be_bytes(invalid_length[10..14].try_into().unwrap());
    invalid_length[10..14].copy_from_slice(&payload_len.saturating_add(1).to_be_bytes());
    assert!(matches!(
        decode_head_frame(&invalid_length),
        Err(V2Error::CorruptFrame {
            reason: FrameCorruption::InvalidLength
        })
    ));
}

#[test]
fn sidecar_codec_distinguishes_corrupt_and_trailing_frames() {
    let head = Head::new(inflight_core(CheckpointEventKind::Pending), None).unwrap();
    let encoded = encode_head_frame(&head).unwrap();

    let mut corrupted = encoded.clone();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0x01;
    assert!(matches!(
        decode_head_frame(&corrupted),
        Err(V2Error::CorruptFrame {
            reason: FrameCorruption::CrcMismatch
        })
    ));

    let mut trailing = encoded.clone();
    trailing.extend_from_slice(&[0x7f, 0x01]);
    assert!(matches!(
        decode_head_frame(&trailing),
        Err(V2Error::UnexpectedCompleteFrame { .. })
    ));
}

#[test]
fn schema_membership_digests_and_global_content_consumption_are_enforced() {
    let conn = Connection::open_in_memory().unwrap();
    apply_catalog(&conn).unwrap();
    let foreign_keys = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);

    let operation_one = OperationId::new(seed(40));
    let first = ContentEvent {
        content_seq: 1,
        payload: b"first".to_vec(),
    };
    let second = ContentEvent {
        content_seq: 2,
        payload: b"second".to_vec(),
    };
    let initial_rows = vec![
        OperationContentMembership {
            operation_id: operation_one,
            content_seq: first.content_seq,
            content_event_digest: first.digest().unwrap(),
        },
        OperationContentMembership {
            operation_id: operation_one,
            content_seq: second.content_seq,
            content_event_digest: second.digest().unwrap(),
        },
    ];
    let initial_candidate = CandidateMembership::new(operation_one, initial_rows.clone()).unwrap();
    let initial_candidate_digest = initial_candidate.candidate_membership_digest().unwrap();
    assert_ne!(
        initial_rows[0].membership_row_digest().unwrap(),
        initial_rows[1].membership_row_digest().unwrap()
    );

    insert_content_event(&conn, &first);
    insert_content_event(&conn, &second);
    insert_prepare_operation(&conn, operation_one, 1, initial_candidate_digest);
    for row in &initial_rows {
        insert_membership_row(&conn, row, initial_candidate_digest).unwrap();
    }

    let aborted = CheckpointOperation {
        operation_id: operation_one,
        operation_generation: 1,
        phase: CheckpointEventKind::Abort,
    };
    let retry_operation = OperationId::new(seed(41));
    let retry = CheckpointOperation {
        operation_id: retry_operation,
        operation_generation: 2,
        phase: CheckpointEventKind::Prepare,
    };
    assert!(aborted.validate_next_operation(&retry).is_ok());

    let reused_row = OperationContentMembership {
        operation_id: retry_operation,
        content_seq: first.content_seq,
        content_event_digest: first.digest().unwrap(),
    };
    let reused_candidate = CandidateMembership::new(retry_operation, vec![reused_row]).unwrap();
    let reused_candidate_digest = reused_candidate.candidate_membership_digest().unwrap();
    insert_prepare_operation(&conn, retry_operation, 2, reused_candidate_digest);
    assert!(insert_membership_row(&conn, &reused_row, reused_candidate_digest).is_err());

    let retried_content = ContentEvent {
        content_seq: 3,
        payload: first.payload.clone(),
    };
    let retried_row = OperationContentMembership {
        operation_id: OperationId::new(seed(42)),
        content_seq: retried_content.content_seq,
        content_event_digest: retried_content.digest().unwrap(),
    };
    let retried_candidate =
        CandidateMembership::new(retried_row.operation_id, vec![retried_row]).unwrap();
    let retried_candidate_digest = retried_candidate.candidate_membership_digest().unwrap();
    insert_content_event(&conn, &retried_content);
    insert_prepare_operation(&conn, retried_row.operation_id, 3, retried_candidate_digest);
    assert!(insert_membership_row(&conn, &retried_row, retried_candidate_digest).is_ok());
}

#[test]
fn schema_terminal_stable_generation_must_match_validator_contract() {
    let conn = Connection::open_in_memory().unwrap();
    apply_catalog(&conn).unwrap();

    let invalid_finalize = HeadCore {
        content_generation: 6,
        ..stable_core(CheckpointEventKind::Finalize)
    };
    assert!(matches!(
        invalid_finalize.validate(),
        Err(V2Error::InvalidHeadCombination)
    ));

    assert!(insert_checkpoint_event(
        &conn,
        HeadLifecycle::Stable,
        CheckpointEventKind::Finalize,
        5,
        6,
        90
    )
    .is_err());
    assert!(insert_anchor_state(
        &conn,
        HeadLifecycle::Stable,
        CheckpointEventKind::Abort,
        5,
        6,
        91
    )
    .is_err());
    assert!(insert_terminal_operation(
        &conn,
        OperationId::new(seed(92)),
        CheckpointEventKind::Abort,
        5,
        6,
        4
    )
    .is_err());
}

#[test]
fn schema_terminal_checkpoint_operations_are_fully_immutable() {
    let conn = Connection::open_in_memory().unwrap();
    apply_catalog(&conn).unwrap();

    let operation_id = OperationId::new(seed(93));
    insert_terminal_operation(&conn, operation_id, CheckpointEventKind::Finalize, 5, 5, 10)
        .unwrap();

    for sql in [
        "UPDATE checkpoint_operations SET stable_generation = stable_generation WHERE operation_id = ?1",
        "UPDATE checkpoint_operations SET content_generation = content_generation WHERE operation_id = ?1",
        "UPDATE checkpoint_operations SET operation_generation = operation_generation WHERE operation_id = ?1",
        "UPDATE checkpoint_operations SET content_root = content_root WHERE operation_id = ?1",
        "UPDATE checkpoint_operations SET content_freeze = content_freeze WHERE operation_id = ?1",
        "UPDATE checkpoint_operations SET phase = phase WHERE operation_id = ?1",
    ] {
        assert_sqlite_error_contains(
            conn.execute(sql, params![operation_id.as_bytes().to_vec()]),
            "checkpoint_operations_terminal_immutable",
        );
    }
}

#[test]
fn schema_inflight_checkpoint_operations_keep_allowed_phase_progression() {
    let conn = Connection::open_in_memory().unwrap();
    apply_catalog(&conn).unwrap();

    let operation_id = OperationId::new(seed(94));
    let candidate_membership_digest = MemberDigest::new(seed(96));
    insert_prepare_operation(&conn, operation_id, 11, candidate_membership_digest);

    assert_eq!(
        conn.execute(
            "UPDATE checkpoint_operations
             SET phase = ?2,
                 content_generation = ?3,
                 content_freeze = ?4,
                 candidate_freeze = ?5
             WHERE operation_id = ?1",
            params![
                operation_id.as_bytes().to_vec(),
                "pending",
                6_i64,
                13_i64,
                15_i64,
            ],
        )
        .unwrap(),
        1
    );
}

#[test]
fn schema_catalog_foundation_is_authoritative_and_legacy_boundary_is_explicit() {
    let names: Vec<_> = schema_objects().iter().map(|object| object.name).collect();
    assert!(names.contains(&"v2_anchor_instance"));
    assert!(names.contains(&"v2_genesis"));
    assert!(names.contains(&"v2_intents"));
    assert!(names.contains(&"content_events"));
    assert!(names.contains(&"checkpoint_operations"));
    assert!(names.contains(&"operation_content_membership"));
    assert!(names.contains(&"checkpoint_events"));
    assert!(names.contains(&"v2_anchor_state"));
    assert_eq!(
        V2_SCHEMA_PARAMS,
        "anchor=v2.3;content=v2;genesis=v2.3;intent=v2.3;legacy-checkpoint=v2.2;sidecar=v2.2"
    );

    assert!(require_current_catalog(Some(V2_SCHEMA_VERSION), Some(V2_SCHEMA_PARAMS)).is_ok());
    assert!(matches!(
        require_current_catalog(
            Some(V2_SCHEMA_VERSION),
            Some("anchor=v2.3;content=v2;genesis=v2.2;intent=v2.2;legacy-checkpoint=v2.2;sidecar=v2.2")
        ),
        Err(V2Error::OfflineMigrationRequired { .. })
    ));
    assert!(matches!(
        require_current_catalog(
            Some("lumina-evidence-db/pre-a0.2"),
            Some("raw_limit=4096;journal=v2;anchor=v1")
        ),
        Err(V2Error::OfflineMigrationRequired { .. })
    ));

    let conn = Connection::open_in_memory().unwrap();
    apply_catalog(&conn).unwrap();
    let foreign_keys = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
    assert_eq!(catalog_hash().unwrap(), actual_catalog_hash(&conn).unwrap());
}

#[test]
fn genesis_is_deterministic_from_same_anchor_instance() {
    let anchor_instance_id = AnchorInstanceId::new(seed(130));
    let first = GenesisRecord::new(anchor_instance_id).unwrap();
    let second = GenesisRecord::new(anchor_instance_id).unwrap();
    let decoded = super::decode_genesis_head(&first.canonical_genesis_bytes).unwrap();

    first.validate().unwrap();
    second.validate().unwrap();
    assert_eq!(decoded.anchor_instance_id, anchor_instance_id);
    assert_eq!(
        first.canonical_genesis_bytes,
        second.canonical_genesis_bytes
    );
    assert_eq!(first.genesis_digest, second.genesis_digest);
}

#[test]
fn different_anchor_instance_ids_produce_distinct_genesis() {
    let first = GenesisRecord::new(AnchorInstanceId::new(seed(131))).unwrap();
    let second = GenesisRecord::new(AnchorInstanceId::new(seed(132))).unwrap();

    assert_ne!(
        first.canonical_genesis_bytes,
        second.canonical_genesis_bytes
    );
    assert_ne!(first.genesis_digest, second.genesis_digest);
}

#[test]
fn successor_blob_hash_and_event_slice_are_validated_from_raw_bytes() {
    let record = IntentRecord::new(&successor_head_fixture()).unwrap();
    record.validate().unwrap();

    let offset = usize::try_from(record.event_value_offset).unwrap();
    let len = usize::try_from(record.event_value_len).unwrap();
    assert_eq!(
        &record.canonical_successor_bytes[offset..offset + len],
        b"event-payload"
    );

    let mut wrong_head_digest = record.clone();
    wrong_head_digest.successor_head_digest = super::HeadDigest::new(seed(180));
    assert!(matches!(
        wrong_head_digest.validate(),
        Err(V2Error::SuccessorHeadDigestMismatch)
    ));

    let mut wrong_event_digest = record.clone();
    wrong_event_digest.successor_event_digest = SuccessorEventDigest::new(seed(181));
    assert!(matches!(
        wrong_event_digest.validate(),
        Err(V2Error::SuccessorEventDigestMismatch)
    ));
}

#[test]
fn digest_fields_are_not_self_referenced_inside_successor_bytes() {
    let record = IntentRecord::new(&successor_head_fixture()).unwrap();
    let encoded = record.canonical_successor_bytes.clone();

    let mut mutated = record.clone();
    mutated.successor_head_digest = super::HeadDigest::new(seed(182));
    mutated.successor_event_digest = SuccessorEventDigest::new(seed(183));

    assert_eq!(encoded, mutated.canonical_successor_bytes);
    assert!(matches!(
        mutated.validate(),
        Err(V2Error::SuccessorHeadDigestMismatch) | Err(V2Error::SuccessorEventDigestMismatch)
    ));
}

#[test]
fn successor_head_encode_rejects_empty_event_payload() {
    let mut head = successor_head_fixture();
    head.event_value.clear();

    assert!(matches!(
        head.validate(),
        Err(V2Error::MissingSuccessorEventValue)
    ));
    assert!(matches!(
        super::encode_successor_head(&head),
        Err(V2Error::MissingSuccessorEventValue)
    ));
}

#[test]
fn successor_head_encode_decode_round_trip_preserves_non_empty_event_payload() {
    let head = successor_head_fixture();

    let encoded = super::encode_successor_head(&head).unwrap();
    let decoded = super::decode_successor_head(&encoded).unwrap();

    assert_eq!(decoded, head);
}

#[test]
fn binding_digest_changes_when_any_witness_field_changes() {
    let baseline = IntentRecord::new(&successor_head_fixture()).unwrap();

    let mut changed_operation_id = successor_head_fixture();
    changed_operation_id.plan_binding_witness.bound_operation_id = OperationId::new(seed(190));
    assert_ne!(
        IntentRecord::new(&changed_operation_id)
            .unwrap()
            .plan_binding_digest,
        baseline.plan_binding_digest
    );

    let mut changed_operation_generation = successor_head_fixture();
    changed_operation_generation
        .plan_binding_witness
        .bound_operation_generation += 1;
    assert_ne!(
        IntentRecord::new(&changed_operation_generation)
            .unwrap()
            .plan_binding_digest,
        baseline.plan_binding_digest
    );

    let mut changed_stable_generation = successor_head_fixture();
    changed_stable_generation
        .plan_binding_witness
        .predecessor_stable_generation += 1;
    assert_ne!(
        IntentRecord::new(&changed_stable_generation)
            .unwrap()
            .plan_binding_digest,
        baseline.plan_binding_digest
    );

    let mut changed_content_generation = successor_head_fixture();
    changed_content_generation
        .plan_binding_witness
        .predecessor_content_generation += 1;
    assert_ne!(
        IntentRecord::new(&changed_content_generation)
            .unwrap()
            .plan_binding_digest,
        baseline.plan_binding_digest
    );

    let mut changed_content_freeze = successor_head_fixture();
    changed_content_freeze
        .plan_binding_witness
        .predecessor_content_freeze += 1;
    changed_content_freeze
        .plan_binding_witness
        .predecessor_candidate_freeze += 1;
    assert_ne!(
        IntentRecord::new(&changed_content_freeze)
            .unwrap()
            .plan_binding_digest,
        baseline.plan_binding_digest
    );

    let mut changed_candidate_freeze = successor_head_fixture();
    changed_candidate_freeze
        .plan_binding_witness
        .predecessor_candidate_freeze += 1;
    assert_ne!(
        IntentRecord::new(&changed_candidate_freeze)
            .unwrap()
            .plan_binding_digest,
        baseline.plan_binding_digest
    );

    let mut changed_stable_root = successor_head_fixture();
    changed_stable_root
        .plan_binding_witness
        .predecessor_stable_root = ContentRootDigest::new(seed(191));
    assert_ne!(
        IntentRecord::new(&changed_stable_root)
            .unwrap()
            .plan_binding_digest,
        baseline.plan_binding_digest
    );

    let mut changed_candidate_root = successor_head_fixture();
    changed_candidate_root
        .plan_binding_witness
        .predecessor_candidate_root = ContentRootDigest::new(seed(192));
    assert_ne!(
        IntentRecord::new(&changed_candidate_root)
            .unwrap()
            .plan_binding_digest,
        baseline.plan_binding_digest
    );

    let mut changed_membership_digest = successor_head_fixture();
    changed_membership_digest
        .plan_binding_witness
        .predecessor_candidate_membership_digest = MemberDigest::new(seed(193));
    assert_ne!(
        IntentRecord::new(&changed_membership_digest)
            .unwrap()
            .plan_binding_digest,
        baseline.plan_binding_digest
    );
}

#[test]
fn intent_id_is_stable_for_same_inputs_and_changes_with_any_input() {
    let record = IntentRecord::new(&successor_head_fixture()).unwrap();

    assert_eq!(
        super::build_intent_id(
            record.anchor_identity,
            record.intent_generation,
            record.prev_head_digest,
            record.successor_head_digest,
        )
        .unwrap(),
        record.intent_id
    );
    assert_ne!(
        super::build_intent_id(
            AnchorIdentity {
                key_epoch: identity().key_epoch + 1,
                ..identity()
            },
            record.intent_generation,
            record.prev_head_digest,
            record.successor_head_digest,
        )
        .unwrap(),
        record.intent_id
    );
    assert_ne!(
        super::build_intent_id(
            record.anchor_identity,
            record.intent_generation + 1,
            record.prev_head_digest,
            record.successor_head_digest,
        )
        .unwrap(),
        record.intent_id
    );
    assert_ne!(
        super::build_intent_id(
            record.anchor_identity,
            record.intent_generation,
            super::HeadDigest::new(seed(194)),
            record.successor_head_digest,
        )
        .unwrap(),
        record.intent_id
    );
    assert_ne!(
        super::build_intent_id(
            record.anchor_identity,
            record.intent_generation,
            record.prev_head_digest,
            super::HeadDigest::new(seed(195)),
        )
        .unwrap(),
        record.intent_id
    );
}

#[test]
fn intent_record_rejects_tampered_intent_id_and_binding_digest() {
    let record = IntentRecord::new(&successor_head_fixture()).unwrap();

    let mut wrong_intent_id = record.clone();
    wrong_intent_id.intent_id = IntentId::new(seed(196));
    assert!(matches!(
        wrong_intent_id.validate(),
        Err(V2Error::IntentIdMismatch)
    ));

    let mut wrong_binding_digest = record.clone();
    wrong_binding_digest.plan_binding_digest = PlanBindingDigest::new(seed(197));
    assert!(matches!(
        wrong_binding_digest.validate(),
        Err(V2Error::PlanBindingDigestMismatch)
    ));
}

#[test]
fn genesis_and_successor_predecessor_misuse_are_rejected() {
    let mut invalid_genesis = GenesisRecord::new(AnchorInstanceId::new(seed(198))).unwrap();
    invalid_genesis.predecessor_head_digest = Some(super::HeadDigest::new(seed(199)));
    assert!(matches!(
        invalid_genesis.validate(),
        Err(V2Error::GenesisCanonicalMismatch)
    ));

    let mut invalid_successor = successor_head_fixture();
    invalid_successor
        .plan_binding_witness
        .bound_operation_generation = 0;
    assert!(matches!(
        invalid_successor.validate(),
        Err(V2Error::InvalidGenerationOrder)
    ));

    let mut invalid_freeze = successor_head_fixture();
    invalid_freeze
        .plan_binding_witness
        .predecessor_candidate_freeze = invalid_freeze
        .plan_binding_witness
        .predecessor_content_freeze
        .saturating_sub(1);
    assert!(matches!(
        invalid_freeze.validate(),
        Err(V2Error::InvalidFreezeOrder)
    ));
}

#[test]
fn v2_store_foundation_round_trip_reads_validated_records() {
    let path = temp_store_path("foundation-round-trip");
    let mut store = V2Store::create_new(&path).unwrap();
    let anchor_instance_id = AnchorInstanceId::new(seed(141));
    let genesis = GenesisRecord::new(anchor_instance_id).unwrap();
    let intent =
        IntentRecord::new(&successor_head_fixture_with_anchor(anchor_instance_id)).unwrap();

    store.save_foundation_genesis_record(&genesis).unwrap();
    store.save_foundation_intent_record(&intent).unwrap();

    assert_eq!(
        store.load_foundation_genesis_record().unwrap(),
        Some(genesis)
    );
    assert_eq!(
        store
            .load_foundation_intent_record(intent.successor_head_digest)
            .unwrap(),
        Some(intent)
    );

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_save_rejects_intent_without_genesis() {
    let path = temp_store_path("foundation-save-orphan-intent");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let intent = IntentRecord::new(&successor_head_fixture()).unwrap();
    let conn = Connection::open(&path).unwrap();
    insert_foundation_anchor_instance_row(&conn, intent.anchor_identity.anchor_instance_id);
    drop(conn);

    let mut store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.save_foundation_intent_record(&intent),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "foundation_genesis_missing"
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_save_rejects_cross_anchor_intent() {
    let path = temp_store_path("foundation-save-cross-anchor-intent");
    let mut store = V2Store::create_new(&path).unwrap();
    let genesis_anchor = AnchorInstanceId::new(seed(160));
    let intent_anchor = AnchorInstanceId::new(seed(161));
    let genesis = GenesisRecord::new(genesis_anchor).unwrap();
    let intent = IntentRecord::new(&successor_head_fixture_with_anchor(intent_anchor)).unwrap();

    store.save_foundation_genesis_record(&genesis).unwrap();
    assert!(matches!(
        store.save_foundation_intent_record(&intent),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "foundation_anchor_instance_mismatch"
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_load_rejects_cross_anchor_intent_row() {
    let path = temp_store_path("foundation-load-cross-anchor-intent");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let store_anchor = AnchorInstanceId::new(seed(162));
    let intent_anchor = AnchorInstanceId::new(seed(163));
    let genesis = GenesisRecord::new(store_anchor).unwrap();
    let intent = IntentRecord::new(&successor_head_fixture_with_anchor(intent_anchor)).unwrap();

    let conn = Connection::open(&path).unwrap();
    insert_foundation_anchor_instance_row(&conn, store_anchor);
    insert_foundation_genesis_row(&conn, &genesis);
    insert_foundation_intent_row(&conn, &intent);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_foundation_intent_record(intent.successor_head_digest),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "foundation_anchor_instance_mismatch"
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_load_rejects_orphan_intent_row() {
    let path = temp_store_path("foundation-load-orphan-intent");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let intent = IntentRecord::new(&successor_head_fixture()).unwrap();
    let conn = Connection::open(&path).unwrap();
    insert_foundation_anchor_instance_row(&conn, intent.anchor_identity.anchor_instance_id);
    insert_foundation_intent_row(&conn, &intent);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_foundation_intent_record(intent.successor_head_digest),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "foundation_genesis_missing"
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_load_rejects_wrong_length_shaped_genesis_digest() {
    let path = temp_store_path("foundation-genesis-digest");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let mut record = GenesisRecord::new(AnchorInstanceId::new(seed(142))).unwrap();
    record.genesis_digest = super::GenesisDigest::new(seed(143));
    insert_foundation_anchor_instance_row(&conn, record.anchor_instance_id);
    insert_foundation_genesis_row(&conn, &record);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_foundation_genesis_record(),
        Err(V2Error::GenesisDigestMismatch)
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_load_rejects_wrong_length_shaped_successor_head_digest() {
    let path = temp_store_path("foundation-head-digest");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let mut record = IntentRecord::new(&successor_head_fixture()).unwrap();
    record.successor_head_digest = super::HeadDigest::new(seed(184));
    insert_foundation_intent_row(&conn, &record);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_foundation_intent_record(record.successor_head_digest),
        Err(V2Error::SuccessorHeadDigestMismatch)
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_load_rejects_wrong_length_shaped_successor_event_digest() {
    let path = temp_store_path("foundation-event-digest");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let mut record = IntentRecord::new(&successor_head_fixture()).unwrap();
    record.successor_event_digest = SuccessorEventDigest::new(seed(185));
    insert_foundation_intent_row(&conn, &record);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_foundation_intent_record(record.successor_head_digest),
        Err(V2Error::SuccessorEventDigestMismatch)
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_load_rejects_non_unique_event_slice_projection() {
    let path = temp_store_path("foundation-event-slice");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let mut record = IntentRecord::new(&successor_head_fixture()).unwrap();
    record.event_value_offset += 1;
    record.event_value_len -= 1;
    insert_foundation_intent_row(&conn, &record);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_foundation_intent_record(record.successor_head_digest),
        Err(V2Error::SuccessorEventSliceMismatch)
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_load_rejects_tampered_intent_projection() {
    let path = temp_store_path("foundation-intent-projection");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let mut record = IntentRecord::new(&successor_head_fixture()).unwrap();
    record.intent_generation += 1;
    insert_foundation_intent_row(&conn, &record);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_foundation_intent_record(record.successor_head_digest),
        Err(V2Error::InvalidCanonicalField {
            context: "IntentRecord",
            field: "canonical_successor_bytes",
        })
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_load_rejects_tampered_binding_digest_and_witness_bytes() {
    let path = temp_store_path("foundation-binding-and-witness");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let mut wrong_binding = IntentRecord::new(&successor_head_fixture()).unwrap();
    wrong_binding.plan_binding_digest = PlanBindingDigest::new(seed(201));
    insert_foundation_intent_row(&conn, &wrong_binding);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_foundation_intent_record(wrong_binding.successor_head_digest),
        Err(V2Error::PlanBindingDigestMismatch)
    ));
    drop(store);
    cleanup_store_path(&path);

    let path = temp_store_path("foundation-witness-bytes");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let baseline = IntentRecord::new(&successor_head_fixture()).unwrap();
    let mut mutated_head = successor_head_fixture();
    mutated_head.plan_binding_witness.predecessor_candidate_root =
        ContentRootDigest::new(seed(202));
    let mutated_record = IntentRecord::new(&mutated_head).unwrap();

    let mut wrong_witness_bytes = baseline.clone();
    wrong_witness_bytes.canonical_successor_bytes = mutated_record.canonical_successor_bytes;
    wrong_witness_bytes.canonical_successor_len = mutated_record.canonical_successor_len;
    wrong_witness_bytes.event_value_offset = mutated_record.event_value_offset;
    wrong_witness_bytes.event_value_len = mutated_record.event_value_len;
    insert_foundation_intent_row(&conn, &wrong_witness_bytes);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_foundation_intent_record(wrong_witness_bytes.successor_head_digest),
        Err(V2Error::SuccessorHeadDigestMismatch)
    ));
    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_save_rejects_invalid_records_before_persisting() {
    let path = temp_store_path("foundation-save-rejects");
    let mut store = V2Store::create_new(&path).unwrap();

    let mut invalid_genesis = GenesisRecord::new(AnchorInstanceId::new(seed(144))).unwrap();
    invalid_genesis.genesis_digest = super::GenesisDigest::new(seed(145));
    assert!(matches!(
        store.save_foundation_genesis_record(&invalid_genesis),
        Err(V2Error::GenesisDigestMismatch)
    ));

    let mut invalid_intent = IntentRecord::new(&successor_head_fixture()).unwrap();
    invalid_intent.successor_event_digest = SuccessorEventDigest::new(seed(186));
    assert!(matches!(
        store.save_foundation_intent_record(&invalid_intent),
        Err(V2Error::SuccessorEventDigestMismatch)
    ));

    drop(store);

    let conn = Connection::open(&path).unwrap();
    let anchor_count = conn
        .query_row("SELECT COUNT(*) FROM v2_anchor_instance", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let genesis_count = conn
        .query_row("SELECT COUNT(*) FROM v2_genesis", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let intent_count = conn
        .query_row("SELECT COUNT(*) FROM v2_intents", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(anchor_count, 0);
    assert_eq!(genesis_count, 0);
    assert_eq!(intent_count, 0);

    drop(conn);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_reads_report_connection_loss_without_touching_path() {
    let path = temp_store_path("foundation-connection-loss-read");
    let mut store = V2Store::create_new(&path).unwrap();

    assert_eq!(store.path(), path.as_path());
    store.force_connection_loss_for_test();
    assert_eq!(store.path(), path.as_path());

    assert!(matches!(
        store.load_foundation_genesis_record(),
        Err(V2Error::StoreUnavailableAfterConnectionLoss)
    ));
    assert!(matches!(
        store.load_foundation_intent_record(super::HeadDigest::new(seed(230))),
        Err(V2Error::StoreUnavailableAfterConnectionLoss)
    ));
    assert!(matches!(
        store.persisted_intent_upper_rowid(),
        Err(V2Error::StoreUnavailableAfterConnectionLoss)
    ));

    cleanup_store_path(&path);
}

#[test]
fn foundation_genesis_receipt_classifier_distinguishes_persisted_missing_and_corrupt() {
    let anchor_instance_id = AnchorInstanceId::new(seed(170));
    let genesis = GenesisRecord::new(anchor_instance_id).unwrap();

    let missing_path = temp_store_path("foundation-genesis-receipt-missing");
    let store = V2Store::create_new(&missing_path).unwrap();
    drop(store);
    assert_eq!(
        super::store::classify_foundation_genesis_receipt_for_test(&missing_path, &genesis),
        FoundationWriteResolution::Missing
    );
    cleanup_store_path(&missing_path);

    let persisted_path = temp_store_path("foundation-genesis-receipt-persisted");
    let mut persisted_store = V2Store::create_new(&persisted_path).unwrap();
    persisted_store
        .save_foundation_genesis_record(&genesis)
        .unwrap();
    drop(persisted_store);
    assert_eq!(
        super::store::classify_foundation_genesis_receipt_for_test(&persisted_path, &genesis),
        FoundationWriteResolution::Persisted
    );
    cleanup_store_path(&persisted_path);

    let corrupt_path = temp_store_path("foundation-genesis-receipt-corrupt");
    let store = V2Store::create_new(&corrupt_path).unwrap();
    drop(store);
    let conn = Connection::open(&corrupt_path).unwrap();
    insert_foundation_anchor_instance_row(&conn, anchor_instance_id);
    drop(conn);
    assert_eq!(
        super::store::classify_foundation_genesis_receipt_for_test(&corrupt_path, &genesis),
        FoundationWriteResolution::Corrupt
    );
    cleanup_store_path(&corrupt_path);
}

#[test]
fn foundation_intent_receipt_classifier_distinguishes_persisted_missing_and_corrupt() {
    let anchor_instance_id = AnchorInstanceId::new(seed(171));
    let genesis = GenesisRecord::new(anchor_instance_id).unwrap();
    let intent = intent_record_fixture(anchor_instance_id, 1);

    let missing_path = temp_store_path("foundation-intent-receipt-missing");
    let mut missing_store = V2Store::create_new(&missing_path).unwrap();
    missing_store
        .save_foundation_genesis_record(&genesis)
        .unwrap();
    drop(missing_store);
    assert_eq!(
        super::store::classify_foundation_intent_receipt_for_test(&missing_path, &intent),
        FoundationWriteResolution::Missing
    );
    cleanup_store_path(&missing_path);

    let persisted_path = temp_store_path("foundation-intent-receipt-persisted");
    let mut persisted_store = V2Store::create_new(&persisted_path).unwrap();
    persisted_store
        .save_foundation_genesis_record(&genesis)
        .unwrap();
    persisted_store
        .save_foundation_intent_record(&intent)
        .unwrap();
    drop(persisted_store);
    assert_eq!(
        super::store::classify_foundation_intent_receipt_for_test(&persisted_path, &intent),
        FoundationWriteResolution::Persisted
    );
    cleanup_store_path(&persisted_path);

    let corrupt_path = temp_store_path("foundation-intent-receipt-corrupt");
    let store = V2Store::create_new(&corrupt_path).unwrap();
    drop(store);
    let conn = Connection::open(&corrupt_path).unwrap();
    insert_foundation_anchor_instance_row(&conn, anchor_instance_id);
    insert_foundation_genesis_row(&conn, &genesis);
    let mut corrupt_record = intent.clone();
    corrupt_record.stable_root = ContentRootDigest::new(seed(172));
    insert_foundation_intent_row(&conn, &corrupt_record);
    drop(conn);
    assert_eq!(
        super::store::classify_foundation_intent_receipt_for_test(&corrupt_path, &intent),
        FoundationWriteResolution::Corrupt
    );
    cleanup_store_path(&corrupt_path);
}

#[test]
fn v2_store_foundation_commit_unknown_freezes_connection_and_classifies_persisted_genesis() {
    let path = temp_store_path("foundation-commit-unknown");
    let mut store = V2Store::create_new(&path).unwrap();
    let genesis = GenesisRecord::new(AnchorInstanceId::new(seed(173))).unwrap();

    store.inject_commit_unknown_for_test();
    assert!(matches!(
        store.save_foundation_genesis_record(&genesis),
        Err(V2Error::FoundationWriteCommitUnknown { kind, resolution })
            if kind == FoundationWriteRecordKind::Genesis
                && resolution == FoundationWriteResolution::Persisted
    ));
    assert_eq!(store.path(), path.as_path());
    assert!(matches!(
        store.load_foundation_genesis_record(),
        Err(V2Error::StoreUnavailableAfterConnectionLoss)
    ));

    let conn = Connection::open(&path).unwrap();
    let genesis_count = conn
        .query_row("SELECT COUNT(*) FROM v2_genesis", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(genesis_count, 1);
    drop(conn);

    cleanup_store_path(&path);
}

#[test]
fn v2_store_foundation_rollback_unknown_freezes_connection_and_classifies_missing_intent() {
    let path = temp_store_path("foundation-rollback-unknown");
    let mut store = V2Store::create_new(&path).unwrap();
    let anchor_instance_id = AnchorInstanceId::new(seed(174));
    let genesis = GenesisRecord::new(anchor_instance_id).unwrap();
    let intent = intent_record_fixture(anchor_instance_id, 2);

    store.save_foundation_genesis_record(&genesis).unwrap();
    store.inject_rollback_unknown_for_test();
    assert!(matches!(
        store.save_foundation_intent_record(&intent),
        Err(V2Error::FoundationWriteRollbackUnknown { kind, resolution })
            if kind == FoundationWriteRecordKind::Intent
                && resolution == FoundationWriteResolution::Missing
    ));
    assert!(matches!(
        store.load_foundation_intent_record(intent.successor_head_digest),
        Err(V2Error::StoreUnavailableAfterConnectionLoss)
    ));

    let conn = Connection::open(&path).unwrap();
    let intent_count = conn
        .query_row("SELECT COUNT(*) FROM v2_intents", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(intent_count, 0);
    drop(conn);

    cleanup_store_path(&path);
}

#[test]
fn v2_store_append_commit_unknown_freezes_connection_without_retry() {
    let path = temp_store_path("append-commit-unknown");
    let mut store = V2Store::create_new(&path).unwrap();

    store.inject_commit_unknown_for_test();
    assert!(matches!(
        store.append_content(b"alpha"),
        Err(V2Error::AppendContentCommitUnknown)
    ));
    assert!(matches!(
        store.persisted_intent_upper_rowid(),
        Err(V2Error::StoreUnavailableAfterConnectionLoss)
    ));

    let conn = Connection::open(&path).unwrap();
    let content_count = conn
        .query_row("SELECT COUNT(*) FROM content_events", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(content_count, 1);
    drop(conn);

    cleanup_store_path(&path);
}

#[test]
fn v2_store_prepare_rollback_unknown_freezes_connection_without_receipt_retry() {
    let path = temp_store_path("prepare-rollback-unknown");
    let mut store = V2Store::create_new(&path).unwrap();

    store.append_content(b"alpha").unwrap();
    store.inject_rollback_unknown_for_test();
    assert!(matches!(
        store.prepare(identity()),
        Err(V2Error::PrepareRollbackUnknown)
    ));
    assert!(matches!(
        store.persisted_intent_upper_rowid(),
        Err(V2Error::StoreUnavailableAfterConnectionLoss)
    ));

    let conn = Connection::open(&path).unwrap();
    let operation_count = conn
        .query_row("SELECT COUNT(*) FROM checkpoint_operations", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(operation_count, 0);
    drop(conn);

    cleanup_store_path(&path);
}

#[test]
fn v2_store_persisted_intent_page_obeys_bounds_and_limits() {
    let path = temp_store_path("persisted-intent-page");
    let mut store = V2Store::create_new(&path).unwrap();
    let anchor_instance_id = AnchorInstanceId::new(seed(175));
    let genesis = GenesisRecord::new(anchor_instance_id).unwrap();
    let first = intent_record_fixture(anchor_instance_id, 1);
    let second = intent_record_fixture(anchor_instance_id, 2);
    let third = intent_record_fixture(anchor_instance_id, 3);

    assert_eq!(store.persisted_intent_upper_rowid().unwrap(), 0);

    store.save_foundation_genesis_record(&genesis).unwrap();
    store.save_foundation_intent_record(&first).unwrap();
    store.save_foundation_intent_record(&second).unwrap();
    store.save_foundation_intent_record(&third).unwrap();

    let upper = store.persisted_intent_upper_rowid().unwrap();
    assert_eq!(upper, 3);
    assert!(store
        .load_persisted_intent_page(0, upper, 0)
        .unwrap()
        .is_empty());
    assert!(matches!(
        store.load_persisted_intent_page(0, upper, 257),
        Err(V2Error::InvalidPersistedIntentLimit { limit }) if limit == 257
    ));

    let bounded = store.load_persisted_intent_page(0, 2, 1).unwrap();
    assert_eq!(bounded.len(), 1);
    assert_eq!(bounded[0].rowid, 1);
    assert_eq!(bounded[0].record, first);

    let page = store.load_persisted_intent_page(1, upper, 10).unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].rowid, 2);
    assert_eq!(page[0].record, second);
    assert_eq!(page[1].rowid, 3);
    assert_eq!(page[1].record, third);

    assert!(store
        .load_persisted_intent_page(upper, upper, 10)
        .unwrap()
        .is_empty());

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_persisted_intent_page_rejects_corrupt_rows() {
    let path = temp_store_path("persisted-intent-page-corrupt");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let anchor_instance_id = AnchorInstanceId::new(seed(176));
    let genesis = GenesisRecord::new(anchor_instance_id).unwrap();
    let mut corrupt_record = intent_record_fixture(anchor_instance_id, 4);
    corrupt_record.intent_generation += 1;

    let conn = Connection::open(&path).unwrap();
    insert_foundation_anchor_instance_row(&conn, anchor_instance_id);
    insert_foundation_genesis_row(&conn, &genesis);
    insert_foundation_intent_row(&conn, &corrupt_record);
    drop(conn);

    let store = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        store.load_persisted_intent_page(0, 1, 10),
        Err(V2Error::InvalidCanonicalField { context, field })
            if context == "IntentRecord" && field == "canonical_successor_bytes"
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn foundation_authority_binding_preserves_v22_sidecar_boundary() {
    assert_eq!(
        V2_SCHEMA_PARAMS,
        "anchor=v2.3;content=v2;genesis=v2.3;intent=v2.3;legacy-checkpoint=v2.2;sidecar=v2.2"
    );
    assert!(matches!(
        require_current_catalog(
            Some(V2_SCHEMA_VERSION),
            Some(
                "anchor=v2.3;content=v2;genesis=v2.3;intent=v2.3;legacy-checkpoint=v2.2;sidecar=v2.1"
            )
        ),
        Err(V2Error::OfflineMigrationRequired { .. })
    ));
}

#[test]
fn v23_schema_rejects_null_wrong_length_and_out_of_bounds_foundation_rows() {
    let conn = Connection::open_in_memory().unwrap();
    apply_catalog(&conn).unwrap();

    assert!(conn
        .execute(
            "INSERT INTO v2_anchor_instance (singleton_id, anchor_instance_id) VALUES (1, NULL)",
            [],
        )
        .is_err());

    conn.execute(
        "INSERT INTO v2_anchor_instance (singleton_id, anchor_instance_id) VALUES (1, ?1)",
        params![seed(140).to_vec()],
    )
    .unwrap();

    let genesis = GenesisRecord::new(AnchorInstanceId::new(seed(140))).unwrap();
    assert!(conn
        .execute(
            "INSERT INTO v2_genesis (
                singleton_id,
                anchor_instance_id,
                protocol_version,
                canonical_genesis_bytes,
                canonical_genesis_len,
                genesis_digest,
                stable_generation,
                content_generation,
                work_generation,
                stable_root,
                content_root,
                candidate_root,
                operation_id,
                predecessor_head_digest,
                predecessor_candidate_freeze,
                plan_binding_digest
            ) VALUES (1, ?1, ?2, ?3, ?4, ?5, 0, 0, 0, ?6, ?7, NULL, NULL, NULL, NULL, NULL)",
            params![
                genesis.anchor_instance_id.as_bytes().to_vec(),
                genesis.protocol_version,
                genesis.canonical_genesis_bytes,
                as_i64(genesis.canonical_genesis_len),
                vec![1_u8; 31],
                genesis.stable_root.as_bytes().to_vec(),
                genesis.content_root.as_bytes().to_vec(),
            ],
        )
        .is_err());

    let record = IntentRecord::new(&successor_head_fixture()).unwrap();
    assert!(conn
        .execute(
            "INSERT INTO v2_intents (
                successor_head_digest,
                protocol_version,
                canonical_successor_bytes,
                canonical_successor_len,
                successor_event_digest,
                event_value_offset,
                event_value_len,
                prev_head_digest,
                intent_generation,
                frame_sequence,
                predecessor_head_digest,
                predecessor_candidate_freeze,
                plan_binding_digest,
                intent_state,
                stable_root,
                content_root
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?12, ?13, ?14, ?15)",
            params![
                record.successor_head_digest.as_bytes().to_vec(),
                V23_PROTOCOL_VERSION,
                record.canonical_successor_bytes.clone(),
                as_i64(record.canonical_successor_len),
                record.successor_event_digest.as_bytes().to_vec(),
                as_i64(record.event_value_offset),
                as_i64(record.event_value_len),
                record.prev_head_digest.as_bytes().to_vec(),
                as_i64(record.intent_generation),
                as_i64(record.frame_sequence),
                as_i64(record.plan_binding_witness.predecessor_candidate_freeze),
                record.plan_binding_digest.as_bytes().to_vec(),
                record.state.as_sql(),
                record.stable_root.as_bytes().to_vec(),
                record.content_root.as_bytes().to_vec(),
            ],
        )
        .is_err());

    assert!(conn
        .execute(
            "INSERT INTO v2_intents (
                successor_head_digest,
                protocol_version,
                canonical_successor_bytes,
                canonical_successor_len,
                successor_event_digest,
                event_value_offset,
                event_value_len,
                prev_head_digest,
                intent_generation,
                frame_sequence,
                predecessor_head_digest,
                predecessor_candidate_freeze,
                plan_binding_digest,
                intent_state,
                stable_root,
                content_root
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                record.successor_head_digest.as_bytes().to_vec(),
                record.protocol_version,
                record.canonical_successor_bytes.clone(),
                as_i64(record.canonical_successor_len),
                record.successor_event_digest.as_bytes().to_vec(),
                as_i64(record.canonical_successor_len),
                99_i64,
                record.prev_head_digest.as_bytes().to_vec(),
                as_i64(record.intent_generation),
                as_i64(record.frame_sequence),
                record.predecessor_head_digest.as_bytes().to_vec(),
                as_i64(record.plan_binding_witness.predecessor_candidate_freeze),
                record.plan_binding_digest.as_bytes().to_vec(),
                record.state.as_sql(),
                record.stable_root.as_bytes().to_vec(),
                record.content_root.as_bytes().to_vec(),
            ],
        )
        .is_err());
}

#[test]
fn v23_schema_enforces_non_null_intent_predecessor() {
    let conn = Connection::open_in_memory().unwrap();
    apply_catalog(&conn).unwrap();

    let record = IntentRecord::new(&successor_head_fixture()).unwrap();
    assert!(conn
        .execute(
            "INSERT INTO v2_intents (
                successor_head_digest,
                protocol_version,
                canonical_successor_bytes,
                canonical_successor_len,
                successor_event_digest,
                event_value_offset,
                event_value_len,
                prev_head_digest,
                intent_generation,
                frame_sequence,
                predecessor_head_digest,
                predecessor_candidate_freeze,
                plan_binding_digest,
                intent_state,
                stable_root,
                content_root
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?12, ?13, ?14, ?15)",
            params![
                record.successor_head_digest.as_bytes().to_vec(),
                V23_PROTOCOL_VERSION,
                record.canonical_successor_bytes,
                as_i64(record.canonical_successor_len),
                record.successor_event_digest.as_bytes().to_vec(),
                as_i64(record.event_value_offset),
                as_i64(record.event_value_len),
                record.prev_head_digest.as_bytes().to_vec(),
                as_i64(record.intent_generation),
                as_i64(record.frame_sequence),
                as_i64(record.plan_binding_witness.predecessor_candidate_freeze),
                record.plan_binding_digest.as_bytes().to_vec(),
                record.state.as_sql(),
                record.stable_root.as_bytes().to_vec(),
                record.content_root.as_bytes().to_vec(),
            ],
        )
        .is_err());
}

#[test]
fn v2_store_schema_meta_hashes_match_public_api() {
    let path = temp_store_path("schema-meta");
    let store = V2Store::create_new(&path).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let (manifest, params, catalog) = conn
        .query_row(
            "SELECT manifest_hash, params_hash, catalog_hash
             FROM schema_meta
             WHERE singleton_id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(manifest, manifest_hash().unwrap().to_vec());
    assert_eq!(params, params_hash().unwrap().to_vec());
    assert_eq!(catalog, catalog_hash().unwrap().to_vec());

    cleanup_store_path(&path);
}

#[test]
fn v2_store_rejects_non_regular_db_leaf() {
    let path = temp_store_path("non-regular-leaf");
    fs::create_dir_all(&path).unwrap();

    assert!(matches!(
        V2Store::create_new(&path),
        Err(V2Error::InvalidPathBinding)
    ));
    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::InvalidPathBinding)
    ));

    cleanup_store_path(&path);
}

#[test]
fn v2_store_create_open_and_prepare_recovery_boundary_are_explicit() {
    let path = temp_store_path("create-open");
    let mut store = V2Store::create_new(&path).unwrap();
    let reopened = V2Store::open_existing(&path).unwrap();
    assert_eq!(reopened.path(), path.as_path());
    drop(reopened);

    let appended = store.append_content(b"alpha").unwrap();
    assert_eq!(appended.event.content_seq, 1);

    let prepared = store.prepare(identity()).unwrap();
    assert_eq!(prepared.stable_generation, 0);
    assert_eq!(prepared.visible_frontier, 1);
    assert_eq!(prepared.membership.rows.len(), 1);

    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::RecoveryRequired { phase }) if phase == "prepare"
    ));

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_append_content_uses_persistent_monotonic_allocator() {
    let path = temp_store_path("allocator");
    let mut store = V2Store::create_new(&path).unwrap();

    let first = store.append_content(b"first").unwrap();
    let second = store.append_content(b"second").unwrap();
    assert_eq!(first.event.content_seq, 1);
    assert_eq!(second.event.content_seq, 2);
    assert_eq!(second.allocator_watermark, 2);
    drop(store);

    let reopened = V2Store::open_existing(&path).unwrap();
    assert_eq!(reopened.path(), path.as_path());
    drop(reopened);

    let conn = Connection::open(&path).unwrap();
    let next_content_seq = conn
        .query_row(
            "SELECT next_content_seq FROM v2_store_state WHERE singleton_id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(next_content_seq, 3);

    cleanup_store_path(&path);
}

#[test]
fn v2_store_prepare_snapshot_rejects_empty_membership_and_keeps_ordered_rows() {
    let empty_path = temp_store_path("prepare-empty");
    let mut empty_store = V2Store::create_new(&empty_path).unwrap();
    assert!(matches!(
        empty_store.prepare(identity()),
        Err(V2Error::NoEligibleContent {
            stable_generation: 0,
            visible_frontier: 0,
        })
    ));
    drop(empty_store);
    cleanup_store_path(&empty_path);

    let path = temp_store_path("prepare-snapshot");
    let mut store = V2Store::create_new(&path).unwrap();
    store.append_content(b"first").unwrap();
    store.append_content(b"second").unwrap();
    let prepared = store.prepare(identity()).unwrap();

    assert_eq!(prepared.membership.rows.len(), 2);
    assert_eq!(prepared.membership.rows[0].content_seq, 1);
    assert_eq!(prepared.membership.rows[1].content_seq, 2);
    assert_eq!(prepared.visible_frontier, 2);
    assert_eq!(prepared.allocator_watermark, 2);
    assert_eq!(
        prepared
            .membership
            .content_candidate_root(&prepared.content_events)
            .unwrap(),
        prepared.head.core.candidate_root.unwrap()
    );

    drop(store);
    cleanup_store_path(&path);
}

#[test]
fn v2_store_prepare_respects_abort_no_reuse_preseeded_membership() {
    let path = temp_store_path("abort-no-reuse");
    let prior_operation = OperationId::new(seed(120));
    let first = ContentEvent {
        content_seq: 1,
        payload: b"first".to_vec(),
    };
    let second = ContentEvent {
        content_seq: 2,
        payload: b"second".to_vec(),
    };
    let row = OperationContentMembership {
        operation_id: prior_operation,
        content_seq: 1,
        content_event_digest: first.digest().unwrap(),
    };
    let membership = CandidateMembership::new(prior_operation, vec![row]).unwrap();
    let candidate_digest = membership.candidate_membership_digest().unwrap();
    let candidate_root = membership.content_candidate_root(&[first.clone()]).unwrap();
    let prior_head = Head::new(
        HeadCore {
            identity: identity(),
            lifecycle: HeadLifecycle::InFlight,
            phase: CheckpointEventKind::Prepare,
            stable_generation: 0,
            content_generation: 1,
            operation_generation: 1,
            content_root: ContentRootDigest::new([0u8; 32]),
            content_freeze: 2,
            candidate_root: Some(candidate_root),
            candidate_freeze: Some(2),
            candidate_membership_digest: Some(candidate_digest),
            operation_id: Some(prior_operation),
        },
        None,
    )
    .unwrap();
    let prior_head_digest = prior_head.digest().unwrap();
    let abort_head = Head::new(
        HeadCore {
            identity: identity(),
            lifecycle: HeadLifecycle::Stable,
            phase: CheckpointEventKind::Abort,
            stable_generation: 0,
            content_generation: 0,
            operation_generation: 1,
            content_root: ContentRootDigest::new([0u8; 32]),
            content_freeze: 2,
            candidate_root: None,
            candidate_freeze: None,
            candidate_membership_digest: None,
            operation_id: None,
        },
        Some(prior_head_digest),
    )
    .unwrap();
    seed_checkpoint_history_store(
        &path,
        3,
        &[first.clone(), second],
        &[prior_head.clone(), abort_head.clone()],
        &[(prior_operation, abort_head.clone())],
        &abort_head,
    );
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "INSERT INTO operation_content_membership (
            operation_id,
            content_seq,
            content_event_digest,
            membership_row_digest,
            candidate_membership_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            prior_operation.as_bytes().to_vec(),
            1_i64,
            row.content_event_digest.as_bytes().to_vec(),
            row.membership_row_digest().unwrap().as_bytes().to_vec(),
            candidate_digest.as_bytes().to_vec(),
        ],
    )
    .unwrap();
    drop(conn);

    let mut reopened = V2Store::open_existing(&path).unwrap();
    assert!(matches!(
        reopened.prepare(identity()),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "membership_window_gap"
    ));
    drop(reopened);

    let conn = Connection::open(&path).unwrap();
    let checkpoint_operation_count = conn
        .query_row("SELECT COUNT(*) FROM checkpoint_operations", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let checkpoint_event_count = conn
        .query_row("SELECT COUNT(*) FROM checkpoint_events", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let anchor_state_count = conn
        .query_row("SELECT COUNT(*) FROM v2_anchor_state", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let membership_count = conn
        .query_row(
            "SELECT COUNT(*) FROM operation_content_membership",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(checkpoint_operation_count, 1);
    assert_eq!(checkpoint_event_count, 2);
    assert_eq!(anchor_state_count, 1);
    assert_eq!(membership_count, 1);

    cleanup_store_path(&path);
}

#[test]
fn v2_store_prepare_transactional_rollback_leaves_no_partial_rows_visible() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_catalog(&conn).unwrap();
    super::store::install_v2_store_catalog(&conn).unwrap();
    conn.execute(
        "INSERT INTO v2_store_state (singleton_id, next_content_seq) VALUES (1, 3)",
        [],
    )
    .unwrap();
    insert_content_event(
        &conn,
        &ContentEvent {
            content_seq: 1,
            payload: b"one".to_vec(),
        },
    );

    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO checkpoint_operations (
            operation_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            head_digest,
            prev_head_digest
        ) VALUES (?1, ?2, ?3, ?4, 'prepare', 0, 1, 1, ?5, 2, ?6, 2, ?7, ?8, NULL)",
        params![
            OperationId::new(seed(130)).as_bytes().to_vec(),
            identity().anchor_instance_id.as_bytes().to_vec(),
            identity().path_binding.as_bytes().to_vec(),
            as_i64(identity().key_epoch),
            [0u8; 32].to_vec(),
            seed(131).to_vec(),
            seed(132).to_vec(),
            seed(133).to_vec(),
        ],
    )
    .unwrap();
    let failed = tx.execute(
        "INSERT INTO v2_anchor_state (
            singleton_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            terminal_checkpoint_digest,
            prev_head_digest,
            head_digest
        ) VALUES (1, ?1, ?2, ?3, 'stable', 'prepare', 0, 1, 1, ?4, 2, NULL, NULL, NULL, NULL, ?5, NULL, ?6)",
        params![
            identity().anchor_instance_id.as_bytes().to_vec(),
            identity().path_binding.as_bytes().to_vec(),
            as_i64(identity().key_epoch),
            [0u8; 32].to_vec(),
            seed(134).to_vec(),
            seed(135).to_vec(),
        ],
    );
    assert!(failed.is_err());
    tx.rollback().unwrap();

    let op_count = conn
        .query_row("SELECT COUNT(*) FROM checkpoint_operations", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let anchor_count = conn
        .query_row("SELECT COUNT(*) FROM v2_anchor_state", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(op_count, 0);
    assert_eq!(anchor_count, 0);
}

#[test]
fn v2_store_open_existing_rejects_tampered_prepare_operation_id() {
    let path = temp_store_path("tamper-opid");
    seed_manual_prepared_store(&path, OperationId::new(seed(220)), 1, 1);

    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "prepare_operation_id_mismatch"
    ));

    cleanup_store_path(&path);
}

#[test]
fn v2_store_open_existing_rejects_tampered_prepare_head_digest() {
    let path = temp_store_path("tamper-head");
    let mut store = V2Store::create_new(&path).unwrap();
    store.append_content(b"alpha").unwrap();
    let prepared = store.prepare(identity()).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE checkpoint_operations
         SET head_digest = ?2
         WHERE operation_id = ?1",
        params![
            prepared.operation_id.as_bytes().to_vec(),
            seed(221).to_vec(),
        ],
    )
    .unwrap();
    drop(conn);

    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "checkpoint_operation_row_invalid"
    ));

    cleanup_store_path(&path);
}

#[test]
fn v2_store_open_existing_rejects_tampered_prepare_freeze_witness() {
    let path = temp_store_path("tamper-freeze");
    let operation_id = super::store::derive_operation_id(identity(), 1, 0, 1, 2, None).unwrap();
    seed_manual_prepared_store(&path, operation_id, 2, 2);

    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "prepare_operation_exceeds_allocator_watermark"
    ));

    cleanup_store_path(&path);
}

#[test]
fn v2_store_sqlite_handle_fail_closed_branch_is_explicit() {
    assert!(matches!(
        super::store::sqlite_main_db_handle_outcome_for_test(rusqlite::ffi::SQLITE_NOTFOUND, true),
        Err(V2Error::SqliteHandleUnavailable { context })
            if context == "sqlite_main_handle_file_control_unsupported"
    ));
    assert!(matches!(
        super::store::sqlite_main_db_handle_outcome_for_test(rusqlite::ffi::SQLITE_OK, true),
        Err(V2Error::SqliteHandleUnavailable { context })
            if context == "sqlite_main_handle_unavailable"
    ));
}

#[test]
fn v2_store_sqlite_handle_identity_mismatch_fails_closed() {
    let err = crate::path_guard::validate_db_leaf_identity_for_test([1, 2, 3], Some([1, 2, 4]))
        .expect_err("mismatched opened handle identity must be rejected");

    assert!(matches!(err, DbError::InvalidPathBinding));
}

#[test]
fn v2_store_open_existing_rejects_self_consistent_prepare_with_stale_freeze() {
    let path = temp_store_path("stale-freeze");
    let operation_id = super::store::derive_operation_id(identity(), 1, 0, 1, 1, None).unwrap();
    seed_manual_prepared_store_with_allocator(&path, operation_id, 1, 1, 3);

    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "inflight_content_freeze_not_current_allocator"
    ));

    cleanup_store_path(&path);
}

#[test]
fn v2_store_open_existing_rejects_terminal_row_operation_id_mismatch() {
    let path = temp_store_path("terminal-opid-mismatch");
    let prepare_operation_id =
        super::store::derive_operation_id(identity(), 1, 0, 1, 1, None).unwrap();
    let prepare_head = manual_prepare_head(prepare_operation_id, 1, 1, 1);
    let terminal_head = stable_head(
        CheckpointEventKind::Abort,
        0,
        0,
        1,
        ContentRootDigest::new([0u8; 32]),
        1,
        Some(prepare_head.digest().unwrap()),
    );
    seed_checkpoint_history_store(
        &path,
        2,
        &[ContentEvent {
            content_seq: 1,
            payload: b"alpha".to_vec(),
        }],
        &[prepare_head, terminal_head.clone()],
        &[(OperationId::new(seed(250)), terminal_head.clone())],
        &terminal_head,
    );

    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "terminal_operation_row_operation_id_mismatch"
    ));

    cleanup_store_path(&path);
}

#[test]
fn v2_store_open_existing_rejects_pending_generation_bump() {
    let path = temp_store_path("pending-generation-bump");
    let operation_id = super::store::derive_operation_id(identity(), 1, 0, 1, 1, None).unwrap();
    let prepare_head = manual_prepare_head(operation_id, 1, 1, 1);
    let pending_head = Head::new(
        HeadCore {
            phase: CheckpointEventKind::Pending,
            operation_generation: 2,
            ..prepare_head.core.clone()
        },
        Some(prepare_head.digest().unwrap()),
    )
    .unwrap();
    seed_checkpoint_history_store(
        &path,
        2,
        &[ContentEvent {
            content_seq: 1,
            payload: b"alpha".to_vec(),
        }],
        &[prepare_head, pending_head.clone()],
        &[(operation_id, pending_head.clone())],
        &pending_head,
    );
    let conn = Connection::open(&path).unwrap();
    insert_membership_row(
        &conn,
        &OperationContentMembership {
            operation_id,
            content_seq: 1,
            content_event_digest: ContentEvent {
                content_seq: 1,
                payload: b"alpha".to_vec(),
            }
            .digest()
            .unwrap(),
        },
        pending_head.core.candidate_membership_digest.unwrap(),
    )
    .unwrap();
    drop(conn);

    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "pending_operation_prev_head_mismatch"
    ));

    cleanup_store_path(&path);
}

#[test]
fn v2_store_open_existing_rejects_terminal_generation_bump() {
    let path = temp_store_path("terminal-generation-bump");
    let prepare_operation_id =
        super::store::derive_operation_id(identity(), 1, 0, 1, 1, None).unwrap();
    let prepare_head = manual_prepare_head(prepare_operation_id, 1, 1, 1);
    let terminal_head = stable_head(
        CheckpointEventKind::Abort,
        0,
        0,
        2,
        ContentRootDigest::new([0u8; 32]),
        1,
        Some(prepare_head.digest().unwrap()),
    );
    seed_checkpoint_history_store(
        &path,
        2,
        &[ContentEvent {
            content_seq: 1,
            payload: b"alpha".to_vec(),
        }],
        &[prepare_head, terminal_head.clone()],
        &[(prepare_operation_id, terminal_head.clone())],
        &terminal_head,
    );

    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "terminal_operation_prev_head_mismatch"
    ));

    cleanup_store_path(&path);
}

#[test]
fn v2_store_open_existing_rejects_terminal_skipping_current_inflight() {
    let path = temp_store_path("terminal-skip-current-inflight");
    let prepare_operation_id =
        super::store::derive_operation_id(identity(), 1, 0, 1, 1, None).unwrap();
    let prepare_head = manual_prepare_head(prepare_operation_id, 1, 1, 1);
    let pending_head = inflight_head(
        CheckpointEventKind::Pending,
        0,
        1,
        1,
        ContentRootDigest::new([0u8; 32]),
        1,
        prepare_head.core.candidate_root.clone().unwrap(),
        1,
        prepare_head
            .core
            .candidate_membership_digest
            .clone()
            .unwrap(),
        prepare_operation_id,
        Some(prepare_head.digest().unwrap()),
    );
    let skipped_terminal = stable_head(
        CheckpointEventKind::Abort,
        0,
        0,
        1,
        ContentRootDigest::new([0u8; 32]),
        1,
        Some(prepare_head.digest().unwrap()),
    );
    seed_checkpoint_history_store(
        &path,
        2,
        &[ContentEvent {
            content_seq: 1,
            payload: b"alpha".to_vec(),
        }],
        &[prepare_head, pending_head, skipped_terminal.clone()],
        &[(prepare_operation_id, skipped_terminal.clone())],
        &skipped_terminal,
    );

    assert!(matches!(
        V2Store::open_existing(&path),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "checkpoint_history_prev_head_gap"
    ));

    cleanup_store_path(&path);
}

#[test]
fn v2_store_membership_window_must_be_exact_and_contiguous() {
    let operation_id = OperationId::new(seed(231));
    let row_one = membership_row(operation_id, 1, b"one");
    let row_two = membership_row(operation_id, 2, b"two");
    let row_three = membership_row(operation_id, 3, b"three");

    assert!(super::store::validate_membership_window_for_test(&[row_one, row_two], 0, 2).is_ok());
    assert!(matches!(
        super::store::validate_membership_window_for_test(&[row_one], 0, 2),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "membership_window_missing_tail"
    ));
    assert!(matches!(
        super::store::validate_membership_window_for_test(&[row_one, row_three], 0, 3),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "membership_window_gap"
    ));
    assert!(matches!(
        super::store::validate_membership_window_for_test(&[row_one, row_three], 0, 2),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "membership_row_outside_prepare_window"
    ));
    assert!(matches!(
        super::store::validate_membership_window_for_test(&[row_one, row_one], 0, 1),
        Err(V2Error::DuplicateContentSeq { content_seq }) if content_seq == 1
    ));
}

#[test]
fn v2_store_history_must_link_latest_visible_heads() {
    let stable_one = stable_head(
        CheckpointEventKind::Finalize,
        0,
        0,
        1,
        ContentRootDigest::new([0u8; 32]),
        0,
        None,
    );
    let stable_two = stable_head(
        CheckpointEventKind::Finalize,
        1,
        1,
        2,
        ContentRootDigest::new(seed(240)),
        1,
        Some(stable_one.digest().unwrap()),
    );
    let good_prepare = inflight_head(
        CheckpointEventKind::Prepare,
        1,
        2,
        3,
        stable_two.core.content_root,
        2,
        ContentRootDigest::new(seed(241)),
        2,
        MemberDigest::new(seed(242)),
        OperationId::new(seed(243)),
        Some(stable_two.digest().unwrap()),
    );
    assert!(super::store::validate_head_history_successor_for_test(
        Some(&stable_two),
        &good_prepare
    )
    .is_ok());

    let skipped_prepare = inflight_head(
        CheckpointEventKind::Prepare,
        0,
        1,
        3,
        stable_one.core.content_root,
        1,
        ContentRootDigest::new(seed(244)),
        1,
        MemberDigest::new(seed(245)),
        OperationId::new(seed(246)),
        Some(stable_one.digest().unwrap()),
    );
    assert!(matches!(
        super::store::validate_head_history_successor_for_test(Some(&stable_two), &skipped_prepare),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "checkpoint_history_prev_head_gap"
    ));

    let pending = inflight_head(
        CheckpointEventKind::Pending,
        1,
        2,
        3,
        stable_two.core.content_root,
        2,
        ContentRootDigest::new(seed(247)),
        2,
        MemberDigest::new(seed(248)),
        good_prepare.core.operation_id.unwrap(),
        Some(good_prepare.digest().unwrap()),
    );
    let good_abort = stable_head(
        CheckpointEventKind::Abort,
        1,
        1,
        3,
        stable_two.core.content_root,
        2,
        Some(pending.digest().unwrap()),
    );
    assert!(
        super::store::validate_head_history_successor_for_test(Some(&pending), &good_abort).is_ok()
    );

    let skipped_terminal = stable_head(
        CheckpointEventKind::Abort,
        1,
        1,
        3,
        stable_two.core.content_root,
        2,
        Some(good_prepare.digest().unwrap()),
    );
    assert!(matches!(
        super::store::validate_head_history_successor_for_test(Some(&pending), &skipped_terminal),
        Err(V2Error::AuthorityInvariantViolation { context })
            if context == "checkpoint_history_prev_head_gap"
    ));
}

fn inflight_core(phase: CheckpointEventKind) -> HeadCore {
    HeadCore {
        identity: identity(),
        lifecycle: HeadLifecycle::InFlight,
        phase,
        stable_generation: 4,
        content_generation: 5,
        operation_generation: 9,
        content_root: ContentRootDigest::new(seed(10)),
        content_freeze: 12,
        candidate_root: Some(ContentRootDigest::new(seed(11))),
        candidate_freeze: Some(14),
        candidate_membership_digest: Some(MemberDigest::new(seed(12))),
        operation_id: Some(OperationId::new(seed(13))),
    }
}

fn stable_core(phase: CheckpointEventKind) -> HeadCore {
    HeadCore {
        identity: identity(),
        lifecycle: HeadLifecycle::Stable,
        phase,
        stable_generation: 5,
        content_generation: 5,
        operation_generation: 9,
        content_root: ContentRootDigest::new(seed(14)),
        content_freeze: 15,
        candidate_root: None,
        candidate_freeze: None,
        candidate_membership_digest: None,
        operation_id: None,
    }
}

fn identity() -> AnchorIdentity {
    AnchorIdentity {
        anchor_instance_id: AnchorInstanceId::new(seed(1)),
        path_binding: PathBindingDigest::new(seed(2)),
        key_epoch: 3,
    }
}

fn successor_head_fixture() -> SuccessorHead {
    successor_head_fixture_with_anchor(identity().anchor_instance_id)
}

fn successor_head_fixture_with_anchor(anchor_instance_id: AnchorInstanceId) -> SuccessorHead {
    SuccessorHead {
        anchor_identity: AnchorIdentity {
            anchor_instance_id,
            ..identity()
        },
        prev_head_digest: super::HeadDigest::new(seed(150)),
        intent_generation: 1,
        frame_sequence: 1,
        predecessor_head_digest: super::HeadDigest::new(seed(151)),
        plan_binding_witness: plan_binding_witness_fixture(),
        state: IntentState::Authorized,
        stable_root: ContentRootDigest::new(seed(153)),
        content_root: ContentRootDigest::new(seed(154)),
        event_value: b"event-payload".to_vec(),
    }
}

fn intent_record_fixture(anchor_instance_id: AnchorInstanceId, index: u8) -> IntentRecord {
    let witness_seed = 160_u8.saturating_add(index.saturating_mul(8));
    IntentRecord::new(&SuccessorHead {
        anchor_identity: AnchorIdentity {
            anchor_instance_id,
            path_binding: PathBindingDigest::new(seed(80_u8.saturating_add(index))),
            key_epoch: 3_u64 + u64::from(index),
        },
        prev_head_digest: super::HeadDigest::new(seed(90_u8.saturating_add(index))),
        intent_generation: u64::from(index) + 1,
        frame_sequence: u64::from(index) + 1,
        predecessor_head_digest: super::HeadDigest::new(seed(100_u8.saturating_add(index))),
        plan_binding_witness: PlanBindingWitness {
            bound_operation_id: OperationId::new(seed(witness_seed)),
            bound_operation_generation: 7_u64 + u64::from(index),
            predecessor_stable_generation: 4_u64 + u64::from(index),
            predecessor_content_generation: 6_u64 + u64::from(index),
            predecessor_content_freeze: 9_u64 + u64::from(index),
            predecessor_candidate_freeze: 11_u64 + u64::from(index),
            predecessor_stable_root: ContentRootDigest::new(seed(witness_seed + 1)),
            predecessor_candidate_root: ContentRootDigest::new(seed(witness_seed + 2)),
            predecessor_candidate_membership_digest: MemberDigest::new(seed(witness_seed + 3)),
        },
        state: if index % 2 == 0 {
            IntentState::Applied
        } else {
            IntentState::Authorized
        },
        stable_root: ContentRootDigest::new(seed(110_u8.saturating_add(index))),
        content_root: ContentRootDigest::new(seed(120_u8.saturating_add(index))),
        event_value: format!("event-payload-{index}").into_bytes(),
    })
    .unwrap()
}

fn plan_binding_witness_fixture() -> PlanBindingWitness {
    PlanBindingWitness {
        bound_operation_id: OperationId::new(seed(152)),
        bound_operation_generation: 7,
        predecessor_stable_generation: 4,
        predecessor_content_generation: 6,
        predecessor_content_freeze: 9,
        predecessor_candidate_freeze: 11,
        predecessor_stable_root: ContentRootDigest::new(seed(155)),
        predecessor_candidate_root: ContentRootDigest::new(seed(156)),
        predecessor_candidate_membership_digest: MemberDigest::new(seed(157)),
    }
}

fn seed(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn insert_content_event(conn: &Connection, event: &ContentEvent) {
    conn.execute(
        "INSERT INTO content_events (content_seq, content_event_digest, content_payload)
         VALUES (?1, ?2, ?3)",
        params![
            as_i64(event.content_seq),
            event.digest().unwrap().as_bytes().to_vec(),
            event.payload.clone()
        ],
    )
    .unwrap();
}

fn insert_foundation_anchor_instance_row(conn: &Connection, anchor_instance_id: AnchorInstanceId) {
    conn.execute(
        "INSERT INTO v2_anchor_instance (singleton_id, anchor_instance_id)
         VALUES (1, ?1)",
        params![anchor_instance_id.as_bytes().to_vec()],
    )
    .unwrap();
}

fn insert_foundation_genesis_row(conn: &Connection, record: &GenesisRecord) {
    conn.execute(
        "INSERT INTO v2_genesis (
            singleton_id,
            anchor_instance_id,
            protocol_version,
            canonical_genesis_bytes,
            canonical_genesis_len,
            genesis_digest,
            stable_generation,
            content_generation,
            work_generation,
            stable_root,
            content_root,
            candidate_root,
            operation_id,
            predecessor_head_digest,
            predecessor_candidate_freeze,
            plan_binding_digest
        ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            record.anchor_instance_id.as_bytes().to_vec(),
            record.protocol_version.as_str(),
            record.canonical_genesis_bytes.clone(),
            as_i64(record.canonical_genesis_len),
            record.genesis_digest.as_bytes().to_vec(),
            as_i64(record.stable_generation),
            as_i64(record.content_generation),
            as_i64(record.work_generation),
            record.stable_root.as_bytes().to_vec(),
            record.content_root.as_bytes().to_vec(),
            record
                .candidate_root
                .map(|digest| digest.as_bytes().to_vec()),
            record
                .operation_id
                .map(|operation_id| operation_id.as_bytes().to_vec()),
            record
                .predecessor_head_digest
                .map(|digest| digest.as_bytes().to_vec()),
            record.predecessor_candidate_freeze.map(as_i64),
            record
                .plan_binding_digest
                .map(|digest| digest.as_bytes().to_vec()),
        ],
    )
    .unwrap();
}

fn insert_foundation_intent_row(conn: &Connection, record: &IntentRecord) {
    conn.execute(
        "INSERT INTO v2_intents (
            successor_head_digest,
            protocol_version,
            canonical_successor_bytes,
            canonical_successor_len,
            successor_event_digest,
            event_value_offset,
            event_value_len,
            prev_head_digest,
            intent_generation,
            frame_sequence,
            predecessor_head_digest,
            predecessor_candidate_freeze,
            plan_binding_digest,
            intent_state,
            stable_root,
            content_root
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            record.successor_head_digest.as_bytes().to_vec(),
            record.protocol_version.as_str(),
            record.canonical_successor_bytes.clone(),
            as_i64(record.canonical_successor_len),
            record.successor_event_digest.as_bytes().to_vec(),
            as_i64(record.event_value_offset),
            as_i64(record.event_value_len),
            record.prev_head_digest.as_bytes().to_vec(),
            as_i64(record.intent_generation),
            as_i64(record.frame_sequence),
            record.predecessor_head_digest.as_bytes().to_vec(),
            as_i64(record.plan_binding_witness.predecessor_candidate_freeze),
            record.plan_binding_digest.as_bytes().to_vec(),
            record.state.as_sql(),
            record.stable_root.as_bytes().to_vec(),
            record.content_root.as_bytes().to_vec(),
        ],
    )
    .unwrap();
}

fn insert_prepare_operation(
    conn: &Connection,
    operation_id: OperationId,
    operation_generation: u64,
    candidate_membership_digest: MemberDigest,
) {
    conn.execute(
        "INSERT INTO checkpoint_operations (
            operation_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            head_digest,
            prev_head_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            operation_id.as_bytes().to_vec(),
            identity().anchor_instance_id.as_bytes().to_vec(),
            identity().path_binding.as_bytes().to_vec(),
            as_i64(identity().key_epoch),
            "prepare",
            4_i64,
            5_i64,
            as_i64(operation_generation),
            seed(60).to_vec(),
            12_i64,
            seed(61).to_vec(),
            14_i64,
            candidate_membership_digest.as_bytes().to_vec(),
            Option::<Vec<u8>>::None,
            Option::<Vec<u8>>::None,
        ],
    )
    .unwrap();
}

fn insert_membership_row(
    conn: &Connection,
    row: &OperationContentMembership,
    candidate_membership_digest: MemberDigest,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO operation_content_membership (
            operation_id,
            content_seq,
            content_event_digest,
            membership_row_digest,
            candidate_membership_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            row.operation_id.as_bytes().to_vec(),
            as_i64(row.content_seq),
            row.content_event_digest.as_bytes().to_vec(),
            row.membership_row_digest().unwrap().as_bytes().to_vec(),
            candidate_membership_digest.as_bytes().to_vec(),
        ],
    )
}

fn insert_checkpoint_event(
    conn: &Connection,
    lifecycle: HeadLifecycle,
    phase: CheckpointEventKind,
    stable_generation: u64,
    content_generation: u64,
    head_seed: u8,
) -> rusqlite::Result<usize> {
    let lifecycle = match lifecycle {
        HeadLifecycle::Stable => "stable",
        HeadLifecycle::InFlight => "in_flight",
    };
    let phase = match phase {
        CheckpointEventKind::Prepare => "prepare",
        CheckpointEventKind::Pending => "pending",
        CheckpointEventKind::Finalize => "finalize",
        CheckpointEventKind::Abort => "abort",
    };
    conn.execute(
        "INSERT INTO checkpoint_events (
            checkpoint_event_digest,
            event_kind,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            head_digest,
            prev_head_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            seed(head_seed).to_vec(),
            phase,
            identity().anchor_instance_id.as_bytes().to_vec(),
            identity().path_binding.as_bytes().to_vec(),
            as_i64(identity().key_epoch),
            lifecycle,
            phase,
            as_i64(stable_generation),
            as_i64(content_generation),
            9_i64,
            seed(head_seed.saturating_add(1)).to_vec(),
            15_i64,
            Option::<Vec<u8>>::None,
            Option::<i64>::None,
            Option::<Vec<u8>>::None,
            Option::<Vec<u8>>::None,
            seed(head_seed.saturating_add(2)).to_vec(),
            Option::<Vec<u8>>::None,
        ],
    )
}

fn insert_anchor_state(
    conn: &Connection,
    lifecycle: HeadLifecycle,
    phase: CheckpointEventKind,
    stable_generation: u64,
    content_generation: u64,
    head_seed: u8,
) -> rusqlite::Result<usize> {
    let lifecycle = match lifecycle {
        HeadLifecycle::Stable => "stable",
        HeadLifecycle::InFlight => "in_flight",
    };
    let phase = match phase {
        CheckpointEventKind::Prepare => "prepare",
        CheckpointEventKind::Pending => "pending",
        CheckpointEventKind::Finalize => "finalize",
        CheckpointEventKind::Abort => "abort",
    };
    conn.execute(
        "INSERT INTO v2_anchor_state (
            singleton_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            terminal_checkpoint_digest,
            prev_head_digest,
            head_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            1_i64,
            identity().anchor_instance_id.as_bytes().to_vec(),
            identity().path_binding.as_bytes().to_vec(),
            as_i64(identity().key_epoch),
            lifecycle,
            phase,
            as_i64(stable_generation),
            as_i64(content_generation),
            9_i64,
            seed(head_seed).to_vec(),
            15_i64,
            Option::<Vec<u8>>::None,
            Option::<i64>::None,
            Option::<Vec<u8>>::None,
            Option::<Vec<u8>>::None,
            seed(head_seed.saturating_add(1)).to_vec(),
            Option::<Vec<u8>>::None,
            seed(head_seed.saturating_add(2)).to_vec(),
        ],
    )
}

fn insert_terminal_operation(
    conn: &Connection,
    operation_id: OperationId,
    phase: CheckpointEventKind,
    stable_generation: u64,
    content_generation: u64,
    operation_generation: u64,
) -> rusqlite::Result<usize> {
    let phase = match phase {
        CheckpointEventKind::Finalize => "finalize",
        CheckpointEventKind::Abort => "abort",
        CheckpointEventKind::Prepare => "prepare",
        CheckpointEventKind::Pending => "pending",
    };
    conn.execute(
        "INSERT INTO checkpoint_operations (
            operation_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            head_digest,
            prev_head_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            operation_id.as_bytes().to_vec(),
            identity().anchor_instance_id.as_bytes().to_vec(),
            identity().path_binding.as_bytes().to_vec(),
            as_i64(identity().key_epoch),
            phase,
            as_i64(stable_generation),
            as_i64(content_generation),
            as_i64(operation_generation),
            seed(95).to_vec(),
            15_i64,
            Option::<Vec<u8>>::None,
            Option::<i64>::None,
            Option::<Vec<u8>>::None,
            Option::<Vec<u8>>::None,
            Option::<Vec<u8>>::None,
        ],
    )
}

fn seed_manual_prepared_store(
    path: &PathBuf,
    row_operation_id: OperationId,
    content_freeze: u64,
    candidate_freeze: u64,
) {
    seed_manual_prepared_store_with_allocator(
        path,
        row_operation_id,
        content_freeze,
        candidate_freeze,
        2,
    );
}

fn seed_manual_prepared_store_with_allocator(
    path: &PathBuf,
    row_operation_id: OperationId,
    content_freeze: u64,
    candidate_freeze: u64,
    next_content_seq: u64,
) {
    let conn = Connection::open(path).unwrap();
    apply_catalog(&conn).unwrap();
    super::store::install_v2_store_catalog(&conn).unwrap();
    conn.execute(
        "INSERT INTO schema_meta (
            singleton_id,
            schema_version,
            schema_params,
            manifest_hash,
            params_hash,
            catalog_hash
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            1_i64,
            V2_SCHEMA_VERSION,
            V2_SCHEMA_PARAMS,
            manifest_hash().unwrap().to_vec(),
            params_hash().unwrap().to_vec(),
            catalog_hash().unwrap().to_vec(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO v2_store_state (singleton_id, next_content_seq) VALUES (1, ?1)",
        params![as_i64(next_content_seq)],
    )
    .unwrap();

    let content_event = ContentEvent {
        content_seq: 1,
        payload: b"alpha".to_vec(),
    };
    insert_content_event(&conn, &content_event);

    let membership_row = OperationContentMembership {
        operation_id: row_operation_id,
        content_seq: 1,
        content_event_digest: content_event.digest().unwrap(),
    };
    let membership = CandidateMembership::new(row_operation_id, vec![membership_row]).unwrap();
    let candidate_membership_digest = membership.candidate_membership_digest().unwrap();
    let candidate_root = membership
        .content_candidate_root(&[content_event.clone()])
        .unwrap();
    let head = Head::new(
        HeadCore {
            identity: identity(),
            lifecycle: HeadLifecycle::InFlight,
            phase: CheckpointEventKind::Prepare,
            stable_generation: 0,
            content_generation: 1,
            operation_generation: 1,
            content_root: ContentRootDigest::new([0u8; 32]),
            content_freeze,
            candidate_root: Some(candidate_root),
            candidate_freeze: Some(candidate_freeze),
            candidate_membership_digest: Some(candidate_membership_digest),
            operation_id: Some(row_operation_id),
        },
        None,
    )
    .unwrap();
    let head_digest = head.digest().unwrap();
    let checkpoint_event_digest = head.checkpoint_event().unwrap().digest().unwrap();

    conn.execute(
        "INSERT INTO checkpoint_operations (
            operation_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            head_digest,
            prev_head_digest
        ) VALUES (?1, ?2, ?3, ?4, 'prepare', 0, 1, 1, ?5, ?6, ?7, ?8, ?9, ?10, NULL)",
        params![
            row_operation_id.as_bytes().to_vec(),
            identity().anchor_instance_id.as_bytes().to_vec(),
            identity().path_binding.as_bytes().to_vec(),
            as_i64(identity().key_epoch),
            [0u8; 32].to_vec(),
            as_i64(content_freeze),
            candidate_root.as_bytes().to_vec(),
            as_i64(candidate_freeze),
            candidate_membership_digest.as_bytes().to_vec(),
            head_digest.as_bytes().to_vec(),
        ],
    )
    .unwrap();
    insert_membership_row(&conn, &membership.rows[0], candidate_membership_digest).unwrap();
    conn.execute(
        "INSERT INTO checkpoint_events (
            checkpoint_event_digest,
            event_kind,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            head_digest,
            prev_head_digest
        ) VALUES (?1, 'prepare', ?2, ?3, ?4, 'in_flight', 'prepare', 0, 1, 1, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)",
        params![
            checkpoint_event_digest.as_bytes().to_vec(),
            identity().anchor_instance_id.as_bytes().to_vec(),
            identity().path_binding.as_bytes().to_vec(),
            as_i64(identity().key_epoch),
            [0u8; 32].to_vec(),
            as_i64(content_freeze),
            candidate_root.as_bytes().to_vec(),
            as_i64(candidate_freeze),
            candidate_membership_digest.as_bytes().to_vec(),
            row_operation_id.as_bytes().to_vec(),
            head_digest.as_bytes().to_vec(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO v2_anchor_state (
            singleton_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            terminal_checkpoint_digest,
            prev_head_digest,
            head_digest
        ) VALUES (1, ?1, ?2, ?3, 'in_flight', 'prepare', 0, 1, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11)",
        params![
            identity().anchor_instance_id.as_bytes().to_vec(),
            identity().path_binding.as_bytes().to_vec(),
            as_i64(identity().key_epoch),
            [0u8; 32].to_vec(),
            as_i64(content_freeze),
            candidate_root.as_bytes().to_vec(),
            as_i64(candidate_freeze),
            candidate_membership_digest.as_bytes().to_vec(),
            row_operation_id.as_bytes().to_vec(),
            checkpoint_event_digest.as_bytes().to_vec(),
            head_digest.as_bytes().to_vec(),
        ],
    )
    .unwrap();
}

fn seed_checkpoint_history_store(
    path: &PathBuf,
    next_content_seq: u64,
    content_events: &[ContentEvent],
    checkpoint_events: &[Head],
    checkpoint_operations: &[(OperationId, Head)],
    anchor_head: &Head,
) {
    let conn = Connection::open(path).unwrap();
    apply_catalog(&conn).unwrap();
    super::store::install_v2_store_catalog(&conn).unwrap();
    conn.execute(
        "INSERT INTO schema_meta (
            singleton_id,
            schema_version,
            schema_params,
            manifest_hash,
            params_hash,
            catalog_hash
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            1_i64,
            V2_SCHEMA_VERSION,
            V2_SCHEMA_PARAMS,
            manifest_hash().unwrap().to_vec(),
            params_hash().unwrap().to_vec(),
            catalog_hash().unwrap().to_vec(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO v2_store_state (singleton_id, next_content_seq) VALUES (1, ?1)",
        params![as_i64(next_content_seq)],
    )
    .unwrap();
    for content_event in content_events {
        insert_content_event(&conn, content_event);
    }
    for (operation_id, head) in checkpoint_operations {
        insert_checkpoint_operation_head(&conn, *operation_id, head);
    }
    for checkpoint_event in checkpoint_events {
        insert_checkpoint_event_head(&conn, checkpoint_event);
    }
    insert_anchor_state_head(&conn, anchor_head);
}

fn insert_checkpoint_event_head(conn: &Connection, head: &Head) {
    conn.execute(
        "INSERT INTO checkpoint_events (
            checkpoint_event_digest,
            event_kind,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            head_digest,
            prev_head_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            head.terminal_checkpoint_digest.as_bytes().to_vec(),
            phase_name(head.core.phase),
            head.core.identity.anchor_instance_id.as_bytes().to_vec(),
            head.core.identity.path_binding.as_bytes().to_vec(),
            as_i64(head.core.identity.key_epoch),
            lifecycle_name(head.core.lifecycle),
            phase_name(head.core.phase),
            as_i64(head.core.stable_generation),
            as_i64(head.core.content_generation),
            as_i64(head.core.operation_generation),
            head.core.content_root.as_bytes().to_vec(),
            as_i64(head.core.content_freeze),
            head.core
                .candidate_root
                .map(|digest| digest.as_bytes().to_vec()),
            head.core.candidate_freeze.map(as_i64),
            head.core
                .candidate_membership_digest
                .map(|digest| digest.as_bytes().to_vec()),
            head.core
                .operation_id
                .map(|operation_id| operation_id.as_bytes().to_vec()),
            head.digest().unwrap().as_bytes().to_vec(),
            head.prev_head_digest
                .map(|digest| digest.as_bytes().to_vec()),
        ],
    )
    .unwrap();
}

fn insert_checkpoint_operation_head(conn: &Connection, operation_id: OperationId, head: &Head) {
    conn.execute(
        "INSERT INTO checkpoint_operations (
            operation_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            head_digest,
            prev_head_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            operation_id.as_bytes().to_vec(),
            head.core.identity.anchor_instance_id.as_bytes().to_vec(),
            head.core.identity.path_binding.as_bytes().to_vec(),
            as_i64(head.core.identity.key_epoch),
            phase_name(head.core.phase),
            as_i64(head.core.stable_generation),
            as_i64(head.core.content_generation),
            as_i64(head.core.operation_generation),
            head.core.content_root.as_bytes().to_vec(),
            as_i64(head.core.content_freeze),
            head.core
                .candidate_root
                .map(|digest| digest.as_bytes().to_vec()),
            head.core.candidate_freeze.map(as_i64),
            head.core
                .candidate_membership_digest
                .map(|digest| digest.as_bytes().to_vec()),
            head.digest().unwrap().as_bytes().to_vec(),
            head.prev_head_digest
                .map(|digest| digest.as_bytes().to_vec()),
        ],
    )
    .unwrap();
}

fn insert_anchor_state_head(conn: &Connection, head: &Head) {
    conn.execute(
        "INSERT INTO v2_anchor_state (
            singleton_id,
            anchor_instance_id,
            path_binding,
            key_epoch,
            lifecycle,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root,
            candidate_freeze,
            candidate_membership_digest,
            operation_id,
            terminal_checkpoint_digest,
            prev_head_digest,
            head_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            1_i64,
            head.core.identity.anchor_instance_id.as_bytes().to_vec(),
            head.core.identity.path_binding.as_bytes().to_vec(),
            as_i64(head.core.identity.key_epoch),
            lifecycle_name(head.core.lifecycle),
            phase_name(head.core.phase),
            as_i64(head.core.stable_generation),
            as_i64(head.core.content_generation),
            as_i64(head.core.operation_generation),
            head.core.content_root.as_bytes().to_vec(),
            as_i64(head.core.content_freeze),
            head.core
                .candidate_root
                .map(|digest| digest.as_bytes().to_vec()),
            head.core.candidate_freeze.map(as_i64),
            head.core
                .candidate_membership_digest
                .map(|digest| digest.as_bytes().to_vec()),
            head.core
                .operation_id
                .map(|operation_id| operation_id.as_bytes().to_vec()),
            head.terminal_checkpoint_digest.as_bytes().to_vec(),
            head.prev_head_digest
                .map(|digest| digest.as_bytes().to_vec()),
            head.digest().unwrap().as_bytes().to_vec(),
        ],
    )
    .unwrap();
}

fn manual_prepare_head(
    operation_id: OperationId,
    content_freeze: u64,
    candidate_freeze: u64,
    operation_generation: u64,
) -> Head {
    let content_event = ContentEvent {
        content_seq: 1,
        payload: b"alpha".to_vec(),
    };
    let membership_row = OperationContentMembership {
        operation_id,
        content_seq: 1,
        content_event_digest: content_event.digest().unwrap(),
    };
    let membership = CandidateMembership::new(operation_id, vec![membership_row]).unwrap();
    let candidate_membership_digest = membership.candidate_membership_digest().unwrap();
    let candidate_root = membership.content_candidate_root(&[content_event]).unwrap();
    Head::new(
        HeadCore {
            identity: identity(),
            lifecycle: HeadLifecycle::InFlight,
            phase: CheckpointEventKind::Prepare,
            stable_generation: 0,
            content_generation: 1,
            operation_generation,
            content_root: ContentRootDigest::new([0u8; 32]),
            content_freeze,
            candidate_root: Some(candidate_root),
            candidate_freeze: Some(candidate_freeze),
            candidate_membership_digest: Some(candidate_membership_digest),
            operation_id: Some(operation_id),
        },
        None,
    )
    .unwrap()
}

fn lifecycle_name(lifecycle: HeadLifecycle) -> &'static str {
    match lifecycle {
        HeadLifecycle::Stable => "stable",
        HeadLifecycle::InFlight => "in_flight",
    }
}

fn phase_name(phase: CheckpointEventKind) -> &'static str {
    match phase {
        CheckpointEventKind::Prepare => "prepare",
        CheckpointEventKind::Pending => "pending",
        CheckpointEventKind::Finalize => "finalize",
        CheckpointEventKind::Abort => "abort",
    }
}

fn as_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap()
}

fn membership_row(
    operation_id: OperationId,
    content_seq: u64,
    payload: &[u8],
) -> OperationContentMembership {
    OperationContentMembership {
        operation_id,
        content_seq,
        content_event_digest: ContentEvent {
            content_seq,
            payload: payload.to_vec(),
        }
        .digest()
        .unwrap(),
    }
}

fn inflight_head(
    phase: CheckpointEventKind,
    stable_generation: u64,
    content_generation: u64,
    operation_generation: u64,
    content_root: ContentRootDigest,
    content_freeze: u64,
    candidate_root: ContentRootDigest,
    candidate_freeze: u64,
    candidate_membership_digest: MemberDigest,
    operation_id: OperationId,
    prev_head_digest: Option<super::HeadDigest>,
) -> Head {
    Head::new(
        HeadCore {
            identity: identity(),
            lifecycle: HeadLifecycle::InFlight,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root: Some(candidate_root),
            candidate_freeze: Some(candidate_freeze),
            candidate_membership_digest: Some(candidate_membership_digest),
            operation_id: Some(operation_id),
        },
        prev_head_digest,
    )
    .unwrap()
}

fn stable_head(
    phase: CheckpointEventKind,
    stable_generation: u64,
    content_generation: u64,
    operation_generation: u64,
    content_root: ContentRootDigest,
    content_freeze: u64,
    prev_head_digest: Option<super::HeadDigest>,
) -> Head {
    Head::new(
        HeadCore {
            identity: identity(),
            lifecycle: HeadLifecycle::Stable,
            phase,
            stable_generation,
            content_generation,
            operation_generation,
            content_root,
            content_freeze,
            candidate_root: None,
            candidate_freeze: None,
            candidate_membership_digest: None,
            operation_id: None,
        },
        prev_head_digest,
    )
    .unwrap()
}

fn assert_sqlite_error_contains(result: rusqlite::Result<usize>, expected: &str) {
    let err = result.expect_err("expected sqlite trigger failure");
    assert!(
        err.to_string().contains(expected),
        "expected sqlite error containing {expected}, got {err}"
    );
}

fn temp_store_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("lumina-evidence-db-v2-{label}-{unique}"));
    fs::create_dir_all(&root).unwrap();
    root.join("evidence.db")
}

fn cleanup_store_path(path: &PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir_all(parent);
    }
}
