use super::*;
#[cfg(windows)]
use lumina_providers::local_inference::lifecycle::*;
#[cfg(windows)]
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::io::Cursor;
use std::path::PathBuf;

#[cfg(windows)]
#[path = "../../tests/support/task_resource_publication_probe.rs"]
mod resource_probe;

#[cfg(windows)]
#[path = "../../tests/support/sd15_publication_probe.rs"]
mod sd15_probe;

fn base() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/task-output-unit-tests");
    std::fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

fn reader(name: &str, bytes: &[u8]) -> OutputReader {
    let mut file = tempfile::tempfile_in(base()).unwrap();
    file.write_all(bytes).unwrap();
    OutputReader {
        file,
        name: name.into(),
        bytes: bytes.len() as u64,
        sha256: Sha256Digest::of_bytes(bytes).as_str().into(),
    }
}

fn png(width: u32, height: u32, color: u8) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([color, 20, 30]));
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut out, image::ImageOutputFormat::Png)
        .unwrap();
    out.into_inner()
}

fn wav(rate: u32, channels: u16, bits: u16) -> Vec<u8> {
    let block = channels * bits / 8;
    let size = u32::from(block) * 2;
    let mut wav = b"RIFF".to_vec();
    wav.extend((36 + size).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(channels.to_le_bytes());
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * u32::from(block)).to_le_bytes());
    wav.extend(block.to_le_bytes());
    wav.extend(bits.to_le_bytes());
    wav.extend(b"data");
    wav.extend(size.to_le_bytes());
    wav.resize(wav.len() + size as usize, 0);
    wav
}

#[test]
fn preflight_checks_decoded_geometry_format_names_and_collected_digest() {
    let expected = ExpectedOutput::Image {
        format: GenerationOutputFormat::Png,
        width: 64,
        height: 64,
    };
    let names = vec!["output.png".into()];
    assert!(prepare_readers(
        expected,
        &names,
        vec![reader("output.png", &png(64, 64, 1))]
    )
    .is_ok());
    for item in [
        reader("output.png", &png(63, 64, 1)),
        reader("other.png", &png(64, 64, 1)),
        reader("output.png", &wav(24000, 1, 16)),
    ] {
        assert!(prepare_readers(expected, &names, vec![item]).is_err());
    }
    let mut bad = reader("output.png", &png(64, 64, 1));
    bad.sha256 = "0".repeat(64);
    assert!(prepare_readers(expected, &names, vec![bad]).is_err());
    assert!(prepare_readers(expected, &names, Vec::new()).is_err());
}

#[test]
fn pocket_output_requires_the_frozen_pcm_format_beyond_general_wav_validation() {
    let names = vec!["output.wav".into()];
    assert!(prepare_readers(
        ExpectedOutput::Speech,
        &names,
        vec![reader("output.wav", &wav(24_000, 1, 16))]
    )
    .is_ok());
    for bytes in [wav(16_000, 1, 16), wav(24_000, 2, 16), wav(24_000, 1, 24)] {
        let mut file = reader("output.wav", &bytes);
        assert!(formats::inspect(&mut file.file).is_ok());
        assert!(prepare_readers(ExpectedOutput::Speech, &names, vec![file]).is_err());
    }
}

#[test]
fn transcript_preserves_exact_unicode_and_silent_transcriptions() {
    let names = vec!["output.txt".into()];
    for text in ["木桌上的苹果。\r\n", "\n"] {
        let CheckedOutput::Text {
            input,
            text: actual,
        } = prepare_readers(
            ExpectedOutput::Transcript,
            &names,
            vec![reader("output.txt", text.as_bytes())],
        )
        .unwrap()
        else {
            panic!("Expected transcript")
        };
        assert_eq!(actual, text);
        assert_eq!(
            input.sha256,
            Sha256Digest::of_bytes(text.as_bytes()).as_str()
        );
        assert_eq!(input.bytes, text.len() as u64);
    }
    for input in [
        b"\xff".as_slice(),
        b"a\0b".as_slice(),
        b"\x1b[31m".as_slice(),
        b"".as_slice(),
    ] {
        assert!(prepare_readers(
            ExpectedOutput::Transcript,
            &names,
            vec![reader("output.txt", input)]
        )
        .is_err());
    }
}

#[test]
fn copying_checks_the_final_chunk_length_and_digest() {
    let bytes = vec![0xa7; 140_001];
    let mut source = reader("output", &bytes);
    let mut target = Vec::new();
    copy_verified(&mut source, &mut target).unwrap();
    assert_eq!(target, bytes);
    source.sha256 = "1".repeat(64);
    assert!(copy_verified(&mut source, &mut std::io::sink()).is_err());
    source.bytes -= 1;
    assert!(copy_verified(&mut source, &mut std::io::sink()).is_err());
}

#[cfg(windows)]
struct Fixture {
    pool: sqlx::SqlitePool,
    media: MediaResultStore,
    config: TaskConfigurationStore,
    root: super::super::MediaOutputRoot,
    _temp: tempfile::TempDir,
}

#[cfg(windows)]
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("import-")
            .tempdir_in(base())
            .unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(temp.path().join("session.db"))
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE sessions(id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions(id) VALUES('session-a')")
            .execute(&pool)
            .await
            .unwrap();
        let media = MediaResultStore::new(pool.clone());
        media.initialize().await.unwrap();
        let config = TaskConfigurationStore::new(pool.clone());
        config.initialize().await.unwrap();
        let root = media
            .register_output_root(&temp.path().join("outputs"))
            .await
            .unwrap();
        Self {
            pool,
            media,
            config,
            root,
            _temp: temp,
        }
    }

    async fn plan(&self, job: &str, batch: u32) -> NativeTaskPlan {
        let profile = GenerationProfile::Sd15CpuTextToImage;
        let service = GenerationParameterService::new(
            profile,
            GenerationModelBinding {
                installation_id: "test-installation".into(),
                version_id: "test-version".into(),
                manifest_digest: Sha256Digest::of_bytes(b"synthetic manifest"),
                runtime: profile.runtime_build(),
                target_capabilities: profile.required_capabilities(),
                assets: profile
                    .recorded_assets()
                    .into_iter()
                    .map(|(role, (sha256, size_bytes))| {
                        (
                            role,
                            GenerationAssetBinding {
                                artifact_id: format!("test-{role:?}"),
                                sha256,
                                size_bytes,
                            },
                        )
                    })
                    .collect(),
            },
        )
        .unwrap();
        let task = service
            .compile(GenerationTaskRequest {
                session_id: "session-a".into(),
                job_id: job.into(),
                parameters: GenerationParameters::Image(Sd15ImageParameters {
                    prompt: Some("An apple".into()),
                    width: Some(64),
                    height: Some(64),
                    batch_count: Some(batch),
                    ..Default::default()
                }),
            })
            .unwrap();
        self.config.save_generation(&task).await.unwrap();
        task.into()
    }

    async fn import(
        &self,
        plan: &NativeTaskPlan,
        readers: Vec<OutputReader>,
    ) -> Result<ImportedTaskOutput> {
        import_readers(
            &self.config,
            &self.media,
            plan,
            &plan.snapshot_digest()?,
            readers,
            &self.root.root_id,
            association(),
        )
        .await
    }
}

#[cfg(windows)]
fn association() -> MediaAssociation {
    MediaAssociation::Message {
        message_id: "assistant-message".into(),
    }
}

#[cfg(windows)]
#[tokio::test]
async fn validates_entire_batch_before_publishing_then_retries_without_duplicates() {
    let fixture = Fixture::new().await;
    let plan = fixture.plan("batch", 2).await;
    let first = png(64, 64, 1);
    let second = png(64, 64, 2);
    assert!(fixture
        .import(
            &plan,
            vec![
                reader("output_0.png", &first),
                reader("output_1.png", &png(1, 1, 2))
            ]
        )
        .await
        .is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM media_results_v1")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let make = || {
        vec![
            reader("output_0.png", &first),
            reader("output_1.png", &second),
        ]
    };
    let result = fixture.import(&plan, make()).await.unwrap();
    let ImportedTaskOutput::Media(ref images) = result else {
        panic!("Expected images")
    };
    assert_eq!(images.len(), 2);
    assert_ne!(images[0].result_id, images[1].result_id);
    for (image, bytes) in images.iter().zip([&first, &second]) {
        let mut content = fixture
            .media
            .open_content(plan.session_id(), &image.result_id)
            .await
            .unwrap();
        assert_eq!(content.read_range(0, bytes.len()).unwrap(), *bytes);
        fixture.config.verify_result(image).await.unwrap();
    }
    assert_eq!(result, fixture.import(&plan, make()).await.unwrap());
    assert!(fixture
        .import(
            &plan,
            vec![
                reader("output_0.png", &second),
                reader("output_1.png", &second)
            ]
        )
        .await
        .is_err());
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM media_results_v1 WHERE state='ready'")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(count, 2);
}

#[cfg(windows)]
#[tokio::test]
async fn rejects_another_execution_digest_before_any_publication() {
    let fixture = Fixture::new().await;
    let first = fixture.plan("first", 1).await;
    let second = fixture.plan("second", 1).await;
    let result = import_readers(
        &fixture.config,
        &fixture.media,
        &first,
        &second.snapshot_digest().unwrap(),
        vec![reader("output.png", &png(64, 64, 1))],
        &fixture.root.root_id,
        association(),
    )
    .await;
    assert!(result.is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM media_results_v1")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[cfg(windows)]
#[tokio::test]
async fn copy_failure_discards_its_own_staging_file_before_returning() {
    let fixture = Fixture::new().await;
    let plan = fixture.plan("copy-failure", 1).await;
    let request = output_request(
        &plan,
        &plan.snapshot_digest().unwrap(),
        &fixture.root.root_id,
        &association(),
        0,
        MediaKind::Image,
    );
    let BeginMediaPublish::Staging(stage) =
        fixture.media.begin_publish(request.clone()).await.unwrap()
    else {
        panic!("Expected staging")
    };
    let mut input = reader("output.png", &png(64, 64, 1));
    let decoded = ExpectedOutput::Image {
        format: GenerationOutputFormat::Png,
        width: 64,
        height: 64,
    }
    .inspect(&mut input.file)
    .unwrap();
    input.sha256 = "0".repeat(64);
    assert!(
        copy_stage(&fixture.media, stage, CheckedMedia { input, decoded })
            .await
            .is_err()
    );
    let pending = fixture
        .root
        .path
        .join(crate::media::domain::segment(&request.session_id))
        .join(crate::media::domain::segment(&request.job_id))
        .join(format!("{}.pending", request.result_id));
    assert!(!pending.exists());
    let writing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM media_results_v1 WHERE state IN ('writing','prepared','ready')",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(writing, 0);
}

#[cfg(windows)]
#[tokio::test]
async fn recognizes_asr_output_as_session_text_without_publishing_a_media_row() {
    use std::collections::BTreeSet;
    let fixture = Fixture::new().await;
    let profile = AudioTaskProfile::Qwen3Asr06bCpu;
    let (sha256, size_bytes) = profile.recorded_model();
    let (spec_hash, spec_bytes) = profile.recorded_model_spec();
    let service = Qwen3AsrParameterService::new(AudioTaskModelBinding {
        installation_id: "test-asr".into(),
        version_id: "test-asr-v1".into(),
        manifest_digest: Sha256Digest::of_bytes(b"synthetic ASR manifest"),
        runtime: profile.runtime_build(),
        target_capabilities: BTreeSet::from([profile.capability()]),
        model: AudioTaskAssetBinding {
            artifact_id: "model".into(),
            sha256,
            size_bytes,
        },
        model_spec: AudioTaskAssetBinding {
            artifact_id: "model-spec".into(),
            sha256: spec_hash,
            size_bytes: spec_bytes,
        },
    })
    .unwrap();
    let task = service
        .compile(Qwen3AsrRequest {
            session_id: "session-a".into(),
            job_id: "asr".into(),
            audio: AudioTaskMediaReference {
                session_id: "session-a".into(),
                result_id: "019a1111-2222-8333-8444-555555555555".into(),
                sha256: Sha256Digest::of_bytes(b"synthetic audio"),
                mime_type: "audio/wav".into(),
                size_bytes: 32044,
                sample_rate: 16000,
                channels: 1,
                sample_frames: 16000,
            },
            context: None,
            language: None,
            max_tokens: None,
            chunking: None,
        })
        .unwrap();
    let config = fixture.config.save_qwen3_asr(&task).await.unwrap();
    let plan = NativeTaskPlan::from(task);
    let bytes = b"The blue door is open.\r\n";
    let ImportedTaskOutput::Transcript(text) = fixture
        .import(&plan, vec![reader("output.txt", bytes)])
        .await
        .unwrap()
    else {
        panic!("Expected transcript")
    };
    assert_eq!(
        text.generation_config_sha256,
        config.generation_config_sha256()
    );
    assert_eq!(text.session_id, "session-a");
    assert_eq!(text.job_id, "asr");
    assert_eq!(text.association, association());
    assert_eq!(text.text.as_bytes(), bytes);
    assert_eq!(
        fixture
            .config
            .read_transcript("session-a", "asr")
            .await
            .unwrap(),
        Some(text.clone())
    );
    assert!(fixture
        .config
        .read_transcript("session-b", "asr")
        .await
        .unwrap()
        .is_none());
    fixture.config.save_transcript(&text).await.unwrap();
    assert!(fixture
        .import(&plan, vec![reader("output.txt", b"replacement")])
        .await
        .is_err());
    assert_eq!(
        fixture
            .config
            .read_transcript("session-a", "asr")
            .await
            .unwrap(),
        Some(text)
    );
    sqlx::query("UPDATE media_task_transcripts_v1 SET text='edited' WHERE session_id='session-a' AND job_id='asr'")
        .execute(&fixture.pool).await.unwrap();
    assert!(fixture
        .config
        .read_transcript("session-a", "asr")
        .await
        .is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM media_results_v1")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
