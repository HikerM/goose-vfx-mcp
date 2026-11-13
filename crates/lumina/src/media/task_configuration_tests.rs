use super::*;
use crate::media::{MediaAssociation, MediaKind, MediaRuntimeBuild};
use lumina_providers::local_inference::lifecycle::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

struct Fixture {
    temp: tempfile::TempDir,
    store: TaskConfigurationStore,
}

impl Fixture {
    async fn connect(path: &Path) -> Pool<Sqlite> {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap()
    }

    async fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/lifecycle-validation/task-configuration-unit-tests");
        std::fs::create_dir_all(&base).unwrap();
        let temp = tempfile::Builder::new()
            .prefix("config-")
            .tempdir_in(base)
            .unwrap();
        let pool = Self::connect(&temp.path().join("sessions.db")).await;
        sqlx::query("CREATE TABLE sessions(id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions(id) VALUES('session-a'),('session-b')")
            .execute(&pool)
            .await
            .unwrap();
        let store = TaskConfigurationStore::new(pool);
        store.initialize().await.unwrap();
        store.initialize().await.unwrap();
        Self { temp, store }
    }

    async fn reopen(&mut self) {
        self.store.pool.close().await;
        self.store =
            TaskConfigurationStore::new(Self::connect(&self.temp.path().join("sessions.db")).await);
        self.store.initialize().await.unwrap();
    }
}

fn generation_service(profile: GenerationProfile) -> GenerationParameterService {
    GenerationParameterService::new(
        profile,
        GenerationModelBinding {
            installation_id: "test-installation".into(),
            version_id: "test-version".into(),
            manifest_digest: Sha256Digest::of_bytes(b"synthetic manifest; no file execution"),
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
    .unwrap()
}

fn generation(session: &str, job: &str, prompt: &str, video: bool) -> PreparedGenerationTask {
    let (profile, parameters) = if video {
        (
            GenerationProfile::H3CudaTextToVideoAudio,
            GenerationParameters::Video(H3VideoParameters {
                prompt: Some(prompt.into()),
                seed: Some(GenerationSeed::Value { value: 42 }),
                ..Default::default()
            }),
        )
    } else {
        (
            GenerationProfile::Sd15CpuTextToImage,
            GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some(prompt.into()),
                seed: Some(GenerationSeed::Value { value: 42 }),
                ..Default::default()
            }),
        )
    };
    generation_service(profile)
        .compile(GenerationTaskRequest {
            session_id: session.into(),
            job_id: job.into(),
            parameters,
        })
        .unwrap()
}

fn audio_binding(profile: AudioTaskProfile) -> AudioTaskModelBinding {
    let (sha256, size_bytes) = profile.recorded_model();
    let (spec_sha, spec_size) = profile.recorded_model_spec();
    AudioTaskModelBinding {
        installation_id: "test-audio-installation".into(),
        version_id: "test-v1".into(),
        manifest_digest: Sha256Digest::of_bytes(b"synthetic audio manifest"),
        runtime: profile.runtime_build(),
        target_capabilities: BTreeSet::from([profile.capability()]),
        model: AudioTaskAssetBinding {
            artifact_id: "main".into(),
            sha256,
            size_bytes,
        },
        model_spec: AudioTaskAssetBinding {
            artifact_id: "spec".into(),
            sha256: spec_sha,
            size_bytes: spec_size,
        },
    }
}

fn audio_input() -> AudioTaskMediaReference {
    AudioTaskMediaReference {
        session_id: "session-a".into(),
        result_id: "019a1111-2222-8333-8444-555555555555".into(),
        sha256: Sha256Digest::of_bytes(b"synthetic audio reference metadata"),
        mime_type: "audio/wav".into(),
        size_bytes: 32_044,
        sample_rate: 16_000,
        channels: 1,
        sample_frames: 16_000,
    }
}

fn pocket() -> PocketTtsPreparedTask {
    PocketTtsParameterService::new(audio_binding(AudioTaskProfile::PocketTtsEnglishCpu))
        .unwrap()
        .compile(PocketTtsRequest {
            session_id: "session-a".into(),
            job_id: "tts".into(),
            text: "A red apple.".into(),
            voice_reference: audio_input(),
            seed: Some(42),
            temperature: None,
            max_frames: None,
            text_chunk_tokens: None,
            frames_after_eos: None,
        })
        .unwrap()
}

fn asr() -> Qwen3AsrPreparedTask {
    Qwen3AsrParameterService::new(audio_binding(AudioTaskProfile::Qwen3Asr06bCpu))
        .unwrap()
        .compile(Qwen3AsrRequest {
            session_id: "session-a".into(),
            job_id: "asr".into(),
            audio: audio_input(),
            context: Some("木桌，苹果".into()),
            language: Some(Qwen3AsrLanguage::Chinese),
            max_tokens: None,
            chunking: Some(Qwen3AsrChunking::WholeFile {}),
        })
        .unwrap()
}

#[tokio::test]
async fn derived_speech_requires_the_actual_parent_and_survives_reopen() {
    let mut fixture = Fixture::new().await;
    let parent = pocket();
    let service =
        PocketTtsParameterService::new(parent.snapshot().metadata.binding.clone()).unwrap();
    let child = service
        .reuse_configuration(
            "session-a",
            parent.snapshot(),
            &parent.snapshot().digest().unwrap(),
            "tts-2",
            PocketTtsPatch::Temperature(0.4),
        )
        .unwrap();
    assert!(fixture.store.save_pocket_tts(&child).await.is_err());
    fixture.store.save_pocket_tts(&parent).await.unwrap();
    let saved = fixture.store.save_pocket_tts(&child).await.unwrap();
    fixture.reopen().await;
    let stored = fixture
        .store
        .read("session-a", "tts-2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored, saved);
    let TaskConfigurationSnapshot::PocketTts(snapshot) = stored.snapshot() else {
        panic!("Wrong adapter")
    };
    assert_eq!(snapshot.parameters.seed, parent.snapshot().parameters.seed);
    service
        .validate_parent(snapshot, parent.snapshot())
        .unwrap();
}

fn publication(record: &TaskConfigurationRecord) -> PublishMediaRequest {
    PublishMediaRequest {
        session_id: record.session_id().into(),
        job_id: record.job_id().into(),
        result_id: uuid::Uuid::new_v4().to_string(),
        root_id: "test-root".into(),
        kind: MediaKind::Image,
        association: MediaAssociation::Message {
            message_id: "message-1".into(),
        },
        source: MediaSource::LocalModel {
            installation_id: record.identity().installation_id.clone(),
            version_id: record.identity().version_id.clone(),
            runtime: MediaRuntimeBuild {
                adapter_id: record.runtime().adapter_id.clone(),
                engine_revision: record.runtime().engine_revision.clone(),
                binary_sha256: record.runtime().binary_sha256.as_str().into(),
            },
            generation_config_sha256: record.generation_config_sha256().into(),
            input_refs: vec![],
        },
    }
}

#[tokio::test]
async fn reopens_all_snapshot_families_without_changing_native_digests_or_schemas() {
    let mut fixture = Fixture::new().await;
    let image = generation("session-a", "image", "A red apple", false);
    let video = generation("session-a", "video", "An apple rolls", true);
    let pocket = pocket();
    let asr = asr();
    let records = [
        fixture.store.save_generation(&image).await.unwrap(),
        fixture.store.save_generation(&video).await.unwrap(),
        fixture.store.save_pocket_tts(&pocket).await.unwrap(),
        fixture.store.save_qwen3_asr(&asr).await.unwrap(),
    ];
    let expected = [
        image.snapshot().digest().unwrap(),
        video.snapshot().digest().unwrap(),
        pocket.snapshot().digest().unwrap(),
        asr.snapshot().digest().unwrap(),
    ];
    for (record, digest) in records.iter().zip(expected) {
        assert_eq!(record.generation_config_sha256(), digest.as_str());
    }
    fixture.reopen().await;
    for record in records {
        assert_eq!(
            fixture
                .store
                .read(record.session_id(), record.job_id())
                .await
                .unwrap()
                .unwrap(),
            record
        );
    }
    assert_eq!(
        fixture
            .store
            .list("session-a", None, 100)
            .await
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        fixture
            .store
            .list("session-a", Some("image"), 1)
            .await
            .unwrap()[0]
            .job_id(),
        "tts"
    );
    assert!(image.ensure_execution_admitted().is_err());
    assert!(pocket.ensure_execution_admitted().is_err());
}

#[tokio::test]
async fn identical_saves_are_idempotent_and_conflicts_never_overwrite() {
    let fixture = Fixture::new().await;
    let task = generation("session-a", "job", "An apple", false);
    let (first, second) = tokio::join!(
        fixture.store.save_generation(&task),
        fixture.store.save_generation(&task)
    );
    let first = first.unwrap();
    assert_eq!(first, second.unwrap());
    assert!(fixture
        .store
        .save_generation(&generation("session-a", "job", "A blue door", false))
        .await
        .is_err());
    assert!(fixture
        .store
        .save_generation(&generation("session-a", "job", "An apple", true))
        .await
        .is_err());
    assert_eq!(
        fixture
            .store
            .read("session-a", "job")
            .await
            .unwrap()
            .unwrap(),
        first
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM media_task_configurations_v1")
        .fetch_one(&fixture.store.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn reads_are_session_scoped_and_session_foreign_keys_are_enforced() {
    let fixture = Fixture::new().await;
    fixture
        .store
        .save_generation(&generation("session-a", "job", "An apple", false))
        .await
        .unwrap();
    assert!(fixture
        .store
        .read("session-b", "job")
        .await
        .unwrap()
        .is_none());
    assert!(fixture
        .store
        .list("session-b", None, 100)
        .await
        .unwrap()
        .is_empty());
    fixture
        .store
        .save_generation(&generation("session-b", "job", "A door", false))
        .await
        .unwrap();
    assert!(fixture
        .store
        .save_generation(&generation("unknown", "job", "An apple", false))
        .await
        .is_err());
    sqlx::query("DELETE FROM sessions WHERE id='session-a'")
        .execute(&fixture.store.pool)
        .await
        .unwrap();
    assert!(fixture
        .store
        .read("session-a", "job")
        .await
        .unwrap()
        .is_none());
    assert!(fixture
        .store
        .read("session-b", "job")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn rejects_digest_identity_schema_and_runtime_tampering_on_read() {
    for case in 0..5 {
        let fixture = Fixture::new().await;
        let task = generation("session-a", "job", "An apple", false);
        let record = fixture.store.save_generation(&task).await.unwrap();
        match case {
            0 => {
                sqlx::query("UPDATE media_task_configurations_v1 SET snapshot_sha256=?")
                    .bind("0".repeat(64))
                    .execute(&fixture.store.pool)
                    .await
                    .unwrap();
            }
            1 => {
                let mut identity = record.identity.clone();
                identity.version_id = "other".into();
                sqlx::query("UPDATE media_task_configurations_v1 SET identity_json=?")
                    .bind(serde_json::to_string(&identity).unwrap())
                    .execute(&fixture.store.pool)
                    .await
                    .unwrap();
            }
            2 => {
                sqlx::query("UPDATE media_task_configurations_v1 SET parameter_schema_id='future-unknown-v100'").execute(&fixture.store.pool).await.unwrap();
            }
            3 => {
                let mut snapshot = task.snapshot().clone();
                snapshot.binding.runtime.binary_sha256 =
                    Sha256Digest::of_bytes(b"different native executable");
                sqlx::query(
                    "UPDATE media_task_configurations_v1 SET snapshot_json=?,snapshot_sha256=?",
                )
                .bind(serde_json::to_string(&snapshot).unwrap())
                .bind(snapshot.digest().unwrap().as_str())
                .execute(&fixture.store.pool)
                .await
                .unwrap();
            }
            4 => {
                sqlx::query("UPDATE media_task_configurations_v1 SET job_id='other-job'")
                    .execute(&fixture.store.pool)
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            fixture
                .store
                .read("session-a", if case == 4 { "other-job" } else { "job" })
                .await
                .is_err(),
            "case {case}"
        );
    }
}

#[tokio::test]
async fn rejects_invalid_effective_fields_and_hidden_extra_runtime_fields() {
    for case in 0..3 {
        let fixture = Fixture::new().await;
        let task = generation("session-a", "job", "An apple", false);
        fixture.store.save_generation(&task).await.unwrap();
        let mut value = serde_json::to_value(task.snapshot()).unwrap();
        match case {
            0 => value["engine"]["backend"] = json!("cuda0"),
            1 => value["parameters"]["parameters"]["width"] = json!(513),
            2 => value["binding"]["runtime"]["internalToken"] = json!("not-allowed"),
            _ => unreachable!(),
        }
        let payload = serde_json::to_string(&value).unwrap();
        sqlx::query("UPDATE media_task_configurations_v1 SET snapshot_json=?,snapshot_sha256=?")
            .bind(&payload)
            .bind(Sha256Digest::of_bytes(payload.as_bytes()).as_str())
            .execute(&fixture.store.pool)
            .await
            .unwrap();
        assert!(fixture.store.read("session-a", "job").await.is_err());
    }
}

#[tokio::test]
async fn multiple_results_reference_one_configuration_and_provenance_must_match() {
    let fixture = Fixture::new().await;
    let record = fixture
        .store
        .save_generation(&generation("session-a", "job", "An apple", false))
        .await
        .unwrap();
    let first = publication(&record);
    let second = publication(&record);
    assert_ne!(first.result_id, second.result_id);
    assert_eq!(fixture.store.verify_publish(&first).await.unwrap(), record);
    assert_eq!(fixture.store.verify_publish(&second).await.unwrap(), record);
    for case in 0..5 {
        let mut request = first.clone();
        if let MediaSource::LocalModel {
            installation_id,
            runtime,
            generation_config_sha256,
            ..
        } = &mut request.source
        {
            match case {
                0 => *installation_id = "other-model".into(),
                1 => runtime.binary_sha256 = "0".repeat(64),
                2 => *generation_config_sha256 = "0".repeat(64),
                3 => request.session_id = "session-b".into(),
                4 => request.job_id = "other-job".into(),
                _ => unreachable!(),
            }
        }
        assert!(fixture.store.verify_publish(&request).await.is_err());
    }
    let media_tables: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name='media_results_v1'")
            .fetch_one(&fixture.store.pool)
            .await
            .unwrap();
    assert_eq!(media_tables, 0);
}

#[tokio::test]
async fn size_limits_apply_before_parsing_and_after_database_tampering() {
    let fixture = Fixture::new().await;
    let task = generation("session-a", "job", "An apple", false);
    fixture.store.save_generation(&task).await.unwrap();
    let oversized = " ".repeat(MAX_TASK_CONFIGURATION_BYTES + 1);
    assert!(parse_snapshot("sd15-image-parameters-v1", &oversized).is_err());
    assert!(bounded_json(&oversized).is_err());
    assert!(
        sqlx::query("UPDATE media_task_configurations_v1 SET snapshot_json=?")
            .bind(&oversized)
            .execute(&fixture.store.pool)
            .await
            .is_err()
    );
    sqlx::query("PRAGMA ignore_check_constraints=ON")
        .execute(&fixture.store.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE media_task_configurations_v1 SET snapshot_json=?")
        .bind(&oversized)
        .execute(&fixture.store.pool)
        .await
        .unwrap();
    assert!(fixture.store.read("session-a", "job").await.is_err());
}

#[tokio::test]
async fn preserves_regeneration_parent_origins_and_complete_change_values() {
    let fixture = Fixture::new().await;
    let service = generation_service(GenerationProfile::Sd15CpuTextToImage);
    let initial = generation("session-a", "first", "An apple", false);
    let parent_id = "019a1111-2222-8333-8444-555555555555";
    let parent = GenerationResultRecord {
        snapshot: initial.snapshot().clone(),
        snapshot_digest: initial.snapshot().digest().unwrap(),
        outputs: vec![GenerationOutputReference {
            session_id: "session-a".into(),
            result_id: parent_id.into(),
            sha256: Sha256Digest::of_bytes(b"synthetic output"),
        }],
    };
    let next = service
        .reuse_result(
            "session-a",
            &parent,
            parent_id,
            "next",
            GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("A blue door".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    let stored = fixture.store.save_generation(&next).await.unwrap();
    assert_eq!(
        stored.snapshot_json(),
        serde_json::to_string(next.snapshot()).unwrap()
    );
    assert_eq!(
        stored.generation_config_sha256(),
        next.snapshot().digest().unwrap().as_str()
    );
    assert!(next.snapshot().parent.is_some());
    assert!(!next.snapshot().changes.is_empty());
}
