use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};

use anyhow::{ensure, Context, Result};
use image::{GenericImageView, ImageFormat};

use super::{MediaAccessError, MediaAvailability, MediaKind, MediaSpec, MAX_MEDIA_BYTES};

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PIXELS: u64 = 32 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ValidatedMedia {
    pub kind: MediaKind,
    pub mime: &'static str,
    pub extension: &'static str,
    pub spec: MediaSpec,
}

fn unsupported() -> anyhow::Error {
    MediaAccessError::new(
        MediaAvailability::UnsupportedFormat,
        "This media format or codec does not have a complete publication validator",
    )
    .into()
}

fn u16_at(bytes: &[u8], index: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(index..index + 2)
            .context("Truncated media header")?
            .try_into()?,
    ))
}

fn u32_at(bytes: &[u8], index: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(index..index + 4)
            .context("Truncated media header")?
            .try_into()?,
    ))
}

fn read_at(file: &mut File, offset: u64, count: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; count];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn dimensions(width: u32, height: u32) -> Result<()> {
    ensure!(
        width > 0
            && height > 0
            && width <= 8192
            && height <= 8192
            && u64::from(width) * u64::from(height) <= MAX_PIXELS,
        "Media dimensions exceed the decoded-pixel limit"
    );
    Ok(())
}

fn decode_image(bytes: &[u8], format: ImageFormat) -> Result<image::DynamicImage> {
    ensure!(
        bytes.len() as u64 <= MAX_IMAGE_BYTES,
        "Encoded image exceeds the byte limit"
    );
    if format == ImageFormat::Png {
        let mut offset = 8_usize;
        loop {
            let header = bytes
                .get(offset..offset + 8)
                .context("Truncated PNG chunk")?;
            let length = u32::from_be_bytes(header[..4].try_into()?) as usize;
            let end = offset
                .checked_add(12)
                .and_then(|v| v.checked_add(length))
                .context("PNG chunk overflow")?;
            ensure!(end <= bytes.len(), "Truncated PNG data");
            if &header[4..] == b"IEND" {
                ensure!(
                    length == 0 && end == bytes.len(),
                    "PNG has trailing or incomplete data"
                );
                break;
            }
            offset = end;
        }
    } else {
        ensure!(
            bytes.ends_with(&[0xff, 0xd9]),
            "JPEG has no terminal end-of-image marker"
        );
    }
    let mut decoder = image::io::Reader::with_format(Cursor::new(bytes), format);
    let mut limits = image::io::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(MAX_PIXELS * 4);
    decoder.limits(limits);
    let decoded = decoder.decode()?;
    dimensions(decoded.width(), decoded.height())?;
    Ok(decoded)
}

pub(crate) fn inspect(file: &mut File) -> Result<ValidatedMedia> {
    let size = file.metadata()?.len();
    ensure!(
        size > 0 && size <= MAX_MEDIA_BYTES,
        "Media byte length exceeds the publication limit"
    );
    let header = read_at(file, 0, usize::try_from(size.min(12))?)?;
    if header.starts_with(b"\x89PNG\r\n\x1a\n") || header.starts_with(&[0xff, 0xd8]) {
        ensure!(
            size <= MAX_IMAGE_BYTES,
            "Image exceeds the publication byte limit"
        );
        let png = header.starts_with(b"\x89PNG");
        let decoded = decode_image(
            &read_at(file, 0, usize::try_from(size)?)?,
            if png {
                ImageFormat::Png
            } else {
                ImageFormat::Jpeg
            },
        )?;
        let (width, height) = decoded.dimensions();
        return Ok(ValidatedMedia {
            kind: MediaKind::Image,
            mime: if png { "image/png" } else { "image/jpeg" },
            extension: if png { "png" } else { "jpg" },
            spec: MediaSpec::Image { width, height },
        });
    }
    if header.len() == 12 && &header[..4] == b"RIFF" {
        ensure!(
            u64::from(u32_at(&header, 4)?) + 8 == size,
            "RIFF length differs from complete file length"
        );
        return match &header[8..12] {
            b"WAVE" => wav(file, size),
            b"AVI " => avi(file, size),
            _ => Err(unsupported()),
        };
    }
    // MP4/H.264 must gain a full demux/decode validator before publication;
    // a MIME declaration or a successful native exit is not that evidence.
    Err(unsupported())
}

#[derive(Clone, Copy)]
struct Chunk {
    tag: [u8; 4],
    data: u64,
    bytes: u64,
}

fn chunks(file: &mut File, start: u64, end: u64) -> Result<Vec<Chunk>> {
    let mut offset = start;
    let mut result = Vec::new();
    while offset < end {
        ensure!(
            end - offset >= 8 && result.len() < 20_000,
            "Incomplete or excessive RIFF chunks"
        );
        let header = read_at(file, offset, 8)?;
        let bytes = u64::from(u32_at(&header, 4)?);
        let next = offset
            .checked_add(8)
            .and_then(|v| v.checked_add(bytes))
            .and_then(|v| v.checked_add(bytes % 2))
            .context("RIFF chunk overflow")?;
        ensure!(next <= end, "RIFF chunk crosses its container boundary");
        result.push(Chunk {
            tag: header[..4].try_into()?,
            data: offset + 8,
            bytes,
        });
        offset = next;
    }
    Ok(result)
}

fn singleton(chunks: &[Chunk], tag: &[u8; 4]) -> Result<Chunk> {
    let matches: Vec<_> = chunks
        .iter()
        .filter(|chunk| &chunk.tag == tag)
        .copied()
        .collect();
    ensure!(
        matches.len() == 1,
        "Missing or duplicate required media chunk"
    );
    Ok(matches[0])
}

fn wav(file: &mut File, size: u64) -> Result<ValidatedMedia> {
    let parts = chunks(file, 12, size)?;
    let fmt = singleton(&parts, b"fmt ")?;
    let data = singleton(&parts, b"data")?;
    if !matches!(fmt.bytes, 16 | 18) {
        return Err(unsupported());
    }
    let header = read_at(file, fmt.data, fmt.bytes as usize)?;
    if fmt.bytes == 18 {
        ensure!(
            u16_at(&header, 16)? == 0,
            "Unsupported WAV format extension"
        );
    }
    let code = u16_at(&header, 0)?;
    let channels = u16_at(&header, 2)?;
    let sample_rate = u32_at(&header, 4)?;
    let bits = u16_at(&header, 14)?;
    if !((code == 1 && matches!(bits, 8 | 16 | 24 | 32)) || (code == 3 && bits == 32)) {
        return Err(unsupported());
    }
    ensure!(
        (1..=8).contains(&channels) && (8_000..=192_000).contains(&sample_rate),
        "Unsupported WAV signal layout"
    );
    let block = channels * (bits / 8);
    ensure!(
        u16_at(&header, 12)? == block && u32_at(&header, 8)? == sample_rate * u32::from(block),
        "WAV sample layout is inconsistent"
    );
    ensure!(
        data.bytes > 0 && data.bytes % u64::from(block) == 0,
        "WAV has incomplete sample frames"
    );
    let frames = data.bytes / u64::from(block);
    let mut remaining = data.bytes;
    file.seek(SeekFrom::Start(data.data))?;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64))?;
        file.read_exact(&mut buffer[..count])?;
        if code == 3 {
            for sample in buffer[..count].chunks_exact(4) {
                let value = f32::from_le_bytes(sample.try_into()?);
                ensure!(value.is_finite(), "WAV contains nonfinite samples");
            }
        }
        remaining -= count as u64;
    }
    Ok(ValidatedMedia {
        kind: MediaKind::Audio,
        mime: "audio/wav",
        extension: "wav",
        spec: MediaSpec::Audio {
            duration_micros: frames * 1_000_000 / u64::from(sample_rate),
            sample_rate,
            channels,
            sample_frames: frames,
        },
    })
}

fn list(file: &mut File, all: &[Chunk], kind: &[u8; 4]) -> Result<Chunk> {
    let mut found = None;
    for &chunk in all {
        if chunk.tag == *b"LIST" {
            ensure!(chunk.bytes >= 4, "Incomplete AVI list");
            if read_at(file, chunk.data, 4)?.as_slice() == kind {
                ensure!(found.is_none(), "Duplicate AVI list");
                found = Some(chunk);
            }
        }
    }
    found.context("Missing AVI list")
}

fn avi(file: &mut File, size: u64) -> Result<ValidatedMedia> {
    let top = chunks(file, 12, size)?;
    ensure!(
        top.iter()
            .all(|c| matches!(&c.tag, b"LIST" | b"idx1" | b"JUNK")),
        "Unsupported AVI top-level chunk"
    );
    let hdrl = list(file, &top, b"hdrl")?;
    let movi = list(file, &top, b"movi")?;
    let headers = chunks(file, hdrl.data + 4, hdrl.data + hdrl.bytes)?;
    let avih = singleton(&headers, b"avih")?;
    ensure!(avih.bytes == 56, "Unsupported AVI main header");
    let main = read_at(file, avih.data, 56)?;
    let width = u32_at(&main, 32)?;
    let height = u32_at(&main, 36)?;
    dimensions(width, height)?;
    ensure!(
        u32_at(&main, 24)? == 1,
        "Only fully verified single-video-stream AVI is supported"
    );
    let strl = list(file, &headers, b"strl")?;
    let stream = chunks(file, strl.data + 4, strl.data + strl.bytes)?;
    let strh = singleton(&stream, b"strh")?;
    let strf = singleton(&stream, b"strf")?;
    ensure!(
        strh.bytes == 56 && strf.bytes == 40,
        "Unsupported AVI stream header"
    );
    let sh = read_at(file, strh.data, 56)?;
    let sf = read_at(file, strf.data, 40)?;
    let jpeg = &sh[4..8] == b"MJPG" && &sf[16..20] == b"MJPG";
    let rgb = matches!(&sh[4..8], b"DIB " | b"\0\0\0\0") && u32_at(&sf, 16)? == 0;
    if &sh[..4] != b"vids" || (!jpeg && !rgb) {
        return Err(unsupported());
    }
    ensure!(
        u32_at(&sf, 0)? == 40
            && u32_at(&sf, 4)? == width
            && u32_at(&sf, 8)? == height
            && u16_at(&sf, 12)? == 1
            && u16_at(&sf, 14)? == 24,
        "AVI frame layout differs from its stream"
    );
    let rate = u32_at(&sh, 24)?;
    let scale = u32_at(&sh, 20)?;
    ensure!(
        rate > 0 && scale > 0 && u32_at(&sh, 28)? == 0,
        "Invalid AVI timeline"
    );
    let micros = u64::from(scale) * 1_000_000 / u64::from(rate);
    ensure!(
        micros > 0 && micros.abs_diff(u64::from(u32_at(&main, 0)?)) <= 1,
        "AVI time headers disagree"
    );
    let frames = chunks(file, movi.data + 4, movi.data + movi.bytes)?;
    ensure!(
        (2..=10_000).contains(&frames.len())
            && frames.len() == u32_at(&main, 16)? as usize
            && frames.len() == u32_at(&sh, 32)? as usize,
        "AVI frame count is incomplete"
    );
    for frame in &frames {
        if jpeg {
            ensure!(
                frame.tag == *b"00dc" && frame.bytes <= MAX_IMAGE_BYTES,
                "Unsupported AVI compressed frame"
            );
            let decoded = decode_image(
                &read_at(file, frame.data, frame.bytes as usize)?,
                ImageFormat::Jpeg,
            )?;
            ensure!(
                decoded.dimensions() == (width, height),
                "AVI decoded frame dimensions differ"
            );
        } else {
            ensure!(frame.tag == *b"00db", "Unsupported AVI raw frame");
            let stride = (u64::from(width) * 3).div_ceil(4) * 4;
            ensure!(
                frame.bytes == stride * u64::from(height),
                "AVI raw frame is truncated"
            );
            for row in 0..height {
                read_at(file, frame.data + u64::from(row) * stride, stride as usize)?;
            }
        }
    }
    for index in top.iter().filter(|c| c.tag == *b"idx1") {
        ensure!(
            index.bytes == frames.len() as u64 * 16,
            "AVI index has an incomplete frame table"
        );
        for (position, frame) in frames.iter().enumerate() {
            let entry = read_at(file, index.data + position as u64 * 16, 16)?;
            let offset = u64::from(u32_at(&entry, 8)?);
            ensure!(
                entry[..4] == frame.tag
                    && u64::from(u32_at(&entry, 12)?) == frame.bytes
                    && (offset + movi.data == frame.data - 8 || offset == frame.data - 8),
                "AVI index refers to another frame"
            );
        }
    }
    let duration = (frames.len() as u64)
        .checked_mul(u64::from(scale))
        .and_then(|v| v.checked_mul(1_000_000))
        .context("AVI duration overflow")?
        / u64::from(rate);
    ensure!(
        duration <= 3_600_000_000,
        "Video duration exceeds the publication limit"
    );
    Ok(ValidatedMedia {
        kind: MediaKind::Video,
        mime: "video/x-msvideo",
        extension: "avi",
        spec: MediaSpec::Video {
            width,
            height,
            duration_micros: duration,
            frame_count: frames.len() as u64,
            fps_numerator: rate,
            fps_denominator: scale,
        },
    })
}
