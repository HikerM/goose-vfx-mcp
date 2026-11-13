use super::*;
use std::io::{Cursor, Write};
use std::path::PathBuf;

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(data);
    if data.len() % 2 != 0 {
        bytes.push(0);
    }
    bytes
}

fn jpeg(width: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, 1, image::Rgb([127, 64, 32]));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, image::ImageOutputFormat::Jpeg(90))
        .unwrap();
    bytes.into_inner()
}

struct Fixture {
    main: Vec<u8>,
    video_header: Vec<u8>,
    video_format: Vec<u8>,
    audio_header: Vec<u8>,
    audio_format: Vec<u8>,
    media: Vec<([u8; 4], Vec<u8>)>,
    audio_first: bool,
    recording_list: bool,
    relative_index: bool,
    index: bool,
}

impl Fixture {
    fn new(jpeg_frames: bool, bits: u16, channels: u16) -> Self {
        let mut main = vec![0; 56];
        for (offset, value) in [(0, 50_000), (12, 0x110), (16, 2), (24, 2), (32, 1), (36, 1)] {
            put32(&mut main, offset, value);
        }
        let mut video_header = vec![0; 56];
        video_header[..4].copy_from_slice(b"vids");
        video_header[4..8].copy_from_slice(if jpeg_frames { b"MJPG" } else { b"DIB " });
        for (offset, value) in [(20, 1), (24, 20), (32, 2)] {
            put32(&mut video_header, offset, value);
        }
        let mut video_format = vec![0; 40];
        for (offset, value) in [(0, 40), (4, 1), (8, 1)] {
            put32(&mut video_format, offset, value);
        }
        put16(&mut video_format, 12, 1);
        put16(&mut video_format, 14, 24);
        if jpeg_frames {
            video_format[16..20].copy_from_slice(b"MJPG");
        }
        let block = channels * (bits / 8);
        let mut audio_header = vec![0; 56];
        audio_header[..4].copy_from_slice(b"auds");
        for (offset, value) in [
            (20, u32::from(block)),
            (24, 8_000 * u32::from(block)),
            (32, 800),
            (44, u32::from(block)),
        ] {
            put32(&mut audio_header, offset, value);
        }
        let mut audio_format = vec![0; 16];
        put16(&mut audio_format, 0, 1);
        put16(&mut audio_format, 2, channels);
        put32(&mut audio_format, 4, 8_000);
        put32(&mut audio_format, 8, 8_000 * u32::from(block));
        put16(&mut audio_format, 12, block);
        put16(&mut audio_format, 14, bits);
        let video = if jpeg_frames {
            jpeg(1)
        } else {
            vec![32, 64, 127, 0]
        };
        let tag = if jpeg_frames { *b"00dc" } else { *b"00db" };
        let audio = (0..usize::from(block) * 400)
            .map(|i| (i % 256) as u8)
            .collect::<Vec<_>>();
        Self {
            main,
            video_header,
            video_format,
            audio_header,
            audio_format,
            media: vec![
                (tag, video.clone()),
                (*b"01wb", audio.clone()),
                (tag, video),
                (*b"01wb", audio),
            ],
            audio_first: false,
            recording_list: false,
            relative_index: false,
            index: true,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let stream = |header: &[u8], format: &[u8]| {
            let mut body = b"strl".to_vec();
            body.extend(chunk(b"strh", header));
            body.extend(chunk(b"strf", format));
            chunk(b"LIST", &body)
        };
        let mut hdrl = b"hdrl".to_vec();
        hdrl.extend(chunk(b"avih", &self.main));
        let video = stream(&self.video_header, &self.video_format);
        let audio = stream(&self.audio_header, &self.audio_format);
        for bytes in if self.audio_first {
            [audio, video]
        } else {
            [video, audio]
        } {
            hdrl.extend(bytes);
        }
        let mut body = b"AVI ".to_vec();
        body.extend(chunk(b"LIST", &hdrl));
        let movi_data = 8 + body.len() + 8;
        let mut payload = Vec::new();
        let mut index = Vec::new();
        for (original_tag, data) in &self.media {
            let mut tag = *original_tag;
            if self.audio_first {
                tag[1] = if tag[1] == b'0' { b'1' } else { b'0' };
            }
            let relative_offset = 4 + if self.recording_list { 12 } else { 0 } + payload.len();
            let offset = if self.relative_index {
                relative_offset
            } else {
                movi_data + relative_offset
            };
            index.extend_from_slice(&tag);
            index.extend_from_slice(&0x10_u32.to_le_bytes());
            index.extend_from_slice(&(offset as u32).to_le_bytes());
            index.extend_from_slice(&(data.len() as u32).to_le_bytes());
            payload.extend(chunk(&tag, data));
        }
        let mut movi = b"movi".to_vec();
        if self.recording_list {
            let mut recording = b"rec ".to_vec();
            recording.extend(payload);
            movi.extend(chunk(b"LIST", &recording));
        } else {
            movi.extend(payload);
        }
        body.extend(chunk(b"LIST", &movi));
        let mut info = b"INFO".to_vec();
        info.extend(chunk(b"ICMT", b"Independent synthetic AVI/PCM fixture"));
        body.extend(chunk(b"LIST", &info));
        if self.index {
            body.extend(chunk(b"idx1", &index));
        }
        chunk(b"RIFF", &body)
    }
}

fn inspect_bytes(bytes: &[u8]) -> Result<ValidatedMedia> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/avi-audio-unit-tests");
    std::fs::create_dir_all(&base)?;
    let mut file = tempfile::Builder::new().prefix("avi-").tempfile_in(base)?;
    file.write_all(bytes)?;
    file.flush()?;
    inspect(file.as_file_mut(), bytes.len() as u64)
}

#[test]
fn decodes_rgb_and_all_supported_pcm_widths_and_channels() {
    for bits in [8, 16, 24, 32] {
        for channels in [1, 2, 8] {
            let result = inspect_bytes(&Fixture::new(false, bits, channels).bytes()).unwrap();
            assert_eq!(result.kind, MediaKind::Video);
            assert_eq!(result.mime, "video/x-msvideo");
            assert_eq!(
                result.spec,
                MediaSpec::Video {
                    width: 1,
                    height: 1,
                    duration_micros: 100_000,
                    frame_count: 2,
                    fps_numerator: 20,
                    fps_denominator: 1,
                }
            );
        }
    }
}

#[test]
fn decodes_mjpeg_reversed_stream_order_recording_lists_and_relative_index() {
    let mut fixture = Fixture::new(true, 16, 2);
    fixture.audio_first = true;
    fixture.recording_list = true;
    fixture.relative_index = true;
    fixture.audio_format.extend_from_slice(&[0, 0]);
    assert!(inspect_bytes(&fixture.bytes()).is_ok());
}

#[test]
fn container_duration_covers_the_longer_verified_track() {
    let mut fixture = Fixture::new(false, 16, 2);
    put32(&mut fixture.audio_header, 32, 1_000);
    fixture.media[3].1.extend(vec![0; 200 * 4]);
    let result = inspect_bytes(&fixture.bytes()).unwrap();
    assert!(matches!(
        result.spec,
        MediaSpec::Video {
            duration_micros: 125_000,
            ..
        }
    ));
}

#[test]
fn accepts_missing_index_only_when_header_does_not_require_it() {
    let mut fixture = Fixture::new(false, 8, 1);
    fixture.index = false;
    assert!(inspect_bytes(&fixture.bytes()).is_err());
    put32(&mut fixture.main, 12, 0);
    assert!(inspect_bytes(&fixture.bytes()).is_ok());
    put32(&mut fixture.main, 12, 0x20);
    assert!(inspect_bytes(&fixture.bytes()).is_err());
}

#[test]
fn rejects_missing_tracks_wrong_stream_indices_and_crossed_codecs() {
    for case in 0..6 {
        let mut fixture = Fixture::new(false, 16, 2);
        match case {
            0 => fixture.media.retain(|(tag, _)| tag != b"01wb"),
            1 => fixture.media[1].0 = *b"02wb",
            2 => fixture.media[1].0 = *b"00wb",
            3 => fixture.media[0].0 = *b"01db",
            4 => fixture.audio_header[..4].copy_from_slice(b"vids"),
            5 => put32(&mut fixture.main, 24, 3),
            _ => unreachable!(),
        }
        assert!(inspect_bytes(&fixture.bytes()).is_err(), "case {case}");
    }
}

#[test]
fn rejects_pcm_header_layout_and_sample_count_mismatches() {
    for case in 0..12 {
        let mut fixture = Fixture::new(false, 16, 2);
        match case {
            0 => put16(&mut fixture.audio_format, 0, 3),
            1 => put16(&mut fixture.audio_format, 2, 0),
            2 => put32(&mut fixture.audio_format, 4, 7_999),
            3 => put32(&mut fixture.audio_format, 8, 8_000),
            4 => put16(&mut fixture.audio_format, 12, 2),
            5 => put16(&mut fixture.audio_format, 14, 12),
            6 => put32(&mut fixture.audio_header, 24, 31_999),
            7 => put32(&mut fixture.audio_header, 32, 799),
            8 => put32(&mut fixture.audio_header, 44, 1),
            9 => fixture.audio_format.extend_from_slice(&[1, 0]),
            10 => fixture.media[1].1.push(0),
            11 => fixture.media[1].1.clear(),
            _ => unreachable!(),
        }
        assert!(inspect_bytes(&fixture.bytes()).is_err(), "case {case}");
    }
}

#[test]
fn rejects_video_headers_bad_jpeg_and_incomplete_raw_frames() {
    for case in 0..10 {
        let mut fixture = Fixture::new(case >= 7, 16, 2);
        match case {
            0 => put32(&mut fixture.main, 0, 40_000),
            1 => put32(&mut fixture.main, 16, 3),
            2 => put32(&mut fixture.video_header, 24, 0),
            3 => put32(&mut fixture.video_header, 28, 1),
            4 => put32(&mut fixture.video_format, 4, 8_193),
            5 => put16(&mut fixture.video_format, 14, 32),
            6 => {
                fixture.media[2].1.pop();
            }
            7 => fixture.media[2].1 = vec![0xff, 0xd8, 0xff, 0xd9],
            8 => fixture.media[2].1 = jpeg(2),
            9 => {
                fixture.media[2].1.pop();
            }
            _ => unreachable!(),
        }
        assert!(inspect_bytes(&fixture.bytes()).is_err(), "case {case}");
    }
}

#[test]
fn rejects_truncation_extra_bytes_and_forged_chunk_boundaries() {
    let original = Fixture::new(false, 24, 2).bytes();
    for length in [0, 11, original.len() - 1, original.len() - 10] {
        assert!(inspect_bytes(&original[..length]).is_err());
    }
    let mut extra = original.clone();
    extra.push(0);
    assert!(inspect_bytes(&extra).is_err());
    let mut boundary = original;
    put32(&mut boundary, 16, u32::MAX);
    assert!(inspect_bytes(&boundary).is_err());
}

#[test]
fn rejects_forged_duplicate_and_mixed_coordinate_indexes() {
    let original = Fixture::new(false, 16, 2).bytes();
    let index = original.windows(4).position(|w| w == b"idx1").unwrap();
    for (offset, value) in [(8, 0x62643032), (12, 1), (16, 1), (20, 3), (32, 4)] {
        let mut bytes = original.clone();
        put32(&mut bytes, index + offset, value);
        assert!(inspect_bytes(&bytes).is_err());
    }
    let mut duplicate = original.clone();
    duplicate.extend_from_slice(&original[index..]);
    let riff_len = duplicate.len() as u32 - 8;
    put32(&mut duplicate, 4, riff_len);
    assert!(inspect_bytes(&duplicate).is_err());
}

#[test]
fn rejects_publication_pixel_and_duration_limits() {
    let mut fixture = Fixture::new(false, 16, 2);
    put32(&mut fixture.main, 32, 8_193);
    assert!(inspect_bytes(&fixture.bytes()).is_err());
    let mut fixture = Fixture::new(false, 16, 2);
    put32(&mut fixture.video_header, 20, 2_000);
    put32(&mut fixture.video_header, 24, 1);
    put32(&mut fixture.main, 0, 2_000_000_000);
    assert!(inspect_bytes(&fixture.bytes()).is_err());
}

#[test]
#[ignore = "Requires the two fixed local native AVI acceptance outputs; does not run a model"]
fn validates_recorded_native_avi_outputs_without_transcoding() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/h3-gpu-native-20261112-r1");
    for (relative, expected_hash) in [
        (
            "t2va/t2va.avi",
            "53ba7cf790ce0e052129c33ec6f84a9b22b0b0b430726c89dac92e7beb67c4e3",
        ),
        (
            "first_frame_plus_text/first_frame_plus_text.avi",
            "4e5587bc3c2010f277c791ca307568e3f7fab81a10096f55cf23b9aff741aa16",
        ),
    ] {
        let mut file = File::open(root.join(relative)).unwrap();
        assert_eq!(
            crate::media::files::digest(&mut file).unwrap(),
            expected_hash
        );
        let size = file.metadata().unwrap().len();
        let result = inspect(&mut file, size).unwrap();
        assert_eq!(
            result.spec,
            MediaSpec::Video {
                width: 256,
                height: 256,
                duration_micros: 925_000,
                frame_count: 22,
                fps_numerator: 24,
                fps_denominator: 1,
            }
        );
    }
}
