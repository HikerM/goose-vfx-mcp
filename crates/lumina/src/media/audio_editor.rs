use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};

use super::{MediaKind, MediaResultRef, MediaSpec, ProjectEditScope, MAX_MEDIA_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WavLayout {
    data_offset: usize,
    data_bytes: usize,
    format_code: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    block_align: usize,
}

pub(crate) fn audio_edit_result_id(
    session_id: &str,
    job_id: &str,
    parent_result_id: &str,
    scope: &ProjectEditScope,
) -> String {
    let mut hash = Sha256::new();
    for value in [
        "lumina-local-audio-edit-v1",
        session_id,
        job_id,
        parent_result_id,
        &serde_json::to_string(scope).expect("ProjectEditScope is serializable"),
    ] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.finalize()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// Replaces a frame-aligned time range in a PCM WAV parent with samples from
/// the replacement WAV. The parent container and all samples outside the
/// range are retained byte-for-byte. Only canonical RIFF/WAVE files with one
/// PCM or IEEE-float `fmt ` chunk and one `data` chunk are accepted.
pub(crate) fn apply_wav_time_range(
    parent: &[u8],
    replacement: &[u8],
    parent_result: &MediaResultRef,
    replacement_result: &MediaResultRef,
    scope: &ProjectEditScope,
) -> Result<Vec<u8>> {
    let ProjectEditScope::TimeRange {
        start_micros,
        duration_micros,
    } = scope
    else {
        bail!("Audio editor requires a time range")
    };
    ensure!(
        parent_result.kind == MediaKind::Audio,
        "Parent result is not audio"
    );
    ensure!(
        replacement_result.kind == MediaKind::Audio,
        "Replacement result is not audio"
    );
    ensure!(
        parent_result.mime == "audio/wav",
        "Audio editor only supports WAV parents"
    );
    ensure!(
        replacement_result.mime == "audio/wav",
        "Audio editor only supports WAV replacements"
    );
    ensure!(
        parent.len() as u64 <= MAX_MEDIA_BYTES,
        "Parent audio exceeds the byte limit"
    );
    ensure!(
        replacement.len() as u64 <= MAX_MEDIA_BYTES,
        "Replacement audio exceeds the byte limit"
    );

    let parent_wav = parse_wav(parent)?;
    let replacement_wav = parse_wav(replacement)?;
    ensure_audio_spec(parent_result, &parent_wav, "Parent")?;
    ensure_audio_spec(replacement_result, &replacement_wav, "Replacement")?;
    ensure!(
        parent_wav.format_code == replacement_wav.format_code
            && parent_wav.channels == replacement_wav.channels
            && parent_wav.sample_rate == replacement_wav.sample_rate
            && parent_wav.bits_per_sample == replacement_wav.bits_per_sample
            && parent_wav.block_align == replacement_wav.block_align,
        "WAV replacement layout must match the parent"
    );
    let parent_frames = parent_wav.data_bytes / parent_wav.block_align;
    let replacement_frames = replacement_wav.data_bytes / replacement_wav.block_align;
    let start_frame = frame_boundary(*start_micros, parent_wav.sample_rate)?;
    let duration_frames = frame_boundary(*duration_micros, parent_wav.sample_rate)?;
    ensure!(
        duration_frames > 0,
        "Audio edit duration must include at least one frame"
    );
    let end_frame = start_frame
        .checked_add(duration_frames)
        .context("Audio edit range overflow")?;
    ensure!(
        end_frame <= parent_frames,
        "Audio edit range exceeds the parent WAV"
    );
    ensure!(
        replacement_frames >= duration_frames,
        "Replacement WAV is shorter than the edited range"
    );

    let start_byte = parent_wav
        .data_offset
        .checked_add(
            start_frame
                .checked_mul(parent_wav.block_align)
                .context("Audio edit offset overflow")?,
        )
        .context("Audio edit offset overflow")?;
    let end_byte = start_byte
        .checked_add(
            duration_frames
                .checked_mul(parent_wav.block_align)
                .context("Audio edit length overflow")?,
        )
        .context("Audio edit length overflow")?;
    let replacement_end = replacement_wav
        .data_offset
        .checked_add(
            duration_frames
                .checked_mul(replacement_wav.block_align)
                .context("Replacement offset overflow")?,
        )
        .context("Replacement offset overflow")?;
    ensure!(
        end_byte <= parent.len() && replacement_end <= replacement.len(),
        "WAV data chunk is truncated"
    );

    let mut output = parent.to_vec();
    output[start_byte..end_byte]
        .copy_from_slice(&replacement[replacement_wav.data_offset..replacement_end]);
    Ok(output)
}

fn frame_boundary(micros: u64, sample_rate: u32) -> Result<usize> {
    let numerator = micros
        .checked_mul(u64::from(sample_rate))
        .context("Audio edit time overflow")?;
    ensure!(
        numerator % 1_000_000 == 0,
        "Audio edit time must align to a complete WAV sample frame"
    );
    usize::try_from(numerator / 1_000_000)
        .context("Audio edit frame offset exceeds platform limits")
}

fn ensure_audio_spec(result: &MediaResultRef, wav: &WavLayout, label: &str) -> Result<()> {
    let MediaSpec::Audio {
        sample_rate,
        channels,
        sample_frames,
        ..
    } = result.spec
    else {
        bail!("{label} audio result has no WAV sample specification")
    };
    ensure!(
        sample_rate == wav.sample_rate
            && channels == wav.channels
            && sample_frames == wav.data_bytes as u64 / wav.block_align as u64,
        "{label} WAV layout differs from its archived media specification"
    );
    Ok(())
}

fn parse_wav(bytes: &[u8]) -> Result<WavLayout> {
    ensure!(bytes.len() >= 12, "WAV header is truncated");
    ensure!(
        &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "Unsupported WAV container"
    );
    let riff_size = u32::from_le_bytes(bytes[4..8].try_into()?);
    ensure!(
        usize::try_from(riff_size)
            .ok()
            .and_then(|v| v.checked_add(8))
            == Some(bytes.len()),
        "WAV RIFF size is inconsistent"
    );
    let mut cursor = 12usize;
    let mut fmt = None;
    let mut data = None;
    while cursor < bytes.len() {
        ensure!(bytes.len() - cursor >= 8, "WAV chunk header is truncated");
        let tag = &bytes[cursor..cursor + 4];
        let chunk_size = usize::try_from(u32::from_le_bytes(
            bytes[cursor + 4..cursor + 8].try_into()?,
        ))?;
        let content = cursor + 8;
        let end = content
            .checked_add(chunk_size)
            .context("WAV chunk length overflow")?;
        ensure!(end <= bytes.len(), "WAV chunk is truncated");
        match tag {
            b"fmt " => ensure!(
                fmt.replace((content, chunk_size)).is_none(),
                "Duplicate WAV fmt chunk"
            ),
            b"data" => ensure!(
                data.replace((content, chunk_size)).is_none(),
                "Duplicate WAV data chunk"
            ),
            _ => {}
        }
        cursor = end
            .checked_add(chunk_size % 2)
            .context("WAV chunk alignment overflow")?;
    }
    let (fmt_offset, fmt_bytes) = fmt.context("Missing WAV fmt chunk")?;
    let (data_offset, data_bytes) = data.context("Missing WAV data chunk")?;
    ensure!(matches!(fmt_bytes, 16 | 18), "Unsupported WAV fmt chunk");
    ensure!(data_bytes > 0, "WAV data chunk is empty");
    let header = &bytes[fmt_offset..fmt_offset + fmt_bytes];
    let format_code = u16::from_le_bytes(header[0..2].try_into()?);
    let channels = u16::from_le_bytes(header[2..4].try_into()?);
    let sample_rate = u32::from_le_bytes(header[4..8].try_into()?);
    let byte_rate = u32::from_le_bytes(header[8..12].try_into()?);
    let block_align = usize::from(u16::from_le_bytes(header[12..14].try_into()?));
    let bits_per_sample = u16::from_le_bytes(header[14..16].try_into()?);
    ensure!(
        matches!(format_code, 1 | 3),
        "Unsupported WAV sample format"
    );
    ensure!((1..=8).contains(&channels), "Unsupported WAV channel count");
    ensure!(
        (8_000..=192_000).contains(&sample_rate),
        "Unsupported WAV sample rate"
    );
    ensure!(
        matches!(bits_per_sample, 8 | 16 | 24 | 32),
        "Unsupported WAV bit depth"
    );
    ensure!(
        block_align > 0
            && byte_rate
                == sample_rate
                    .checked_mul(u32::try_from(block_align)?)
                    .context("WAV byte rate overflow")?,
        "WAV sample layout is inconsistent"
    );
    ensure!(
        data_bytes % block_align == 0,
        "WAV data has incomplete sample frames"
    );
    Ok(WavLayout {
        data_offset,
        data_bytes,
        format_code,
        channels,
        sample_rate,
        bits_per_sample,
        block_align,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{MediaAssociation, MediaOwnership, MediaRuntimeBuild, MediaSource};

    fn wav(sample_rate: u32, channels: u16, samples: &[u8]) -> Vec<u8> {
        let block = usize::from(channels);
        let byte_rate = sample_rate * block as u32;
        let riff_size = 36 + samples.len() as u32;
        let mut out = Vec::with_capacity(riff_size as usize + 8);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&riff_size.to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&(block as u16).to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        out.extend_from_slice(samples);
        out
    }

    fn result(mime: &str, frames: u64, sample_rate: u32, channels: u16) -> MediaResultRef {
        MediaResultRef {
            schema_version: 1,
            result_id: "00000000-0000-0000-0000-000000000001".into(),
            session_id: "session".into(),
            association: MediaAssociation::Message {
                message_id: "message".into(),
            },
            job_id: "job".into(),
            kind: MediaKind::Audio,
            mime: mime.into(),
            file_name: "audio.wav".into(),
            bytes: 0,
            sha256: "0".repeat(64),
            created_at: 0,
            root_id: "root".into(),
            relative_path: "session/job/audio.wav".into(),
            spec: MediaSpec::Audio {
                duration_micros: frames * 1_000_000 / u64::from(sample_rate),
                sample_rate,
                channels,
                sample_frames: frames,
            },
            ownership: MediaOwnership::ApplicationGenerated,
            source: MediaSource::LocalModel {
                installation_id: "installation".into(),
                version_id: "version".into(),
                runtime: MediaRuntimeBuild {
                    adapter_id: "adapter".into(),
                    engine_revision: "revision".into(),
                    binary_sha256: "0".repeat(64),
                },
                generation_config_sha256: "0".repeat(64),
                input_refs: Vec::new(),
            },
        }
    }

    #[test]
    fn replaces_only_selected_wav_frames() {
        let parent = wav(8_000, 1, &[1, 2, 3, 4]);
        let replacement = wav(8_000, 1, &[9, 8, 7, 6]);
        let scope = ProjectEditScope::TimeRange {
            start_micros: 125,
            duration_micros: 250,
        };
        let output = apply_wav_time_range(
            &parent,
            &replacement,
            &result("audio/wav", 4, 8_000, 1),
            &result("audio/wav", 4, 8_000, 1),
            &scope,
        )
        .unwrap();
        assert_eq!(&output[44..48], &[1, 9, 8, 4]);
    }

    #[test]
    fn rejects_unaligned_or_mismatched_wav_replacements() {
        let parent = wav(8_000, 1, &[1, 2, 3, 4]);
        let replacement = wav(16_000, 1, &[9, 8, 7, 6]);
        let unaligned = ProjectEditScope::TimeRange {
            start_micros: 1,
            duration_micros: 250,
        };
        let error = apply_wav_time_range(
            &parent,
            &replacement,
            &result("audio/wav", 4, 8_000, 1),
            &result("audio/wav", 4, 16_000, 1),
            &unaligned,
        )
        .unwrap_err();
        assert!(error.to_string().contains("align") || error.to_string().contains("layout"));
    }
}
