use std::collections::{BTreeMap, BTreeSet};

use super::encoding::{
    checked_usize_to_u64, decode_fixed_32, decode_optional_fixed_32, decode_optional_u64_field,
    decode_u64_field, decode_u8_field, sha256_domain, CanonicalReader, CanonicalWriter,
};
use super::error::{V2Error, V2Result};

macro_rules! fixed_bytes_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            pub const fn new(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            pub fn from_slice(bytes: &[u8]) -> V2Result<Self> {
                Ok(Self(decode_fixed_32(bytes, stringify!($name), "bytes")?))
            }

            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl From<[u8; 32]> for $name {
            fn from(value: [u8; 32]) -> Self {
                Self(value)
            }
        }
    };
}

fixed_bytes_type!(AnchorInstanceId);
fixed_bytes_type!(PathBindingDigest);
fixed_bytes_type!(PlanBindingDigest);
fixed_bytes_type!(OperationId);
fixed_bytes_type!(HeadDigest);
fixed_bytes_type!(CheckpointEventDigest);
fixed_bytes_type!(ContentEventDigest);
fixed_bytes_type!(MemberDigest);
fixed_bytes_type!(ContentRootDigest);
fixed_bytes_type!(GenesisDigest);
fixed_bytes_type!(SuccessorEventDigest);
fixed_bytes_type!(IntentId);

pub const EMPTY_ROOT: ContentRootDigest = ContentRootDigest::new([0u8; 32]);
pub const V23_PROTOCOL_VERSION: &str = "goose-evidence-db/v2.3-foundation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnchorIdentity {
    pub anchor_instance_id: AnchorInstanceId,
    pub path_binding: PathBindingDigest,
    pub key_epoch: u64,
}

impl AnchorIdentity {
    pub const CANONICAL_PREFIX_LEN: usize = 72;

    pub fn to_canonical_prefix_bytes(&self) -> [u8; Self::CANONICAL_PREFIX_LEN] {
        let mut out = [0u8; Self::CANONICAL_PREFIX_LEN];
        out[..32].copy_from_slice(self.anchor_instance_id.as_bytes());
        out[32..64].copy_from_slice(self.path_binding.as_bytes());
        out[64..72].copy_from_slice(&self.key_epoch.to_be_bytes());
        out
    }

    pub fn from_canonical_prefix_bytes(bytes: &[u8]) -> V2Result<Self> {
        if bytes.len() != Self::CANONICAL_PREFIX_LEN {
            return Err(V2Error::InvalidCanonicalLength {
                context: "AnchorIdentity",
                expected: Self::CANONICAL_PREFIX_LEN,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            anchor_instance_id: AnchorInstanceId::from_slice(&bytes[..32])?,
            path_binding: PathBindingDigest::from_slice(&bytes[32..64])?,
            key_epoch: u64::from_be_bytes(bytes[64..72].try_into().map_err(|_| {
                V2Error::InvalidCanonicalField {
                    context: "AnchorIdentity",
                    field: "key_epoch",
                }
            })?),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeadLifecycle {
    Stable,
    InFlight,
}

impl HeadLifecycle {
    fn tag(self) -> u8 {
        match self {
            Self::Stable => 1,
            Self::InFlight => 2,
        }
    }

    fn from_tag(tag: u8) -> V2Result<Self> {
        match tag {
            1 => Ok(Self::Stable),
            2 => Ok(Self::InFlight),
            _ => Err(V2Error::InvalidEnumTag {
                context: "HeadLifecycle",
                field: "lifecycle",
                tag,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckpointEventKind {
    Prepare,
    Pending,
    Finalize,
    Abort,
}

impl CheckpointEventKind {
    fn tag(self) -> u8 {
        match self {
            Self::Prepare => 1,
            Self::Pending => 2,
            Self::Finalize => 3,
            Self::Abort => 4,
        }
    }

    fn from_tag(tag: u8) -> V2Result<Self> {
        match tag {
            1 => Ok(Self::Prepare),
            2 => Ok(Self::Pending),
            3 => Ok(Self::Finalize),
            4 => Ok(Self::Abort),
            _ => Err(V2Error::InvalidEnumTag {
                context: "CheckpointEventKind",
                field: "phase",
                tag,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadCore {
    pub identity: AnchorIdentity,
    pub lifecycle: HeadLifecycle,
    pub phase: CheckpointEventKind,
    pub stable_generation: u64,
    pub content_generation: u64,
    pub operation_generation: u64,
    pub content_root: ContentRootDigest,
    pub content_freeze: u64,
    pub candidate_root: Option<ContentRootDigest>,
    pub candidate_freeze: Option<u64>,
    pub candidate_membership_digest: Option<MemberDigest>,
    pub operation_id: Option<OperationId>,
}

impl HeadCore {
    pub fn validate(&self) -> V2Result<()> {
        if self.content_generation < self.stable_generation
            || self.operation_generation < self.stable_generation
        {
            return Err(V2Error::InvalidGenerationOrder);
        }
        if let Some(candidate_freeze) = self.candidate_freeze {
            if candidate_freeze < self.content_freeze {
                return Err(V2Error::InvalidFreezeOrder);
            }
        }
        let has_candidate = self.candidate_root.is_some()
            && self.candidate_freeze.is_some()
            && self.candidate_membership_digest.is_some()
            && self.operation_id.is_some();
        let has_no_candidate = self.candidate_root.is_none()
            && self.candidate_freeze.is_none()
            && self.candidate_membership_digest.is_none()
            && self.operation_id.is_none();
        match (self.lifecycle, self.phase) {
            (HeadLifecycle::InFlight, CheckpointEventKind::Prepare)
            | (HeadLifecycle::InFlight, CheckpointEventKind::Pending)
                if has_candidate =>
            {
                Ok(())
            }
            (HeadLifecycle::Stable, CheckpointEventKind::Finalize)
            | (HeadLifecycle::Stable, CheckpointEventKind::Abort)
                if has_no_candidate && self.content_generation == self.stable_generation =>
            {
                Ok(())
            }
            _ => Err(V2Error::InvalidHeadCombination),
        }
    }

    pub fn canonical_bytes(&self) -> V2Result<Vec<u8>> {
        self.validate()?;
        let mut writer = CanonicalWriter::with_capacity(256);
        writer.field_bytes(1, &self.identity.to_canonical_prefix_bytes())?;
        writer.field_u8(2, self.lifecycle.tag())?;
        writer.field_u8(3, self.phase.tag())?;
        writer.field_u64(4, self.stable_generation)?;
        writer.field_u64(5, self.content_generation)?;
        writer.field_u64(6, self.operation_generation)?;
        writer.field_bytes(7, &self.content_root.as_bytes()[..])?;
        writer.field_u64(8, self.content_freeze)?;
        writer.field_optional_bytes(
            9,
            self.candidate_root
                .as_ref()
                .map(|value| &value.as_bytes()[..]),
        )?;
        writer.field_optional_u64(10, self.candidate_freeze)?;
        writer.field_optional_bytes(
            11,
            self.candidate_membership_digest
                .as_ref()
                .map(|value| &value.as_bytes()[..]),
        )?;
        writer.field_optional_bytes(
            12,
            self.operation_id
                .as_ref()
                .map(|value| &value.as_bytes()[..]),
        )?;
        Ok(writer.into_inner())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> V2Result<Self> {
        let mut reader = CanonicalReader::new("HeadCore", bytes);
        let identity = AnchorIdentity::from_canonical_prefix_bytes(reader.field(1, "identity")?)?;
        let lifecycle = HeadLifecycle::from_tag(decode_u8_field(
            reader.field(2, "lifecycle")?,
            "HeadCore",
            "lifecycle",
        )?)?;
        let phase = CheckpointEventKind::from_tag(decode_u8_field(
            reader.field(3, "phase")?,
            "HeadCore",
            "phase",
        )?)?;
        let head = Self {
            identity,
            lifecycle,
            phase,
            stable_generation: decode_u64_field(
                reader.field(4, "stable_generation")?,
                "HeadCore",
                "stable_generation",
            )?,
            content_generation: decode_u64_field(
                reader.field(5, "content_generation")?,
                "HeadCore",
                "content_generation",
            )?,
            operation_generation: decode_u64_field(
                reader.field(6, "operation_generation")?,
                "HeadCore",
                "operation_generation",
            )?,
            content_root: ContentRootDigest::from_slice(reader.field(7, "content_root")?)?,
            content_freeze: decode_u64_field(
                reader.field(8, "content_freeze")?,
                "HeadCore",
                "content_freeze",
            )?,
            candidate_root: decode_optional_fixed_32(
                reader.field(9, "candidate_root")?,
                "HeadCore",
                "candidate_root",
            )?
            .map(ContentRootDigest::new),
            candidate_freeze: decode_optional_u64_field(
                reader.field(10, "candidate_freeze")?,
                "HeadCore",
                "candidate_freeze",
            )?,
            candidate_membership_digest: decode_optional_fixed_32(
                reader.field(11, "candidate_membership_digest")?,
                "HeadCore",
                "candidate_membership_digest",
            )?
            .map(MemberDigest::new),
            operation_id: decode_optional_fixed_32(
                reader.field(12, "operation_id")?,
                "HeadCore",
                "operation_id",
            )?
            .map(OperationId::new),
        };
        reader.finish()?;
        head.validate()?;
        Ok(head)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointEvent {
    pub kind: CheckpointEventKind,
    pub prev_head_digest: Option<HeadDigest>,
    pub head_core: HeadCore,
}

impl CheckpointEvent {
    pub fn new(
        kind: CheckpointEventKind,
        prev_head_digest: Option<HeadDigest>,
        head_core: HeadCore,
    ) -> V2Result<Self> {
        head_core.validate()?;
        if kind != head_core.phase {
            return Err(V2Error::InvalidCheckpointEvent);
        }
        Ok(Self {
            kind,
            prev_head_digest,
            head_core,
        })
    }

    pub fn canonical_bytes(&self) -> V2Result<Vec<u8>> {
        let mut writer = CanonicalWriter::with_capacity(320);
        writer.field_u8(1, self.kind.tag())?;
        writer.field_optional_bytes(
            2,
            self.prev_head_digest
                .as_ref()
                .map(|value| &value.as_bytes()[..]),
        )?;
        writer.field_bytes(3, &self.head_core.canonical_bytes()?)?;
        Ok(writer.into_inner())
    }

    pub fn digest(&self) -> V2Result<CheckpointEventDigest> {
        Ok(CheckpointEventDigest::new(sha256_domain(
            "goose.evidence-db.v2.checkpoint-event",
            &[&self.canonical_bytes()?],
        )?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub core: HeadCore,
    pub terminal_checkpoint_digest: CheckpointEventDigest,
    pub prev_head_digest: Option<HeadDigest>,
}

impl Head {
    pub fn new(core: HeadCore, prev_head_digest: Option<HeadDigest>) -> V2Result<Self> {
        let terminal_checkpoint_digest =
            CheckpointEvent::new(core.phase, prev_head_digest, core.clone())?.digest()?;
        let head = Self {
            core,
            terminal_checkpoint_digest,
            prev_head_digest,
        };
        head.validate()?;
        Ok(head)
    }

    pub fn checkpoint_event(&self) -> V2Result<CheckpointEvent> {
        CheckpointEvent::new(self.core.phase, self.prev_head_digest, self.core.clone())
    }

    pub fn validate(&self) -> V2Result<()> {
        self.core.validate()?;
        let expected_digest = self.checkpoint_event()?.digest()?;
        if expected_digest != self.terminal_checkpoint_digest {
            return Err(V2Error::CheckpointDigestMismatch);
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> V2Result<Vec<u8>> {
        self.validate()?;
        let mut writer = CanonicalWriter::with_capacity(384);
        writer.field_bytes(1, &self.core.canonical_bytes()?)?;
        writer.field_bytes(2, &self.terminal_checkpoint_digest.as_bytes()[..])?;
        writer.field_optional_bytes(
            3,
            self.prev_head_digest
                .as_ref()
                .map(|value| &value.as_bytes()[..]),
        )?;
        Ok(writer.into_inner())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> V2Result<Self> {
        let mut reader = CanonicalReader::new("Head", bytes);
        let head = Self {
            core: HeadCore::from_canonical_bytes(reader.field(1, "core")?)?,
            terminal_checkpoint_digest: CheckpointEventDigest::from_slice(
                reader.field(2, "terminal_checkpoint_digest")?,
            )?,
            prev_head_digest: decode_optional_fixed_32(
                reader.field(3, "prev_head_digest")?,
                "Head",
                "prev_head_digest",
            )?
            .map(HeadDigest::new),
        };
        reader.finish()?;
        head.validate()?;
        Ok(head)
    }

    pub fn digest(&self) -> V2Result<HeadDigest> {
        Ok(HeadDigest::new(sha256_domain(
            "goose.evidence-db.v2.head",
            &[&self.canonical_bytes()?],
        )?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointOperation {
    pub operation_id: OperationId,
    pub operation_generation: u64,
    pub phase: CheckpointEventKind,
}

impl CheckpointOperation {
    pub fn validate_next_operation(&self, next: &Self) -> V2Result<()> {
        if next.operation_generation <= self.operation_generation {
            return Err(V2Error::InvalidGenerationOrder);
        }
        if self.phase == CheckpointEventKind::Abort && next.operation_id == self.operation_id {
            return Err(V2Error::AbortedOperationReuse);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentEvent {
    pub content_seq: u64,
    pub payload: Vec<u8>,
}

impl ContentEvent {
    pub fn canonical_bytes(&self) -> V2Result<Vec<u8>> {
        let mut writer = CanonicalWriter::with_capacity(self.payload.len().saturating_add(16));
        writer.field_u64(1, self.content_seq)?;
        writer.field_bytes(2, &self.payload)?;
        Ok(writer.into_inner())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> V2Result<Self> {
        let mut reader = CanonicalReader::new("ContentEvent", bytes);
        let event = Self {
            content_seq: decode_u64_field(
                reader.field(1, "content_seq")?,
                "ContentEvent",
                "content_seq",
            )?,
            payload: reader.field(2, "payload")?.to_vec(),
        };
        reader.finish()?;
        Ok(event)
    }

    pub fn digest(&self) -> V2Result<ContentEventDigest> {
        Ok(ContentEventDigest::new(sha256_domain(
            "goose.evidence-db.v2.content-event",
            &[&self.canonical_bytes()?],
        )?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationContentMembership {
    pub operation_id: OperationId,
    pub content_seq: u64,
    pub content_event_digest: ContentEventDigest,
}

impl OperationContentMembership {
    pub fn canonical_bytes(&self) -> V2Result<Vec<u8>> {
        let mut writer = CanonicalWriter::with_capacity(96);
        writer.field_bytes(1, &self.operation_id.as_bytes()[..])?;
        writer.field_u64(2, self.content_seq)?;
        writer.field_bytes(3, &self.content_event_digest.as_bytes()[..])?;
        Ok(writer.into_inner())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> V2Result<Self> {
        let mut reader = CanonicalReader::new("OperationContentMembership", bytes);
        let row = Self {
            operation_id: OperationId::from_slice(reader.field(1, "operation_id")?)?,
            content_seq: decode_u64_field(
                reader.field(2, "content_seq")?,
                "OperationContentMembership",
                "content_seq",
            )?,
            content_event_digest: ContentEventDigest::from_slice(
                reader.field(3, "content_event_digest")?,
            )?,
        };
        reader.finish()?;
        Ok(row)
    }

    pub fn membership_row_digest(&self) -> V2Result<MemberDigest> {
        Ok(MemberDigest::new(sha256_domain(
            "goose.evidence-db.v2.membership-row",
            &[&self.canonical_bytes()?],
        )?))
    }

    pub fn digest(&self) -> V2Result<MemberDigest> {
        self.membership_row_digest()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateMembership {
    pub operation_id: OperationId,
    pub rows: Vec<OperationContentMembership>,
}

impl CandidateMembership {
    pub fn new(operation_id: OperationId, rows: Vec<OperationContentMembership>) -> V2Result<Self> {
        let candidate = Self { operation_id, rows };
        candidate.validate()?;
        Ok(candidate)
    }

    pub fn validate(&self) -> V2Result<()> {
        let mut last_seq = None::<u64>;
        let mut digests = BTreeSet::new();
        for row in &self.rows {
            if row.operation_id != self.operation_id {
                return Err(V2Error::MembershipOperationMismatch);
            }
            if let Some(previous) = last_seq {
                if row.content_seq <= previous {
                    return Err(V2Error::DuplicateContentSeq {
                        content_seq: row.content_seq,
                    });
                }
            }
            if !digests.insert(row.content_event_digest) {
                return Err(V2Error::DuplicateContentEventDigest);
            }
            last_seq = Some(row.content_seq);
        }
        Ok(())
    }

    pub fn candidate_membership_digest(&self) -> V2Result<MemberDigest> {
        self.validate()?;
        let mut encoded_row_digests = Vec::with_capacity(self.rows.len().saturating_mul(32));
        for row in &self.rows {
            encoded_row_digests.extend_from_slice(row.membership_row_digest()?.as_bytes());
        }
        Ok(MemberDigest::new(sha256_domain(
            "goose.evidence-db.v2.candidate-membership",
            &[&self.operation_id.as_bytes()[..], &encoded_row_digests],
        )?))
    }

    pub fn membership_digest(&self) -> V2Result<MemberDigest> {
        self.candidate_membership_digest()
    }

    pub fn content_candidate_root(
        &self,
        content_events: &[ContentEvent],
    ) -> V2Result<ContentRootDigest> {
        self.validate()?;
        let mut event_digests = BTreeMap::new();
        for event in content_events {
            let digest = event.digest()?;
            if event_digests.insert(event.content_seq, digest).is_some() {
                return Err(V2Error::DuplicateContentSeq {
                    content_seq: event.content_seq,
                });
            }
        }
        let mut ordered = Vec::with_capacity(self.rows.len().saturating_mul(32));
        for row in &self.rows {
            let Some(digest) = event_digests.remove(&row.content_seq) else {
                return Err(V2Error::MissingContentEvent {
                    content_seq: row.content_seq,
                });
            };
            if digest != row.content_event_digest {
                return Err(V2Error::ContentEventDigestMismatch {
                    content_seq: row.content_seq,
                });
            }
            ordered.extend_from_slice(digest.as_bytes());
        }
        if !event_digests.is_empty() {
            return Err(V2Error::InvalidCanonicalField {
                context: "CandidateMembership",
                field: "unexpected_content_event",
            });
        }
        Ok(ContentRootDigest::new(sha256_domain(
            "goose.evidence-db.v2.content-candidate-root",
            &[&self.operation_id.as_bytes()[..], &ordered],
        )?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntentState {
    Authorized,
    Applied,
}

impl IntentState {
    fn tag(self) -> u8 {
        match self {
            Self::Authorized => 1,
            Self::Applied => 2,
        }
    }

    fn from_tag(tag: u8) -> V2Result<Self> {
        match tag {
            1 => Ok(Self::Authorized),
            2 => Ok(Self::Applied),
            _ => Err(V2Error::InvalidEnumTag {
                context: "IntentState",
                field: "intent_state",
                tag,
            }),
        }
    }

    pub fn as_sql(self) -> &'static str {
        match self {
            Self::Authorized => "authorized",
            Self::Applied => "applied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanBindingWitness {
    pub bound_operation_id: OperationId,
    pub bound_operation_generation: u64,
    pub predecessor_stable_generation: u64,
    pub predecessor_content_generation: u64,
    pub predecessor_content_freeze: u64,
    pub predecessor_candidate_freeze: u64,
    pub predecessor_stable_root: ContentRootDigest,
    pub predecessor_candidate_root: ContentRootDigest,
    pub predecessor_candidate_membership_digest: MemberDigest,
}

impl PlanBindingWitness {
    pub fn validate(&self) -> V2Result<()> {
        if self.bound_operation_generation == 0
            || self.predecessor_content_generation < self.predecessor_stable_generation
        {
            return Err(V2Error::InvalidGenerationOrder);
        }
        if self.predecessor_candidate_freeze < self.predecessor_content_freeze {
            return Err(V2Error::InvalidFreezeOrder);
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> V2Result<Vec<u8>> {
        self.validate()?;
        let mut writer = CanonicalWriter::with_capacity(256);
        writer.field_bytes(1, &self.bound_operation_id.as_bytes()[..])?;
        writer.field_u64(2, self.bound_operation_generation)?;
        writer.field_u64(3, self.predecessor_stable_generation)?;
        writer.field_u64(4, self.predecessor_content_generation)?;
        writer.field_u64(5, self.predecessor_content_freeze)?;
        writer.field_u64(6, self.predecessor_candidate_freeze)?;
        writer.field_bytes(7, &self.predecessor_stable_root.as_bytes()[..])?;
        writer.field_bytes(8, &self.predecessor_candidate_root.as_bytes()[..])?;
        writer.field_bytes(
            9,
            &self.predecessor_candidate_membership_digest.as_bytes()[..],
        )?;
        Ok(writer.into_inner())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> V2Result<Self> {
        let mut reader = CanonicalReader::new("PlanBindingWitness", bytes);
        let witness = Self {
            bound_operation_id: OperationId::from_slice(reader.field(1, "bound_operation_id")?)?,
            bound_operation_generation: decode_u64_field(
                reader.field(2, "bound_operation_generation")?,
                "PlanBindingWitness",
                "bound_operation_generation",
            )?,
            predecessor_stable_generation: decode_u64_field(
                reader.field(3, "predecessor_stable_generation")?,
                "PlanBindingWitness",
                "predecessor_stable_generation",
            )?,
            predecessor_content_generation: decode_u64_field(
                reader.field(4, "predecessor_content_generation")?,
                "PlanBindingWitness",
                "predecessor_content_generation",
            )?,
            predecessor_content_freeze: decode_u64_field(
                reader.field(5, "predecessor_content_freeze")?,
                "PlanBindingWitness",
                "predecessor_content_freeze",
            )?,
            predecessor_candidate_freeze: decode_u64_field(
                reader.field(6, "predecessor_candidate_freeze")?,
                "PlanBindingWitness",
                "predecessor_candidate_freeze",
            )?,
            predecessor_stable_root: ContentRootDigest::from_slice(
                reader.field(7, "predecessor_stable_root")?,
            )?,
            predecessor_candidate_root: ContentRootDigest::from_slice(
                reader.field(8, "predecessor_candidate_root")?,
            )?,
            predecessor_candidate_membership_digest: MemberDigest::from_slice(
                reader.field(9, "predecessor_candidate_membership_digest")?,
            )?,
        };
        reader.finish()?;
        witness.validate()?;
        Ok(witness)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisHead {
    pub anchor_instance_id: AnchorInstanceId,
}

impl GenesisHead {
    pub fn canonical_bytes(&self) -> V2Result<Vec<u8>> {
        let mut writer = CanonicalWriter::with_capacity(160);
        writer.field_bytes(1, V23_PROTOCOL_VERSION.as_bytes())?;
        writer.field_bytes(2, &self.anchor_instance_id.as_bytes()[..])?;
        writer.field_u64(3, 0)?;
        writer.field_u64(4, 0)?;
        writer.field_u64(5, 0)?;
        writer.field_bytes(6, &EMPTY_ROOT.as_bytes()[..])?;
        writer.field_bytes(7, &EMPTY_ROOT.as_bytes()[..])?;
        writer.field_optional_bytes(8, None)?;
        writer.field_optional_bytes(9, None)?;
        writer.field_optional_bytes(10, None)?;
        writer.field_optional_u64(11, None)?;
        writer.field_optional_bytes(12, None)?;
        Ok(writer.into_inner())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> V2Result<Self> {
        let mut reader = CanonicalReader::new("GenesisHead", bytes);
        let protocol_version = reader.field(1, "protocol_version")?;
        if protocol_version != V23_PROTOCOL_VERSION.as_bytes() {
            return Err(V2Error::InvalidProtocolVersion {
                context: "GenesisHead",
            });
        }
        let anchor_instance_id =
            AnchorInstanceId::from_slice(reader.field(2, "anchor_instance_id")?)?;
        let stable_generation = decode_u64_field(
            reader.field(3, "stable_generation")?,
            "GenesisHead",
            "stable_generation",
        )?;
        let content_generation = decode_u64_field(
            reader.field(4, "content_generation")?,
            "GenesisHead",
            "content_generation",
        )?;
        let work_generation = decode_u64_field(
            reader.field(5, "work_generation")?,
            "GenesisHead",
            "work_generation",
        )?;
        let stable_root = ContentRootDigest::from_slice(reader.field(6, "stable_root")?)?;
        let content_root = ContentRootDigest::from_slice(reader.field(7, "content_root")?)?;
        let candidate_root = decode_optional_fixed_32(
            reader.field(8, "candidate_root")?,
            "GenesisHead",
            "candidate_root",
        )?;
        let operation_id = decode_optional_fixed_32(
            reader.field(9, "operation_id")?,
            "GenesisHead",
            "operation_id",
        )?;
        let predecessor_head_digest = decode_optional_fixed_32(
            reader.field(10, "predecessor_head_digest")?,
            "GenesisHead",
            "predecessor_head_digest",
        )?;
        let predecessor_candidate_freeze = decode_optional_u64_field(
            reader.field(11, "predecessor_candidate_freeze")?,
            "GenesisHead",
            "predecessor_candidate_freeze",
        )?;
        let plan_binding_digest = decode_optional_fixed_32(
            reader.field(12, "plan_binding_digest")?,
            "GenesisHead",
            "plan_binding_digest",
        )?;
        reader.finish()?;

        if stable_generation != 0
            || content_generation != 0
            || work_generation != 0
            || stable_root != EMPTY_ROOT
            || content_root != EMPTY_ROOT
            || candidate_root.is_some()
            || operation_id.is_some()
            || predecessor_head_digest.is_some()
            || predecessor_candidate_freeze.is_some()
            || plan_binding_digest.is_some()
        {
            return Err(V2Error::GenesisCanonicalMismatch);
        }

        Ok(Self { anchor_instance_id })
    }

    pub fn digest(&self) -> V2Result<GenesisDigest> {
        Ok(GenesisDigest::new(sha256_domain(
            "goose.evidence-db.v2.3.genesis-head",
            &[&self.canonical_bytes()?],
        )?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisRecord {
    pub protocol_version: String,
    pub anchor_instance_id: AnchorInstanceId,
    pub canonical_genesis_bytes: Vec<u8>,
    pub canonical_genesis_len: u64,
    pub genesis_digest: GenesisDigest,
    pub stable_generation: u64,
    pub content_generation: u64,
    pub work_generation: u64,
    pub stable_root: ContentRootDigest,
    pub content_root: ContentRootDigest,
    pub candidate_root: Option<ContentRootDigest>,
    pub operation_id: Option<OperationId>,
    pub predecessor_head_digest: Option<HeadDigest>,
    pub predecessor_candidate_freeze: Option<u64>,
    pub plan_binding_digest: Option<PlanBindingDigest>,
}

impl GenesisRecord {
    pub fn new(anchor_instance_id: AnchorInstanceId) -> V2Result<Self> {
        let head = GenesisHead { anchor_instance_id };
        let canonical_genesis_bytes = head.canonical_bytes()?;
        let canonical_genesis_len = checked_usize_to_u64(canonical_genesis_bytes.len())?;
        Ok(Self {
            protocol_version: V23_PROTOCOL_VERSION.to_owned(),
            anchor_instance_id,
            canonical_genesis_len,
            canonical_genesis_bytes,
            genesis_digest: head.digest()?,
            stable_generation: 0,
            content_generation: 0,
            work_generation: 0,
            stable_root: EMPTY_ROOT,
            content_root: EMPTY_ROOT,
            candidate_root: None,
            operation_id: None,
            predecessor_head_digest: None,
            predecessor_candidate_freeze: None,
            plan_binding_digest: None,
        })
    }

    pub fn validate(&self) -> V2Result<()> {
        if self.protocol_version != V23_PROTOCOL_VERSION {
            return Err(V2Error::InvalidProtocolVersion {
                context: "GenesisRecord",
            });
        }
        if self.canonical_genesis_len != checked_usize_to_u64(self.canonical_genesis_bytes.len())? {
            return Err(V2Error::InvalidCanonicalLength {
                context: "GenesisRecord",
                expected: usize::try_from(self.canonical_genesis_len)
                    .unwrap_or(self.canonical_genesis_bytes.len()),
                actual: self.canonical_genesis_bytes.len(),
            });
        }
        if self.stable_generation != 0
            || self.content_generation != 0
            || self.work_generation != 0
            || self.stable_root != EMPTY_ROOT
            || self.content_root != EMPTY_ROOT
            || self.candidate_root.is_some()
            || self.operation_id.is_some()
            || self.predecessor_head_digest.is_some()
            || self.predecessor_candidate_freeze.is_some()
            || self.plan_binding_digest.is_some()
        {
            return Err(V2Error::GenesisCanonicalMismatch);
        }

        let expected = GenesisHead {
            anchor_instance_id: self.anchor_instance_id,
        };
        let canonical_genesis_bytes = expected.canonical_bytes()?;
        if self.canonical_genesis_bytes != canonical_genesis_bytes {
            return Err(V2Error::GenesisCanonicalMismatch);
        }
        if self.genesis_digest != expected.digest()? {
            return Err(V2Error::GenesisDigestMismatch);
        }
        GenesisHead::from_canonical_bytes(&self.canonical_genesis_bytes)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessorHead {
    pub anchor_identity: AnchorIdentity,
    pub prev_head_digest: HeadDigest,
    pub intent_generation: u64,
    pub frame_sequence: u64,
    pub predecessor_head_digest: HeadDigest,
    pub plan_binding_witness: PlanBindingWitness,
    pub state: IntentState,
    pub stable_root: ContentRootDigest,
    pub content_root: ContentRootDigest,
    pub event_value: Vec<u8>,
}

impl SuccessorHead {
    pub fn validate(&self) -> V2Result<()> {
        if self.intent_generation == 0 {
            return Err(V2Error::InvalidIntentGeneration);
        }
        if self.frame_sequence == 0 {
            return Err(V2Error::InvalidFrameSequence);
        }
        self.plan_binding_witness.validate()?;
        if self.event_value.is_empty() {
            return Err(V2Error::MissingSuccessorEventValue);
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> V2Result<Vec<u8>> {
        self.validate()?;
        let mut writer = CanonicalWriter::with_capacity(self.event_value.len().saturating_add(384));
        writer.field_bytes(1, V23_PROTOCOL_VERSION.as_bytes())?;
        writer.field_bytes(2, &self.anchor_identity.to_canonical_prefix_bytes())?;
        writer.field_bytes(3, &self.prev_head_digest.as_bytes()[..])?;
        writer.field_u64(4, self.intent_generation)?;
        writer.field_u64(5, self.frame_sequence)?;
        writer.field_bytes(6, &self.predecessor_head_digest.as_bytes()[..])?;
        writer.field_bytes(7, &self.plan_binding_witness.canonical_bytes()?)?;
        writer.field_u8(8, self.state.tag())?;
        writer.field_bytes(9, &self.stable_root.as_bytes()[..])?;
        writer.field_bytes(10, &self.content_root.as_bytes()[..])?;
        writer.field_bytes(11, &self.event_value)?;
        Ok(writer.into_inner())
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> V2Result<Self> {
        let parsed = ParsedSuccessorHead::from_canonical_bytes(bytes)?;
        Ok(Self {
            anchor_identity: parsed.anchor_identity,
            prev_head_digest: parsed.prev_head_digest,
            intent_generation: parsed.intent_generation,
            frame_sequence: parsed.frame_sequence,
            predecessor_head_digest: parsed.predecessor_head_digest,
            plan_binding_witness: parsed.plan_binding_witness,
            state: parsed.state,
            stable_root: parsed.stable_root,
            content_root: parsed.content_root,
            event_value: parsed.event_value.to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRecord {
    pub protocol_version: String,
    pub canonical_successor_bytes: Vec<u8>,
    pub canonical_successor_len: u64,
    pub successor_head_digest: HeadDigest,
    pub successor_event_digest: SuccessorEventDigest,
    pub event_value_offset: u64,
    pub event_value_len: u64,
    pub anchor_identity: AnchorIdentity,
    pub prev_head_digest: HeadDigest,
    pub intent_generation: u64,
    pub frame_sequence: u64,
    pub intent_id: IntentId,
    pub predecessor_head_digest: HeadDigest,
    pub plan_binding_witness: PlanBindingWitness,
    pub plan_binding_digest: PlanBindingDigest,
    pub state: IntentState,
    pub stable_root: ContentRootDigest,
    pub content_root: ContentRootDigest,
}

impl IntentRecord {
    pub fn derive_intent_id(
        anchor_identity: AnchorIdentity,
        intent_generation: u64,
        prev_head_digest: HeadDigest,
        successor_head_digest: HeadDigest,
    ) -> V2Result<IntentId> {
        let mut writer = CanonicalWriter::with_capacity(160);
        writer.field_bytes(1, &anchor_identity.to_canonical_prefix_bytes())?;
        writer.field_u64(2, intent_generation)?;
        writer.field_bytes(3, &prev_head_digest.as_bytes()[..])?;
        writer.field_bytes(4, &successor_head_digest.as_bytes()[..])?;
        Ok(IntentId::new(sha256_domain(
            "goose.evidence-db.v2.3.intent-id",
            &[&writer.into_inner()],
        )?))
    }

    pub fn derive_plan_binding_digest(
        intent_id: IntentId,
        intent_generation: u64,
        frame_sequence: u64,
        prev_head_digest: HeadDigest,
        successor_head_digest: HeadDigest,
        successor_event_digest: SuccessorEventDigest,
        canonical_successor_bytes: &[u8],
        plan_binding_witness: PlanBindingWitness,
    ) -> V2Result<PlanBindingDigest> {
        plan_binding_witness.validate()?;
        let witness_bytes = plan_binding_witness.canonical_bytes()?;
        let mut writer =
            CanonicalWriter::with_capacity(canonical_successor_bytes.len().saturating_add(512));
        writer.field_bytes(1, &intent_id.as_bytes()[..])?;
        writer.field_u64(2, intent_generation)?;
        writer.field_u64(3, frame_sequence)?;
        writer.field_bytes(4, &prev_head_digest.as_bytes()[..])?;
        writer.field_bytes(5, &successor_head_digest.as_bytes()[..])?;
        writer.field_bytes(6, &successor_event_digest.as_bytes()[..])?;
        writer.field_bytes(7, canonical_successor_bytes)?;
        writer.field_bytes(8, &witness_bytes)?;
        Ok(PlanBindingDigest::new(sha256_domain(
            "goose.evidence-db.v2.3.plan-binding",
            &[&writer.into_inner()],
        )?))
    }

    pub fn new(successor: &SuccessorHead) -> V2Result<Self> {
        let canonical_successor_bytes = successor.canonical_bytes()?;
        let canonical_successor_len = checked_usize_to_u64(canonical_successor_bytes.len())?;
        let parsed = ParsedSuccessorHead::from_canonical_bytes(&canonical_successor_bytes)?;
        let successor_head_digest = parsed.head_digest()?;
        let successor_event_digest = parsed.event_digest()?;
        let event_value_offset = checked_usize_to_u64(parsed.event_value_offset)?;
        let event_value_len = checked_usize_to_u64(parsed.event_value_len)?;
        let anchor_identity = parsed.anchor_identity;
        let prev_head_digest = parsed.prev_head_digest;
        let intent_generation = parsed.intent_generation;
        let frame_sequence = parsed.frame_sequence;
        let intent_id = parsed.intent_id(successor_head_digest)?;
        let predecessor_head_digest = parsed.predecessor_head_digest;
        let plan_binding_witness = parsed.plan_binding_witness;
        let plan_binding_digest =
            parsed.plan_binding_digest(intent_id, successor_head_digest, successor_event_digest)?;
        let state = parsed.state;
        let stable_root = parsed.stable_root;
        let content_root = parsed.content_root;
        Ok(Self {
            protocol_version: V23_PROTOCOL_VERSION.to_owned(),
            canonical_successor_len,
            successor_head_digest,
            successor_event_digest,
            event_value_offset,
            event_value_len,
            canonical_successor_bytes,
            anchor_identity,
            prev_head_digest,
            intent_generation,
            frame_sequence,
            intent_id,
            predecessor_head_digest,
            plan_binding_witness,
            plan_binding_digest,
            state,
            stable_root,
            content_root,
        })
    }

    pub fn validate(&self) -> V2Result<()> {
        if self.protocol_version != V23_PROTOCOL_VERSION {
            return Err(V2Error::InvalidProtocolVersion {
                context: "IntentRecord",
            });
        }
        if self.intent_generation == 0 {
            return Err(V2Error::InvalidIntentGeneration);
        }
        if self.frame_sequence == 0 {
            return Err(V2Error::InvalidFrameSequence);
        }
        if self.canonical_successor_len
            != checked_usize_to_u64(self.canonical_successor_bytes.len())?
        {
            return Err(V2Error::InvalidCanonicalLength {
                context: "IntentRecord",
                expected: usize::try_from(self.canonical_successor_len)
                    .unwrap_or(self.canonical_successor_bytes.len()),
                actual: self.canonical_successor_bytes.len(),
            });
        }

        let parsed = ParsedSuccessorHead::from_canonical_bytes(&self.canonical_successor_bytes)?;
        if self.anchor_identity != parsed.anchor_identity
            || self.prev_head_digest != parsed.prev_head_digest
            || self.intent_generation != parsed.intent_generation
            || self.frame_sequence != parsed.frame_sequence
            || self.predecessor_head_digest != parsed.predecessor_head_digest
            || self.plan_binding_witness != parsed.plan_binding_witness
            || self.state != parsed.state
            || self.stable_root != parsed.stable_root
            || self.content_root != parsed.content_root
        {
            return Err(V2Error::InvalidCanonicalField {
                context: "IntentRecord",
                field: "canonical_successor_bytes",
            });
        }
        if self.successor_head_digest != parsed.head_digest()? {
            return Err(V2Error::SuccessorHeadDigestMismatch);
        }
        if self.successor_event_digest != parsed.event_digest()? {
            return Err(V2Error::SuccessorEventDigestMismatch);
        }
        let expected_intent_id = parsed.intent_id(self.successor_head_digest)?;
        if self.intent_id != expected_intent_id {
            return Err(V2Error::IntentIdMismatch);
        }
        let expected_plan_binding_digest = parsed.plan_binding_digest(
            expected_intent_id,
            self.successor_head_digest,
            self.successor_event_digest,
        )?;
        if self.plan_binding_digest != expected_plan_binding_digest {
            return Err(V2Error::PlanBindingDigestMismatch);
        }
        if self.event_value_offset != checked_usize_to_u64(parsed.event_value_offset)?
            || self.event_value_len != checked_usize_to_u64(parsed.event_value_len)?
        {
            return Err(V2Error::SuccessorEventSliceMismatch);
        }
        Ok(())
    }
}

struct ParsedSuccessorHead<'a> {
    canonical_bytes: &'a [u8],
    anchor_identity: AnchorIdentity,
    prev_head_digest: HeadDigest,
    intent_generation: u64,
    frame_sequence: u64,
    predecessor_head_digest: HeadDigest,
    plan_binding_witness: PlanBindingWitness,
    state: IntentState,
    stable_root: ContentRootDigest,
    content_root: ContentRootDigest,
    event_value: &'a [u8],
    event_value_offset: usize,
    event_value_len: usize,
}

impl<'a> ParsedSuccessorHead<'a> {
    fn from_canonical_bytes(bytes: &'a [u8]) -> V2Result<Self> {
        let mut reader = CanonicalReader::new("SuccessorHead", bytes);
        let protocol_version = reader.field(1, "protocol_version")?;
        if protocol_version != V23_PROTOCOL_VERSION.as_bytes() {
            return Err(V2Error::InvalidProtocolVersion {
                context: "SuccessorHead",
            });
        }

        let anchor_identity =
            AnchorIdentity::from_canonical_prefix_bytes(reader.field(2, "anchor_identity")?)?;
        let prev_head_digest = HeadDigest::from_slice(reader.field(3, "prev_head_digest")?)?;
        let intent_generation = decode_u64_field(
            reader.field(4, "intent_generation")?,
            "SuccessorHead",
            "intent_generation",
        )?;
        let frame_sequence = decode_u64_field(
            reader.field(5, "frame_sequence")?,
            "SuccessorHead",
            "frame_sequence",
        )?;
        let predecessor_head_digest =
            HeadDigest::from_slice(reader.field(6, "predecessor_head_digest")?)?;
        let plan_binding_witness =
            PlanBindingWitness::from_canonical_bytes(reader.field(7, "plan_binding_witness")?)?;
        let state = IntentState::from_tag(decode_u8_field(
            reader.field(8, "intent_state")?,
            "SuccessorHead",
            "intent_state",
        )?)?;
        let stable_root = ContentRootDigest::from_slice(reader.field(9, "stable_root")?)?;
        let content_root = ContentRootDigest::from_slice(reader.field(10, "content_root")?)?;
        let event_value = reader.field_with_range(11, "event_value")?;
        reader.finish()?;

        if intent_generation == 0 {
            return Err(V2Error::InvalidIntentGeneration);
        }
        if frame_sequence == 0 {
            return Err(V2Error::InvalidFrameSequence);
        }
        if event_value.value.is_empty() {
            return Err(V2Error::MissingSuccessorEventValue);
        }

        Ok(Self {
            canonical_bytes: bytes,
            anchor_identity,
            prev_head_digest,
            intent_generation,
            frame_sequence,
            predecessor_head_digest,
            plan_binding_witness,
            state,
            stable_root,
            content_root,
            event_value: event_value.value,
            event_value_offset: event_value.value_offset,
            event_value_len: event_value.value_len,
        })
    }

    fn head_digest(&self) -> V2Result<HeadDigest> {
        Ok(HeadDigest::new(sha256_domain(
            "goose.evidence-db.v2.3.successor-head",
            &[self.canonical_bytes()],
        )?))
    }

    fn event_digest(&self) -> V2Result<SuccessorEventDigest> {
        Ok(SuccessorEventDigest::new(sha256_domain(
            "goose.evidence-db.v2.3.successor-event",
            &[self.event_value],
        )?))
    }

    fn intent_id(&self, successor_head_digest: HeadDigest) -> V2Result<IntentId> {
        IntentRecord::derive_intent_id(
            self.anchor_identity,
            self.intent_generation,
            self.prev_head_digest,
            successor_head_digest,
        )
    }

    fn plan_binding_digest(
        &self,
        intent_id: IntentId,
        successor_head_digest: HeadDigest,
        successor_event_digest: SuccessorEventDigest,
    ) -> V2Result<PlanBindingDigest> {
        IntentRecord::derive_plan_binding_digest(
            intent_id,
            self.intent_generation,
            self.frame_sequence,
            self.prev_head_digest,
            successor_head_digest,
            successor_event_digest,
            self.canonical_bytes(),
            self.plan_binding_witness,
        )
    }

    fn canonical_bytes(&self) -> &'a [u8] {
        self.canonical_bytes
    }
}
