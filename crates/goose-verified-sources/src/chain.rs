use std::collections::BTreeMap;

use crate::canonical::Digest32;
use crate::crypto::verify_signature_quorum;
use crate::error::{ErrorCode, Result};
use crate::wire::{DocKind, Release, ReleaseKey, ReleaseState, State, VerifiedEnvelope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDocument {
    pub document_digest: Digest32,
    pub root_digest: Digest32,
    pub state_digest: Digest32,
    pub canonical_root: Vec<u8>,
    pub canonical_state: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupState {
    Absent,
    Verified(ResolvedDocument),
    Fork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingSequence {
    Absent,
    Duplicate,
    EquivalentVariant,
    Fork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceDisposition {
    Accepted,
    Duplicate,
    EquivalentVariant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainVerification {
    pub disposition: SequenceDisposition,
    pub parent: Option<ResolvedDocument>,
}

pub trait ChainLookup {
    fn lookup(&self, source_id: &str, sequence: u64) -> LookupState;
}

pub trait VerificationContext: ChainLookup {
    fn classify_existing(
        &self,
        source_id: &str,
        sequence: u64,
        document_digest: &Digest32,
        signature_set_digest: &Digest32,
    ) -> ExistingSequence;
}

pub fn verify_chain<C>(context: &C, document: &VerifiedEnvelope) -> Result<ChainVerification>
where
    C: VerificationContext,
{
    let payload = &document.envelope.payload;
    match payload.doc_kind {
        DocKind::Bootstrap => match context.lookup(&payload.source_id, 0) {
            LookupState::Absent => {}
            LookupState::Fork => return Err(ErrorCode::LookupFork),
            LookupState::Verified(_) => return Err(ErrorCode::BootstrapConflict),
        },
        DocKind::Snapshot | DocKind::Rotation | DocKind::Revocation => {
            let parent = match context.lookup(&payload.source_id, payload.sequence - 1) {
                LookupState::Absent => return Err(ErrorCode::MissingParent),
                LookupState::Fork => return Err(ErrorCode::LookupFork),
                LookupState::Verified(parent) => parent,
            };
            if payload.parent_document_digest != Some(parent.document_digest) {
                return Err(ErrorCode::ParentDigestMismatch);
            }
            match payload.doc_kind {
                DocKind::Snapshot => verify_snapshot(document, &parent)?,
                DocKind::Rotation => verify_rotation(document, &parent)?,
                DocKind::Revocation => verify_revocation(document, &parent)?,
                DocKind::Bootstrap => {}
            }
            let disposition = classify_current_sequence(context, document)?;
            return Ok(ChainVerification {
                disposition,
                parent: Some(parent),
            });
        }
    }

    let disposition = classify_current_sequence(context, document)?;
    Ok(ChainVerification {
        disposition,
        parent: None,
    })
}

fn classify_current_sequence<C>(
    context: &C,
    document: &VerifiedEnvelope,
) -> Result<SequenceDisposition>
where
    C: VerificationContext,
{
    match context.classify_existing(
        &document.envelope.payload.source_id,
        document.envelope.payload.sequence,
        &document.digests.document,
        &document.digests.signature_set,
    ) {
        ExistingSequence::Absent => Ok(SequenceDisposition::Accepted),
        ExistingSequence::Duplicate => Ok(SequenceDisposition::Duplicate),
        ExistingSequence::EquivalentVariant => Ok(SequenceDisposition::EquivalentVariant),
        ExistingSequence::Fork => Err(ErrorCode::SequenceFork),
    }
}

fn verify_snapshot(document: &VerifiedEnvelope, parent: &ResolvedDocument) -> Result<()> {
    if document.canonical_root != parent.canonical_root {
        return Err(ErrorCode::TransitionViolation);
    }
    let parent_state = State::parse_canonical_bytes(&parent.canonical_state)?;
    let current_state = &document.envelope.payload.state;
    let parent_map = releases_by_key(&parent_state.releases);
    let current_map = releases_by_key(&current_state.releases);

    for (key, parent_release) in &parent_map {
        if current_map.get(key) != Some(parent_release) {
            return Err(ErrorCode::TransitionViolation);
        }
    }
    for (key, current_release) in &current_map {
        if !parent_map.contains_key(key) && !current_release.is_active() {
            return Err(ErrorCode::TransitionViolation);
        }
    }
    Ok(())
}

fn verify_rotation(document: &VerifiedEnvelope, parent: &ResolvedDocument) -> Result<()> {
    let prior_root = document
        .envelope
        .payload
        .prior_root
        .as_ref()
        .ok_or(ErrorCode::TransitionViolation)?;
    if prior_root.to_canonical_json_bytes()? != parent.canonical_root {
        return Err(ErrorCode::TransitionViolation);
    }
    if document.canonical_root == parent.canonical_root {
        return Err(ErrorCode::TransitionViolation);
    }
    if !document
        .envelope
        .payload
        .root
        .spki_set()
        .is_disjoint(&prior_root.spki_set())
    {
        return Err(ErrorCode::TransitionViolation);
    }
    if document.canonical_state != parent.canonical_state {
        return Err(ErrorCode::TransitionViolation);
    }
    verify_signature_quorum(
        prior_root,
        &document.envelope.signatures,
        document.digests.signature_message,
    )?;
    Ok(())
}

fn verify_revocation(document: &VerifiedEnvelope, parent: &ResolvedDocument) -> Result<()> {
    if document.canonical_root != parent.canonical_root {
        return Err(ErrorCode::TransitionViolation);
    }
    let parent_state = State::parse_canonical_bytes(&parent.canonical_state)?;
    let current_state = &document.envelope.payload.state;
    if parent_state.releases.len() != current_state.releases.len() {
        return Err(ErrorCode::TransitionViolation);
    }
    let parent_map = releases_by_key(&parent_state.releases);
    let current_map = releases_by_key(&current_state.releases);
    if parent_map.len() != current_map.len() {
        return Err(ErrorCode::TransitionViolation);
    }

    let mut revoked_one = false;
    for (key, parent_release) in &parent_map {
        let current_release = current_map.get(key).ok_or(ErrorCode::TransitionViolation)?;
        if current_release == parent_release {
            continue;
        }
        if revoked_one {
            return Err(ErrorCode::TransitionViolation);
        }
        match (&parent_release.state, &current_release.state) {
            (
                ReleaseState::Active,
                ReleaseState::Revoked {
                    revoked_at_sequence,
                    ..
                },
            ) if *revoked_at_sequence == document.envelope.payload.sequence => {
                revoked_one = true;
            }
            _ => return Err(ErrorCode::TransitionViolation),
        }
    }
    if !revoked_one {
        return Err(ErrorCode::TransitionViolation);
    }
    Ok(())
}

fn releases_by_key(releases: &[Release]) -> BTreeMap<ReleaseKey, Release> {
    releases
        .iter()
        .cloned()
        .map(|release| (release.key(), release))
        .collect()
}
