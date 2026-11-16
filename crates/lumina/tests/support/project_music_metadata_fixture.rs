use super::*;
use crate::media::{
    BeginMediaPublish, MediaInputRef, MediaKind, MediaRuntimeBuild, MediaSource,
    PublishMediaRequest,
};
use std::io::Write;

#[tokio::test]
#[ignore = "Prepares real-WAV metadata through publication APIs without generating new audio"]
async fn prepare_project_music_metadata_fixture() -> Result<()> {
    ensure!(
        std::env::var("LUMINA_PROJECT_MUSIC_REUSE").as_deref() == Ok("metadata_fixture"),
        "Explicit metadata fixture mode required"
    );
    let validation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let run = PathBuf::from(
        std::env::var_os("LUMINA_TASK_RESOURCE_RUN_DIR").context("Missing fixture directory")?,
    )
    .canonicalize()?;
    ensure!(
        run.parent() == Some(validation.as_path()),
        "Unexpected fixture directory"
    );
    let case_bytes = read_bounded(&run.join("project-reuse-case.json"), 1024 * 1024)?;
    let case: Value = serde_json::from_slice(&case_bytes)?;
    ensure!(
        case["parentProofKind"] == "normal_tool" && case["patch"] == json!({"seed":1}),
        "Metadata fixture requires the original real source and subsequent seed patch"
    );
    validate_case(&validation, &case, true)?;
    let count = case["metadataPressure"]["referenceCount"]
        .as_u64()
        .context("Missing reference count")?;
    let message_bytes = case["metadataPressure"]["messageBytes"]
        .as_u64()
        .context("Missing message byte count")?;
    ensure!(
        (1..=128).contains(&count) && (8..=1024).contains(&message_bytes),
        "Metadata pressure exceeds publication API bounds"
    );
    let refs: Vec<MediaInputRef> = (0..count)
        .map(|index| {
            let mut message_id = format!("{index:03}-");
            while message_id.len() < message_bytes as usize {
                message_id.push(if message_id.len() % 2 == 0 { '"' } else { '\\' });
            }
            MediaInputRef::Message { message_id }
        })
        .collect();
    let directory = run.join("output-root/publication");
    let database = directory.join("sessions/sessions.db");
    ensure!(
        files::digest(&mut File::open(&database)?)? == case["backupDatabaseSha256"],
        "Fixture database differs from frozen backup"
    );
    let manager = SessionManager::new(directory.clone());
    let fixture = manager
        .create_session(
            directory.clone(),
            "Publication API metadata pressure fixture".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    manager
        .update(&fixture.id)
        .project_id(Some(case_string(&case, "parentProjectId")?.into()))
        .apply()
        .await?;
    let pool = manager.storage().pool().await?.clone();
    let assets = ProjectAssetStore::new(pool.clone());
    let parent = assets
        .read(&fixture.id, PARENT_ASSET)
        .await?
        .context("Missing original parent")?;
    ensure!(
        parent.content_sha256 == PARENT_CONTENT,
        "Original parent changed"
    );
    let parent_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)")
            .bind(case_string(&case, "parentSessionId")?)
            .fetch_one(&pool)
            .await?;
    ensure!(!parent_exists, "Original source session was not deleted");
    let parent_config: ArchivedTaskConfiguration =
        serde_json::from_value(parent.content["configuration"].clone())?;
    let parent_record = parent_config.validate(&parent.source_session_id, &parent.job_id)?;
    let TaskConfigurationSnapshot::StableAudioMusic(snapshot) = parent_record.snapshot() else {
        anyhow::bail!("Original parent is not music")
    };
    let job = format!("project-music-metadata-{}", uuid::Uuid::new_v4());
    let mut request = serde_json::to_value(&snapshot.parameters)?;
    request["sessionId"] = json!(fixture.id);
    request["jobId"] = json!(job);
    let task = StableAudioMusicParameterService::new(snapshot.metadata.binding.clone())?
        .compile_json(request)?;
    ensure!(
        task.snapshot().parameters == snapshot.parameters,
        "Fixture changed original parameters"
    );
    assets.freeze_job(&fixture.id, &job, None).await?;
    let configurations = TaskConfigurationStore::new(pool.clone());
    let saved = configurations.save_music(&task).await?;
    let media = MediaResultStore::new(pool.clone());
    let original = media
        .open_content(&parent.source_session_id, PARENT_ASSET)
        .await?;
    let wav = read_bounded(original.path(), 705644)?;
    ensure!(
        wav.len() == 705644 && Sha256Digest::of_bytes(&wav).as_str() == PARENT_WAV,
        "Original WAV differs"
    );
    let root = media
        .register_output_root(&manager.storage().media_output_directory())
        .await?;
    let runtime = &snapshot.metadata.binding.runtime;
    let publication_request = PublishMediaRequest {
        result_id: uuid::Uuid::new_v4().to_string(),
        session_id: fixture.id.clone(),
        job_id: job.clone(),
        root_id: root.root_id,
        kind: MediaKind::Audio,
        association: MediaAssociation::Message {
            message_id: "api-metadata-fixture-existing-audio".into(),
        },
        source: MediaSource::LocalModel {
            installation_id: saved.identity().installation_id.clone(),
            version_id: saved.identity().version_id.clone(),
            runtime: MediaRuntimeBuild {
                adapter_id: runtime.adapter_id.clone(),
                engine_revision: runtime.engine_revision.clone(),
                binary_sha256: runtime.binary_sha256.as_str().into(),
            },
            generation_config_sha256: saved.generation_config_sha256().into(),
            input_refs: refs,
        },
    };
    let BeginMediaPublish::Staging(mut staged) = media.begin_publish(publication_request).await?
    else {
        anyhow::bail!("Expected fresh fixture publication")
    };
    staged.write_all(&wav)?;
    let result = media.publish(staged).await?;
    ensure!(
        result.sha256 == PARENT_WAV && result.bytes == 705644,
        "Fixture changed source audio"
    );
    let archive = assets
        .read(&fixture.id, &result.result_id)
        .await?
        .context("Fixture archive missing")?;
    let asset_bytes: i64 = sqlx::query_scalar(
        "SELECT length(CAST(content_json AS BLOB)) FROM project_assets_v1 WHERE asset_id=?",
    )
    .bind(&result.result_id)
    .fetch_one(&pool)
    .await?;
    drop(original);
    pool.close().await;
    let export = export_retained_source(&directory, &result, &archive, &parent).await?;
    write_report(
        &run.join("fixture-result.json"),
        json!({
            "schemaVersion":1,"status":"API_METADATA_FIXTURE_READY","generatedNewAudio":false,
            "scope":"Existing real WAV republished through MediaResultStore with legal metadata pressure; these message references are API fixtures, not music producer output.",
            "caseSha256":Sha256Digest::of_bytes(&case_bytes),"originalSourceAssetId":PARENT_ASSET,
            "originalSourceContentSha256":PARENT_CONTENT,"originalSourceOutputSha256":PARENT_WAV,
            "metadataPressure":{"referenceCount":count,"messageBytes":message_bytes},
            "parentAssetBytes":asset_bytes,"exportSource":export,"sources":sources()
        }),
    )
}
