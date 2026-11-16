use super::*;
use crate::config::LuminaMode;
use crate::media::*;
use crate::project::{ProjectAssetStore, ProjectService};
use crate::session::{SessionManager, SessionType};
use serde_json::json;
use std::collections::BTreeSet;
use std::io::Write;

#[path = "project_task_reuse_payload_tests.rs"]
mod payload_tests;

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
            .join("../../target/lifecycle-validation/project-task-reuse-synthetic");
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

impl Fixture {
    async fn reopen(&mut self) {
        self.manager.storage().pool().await.unwrap().close().await;
        self.manager = Arc::new(SessionManager::new(self._directory.path().join("storage")));
        let pool = self.manager.storage().pool().await.unwrap().clone();
        self.media = MediaResultStore::new(pool.clone());
        self.media.initialize().await.unwrap();
        self.configuration = TaskConfigurationStore::new(pool.clone());
        self.configuration.initialize().await.unwrap();
        self.assets = ProjectAssetStore::new(pool);
    }

    async fn prepare(
        &self,
        session: &str,
        asset_id: &str,
        job: &str,
        patch: Value,
    ) -> Result<PreparedProjectTask> {
        self.assets.freeze_job(session, job, None).await?;
        let pool = self.manager.storage().pool().await?;
        let mut tx = pool.begin().await?;
        let asset = ProjectAssetStore::read_for_frozen_job_in_tx(&mut tx, session, job, asset_id)
            .await?
            .context("Parent is not in the current project")?;
        tx.commit().await?;
        let (archive, output) = parent_configuration(&asset)?;
        let record = archive.validate(&asset.source_session_id, &asset.job_id)?;
        let contract = contract_for_record(&record)?;
        let request = ProjectTaskReuseRequest {
            identity: record.identity().clone(),
            parent_asset_id: asset_id.into(),
            new_job_id: job.into(),
            patch: patch.as_object().context("Expected patch object")?.clone(),
            edit_scope: None,
            edit_replacement_asset_id: None,
        };
        prepare_registered(
            self.manager.storage(),
            session,
            request,
            asset,
            ParentTask {
                archive,
                record,
                output,
            },
            contract,
        )
        .await
    }

    async fn publish(
        &self,
        plan: &NativeTaskPlan,
        record: &TaskConfigurationRecord,
    ) -> MediaResultRef {
        let (kind, bytes) = match plan {
            NativeTaskPlan::Generation(task) => {
                let EffectiveGenerationParameters::Image(parameters) = &task.snapshot().parameters
                else {
                    panic!("Fixture expects image generation")
                };
                let mut bytes = std::io::Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(parameters.width, parameters.height)
                    .write_to(&mut bytes, image::ImageOutputFormat::Png)
                    .unwrap();
                (MediaKind::Image, bytes.into_inner())
            }
            NativeTaskPlan::StableAudioMusic(_) => {
                (MediaKind::Audio, synthetic_wav(44100, 2, 176400))
            }
            NativeTaskPlan::PocketTts(_) => (MediaKind::Audio, synthetic_wav(24000, 1, 24000)),
            NativeTaskPlan::Qwen3Asr(_) => panic!("ASR publishes a transcript"),
        };
        let runtime = plan.runtime();
        let request = PublishMediaRequest {
            result_id: uuid::Uuid::new_v4().to_string(),
            session_id: record.session_id().into(),
            job_id: record.job_id().into(),
            root_id: self.root.root_id.clone(),
            kind,
            association: MediaAssociation::Message {
                message_id: "synthetic-output".into(),
            },
            source: MediaSource::LocalModel {
                installation_id: record.identity().installation_id.clone(),
                version_id: record.identity().version_id.clone(),
                runtime: MediaRuntimeBuild {
                    adapter_id: runtime.adapter_id.clone(),
                    engine_revision: runtime.engine_revision.clone(),
                    binary_sha256: runtime.binary_sha256.as_str().into(),
                },
                generation_config_sha256: record.generation_config_sha256().into(),
                input_refs: vec![],
            },
        };
        self.media
            .publish(self.stage(request, &bytes).await)
            .await
            .unwrap()
    }

    async fn music_parent(&self) -> MediaResultRef {
        self.assets
            .freeze_job(&self.source, "parent-music", None)
            .await
            .unwrap();
        let task = StableAudioMusicParameterService::new(audio_binding(AudioTaskProfile::StableAudioSmallMusicCpu)).unwrap()
            .compile_json(json!({"sessionId":self.source,"jobId":"parent-music","prompt":"Synthetic piano","negativePrompt":"noise","seed":42,"steps":8})).unwrap();
        let record = self.configuration.save_music(&task).await.unwrap();
        self.publish(&task.into(), &record).await
    }

    async fn transcript(&self, plan: &NativeTaskPlan, record: &TaskConfigurationRecord) -> String {
        assert!(matches!(plan, NativeTaskPlan::Qwen3Asr(_)));
        let text = "Synthetic transcript; no model execution".to_owned();
        self.configuration
            .save_transcript(&NativeTaskTranscript {
                session_id: record.session_id().into(),
                job_id: record.job_id().into(),
                association: MediaAssociation::Message {
                    message_id: "synthetic-transcript".into(),
                },
                generation_config_sha256: record.generation_config_sha256().into(),
                bytes: text.len() as u64,
                sha256: Sha256Digest::of_bytes(text.as_bytes()).as_str().into(),
                text,
            })
            .await
            .unwrap();
        self.assets
            .list(record.session_id(), None, 100)
            .await
            .unwrap()
            .into_iter()
            .find(|asset| asset.job_id == record.job_id())
            .unwrap()
            .asset_id
    }

    async fn grant_count(&self, session: &str, job: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_task_authorizations_v1 WHERE session_id=? AND job_id=?",
        )
        .bind(session)
        .bind(job)
        .fetch_one(self.manager.storage().pool().await.unwrap())
        .await
        .unwrap()
    }
}

#[path = "project_task_reuse_generation_tests.rs"]
mod generation;

#[tokio::test]
async fn project_authorization_upgrade_preserves_existing_configuration_and_asset() {
    let mut f = Fixture::new().await;
    let parent = f.music_parent().await;
    let original = f
        .configuration
        .read(&f.source, &parent.job_id)
        .await
        .unwrap()
        .unwrap();
    let asset = f
        .assets
        .read(&f.source, &parent.result_id)
        .await
        .unwrap()
        .unwrap();
    assert!(asset.content["configuration"].get("projectScope").is_none());
    sqlx::query("DROP TABLE project_task_authorizations_v1")
        .execute(f.manager.storage().pool().await.unwrap())
        .await
        .unwrap();
    f.reopen().await;
    assert_eq!(
        f.configuration
            .read(&f.source, &parent.job_id)
            .await
            .unwrap()
            .unwrap(),
        original
    );
    assert_eq!(
        f.assets
            .read(&f.source, &parent.result_id)
            .await
            .unwrap()
            .unwrap()
            .content_sha256,
        asset.content_sha256
    );
    let reader = f.new_reader().await;
    let task = f
        .prepare(
            &reader,
            &parent.result_id,
            "upgraded-child",
            json!({"prompt":"Synthetic upgraded task"}),
        )
        .await
        .unwrap();
    f.configuration.save_project_task(&task).await.unwrap();
    assert_eq!(f.grant_count(&reader, "upgraded-child").await, 1);
}

#[tokio::test]
async fn project_edit_scope_is_bounds_checked_and_requires_image_editor_inputs() {
    let f = Fixture::new().await;
    let parent = f.music_parent().await;
    let total = match parent.spec {
        MediaSpec::Audio {
            duration_micros, ..
        }
        | MediaSpec::Video {
            duration_micros, ..
        } => duration_micros,
        MediaSpec::Image { .. } => panic!("Expected an audio parent"),
    };
    let output = ParentOutput::Media {
        result: parent.clone(),
    };
    let valid = ProjectEditScope::TimeRange {
        start_micros: 0,
        duration_micros: total / 2,
    };
    validate_edit_scope(&output, &valid).unwrap();

    let out_of_bounds = ProjectEditScope::TimeRange {
        start_micros: total,
        duration_micros: 1,
    };
    let error = validate_edit_scope(&output, &out_of_bounds).unwrap_err();
    assert!(error
        .to_string()
        .contains("exceeds the archived media duration"));

    let wrong_kind = ProjectEditScope::ImageRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let error = validate_edit_scope(&output, &wrong_kind).unwrap_err();
    assert!(error
        .to_string()
        .contains("requires an archived image result"));
}

#[tokio::test]
async fn music_project_reuse_is_atomic_survives_deletion_and_reopen_and_keeps_bounded_lineage() {
    let mut f = Fixture::new().await;
    let source = f.source.clone();
    let parent = f.music_parent().await;
    let second = f.new_reader().await;
    f.manager.delete_session(&source).await.unwrap();
    let child = f
        .prepare(
            &second,
            &parent.result_id,
            "child-music",
            json!({"negativePrompt":null}),
        )
        .await
        .unwrap();
    assert_eq!(f.grant_count(&second, "child-music").await, 0);
    assert!(f
        .configuration
        .read(&second, "child-music")
        .await
        .unwrap()
        .is_none());
    let NativeTaskPlan::StableAudioMusic(task) = child.plan() else {
        panic!("Expected music")
    };
    assert!(task.snapshot().parameters.negative_prompt.is_none());
    let pool = f.manager.storage().pool().await.unwrap();
    sqlx::query("CREATE TRIGGER reject_project_child BEFORE INSERT ON media_task_configurations_v1 BEGIN SELECT RAISE(ABORT,'synthetic transaction failure'); END").execute(pool).await.unwrap();
    assert!(f.configuration.save_project_task(&child).await.is_err());
    assert_eq!(f.grant_count(&second, "child-music").await, 0);
    sqlx::query("DROP TRIGGER reject_project_child")
        .execute(pool)
        .await
        .unwrap();
    let saved = f.configuration.save_project_task(&child).await.unwrap();
    assert_eq!(
        f.configuration.save_project_task(&child).await.unwrap(),
        saved
    );
    f.reopen().await;
    assert_eq!(
        f.configuration
            .read(&second, "child-music")
            .await
            .unwrap()
            .unwrap(),
        saved
    );
    let result = f.publish(child.plan(), &saved).await;
    let asset = f
        .assets
        .read(&second, &result.result_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        asset.content["projectLineage"]["parentAssetId"],
        parent.result_id
    );
    assert_eq!(
        asset.content["projectLineage"]["patch"],
        json!({"negativePrompt":null})
    );
    let third = f.new_reader().await;
    f.manager.delete_session(&second).await.unwrap();
    let grandchild = f
        .prepare(
            &third,
            &result.result_id,
            "grandchild-music",
            json!({"prompt":"Synthetic flute"}),
        )
        .await
        .unwrap();
    let grandchild_record = f
        .configuration
        .save_project_task(&grandchild)
        .await
        .unwrap();
    let grandchild_result = f.publish(grandchild.plan(), &grandchild_record).await;
    let grandchild_asset = f
        .assets
        .read(&third, &grandchild_result.result_id)
        .await
        .unwrap()
        .unwrap();
    let lineage = &grandchild_asset.content["projectLineage"];
    assert_eq!(lineage["parentAssetId"], result.result_id);
    assert!(lineage["parentConfiguration"].get("projectScope").is_some());
    assert!(lineage["parentConfiguration"]
        .get("projectLineage")
        .is_none());
    assert!(grandchild.authorization().encode().unwrap().len() < 65536);
    assert_eq!(
        f.media
            .read(&source, &parent.result_id)
            .await
            .unwrap()
            .unwrap()
            .result,
        parent
    );
    assert!(f
        .media
        .read(&third, &parent.result_id)
        .await
        .unwrap()
        .is_none());
    assert!(f
        .assets
        .read(&f.other, &grandchild_result.result_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn project_reuse_rejects_project_switch_tampered_lineage_and_invalid_patches_without_persisting(
) {
    let f = Fixture::new().await;
    let parent = f.music_parent().await;
    let child_session = f.new_reader().await;
    for (job, patch) in [
        ("null-steps", json!({"steps":null})),
        ("multi-field", json!({"steps":10,"seed":7})),
        ("unknown-field", json!({"unknown":1})),
    ] {
        assert!(f
            .prepare(&child_session, &parent.result_id, job, patch)
            .await
            .is_err());
        assert_eq!(f.grant_count(&child_session, job).await, 0);
    }
    assert!(f
        .prepare(
            &f.other,
            &parent.result_id,
            "cross-project",
            json!({"steps":10})
        )
        .await
        .is_err());
    let prepared = f
        .prepare(
            &child_session,
            &parent.result_id,
            "switch",
            json!({"steps":10}),
        )
        .await
        .unwrap();
    let other_project = f
        .manager
        .get_session(&f.other, false)
        .await
        .unwrap()
        .project_id;
    f.manager
        .update(&child_session)
        .project_id(other_project)
        .apply()
        .await
        .unwrap();
    assert!(f.configuration.save_project_task(&prepared).await.is_err());
    assert!(authorize_project_inputs(
        f.manager.storage(),
        &child_session,
        prepared.plan(),
        prepared.authorization()
    )
    .await
    .is_err());
    assert_eq!(f.grant_count(&child_session, "switch").await, 0);
    f.manager
        .update(&child_session)
        .project_id(Some(f.project.clone()))
        .apply()
        .await
        .unwrap();
    let record = f.configuration.save_project_task(&prepared).await.unwrap();
    let mut forged = prepared.authorization().clone();
    forged.patch.insert("steps".into(), json!(12));
    assert!(forged.validate_child(&record).is_err());
    let encoded = forged.encode().unwrap();
    sqlx::query("UPDATE project_task_authorizations_v1 SET payload=?,sha256=? WHERE session_id=? AND job_id='switch'")
        .bind(&encoded).bind(Sha256Digest::of_bytes(encoded.as_bytes()).as_str()).bind(&child_session)
        .execute(f.manager.storage().pool().await.unwrap()).await.unwrap();
    assert!(f
        .configuration
        .read(&child_session, "switch")
        .await
        .is_err());
    let mut tx = f
        .manager
        .storage()
        .pool()
        .await
        .unwrap()
        .begin()
        .await
        .unwrap();
    let archive = ArchivedTaskConfiguration::read(&mut tx, &child_session, "switch")
        .await
        .unwrap();
    let archived = archive.validate(&child_session, "switch").unwrap();
    assert!(project_lineage(&mut tx, &archived).await.is_err());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn asr_project_reuse_authorizes_exact_inherited_input_and_three_stage_replacement_chain() {
    let mut f = Fixture::new().await;
    // This input predates project scopes, so no independent project asset can grant access.
    let input = f
        .media
        .publish(
            f.stage(
                f.mcp_request("legacy-audio"),
                &synthetic_wav(16000, 1, 16000),
            )
            .await,
        )
        .await
        .unwrap();
    assert!(f
        .assets
        .read(&f.source, &input.result_id)
        .await
        .unwrap()
        .is_none());
    let ProjectInputReference::Audio(reference) =
        input_from_result(&input, SessionMediaParameterKind::Audio).unwrap()
    else {
        unreachable!()
    };
    let input_path = {
        let held = f
            .media
            .open_content(&input.session_id, &input.result_id)
            .await
            .unwrap();
        held.path().to_path_buf()
    };
    f.assets
        .freeze_job(&f.source, "parent-asr", None)
        .await
        .unwrap();
    let parent = Qwen3AsrParameterService::new(audio_binding(AudioTaskProfile::Qwen3Asr06bCpu))
        .unwrap()
        .compile(Qwen3AsrRequest {
            session_id: f.source.clone(),
            job_id: "parent-asr".into(),
            audio: reference.clone(),
            context: Some("Synthetic first".into()),
            language: Some(Qwen3AsrLanguage::Chinese),
            max_tokens: None,
            chunking: Some(Qwen3AsrChunking::WholeFile {}),
        })
        .unwrap();
    let parent_record = f.configuration.save_qwen3_asr(&parent).await.unwrap();
    let parent_asset = f.transcript(&parent.into(), &parent_record).await;
    f.assets
        .freeze_call(&f.source, "replacement-audio")
        .await
        .unwrap();
    let replacement = f
        .media
        .publish(
            f.stage(
                f.mcp_request("replacement-audio"),
                &synthetic_wav(16000, 1, 32000),
            )
            .await,
        )
        .await
        .unwrap();
    let second = f.new_reader().await;
    f.manager.delete_session(&f.source).await.unwrap();
    let inherited = f
        .prepare(
            &second,
            &parent_asset,
            "inherited",
            json!({"context":"Synthetic inherited input"}),
        )
        .await
        .unwrap();
    assert_eq!(
        inherited.plan().input_references(),
        vec![NativeTaskInputReference::Audio(reference.clone())]
    );
    let inherited_record = f.configuration.save_project_task(&inherited).await.unwrap();
    let inherited_asset = f.transcript(inherited.plan(), &inherited_record).await;
    assert!(f
        .media
        .read(&second, &input.result_id)
        .await
        .unwrap()
        .is_none());
    let third = f.new_reader().await;
    assert!(f
        .prepare(
            &third,
            &inherited_asset,
            "inline-input",
            json!({"audio":reference})
        )
        .await
        .is_err());
    let changed = f
        .prepare(
            &third,
            &inherited_asset,
            "replacement",
            json!({"audio":replacement.result_id}),
        )
        .await
        .unwrap();
    let changed_record = f.configuration.save_project_task(&changed).await.unwrap();
    let changed_asset = f.transcript(changed.plan(), &changed_record).await;
    assert_eq!(changed.plan().input_references()[0].session_id(), f.source);
    assert_eq!(
        changed.plan().input_references()[0].result_id(),
        replacement.result_id
    );
    // The old before-reference remains canonical evidence, but is no longer an execution input.
    assert!(input_path.starts_with(f._directory.path()));
    std::fs::remove_file(&input_path).unwrap();
    let fourth = f.new_reader().await;
    f.manager.delete_session(&second).await.unwrap();
    f.manager.delete_session(&third).await.unwrap();
    let final_task = f
        .prepare(
            &fourth,
            &changed_asset,
            "last-asr",
            json!({"context":"Synthetic final context"}),
        )
        .await
        .unwrap();
    assert_eq!(
        final_task.plan().input_references(),
        changed.plan().input_references()
    );
    let final_record = f
        .configuration
        .save_project_task(&final_task)
        .await
        .unwrap();
    f.reopen().await;
    assert_eq!(
        f.configuration
            .read(&fourth, "last-asr")
            .await
            .unwrap()
            .unwrap(),
        final_record
    );
    assert_eq!(final_task.authorization().input_references().len(), 1);
    assert_eq!(
        final_task.authorization().compilation.allowed_inputs.len(),
        2
    );
    let old = f
        .media
        .read(&f.source, &input.result_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.result, input);
    let original_replacement = f
        .media
        .read(&f.source, &replacement.result_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original_replacement.result, replacement);
}

#[tokio::test]
async fn tts_project_reuse_preserves_original_voice_scope_and_verifies_replacement_files() {
    let f = Fixture::new().await;
    let voice = f
        .media
        .publish(
            f.stage(f.mcp_request("voice"), &synthetic_wav(16000, 1, 16000))
                .await,
        )
        .await
        .unwrap();
    let ProjectInputReference::Audio(reference) =
        input_from_result(&voice, SessionMediaParameterKind::Audio).unwrap()
    else {
        unreachable!()
    };
    f.assets
        .freeze_job(&f.source, "parent-tts", None)
        .await
        .unwrap();
    let task = PocketTtsParameterService::new(audio_binding(AudioTaskProfile::PocketTtsEnglishCpu))
        .unwrap()
        .compile(PocketTtsRequest {
            session_id: f.source.clone(),
            job_id: "parent-tts".into(),
            text: "Synthetic first voice".into(),
            voice_reference: reference.clone(),
            seed: Some(918),
            temperature: None,
            max_frames: None,
            text_chunk_tokens: None,
            frames_after_eos: None,
        })
        .unwrap();
    let record = f.configuration.save_pocket_tts(&task).await.unwrap();
    let parent = f.publish(&task.into(), &record).await;
    f.assets
        .freeze_call(&f.source, "replacement-voice")
        .await
        .unwrap();
    let replacement = f
        .media
        .publish(
            f.stage(
                f.mcp_request("replacement-voice"),
                &synthetic_wav(16000, 1, 24000),
            )
            .await,
        )
        .await
        .unwrap();
    let second = f.new_reader().await;
    f.manager.delete_session(&f.source).await.unwrap();
    let inherited = f
        .prepare(
            &second,
            &parent.result_id,
            "child-tts",
            json!({"text":"Synthetic second voice"}),
        )
        .await
        .unwrap();
    assert_eq!(
        inherited.plan().input_references(),
        vec![NativeTaskInputReference::Audio(reference)]
    );
    let child_record = f.configuration.save_project_task(&inherited).await.unwrap();
    let child_asset = f.publish(inherited.plan(), &child_record).await;
    assert!(f
        .prepare(
            &second,
            &child_asset.result_id,
            "null-voice",
            json!({"voiceReference":null})
        )
        .await
        .is_err());
    let third = f.new_reader().await;
    let replaced = f
        .prepare(
            &third,
            &child_asset.result_id,
            "replacement-tts",
            json!({"voiceReference":replacement.result_id}),
        )
        .await
        .unwrap();
    assert_eq!(replaced.plan().input_references()[0].session_id(), f.source);
    assert_eq!(
        replaced.plan().input_references()[0].result_id(),
        replacement.result_id
    );
    let saved = f.configuration.save_project_task(&replaced).await.unwrap();
    assert_eq!(
        f.configuration
            .read(&third, "replacement-tts")
            .await
            .unwrap()
            .unwrap(),
        saved
    );
    let path = {
        let held = f
            .media
            .open_content(&replacement.session_id, &replacement.result_id)
            .await
            .unwrap();
        held.path().to_path_buf()
    };
    assert!(path.starts_with(f._directory.path()));
    std::fs::write(path, synthetic_wav(16000, 1, 8000)).unwrap();
    assert!(authorize_project_inputs(
        f.manager.storage(),
        &third,
        replaced.plan(),
        replaced.authorization()
    )
    .await
    .is_err());
    assert!(f
        .prepare(
            &third,
            &child_asset.result_id,
            "changed-file",
            json!({"voiceReference":replacement.result_id})
        )
        .await
        .is_err());
    assert_eq!(f.grant_count(&third, "changed-file").await, 0);
}
