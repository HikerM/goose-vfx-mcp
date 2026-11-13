use super::{
    chunks, decode_image, dimensions, list, read_at, singleton, u16_at, u32_at, unsupported, Chunk,
    ValidatedMedia, MAX_IMAGE_BYTES,
};
use crate::media::{MediaKind, MediaSpec, MAX_MEDIA_BYTES};
use anyhow::{ensure, Context, Result};
use image::{GenericImageView, ImageFormat};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

const MAX_CHUNKS: usize = 20_000;
const MAX_DURATION_MICROS: u64 = 3_600_000_000;

struct VideoStream {
    index: u8,
    jpeg: bool,
    width: u32,
    height: u32,
    scale: u32,
    rate: u32,
    frames: u32,
}

struct AudioStream {
    index: u8,
    channels: u16,
    bits: u16,
    block: u16,
    sample_rate: u32,
    frames: u32,
}

fn bounded_chunks(file: &mut File, start: u64, end: u64, budget: &mut usize) -> Result<Vec<Chunk>> {
    let parts = chunks(file, start, end)?;
    *budget = budget
        .checked_add(parts.len())
        .context("AVI chunk count overflow")?;
    ensure!(*budget <= MAX_CHUNKS, "Excessive AVI chunk count");
    Ok(parts)
}

fn list_kind(file: &mut File, chunk: Chunk) -> Result<[u8; 4]> {
    ensure!(chunk.bytes >= 4, "Incomplete AVI list");
    Ok(read_at(file, chunk.data, 4)?.as_slice().try_into()?)
}

fn stream_headers(file: &mut File, chunk: Chunk, budget: &mut usize) -> Result<(Vec<u8>, Vec<u8>)> {
    let parts = bounded_chunks(file, chunk.data + 4, chunk.data + chunk.bytes, budget)?;
    ensure!(
        parts
            .iter()
            .all(|c| matches!(&c.tag, b"strh" | b"strf" | b"strn" | b"JUNK")),
        "Unsupported AVI stream metadata"
    );
    let header = singleton(&parts, b"strh")?;
    let format = singleton(&parts, b"strf")?;
    ensure!(header.bytes == 56, "Unsupported AVI stream header");
    ensure!(
        matches!(format.bytes, 16 | 18 | 40),
        "Unsupported AVI stream format"
    );
    let sh = read_at(file, header.data, 56)?;
    ensure!(
        u32_at(&sh, 8)? == 0 && u32_at(&sh, 16)? == 0 && u32_at(&sh, 28)? == 0,
        "Disabled, delayed or preroll AVI streams are unsupported"
    );
    Ok((sh, read_at(file, format.data, format.bytes as usize)?))
}

fn video_stream(index: u8, sh: &[u8], sf: &[u8], main: &[u8]) -> Result<VideoStream> {
    ensure!(sf.len() == 40, "Unsupported AVI video format header");
    let jpeg = &sh[4..8] == b"MJPG" && &sf[16..20] == b"MJPG";
    let rgb = matches!(&sh[4..8], b"DIB " | b"\0\0\0\0") && u32_at(sf, 16)? == 0;
    if !jpeg && !rgb {
        return Err(unsupported());
    }
    let width = u32_at(main, 32)?;
    let height = u32_at(main, 36)?;
    dimensions(width, height)?;
    ensure!(
        u32_at(sf, 0)? == 40
            && u32_at(sf, 4)? == width
            && u32_at(sf, 8)? == height
            && u16_at(sf, 12)? == 1
            && u16_at(sf, 14)? == 24
            && u32_at(sf, 32)? == 0
            && u32_at(sf, 36)? == 0
            && u32_at(sh, 44)? == 0,
        "AVI video frame layout differs from its stream"
    );
    let rectangle = &sh[48..56];
    ensure!(
        rectangle == [0; 8]
            || (u16_at(rectangle, 0)? == 0
                && u16_at(rectangle, 2)? == 0
                && u32::from(u16_at(rectangle, 4)?) == width
                && u32::from(u16_at(rectangle, 6)?) == height),
        "Unsupported AVI video source rectangle"
    );
    if rgb {
        let expected = (u64::from(width) * 3).div_ceil(4) * 4 * u64::from(height);
        ensure!(
            u32_at(sf, 20)? == 0 || u64::from(u32_at(sf, 20)?) == expected,
            "AVI RGB image size differs from its dimensions"
        );
    }
    let rate = u32_at(sh, 24)?;
    let scale = u32_at(sh, 20)?;
    ensure!(rate > 0 && scale > 0, "Invalid AVI video time base");
    let micros = u64::from(scale) * 1_000_000 / u64::from(rate);
    ensure!(
        micros > 0 && micros.abs_diff(u64::from(u32_at(main, 0)?)) <= 1,
        "AVI time headers disagree"
    );
    let frames = u32_at(sh, 32)?;
    ensure!(
        (2..=10_000).contains(&frames) && frames == u32_at(main, 16)?,
        "AVI video frame headers disagree"
    );
    Ok(VideoStream {
        index,
        jpeg,
        width,
        height,
        scale,
        rate,
        frames,
    })
}

fn audio_stream(index: u8, sh: &[u8], sf: &[u8]) -> Result<AudioStream> {
    ensure!(matches!(sf.len(), 16 | 18), "Unsupported AVI audio format");
    if u16_at(sf, 0)? != 1 || sh[4..8] != [0; 4] {
        return Err(unsupported());
    }
    if sf.len() == 18 {
        ensure!(u16_at(sf, 16)? == 0, "Unsupported AVI PCM extension");
    }
    let channels = u16_at(sf, 2)?;
    let sample_rate = u32_at(sf, 4)?;
    let bits = u16_at(sf, 14)?;
    ensure!(
        (1..=8).contains(&channels)
            && (8_000..=192_000).contains(&sample_rate)
            && matches!(bits, 8 | 16 | 24 | 32),
        "Unsupported AVI PCM signal layout"
    );
    let block = channels * (bits / 8);
    let rate = u32_at(sh, 24)?;
    let scale = u32_at(sh, 20)?;
    ensure!(
        u16_at(sf, 12)? == block
            && u32_at(sf, 8)? == sample_rate * u32::from(block)
            && u32_at(sh, 44)? == u32::from(block)
            && scale > 0
            && u64::from(rate) == u64::from(sample_rate) * u64::from(scale)
            && sh[48..56] == [0; 8],
        "AVI PCM block layout or time base is inconsistent"
    );
    let frames = u32_at(sh, 32)?;
    ensure!(frames > 0, "AVI audio stream has no samples");
    Ok(AudioStream {
        index,
        channels,
        bits,
        block,
        sample_rate,
        frames,
    })
}

fn media_chunks(file: &mut File, movi: Chunk, budget: &mut usize) -> Result<Vec<Chunk>> {
    let mut media = Vec::new();
    for chunk in bounded_chunks(file, movi.data + 4, movi.data + movi.bytes, budget)? {
        if chunk.tag == *b"LIST" {
            ensure!(
                list_kind(file, chunk)? == *b"rec ",
                "Unsupported AVI media list"
            );
            for item in bounded_chunks(file, chunk.data + 4, chunk.data + chunk.bytes, budget)? {
                ensure!(
                    item.tag != *b"LIST",
                    "Nested AVI recording lists are unsupported"
                );
                if item.tag != *b"JUNK" {
                    media.push(item);
                }
            }
        } else if chunk.tag != *b"JUNK" {
            media.push(chunk);
        }
    }
    Ok(media)
}

fn decode_video(file: &mut File, frame: Chunk, video: &VideoStream) -> Result<()> {
    if video.jpeg {
        ensure!(
            &frame.tag[2..] == b"dc" && frame.bytes <= MAX_IMAGE_BYTES,
            "Unsupported AVI compressed frame"
        );
        let decoded = decode_image(
            &read_at(file, frame.data, frame.bytes as usize)?,
            ImageFormat::Jpeg,
        )?;
        ensure!(
            decoded.dimensions() == (video.width, video.height),
            "AVI decoded frame dimensions differ"
        );
    } else {
        ensure!(&frame.tag[2..] == b"db", "Unsupported AVI RGB frame tag");
        let stride = (u64::from(video.width) * 3).div_ceil(4) * 4;
        ensure!(
            frame.bytes == stride * u64::from(video.height),
            "AVI RGB frame is truncated"
        );
        for row in 0..video.height {
            read_at(file, frame.data + u64::from(row) * stride, stride as usize)?;
        }
    }
    Ok(())
}

fn decode_pcm(file: &mut File, chunk: Chunk, audio: &AudioStream) -> Result<u64> {
    ensure!(
        &chunk.tag[2..] == b"wb" && chunk.bytes > 0 && chunk.bytes % u64::from(audio.block) == 0,
        "AVI PCM chunk has incomplete sample frames"
    );
    let mut remaining = chunk.bytes;
    let mut buffer = [0_u8; 64 * 1024];
    let capacity = buffer.len() / usize::from(audio.block) * usize::from(audio.block);
    let sample_bytes = usize::from(audio.bits / 8);
    let mut decoded_samples = 0_u64;
    file.seek(SeekFrom::Start(chunk.data))?;
    while remaining > 0 {
        let count = usize::try_from(remaining.min(capacity as u64))?;
        file.read_exact(&mut buffer[..count])?;
        for sample in buffer[..count].chunks_exact(sample_bytes) {
            let amplitude = match audio.bits {
                8 => (f64::from(sample[0]) - 128.) / 128.,
                16 => f64::from(i16::from_le_bytes(sample.try_into()?)) / 32_768.,
                24 => {
                    let signed = i32::from_le_bytes([0, sample[0], sample[1], sample[2]]) >> 8;
                    f64::from(signed) / 8_388_608.
                }
                32 => f64::from(i32::from_le_bytes(sample.try_into()?)) / 2_147_483_648.,
                _ => unreachable!("PCM sample width was validated"),
            };
            ensure!(amplitude.is_finite(), "Invalid AVI PCM sample");
            decoded_samples += 1;
        }
        remaining -= count as u64;
    }
    let frames = chunk.bytes / u64::from(audio.block);
    ensure!(
        decoded_samples == frames * u64::from(audio.channels),
        "AVI PCM decoded sample count differs"
    );
    Ok(frames)
}

fn validate_index(
    file: &mut File,
    top: &[Chunk],
    movi: Chunk,
    media: &[Chunk],
    flags: u32,
) -> Result<()> {
    let indexes: Vec<_> = top.iter().filter(|c| c.tag == *b"idx1").collect();
    ensure!(indexes.len() <= 1, "Duplicate AVI index");
    ensure!(
        (flags & 0x10 != 0) == !indexes.is_empty(),
        "AVI index presence differs from main header"
    );
    let Some(index) = indexes.first() else {
        ensure!(flags & 0x20 == 0, "AVI requires a missing index");
        return Ok(());
    };
    ensure!(
        index.bytes == media.len() as u64 * 16,
        "AVI index has an incomplete media table"
    );
    let mut absolute = None;
    for (position, chunk) in media.iter().enumerate() {
        let entry = read_at(file, index.data + position as u64 * 16, 16)?;
        let offset = u64::from(u32_at(&entry, 8)?);
        let is_absolute = *absolute.get_or_insert(offset == chunk.data - 8);
        let resolved = if is_absolute {
            offset
        } else {
            offset + movi.data
        };
        ensure!(
            entry[..4] == chunk.tag
                && matches!(u32_at(&entry, 4)?, 0 | 0x10)
                && u64::from(u32_at(&entry, 12)?) == chunk.bytes
                && resolved == chunk.data - 8,
            "AVI index refers to a different or incomplete media chunk"
        );
    }
    Ok(())
}

pub(super) fn inspect(file: &mut File, size: u64) -> Result<ValidatedMedia> {
    ensure!(
        size >= 12 && size <= MAX_MEDIA_BYTES && file.metadata()?.len() == size,
        "AVI byte length exceeds the publication limit or changed"
    );
    let riff = read_at(file, 0, 12)?;
    ensure!(
        &riff[..4] == b"RIFF" && &riff[8..] == b"AVI " && u64::from(u32_at(&riff, 4)?) + 8 == size,
        "AVI RIFF length differs from complete file length"
    );
    let mut budget = 0;
    let top = bounded_chunks(file, 12, size, &mut budget)?;
    for &chunk in &top {
        match &chunk.tag {
            b"LIST" => match &list_kind(file, chunk)? {
                b"hdrl" | b"movi" => {}
                b"INFO" => {
                    let info = bounded_chunks(
                        file,
                        chunk.data + 4,
                        chunk.data + chunk.bytes,
                        &mut budget,
                    )?;
                    ensure!(
                        info.iter().all(|c| c.tag != *b"LIST"),
                        "Nested AVI metadata is unsupported"
                    );
                }
                _ => return Err(unsupported()),
            },
            b"idx1" | b"JUNK" => {}
            _ => return Err(unsupported()),
        }
    }
    let hdrl = list(file, &top, b"hdrl")?;
    let movi = list(file, &top, b"movi")?;
    let headers = bounded_chunks(file, hdrl.data + 4, hdrl.data + hdrl.bytes, &mut budget)?;
    ensure!(
        headers
            .iter()
            .all(|c| matches!(&c.tag, b"avih" | b"LIST" | b"JUNK")),
        "Unsupported AVI header chunk"
    );
    let avih = singleton(&headers, b"avih")?;
    ensure!(avih.bytes == 56, "Unsupported AVI main header");
    let main = read_at(file, avih.data, 56)?;
    ensure!(
        u32_at(&main, 24)? == 2 && u32_at(&main, 20)? == 0,
        "Expected exactly one video and one audio stream without preroll"
    );
    let mut video = None;
    let mut audio = None;
    let streams: Vec<_> = headers.iter().filter(|c| c.tag == *b"LIST").collect();
    ensure!(streams.len() == 2, "AVI stream count differs from headers");
    for (index, &&chunk) in streams.iter().enumerate() {
        ensure!(
            list_kind(file, chunk)? == *b"strl",
            "Unsupported AVI header list"
        );
        let (sh, sf) = stream_headers(file, chunk, &mut budget)?;
        match &sh[..4] {
            b"vids" => {
                ensure!(video.is_none(), "Duplicate AVI video stream");
                video = Some(video_stream(index as u8, &sh, &sf, &main)?);
            }
            b"auds" => {
                ensure!(audio.is_none(), "Duplicate AVI audio stream");
                audio = Some(audio_stream(index as u8, &sh, &sf)?);
            }
            _ => return Err(unsupported()),
        }
    }
    let video = video.context("Missing AVI video stream")?;
    let audio = audio.context("Missing AVI audio stream")?;
    let media = media_chunks(file, movi, &mut budget)?;
    let mut video_frames = 0_u64;
    let mut audio_frames = 0_u64;
    for &chunk in &media {
        ensure!(
            chunk.tag[0] == b'0' && matches!(chunk.tag[1], b'0' | b'1'),
            "AVI chunk references an undeclared stream"
        );
        let index = chunk.tag[1] - b'0';
        if index == video.index {
            video_frames += 1;
            ensure!(
                video_frames <= u64::from(video.frames),
                "Excess AVI video frames"
            );
            decode_video(file, chunk, &video)?;
        } else {
            ensure!(index == audio.index, "AVI chunk stream mismatch");
            audio_frames = audio_frames
                .checked_add(decode_pcm(file, chunk, &audio)?)
                .context("AVI audio sample count overflow")?;
            ensure!(
                audio_frames <= u64::from(audio.frames),
                "Excess AVI audio samples"
            );
        }
    }
    ensure!(
        video_frames == u64::from(video.frames) && audio_frames == u64::from(audio.frames),
        "AVI media counts differ from stream headers"
    );
    validate_index(file, &top, movi, &media, u32_at(&main, 12)?)?;
    let video_duration = video_frames
        .checked_mul(u64::from(video.scale))
        .and_then(|v| v.checked_mul(1_000_000))
        .context("AVI duration overflow")?
        / u64::from(video.rate);
    let audio_duration = audio_frames * 1_000_000 / u64::from(audio.sample_rate);
    let duration_micros = video_duration.max(audio_duration);
    ensure!(
        duration_micros <= MAX_DURATION_MICROS,
        "Video duration exceeds the publication limit"
    );
    Ok(ValidatedMedia {
        kind: MediaKind::Video,
        mime: "video/x-msvideo",
        extension: "avi",
        spec: MediaSpec::Video {
            width: video.width,
            height: video.height,
            duration_micros,
            frame_count: video_frames,
            fps_numerator: video.rate,
            fps_denominator: video.scale,
        },
    })
}

#[cfg(test)]
#[path = "avi_audio_tests.rs"]
mod tests;
