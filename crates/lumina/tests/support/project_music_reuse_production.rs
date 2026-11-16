use super::*;
use crate::agents::platform_extensions::{PlatformExtensionContext, PLATFORM_EXTENSIONS};
use crate::agents::ToolCallContext;
use crate::media::NativeTaskJobs;
use rmcp::model::RawContent;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "Verifies real project reuse through normal native_tasks admission and replay"]
async fn verify_project_music_reuse_production() -> Result<()> {
    ensure!(
        std::env::var("LUMINA_PROJECT_MUSIC_REUSE").as_deref() == Ok("production"),
        "Explicit production acceptance mode required"
    );
    ensure!(
        std::env::var_os("LUMINA_TASK_RESOURCE_OWNER").is_none(),
        "Normal-entry acceptance must not use a measurement owner"
    );
    let validation = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation")
        .canonicalize()?;
    let run = PathBuf::from(
        std::env::var_os("LUMINA_TASK_RESOURCE_RUN_DIR").context("Missing acceptance directory")?,
    )
    .canonicalize()?;
    ensure!(
        run.parent() == Some(validation.as_path()),
        "Unexpected acceptance directory"
    );
    let case_bytes = read_bounded(&run.join("project-reuse-case.json"), 1024 * 1024)?;
    let case: Value = serde_json::from_slice(&case_bytes)?;
    let source = validate_case(&validation, &case, true)?;
    let case_sha = Sha256Digest::of_bytes(&case_bytes);
    let parent_id = case_string(&case, "parentAssetId")?;
    let parent_sha = case_string(&case, "parentAssetContentSha256")?;
    let parent_session = case_string(&case, "parentSessionId")?;
    let project_id = case_string(&case, "parentProjectId")?;
    let directory = run.join("output-root/publication");
    ensure!(
        files::digest(&mut File::open(directory.join("sessions/sessions.db"))?)?
            == case["backupDatabaseSha256"],
        "Acceptance database differs from frozen backup"
    );
    let manager = Arc::new(SessionManager::new(directory.clone()));
    let storage = Arc::clone(manager.storage());
    let child = manager
        .create_session(
            directory.clone(),
            "Normal project music reuse acceptance".into(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await?;
    manager
        .update(&child.id)
        .project_id(Some(project_id.into()))
        .apply()
        .await?;
    let child = manager.get_session(&child.id, false).await?;
    let pool = storage.pool().await?.clone();
    let parent_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)")
            .bind(parent_session)
            .fetch_one(&pool)
            .await?;
    ensure!(!parent_exists, "Original source session was not deleted");
    let assets = ProjectAssetStore::new(pool.clone());
    let parent = assets
        .read(&child.id, parent_id)
        .await?
        .context("Parent archive is missing")?;
    ensure!(
        parent.content_sha256 == parent_sha
            && parent.project_id == project_id
            && parent.source_session_id == parent_session
            && parent.job_id == case["parentJobId"],
        "Parent identity differs from case"
    );
    let archive: ArchivedTaskConfiguration =
        serde_json::from_value(parent.content["configuration"].clone())?;
    let parent_record = archive.validate(parent_session, &parent.job_id)?;
    let TaskConfigurationSnapshot::StableAudioMusic(parent_snapshot) = parent_record.snapshot()
    else {
        anyhow::bail!("Parent is not music")
    };
    let media = MediaResultStore::new(pool.clone());
    let parent_content = media.open_content(parent_session, parent_id).await?;
    ensure!(
        files::digest(&mut File::open(parent_content.path())?)? == case["parentOutputSha256"],
        "Parent WAV changed"
    );
    if let Some(source) = &source {
        let source_run = PathBuf::from(case_string(&case, "parentReportPath")?)
            .canonicalize()?
            .parent()
            .context("Missing source run")?
            .to_path_buf();
        ensure!(
            source["snapshotSha256"] == parent_record.generation_config_sha256()
                && source["result"] == parent.content["result"]
                && parent_content.path().canonicalize()?
                    == contained_path(source, "publishedPath", &source_run)?,
            "Parent archive differs from source proof"
        );
    }
    let app = Arc::new(InstallationApplication::new(
        &validation.join("m4-managed-audio-install-20261113-r1"),
        Arc::new(|| Ok(None)),
    ));
    storage.bind_native_installations(Arc::clone(&app))?;
    let installation = app.service().await?;
    installation.wait_ready().await?;
    let call_id = "project-music-production-reuse";
    let job = NativeTaskJobs::job_id(&child.id, call_id)?;
    let client = (PLATFORM_EXTENSIONS["native_tasks"].client_factory)(PlatformExtensionContext {
        extension_manager: None,
        session_manager: Arc::clone(&manager),
        session: Some(Arc::new(child.clone())),
        use_login_shell_path: false,
    });
    let context = ToolCallContext::new(child.id.clone(), None, Some(call_id.into()));
    let input = json!({"identity":parent_record.identity(),"parentAssetId":parent_id,"patch":case["patch"]});
    let count_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_results_v1 WHERE session_id=?")
            .bind(&child.id)
            .fetch_one(&pool)
            .await?;
    ensure!(
        count_before == 0,
        "New acceptance session already has results"
    );
    let started = Instant::now();
    let reply = client
        .call_tool(
            &context,
            "project_reuse",
            input.as_object().cloned(),
            CancellationToken::new(),
        )
        .await?;
    let elapsed_millis = started.elapsed().as_millis();
    ensure!(
        reply.is_error != Some(true),
        "Normal project tool failed: {reply:?}"
    );
    let [status, link] = reply.content.as_slice() else {
        anyhow::bail!("Expected completion and one audio link")
    };
    let RawContent::Text(status) = &status.raw else {
        anyhow::bail!("Missing tool completion")
    };
    let status: Value = serde_json::from_str(&status.text)?;
    let ids = status["resultIds"]
        .as_array()
        .context("Missing result IDs")?;
    ensure!(ids.len() == 1, "Expected one music result");
    let result_id = ids[0].as_str().context("Invalid result ID")?;
    let RawContent::ResourceLink(link) = &link.raw else {
        anyhow::bail!("Missing audio resource link")
    };
    ensure!(
        status["status"] == "completed"
            && status["jobId"] == job
            && link.uri == format!("lumina-media://result/{result_id}")
            && link.mime_type.as_deref() == Some("audio/wav"),
        "Normal tool returned an unexpected result"
    );
    let count_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_results_v1 WHERE session_id=?")
            .bind(&child.id)
            .fetch_one(&pool)
            .await?;
    ensure!(
        count_after == 1,
        "Normal call did not publish exactly one result"
    );
    let replay = client
        .call_tool(
            &context,
            "project_reuse",
            input.as_object().cloned(),
            CancellationToken::new(),
        )
        .await?;
    ensure!(
        serde_json::to_value(&reply)? == serde_json::to_value(&replay)?,
        "Same-call replay differs"
    );
    let count_replayed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_results_v1 WHERE session_id=?")
            .bind(&child.id)
            .fetch_one(&pool)
            .await?;
    let call_state: String = sqlx::query_scalar(
        "SELECT state FROM native_task_calls_v1 WHERE session_id=? AND job_id=?",
    )
    .bind(&child.id)
    .bind(&job)
    .fetch_one(&pool)
    .await?;
    ensure!(
        count_replayed == count_after && call_state == "completed",
        "Replay duplicated output or call is not completed"
    );
    let content = media.open_content(&child.id, result_id).await?;
    let result = content.result().clone();
    let published_path = content.path().to_path_buf();
    ensure!(
        result.bytes == 705644
            && result.mime == "audio/wav"
            && result.association
                == MediaAssociation::ToolCall {
                    tool_call_id: call_id.into()
                }
            && matches!(
                result.spec,
                MediaSpec::Audio {
                    sample_rate: 44100,
                    channels: 2,
                    sample_frames: 176400,
                    ..
                }
            )
            && files::digest(&mut File::open(&published_path)?)? == result.sha256,
        "Published WAV or association differs"
    );
    let configurations = TaskConfigurationStore::new(pool.clone());
    let child_record = configurations.verify_result(&result).await?;
    let TaskConfigurationSnapshot::StableAudioMusic(child_snapshot) = child_record.snapshot()
    else {
        anyhow::bail!("Child is not music")
    };
    let parent_parameters = serde_json::to_value(&parent_snapshot.parameters)?;
    let child_parameters = serde_json::to_value(&child_snapshot.parameters)?;
    let mut expected = parent_parameters.clone();
    for (key, value) in case["patch"].as_object().context("Missing patch")? {
        expected[key] = value.clone();
    }
    ensure!(
        child_parameters == expected && child_record.identity() == parent_record.identity(),
        "Normal tool changed parameters or identity outside patch"
    );
    let child_archive = assets
        .read(&child.id, result_id)
        .await?
        .context("Child archive missing")?;
    ensure!(
        child_archive.content["projectLineage"]["parentAssetId"] == parent_id
            && child_archive.content["projectLineage"]["parentAssetContentSha256"] == parent_sha
            && child_archive.content["projectLineage"]["patch"] == case["patch"]
            && assets.read(&child.id, parent_id).await? == Some(parent.clone()),
        "Parent or child lineage changed"
    );

    // Inspect the registered evidence after the ordinary tool has completed; this
    // assessment is never supplied to execution or used as a test-only receipt.
    let diagnostic = prepare_project_reused_task(
        &installation,
        &storage,
        &child.id,
        ProjectTaskReuseRequest {
            identity: parent_record.identity().clone(),
            parent_asset_id: parent_id.into(),
            new_job_id: job.clone(),
            patch: serde_json::from_value(case["patch"].clone())?,
            edit_scope: None,
            edit_replacement_asset_id: None,
        },
    )
    .await?;
    ensure!(
        diagnostic.plan().snapshot_digest()?.as_str() == child_record.generation_config_sha256(),
        "Post-execution diagnostic plan differs from completed task"
    );
    let publication = crate::media::task_publication::project_reuse_context(&diagnostic)?;
    let diagnostic_plan = diagnostic.plan().clone();
    let diagnostic_context = publication.clone();
    let output_root = storage.media_output_directory();
    let (evidence_sha, budget) = tokio::task::spawn_blocking(move || -> Result<_> {
        let assessment = assess_current_task_resources_with_publication(
            &diagnostic_plan,
            &output_root,
            &diagnostic_context,
        )?;
        let TaskResourceAssessment::Admitted(admission) = assessment else {
            anyhow::bail!("Completed task's resource profile is unavailable: {assessment:?}")
        };
        Ok((
            admission.evidence_sha256().clone(),
            serde_json::to_value(admission.budget())?,
        ))
    })
    .await??;
    write_report(
        &run.join("tool-response.json"),
        json!({"tool":"native_tasks__project_reuse","request":input,"response":reply,"replay":replay,
        "sameCallReplayed":true,"resultCounts":{"before":count_before,"after":count_after,"afterReplay":count_replayed},"nativeCallState":call_state}),
    )?;
    drop(content);
    pool.close().await;
    let reopened = SessionStorage::new(directory.clone());
    let reopened_pool = reopened.pool().await?.clone();
    let reopened_assets = ProjectAssetStore::new(reopened_pool.clone());
    let held = MediaResultStore::new(reopened_pool.clone())
        .open_content(&child.id, result_id)
        .await?;
    let verified = TaskConfigurationStore::new(reopened_pool.clone())
        .verify_result(&result)
        .await?;
    ensure!(
        held.result() == &result
            && verified == child_record
            && files::digest(&mut File::open(held.path())?)? == result.sha256
            && reopened_assets.read(&child.id, result_id).await? == Some(child_archive.clone())
            && reopened_assets.read(&child.id, parent_id).await? == Some(parent.clone())
            && files::digest(&mut File::open(parent_content.path())?)?
                == case["parentOutputSha256"],
        "Reopened task or project versions differ"
    );
    let source_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)")
            .bind(parent_session)
            .fetch_one(&reopened_pool)
            .await?;
    ensure!(!source_exists, "Normal reuse recreated the source session");
    drop((held, parent_content));
    reopened_pool.close().await;
    let export = export_retained_source(&directory, &result, &child_archive, &parent).await?;
    app.shutdown().await?;
    write_report(
        &run.join("production.json"),
        json!({"schemaVersion":1,"status":"PASS_NORMAL_TOOL_ENTRY",
        "scope":"One actual project_reuse call with normal registered resource admission, same-call replay, immutable parent and database reopen; not natural-language or desktop acceptance.",
        "caseSha256":case_sha,"publicationContext":publication,"resourceEvidenceSha256":evidence_sha,
        "admissionDiagnostic":{"performedAfterExecution":true,"usedForExecution":false,"budget":budget},
        "result":result,"publishedPath":published_path,"parentUnchanged":true,"lineageVerified":true,
        "reopenedVerified":true,"sourceSessionAbsent":true,"sameCallReplayed":true,"nativeCallCompleted":true,
        "parentParameters":parent_parameters,"childParameters":child_parameters,"patch":case["patch"],
        "elapsedMillis":elapsed_millis,"sources":sources(),"exportSource":export}),
    )
}
