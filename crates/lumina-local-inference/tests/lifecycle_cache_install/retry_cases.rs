use super::*;

fn downloading(fixture: &Fixture, now: i64) -> (WorkerLease, FileTransferRecord) {
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    let lease = fixture
        .repository
        .claim(&job.id, job.version, "retry-worker", now, 120)
        .unwrap();
    let job = fixture.repository.read_job(&job.id).unwrap().unwrap();
    if job.stage == LifecycleStage::ReadyToInstall {
        fixture
            .repository
            .advance(&lease, job.version, LifecycleStage::Downloading, now)
            .unwrap();
    }
    let file = fixture
        .repository
        .file_transfers(&job.id)
        .unwrap()
        .remove(0);
    fixture
        .repository
        .begin_file_attempt(&lease, &file.artifact_id, now)
        .unwrap();
    (lease, file)
}

fn fail(
    repository: &LifecycleRepository,
    lease: &WorkerLease,
    file: &FileTransferRecord,
    code: LifecycleErrorCode,
    now: i64,
) -> FileRetryBudget {
    repository
        .fail_installation_file(
            lease,
            &file.artifact_id,
            file.version,
            FileAttemptFailure {
                code,
                retry_after_seconds: None,
            },
            now,
        )
        .unwrap()
}

#[test]
fn transient_retry_budget_survives_reopen_exhausts_and_resets_once_per_control_receipt() {
    let fixture = Fixture::new(true);
    let id = &fixture.request.job_id;
    let path = fixture.temp.path().join("models.db");
    let mut now = 40;
    for attempt in 1..=FILE_RETRY_MAX_ATTEMPTS {
        let (lease, file) = downloading(&fixture, now);
        let budget = fail(
            &fixture.repository,
            &lease,
            &file,
            LifecycleErrorCode::SourceTimeout,
            now,
        );
        assert_eq!(budget.cycle_attempts, attempt);
        assert_eq!(budget.recorded_attempts, u64::from(attempt));
        let reopened = LifecycleRepository::open(&path).unwrap();
        assert_eq!(
            reopened.file_retry_budgets(id).unwrap(),
            vec![budget.clone()]
        );
        let job = reopened.read_job(id).unwrap().unwrap();
        assert!(reopened
            .begin_file_attempt(&lease, &file.artifact_id, now)
            .is_err());
        if attempt < FILE_RETRY_MAX_ATTEMPTS {
            let due = now + 2i64.pow(attempt);
            assert_eq!(budget.state, FileRetryState::Waiting { retry_at: due });
            assert_eq!(job.status, JobStatus::Queued);
            assert!(reopened
                .recoverable_jobs(due - 1, None, 1)
                .unwrap()
                .is_empty());
            assert_eq!(
                reopened.recoverable_jobs(due, None, 1).unwrap(),
                vec![job.clone()]
            );
            assert!(reopened
                .claim(id, job.version, "early", due - 1, 120)
                .is_err());
            now = due;
        } else {
            assert_eq!(budget.state, FileRetryState::Exhausted);
            assert_eq!(job.status, JobStatus::Failed);
            assert!(reopened
                .recoverable_jobs(now + 1000, None, 100)
                .unwrap()
                .is_empty());
        }
    }
    let job = fixture.repository.read_job(id).unwrap().unwrap();
    let request = control_request(
        id,
        job.version,
        InstallationJobAction::Retry,
        "manual-cycle",
    );
    let first = fixture
        .repository
        .control_installation(&request, &fixture.store, now + 1)
        .unwrap();
    let reset = fixture.repository.file_retry_budgets(id).unwrap().remove(0);
    assert_eq!(
        (reset.cycle, reset.cycle_attempts, reset.recorded_attempts),
        (1, 0, 3)
    );
    let second = fixture
        .repository
        .control_installation(&request, &fixture.store, now + 2)
        .unwrap();
    assert!(second.replayed);
    assert_eq!(first.job, second.job);
    assert_eq!(
        fixture.repository.file_retry_budgets(id).unwrap(),
        vec![reset]
    );
    fixture.run(now + 3).unwrap();
    let verified = fixture.repository.file_retry_budgets(id).unwrap().remove(0);
    assert_eq!(
        (verified.state, verified.cycle, verified.recorded_attempts),
        (FileRetryState::Verified, 1, 4)
    );
}

#[test]
fn active_attempt_is_preserved_across_lease_reclaim_and_explicit_pause_resume() {
    let fixture = Fixture::new(true);
    let (lease, file) = downloading(&fixture, 40);
    let active = fixture
        .repository
        .file_retry_budgets(&lease.job_id)
        .unwrap()
        .remove(0);
    assert_eq!(
        fixture
            .repository
            .begin_file_attempt(&lease, &file.artifact_id, 41)
            .unwrap(),
        active
    );
    let reopened = LifecycleRepository::open(&fixture.temp.path().join("models.db")).unwrap();
    let job = reopened.read_job(&lease.job_id).unwrap().unwrap();
    let recovered = reopened
        .claim(&job.id, job.version, "after-crash", 161, 120)
        .unwrap();
    assert_eq!(
        reopened
            .begin_file_attempt(&recovered, &file.artifact_id, 161)
            .unwrap(),
        active
    );
    assert!(fixture
        .repository
        .begin_file_attempt(&lease, &file.artifact_id, 161)
        .is_err());
    let job = reopened.read_job(&job.id).unwrap().unwrap();
    let paused = reopened
        .control_installation(
            &control_request(
                &job.id,
                job.version,
                InstallationJobAction::Pause,
                "pause-active",
            ),
            &fixture.store,
            162,
        )
        .unwrap();
    reopened
        .control_installation(
            &control_request(
                &job.id,
                paused.job.version,
                InstallationJobAction::Resume,
                "resume-active",
            ),
            &fixture.store,
            163,
        )
        .unwrap();
    fixture.run(164).unwrap();
    let verified = reopened.file_retry_budgets(&job.id).unwrap().remove(0);
    assert_eq!(
        (
            verified.cycle_attempts,
            verified.recorded_attempts,
            verified.cycle
        ),
        (1, 1, 0)
    );
}

#[test]
fn permanent_failures_require_user_action_and_never_schedule_background_retry() {
    for code in [
        LifecycleErrorCode::SourceAuthRequired,
        LifecycleErrorCode::SourceForbidden,
        LifecycleErrorCode::HashMismatch,
        LifecycleErrorCode::GgufInvalid,
        LifecycleErrorCode::FormatForbidden,
        LifecycleErrorCode::RevisionChanged,
        LifecycleErrorCode::DiskInsufficient,
        LifecycleErrorCode::SourcePartialFailure,
    ] {
        let fixture = Fixture::new(true);
        let (lease, file) = downloading(&fixture, 40);
        let budget = fail(&fixture.repository, &lease, &file, code, 41);
        assert_eq!(budget.state, FileRetryState::NeedsAction);
        assert_eq!(budget.last_failure.as_ref().unwrap().code, code);
        assert!(budget.next_retry_at().is_none());
        assert!(fixture
            .repository
            .recoverable_jobs(10000, None, 64)
            .unwrap()
            .is_empty());
        let job = fixture.repository.read_job(&lease.job_id).unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Failed);
        fixture
            .repository
            .control_installation(
                &control_request(
                    &job.id,
                    job.version,
                    InstallationJobAction::Retry,
                    "after-action",
                ),
                &fixture.store,
                42,
            )
            .unwrap();
        let reset = fixture
            .repository
            .file_retry_budgets(&job.id)
            .unwrap()
            .remove(0);
        assert_eq!(
            (reset.cycle, reset.cycle_attempts, reset.recorded_attempts),
            (1, 0, 1)
        );
    }
}

#[test]
fn retry_failure_and_manual_reset_are_atomic_with_job_file_and_control_events() {
    let fixture = Fixture::new(true);
    let path = fixture.temp.path().join("models.db");
    let (lease, file) = downloading(&fixture, 40);
    let before_job = fixture.repository.read_job(&lease.job_id).unwrap().unwrap();
    let before_budget = fixture
        .repository
        .file_retry_budgets(&lease.job_id)
        .unwrap();
    let before_events = fixture.repository.events(&lease.job_id, 0, 100).unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TRIGGER reject_retry BEFORE INSERT ON installation_file_retry_events WHEN NEW.version > 1 BEGIN SELECT RAISE(ABORT, 'injected retry failure'); END;").unwrap();
    assert!(fixture
        .repository
        .fail_installation_file(
            &lease,
            &file.artifact_id,
            file.version,
            FileAttemptFailure {
                code: LifecycleErrorCode::SourceTimeout,
                retry_after_seconds: None
            },
            41
        )
        .is_err());
    assert_eq!(
        fixture.repository.read_job(&lease.job_id).unwrap().unwrap(),
        before_job
    );
    assert_eq!(
        fixture.repository.file_transfers(&lease.job_id).unwrap(),
        vec![file.clone()]
    );
    assert_eq!(
        fixture
            .repository
            .file_retry_budgets(&lease.job_id)
            .unwrap(),
        before_budget
    );
    assert_eq!(
        fixture.repository.events(&lease.job_id, 0, 100).unwrap(),
        before_events
    );
    raw.execute_batch("DROP TRIGGER reject_retry;").unwrap();
    let stopped = fail(
        &fixture.repository,
        &lease,
        &file,
        LifecycleErrorCode::HashMismatch,
        41,
    );
    let job = fixture.repository.read_job(&lease.job_id).unwrap().unwrap();
    let request = control_request(
        &job.id,
        job.version,
        InstallationJobAction::Retry,
        "atomic-reset",
    );
    raw.execute_batch("CREATE TRIGGER reject_control BEFORE INSERT ON installation_control_receipts BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END;").unwrap();
    assert!(fixture
        .repository
        .control_installation(&request, &fixture.store, 42)
        .is_err());
    assert_eq!(
        fixture.repository.file_retry_budgets(&job.id).unwrap(),
        vec![stopped]
    );
    assert_eq!(fixture.repository.read_job(&job.id).unwrap().unwrap(), job);
    raw.execute_batch("DROP TRIGGER reject_control;").unwrap();
    assert!(
        !fixture
            .repository
            .control_installation(&request, &fixture.store, 43)
            .unwrap()
            .replayed
    );
}

#[test]
fn concurrent_failure_recording_consumes_one_attempt_and_fences_the_other_connection() {
    let fixture = Fixture::new(true);
    let (lease, file) = downloading(&fixture, 40);
    let other = LifecycleRepository::open(&fixture.temp.path().join("models.db")).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let workers: Vec<_> = [&fixture.repository, &other]
            .into_iter()
            .map(|repository| {
                let barrier = &barrier;
                let lease = &lease;
                let file = &file;
                scope.spawn(move || {
                    barrier.wait();
                    repository.fail_installation_file(
                        lease,
                        &file.artifact_id,
                        file.version,
                        FileAttemptFailure {
                            code: LifecycleErrorCode::SourceUnavailable,
                            retry_after_seconds: None,
                        },
                        41,
                    )
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let budget = other.file_retry_budgets(&lease.job_id).unwrap().remove(0);
    assert_eq!(
        (
            budget.cycle_attempts,
            budget.recorded_attempts,
            budget.version
        ),
        (1, 1, 2)
    );
    assert_eq!(budget.state, FileRetryState::Waiting { retry_at: 43 });
}

#[test]
fn rate_limit_retry_uses_transport_delay_and_preserves_checkpoint() {
    let fixture = Fixture::new(true);
    let (lease, file) = downloading(&fixture, 40);
    let checkpoint = TransferCheckpoint {
        completed_bytes: 128,
        prefix_sha256: Sha256Digest::of_bytes(&fixture.bytes[..128]),
        resume_validator: ResumeValidator::StrongEtag {
            value: "\"unit-etag\"".into(),
        },
    };
    let file = fixture
        .repository
        .checkpoint_file(
            &lease,
            &file.artifact_id,
            file.version,
            checkpoint.clone(),
            41,
        )
        .unwrap();
    let budget = fixture
        .repository
        .fail_installation_file(
            &lease,
            &file.artifact_id,
            file.version,
            FileAttemptFailure {
                code: LifecycleErrorCode::SourceRateLimited,
                retry_after_seconds: Some(30),
            },
            42,
        )
        .unwrap();
    assert_eq!(budget.state, FileRetryState::Waiting { retry_at: 72 });
    assert_eq!(
        fixture
            .repository
            .read_file_transfer(&lease.job_id, &file.artifact_id)
            .unwrap()
            .unwrap()
            .checkpoint,
        checkpoint
    );
    assert!(fixture
        .repository
        .recoverable_jobs(71, None, 64)
        .unwrap()
        .is_empty());
    let job = fixture
        .repository
        .recoverable_jobs(72, None, 64)
        .unwrap()
        .remove(0);
    let next = fixture
        .repository
        .claim(&job.id, job.version, "after-delay", 72, 120)
        .unwrap();
    let file = fixture
        .repository
        .read_file_transfer(&job.id, &file.artifact_id)
        .unwrap()
        .unwrap();
    fixture
        .repository
        .begin_file_attempt(&next, &file.artifact_id, 72)
        .unwrap();
    let resumed = fixture
        .repository
        .resume_failed_file(&next, &file.artifact_id, file.version, 72)
        .unwrap();
    assert_eq!(resumed.checkpoint, checkpoint);
    assert_eq!(resumed.state, FileTransferState::Downloading);
}

#[test]
fn retry_budget_tampering_and_rehashed_history_replacement_fail_closed() {
    let fixture = Fixture::new(true);
    let (lease, file) = downloading(&fixture, 40);
    let original = fixture
        .repository
        .file_retry_budgets(&lease.job_id)
        .unwrap()
        .remove(0);
    let mut changed = original.clone();
    changed.recorded_attempts += 1;
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute(
        "UPDATE installation_file_retries SET payload=?1",
        [serde_json::to_string(&changed).unwrap()],
    )
    .unwrap();
    assert!(fixture
        .repository
        .file_retry_budgets(&lease.job_id)
        .is_err());
    raw.execute(
        "UPDATE installation_file_retries SET digest=?1",
        [Sha256Digest::of_serializable(&changed).unwrap().as_str()],
    )
    .unwrap();
    assert!(fixture
        .repository
        .file_retry_budgets(&lease.job_id)
        .is_err());
    assert!(fixture
        .repository
        .begin_file_attempt(&lease, &file.artifact_id, 41)
        .is_err());
}

#[test]
fn v8_migration_preserves_control_receipts_and_marks_previous_attempt_history_unknown() {
    let fixture = Fixture::new(true);
    let id = &fixture.request.job_id;
    let path = fixture.temp.path().join("models.db");
    let request = control_request(id, 0, InstallationJobAction::Pause, "v8-pause");
    let original = fixture
        .repository
        .control_installation(&request, &fixture.store, 40)
        .unwrap();
    drop(fixture.repository);
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("DROP TABLE installation_failures; DROP TABLE installation_file_retries; DROP TABLE installation_file_retry_events; PRAGMA user_version=8;").unwrap();
    let reopened = LifecycleRepository::open(&path).unwrap();
    assert_eq!(reopened.read_job(id).unwrap().unwrap(), original.job);
    let replay = reopened
        .control_installation(&request, &fixture.store, 41)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.job, original.job);
    let budget = reopened.file_retry_budgets(id).unwrap().remove(0);
    assert_eq!(budget.history_origin, RetryHistoryOrigin::LegacyUntracked);
    assert_eq!((budget.recorded_attempts, budget.cycle_attempts), (0, 0));
    assert_eq!(
        raw.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        MODEL_DATABASE_SCHEMA_VERSION
    );
}

#[test]
fn v8_retry_migration_rolls_back_on_corrupt_file_or_schema_collision() {
    for corrupt_file in [false, true] {
        let fixture = Fixture::new(true);
        let path = fixture.temp.path().join("models.db");
        drop(fixture.repository);
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch("DROP TABLE installation_failures; DROP TABLE installation_file_retries; DROP TABLE installation_file_retry_events; PRAGMA user_version=8;").unwrap();
        if corrupt_file {
            raw.execute("UPDATE job_files SET payload='{}'", [])
                .unwrap();
        } else {
            raw.execute_batch("CREATE TABLE installation_file_retries (collision INTEGER);")
                .unwrap();
        }
        assert!(LifecycleRepository::open(&path).is_err());
        assert_eq!(
            raw.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            8
        );
        assert_eq!(
            raw.query_row::<u32, _, _>(
                "SELECT count(*) FROM sqlite_master WHERE name='installation_file_retry_events'",
                [],
                |row| row.get(0)
            )
            .unwrap(),
            0
        );
    }
}

#[test]
fn application_job_query_exposes_durable_failure_without_changing_control_result_snapshots() {
    let fixture = Fixture::with_layout(true, true);
    let (lease, file) = downloading(&fixture, 40);
    let budget = fail(
        &fixture.repository,
        &lease,
        &file,
        LifecycleErrorCode::SourceTimeout,
        41,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let application =
            InstallationApplication::new(&fixture.temp.path().join("data"), Arc::new(|| Ok(None)));
        let response = application
            .list_request(
                lumina_sdk_types::custom_requests::ModelInstallationJobsListRequest::default(),
            )
            .await
            .unwrap();
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["fileRetries"][0]["jobId"], lease.job_id);
        assert_eq!(value["fileRetries"][0]["state"], "waiting");
        assert_eq!(value["fileRetries"][0]["nextRetryAt"], 43);
        assert_eq!(
            value["fileRetries"][0]["recordedAttempts"],
            budget.recorded_attempts
        );
        assert_eq!(
            value["fileRetries"][0]["lastFailure"]["code"],
            "SOURCE_TIMEOUT"
        );
        assert!(value["jobs"][0].get("fileRetries").is_none());
        assert_eq!(application.state(), InstallationApplicationState::Dormant);
    });
}

#[test]
fn application_worker_discovers_due_retry_and_finishes_without_resubmission() {
    let fixture = Fixture::new(true);
    let (lease, file) = downloading(&fixture, 40);
    fail(
        &fixture.repository,
        &lease,
        &file,
        LifecycleErrorCode::SourceTimeout,
        41,
    );
    let input = InstallationInput::new(
        &fixture.store,
        fixture.request.installation_id.clone(),
        fixture.request.version_id.clone(),
        InstallationSource::LocalCache {
            files: fixture.request.source_files.clone(),
        },
    )
    .unwrap();
    fixture
        .repository
        .adopt_installation_input(&lease.job_id, &input, 41)
        .unwrap();
    let Fixture {
        repository, store, ..
    } = fixture;
    let repository = Arc::new(repository);
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(42));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let service = InstallationService::start(
            repository.clone(),
            Arc::new(store),
            Arc::new(|| panic!("Local cache retries do not use credentials")),
            {
                let clock = clock.clone();
                Arc::new(move || clock.load(std::sync::atomic::Ordering::SeqCst))
            },
            std::time::Duration::from_millis(10),
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), service.wait_ready())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            repository.read_job(&lease.job_id).unwrap().unwrap().status,
            JobStatus::Queued
        );
        assert_eq!(
            repository.file_retry_budgets(&lease.job_id).unwrap()[0].recorded_attempts,
            1
        );
        clock.store(43, std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if repository.read_job(&lease.job_id).unwrap().unwrap().status
                    == JobStatus::Completed
                {
                    break;
                }
                assert_ne!(service.state(), InstallationServiceState::Failed);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        service.shutdown().await.unwrap();
        let budget = repository
            .file_retry_budgets(&lease.job_id)
            .unwrap()
            .remove(0);
        assert_eq!(
            (budget.state, budget.recorded_attempts, budget.cycle),
            (FileRetryState::Verified, 2, 0)
        );
    });
}
