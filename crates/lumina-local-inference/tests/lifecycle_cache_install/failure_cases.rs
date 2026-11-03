use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn bind(fixture: &Fixture, network: bool) -> LifecycleJob {
    let mut input = cache_input(fixture);
    if network {
        input.source = InstallationSource::ModelScope {};
    }
    fixture
        .repository
        .adopt_installation_input(&fixture.request.job_id, &input, 32)
        .unwrap();
    fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap()
}

fn admission(code: LifecycleErrorCode) -> InstallationFailureDetails {
    InstallationFailureDetails::operation(code, InstallationFailureOrigin::Admission)
}

async fn wait_blocked(repository: &LifecycleRepository, id: &str, after: u64) -> LifecycleJob {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let job = repository.read_job(id).unwrap().unwrap();
            if job.status == JobStatus::Blocked && job.version > after {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[test]
fn credential_failure_is_persisted_once_across_polls_and_restart_until_explicit_retry() {
    let fixture = Fixture::new(true);
    let job = bind(&fixture, true);
    let path = fixture.temp.path().join("models.db");
    let calls = Arc::new(AtomicUsize::new(0));
    let credentials: InstallationCredentials = {
        let calls = calls.clone();
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!(
                "private-sentinel-token https://unit.invalid/?token=private-sentinel-token"
            )
        })
    };
    let repository = Arc::new(fixture.repository);
    let store = Arc::new(fixture.store);
    service_runtime().block_on(async {
        let service = InstallationService::start(
            repository.clone(),
            store.clone(),
            credentials.clone(),
            Arc::new(|| 500),
            std::time::Duration::from_millis(10),
        )
        .unwrap();
        service.wait_ready().await.unwrap();
        let blocked = wait_blocked(&repository, &job.id, 0).await;
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository.file_retry_budgets(&job.id).unwrap()[0].recorded_attempts,
            0
        );
        let first = repository
            .latest_installation_failure(&job.id)
            .unwrap()
            .unwrap();
        assert_eq!(first.details.origin, InstallationFailureOrigin::Credentials);
        assert_eq!(
            first.failure.message_key,
            "models.installation.failure.source_auth_required"
        );
        assert_eq!(first.job, blocked);
        service.shutdown().await.unwrap();
        let reopened = Arc::new(LifecycleRepository::open(&path).unwrap());
        let restarted = InstallationService::start(
            reopened.clone(),
            store,
            credentials,
            Arc::new(|| 501),
            std::time::Duration::from_millis(10),
        )
        .unwrap();
        restarted.wait_ready().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            reopened.latest_installation_failure(&job.id).unwrap(),
            Some(first)
        );
        restarted
            .control_job(&control_request(
                &job.id,
                blocked.version,
                InstallationJobAction::Retry,
                "credentials-retry",
            ))
            .unwrap();
        let next = wait_blocked(&reopened, &job.id, blocked.version).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            reopened
                .latest_installation_failure(&job.id)
                .unwrap()
                .unwrap()
                .job,
            next
        );
        restarted.shutdown().await.unwrap();
    });
    let raw = rusqlite::Connection::open(path).unwrap();
    let payloads: Vec<String> = raw
        .prepare("SELECT payload FROM installation_failures")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(payloads.len(), 2);
    assert!(payloads
        .iter()
        .all(|p| !p.contains("private-sentinel-token") && !p.contains("https://unit.invalid")));
}

#[test]
fn late_credential_failure_cannot_override_pause_or_another_workers_live_lease() {
    for pause in [true, false] {
        let fixture = Fixture::new(true);
        let observed = bind(&fixture, true);
        let other = LifecycleRepository::open(&fixture.temp.path().join("models.db")).unwrap();
        if pause {
            other
                .control_installation(
                    &control_request(
                        &observed.id,
                        observed.version,
                        InstallationJobAction::Pause,
                        "user-pause",
                    ),
                    &fixture.store,
                    40,
                )
                .unwrap();
        } else {
            other
                .claim(&observed.id, observed.version, "other-worker", 40, 120)
                .unwrap();
        }
        let winner = other.read_job(&observed.id).unwrap().unwrap();
        assert!(fixture
            .repository
            .block_installation(
                &observed,
                &fixture.store,
                InstallationFailureDetails::operation(
                    LifecycleErrorCode::SourceAuthRequired,
                    InstallationFailureOrigin::Credentials
                ),
                41
            )
            .unwrap()
            .is_none());
        assert_eq!(other.read_job(&observed.id).unwrap().unwrap(), winner);
        assert!(other
            .latest_installation_failure(&observed.id)
            .unwrap()
            .is_none());
    }
}

#[test]
fn failure_recording_requires_a_bound_matching_store_and_fences_expired_workers() {
    let fixture = Fixture::new(true);
    let legacy = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    assert!(fixture
        .repository
        .block_installation(
            &legacy,
            &fixture.store,
            admission(LifecycleErrorCode::DiskInsufficient),
            40
        )
        .unwrap()
        .is_none());
    let job = bind(&fixture, false);
    let wrong_store = InstallationStore::open(&fixture.temp.path().join("wrong-store")).unwrap();
    assert!(fixture
        .repository
        .block_installation(
            &job,
            &wrong_store,
            admission(LifecycleErrorCode::DiskInsufficient),
            40
        )
        .unwrap()
        .is_none());
    let lease = fixture
        .repository
        .claim(&job.id, job.version, "expired", 40, 1)
        .unwrap();
    let running = fixture.repository.read_job(&job.id).unwrap().unwrap();
    assert!(fixture
        .repository
        .block_installation(
            &running,
            &fixture.store,
            admission(LifecycleErrorCode::DiskInsufficient),
            40
        )
        .unwrap()
        .is_none());
    let blocked = fixture
        .repository
        .block_installation(
            &running,
            &fixture.store,
            admission(LifecycleErrorCode::DiskInsufficient),
            41,
        )
        .unwrap()
        .unwrap();
    assert_eq!(blocked.job.status, JobStatus::Blocked);
    assert!(blocked.job.generation > lease.generation);
    assert!(fixture
        .repository
        .renew_lease(&lease, blocked.job.version, 42, 120)
        .is_err());
    assert!(fixture
        .repository
        .fail_installation_operation(
            &lease,
            blocked.job.version,
            InstallationFailureDetails::operation(
                LifecycleErrorCode::InstallFailed,
                InstallationFailureOrigin::Commit
            ),
            42
        )
        .is_err());
}

#[test]
fn actual_commit_path_failure_has_durable_details_and_can_resume_after_storage_repair() {
    let fixture = Fixture::new(true);
    bind(&fixture, false);
    let obstacle = fixture.store.root().join("blobs");
    std::fs::write(&obstacle, b"owned-test-obstacle").unwrap();
    assert!(fixture.run(40).is_err());
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (job.status, job.stage),
        (JobStatus::Failed, LifecycleStage::Installing)
    );
    let failure = fixture
        .repository
        .latest_installation_failure(&job.id)
        .unwrap()
        .unwrap();
    assert_eq!(failure.details.origin, InstallationFailureOrigin::Commit);
    assert_eq!(failure.details.code, LifecycleErrorCode::InstallFailed);
    assert!(failure.details.artifact_id.is_none());
    assert_eq!(
        fixture.repository.file_retry_budgets(&job.id).unwrap()[0].state,
        FileRetryState::Verified
    );
    assert!(fixture
        .repository
        .active_installations()
        .unwrap()
        .is_empty());
    assert_eq!(std::fs::read(&obstacle).unwrap(), b"owned-test-obstacle");
    std::fs::remove_file(&obstacle).unwrap();
    fixture
        .repository
        .control_installation(
            &control_request(
                &job.id,
                job.version,
                InstallationJobAction::Retry,
                "after-storage-repair",
            ),
            &fixture.store,
            41,
        )
        .unwrap();
    fixture.run(42).unwrap();
    assert_eq!(
        fixture
            .repository
            .latest_installation_failure(&job.id)
            .unwrap(),
        Some(failure)
    );
    assert_eq!(
        fixture.repository.file_retry_budgets(&job.id).unwrap()[0].recorded_attempts,
        1
    );
    assert_eq!(
        std::fs::read(fixture.request.source_files.values().next().unwrap()).unwrap(),
        fixture.bytes
    );
}

#[test]
fn admission_classifies_disk_pressure_without_expiring_an_already_accepted_plan() {
    let fixture = Fixture::new(true);
    let job = bind(&fixture, false);
    let plan = fixture.repository.read_plan(&job.plan_id).unwrap().unwrap();
    let error =
        validate_installation_admission(&plan, job.accepted_at, plan.required_disk_bytes - 1)
            .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<InstallationOperationError>()
            .unwrap()
            .code,
        LifecycleErrorCode::DiskInsufficient
    );
    validate_installation_admission(&plan, job.accepted_at, u64::MAX).unwrap();
    let failure = fixture
        .repository
        .block_installation(
            &job,
            &fixture.store,
            admission(LifecycleErrorCode::DiskInsufficient),
            plan.expires_at + 1,
        )
        .unwrap()
        .unwrap();
    assert_eq!(failure.details.code, LifecycleErrorCode::DiskInsufficient);
    assert!(failure.failure.recoverable);
    assert_eq!(failure.job.status, JobStatus::Blocked);
    assert_eq!(
        fixture.repository.file_retry_budgets(&job.id).unwrap()[0].recorded_attempts,
        0
    );
}

#[test]
fn failure_insert_rollback_preserves_job_file_budget_and_all_events() {
    let fixture = Fixture::new(true);
    let job = bind(&fixture, false);
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute_batch("CREATE TRIGGER reject_failure BEFORE INSERT ON installation_failures BEGIN SELECT RAISE(ABORT,'injected failure write'); END;").unwrap();
    assert!(fixture
        .repository
        .block_installation(
            &job,
            &fixture.store,
            admission(LifecycleErrorCode::DiskInsufficient),
            40
        )
        .is_err());
    assert_eq!(fixture.repository.read_job(&job.id).unwrap().unwrap(), job);
    assert!(fixture
        .repository
        .latest_installation_failure(&job.id)
        .unwrap()
        .is_none());
    let lease = fixture
        .repository
        .claim(&job.id, job.version, "file-worker", 40, 120)
        .unwrap();
    let current = fixture.repository.read_job(&job.id).unwrap().unwrap();
    fixture
        .repository
        .advance(&lease, current.version, LifecycleStage::Downloading, 40)
        .unwrap();
    let file = fixture
        .repository
        .file_transfers(&job.id)
        .unwrap()
        .remove(0);
    fixture
        .repository
        .begin_file_attempt(&lease, &file.artifact_id, 40)
        .unwrap();
    let before_job = fixture.repository.read_job(&job.id).unwrap().unwrap();
    let budgets = fixture.repository.file_retry_budgets(&job.id).unwrap();
    let events = fixture.repository.events(&job.id, 0, 100).unwrap();
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
        fixture.repository.read_job(&job.id).unwrap().unwrap(),
        before_job
    );
    assert_eq!(
        fixture.repository.file_transfers(&job.id).unwrap(),
        vec![file]
    );
    assert_eq!(
        fixture.repository.file_retry_budgets(&job.id).unwrap(),
        budgets
    );
    assert_eq!(fixture.repository.events(&job.id, 0, 100).unwrap(), events);
    assert!(fixture
        .repository
        .fail_installation_operation(
            &lease,
            before_job.version,
            InstallationFailureDetails::operation(
                LifecycleErrorCode::InstallFailed,
                InstallationFailureOrigin::Transfer
            ),
            41
        )
        .is_err());
    assert_eq!(
        fixture.repository.read_job(&job.id).unwrap().unwrap(),
        before_job
    );
    raw.execute_batch("DROP TRIGGER reject_failure;").unwrap();
}

#[test]
fn failure_query_distinguishes_current_failure_from_retry_and_completed_history() {
    let fixture = Fixture::with_layout(true, true);
    let mut wrong = fixture.bytes.clone();
    wrong[1000] ^= 1;
    std::fs::write(fixture.request.source_files.values().next().unwrap(), wrong).unwrap();
    fixture.run(40).unwrap_err();
    let id = fixture.request.job_id.clone();
    let failed = fixture.repository.read_job(&id).unwrap().unwrap();
    service_runtime().block_on(async {
        let app =
            InstallationApplication::new(&fixture.temp.path().join("data"), Arc::new(|| Ok(None)));
        let first = app.list_request(Default::default()).await.unwrap();
        assert_eq!(first.failures.len(), 1);
        assert!(first.failures[0].is_current);
        assert!(matches!(
            first.failures[0].code,
            lumina_sdk_types::custom_requests::ModelInstallationFailureCode::HashMismatch
        ));
        assert_eq!(
            first.failures[0].artifact_id,
            Some(fixture.request.source_files.keys().next().unwrap().clone())
        );
        fixture
            .repository
            .control_installation(
                &control_request(
                    &id,
                    failed.version,
                    InstallationJobAction::Retry,
                    "query-retry",
                ),
                &fixture.store,
                41,
            )
            .unwrap();
        let retried = app.list_request(Default::default()).await.unwrap();
        assert!(!retried.failures[0].is_current);
        assert_eq!(retried.failures[0].job_version, failed.version);
        std::fs::write(
            fixture.request.source_files.values().next().unwrap(),
            &fixture.bytes,
        )
        .unwrap();
        fixture.run(42).unwrap();
        let completed = app.list_request(Default::default()).await.unwrap();
        assert!(!completed.failures[0].is_current);
        assert_eq!(completed.failures[0].cause_id, first.failures[0].cause_id);
        assert_eq!(app.state(), InstallationApplicationState::Dormant);
    });
}

#[test]
fn tampered_failure_and_rehashed_foreign_history_are_rejected() {
    let fixture = Fixture::new(true);
    let job = bind(&fixture, true);
    let record = fixture
        .repository
        .block_installation(
            &job,
            &fixture.store,
            InstallationFailureDetails::operation(
                LifecycleErrorCode::SourceAuthRequired,
                InstallationFailureOrigin::Credentials,
            ),
            40,
        )
        .unwrap()
        .unwrap();
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute("UPDATE installation_failures SET payload='{}'", [])
        .unwrap();
    assert!(fixture
        .repository
        .latest_installation_failure(&job.id)
        .is_err());
    let mut changed = record.clone();
    changed
        .failure
        .message_args
        .insert("token".into(), "private-sentinel".into());
    raw.execute(
        "UPDATE installation_failures SET payload=?1,digest=?2",
        rusqlite::params![
            serde_json::to_string(&changed).unwrap(),
            Sha256Digest::of_serializable(&changed).unwrap().as_str()
        ],
    )
    .unwrap();
    assert!(fixture
        .repository
        .latest_installation_failure(&job.id)
        .is_err());
    changed = record;
    changed.job.generation += 1;
    raw.execute(
        "UPDATE installation_failures SET payload=?1,digest=?2",
        rusqlite::params![
            serde_json::to_string(&changed).unwrap(),
            Sha256Digest::of_serializable(&changed).unwrap().as_str()
        ],
    )
    .unwrap();
    assert!(fixture
        .repository
        .latest_installation_failure(&job.id)
        .is_err());
}

#[test]
fn schema_nine_failure_upgrade_preserves_receipts_and_does_not_invent_prior_failures() {
    for collision in [false, true] {
        let fixture = Fixture::new(true);
        let path = fixture.temp.path().join("models.db");
        let pause = control_request(
            &fixture.request.job_id,
            0,
            InstallationJobAction::Pause,
            "v9-pause",
        );
        let receipt = fixture
            .repository
            .control_installation(&pause, &fixture.store, 40)
            .unwrap();
        let budgets = fixture
            .repository
            .file_retry_budgets(&pause.job_id)
            .unwrap();
        drop(fixture.repository);
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch("DROP TABLE installation_transfer_credits; DROP TABLE installation_storage_reservations; DROP TABLE installation_storage_events; DROP TABLE installation_failures; PRAGMA user_version=9;")
            .unwrap();
        if collision {
            raw.execute_batch("CREATE TABLE installation_failures (legacy INTEGER); INSERT INTO installation_failures VALUES (17);").unwrap();
            assert!(LifecycleRepository::open(&path).is_err());
            assert_eq!(
                raw.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                    .unwrap(),
                9
            );
            assert_eq!(
                raw.query_row::<u32, _, _>("SELECT legacy FROM installation_failures", [], |row| {
                    row.get(0)
                })
                .unwrap(),
                17
            );
        } else {
            let reopened = LifecycleRepository::open(&path).unwrap();
            assert_eq!(
                reopened.read_job(&pause.job_id).unwrap().unwrap(),
                receipt.job
            );
            assert_eq!(reopened.file_retry_budgets(&pause.job_id).unwrap(), budgets);
            assert!(reopened
                .latest_installation_failure(&pause.job_id)
                .unwrap()
                .is_none());
            let replay = reopened
                .control_installation(&pause, &fixture.store, 41)
                .unwrap();
            assert!(replay.replayed);
            assert_eq!(replay.job, receipt.job);
            assert_eq!(
                raw.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                    .unwrap(),
                MODEL_DATABASE_SCHEMA_VERSION
            );
        }
    }
}

#[test]
fn cancellation_acknowledgement_never_resolves_credentials_or_records_failure() {
    let fixture = Fixture::new(true);
    let job = bind(&fixture, true);
    fixture
        .repository
        .claim(&job.id, job.version, "old-worker", 40, 120)
        .unwrap();
    let running = fixture.repository.read_job(&job.id).unwrap().unwrap();
    fixture
        .repository
        .transition(&job.id, running.version, JobStatus::Cancelling, 41)
        .unwrap();
    let repository = Arc::new(fixture.repository);
    service_runtime().block_on(async {
        let page = recover_installations_page(
            repository.clone(),
            Arc::new(fixture.store),
            RecoveryPageRequest {
                after_id: None,
                limit: 64,
                worker_name: "cancel-ack".into(),
            },
            Arc::new(|| panic!("Cancellation does not require credentials")),
            || 42,
        )
        .await
        .unwrap();
        assert_eq!(page.outcomes[0].disposition, RecoveryDisposition::Cancelled);
        assert_eq!(
            repository.read_job(&job.id).unwrap().unwrap().status,
            JobStatus::Cancelled
        );
        assert!(repository
            .latest_installation_failure(&job.id)
            .unwrap()
            .is_none());
        assert_eq!(
            repository.file_retry_budgets(&job.id).unwrap()[0].recorded_attempts,
            0
        );
    });
}

#[test]
fn malformed_ephemeral_token_blocks_before_claim_without_persisting_token_material() {
    let fixture = Fixture::new(true);
    let job = bind(&fixture, true);
    let repository = Arc::new(fixture.repository);
    service_runtime().block_on(async {
        let page = recover_installations_page(
            repository.clone(),
            Arc::new(fixture.store),
            RecoveryPageRequest {
                after_id: None,
                limit: 64,
                worker_name: "invalid-token".into(),
            },
            Arc::new(|| Ok(Some("private-token-sentinel\nbad".into()))),
            || 40,
        )
        .await
        .unwrap();
        assert_eq!(
            page.outcomes[0].disposition,
            RecoveryDisposition::Failed {
                code: LifecycleErrorCode::SourceAuthRequired
            }
        );
        let failure = repository
            .latest_installation_failure(&job.id)
            .unwrap()
            .unwrap();
        assert_eq!(failure.job.status, JobStatus::Blocked);
        assert_eq!(
            failure.details.origin,
            InstallationFailureOrigin::Credentials
        );
        assert!(!serde_json::to_string(&failure)
            .unwrap()
            .contains("private-token-sentinel"));
        assert_eq!(
            repository.file_retry_budgets(&job.id).unwrap()[0].recorded_attempts,
            0
        );
    });
}
