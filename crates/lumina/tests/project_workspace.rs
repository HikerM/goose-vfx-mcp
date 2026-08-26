use lumina::config::LuminaMode;
use lumina::project::{
    CreateWorkItem, ProjectService, RunCompletion, RunPhase, RunStatus, WorkItemStatus,
};
use lumina::session::{SessionManager, SessionType};

#[tokio::test]
async fn project_work_survives_restart_and_preserves_run_history() {
    let storage_dir = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        project_dir.path().join("Cargo.toml"),
        "[package]\nname = \"sample\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir(project_dir.path().join(".git")).unwrap();

    let session_manager = SessionManager::new(storage_dir.path().to_path_buf());
    let projects = ProjectService::new(session_manager.storage().clone());
    let opened = projects
        .open_project(project_dir.path(), Some("Sample workspace"))
        .await
        .unwrap();
    let reopened = projects
        .open_project(project_dir.path(), None)
        .await
        .unwrap();
    assert_eq!(opened.id, reopened.id);
    assert_eq!(reopened.name, "Sample workspace");
    assert!(reopened.profile.git_repository);
    assert!(reopened
        .profile
        .detected_stacks
        .contains(&"Rust".to_string()));

    let work_item = projects
        .create_work_item(CreateWorkItem {
            project_id: opened.id.clone(),
            title: "Implement project workflow".to_string(),
            objective: "Persist project-level development work".to_string(),
            acceptance_criteria: vec!["Run history remains available".to_string()],
        })
        .await
        .unwrap();
    let session = session_manager
        .create_session(
            project_dir.path().to_path_buf(),
            "Project task".to_string(),
            SessionType::User,
            LuminaMode::Auto,
        )
        .await
        .unwrap();
    let run = projects
        .start_run(&work_item.id, Some(&session.id))
        .await
        .unwrap();
    assert_eq!(
        projects
            .work_item_for_session(&session.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        work_item.id
    );
    assert!(projects
        .start_run(&work_item.id, Some(&session.id))
        .await
        .unwrap_err()
        .to_string()
        .contains("already has an active run"));
    assert!(projects
        .complete_work_item(&work_item.id)
        .await
        .unwrap_err()
        .to_string()
        .contains("only a reviewed work item can be completed"));
    projects
        .update_run_phase(&run.id, RunPhase::Validating)
        .await
        .unwrap();
    let checkpoint = projects
        .create_checkpoint(
            &run.id,
            RunPhase::Validating,
            serde_json::json!({"completed": ["implementation"]}),
            true,
        )
        .await
        .unwrap();
    assert_eq!(checkpoint.sequence, 2);
    let change_set = projects
        .ensure_change_set(&run.id, Some("base-revision"))
        .await
        .unwrap();
    assert_eq!(change_set.base_revision.as_deref(), Some("base-revision"));
    projects
        .finish_run(
            &run.id,
            RunCompletion {
                status: RunStatus::Succeeded,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .unwrap();
    let follow_up_run = projects
        .start_run(&work_item.id, Some(&session.id))
        .await
        .unwrap();
    projects
        .finish_run(
            &follow_up_run.id,
            RunCompletion {
                status: RunStatus::Succeeded,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .unwrap();
    let completed = projects.complete_work_item(&work_item.id).await.unwrap();
    assert_eq!(completed.status, WorkItemStatus::Completed);

    drop(projects);
    drop(session_manager);

    let restarted_manager = SessionManager::new(storage_dir.path().to_path_buf());
    let restarted_projects = ProjectService::new(restarted_manager.storage().clone());
    let snapshot = restarted_projects.snapshot(&opened.id).await.unwrap();
    assert_eq!(snapshot.work_items.len(), 1);
    assert_eq!(snapshot.runs.len(), 2);
    assert!(snapshot
        .runs
        .iter()
        .all(|run| run.status == RunStatus::Succeeded));
    assert_eq!(snapshot.run_steps.len(), 12);
    assert!(snapshot
        .run_steps
        .iter()
        .all(|step| step.status.as_str() == "completed"));
    assert_eq!(snapshot.checkpoints.len(), 2);
    assert!(snapshot
        .runs
        .iter()
        .all(|run| run.session_id.as_deref() == Some(session.id.as_str())));
}

#[tokio::test]
async fn startup_recovery_interrupts_only_non_terminal_runs() {
    let storage_dir = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let session_manager = SessionManager::new(storage_dir.path().to_path_buf());
    let projects = ProjectService::new(session_manager.storage().clone());
    let project = projects
        .open_project(project_dir.path(), Some("Recovery workspace"))
        .await
        .unwrap();

    let first = projects
        .create_work_item(CreateWorkItem {
            project_id: project.id.clone(),
            title: "First".to_string(),
            objective: "Complete normally".to_string(),
            acceptance_criteria: vec![],
        })
        .await
        .unwrap();
    let first_run = projects.start_run(&first.id, None).await.unwrap();
    projects
        .finish_run(
            &first_run.id,
            RunCompletion {
                status: RunStatus::Succeeded,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .unwrap();

    let second = projects
        .create_work_item(CreateWorkItem {
            project_id: project.id.clone(),
            title: "Second".to_string(),
            objective: "Remain active until restart".to_string(),
            acceptance_criteria: vec![],
        })
        .await
        .unwrap();
    let second_run = projects.start_run(&second.id, None).await.unwrap();

    assert_eq!(projects.interrupt_abandoned_runs().await.unwrap(), 1);
    let snapshot = projects.snapshot(&project.id).await.unwrap();
    assert_eq!(
        snapshot
            .runs
            .iter()
            .find(|run| run.id == first_run.id)
            .unwrap()
            .status,
        RunStatus::Succeeded
    );
    assert_eq!(
        snapshot
            .runs
            .iter()
            .find(|run| run.id == second_run.id)
            .unwrap()
            .status,
        RunStatus::Interrupted
    );
    assert_eq!(snapshot.interrupted_run_count, 1);
}
