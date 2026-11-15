use super::*;
use crate::project::{CreateWorkItem, Project, ProjectService};
use crate::session::SessionType;
use std::path::Path;

async fn fixture(root: &Path) -> Result<(Agent, Session, ProjectService, Project)> {
    let manager = Arc::new(SessionManager::new(root.join("sessions")));
    let projects = ProjectService::new(Arc::clone(manager.storage()));
    let project_root = root.join("workspace");
    std::fs::create_dir_all(&project_root)?;
    let project = projects
        .open_project(&project_root, Some("Context project"))
        .await?;
    let session = manager
        .create_session(
            project_root,
            "Project context test".into(),
            SessionType::Hidden,
            LuminaMode::Auto,
        )
        .await?;
    manager
        .update(&session.id)
        .project_id(Some(project.id.clone()))
        .apply()
        .await?;
    let session = manager.get_session(&session.id, false).await?;
    let agent = Agent::with_config(AgentConfig::new(
        manager,
        Arc::new(PermissionManager::new(root.join("permissions"))),
        None,
        LuminaMode::Auto,
        true,
        LuminaPlatform::LuminaCli,
    ));
    Ok((agent, session, projects, project))
}

#[tokio::test]
async fn workbench_project_context_uses_persisted_identity_and_current_task() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (agent, session, projects, project) = fixture(temp.path()).await?;
    let work = projects
        .create_work_item(CreateWorkItem {
            project_id: project.id.clone(),
            title: "Render the opening".into(),
            objective: "Keep the original soundtrack".into(),
            acceptance_criteria: vec!["Exactly six seconds".into(), "Stereo output".into()],
        })
        .await?;
    let run = projects.start_run(&work.id, Some(&session.id)).await?;
    projects
        .create_work_item(CreateWorkItem {
            project_id: project.id.clone(),
            title: "UNRELATED_TASK_TITLE".into(),
            objective: "UNRELATED_TASK_OBJECTIVE".into(),
            acceptance_criteria: vec!["UNRELATED_TASK_CRITERION".into()],
        })
        .await?;
    let other_root = temp.path().join("other-project");
    std::fs::create_dir(&other_root)?;
    let other = projects
        .open_project(&other_root, Some("OTHER_PROJECT_NAME"))
        .await?;
    projects
        .create_work_item(CreateWorkItem {
            project_id: other.id.clone(),
            title: "OTHER_PROJECT_TASK".into(),
            objective: "OTHER_PROJECT_OBJECTIVE".into(),
            acceptance_criteria: vec![],
        })
        .await?;

    let context = agent.load_project_instructions(&session).await?.unwrap();
    assert!(project.id.starts_with("project_"));
    for expected in [
        project.name.as_str(),
        project.id.as_str(),
        work.id.as_str(),
        work.title.as_str(),
        work.objective.as_str(),
        "Exactly six seconds",
        "Stereo output",
        run.id.as_str(),
        run.phase.as_str(),
    ] {
        assert!(context.contains(expected), "Missing {expected}: {context}");
    }
    assert!(context.contains(&project.root_path.display().to_string()));
    assert!(context.contains(&project.canonical_root.display().to_string()));
    assert!(!context.contains("UNRELATED_TASK"));
    assert!(!context.contains("OTHER_PROJECT"));
    assert!(!context.contains(&other.id));
    agent
        .config
        .session_manager
        .storage()
        .pool()
        .await?
        .close()
        .await;
    drop(agent);
    drop(projects);

    let reopened_manager = Arc::new(SessionManager::new(temp.path().join("sessions")));
    let reopened_session = reopened_manager.get_session(&session.id, false).await?;
    assert_eq!(
        reopened_session.project_id.as_deref(),
        Some(project.id.as_str())
    );
    let reopened_agent = Agent::with_config(AgentConfig::new(
        reopened_manager,
        Arc::new(PermissionManager::new(temp.path().join("permissions"))),
        None,
        LuminaMode::Auto,
        true,
        LuminaPlatform::LuminaCli,
    ));
    let reopened_context = reopened_agent
        .load_project_instructions(&reopened_session)
        .await?
        .unwrap();
    assert_eq!(reopened_context, context);
    reopened_agent
        .config
        .session_manager
        .storage()
        .pool()
        .await?
        .close()
        .await;
    Ok(())
}

#[tokio::test]
async fn workbench_project_context_without_task_still_has_project_root() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (agent, session, _, project) = fixture(temp.path()).await?;
    let context = agent.load_project_instructions(&session).await?.unwrap();
    assert!(context.contains(&project.id));
    assert!(context.contains(&project.root_path.display().to_string()));
    assert!(!context.contains("Current work item"));
    assert!(!context.contains("Current run ID"));
    agent
        .config
        .session_manager
        .storage()
        .pool()
        .await?
        .close()
        .await;
    Ok(())
}

#[tokio::test]
async fn workbench_project_context_rejects_other_project_task_association() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (agent, mut session, projects, project) = fixture(temp.path()).await?;
    let work = projects
        .create_work_item(CreateWorkItem {
            project_id: project.id.clone(),
            title: "Original task".into(),
            objective: "Original objective".into(),
            acceptance_criteria: vec![],
        })
        .await?;
    projects
        .attach_session(&project.id, &work.id, &session.id)
        .await?;
    let other_root = temp.path().join("other-project");
    std::fs::create_dir(&other_root)?;
    let other = projects.open_project(&other_root, None).await?;
    session.project_id = Some(other.id);
    let error = agent.load_project_instructions(&session).await.unwrap_err();
    assert!(error.to_string().contains("different project"));
    agent
        .config
        .session_manager
        .storage()
        .pool()
        .await?
        .close()
        .await;
    Ok(())
}

#[tokio::test]
async fn workbench_project_context_rejects_run_from_previous_task() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (agent, session, projects, project) = fixture(temp.path()).await?;
    let previous = projects
        .create_work_item(CreateWorkItem {
            project_id: project.id.clone(),
            title: "Previous task".into(),
            objective: "Previous objective".into(),
            acceptance_criteria: vec![],
        })
        .await?;
    projects.start_run(&previous.id, Some(&session.id)).await?;
    let current = projects
        .create_work_item(CreateWorkItem {
            project_id: project.id.clone(),
            title: "Current task".into(),
            objective: "Current objective".into(),
            acceptance_criteria: vec![],
        })
        .await?;
    projects
        .attach_session(&project.id, &current.id, &session.id)
        .await?;
    let error = agent.load_project_instructions(&session).await.unwrap_err();
    assert!(error.to_string().contains("current work item"));
    agent
        .config
        .session_manager
        .storage()
        .pool()
        .await?
        .close()
        .await;
    Ok(())
}

#[tokio::test]
async fn workbench_project_context_distinguishes_absent_missing_and_database_failure() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let (agent, mut session, _, project) = fixture(temp.path()).await?;
    session.project_id = None;
    assert!(agent.load_project_instructions(&session).await?.is_none());
    session.project_id = Some(format!("project_{}", Uuid::new_v4()));
    let missing = agent.load_project_instructions(&session).await.unwrap_err();
    assert!(matches!(
        missing.downcast_ref::<sqlx::Error>(),
        Some(sqlx::Error::RowNotFound)
    ));
    session.project_id = Some(project.id);
    agent
        .config
        .session_manager
        .storage()
        .pool()
        .await?
        .close()
        .await;
    let unavailable = agent.load_project_instructions(&session).await.unwrap_err();
    assert!(matches!(
        unavailable.downcast_ref::<sqlx::Error>(),
        Some(sqlx::Error::PoolClosed)
    ));
    Ok(())
}

#[tokio::test]
async fn legacy_project_context_preserves_markdown_without_database_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (agent, mut session, _, _) = fixture(temp.path()).await?;
    agent
        .config
        .session_manager
        .storage()
        .pool()
        .await?
        .close()
        .await;
    let root = temp.path().display().to_string();
    let _env = env_lock::lock_env([("LUMINA_PATH_ROOT", Some(root.as_str()))]);
    let legacy_dir = crate::config::paths::Paths::data_dir().join("projects");
    std::fs::create_dir_all(&legacy_dir)?;
    std::fs::write(
        legacy_dir.join("legacy-context.md"),
        "---\nname: Legacy title\ndescription: Legacy description\n---\nLegacy project instructions.\n",
    )?;
    session.project_id = Some("legacy-context".into());
    let context = agent.load_project_instructions(&session).await?.unwrap();
    assert!(context.starts_with("# Project: legacy-context\n\n"));
    assert!(context.contains("Legacy description"));
    assert!(context.contains("Legacy project instructions."));
    session.project_id = Some("missing-legacy-context".into());
    assert!(agent.load_project_instructions(&session).await.is_err());
    Ok(())
}
