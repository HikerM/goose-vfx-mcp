use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

enum StopAction {
    Pause,
    Shutdown,
}

fn slot_locks(store: &InstallationStore, namespace: &str) -> Vec<std::fs::File> {
    let directory = store.root().join("locks").join(namespace);
    std::fs::create_dir_all(&directory).unwrap();
    (0..4)
        .map(|index| {
            let digest = Sha256Digest::of_bytes(index.to_string().as_bytes());
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(directory.join(format!("{}.lock", digest.as_str())))
                .unwrap();
            file.try_lock_exclusive().unwrap();
            file
        })
        .collect()
}

fn exercise_slot_wait(namespace: &str, expected_attempts: u64, action: StopAction) {
    let fixture = Fixture::new(true);
    let mut input = cache_input(&fixture);
    input.source = InstallationSource::ModelScope {};
    let id = fixture.request.job_id.clone();
    fixture
        .repository
        .adopt_installation_input(&id, &input, 32)
        .unwrap();
    let held_slots = slot_locks(&fixture.store, namespace);
    let repository = Arc::new(fixture.repository);
    let store = Arc::new(fixture.store);
    let control = Arc::new(InstallationWorkerControl::default());
    let waiting_checks = Arc::new(AtomicUsize::new(0));
    let observed = repository.clone();
    let observed_id = id.clone();
    let checks = waiting_checks.clone();

    service_runtime().block_on(async {
        let worker = tokio::spawn(resume_installation_with_control(
            repository.clone(),
            store.clone(),
            id.clone(),
            None,
            "slot-wait-worker".into(),
            move || {
                let job = observed.read_job(&observed_id).unwrap().unwrap();
                let budget = observed.file_retry_budgets(&observed_id).unwrap();
                if job.status == JobStatus::Running
                    && job.stage == LifecycleStage::Downloading
                    && budget[0].recorded_attempts == expected_attempts
                {
                    checks.fetch_add(1, Ordering::SeqCst);
                }
                500
            },
            control.clone(),
        ));
        let waiting = tokio::time::timeout(Duration::from_secs(5), async {
            while waiting_checks.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if waiting.is_err() {
            control.request_stop();
        } else {
            let job = repository.read_job(&id).unwrap().unwrap();
            assert_eq!(job.status, JobStatus::Running);
            match action {
                StopAction::Pause => {
                    repository
                        .control_installation(
                            &control_request(
                                &id,
                                job.version,
                                InstallationJobAction::Pause,
                                "pause-while-waiting-for-slot",
                            ),
                            &store,
                            500,
                        )
                        .unwrap();
                }
                StopAction::Shutdown => control.request_stop(),
            }
        }
        let stopped = tokio::time::timeout(Duration::from_secs(5), worker).await;
        control.request_stop();
        assert!(
            waiting.is_ok(),
            "Network worker did not wait for {namespace}"
        );
        assert!(stopped.unwrap().unwrap().is_err());
        assert_eq!(
            repository.read_job(&id).unwrap().unwrap().status,
            match action {
                StopAction::Pause => JobStatus::Paused,
                StopAction::Shutdown => JobStatus::Queued,
            }
        );
        let budget = repository.file_retry_budgets(&id).unwrap().remove(0);
        assert_eq!(budget.recorded_attempts, expected_attempts);
        assert!(budget.last_failure.is_none());
        assert_eq!(
            budget.state,
            if expected_attempts == 0 {
                FileRetryState::Ready
            } else {
                FileRetryState::Active
            }
        );
        assert_eq!(
            repository.file_transfers(&id).unwrap()[0]
                .checkpoint
                .completed_bytes,
            0
        );
        assert!(repository
            .latest_installation_failure(&id)
            .unwrap()
            .is_none());
        assert!(repository.active_installations().unwrap().is_empty());
    });

    drop(held_slots);
    let files = slot_locks(&store, "file-slots");
    let requests = slot_locks(&store, "http-slots");
    assert_eq!(files.len(), 4);
    assert_eq!(requests.len(), 4);
}

#[test]
fn file_and_http_slot_waits_preserve_user_pause_and_release_worker_permits() {
    exercise_slot_wait("file-slots", 0, StopAction::Pause);
    exercise_slot_wait("http-slots", 1, StopAction::Pause);
}

#[test]
fn file_and_http_slot_waits_requeue_on_shutdown_without_recording_transfer_failure() {
    exercise_slot_wait("file-slots", 0, StopAction::Shutdown);
    exercise_slot_wait("http-slots", 1, StopAction::Shutdown);
}
