use super::encoding::{checked_u32_len, checked_usize_to_u64, sha256_domain};
use super::error::{FrameCorruption, V2Error, V2Result};
use super::model::Head;

pub const SIDECAR_FRAME_MAGIC: [u8; 8] = *b"GEV2HDR\0";
pub const SIDECAR_FRAME_VERSION: u16 = 1;

const HEADER_LEN: usize = 8 + 2 + 4;
const FOOTER_LEN: usize = 32 + 4;
const MIN_FRAME_LEN: usize = HEADER_LEN + FOOTER_LEN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSidecarFrame {
    pub head: Head,
    pub payload_digest: [u8; 32],
    pub crc32c: u32,
    pub encoded_len: usize,
}

pub fn encode_head_frame(head: &Head) -> V2Result<Vec<u8>> {
    let payload = head.canonical_bytes()?;
    let payload_digest = sha256_domain("lumina.evidence-db.v2.sidecar-payload", &[&payload])?;
    let payload_len = checked_u32_len(payload.len())?;
    let total_len = HEADER_LEN
        .checked_add(payload.len())
        .and_then(|value| value.checked_add(FOOTER_LEN))
        .ok_or(V2Error::NumericOverflow)?;
    let mut frame = Vec::with_capacity(total_len);
    frame.extend_from_slice(&SIDECAR_FRAME_MAGIC);
    frame.extend_from_slice(&SIDECAR_FRAME_VERSION.to_be_bytes());
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(&payload_digest);
    let crc32c = crc32c(&frame);
    frame.extend_from_slice(&crc32c.to_be_bytes());
    Ok(frame)
}

pub fn decode_head_frame(bytes: &[u8]) -> V2Result<DecodedSidecarFrame> {
    if bytes.len() < MIN_FRAME_LEN {
        return Err(V2Error::IncompleteTailFrame {
            expected_len: checked_usize_to_u64(MIN_FRAME_LEN)?,
            actual_len: checked_usize_to_u64(bytes.len())?,
        });
    }
    if bytes[..8] != SIDECAR_FRAME_MAGIC {
        return Err(V2Error::CorruptFrame {
            reason: FrameCorruption::BadMagic,
        });
    }
    let version =
        u16::from_be_bytes(bytes[8..10].try_into().map_err(|_| V2Error::CorruptFrame {
            reason: FrameCorruption::BadMagic,
        })?);
    if version != SIDECAR_FRAME_VERSION {
        return Err(V2Error::CorruptFrame {
            reason: FrameCorruption::UnsupportedVersion,
        });
    }
    let payload_len =
        u32::from_be_bytes(
            bytes[10..14]
                .try_into()
                .map_err(|_| V2Error::CorruptFrame {
                    reason: FrameCorruption::BadMagic,
                })?,
        );
    let payload_len = usize::try_from(payload_len).map_err(|_| V2Error::NumericOverflow)?;
    let expected_len = HEADER_LEN
        .checked_add(payload_len)
        .and_then(|value| value.checked_add(FOOTER_LEN))
        .ok_or(V2Error::NumericOverflow)?;
    if bytes.len() < expected_len {
        if tail_footer_is_complete(bytes)? {
            return Err(V2Error::CorruptFrame {
                reason: FrameCorruption::InvalidLength,
            });
        }
        return Err(V2Error::IncompleteTailFrame {
            expected_len: checked_usize_to_u64(expected_len)?,
            actual_len: checked_usize_to_u64(bytes.len())?,
        });
    }

    let payload_range = HEADER_LEN..HEADER_LEN + payload_len;
    let digest_range = payload_range.end..payload_range.end + 32;
    let crc_range = digest_range.end..digest_range.end + 4;
    let payload = &bytes[payload_range];
    let payload_digest = &bytes[digest_range.clone()];
    let expected_payload_digest =
        sha256_domain("lumina.evidence-db.v2.sidecar-payload", &[payload])?;
    if payload_digest != expected_payload_digest {
        return Err(V2Error::CorruptFrame {
            reason: FrameCorruption::PayloadDigestMismatch,
        });
    }

    let crc32c = u32::from_be_bytes(bytes[crc_range.clone()].try_into().map_err(|_| {
        V2Error::CorruptFrame {
            reason: FrameCorruption::CrcMismatch,
        }
    })?);
    let actual_crc32c = crc32c(&bytes[..crc_range.start]);
    if crc32c != actual_crc32c {
        return Err(V2Error::CorruptFrame {
            reason: FrameCorruption::CrcMismatch,
        });
    }

    if bytes.len() > expected_len {
        return Err(V2Error::UnexpectedCompleteFrame {
            frame_len: checked_usize_to_u64(expected_len)?,
            actual_len: checked_usize_to_u64(bytes.len())?,
        });
    }

    Ok(DecodedSidecarFrame {
        head: Head::from_canonical_bytes(payload)?,
        payload_digest: expected_payload_digest,
        crc32c,
        encoded_len: expected_len,
    })
}

fn tail_footer_is_complete(bytes: &[u8]) -> V2Result<bool> {
    if bytes.len() < MIN_FRAME_LEN {
        return Ok(false);
    }

    let payload_end = bytes.len() - FOOTER_LEN;
    let payload = &bytes[HEADER_LEN..payload_end];
    let payload_digest = &bytes[payload_end..payload_end + 32];
    let expected_payload_digest =
        sha256_domain("lumina.evidence-db.v2.sidecar-payload", &[payload])?;
    if payload_digest != expected_payload_digest {
        return Ok(false);
    }

    let crc_start = payload_end + 32;
    let stored_crc32c = u32::from_be_bytes([
        bytes[crc_start],
        bytes[crc_start + 1],
        bytes[crc_start + 2],
        bytes[crc_start + 3],
    ]);
    Ok(stored_crc32c == crc32c(&bytes[..crc_start]))
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82F6_3B78 & mask);
        }
    }
    !crc
}
