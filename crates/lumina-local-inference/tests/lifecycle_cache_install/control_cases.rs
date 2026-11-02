use super::*;

fn receipt_count(path: &Path) -> u64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM installation_control_receipts",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn control_replays_keep_the_original_result_after_later_actions_and_database_reopen() {
    let fixture = Fixture::new(true);
    let id = &fixture.request.job_id;
    let path = fixture.temp.path().join("models.db");
    let pause = control_request(id, 0, InstallationJobAction::Pause, "pause-once");
    let paused = fixture
        .repository
        .control_installation(&pause, &fixture.store, 32)
        .unwrap();
    assert!(!paused.replayed);
    let resume = control_request(
        id,
        paused.job.version,
        InstallationJobAction::Resume,
        "resume-once",
    );
    let resumed = fixture
        .repository
        .control_installation(&resume, &fixture.store, 33)
        .unwrap();
    assert_eq!(resumed.job.status, JobStatus::Queued);
    let replay = fixture
        .repository
        .control_installation(&pause, &fixture.store, 34)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.job, paused.job);
    assert_eq!(
        fixture.repository.read_job(id).unwrap().unwrap(),
        resumed.job
    );
    let mut changed = pause.clone();
    changed.action = InstallationJobAction::Cancel;
    assert!(fixture
        .repository
        .control_installation(&changed, &fixture.store, 34)
        .is_err());
    changed = pause.clone();
    changed.expected_version = resumed.job.version;
    assert!(fixture
        .repository
        .control_installation(&changed, &fixture.store, 34)
        .is_err());
    changed = pause.clone();
    changed.job_id = "different-job".into();
    assert!(fixture
        .repository
        .control_installation(&changed, &fixture.store, 34)
        .is_err());
    let other = InstallationStore::open(&fixture.temp.path().join("other-store")).unwrap();
    assert!(fixture
        .repository
        .control_installation(&pause, &other, 34)
        .is_err());
    assert_eq!(fixture.repository.events(id, 0, 100).unwrap().len(), 3);
    assert_eq!(receipt_count(&path), 2);
    drop(fixture.repository);
    let repository = LifecycleRepository::open(&path).unwrap();
    let replay = repository
        .control_installation(&resume, &fixture.store, 500)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.job, resumed.job);
    assert_eq!(repository.events(&resume.job_id, 0, 100).unwrap().len(), 3);
}

#[test]
fn failed_control_receipt_insertion_rolls_back_the_job_and_event_atomically() {
    let fixture = Fixture::new(true);
    let path = fixture.temp.path().join("models.db");
    let before = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    let events = fixture.repository.events(&before.id, 0, 100).unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TRIGGER reject_control_receipt BEFORE INSERT ON installation_control_receipts BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    let request = control_request(
        &before.id,
        before.version,
        InstallationJobAction::Pause,
        "atomic-pause",
    );
    assert!(fixture
        .repository
        .control_installation(&request, &fixture.store, 32)
        .is_err());
    assert_eq!(
        fixture.repository.read_job(&before.id).unwrap().unwrap(),
        before
    );
    assert_eq!(
        fixture.repository.events(&before.id, 0, 100).unwrap(),
        events
    );
    assert_eq!(receipt_count(&path), 0);
    raw.execute_batch("DROP TRIGGER reject_control_receipt;")
        .unwrap();
    let result = fixture
        .repository
        .control_installation(&request, &fixture.store, 32)
        .unwrap();
    assert!(!result.replayed);
    assert_eq!(result.job.status, JobStatus::Paused);
    assert_eq!(receipt_count(&path), 1);
}

#[test]
fn independent_connections_deduplicate_controls_and_enforce_compare_and_swap() {
    for same_key in [false, true] {
        let fixture = Fixture::new(true);
        let path = fixture.temp.path().join("models.db");
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|index| {
                let path = path.clone();
                let root = fixture.store.root().to_owned();
                let barrier = Arc::clone(&barrier);
                let id = fixture.request.job_id.clone();
                std::thread::spawn(move || {
                    let repository = LifecycleRepository::open(&path).unwrap();
                    let store = InstallationStore::open(&root).unwrap();
                    let key = if same_key {
                        "same-control".into()
                    } else {
                        format!("control-{index}")
                    };
                    let request = control_request(&id, 0, InstallationJobAction::Pause, &key);
                    barrier.wait();
                    repository.control_installation(&request, &store, 32)
                })
            })
            .collect();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(
            results
                .iter()
                .filter(|result| result.as_ref().is_ok_and(|outcome| !outcome.replayed))
                .count(),
            1
        );
        if same_key {
            assert!(results.iter().all(Result::is_ok));
            assert_eq!(
                results
                    .iter()
                    .filter(|result| result.as_ref().is_ok_and(|outcome| outcome.replayed))
                    .count(),
                1
            );
        } else {
            assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
            assert!(results
                .iter()
                .find_map(|result| result.as_ref().err())
                .unwrap()
                .to_string()
                .contains("version conflict"));
        }
        assert_eq!(receipt_count(&path), 1);
        assert_eq!(
            fixture
                .repository
                .events(&fixture.request.job_id, 0, 100)
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn control_receipts_fence_workers_and_record_terminal_cancel_without_duplicate_events() {
    let fixture = Fixture::new(true);
    let id = &fixture.request.job_id;
    let lease = fixture
        .repository
        .claim(id, 0, "old-worker", 32, 120)
        .unwrap();
    let running = fixture.repository.read_job(id).unwrap().unwrap();
    let pause = control_request(
        id,
        running.version,
        InstallationJobAction::Pause,
        "pause-running",
    );
    let paused = fixture
        .repository
        .control_installation(&pause, &fixture.store, 33)
        .unwrap();
    assert!(fixture
        .repository
        .renew_lease(&lease, paused.job.version, 34, 120)
        .is_err());
    let cancel = control_request(
        id,
        paused.job.version,
        InstallationJobAction::Cancel,
        "cancel-paused",
    );
    let cancelled = fixture
        .repository
        .control_installation(&cancel, &fixture.store, 34)
        .unwrap();
    assert_eq!(cancelled.job.status, JobStatus::Cancelled);
    let before_events = fixture.repository.events(id, 0, 100).unwrap();
    let repeated = control_request(
        id,
        cancelled.job.version,
        InstallationJobAction::Cancel,
        "cancel-terminal",
    );
    let result = fixture
        .repository
        .control_installation(&repeated, &fixture.store, 35)
        .unwrap();
    assert!(!result.replayed);
    assert_eq!(result.job, cancelled.job);
    assert!(
        fixture
            .repository
            .control_installation(&repeated, &fixture.store, 36)
            .unwrap()
            .replayed
    );
    assert_eq!(
        fixture.repository.events(id, 0, 100).unwrap(),
        before_events
    );
}

#[test]
fn control_receipt_corruption_and_forged_historical_results_are_rejected() {
    for recompute_digest in [false, true] {
        let fixture = Fixture::new(true);
        let request = control_request(
            &fixture.request.job_id,
            0,
            InstallationJobAction::Pause,
            "tamper-control",
        );
        let accepted = fixture
            .repository
            .control_installation(&request, &fixture.store, 32)
            .unwrap();
        let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
        let (payload, digest): (String, String) = raw
            .query_row(
                "SELECT payload,digest FROM installation_control_receipts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let changed = payload.replace("\"status\":\"paused\"", "\"status\":\"cancelled\"");
        assert_ne!(payload, changed);
        let digest = if recompute_digest {
            Sha256Digest::of_bytes(changed.as_bytes())
                .as_str()
                .to_owned()
        } else {
            digest
        };
        raw.execute(
            "UPDATE installation_control_receipts SET payload=?1,digest=?2",
            rusqlite::params![changed, digest],
        )
        .unwrap();
        let error = fixture
            .repository
            .control_installation(&request, &fixture.store, 33)
            .unwrap_err();
        assert!(error.to_string().contains(if recompute_digest {
            "committed job event"
        } else {
            "integrity"
        }));
        assert_eq!(
            fixture
                .repository
                .read_job(&request.job_id)
                .unwrap()
                .unwrap(),
            accepted.job
        );
    }
}

#[test]
fn invalid_control_keys_and_another_input_store_never_change_the_job() {
    let fixture = Fixture::new(true);
    let input = cache_input(&fixture);
    fixture
        .repository
        .adopt_installation_input(&fixture.request.job_id, &input, 32)
        .unwrap();
    let before = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    for key in [
        String::new(),
        " ".into(),
        "bad\nkey".into(),
        "x".repeat(257),
    ] {
        let request = control_request(
            &before.id,
            before.version,
            InstallationJobAction::Pause,
            &key,
        );
        assert!(fixture
            .repository
            .control_installation(&request, &fixture.store, 33)
            .is_err());
    }
    let other = InstallationStore::open(&fixture.temp.path().join("another-store")).unwrap();
    let request = control_request(
        &before.id,
        before.version,
        InstallationJobAction::Pause,
        "wrong-store",
    );
    assert!(fixture
        .repository
        .control_installation(&request, &other, 33)
        .is_err());
    assert_eq!(
        fixture.repository.read_job(&before.id).unwrap().unwrap(),
        before
    );
    assert_eq!(receipt_count(&fixture.temp.path().join("models.db")), 0);
}

#[test]
fn schema_seven_control_upgrade_preserves_inputs_and_rolls_back_on_table_collision() {
    for collision in [false, true] {
        let fixture = Fixture::new(true);
        let input = cache_input(&fixture);
        fixture
            .repository
            .adopt_installation_input(&fixture.request.job_id, &input, 32)
            .unwrap();
        let before = fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap();
        let intent = fixture
            .repository
            .read_installation_input(&before.id)
            .unwrap()
            .unwrap();
        let path = fixture.temp.path().join("models.db");
        drop(fixture.repository);
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch("DROP TABLE installation_failures; DROP TABLE installation_file_retries; DROP TABLE installation_file_retry_events; DROP TABLE installation_control_receipts; PRAGMA user_version=7;")
            .unwrap();
        if collision {
            raw.execute_batch("CREATE TABLE installation_control_receipts (unrecognized TEXT);")
                .unwrap();
            assert!(LifecycleRepository::open(&path).is_err());
            assert_eq!(
                raw.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                    .unwrap(),
                7
            );
        } else {
            let repository = LifecycleRepository::open(&path).unwrap();
            assert_eq!(
                raw.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                    .unwrap(),
                MODEL_DATABASE_SCHEMA_VERSION
            );
            assert_eq!(repository.read_job(&before.id).unwrap().unwrap(), before);
            assert_eq!(
                repository
                    .read_installation_input(&before.id)
                    .unwrap()
                    .unwrap(),
                intent
            );
            assert_eq!(receipt_count(&path), 0);
        }
        assert_eq!(
            raw.query_row::<String, _, _>(
                "SELECT payload FROM installation_inputs WHERE job_id=?1",
                [&before.id],
                |row| row.get(0)
            )
            .unwrap(),
            serde_json::to_string(&intent).unwrap()
        );
    }
}

#[test]
fn application_control_api_replays_after_restart_without_repeating_a_later_action() {
    use lumina_sdk_types::custom_requests::{
        ModelInstallationJobAction, ModelInstallationJobControlRequest, ModelInstallationJobStatus,
    };
    let fixture = Fixture::with_layout(true, true);
    let mut input = cache_input(&fixture);
    input.source = InstallationSource::ModelScope {};
    fixture
        .repository
        .adopt_installation_input(&fixture.request.job_id, &input, 32)
        .unwrap();
    let id = fixture.request.job_id.clone();
    let data = fixture.temp.path().join("data");
    drop(fixture.repository);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let application = InstallationApplication::new(
                &data,
                Arc::new(|| anyhow::bail!("No unit credentials")),
            );
            let pause = ModelInstallationJobControlRequest {
                job_id: id.clone(),
                expected_version: 0,
                action: ModelInstallationJobAction::Pause,
                idempotency_key: "api-pause".into(),
            };
            let paused = application.control_request(pause.clone()).await.unwrap();
            assert!(!paused.replayed);
            let resumed = application
                .control_request(ModelInstallationJobControlRequest {
                    job_id: id.clone(),
                    expected_version: paused.job.version,
                    action: ModelInstallationJobAction::Resume,
                    idempotency_key: "api-resume".into(),
                })
                .await
                .unwrap();
            assert!(!resumed.replayed);
            application.shutdown().await.unwrap();
            let restarted = InstallationApplication::new(
                &data,
                Arc::new(|| anyhow::bail!("No unit credentials")),
            );
            restarted.start_if_present().await.unwrap();
            restarted
                .service()
                .await
                .unwrap()
                .wait_ready()
                .await
                .unwrap();
            let replay = restarted.control_request(pause).await.unwrap();
            assert!(replay.replayed);
            assert!(matches!(
                replay.job.status,
                ModelInstallationJobStatus::Paused
            ));
            assert_eq!(replay.job.version, paused.job.version);
            let current = restarted
                .service()
                .await
                .unwrap()
                .repository()
                .read_job(&id)
                .unwrap()
                .unwrap();
            assert_eq!(current.status, JobStatus::Blocked);
            assert_eq!(current.version, resumed.job.version + 1);
            assert_eq!(
                restarted
                    .service()
                    .await
                    .unwrap()
                    .repository()
                    .latest_installation_failure(&id)
                    .unwrap()
                    .unwrap()
                    .details
                    .origin,
                InstallationFailureOrigin::Credentials
            );
            restarted.shutdown().await.unwrap();
        });
}
