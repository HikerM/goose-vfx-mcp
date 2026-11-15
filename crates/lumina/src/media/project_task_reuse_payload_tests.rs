use super::*;

async fn assert_absent(f: &Fixture, session: &str, job: &str) {
    assert!(f.configuration.read(session, job).await.unwrap().is_none());
    assert_eq!(f.grant_count(session, job).await, 0);
    assert!(f
        .assets
        .list(session, None, 100)
        .await
        .unwrap()
        .iter()
        .all(|asset| asset.job_id != job));
}

fn provenance_references() -> Vec<MediaInputRef> {
    (0..128)
        .map(|index| MediaInputRef::Message {
            message_id: format!("{index:03}{}", "m".repeat(1021)),
        })
        .collect()
}

async fn voice(f: &Fixture, call: &str) -> MediaResultRef {
    f.media
        .publish(
            f.stage(f.mcp_request(call), &synthetic_wav(16000, 1, 16000))
                .await,
        )
        .await
        .unwrap()
}

fn audio_reference(result: &MediaResultRef) -> AudioTaskMediaReference {
    let ProjectInputReference::Audio(reference) =
        input_from_result(result, SessionMediaParameterKind::Audio).unwrap()
    else {
        unreachable!()
    };
    reference
}

#[tokio::test]
async fn project_small_music_payload_tier_survives_save_reopen_and_v2_context() {
    let mut f = Fixture::new().await;
    let parent = f.music_parent().await;
    let reader = f.new_reader().await;
    let task = f
        .prepare(
            &reader,
            &parent.result_id,
            "small-music",
            json!({"prompt":"Synthetic small payload"}),
        )
        .await
        .unwrap();
    assert_eq!(task.payload_tier().unwrap(), ProjectReusePayloadTier::Small);
    assert!(matches!(
        crate::media::task_publication::project_reuse_context(&task).unwrap(),
        TaskPublicationContext::ProjectParameterReuseV2 {
            payload_tier: ProjectReusePayloadTier::Small,
            ..
        }
    ));
    let saved = f.configuration.save_project_task(&task).await.unwrap();
    assert_eq!(
        saved.project_scope().unwrap().payload_tier(),
        Some(ProjectReusePayloadTier::Small)
    );
    let published = f.publish(task.plan(), &saved).await;
    f.reopen().await;
    let reopened = f
        .configuration
        .read(&reader, "small-music")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened, saved);
    let mut connection = f
        .manager
        .storage()
        .pool()
        .await
        .unwrap()
        .acquire()
        .await
        .unwrap();
    let authorization =
        VerifiedProjectTaskAuthorization::read_from(&mut connection, &reader, "small-music")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        authorization.payload_tier(),
        Some(ProjectReusePayloadTier::Small)
    );
    drop(connection);
    let archived = f
        .assets
        .read(&reader, &published.result_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        archived.content["configuration"]["projectScope"]["payloadTier"],
        json!("small")
    );
    let next = f.new_reader().await;
    let task = f
        .prepare(
            &next,
            &published.result_id,
            "small-next",
            json!({"seed":918}),
        )
        .await
        .unwrap();
    assert_eq!(task.payload_tier().unwrap(), ProjectReusePayloadTier::Small);
}

#[tokio::test]
async fn project_valid_large_transcript_uses_standard_and_rejects_small_atomically() {
    let mut f = Fixture::new().await;
    let input = voice(&f, "transcript-input").await;
    f.assets
        .freeze_job(&f.source, "large-transcript-parent", None)
        .await
        .unwrap();
    let parent = Qwen3AsrParameterService::new(audio_binding(AudioTaskProfile::Qwen3Asr06bCpu))
        .unwrap()
        .compile(Qwen3AsrRequest {
            session_id: f.source.clone(),
            job_id: "large-transcript-parent".into(),
            audio: audio_reference(&input),
            context: None,
            language: None,
            max_tokens: None,
            chunking: Some(Qwen3AsrChunking::WholeFile {}),
        })
        .unwrap();
    let parent_record = f.configuration.save_qwen3_asr(&parent).await.unwrap();
    let text = "Synthetic transcript passage. ".repeat(12_000);
    assert!(text.len() > ProjectReusePayloadTier::Small.source_assets_limit());
    f.configuration
        .save_transcript(&NativeTaskTranscript {
            session_id: f.source.clone(),
            job_id: "large-transcript-parent".into(),
            association: MediaAssociation::Message {
                message_id: "synthetic-large-transcript".into(),
            },
            generation_config_sha256: parent_record.generation_config_sha256().into(),
            bytes: text.len() as u64,
            sha256: Sha256Digest::of_bytes(text.as_bytes()).as_str().into(),
            text,
        })
        .await
        .unwrap();
    let asset_id = f
        .assets
        .list(&f.source, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|asset| asset.job_id == "large-transcript-parent")
        .unwrap()
        .asset_id;
    let original_asset = f.assets.read(&f.source, &asset_id).await.unwrap().unwrap();
    assert!(
        serde_json::to_vec(&original_asset.content).unwrap().len()
            > ProjectReusePayloadTier::Small.source_assets_limit()
    );
    let reader = f.new_reader().await;
    let mut task = f
        .prepare(
            &reader,
            &asset_id,
            "large-transcript-child",
            json!({"context":"Project vocabulary"}),
        )
        .await
        .unwrap();
    assert_eq!(
        task.payload_tier().unwrap(),
        ProjectReusePayloadTier::Standard
    );
    task.authorization.compilation.payload_tier = Some(ProjectReusePayloadTier::Small);
    let error = f.configuration.save_project_task(&task).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("Transcript publication requires the standard payload tier"));
    assert_absent(&f, &reader, "large-transcript-child").await;
    task.authorization.compilation.payload_tier = Some(ProjectReusePayloadTier::Standard);
    let saved = f.configuration.save_project_task(&task).await.unwrap();
    f.transcript(task.plan(), &saved).await;
    f.reopen().await;
    assert_eq!(
        f.configuration
            .read(&reader, "large-transcript-child")
            .await
            .unwrap()
            .unwrap(),
        saved
    );
    assert_eq!(
        f.configuration
            .read(&f.source, "large-transcript-parent")
            .await
            .unwrap()
            .unwrap(),
        parent_record
    );
    let retained = f.assets.read(&reader, &asset_id).await.unwrap().unwrap();
    assert_eq!(retained.content_sha256, original_asset.content_sha256);
    assert_eq!(retained.content, original_asset.content);
}

#[tokio::test]
async fn project_parent_and_replacement_payloads_share_one_source_budget() {
    let f = Fixture::new().await;
    let original_voice = voice(&f, "original-voice").await;
    f.assets
        .freeze_job(&f.source, "tts-provenance-parent", None)
        .await
        .unwrap();
    let task = PocketTtsParameterService::new(audio_binding(AudioTaskProfile::PocketTtsEnglishCpu))
        .unwrap()
        .compile(PocketTtsRequest {
            session_id: f.source.clone(),
            job_id: "tts-provenance-parent".into(),
            text: "Synthetic speech".into(),
            voice_reference: audio_reference(&original_voice),
            seed: Some(42),
            temperature: None,
            max_frames: None,
            text_chunk_tokens: None,
            frames_after_eos: None,
        })
        .unwrap();
    let record = f.configuration.save_pocket_tts(&task).await.unwrap();
    let plan: NativeTaskPlan = task.into();
    let runtime = plan.runtime();
    let request = PublishMediaRequest {
        result_id: uuid::Uuid::new_v4().to_string(),
        session_id: f.source.clone(),
        job_id: "tts-provenance-parent".into(),
        root_id: f.root.root_id.clone(),
        kind: MediaKind::Audio,
        association: MediaAssociation::Message {
            message_id: "synthetic-provenance-output".into(),
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
            input_refs: provenance_references(),
        },
    };
    let parent = f
        .media
        .publish(f.stage(request, &synthetic_wav(24000, 1, 24000)).await)
        .await
        .unwrap();
    f.assets
        .freeze_call(&f.source, "replacement-provenance")
        .await
        .unwrap();
    let mut request = f.mcp_request("replacement-provenance");
    let MediaSource::Mcp { input_refs, .. } = &mut request.source else {
        unreachable!()
    };
    *input_refs = provenance_references();
    let replacement = f
        .media
        .publish(f.stage(request, &synthetic_wav(16000, 1, 16000)).await)
        .await
        .unwrap();
    let parent_asset = f
        .assets
        .read(&f.source, &parent.result_id)
        .await
        .unwrap()
        .unwrap();
    let replacement_asset = f
        .assets
        .read(&f.source, &replacement.result_id)
        .await
        .unwrap()
        .unwrap();
    let parent_bytes = serde_json::to_vec(&parent_asset.content).unwrap().len();
    let replacement_bytes = serde_json::to_vec(&replacement_asset.content)
        .unwrap()
        .len();
    let small_limit = ProjectReusePayloadTier::Small.source_assets_limit();
    assert!(parent_bytes < small_limit && replacement_bytes < small_limit);
    assert!(parent_bytes + replacement_bytes > small_limit);
    let reader = f.new_reader().await;
    let mut child = f
        .prepare(
            &reader,
            &parent.result_id,
            "combined-source-child",
            json!({"voiceReference":replacement.result_id}),
        )
        .await
        .unwrap();
    assert_eq!(
        child.payload_tier().unwrap(),
        ProjectReusePayloadTier::Standard
    );
    child.authorization.compilation.payload_tier = Some(ProjectReusePayloadTier::Small);
    let error = f.configuration.save_project_task(&child).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Project reuse payload exceeds its byte bound"),
        "{error:#}"
    );
    assert_absent(&f, &reader, "combined-source-child").await;
    child.authorization.compilation.payload_tier = Some(ProjectReusePayloadTier::Standard);
    f.configuration.save_project_task(&child).await.unwrap();
    assert_eq!(f.grant_count(&reader, "combined-source-child").await, 1);
}

#[tokio::test]
async fn project_legacy_authorization_without_payload_tier_preserves_wire_and_reopens() {
    let mut f = Fixture::new().await;
    let parent = f.music_parent().await;
    let reader = f.new_reader().await;
    let mut task = f
        .prepare(
            &reader,
            &parent.result_id,
            "legacy-tier-child",
            json!({"prompt":"Legacy-compatible music"}),
        )
        .await
        .unwrap();
    task.authorization.compilation.payload_tier = None;
    let original_encoding = task.authorization.encode().unwrap();
    assert!(!original_encoding.contains("payloadTier"));
    let decoded: VerifiedProjectTaskAuthorization =
        serde_json::from_str(&original_encoding).unwrap();
    assert_eq!(decoded.encode().unwrap(), original_encoding);
    let saved = f.configuration.save_project_task(&task).await.unwrap();
    assert_eq!(saved.project_scope().unwrap().payload_tier(), None);
    let result = f.publish(task.plan(), &saved).await;
    f.reopen().await;
    assert_eq!(
        f.configuration
            .read(&reader, "legacy-tier-child")
            .await
            .unwrap()
            .unwrap(),
        saved
    );
    let payload: String = sqlx::query_scalar(
        "SELECT payload FROM project_task_authorizations_v1 WHERE session_id=? AND job_id=?",
    )
    .bind(&reader)
    .bind("legacy-tier-child")
    .fetch_one(f.manager.storage().pool().await.unwrap())
    .await
    .unwrap();
    assert_eq!(payload, original_encoding);
    let archived = f
        .assets
        .read(&reader, &result.result_id)
        .await
        .unwrap()
        .unwrap();
    assert!(archived.content["configuration"]["projectScope"]
        .get("payloadTier")
        .is_none());
    assert!(archived.content["projectLineage"]["compilation"]
        .get("payloadTier")
        .is_none());
    let next = f.new_reader().await;
    let next_task = f
        .prepare(
            &next,
            &result.result_id,
            "upgraded-tier-child",
            json!({"seed":43}),
        )
        .await
        .unwrap();
    assert_eq!(
        next_task.payload_tier().unwrap(),
        ProjectReusePayloadTier::Small
    );
    f.configuration.save_project_task(&next_task).await.unwrap();
}
