use super::*;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

struct Fixture {
    store: MediaResultStore,
    root: MediaOutputRoot,
    _temp: tempfile::TempDir,
}

impl Fixture {
    async fn connect(path: &Path) -> MediaResultStore {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let store = MediaResultStore::new(pool);
        store.initialize().await.unwrap();
        store
    }

    fn temporary_directory() -> tempfile::TempDir {
        let base = PathBuf::from("E:/Codex/temp/lumina-media-results-tests");
        std::fs::create_dir_all(&base).unwrap();
        let base = std::fs::canonicalize(base).unwrap();
        tempfile::Builder::new()
            .prefix("media-")
            .tempdir_in(base)
            .unwrap()
    }

    async fn new() -> Self {
        let temp = Self::temporary_directory();
        let store = Self::connect(&temp.path().join("session-test.db")).await;
        let root = store
            .register_output_root(&temp.path().join("outputs"))
            .await
            .unwrap();
        Self {
            _temp: temp,
            store,
            root,
        }
    }

    #[cfg(windows)]
    async fn with_session_storage() -> (Self, crate::session::session_manager::SessionStorage) {
        let temp = Self::temporary_directory();
        let storage =
            crate::session::session_manager::SessionStorage::new(temp.path().to_path_buf());
        let store = MediaResultStore::new(storage.pool().await.unwrap().clone());
        let output_directory = storage.media_output_directory();
        assert!(output_directory.starts_with(temp.path()));
        let root = store.register_output_root(&output_directory).await.unwrap();
        assert!(root.path.starts_with(temp.path()));
        (
            Self {
                _temp: temp,
                store,
                root,
            },
            storage,
        )
    }

    fn request(&self, kind: MediaKind) -> PublishMediaRequest {
        PublishMediaRequest {
            result_id: uuid::Uuid::new_v4().to_string(),
            session_id: "session:one".into(),
            job_id: "job:one".into(),
            root_id: self.root.root_id.clone(),
            kind,
            association: MediaAssociation::ToolCall {
                tool_call_id: "call-1".into(),
            },
            source: MediaSource::Mcp {
                server_id: "test-server".into(),
                tool_name: "generate_media".into(),
                tool_call_id: "call-1".into(),
                input_refs: vec![],
            },
        }
    }

    async fn stage(&self, request: PublishMediaRequest, bytes: &[u8]) -> StagedMedia {
        let BeginMediaPublish::Staging(mut staged) =
            self.store.begin_publish(request).await.unwrap()
        else {
            panic!("Expected a new staging handle")
        };
        staged.write_all(bytes).unwrap();
        staged
    }

    async fn publish(&self, request: PublishMediaRequest, bytes: &[u8]) -> MediaResultRef {
        self.store
            .publish(self.stage(request, bytes).await)
            .await
            .unwrap()
    }
}

fn png() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(3, 2, image::Rgb([255, 0, 0]));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, image::ImageOutputFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn riff_chunk(tag: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    result.extend_from_slice(tag);
    result.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    result.extend_from_slice(bytes);
    if bytes.len() % 2 != 0 {
        result.push(0);
    }
    result
}

fn wav(silent: bool) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1_u16.to_le_bytes());
    fmt.extend_from_slice(&1_u16.to_le_bytes());
    fmt.extend_from_slice(&8_000_u32.to_le_bytes());
    fmt.extend_from_slice(&16_000_u32.to_le_bytes());
    fmt.extend_from_slice(&2_u16.to_le_bytes());
    fmt.extend_from_slice(&16_u16.to_le_bytes());
    let mut data = vec![0; 16_000];
    if !silent {
        data[..2].copy_from_slice(&1_000_i16.to_le_bytes());
    }
    let mut body = b"WAVE".to_vec();
    body.extend(riff_chunk(b"fmt ", &fmt));
    body.extend(riff_chunk(b"data", &data));
    riff_chunk(b"RIFF", &body)
}

fn float_wav(last_sample: f32) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&3_u16.to_le_bytes());
    fmt.extend_from_slice(&1_u16.to_le_bytes());
    fmt.extend_from_slice(&8_000_u32.to_le_bytes());
    fmt.extend_from_slice(&32_000_u32.to_le_bytes());
    fmt.extend_from_slice(&4_u16.to_le_bytes());
    fmt.extend_from_slice(&32_u16.to_le_bytes());
    let mut data = vec![0; 80_000];
    data[79_996..].copy_from_slice(&last_sample.to_le_bytes());
    let mut body = b"WAVE".to_vec();
    body.extend(riff_chunk(b"fmt ", &fmt));
    body.extend(riff_chunk(b"data", &data));
    riff_chunk(b"RIFF", &body)
}

fn avi(static_pixels: bool) -> Vec<u8> {
    avi_with_frames(
        [
            vec![0, 0, 255, 0],
            if static_pixels {
                vec![0, 0, 255, 0]
            } else {
                vec![255, 0, 0, 0]
            },
        ],
        false,
    )
}

fn jpeg() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0]));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, image::ImageOutputFormat::Jpeg(90))
        .unwrap();
    bytes.into_inner()
}

fn avi_with_frames(frame_data: [Vec<u8>; 2], jpeg: bool) -> Vec<u8> {
    let mut avih = vec![0; 56];
    for (offset, value) in [(0, 500_000_u32), (16, 2), (24, 1), (32, 1), (36, 1)] {
        avih[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    let mut strh = vec![0; 56];
    strh[..4].copy_from_slice(b"vids");
    strh[4..8].copy_from_slice(if jpeg { b"MJPG" } else { b"DIB " });
    for (offset, value) in [(20, 1_u32), (24, 2), (32, 2)] {
        strh[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    let mut strf = vec![0; 40];
    for (offset, value) in [(0, 40_u32), (4, 1), (8, 1)] {
        strf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    strf[12..14].copy_from_slice(&1_u16.to_le_bytes());
    strf[14..16].copy_from_slice(&24_u16.to_le_bytes());
    if jpeg {
        strf[16..20].copy_from_slice(b"MJPG");
    }
    let mut stream = b"strl".to_vec();
    stream.extend(riff_chunk(b"strh", &strh));
    stream.extend(riff_chunk(b"strf", &strf));
    let mut header = b"hdrl".to_vec();
    header.extend(riff_chunk(b"avih", &avih));
    header.extend(riff_chunk(b"LIST", &stream));
    let mut frames = b"movi".to_vec();
    for frame in frame_data {
        frames.extend(riff_chunk(if jpeg { b"00dc" } else { b"00db" }, &frame));
    }
    let mut body = b"AVI ".to_vec();
    body.extend(riff_chunk(b"LIST", &header));
    body.extend(riff_chunk(b"LIST", &frames));
    riff_chunk(b"RIFF", &body)
}

#[cfg(windows)]
#[tokio::test]
async fn published_metadata_is_measured_and_queries_are_session_bound() {
    let fixture = Fixture::new().await;
    let request = fixture.request(MediaKind::Image);
    let bytes = png();
    let result = fixture.publish(request.clone(), &bytes).await;
    assert_eq!(result.bytes, bytes.len() as u64);
    assert_eq!(result.mime, "image/png");
    assert_eq!(
        result.spec,
        MediaSpec::Image {
            width: 3,
            height: 2
        }
    );
    assert_eq!(result.ownership, MediaOwnership::ApplicationImported);
    assert!(
        result.created_at > 1_000_000_000_000
            && result.created_at <= chrono::Utc::now().timestamp_millis()
    );
    assert!(matches!(result.source, MediaSource::Mcp { .. }));
    assert!(fixture
        .store
        .read("another-session", &result.result_id)
        .await
        .unwrap()
        .is_none());
    assert!(fixture
        .store
        .open_content("another-session", &result.result_id)
        .await
        .is_err());
    let mut content = fixture
        .store
        .open_content(&result.session_id, &result.result_id)
        .await
        .unwrap();
    assert_eq!(content.read_range(0, bytes.len()).unwrap(), bytes);
    assert!(content.read_range(result.bytes, 1).is_err());
    assert!(content.read_range(0, MAX_MEDIA_READ_BYTES + 1).is_err());
    drop(content);
    let BeginMediaPublish::Existing(repeated) =
        fixture.store.begin_publish(request.clone()).await.unwrap()
    else {
        panic!("Duplicate result was not idempotent")
    };
    assert_eq!(repeated, result);
    let mut conflict = request;
    conflict.job_id = "different-job".into();
    assert!(fixture.store.begin_publish(conflict).await.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn recovery_handles_both_sides_of_atomic_file_publication() {
    for renamed in [false, true] {
        let fixture = Fixture::new().await;
        let request = fixture.request(MediaKind::Image);
        let staged = fixture.stage(request.clone(), &png()).await;
        let prepared = fixture.store.prepare(staged).await.unwrap();
        if renamed {
            files::rename(
                &prepared.staged.file,
                &prepared
                    .staged
                    .directory
                    .path
                    .join(&prepared.result.file_name),
            )
            .unwrap();
        }
        let result = prepared.result.clone();
        drop(prepared);
        assert_eq!(
            fixture
                .store
                .read(&request.session_id, &request.result_id)
                .await
                .unwrap()
                .unwrap()
                .availability,
            MediaAvailability::Publishing
        );
        assert!(fixture
            .store
            .open_content(&request.session_id, &request.result_id)
            .await
            .is_err());
        fixture.store.pool.close().await;
        let restarted = Fixture::connect(&fixture._temp.path().join("session-test.db")).await;
        assert_eq!(
            restarted.recover().await.unwrap(),
            vec![MediaRecoveryOutcome {
                result_id: request.result_id.clone(),
                availability: MediaAvailability::Available
            }]
        );
        assert!(restarted.recover().await.unwrap().is_empty());
        assert_eq!(
            restarted
                .read(&request.session_id, &request.result_id)
                .await
                .unwrap()
                .unwrap()
                .result,
            result
        );
    }
}

#[cfg(windows)]
#[tokio::test]
async fn interrupted_unvalidated_output_is_never_promoted() {
    let fixture = Fixture::new().await;
    let request = fixture.request(MediaKind::Image);
    drop(fixture.stage(request.clone(), b"incomplete").await);
    assert_eq!(
        fixture.store.recover().await.unwrap()[0].availability,
        MediaAvailability::Publishing
    );
    assert!(fixture
        .store
        .read(&request.session_id, &request.result_id)
        .await
        .unwrap()
        .is_none());
    assert!(fixture
        .store
        .open_content(&request.session_id, &request.result_id)
        .await
        .is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn root_configuration_changes_do_not_retarget_existing_results() {
    let fixture = Fixture::new().await;
    let first = fixture
        .publish(fixture.request(MediaKind::Image), &png())
        .await;
    let second_root = fixture
        .store
        .register_output_root(&fixture._temp.path().join("new-outputs"))
        .await
        .unwrap();
    let mut request = fixture.request(MediaKind::Image);
    request.root_id = second_root.root_id.clone();
    let second = fixture.publish(request, &png()).await;
    assert_ne!(first.root_id, second.root_id);
    let first_handle = fixture
        .store
        .open_content(&first.session_id, &first.result_id)
        .await
        .unwrap();
    let second_handle = fixture
        .store
        .open_content(&second.session_id, &second.result_id)
        .await
        .unwrap();
    assert!(first_handle.path().starts_with(&fixture.root.path));
    assert!(second_handle.path().starts_with(&second_root.path));
}

#[cfg(windows)]
#[tokio::test]
async fn atomic_rename_preserves_exact_names_at_every_utf16_alignment() {
    use std::os::windows::ffi::OsStrExt;

    let fixture = Fixture::new().await;
    let root = checked_root(&fixture.root).unwrap();
    for prefix in ["ascii", "结果🦀"] {
        for alignment in 0..4 {
            let directory = root
                .child(&format!("rename-{prefix}-{alignment}"), true)
                .unwrap();
            let mut writer = directory.open_file("output.pending", true, true).unwrap();
            let expected_identity = files::identity(&writer, false).unwrap();
            let bytes = png();
            writer.write_all(&bytes).unwrap();
            let base = format!("{prefix}.png");
            let units = directory.path.join(&base).as_os_str().encode_wide().count();
            let padding = (alignment + 4 - units % 4) % 4;
            let name = format!("{}{base}", "x".repeat(padding));
            let destination = directory.path.join(&name);
            assert_eq!(destination.as_os_str().encode_wide().count() % 4, alignment);

            files::rename(&writer, &destination).unwrap();
            drop(writer);

            let mut published = directory.open_file(&name, false, false).unwrap();
            assert_eq!(
                files::identity(&published, false).unwrap(),
                expected_identity
            );
            let mut actual = Vec::new();
            published.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, bytes);
            let entries: Vec<_> = std::fs::read_dir(&directory.path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(entries, vec![std::ffi::OsString::from(name)]);
            assert!(std::fs::OpenOptions::new()
                .write(true)
                .open(&destination)
                .is_err());
            assert!(std::fs::remove_file(&destination).is_err());
        }
    }
}

#[cfg(windows)]
#[tokio::test]
async fn absent_altered_and_same_name_replaced_assets_are_distinct() {
    let fixture = Fixture::new().await;
    for mode in 0..3 {
        let request = fixture.request(MediaKind::Image);
        let result = fixture.publish(request.clone(), &png()).await;
        let path = fixture.root.path.join(&result.relative_path);
        match mode {
            0 => std::fs::remove_file(&path).unwrap(),
            1 => {
                let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.write_all(b"bad").unwrap();
            }
            _ => {
                std::fs::rename(&path, path.with_extension("original")).unwrap();
                std::fs::write(&path, png()).unwrap();
            }
        }
        let state = fixture
            .store
            .read(&result.session_id, &result.result_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.result, result);
        assert_eq!(
            state.availability,
            if mode == 0 {
                MediaAvailability::Missing
            } else {
                MediaAvailability::Tampered
            }
        );
        assert!(fixture.store.recover().await.unwrap().is_empty());
        assert!(fixture.store.begin_publish(request).await.is_err());
    }
}

#[cfg(windows)]
#[tokio::test]
async fn verified_handle_blocks_write_delete_and_ancestor_replacement() {
    let fixture = Fixture::new().await;
    let bytes = png();
    let result = fixture
        .publish(fixture.request(MediaKind::Image), &bytes)
        .await;
    let mut content = fixture
        .store
        .open_content(&result.session_id, &result.result_id)
        .await
        .unwrap();
    let path = content.path().to_path_buf();
    assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
    assert!(std::fs::remove_file(&path).is_err());
    assert!(std::fs::rename(&path, path.with_extension("replaced")).is_err());
    assert!(std::fs::rename(
        path.parent().unwrap(),
        path.parent().unwrap().with_extension("moved")
    )
    .is_err());
    assert_eq!(content.read_range(0, bytes.len()).unwrap(), bytes);
    drop(content);
    let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    assert_eq!(
        fixture
            .store
            .read(&result.session_id, &result.result_id)
            .await
            .unwrap()
            .unwrap()
            .availability,
        MediaAvailability::AccessDenied
    );
    drop(writer);
    assert_eq!(
        fixture
            .store
            .read(&result.session_id, &result.result_id)
            .await
            .unwrap()
            .unwrap()
            .availability,
        MediaAvailability::Available
    );
}

#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    use std::os::windows::process::CommandExt;
    let path = |p: &Path| p.to_str().unwrap().trim_start_matches(r"\\?\").to_owned();
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(path(link))
        .arg(path(target))
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[cfg(windows)]
#[tokio::test]
async fn junction_and_recreated_output_root_are_rejected() {
    let fixture = Fixture::new().await;
    let result = fixture
        .publish(fixture.request(MediaKind::Image), &png())
        .await;
    let path = fixture.root.path.join(&result.relative_path);
    let job = path.parent().unwrap();
    let external = fixture._temp.path().join("external");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join(&result.file_name), png()).unwrap();
    std::fs::rename(job, job.with_extension("original")).unwrap();
    junction(job, &external);
    assert_eq!(
        fixture
            .store
            .read(&result.session_id, &result.result_id)
            .await
            .unwrap()
            .unwrap()
            .availability,
        MediaAvailability::UnsafePath
    );
    std::fs::remove_dir(job).unwrap();
    std::fs::rename(&fixture.root.path, fixture._temp.path().join("old-root")).unwrap();
    std::fs::create_dir(&fixture.root.path).unwrap();
    assert_eq!(
        fixture
            .store
            .read(&result.session_id, &result.result_id)
            .await
            .unwrap()
            .unwrap()
            .availability,
        MediaAvailability::UnsafePath
    );
}

#[cfg(windows)]
#[tokio::test]
async fn publication_never_overwrites_an_existing_final_name() {
    let fixture = Fixture::new().await;
    let request = fixture.request(MediaKind::Image);
    let staged = fixture.stage(request.clone(), &png()).await;
    let prepared = fixture.store.prepare(staged).await.unwrap();
    let path = prepared
        .staged
        .directory
        .path
        .join(&prepared.result.file_name);
    std::fs::write(&path, b"existing unrelated bytes").unwrap();
    assert!(fixture.store.commit(prepared).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"existing unrelated bytes");
    assert_eq!(
        fixture.store.recover().await.unwrap()[0].availability,
        MediaAvailability::Tampered
    );
}

#[cfg(windows)]
#[tokio::test]
async fn complete_audio_and_video_frames_produce_measured_specs() {
    let fixture = Fixture::new().await;
    for (bytes, sample_frames) in [
        (wav(false), 8_000),
        (wav(true), 8_000),
        (float_wav(0.0), 20_000),
    ] {
        let audio = fixture
            .publish(fixture.request(MediaKind::Audio), &bytes)
            .await;
        assert_eq!(audio.bytes, bytes.len() as u64);
        assert_eq!(
            audio.spec,
            MediaSpec::Audio {
                duration_micros: sample_frames * 1_000_000 / 8_000,
                sample_rate: 8_000,
                channels: 1,
                sample_frames
            }
        );
        let mut content = fixture
            .store
            .open_content(&audio.session_id, &audio.result_id)
            .await
            .unwrap();
        assert_eq!(content.read_range(0, bytes.len()).unwrap(), bytes);
    }
    for bytes in [
        avi(false),
        avi(true),
        avi_with_frames([jpeg(), jpeg()], true),
    ] {
        let video = fixture
            .publish(fixture.request(MediaKind::Video), &bytes)
            .await;
        assert_eq!(video.bytes, bytes.len() as u64);
        assert_eq!(
            video.spec,
            MediaSpec::Video {
                width: 1,
                height: 1,
                duration_micros: 1_000_000,
                frame_count: 2,
                fps_numerator: 2,
                fps_denominator: 1
            }
        );
        let mut content = fixture
            .store
            .open_content(&video.session_id, &video.result_id)
            .await
            .unwrap();
        assert_eq!(content.read_range(0, bytes.len()).unwrap(), bytes);
    }
}

#[cfg(windows)]
#[tokio::test]
async fn truncated_nonfinite_wrong_kind_and_unverified_codecs_are_not_ready() {
    let fixture = Fixture::new().await;
    let mut truncated = png();
    truncated.pop();
    let mut truncated_audio = wav(true);
    truncated_audio.pop();
    let mut truncated_video = avi(true);
    truncated_video.pop();
    for (kind, bytes) in [
        (MediaKind::Image, truncated),
        (MediaKind::Audio, truncated_audio),
        (MediaKind::Video, truncated_video),
        (MediaKind::Image, wav(false)),
        (
            MediaKind::Video,
            b"\0\0\0\x18ftypmp42declared video only".to_vec(),
        ),
    ] {
        let request = fixture.request(kind);
        let staged = fixture.stage(request.clone(), &bytes).await;
        assert!(fixture.store.publish(staged).await.is_err());
        assert!(fixture
            .store
            .read(&request.session_id, &request.result_id)
            .await
            .unwrap()
            .is_none());
        assert!(fixture
            .store
            .open_content(&request.session_id, &request.result_id)
            .await
            .is_err());
    }
    let mut missing_track = avi(false);
    let offset = missing_track
        .windows(4)
        .position(|bytes| bytes == b"strh")
        .unwrap();
    missing_track[offset..offset + 4].copy_from_slice(b"JUNK");
    let mut unsupported_codec = avi(false);
    let offset = unsupported_codec
        .windows(4)
        .position(|bytes| bytes == b"DIB ")
        .unwrap();
    unsupported_codec[offset..offset + 4].copy_from_slice(b"H264");
    for (bytes, expected) in [
        (
            avi_with_frames([jpeg(), vec![0, 1, 2]], true),
            "JPEG has no terminal",
        ),
        (missing_track, "Missing or duplicate required media chunk"),
        (
            unsupported_codec,
            "does not have a complete publication validator",
        ),
    ] {
        let request = fixture.request(MediaKind::Video);
        let staged = fixture.stage(request.clone(), &bytes).await;
        let error = fixture.store.publish(staged).await.unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        assert!(fixture
            .store
            .read(&request.session_id, &request.result_id)
            .await
            .unwrap()
            .is_none());
        assert!(fixture
            .store
            .open_content(&request.session_id, &request.result_id)
            .await
            .is_err());
    }
    for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let request = fixture.request(MediaKind::Audio);
        let staged = fixture.stage(request.clone(), &float_wav(sample)).await;
        let error = fixture.store.publish(staged).await.unwrap_err();
        assert!(error.to_string().contains("nonfinite samples"), "{error:#}");
        assert!(fixture
            .store
            .read(&request.session_id, &request.result_id)
            .await
            .unwrap()
            .is_none());
        assert!(fixture
            .store
            .open_content(&request.session_id, &request.result_id)
            .await
            .is_err());
    }
}

#[test]
fn identifiers_are_injective_safe_segments_and_associations_are_required() {
    let values = ["../escape", "C:\\outside", "A", "a", "con", "name.", "会话"];
    let paths: std::collections::BTreeSet<_> =
        values.iter().map(|value| domain::segment(value)).collect();
    assert_eq!(paths.len(), values.len());
    assert!(paths.iter().all(|path| path
        .bytes()
        .all(|b| b.is_ascii_hexdigit() || b == b'i' || b == b'-')));
    for value in values {
        let path = domain::segment(value);
        let bytes: Vec<u8> = path[3..]
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert_eq!(String::from_utf8(bytes).unwrap(), value);
    }
    assert!(serde_json::from_value::<MediaAssociation>(serde_json::json!({})).is_err());
}

#[cfg(windows)]
fn rename_prepared(prepared: &PreparedMedia) {
    files::rename(
        &prepared.staged.file,
        &prepared
            .staged
            .directory
            .path
            .join(&prepared.result.file_name),
    )
    .unwrap();
}

#[cfg(windows)]
async fn publication_state(store: &MediaResultStore, result_id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM media_results_v1 WHERE result_id=?")
        .bind(result_id)
        .fetch_one(&store.pool)
        .await
        .unwrap()
}

#[cfg(windows)]
#[tokio::test]
async fn session_storage_startup_recovers_both_sides_of_rename_after_real_database_close() {
    use crate::session::session_manager::SessionStorage;
    for renamed in [false, true] {
        let (fixture, storage) = Fixture::with_session_storage().await;
        let request = fixture.request(MediaKind::Image);
        let prepared = fixture
            .store
            .prepare(fixture.stage(request.clone(), &png()).await)
            .await
            .unwrap();
        if renamed {
            rename_prepared(&prepared);
        }
        let expected = prepared.result.clone();
        drop(prepared);
        assert_eq!(
            publication_state(&fixture.store, &request.result_id).await,
            "prepared"
        );
        fixture.store.pool.close().await;
        drop(storage);

        let reopened = SessionStorage::new(fixture._temp.path().to_path_buf());
        let store = MediaResultStore::new(reopened.pool().await.unwrap().clone());
        assert_eq!(publication_state(&store, &request.result_id).await, "ready");
        let content = store
            .open_content(&request.session_id, &request.result_id)
            .await
            .unwrap();
        assert_eq!(content.result(), &expected);
        drop(content);
        assert!(store.recover().await.unwrap().is_empty());
        store.pool.close().await;
    }
}

#[cfg(windows)]
#[tokio::test]
async fn session_storage_startup_does_not_recover_or_remove_a_live_writer() {
    use crate::session::session_manager::SessionStorage;
    let (fixture, _storage) = Fixture::with_session_storage().await;
    let request = fixture.request(MediaKind::Image);
    let bytes = png();
    let mut staged = fixture.stage(request.clone(), &bytes[..12]).await;
    let identity = staged.identity.clone();

    let second = SessionStorage::new(fixture._temp.path().to_path_buf());
    let store = MediaResultStore::new(second.pool().await.unwrap().clone());
    assert_eq!(
        publication_state(&store, &request.result_id).await,
        "writing"
    );
    assert_eq!(files::identity(&staged.file, false).unwrap(), identity);
    assert_eq!(staged.file.metadata().unwrap().len(), 12);
    assert!(store
        .read(&request.session_id, &request.result_id)
        .await
        .unwrap()
        .is_none());
    staged.write_all(&bytes[12..]).unwrap();
    let result = fixture.store.publish(staged).await.unwrap();
    let content = store
        .open_content(&request.session_id, &request.result_id)
        .await
        .unwrap();
    assert_eq!(content.result(), &result);
    drop(content);
    store.pool.close().await;
    fixture.store.pool.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn session_storage_startup_cannot_take_a_live_prepared_publication_before_or_after_rename() {
    use crate::session::session_manager::SessionStorage;
    for renamed in [false, true] {
        let (fixture, _storage) = Fixture::with_session_storage().await;
        let request = fixture.request(MediaKind::Image);
        let mut prepared = fixture
            .store
            .prepare(fixture.stage(request.clone(), &png()).await)
            .await
            .unwrap();
        if renamed {
            rename_prepared(&prepared);
        }
        let expected = prepared.result.clone();
        let identity = prepared.staged.identity.clone();
        let second = SessionStorage::new(fixture._temp.path().to_path_buf());
        let store = MediaResultStore::new(second.pool().await.unwrap().clone());
        assert_eq!(
            publication_state(&store, &request.result_id).await,
            "prepared"
        );
        assert_eq!(
            files::identity(&prepared.staged.file, false).unwrap(),
            identity
        );
        assert_eq!(
            files::digest(&mut prepared.staged.file).unwrap(),
            expected.sha256
        );
        assert_eq!(
            store
                .read(&request.session_id, &request.result_id)
                .await
                .unwrap()
                .unwrap()
                .availability,
            MediaAvailability::Publishing
        );
        assert_eq!(
            store.recover().await.unwrap(),
            vec![MediaRecoveryOutcome {
                result_id: request.result_id.clone(),
                availability: MediaAvailability::AccessDenied,
            }]
        );
        if renamed {
            fixture
                .store
                .mark_ready(&prepared.result, &identity)
                .await
                .unwrap();
            drop(prepared);
        } else {
            assert_eq!(fixture.store.commit(prepared).await.unwrap(), expected);
        }
        let content = store
            .open_content(&request.session_id, &request.result_id)
            .await
            .unwrap();
        assert_eq!(content.result(), &expected);
        drop(content);
        store.pool.close().await;
        fixture.store.pool.close().await;
    }
}

#[cfg(windows)]
#[tokio::test]
async fn two_independent_session_storages_can_recover_the_same_prepared_media_concurrently() {
    use crate::session::session_manager::SessionStorage;
    for renamed in [false, true] {
        let (fixture, storage) = Fixture::with_session_storage().await;
        let request = fixture.request(MediaKind::Image);
        let prepared = fixture
            .store
            .prepare(fixture.stage(request.clone(), &png()).await)
            .await
            .unwrap();
        if renamed {
            rename_prepared(&prepared);
        }
        let expected = prepared.result.clone();
        drop(prepared);
        fixture.store.pool.close().await;
        drop(storage);

        let first = SessionStorage::new(fixture._temp.path().to_path_buf());
        let second = SessionStorage::new(fixture._temp.path().to_path_buf());
        let (first_pool, second_pool) = tokio::join!(first.pool(), second.pool());
        let first_store = MediaResultStore::new(first_pool.unwrap().clone());
        let second_store = MediaResultStore::new(second_pool.unwrap().clone());
        for store in [&first_store, &second_store] {
            assert_eq!(publication_state(store, &request.result_id).await, "ready");
            let content = store
                .open_content(&request.session_id, &request.result_id)
                .await
                .unwrap();
            assert_eq!(content.result(), &expected);
        }
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_results_v1 WHERE result_id=?")
                .bind(&request.result_id)
                .fetch_one(&first_store.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        let entries: Vec<_> =
            std::fs::read_dir(fixture.root.path.join(relative_directory(&request)))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
        assert_eq!(entries, vec![expected.file_name]);
        assert!(first_store.recover().await.unwrap().is_empty());
        first_store.pool.close().await;
        second_store.pool.close().await;
    }
}

#[cfg(windows)]
#[tokio::test]
async fn corrupt_first_journal_does_not_stop_startup_recovery_or_normal_conversations() {
    use crate::session::session_manager::{SessionManager, SessionType};
    let (fixture, storage) = Fixture::with_session_storage().await;
    let mut broken = fixture.request(MediaKind::Image);
    broken.result_id = "00000000-0000-4000-8000-000000000001".into();
    let mut healthy = fixture.request(MediaKind::Image);
    healthy.result_id = "00000000-0000-4000-8000-000000000002".into();
    for request in [&broken, &healthy] {
        drop(
            fixture
                .store
                .prepare(fixture.stage(request.clone(), &png()).await)
                .await
                .unwrap(),
        );
    }
    sqlx::query("UPDATE media_results_v1 SET request_payload=? WHERE result_id=?")
        .bind("{invalid-json")
        .bind(&broken.result_id)
        .execute(&fixture.store.pool)
        .await
        .unwrap();
    fixture.store.pool.close().await;
    drop(storage);

    let manager = SessionManager::new(fixture._temp.path().to_path_buf());
    let session = manager
        .create_session(
            fixture._temp.path().to_path_buf(),
            "ordinary session".into(),
            SessionType::User,
            crate::config::LuminaMode::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        manager.get_session(&session.id, false).await.unwrap().name,
        "ordinary session"
    );
    let store = MediaResultStore::new(manager.storage().pool().await.unwrap().clone());
    assert_eq!(
        publication_state(&store, &broken.result_id).await,
        "prepared"
    );
    assert_eq!(publication_state(&store, &healthy.result_id).await, "ready");
    let content = store
        .open_content(&healthy.session_id, &healthy.result_id)
        .await
        .unwrap();
    assert_eq!(content.result().result_id, healthy.result_id);
    drop(content);
    assert_eq!(
        store.recover().await.unwrap(),
        vec![MediaRecoveryOutcome {
            result_id: broken.result_id,
            availability: MediaAvailability::IoUnavailable,
        }]
    );
    store.pool.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn media_recovery_query_failure_does_not_poison_session_storage_initialization() {
    use crate::session::session_manager::{SessionManager, SessionType};
    let (fixture, storage) = Fixture::with_session_storage().await;
    sqlx::query("ALTER TABLE media_results_v1 RENAME COLUMN state TO unavailable_state")
        .execute(&fixture.store.pool)
        .await
        .unwrap();
    fixture.store.pool.close().await;
    drop(storage);
    let manager = SessionManager::new(fixture._temp.path().to_path_buf());
    let session = manager
        .create_session(
            fixture._temp.path().to_path_buf(),
            "recovery unavailable".into(),
            SessionType::User,
            crate::config::LuminaMode::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        manager.get_session(&session.id, false).await.unwrap().name,
        "recovery unavailable"
    );
    manager.storage().pool().await.unwrap().close().await;
}
