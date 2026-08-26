mod codec;
mod encoding;
mod error;
mod model;
mod schema;
mod sidecar;
mod store;
mod types;

#[cfg(test)]
mod tests;

pub use codec::{
    build_genesis_record, build_intent_id, build_intent_record, build_plan_binding_digest,
    candidate_membership_digest, checkpoint_event_digest_from_head_core,
    checkpoint_event_from_head_core, content_candidate_root, decode_content_event,
    decode_genesis_head, decode_head_payload, decode_membership_row, decode_successor_head,
    encode_content_event, encode_genesis_head, encode_head_payload, encode_membership_row,
    encode_successor_head, head_digest_from_head_core, membership_digest, membership_row_digest,
};
pub use error::{
    FoundationWriteRecordKind, FoundationWriteResolution, FrameCorruption, V2Error, V2Result,
};
pub use schema::{
    actual_catalog_hash, apply_catalog, catalog_hash, catalog_projection_bytes,
    enforce_foreign_keys, manifest_hash, params_hash, require_current_catalog, schema_objects,
    SchemaObject, V2_SCHEMA_PARAMS, V2_SCHEMA_VERSION,
};
pub use sidecar::{
    decode_head_frame, encode_head_frame, DecodedSidecarFrame, SIDECAR_FRAME_MAGIC,
    SIDECAR_FRAME_VERSION,
};
pub use store::{AppendedContent, PersistedIntentPageEntry, PreparedCheckpoint, V2Store};
pub use types::{
    AnchorIdentity, AnchorInstanceId, CandidateMembership, CheckpointEvent, CheckpointEventDigest,
    CheckpointEventKind, CheckpointOperation, ContentEvent, ContentEventDigest, ContentRootDigest,
    GenesisDigest, GenesisHead, GenesisRecord, Head, HeadCore, HeadDigest, HeadLifecycle, IntentId,
    IntentRecord, IntentState, MemberDigest, OperationContentMembership, OperationId,
    PathBindingDigest, PlanBindingDigest, PlanBindingWitness, SuccessorEventDigest, SuccessorHead,
    EMPTY_ROOT, V23_PROTOCOL_VERSION,
};
