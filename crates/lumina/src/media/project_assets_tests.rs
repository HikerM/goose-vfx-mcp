#![cfg(windows)]

use crate::config::LuminaMode;
use crate::media::*;
use crate::project::{ProjectAssetRecord, ProjectAssetStore, ProjectService};
use crate::session::{SessionManager, SessionType};
use lumina_providers::local_inference::lifecycle::*;
use serde_json::json;
use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Arc;

struct Fixture {
    _directory: tempfile::TempDir,
    manager: Arc<SessionManager>,
    media: MediaResultStore,
    configuration: TaskConfigurationStore,
    assets: ProjectAssetStore,
    root: MediaOutputRoot,
    source: String,
    other: String,
    project: String,
    project_root: std::path::PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/lifecycle-validation/project-assets-publication-synthetic");
        std::fs::create_dir_all(&base).unwrap();
        let directory = tempfile::tempdir_in(base.canonicalize().unwrap()).unwrap();
        let manager = Arc::new(SessionManager::new(directory.path().join("storage")));
        let projects = ProjectService::new(manager.storage().clone());
        let mut ids = Vec::new();
        let mut project_ids = Vec::new();
        for name in ["source", "other"] {
            let path = directory.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            let project = projects.open_project(&path, Some(name)).await.unwrap();
            let session = manager
                .create_session(path, name.into(), SessionType::User, LuminaMode::Auto)
                .await
                .unwrap();
            manager
                .update(&session.id)
                .project_id(Some(project.id.clone()))
                .apply()
                .await
                .unwrap();
            ids.push(session.id);
            project_ids.push(project.id);
        }
        let pool = manager.storage().pool().await.unwrap().clone();
        let media = MediaResultStore::new(pool.clone());
        media.initialize().await.unwrap();
        let configuration = TaskConfigurationStore::new(pool.clone());
        configuration.initialize().await.unwrap();
        let assets = ProjectAssetStore::new(pool);
        let root = media
            .register_output_root(&directory.path().join("outputs"))
            .await
            .unwrap();
        let project_root = directory.path().join("source");
        Self {
            _directory: directory,
            manager,
            media,
            configuration,
            assets,
            root,
            source: ids[0].clone(),
            other: ids[1].clone(),
            project: project_ids[0].clone(),
            project_root,
        }
    }

    async fn new_reader(&self) -> String {
        let session = self
            .manager
            .create_session(
                self.project_root.clone(),
                "New project session".into(),
                SessionType::User,
                LuminaMode::Auto,
            )
            .await
            .unwrap();
        self.manager
            .update(&session.id)
            .project_id(Some(self.project.clone()))
            .apply()
            .await
            .unwrap();
        session.id
    }

    fn mcp_request(&self, call: &str) -> PublishMediaRequest {
        PublishMediaRequest {
            result_id: uuid::Uuid::new_v4().to_string(),
            session_id: self.source.clone(),
            job_id: format!("synthetic-{call}"),
            root_id: self.root.root_id.clone(),
            kind: MediaKind::Audio,
            association: MediaAssociation::ToolCall {
                tool_call_id: call.into(),
            },
            source: MediaSource::Mcp {
                server_id: "synthetic-mcp".into(),
                tool_name: "audio_fixture".into(),
                tool_call_id: call.into(),
                input_refs: vec![],
            },
        }
    }

    async fn stage(&self, request: PublishMediaRequest, bytes: &[u8]) -> StagedMedia {
        let BeginMediaPublish::Staging(mut staged) =
            self.media.begin_publish(request).await.unwrap()
        else {
            panic!("Expected new publication")
        };
        staged.write_all(bytes).unwrap();
        staged
    }
}

fn synthetic_wav(rate: u32, channels: u16, frames: u32) -> Vec<u8> {
    let data_bytes = frames * channels as u32 * 2;
    let mut out = Vec::with_capacity(data_bytes as usize + 44);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    out.resize(data_bytes as usize + 44, 0);
    out
}

fn audio_binding(profile: AudioTaskProfile) -> AudioTaskModelBinding {
    let (sha256, size_bytes) = profile.recorded_model();
    let (spec_sha, spec_bytes) = profile.recorded_model_spec();
    AudioTaskModelBinding {
        installation_id: "synthetic-installation".into(),
        version_id: "synthetic-version".into(),
        manifest_digest: Sha256Digest::of_bytes(b"synthetic manifest; no model execution"),
        runtime: profile.runtime_build(),
        target_capabilities: BTreeSet::from([profile.capability()]),
        model: AudioTaskAssetBinding {
            artifact_id: "model".into(),
            sha256,
            size_bytes,
        },
        model_spec: AudioTaskAssetBinding {
            artifact_id: "spec".into(),
            sha256: spec_sha,
            size_bytes: spec_bytes,
        },
    }
}

fn verify_configuration(asset: &ProjectAssetRecord) -> TaskConfigurationRecord {
    let archive: super::super::task_configuration::ArchivedTaskConfiguration =
        serde_json::from_value(asset.content["configuration"].clone()).unwrap();
    archive
        .validate(&asset.source_session_id, &asset.job_id)
        .unwrap()
}

#[tokio::test]
async fn synthetic_mcp_wav_publication_archives_for_new_project_session_without_relaxing_media_scope(
) {
    let f = Fixture::new().await;
    f.assets.freeze_call(&f.source, "mcp-wav").await.unwrap();
    let request = f.mcp_request("mcp-wav");
    let staged = f
        .stage(request.clone(), &synthetic_wav(16000, 1, 16000))
        .await;
    let result = f.media.publish(staged).await.unwrap();
    let reader = f.new_reader().await;
    let list = f.assets.list(&reader, None, 20).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].asset_id, result.result_id);
    let asset = f
        .assets
        .read(&reader, &result.result_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(asset.project_id, f.project);
    assert_eq!(asset.source_session_id, f.source);
    assert_eq!(
        asset.content["result"],
        serde_json::to_value(&result).unwrap()
    );
    assert!(asset.content["configuration"].is_null());
    assert!(f
        .media
        .read(&reader, &result.result_id)
        .await
        .unwrap()
        .is_none());
    assert!(f
        .assets
        .read(&f.other, &result.result_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn synthetic_music_publication_uses_archived_valid_configuration_after_source_session_deletion(
) {
    let f = Fixture::new().await;
    let job = "synthetic-music";
    f.assets.freeze_job(&f.source, job, None).await.unwrap();
    let task = StableAudioMusicParameterService::new(audio_binding(AudioTaskProfile::StableAudioSmallMusicCpu)).unwrap()
        .compile_json(json!({"sessionId":f.source,"jobId":job,"prompt":"Synthetic piano request","seed":42,"steps":8})).unwrap();
    let config = f.configuration.save_music(&task).await.unwrap();
    let request = PublishMediaRequest {
        result_id: uuid::Uuid::new_v4().to_string(),
        session_id: f.source.clone(),
        job_id: job.into(),
        root_id: f.root.root_id.clone(),
        kind: MediaKind::Audio,
        association: MediaAssociation::Message {
            message_id: "synthetic-message".into(),
        },
        source: MediaSource::LocalModel {
            installation_id: config.identity().installation_id.clone(),
            version_id: config.identity().version_id.clone(),
            runtime: MediaRuntimeBuild {
                adapter_id: task.snapshot().metadata.binding.runtime.adapter_id.clone(),
                engine_revision: task
                    .snapshot()
                    .metadata
                    .binding
                    .runtime
                    .engine_revision
                    .clone(),
                binary_sha256: task
                    .snapshot()
                    .metadata
                    .binding
                    .runtime
                    .binary_sha256
                    .as_str()
                    .into(),
            },
            generation_config_sha256: config.generation_config_sha256().into(),
            input_refs: vec![],
        },
    };
    f.manager.delete_session(&f.source).await.unwrap();
    assert!(f
        .configuration
        .read(&f.source, job)
        .await
        .unwrap()
        .is_none());
    let staged = f.stage(request, &synthetic_wav(44100, 2, 176400)).await;
    let result = f.media.publish(staged).await.unwrap();
    let reader = f.new_reader().await;
    let asset = f
        .assets
        .read(&reader, &result.result_id)
        .await
        .unwrap()
        .unwrap();
    let archived = verify_configuration(&asset);
    assert_eq!(archived, config);
    assert!(matches!(
        archived.snapshot(),
        TaskConfigurationSnapshot::StableAudioMusic(_)
    ));
    assert_eq!(asset.content["result"]["sha256"], result.sha256);
}

#[tokio::test]
async fn synthetic_media_asset_failure_rolls_back_ready_and_recovers_once_in_the_frozen_project() {
    let f = Fixture::new().await;
    f.assets.freeze_call(&f.source, "rollback").await.unwrap();
    let request = f.mcp_request("rollback");
    let pool = f.manager.storage().pool().await.unwrap();
    sqlx::query("CREATE TRIGGER fail_project_asset BEFORE INSERT ON project_assets_v1 BEGIN SELECT RAISE(ABORT,'synthetic asset failure'); END").execute(pool).await.unwrap();
    let staged = f
        .stage(request.clone(), &synthetic_wav(16000, 1, 16000))
        .await;
    assert!(f.media.publish(staged).await.is_err());
    let state: String = sqlx::query_scalar("SELECT state FROM media_results_v1 WHERE result_id=?")
        .bind(&request.result_id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(state, "prepared");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_assets_v1")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let other_project = f
        .manager
        .get_session(&f.other, false)
        .await
        .unwrap()
        .project_id;
    f.manager
        .update(&f.source)
        .project_id(other_project)
        .apply()
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER fail_project_asset")
        .execute(pool)
        .await
        .unwrap();
    let recovered = f.media.recover().await.unwrap();
    assert!(recovered.iter().any(
        |r| r.result_id == request.result_id && r.availability == MediaAvailability::Available
    ));
    f.media.recover().await.unwrap();
    assert!(matches!(
        f.media.begin_publish(request.clone()).await.unwrap(),
        BeginMediaPublish::Existing(_)
    ));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_assets_v1 WHERE asset_id=?")
        .bind(&request.result_id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let reader = f.new_reader().await;
    assert_eq!(
        f.assets
            .read(&reader, &request.result_id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        f.project
    );
    assert!(f
        .assets
        .read(&f.other, &request.result_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn synthetic_asr_save_archives_full_transcript_and_valid_configuration_after_session_deletion(
) {
    let f = Fixture::new().await;
    let job = "synthetic-asr";
    f.assets.freeze_job(&f.source, job, None).await.unwrap();
    let task = Qwen3AsrParameterService::new(audio_binding(AudioTaskProfile::Qwen3Asr06bCpu))
        .unwrap()
        .compile(Qwen3AsrRequest {
            session_id: f.source.clone(),
            job_id: job.into(),
            audio: AudioTaskMediaReference {
                session_id: f.source.clone(),
                result_id: uuid::Uuid::new_v4().to_string(),
                sha256: Sha256Digest::of_bytes(b"synthetic ASR input metadata"),
                mime_type: "audio/wav".into(),
                size_bytes: 32044,
                sample_rate: 16000,
                channels: 1,
                sample_frames: 16000,
            },
            context: Some("Synthetic input".into()),
            language: Some(Qwen3AsrLanguage::Chinese),
            max_tokens: None,
            chunking: Some(Qwen3AsrChunking::WholeFile {}),
        })
        .unwrap();
    let config = f.configuration.save_qwen3_asr(&task).await.unwrap();
    let text = "这是合成转录夹具，并非模型推理。\r\n".repeat(400);
    let transcript = NativeTaskTranscript {
        session_id: f.source.clone(),
        job_id: job.into(),
        association: MediaAssociation::Message {
            message_id: "synthetic-asr-message".into(),
        },
        generation_config_sha256: config.generation_config_sha256().into(),
        bytes: text.len() as u64,
        sha256: Sha256Digest::of_bytes(text.as_bytes()).as_str().into(),
        text,
    };
    f.configuration.save_transcript(&transcript).await.unwrap();
    f.configuration.save_transcript(&transcript).await.unwrap();
    f.manager.delete_session(&f.source).await.unwrap();
    let reader = f.new_reader().await;
    let list = f.assets.list(&reader, None, 20).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].kind, "transcript");
    let asset = f
        .assets
        .read(&reader, &list[0].asset_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        asset.content["transcript"],
        serde_json::to_value(&transcript).unwrap()
    );
    assert_eq!(verify_configuration(&asset), config);
    assert!(f
        .assets
        .read(&f.other, &asset.asset_id)
        .await
        .unwrap()
        .is_none());
}
