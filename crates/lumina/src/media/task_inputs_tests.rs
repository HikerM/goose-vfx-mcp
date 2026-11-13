use super::*;
use crate::media::{BeginMediaPublish, MediaAssociation, MediaSource, PublishMediaRequest};
use lumina_providers::local_inference::lifecycle::*;
use sqlx::sqlite::SqlitePoolOptions;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

const SESSION: &str = "input-fixture-session";

struct Fixture {
    store: MediaResultStore,
    root: crate::media::MediaOutputRoot,
    _temp: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/lifecycle-validation/task-inputs-unit");
        std::fs::create_dir_all(&base).unwrap();
        let temp = tempfile::tempdir_in(base.canonicalize().unwrap()).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let store = MediaResultStore::new(pool);
        store.initialize().await.unwrap();
        let root = store
            .register_output_root(&temp.path().join("outputs"))
            .await
            .unwrap();
        Self {
            store,
            root,
            _temp: temp,
        }
    }

    async fn publish(&self, kind: MediaKind, bytes: &[u8]) -> MediaResultRef {
        let request = PublishMediaRequest {
            result_id: uuid::Uuid::new_v4().to_string(),
            session_id: SESSION.into(),
            job_id: "producer-job".into(),
            root_id: self.root.root_id.clone(),
            kind,
            association: MediaAssociation::Message {
                message_id: "input-message".into(),
            },
            source: MediaSource::Mcp {
                server_id: "fixture".into(),
                tool_name: "produce_input".into(),
                tool_call_id: "fixture-call".into(),
                input_refs: vec![],
            },
        };
        let BeginMediaPublish::Staging(mut staged) =
            self.store.begin_publish(request).await.unwrap()
        else {
            panic!()
        };
        staged.write_all(bytes).unwrap();
        self.store.publish(staged).await.unwrap()
    }
}

fn asr_plan(input: AudioTaskMediaReference) -> NativeTaskPlan {
    let profile = AudioTaskProfile::Qwen3Asr06bCpu;
    let asset = |id: &str, (sha256, size_bytes)| AudioTaskAssetBinding {
        artifact_id: id.into(),
        sha256,
        size_bytes,
    };
    let session_id = input.session_id.clone();
    Qwen3AsrParameterService::new(AudioTaskModelBinding {
        installation_id: "fixture-install".into(),
        version_id: "v1".into(),
        manifest_digest: Sha256Digest::of_bytes(b"fixture"),
        runtime: profile.runtime_build(),
        target_capabilities: std::collections::BTreeSet::from([profile.capability()]),
        model: asset("main", profile.recorded_model()),
        model_spec: asset("spec", profile.recorded_model_spec()),
    })
    .unwrap()
    .compile(Qwen3AsrRequest {
        session_id,
        job_id: "consumer-job".into(),
        audio: input,
        context: None,
        language: None,
        max_tokens: None,
        chunking: None,
    })
    .unwrap()
    .into()
}

fn video_plan(session: &str, first_frame: Option<GenerationMediaInput>) -> NativeTaskPlan {
    let profile = if first_frame.is_some() {
        GenerationProfile::H3CudaFirstFrameToVideoAudio
    } else {
        GenerationProfile::H3CudaTextToVideoAudio
    };
    let binding = GenerationModelBinding {
        installation_id: "fixture-video".into(),
        version_id: "v1".into(),
        manifest_digest: Sha256Digest::of_bytes(b"fixture video"),
        runtime: profile.runtime_build(),
        target_capabilities: profile.required_capabilities(),
        assets: profile
            .recorded_assets()
            .into_iter()
            .map(|(role, (sha256, size_bytes))| {
                (
                    role,
                    GenerationAssetBinding {
                        artifact_id: format!("{role:?}"),
                        sha256,
                        size_bytes,
                    },
                )
            })
            .collect(),
    };
    GenerationParameterService::new(profile, binding)
        .unwrap()
        .compile(GenerationTaskRequest {
            session_id: session.into(),
            job_id: "video-job".into(),
            parameters: GenerationParameters::Video(H3VideoParameters {
                prompt: Some("An apple".into()),
                first_frame,
                seed: Some(GenerationSeed::Value { value: 42 }),
                ..Default::default()
            }),
        })
        .unwrap()
        .into()
}

fn audio_reference(result: &MediaResultRef) -> AudioTaskMediaReference {
    AudioTaskMediaReference {
        session_id: result.session_id.clone(),
        result_id: result.result_id.clone(),
        sha256: Sha256Digest::parse(&result.sha256).unwrap(),
        mime_type: "audio/wav".into(),
        size_bytes: result.bytes,
        sample_rate: 16_000,
        channels: 1,
        sample_frames: 800,
    }
}

fn wave() -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend(1636_u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(16000_u32.to_le_bytes());
    bytes.extend(32000_u32.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(1600_u32.to_le_bytes());
    bytes.extend(
        (0..800).flat_map(|index| (if index % 2 == 0 { 1000_i16 } else { -1000 }).to_le_bytes()),
    );
    bytes
}

fn write_access(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).open(path)
}

#[tokio::test]
async fn native_inputs_hold_verified_store_handle_until_caller_drops_them() {
    let fixture = Fixture::new().await;
    let result = fixture.publish(MediaKind::Audio, &wave()).await;
    let plan = asr_plan(audio_reference(&result));
    let inputs = fixture
        .store
        .open_native_task_inputs(SESSION, &plan)
        .await
        .unwrap();
    assert_eq!(inputs.len(), 1);
    let path = fixture.root.path.join(result.relative_path);
    assert_eq!(write_access(&path).unwrap_err().raw_os_error(), Some(32));
    drop(inputs);
    write_access(&path).unwrap();
}

#[tokio::test]
async fn rejects_other_session_unknown_result_and_mismatched_audio_metadata() {
    let fixture = Fixture::new().await;
    let result = fixture.publish(MediaKind::Audio, &wave()).await;
    let input = audio_reference(&result);
    assert!(fixture
        .store
        .open_native_task_inputs("another-session", &asr_plan(input.clone()))
        .await
        .is_err());
    for changed in [
        AudioTaskMediaReference {
            session_id: "another-session".into(),
            ..input.clone()
        },
        AudioTaskMediaReference {
            result_id: uuid::Uuid::new_v4().to_string(),
            ..input.clone()
        },
        AudioTaskMediaReference {
            sha256: Sha256Digest::of_bytes(b"different bytes"),
            ..input.clone()
        },
        AudioTaskMediaReference {
            size_bytes: input.size_bytes + 1,
            ..input.clone()
        },
        AudioTaskMediaReference {
            channels: 2,
            ..input.clone()
        },
        AudioTaskMediaReference {
            sample_rate: 8_000,
            ..input.clone()
        },
        AudioTaskMediaReference {
            sample_frames: 799,
            ..input.clone()
        },
    ] {
        let session = changed.session_id.clone();
        assert!(fixture
            .store
            .open_native_task_inputs(&session, &asr_plan(changed))
            .await
            .is_err());
    }
    let path = fixture.root.path.join(result.relative_path);
    let mut altered = wave();
    altered[44] ^= 1;
    std::fs::write(path, altered).unwrap();
    assert!(fixture
        .store
        .open_native_task_inputs(SESSION, &asr_plan(input))
        .await
        .is_err());
}

#[tokio::test]
async fn validated_first_frame_checks_image_size_mime_and_media_kind() {
    let fixture = Fixture::new().await;
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(3, 2, image::Rgb([250, 20, 10])))
        .write_to(&mut bytes, image::ImageOutputFormat::Png)
        .unwrap();
    let result = fixture.publish(MediaKind::Image, bytes.get_ref()).await;
    let input = GenerationMediaInput {
        session_id: SESSION.into(),
        result_id: result.result_id.clone(),
        sha256: Sha256Digest::parse(&result.sha256).unwrap(),
        mime_type: result.mime.clone(),
        size_bytes: result.bytes,
        width: 3,
        height: 2,
    };
    assert_eq!(
        fixture
            .store
            .open_native_task_inputs(SESSION, &video_plan(SESSION, Some(input.clone())))
            .await
            .unwrap()
            .len(),
        1
    );
    for changed in [
        GenerationMediaInput {
            width: 2,
            ..input.clone()
        },
        GenerationMediaInput {
            height: 3,
            ..input.clone()
        },
        GenerationMediaInput {
            mime_type: "image/jpeg".into(),
            ..input
        },
    ] {
        assert!(fixture
            .store
            .open_native_task_inputs(SESSION, &video_plan(SESSION, Some(changed)))
            .await
            .is_err());
    }
    assert!(fixture
        .store
        .open_native_task_inputs(SESSION, &asr_plan(audio_reference(&result)))
        .await
        .is_err());
}

#[tokio::test]
async fn tasks_without_media_still_require_the_authenticated_session() {
    let fixture = Fixture::new().await;
    let plan = video_plan(SESSION, None);
    assert!(fixture
        .store
        .open_native_task_inputs(SESSION, &plan)
        .await
        .unwrap()
        .is_empty());
    assert!(fixture
        .store
        .open_native_task_inputs("another-session", &plan)
        .await
        .is_err());
}
