use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCorruption {
    BadMagic,
    UnsupportedVersion,
    InvalidLength,
    PayloadDigestMismatch,
    CrcMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundationWriteRecordKind {
    Genesis,
    Intent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundationWriteResolution {
    Persisted,
    Missing,
    Corrupt,
}

#[derive(Debug, Error)]
pub enum V2Error {
    #[error("v2_database_not_found")]
    DatabaseNotFound,
    #[error("v2_database_already_initialized")]
    DatabaseAlreadyInitialized,
    #[error("v2_unsupported_platform")]
    UnsupportedPlatform,
    #[error("v2_unsupported_filesystem")]
    UnsupportedFilesystem,
    #[error("v2_invalid_path_binding")]
    InvalidPathBinding,
    #[error("v2_path_reparse_rejected")]
    PathReparseRejected,
    #[error("v2_hardlink_rejected")]
    HardlinkRejected,
    #[error("v2_numeric_overflow")]
    NumericOverflow,
    #[error("v2_invalid_canonical_length:{context}:{expected}:{actual}")]
    InvalidCanonicalLength {
        context: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("v2_invalid_canonical_field:{context}:{field}")]
    InvalidCanonicalField {
        context: &'static str,
        field: &'static str,
    },
    #[error("v2_invalid_enum_tag:{context}:{field}:{tag}")]
    InvalidEnumTag {
        context: &'static str,
        field: &'static str,
        tag: u8,
    },
    #[error("v2_invalid_head_combination")]
    InvalidHeadCombination,
    #[error("v2_invalid_checkpoint_event")]
    InvalidCheckpointEvent,
    #[error("v2_invalid_protocol_version:{context}")]
    InvalidProtocolVersion { context: &'static str },
    #[error("v2_invalid_generation_order")]
    InvalidGenerationOrder,
    #[error("v2_invalid_freeze_order")]
    InvalidFreezeOrder,
    #[error("v2_invalid_intent_generation")]
    InvalidIntentGeneration,
    #[error("v2_invalid_frame_sequence")]
    InvalidFrameSequence,
    #[error("v2_missing_intent_predecessor")]
    MissingIntentPredecessor,
    #[error("v2_missing_successor_event_value")]
    MissingSuccessorEventValue,
    #[error("v2_intent_id_mismatch")]
    IntentIdMismatch,
    #[error("v2_genesis_canonical_mismatch")]
    GenesisCanonicalMismatch,
    #[error("v2_genesis_digest_mismatch")]
    GenesisDigestMismatch,
    #[error("v2_plan_binding_digest_mismatch")]
    PlanBindingDigestMismatch,
    #[error("v2_successor_head_digest_mismatch")]
    SuccessorHeadDigestMismatch,
    #[error("v2_successor_event_digest_mismatch")]
    SuccessorEventDigestMismatch,
    #[error("v2_successor_event_slice_mismatch")]
    SuccessorEventSliceMismatch,
    #[error("v2_duplicate_content_seq:{content_seq}")]
    DuplicateContentSeq { content_seq: u64 },
    #[error("v2_duplicate_content_event_digest")]
    DuplicateContentEventDigest,
    #[error("v2_membership_operation_mismatch")]
    MembershipOperationMismatch,
    #[error("v2_missing_content_event:{content_seq}")]
    MissingContentEvent { content_seq: u64 },
    #[error("v2_content_event_digest_mismatch:{content_seq}")]
    ContentEventDigestMismatch { content_seq: u64 },
    #[error("v2_aborted_operation_reuse")]
    AbortedOperationReuse,
    #[error("v2_checkpoint_digest_mismatch")]
    CheckpointDigestMismatch,
    #[error("v2_frame_incomplete_tail:{expected_len}:{actual_len}")]
    IncompleteTailFrame { expected_len: u64, actual_len: u64 },
    #[error("v2_frame_corrupt:{reason:?}")]
    CorruptFrame { reason: FrameCorruption },
    #[error("v2_unexpected_complete_frame:{frame_len}:{actual_len}")]
    UnexpectedCompleteFrame { frame_len: u64, actual_len: u64 },
    #[error("v2_offline_migration_required")]
    OfflineMigrationRequired {
        found_version: Option<String>,
        found_params: Option<String>,
    },
    #[error("v2_legacy_catalog_rejected")]
    LegacyCatalogRejected,
    #[error("v2_schema_fence_mismatch")]
    SchemaFenceMismatch,
    #[error("v2_missing_schema_meta")]
    MissingSchemaMeta,
    #[error("v2_sqlite_foreign_keys_disabled")]
    ForeignKeysDisabled,
    #[error("v2_empty_content_payload")]
    EmptyContentPayload,
    #[error("v2_invalid_content_payload_length:{len}")]
    InvalidContentPayloadLength { len: u64 },
    #[error("v2_content_allocator_rewind:{next_content_seq}:{max_content_seq}")]
    ContentAllocatorRewind {
        next_content_seq: u64,
        max_content_seq: u64,
    },
    #[error("v2_recovery_required:{phase}")]
    RecoveryRequired { phase: &'static str },
    #[error("v2_no_eligible_content:{stable_generation}:{visible_frontier}")]
    NoEligibleContent {
        stable_generation: u64,
        visible_frontier: u64,
    },
    #[error("v2_anchor_identity_mismatch")]
    AnchorIdentityMismatch,
    #[error("v2_store_unavailable_after_connection_loss")]
    StoreUnavailableAfterConnectionLoss,
    #[error("v2_sqlite_handle_unavailable:{context}")]
    SqliteHandleUnavailable { context: &'static str },
    #[error("v2_authority_invariant_violation:{context}")]
    AuthorityInvariantViolation { context: &'static str },
    #[error("v2_foundation_write_commit_unknown:{kind:?}:{resolution:?}")]
    FoundationWriteCommitUnknown {
        kind: FoundationWriteRecordKind,
        resolution: FoundationWriteResolution,
    },
    #[error("v2_foundation_write_rollback_unknown:{kind:?}:{resolution:?}")]
    FoundationWriteRollbackUnknown {
        kind: FoundationWriteRecordKind,
        resolution: FoundationWriteResolution,
    },
    #[error("v2_append_content_commit_unknown")]
    AppendContentCommitUnknown,
    #[error("v2_append_content_rollback_unknown")]
    AppendContentRollbackUnknown,
    #[error("v2_prepare_commit_unknown")]
    PrepareCommitUnknown,
    #[error("v2_prepare_rollback_unknown")]
    PrepareRollbackUnknown,
    #[error("v2_invalid_persisted_intent_limit:{limit}")]
    InvalidPersistedIntentLimit { limit: u64 },
    #[error("v2_io:{0}")]
    Io(#[from] std::io::Error),
    #[error("v2_sqlite:{0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub type V2Result<T> = Result<T, V2Error>;
