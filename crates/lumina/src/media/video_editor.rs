use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};

use super::{MediaKind, MediaResultRef, MediaSpec, ProjectEditScope, MAX_MEDIA_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoEncoding {
    Mjpeg,
    RawRgb,
}

#[derive(Debug, Clone, Copy)]
struct AviFrame {
    data: usize,
    bytes: usize,
}

#[derive(Debug)]
struct AviLayout {
    width: u32,
    height: u32,
    scale: u32,
    rate: u32,
    encoding: VideoEncoding,
    frames: Vec<AviFrame>,
}

#[derive(Debug, Clone, Copy)]
struct Chunk {
    tag: [u8; 4],
    data: usize,
    bytes: usize,
}

pub(crate) fn video_edit_result_id(
    session_id: &str,
    job_id: &str,
    parent_result_id: &str,
    scope: &ProjectEditScope,
) -> String {
    let mut hash = Sha256::new();
    for value in [
        "lumina-local-video-edit-v1",
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

/// Replaces frame payloads in a single-stream AVI without changing its
/// container layout. Equal payload lengths keep any existing idx1 offsets valid.
pub(crate) fn apply_avi_time_range(
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
        bail!("Video editor requires a time range")
    };
    ensure!(
        parent_result.kind == MediaKind::Video && replacement_result.kind == MediaKind::Video,
        "Video editor requires video inputs"
    );
    ensure!(
        parent_result.mime == "video/x-msvideo" && replacement_result.mime == "video/x-msvideo",
        "Video editor only supports video/x-msvideo"
    );
    ensure!(
        parent.len() as u64 <= MAX_MEDIA_BYTES,
        "Parent video exceeds the byte limit"
    );
    ensure!(
        replacement.len() as u64 <= MAX_MEDIA_BYTES,
        "Replacement video exceeds the byte limit"
    );

    let parent_avi = parse_video_only_avi(parent)?;
    let replacement_avi = parse_video_only_avi(replacement)?;
    ensure_video_spec(parent_result, &parent_avi, "Parent")?;
    ensure_video_spec(replacement_result, &replacement_avi, "Replacement")?;
    ensure!(
        parent_avi.width == replacement_avi.width
            && parent_avi.height == replacement_avi.height
            && parent_avi.scale == replacement_avi.scale
            && parent_avi.rate == replacement_avi.rate
            && parent_avi.encoding == replacement_avi.encoding,
        "AVI replacement layout must match the parent"
    );

    let start_frame = frame_boundary(*start_micros, parent_avi.scale, parent_avi.rate)?;
    let duration_frames = frame_boundary(*duration_micros, parent_avi.scale, parent_avi.rate)?;
    ensure!(
        duration_frames > 0,
        "Video edit duration must include at least one frame"
    );
    let end_frame = start_frame
        .checked_add(duration_frames)
        .context("Video edit range overflow")?;
    ensure!(
        end_frame <= parent_avi.frames.len(),
        "Video edit range exceeds the parent AVI"
    );
    ensure!(
        replacement_avi.frames.len() >= duration_frames,
        "Replacement AVI is shorter than the edited range"
    );

    let mut output = parent.to_vec();
    for (target, source) in parent_avi.frames[start_frame..end_frame]
        .iter()
        .zip(replacement_avi.frames.iter().take(duration_frames))
    {
        ensure!(
            target.bytes == source.bytes,
            "AVI replacement frame length differs; refusing to rewrite container indexes"
        );
        output[target.data..target.data + target.bytes]
            .copy_from_slice(&replacement[source.data..source.data + source.bytes]);
    }
    Ok(output)
}

fn frame_boundary(micros: u64, scale: u32, rate: u32) -> Result<usize> {
    let denominator = u64::from(scale)
        .checked_mul(1_000_000)
        .context("AVI time base overflow")?;
    let numerator = micros
        .checked_mul(u64::from(rate))
        .context("Video edit time overflow")?;
    ensure!(
        numerator % denominator == 0,
        "Video edit time must align to a complete AVI frame"
    );
    usize::try_from(numerator / denominator)
        .context("Video edit frame offset exceeds platform limits")
}

fn ensure_video_spec(result: &MediaResultRef, avi: &AviLayout, label: &str) -> Result<()> {
    let MediaSpec::Video {
        width,
        height,
        duration_micros,
        frame_count,
        fps_numerator,
        fps_denominator,
        ..
    } = &result.spec
    else {
        bail!("{label} result has no video specification")
    };
    ensure!(
        *width == avi.width
            && *height == avi.height
            && *duration_micros
                == (avi.frames.len() as u64)
                    .checked_mul(u64::from(avi.scale))
                    .and_then(|value| value.checked_mul(1_000_000))
                    .map(|value| value / u64::from(avi.rate))
                    .context("AVI duration overflow")?
            && *frame_count == avi.frames.len() as u64
            && *fps_numerator == avi.rate
            && *fps_denominator == avi.scale,
        "{label} AVI layout differs from its archived media specification"
    );
    Ok(())
}

fn parse_video_only_avi(bytes: &[u8]) -> Result<AviLayout> {
    ensure!(bytes.len() >= 12, "AVI header is truncated");
    ensure!(
        &bytes[..4] == b"RIFF" && &bytes[8..12] == b"AVI ",
        "Unsupported AVI container"
    );
    let riff_size = u32::from_le_bytes(bytes[4..8].try_into()?);
    ensure!(
        usize::try_from(riff_size)
            .ok()
            .and_then(|value| value.checked_add(8))
            == Some(bytes.len()),
        "AVI RIFF size is inconsistent"
    );
    let top = chunks(bytes, 12, bytes.len())?;
    ensure!(
        top.iter()
            .all(|chunk| matches!(&chunk.tag, b"LIST" | b"idx1" | b"JUNK")),
        "Unsupported AVI top-level chunk"
    );
    let hdrl = singleton_list(bytes, &top, b"hdrl")?;
    let movi = singleton_list(bytes, &top, b"movi")?;
    ensure!(hdrl.bytes >= 4 && movi.bytes >= 4, "AVI list is truncated");

    let headers = chunks(bytes, hdrl.data + 4, hdrl.data + hdrl.bytes)?;
    let avih = singleton(&headers, b"avih")?;
    ensure!(avih.bytes == 56, "Unsupported AVI main header");
    let main = &bytes[avih.data..avih.data + avih.bytes];
    ensure!(
        u32_at(main, 24)? == 1,
        "Video time range editing requires a single video stream without audio"
    );
    let streams: Vec<_> = headers
        .iter()
        .filter(|chunk| chunk.tag == *b"LIST")
        .collect();
    ensure!(
        streams.len() == 1,
        "Video time range editing requires one AVI video stream"
    );
    let stream = streams[0];
    ensure!(
        bytes.get(stream.data..stream.data + 4) == Some(b"strl"),
        "Unsupported AVI stream list"
    );
    let stream_chunks = chunks(bytes, stream.data + 4, stream.data + stream.bytes)?;
    let strh = singleton(&stream_chunks, b"strh")?;
    let strf = singleton(&stream_chunks, b"strf")?;
    ensure!(
        strh.bytes == 56 && strf.bytes == 40,
        "Unsupported AVI video stream header"
    );
    let stream_header = &bytes[strh.data..strh.data + strh.bytes];
    let format = &bytes[strf.data..strf.data + strf.bytes];
    ensure!(&stream_header[..4] == b"vids", "AVI stream is not video");
    let encoding = if &stream_header[4..8] == b"MJPG" && &format[16..20] == b"MJPG" {
        VideoEncoding::Mjpeg
    } else if matches!(&stream_header[4..8], b"DIB " | b"\0\0\0\0") && u32_at(format, 16)? == 0 {
        VideoEncoding::RawRgb
    } else {
        return Err(anyhow::anyhow!("Unsupported AVI video codec"));
    };
    let width = u32_at(main, 32)?;
    let height = u32_at(main, 36)?;
    ensure!(
        width > 0 && height > 0 && width <= 8192 && height <= 8192,
        "AVI dimensions exceed the local edit limit"
    );
    ensure!(
        u32_at(format, 0)? == 40
            && u32_at(format, 4)? == width
            && u32_at(format, 8)? == height
            && u16_at(format, 12)? == 1
            && u16_at(format, 14)? == 24,
        "AVI video frame layout differs from its stream"
    );
    let rate = u32_at(stream_header, 24)?;
    let scale = u32_at(stream_header, 20)?;
    ensure!(rate > 0 && scale > 0, "Invalid AVI time base");
    ensure!(
        u32_at(stream_header, 28)? == 0,
        "AVI video stream has a non-zero start offset"
    );
    let micros_per_frame = u64::from(scale)
        .checked_mul(1_000_000)
        .context("AVI frame duration overflow")?
        / u64::from(rate);
    ensure!(
        micros_per_frame > 0 && micros_per_frame.abs_diff(u64::from(u32_at(main, 0)?)) <= 1,
        "AVI time headers disagree"
    );
    let frames = u32_at(main, 16)?;
    ensure!(
        (2..=10_000).contains(&frames) && frames == u32_at(stream_header, 32)?,
        "AVI frame headers disagree"
    );

    let media_chunks = chunks(bytes, movi.data + 4, movi.data + movi.bytes)?;
    let expected_tag = match encoding {
        VideoEncoding::Mjpeg => *b"00dc",
        VideoEncoding::RawRgb => *b"00db",
    };
    ensure!(
        media_chunks.len() == frames as usize
            && media_chunks.iter().all(|chunk| chunk.tag == expected_tag),
        "AVI media frames differ from the declared video stream"
    );
    let raw_stride = (u64::from(width) * 3).div_ceil(4) * 4;
    let raw_bytes = raw_stride
        .checked_mul(u64::from(height))
        .context("AVI raw frame size overflow")?;
    let mut parsed_frames = Vec::with_capacity(media_chunks.len());
    for chunk in media_chunks {
        match encoding {
            VideoEncoding::RawRgb => ensure!(
                chunk.bytes as u64 == raw_bytes,
                "AVI RGB frame is truncated or has a different stride"
            ),
            VideoEncoding::Mjpeg => {
                let decoded = image::load_from_memory(&bytes[chunk.data..chunk.data + chunk.bytes])
                    .context("AVI MJPEG frame is not a valid JPEG")?;
                ensure!(
                    decoded.width() == width && decoded.height() == height,
                    "AVI MJPEG frame dimensions differ from the stream"
                );
            }
        }
        parsed_frames.push(AviFrame {
            data: chunk.data,
            bytes: chunk.bytes,
        });
    }
    Ok(AviLayout {
        width,
        height,
        scale,
        rate,
        encoding,
        frames: parsed_frames,
    })
}

fn chunks(bytes: &[u8], start: usize, end: usize) -> Result<Vec<Chunk>> {
    let mut cursor = start;
    let mut result = Vec::new();
    while cursor < end {
        ensure!(
            end - cursor >= 8 && result.len() < 20_000,
            "Incomplete or excessive AVI chunks"
        );
        let tag: [u8; 4] = bytes[cursor..cursor + 4].try_into()?;
        let size = usize::try_from(u32::from_le_bytes(
            bytes[cursor + 4..cursor + 8].try_into()?,
        ))?;
        let data = cursor + 8;
        let next = data
            .checked_add(size)
            .and_then(|value| value.checked_add(size % 2))
            .context("AVI chunk length overflow")?;
        ensure!(next <= end, "AVI chunk crosses its container boundary");
        result.push(Chunk {
            tag,
            data,
            bytes: size,
        });
        cursor = next;
    }
    Ok(result)
}

fn singleton(chunks: &[Chunk], tag: &[u8; 4]) -> Result<Chunk> {
    let mut found = None;
    for chunk in chunks.iter().copied().filter(|chunk| &chunk.tag == tag) {
        ensure!(found.is_none(), "Duplicate required AVI chunk");
        found = Some(chunk);
    }
    found.context("Missing required AVI chunk")
}

fn singleton_list(bytes: &[u8], chunks: &[Chunk], kind: &[u8; 4]) -> Result<Chunk> {
    let mut found = None;
    for list in chunks.iter().copied().filter(|chunk| chunk.tag == *b"LIST") {
        ensure!(list.bytes >= 4, "AVI LIST is truncated");
        if &bytes[list.data..list.data + 4] == kind {
            ensure!(found.is_none(), "Duplicate required AVI LIST");
            found = Some(list);
        }
    }
    found.context("Missing required AVI LIST")
}

fn u16_at(bytes: &[u8], index: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(index..index + 2)
            .context("Truncated AVI header")?
            .try_into()?,
    ))
}

fn u32_at(bytes: &[u8], index: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(index..index + 4)
            .context("Truncated AVI header")?
            .try_into()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{MediaAssociation, MediaOwnership, MediaRuntimeBuild, MediaSource};

    fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut output = tag.to_vec();
        output.extend_from_slice(&(data.len() as u32).to_le_bytes());
        output.extend_from_slice(data);
        if data.len() % 2 != 0 {
            output.push(0);
        }
        output
    }

    fn avi(frames: &[[u8; 4]]) -> Vec<u8> {
        let mut main = vec![0_u8; 56];
        main[0..4].copy_from_slice(&50_000_u32.to_le_bytes());
        main[12..16].copy_from_slice(&0x100_u32.to_le_bytes());
        main[16..20].copy_from_slice(&(frames.len() as u32).to_le_bytes());
        main[24..28].copy_from_slice(&1_u32.to_le_bytes());
        main[32..36].copy_from_slice(&1_u32.to_le_bytes());
        main[36..40].copy_from_slice(&1_u32.to_le_bytes());
        let mut stream_header = vec![0_u8; 56];
        stream_header[..4].copy_from_slice(b"vids");
        stream_header[4..8].copy_from_slice(b"DIB ");
        stream_header[20..24].copy_from_slice(&1_u32.to_le_bytes());
        stream_header[24..28].copy_from_slice(&20_u32.to_le_bytes());
        stream_header[32..36].copy_from_slice(&(frames.len() as u32).to_le_bytes());
        let mut stream_format = vec![0_u8; 40];
        stream_format[0..4].copy_from_slice(&40_u32.to_le_bytes());
        stream_format[4..8].copy_from_slice(&1_u32.to_le_bytes());
        stream_format[8..12].copy_from_slice(&1_u32.to_le_bytes());
        stream_format[12..14].copy_from_slice(&1_u16.to_le_bytes());
        stream_format[14..16].copy_from_slice(&24_u16.to_le_bytes());
        let mut stream = b"strl".to_vec();
        stream.extend(chunk(b"strh", &stream_header));
        stream.extend(chunk(b"strf", &stream_format));
        let mut hdrl = b"hdrl".to_vec();
        hdrl.extend(chunk(b"avih", &main));
        hdrl.extend(chunk(b"LIST", &stream));
        let mut movi = b"movi".to_vec();
        for frame in frames {
            movi.extend(chunk(b"00db", frame));
        }
        let mut body = b"AVI ".to_vec();
        body.extend(chunk(b"LIST", &hdrl));
        body.extend(chunk(b"LIST", &movi));
        chunk(b"RIFF", &body)
    }

    fn result(frames: u64) -> MediaResultRef {
        MediaResultRef {
            schema_version: 1,
            result_id: "00000000-0000-0000-0000-000000000001".into(),
            session_id: "session".into(),
            association: MediaAssociation::Message {
                message_id: "message".into(),
            },
            job_id: "job".into(),
            kind: MediaKind::Video,
            mime: "video/x-msvideo".into(),
            file_name: "video.avi".into(),
            bytes: 0,
            sha256: "0".repeat(64),
            created_at: 0,
            root_id: "root".into(),
            relative_path: "session/job/video.avi".into(),
            spec: MediaSpec::Video {
                width: 1,
                height: 1,
                duration_micros: frames * 50_000,
                frame_count: frames,
                fps_numerator: 20,
                fps_denominator: 1,
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
    fn replaces_only_selected_raw_avi_frames() {
        let parent = avi(&[[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]]);
        let replacement = avi(&[[20, 21, 22, 23], [24, 25, 26, 27], [28, 29, 30, 31]]);
        let scope = ProjectEditScope::TimeRange {
            start_micros: 50_000,
            duration_micros: 50_000,
        };
        let output =
            apply_avi_time_range(&parent, &replacement, &result(3), &result(3), &scope).unwrap();
        let parsed = parse_video_only_avi(&output).unwrap();
        assert_eq!(
            &output[parsed.frames[0].data..parsed.frames[0].data + 4],
            &[1, 2, 3, 4]
        );
        assert_eq!(
            &output[parsed.frames[1].data..parsed.frames[1].data + 4],
            &[20, 21, 22, 23]
        );
        assert_eq!(
            &output[parsed.frames[2].data..parsed.frames[2].data + 4],
            &[9, 10, 11, 12]
        );
    }

    #[test]
    fn rejects_non_avi_and_unaligned_or_mismatched_video() {
        let parent = avi(&[[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]]);
        let replacement = avi(&[[20, 21, 22, 23], [24, 25, 26, 27], [28, 29, 30, 31]]);
        let scope = ProjectEditScope::TimeRange {
            start_micros: 1,
            duration_micros: 50_000,
        };
        let error =
            apply_avi_time_range(b"not an avi", &replacement, &result(3), &result(3), &scope)
                .unwrap_err();
        assert!(error.to_string().contains("AVI"));
        let error = apply_avi_time_range(&parent, &replacement, &result(3), &result(3), &scope)
            .unwrap_err();
        assert!(error.to_string().contains("align"));
    }
}
