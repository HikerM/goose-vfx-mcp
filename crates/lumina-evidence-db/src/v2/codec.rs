use super::error::V2Result;
use super::model::{
    AnchorIdentity, AnchorInstanceId, CandidateMembership, CheckpointEvent, CheckpointEventDigest,
    ContentEvent, ContentRootDigest, GenesisHead, GenesisRecord, Head, HeadCore, HeadDigest,
    IntentId, IntentRecord, MemberDigest, OperationContentMembership, PlanBindingDigest,
    PlanBindingWitness, SuccessorEventDigest, SuccessorHead,
};

pub fn encode_head_payload(head: &Head) -> V2Result<Vec<u8>> {
    head.canonical_bytes()
}

pub fn decode_head_payload(bytes: &[u8]) -> V2Result<Head> {
    Head::from_canonical_bytes(bytes)
}

pub fn checkpoint_event_from_head_core(
    head_core: HeadCore,
    prev_head_digest: Option<HeadDigest>,
) -> V2Result<CheckpointEvent> {
    CheckpointEvent::new(head_core.phase, prev_head_digest, head_core)
}

pub fn checkpoint_event_digest_from_head_core(
    head_core: HeadCore,
    prev_head_digest: Option<HeadDigest>,
) -> V2Result<CheckpointEventDigest> {
    checkpoint_event_from_head_core(head_core, prev_head_digest)?.digest()
}

pub fn head_digest_from_head_core(
    head_core: HeadCore,
    prev_head_digest: Option<HeadDigest>,
) -> V2Result<HeadDigest> {
    Head::new(head_core, prev_head_digest)?.digest()
}

pub fn encode_content_event(event: &ContentEvent) -> V2Result<Vec<u8>> {
    event.canonical_bytes()
}

pub fn decode_content_event(bytes: &[u8]) -> V2Result<ContentEvent> {
    ContentEvent::from_canonical_bytes(bytes)
}

pub fn encode_membership_row(row: &OperationContentMembership) -> V2Result<Vec<u8>> {
    row.canonical_bytes()
}

pub fn decode_membership_row(bytes: &[u8]) -> V2Result<OperationContentMembership> {
    OperationContentMembership::from_canonical_bytes(bytes)
}

pub fn membership_row_digest(row: &OperationContentMembership) -> V2Result<MemberDigest> {
    row.membership_row_digest()
}

pub fn candidate_membership_digest(candidate: &CandidateMembership) -> V2Result<MemberDigest> {
    candidate.candidate_membership_digest()
}

pub fn membership_digest(candidate: &CandidateMembership) -> V2Result<MemberDigest> {
    candidate_membership_digest(candidate)
}

pub fn content_candidate_root(
    candidate: &CandidateMembership,
    content_events: &[ContentEvent],
) -> V2Result<ContentRootDigest> {
    candidate.content_candidate_root(content_events)
}

pub fn encode_genesis_head(head: &GenesisHead) -> V2Result<Vec<u8>> {
    head.canonical_bytes()
}

pub fn decode_genesis_head(bytes: &[u8]) -> V2Result<GenesisHead> {
    GenesisHead::from_canonical_bytes(bytes)
}

pub fn build_genesis_record(anchor_instance_id: AnchorInstanceId) -> V2Result<GenesisRecord> {
    GenesisRecord::new(anchor_instance_id)
}

pub fn encode_successor_head(head: &SuccessorHead) -> V2Result<Vec<u8>> {
    head.canonical_bytes()
}

pub fn decode_successor_head(bytes: &[u8]) -> V2Result<SuccessorHead> {
    SuccessorHead::from_canonical_bytes(bytes)
}

pub fn build_intent_record(head: &SuccessorHead) -> V2Result<IntentRecord> {
    IntentRecord::new(head)
}

pub fn build_intent_id(
    anchor_identity: AnchorIdentity,
    intent_generation: u64,
    prev_head_digest: HeadDigest,
    successor_head_digest: HeadDigest,
) -> V2Result<IntentId> {
    IntentRecord::derive_intent_id(
        anchor_identity,
        intent_generation,
        prev_head_digest,
        successor_head_digest,
    )
}

pub fn build_plan_binding_digest(
    intent_id: IntentId,
    intent_generation: u64,
    frame_sequence: u64,
    prev_head_digest: HeadDigest,
    successor_head_digest: HeadDigest,
    successor_event_digest: SuccessorEventDigest,
    canonical_successor_bytes: &[u8],
    plan_binding_witness: PlanBindingWitness,
) -> V2Result<PlanBindingDigest> {
    IntentRecord::derive_plan_binding_digest(
        intent_id,
        intent_generation,
        frame_sequence,
        prev_head_digest,
        successor_head_digest,
        successor_event_digest,
        canonical_successor_bytes,
        plan_binding_witness,
    )
}
