use super::*;

fn request(fixture: &Fixture, id: &str) -> (JobAcceptance, InstallationInput) {
    let request = acceptance(fixture, id);
    let mut input = cache_input(fixture);
    input.installation_id = format!("installation:{id}");
    input.version_id = format!("version:{id}");
    (request, input)
}

fn bind(fixture: &Fixture) -> StorageReservation {
    fixture
        .repository
        .adopt_installation_input(&fixture.request.job_id, &cache_input(fixture), 32)
        .unwrap();
    fixture
        .repository
        .storage_reservation(&fixture.request.job_id)
        .unwrap()
        .unwrap()
}

#[test]
fn new_admission_pins_an_existing_registered_blob_and_reserves_only_the_difference() {
    let fixture = Fixture::new(true);
    let original = bind(&fixture);
    fixture.run(40).unwrap();
    let (acceptance, input) = request(&fixture, "reuse-registered-blob");
    let size = fixture.bytes.len() as u64;
    let reduced = original.peak_bytes - size;
    assert!(fixture
        .repository
        .enqueue_installation(&acceptance, &input, 41, reduced - 1)
        .is_err());
    assert!(fixture
        .repository
        .read_job(&acceptance.id)
        .unwrap()
        .is_none());
    fixture
        .repository
        .enqueue_installation(&acceptance, &input, 42, reduced)
        .unwrap();
    let reservation = fixture
        .repository
        .storage_reservation(&acceptance.id)
        .unwrap()
        .unwrap();
    assert_eq!(reservation.peak_bytes, original.peak_bytes);
    assert_eq!(reservation.reserved_bytes, reduced);
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    let digest = Sha256Digest::of_bytes(&fixture.bytes);
    let pin: (String, u64) = raw
        .query_row(
            "SELECT pinned_blob, bytes FROM installation_blob_admission WHERE job_id=?1",
            [&acceptance.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(pin, (digest.as_str().to_string(), size));
    let second = CacheInstallRequest {
        job_id: acceptance.id.clone(),
        installation_id: input.installation_id,
        version_id: input.version_id,
        source_files: fixture.request.source_files.clone(),
    };
    install_from_local_cache(
        &fixture.repository,
        &fixture.store,
        &second,
        "second-worker",
        || 43,
    )
    .unwrap();
    let remaining_pins: u64 = raw
        .query_row(
            "SELECT COUNT(*) FROM installation_blob_admission WHERE job_id=?1",
            [&acceptance.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_pins, 0);
    assert_eq!(fixture.repository.blob_reference_count(&digest).unwrap(), 2);
}

#[test]
fn concurrent_connections_admit_only_one_peak_when_capacity_cannot_cover_both() {
    let fixture = Fixture::new(true);
    let peak = fixture
        .repository
        .read_plan("unit-plan")
        .unwrap()
        .unwrap()
        .required_disk_bytes;
    let other = LifecycleRepository::open(&fixture.temp.path().join("models.db")).unwrap();
    let (a, ai) = request(&fixture, "quota-a");
    let (b, bi) = request(&fixture, "quota-b");
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            fixture.repository.enqueue_installation(&a, &ai, 40, peak)
        });
        let second = scope.spawn(|| {
            barrier.wait();
            other.enqueue_installation(&b, &bi, 40, peak)
        });
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let error = results.iter().find_map(|r| r.as_ref().err()).unwrap();
    assert_eq!(
        error
            .downcast_ref::<InstallationOperationError>()
            .unwrap()
            .code,
        LifecycleErrorCode::DiskInsufficient
    );
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(
        fixture
            .repository
            .storage_reservation(&winner.id)
            .unwrap()
            .unwrap()
            .reserved_bytes,
        peak
    );
    assert_eq!(fixture.repository.list_jobs(None, 100).unwrap().len(), 2);
    assert!(fixture
        .repository
        .storage_reservation(if winner.id == a.id { &b.id } else { &a.id })
        .unwrap()
        .is_none());
}

#[test]
fn directories_on_the_same_volume_share_one_admission_pool() {
    let fixture = Fixture::new(true);
    let second_store = InstallationStore::open(&fixture.temp.path().join("second-root")).unwrap();
    assert_eq!(
        installation_storage_volume(fixture.store.root()).unwrap(),
        installation_storage_volume(second_store.root()).unwrap()
    );
    let peak = fixture
        .repository
        .read_plan("unit-plan")
        .unwrap()
        .unwrap()
        .required_disk_bytes;
    let (a, ai) = request(&fixture, "first-root");
    fixture
        .repository
        .enqueue_installation(&a, &ai, 40, peak)
        .unwrap();
    let (b, mut bi) = request(&fixture, "second-root");
    bi.store_root = second_store.root().to_path_buf();
    assert!(fixture
        .repository
        .enqueue_installation(&b, &bi, 41, peak)
        .is_err());
    assert!(fixture.repository.read_job(&b.id).unwrap().is_none());
}

#[test]
fn accepted_request_and_input_replays_do_not_reacquire_released_space() {
    let fixture = Fixture::new(true);
    let (request, input) = request(&fixture, "replay-space");
    let job = fixture
        .repository
        .enqueue_installation(&request, &input, 40, 1 << 30)
        .unwrap();
    let cancelled = fixture
        .repository
        .control_installation(
            &control_request(
                &job.id,
                job.version,
                InstallationJobAction::Cancel,
                "cancel-space",
            ),
            &fixture.store,
            41,
        )
        .unwrap();
    assert!(fixture
        .repository
        .release_inactive_storage(&job.id, &fixture.store, 42)
        .unwrap());
    let released = fixture
        .repository
        .storage_reservation(&job.id)
        .unwrap()
        .unwrap();
    assert_eq!(released.reserved_bytes, 0);
    assert_eq!(
        fixture
            .repository
            .enqueue_installation(&request, &input, 10000, 0)
            .unwrap(),
        cancelled.job
    );
    fixture
        .repository
        .adopt_installation_input(&job.id, &input, 10000)
        .unwrap();
    assert_eq!(
        fixture.repository.storage_reservation(&job.id).unwrap(),
        Some(released)
    );
}

#[test]
fn paused_and_cancelled_tasks_hold_space_until_the_writer_lock_is_quiescent() {
    let fixture = Fixture::new(true);
    let held = bind(&fixture);
    let job = fixture.repository.read_job(&held.job_id).unwrap().unwrap();
    let paused = fixture
        .repository
        .control_installation(
            &control_request(
                &job.id,
                job.version,
                InstallationJobAction::Pause,
                "pause-space",
            ),
            &fixture.store,
            40,
        )
        .unwrap()
        .job;
    assert!(!fixture
        .repository
        .release_inactive_storage(&job.id, &fixture.store, 41)
        .unwrap());
    assert_eq!(
        fixture.repository.storage_reservation(&job.id).unwrap(),
        Some(held.clone())
    );
    let path = fixture.store.root().join("locks/transfers").join(format!(
        "{}.lock",
        Sha256Digest::of_bytes(job.id.as_bytes()).as_str()
    ));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = std::fs::File::create(path).unwrap();
    lock.try_lock_exclusive().unwrap();
    fixture
        .repository
        .control_installation(
            &control_request(
                &job.id,
                paused.version,
                InstallationJobAction::Cancel,
                "cancel-paused-space",
            ),
            &fixture.store,
            42,
        )
        .unwrap();
    assert!(!fixture
        .repository
        .release_inactive_storage(&job.id, &fixture.store, 43)
        .unwrap());
    assert_eq!(
        fixture.repository.storage_reservation(&job.id).unwrap(),
        Some(held)
    );
    drop(lock);
    assert!(fixture
        .repository
        .release_inactive_storage(&job.id, &fixture.store, 44)
        .unwrap());
    assert!(!fixture
        .repository
        .release_inactive_storage(&job.id, &fixture.store, 45)
        .unwrap());
}

#[test]
fn failure_release_and_explicit_retry_reacquire_do_not_change_historical_control_receipts() {
    let fixture = Fixture::new(true);
    bind(&fixture);
    let mut wrong = fixture.bytes.clone();
    wrong[1000] ^= 1;
    std::fs::write(fixture.request.source_files.values().next().unwrap(), wrong).unwrap();
    fixture.run(40).unwrap_err();
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    let released = fixture
        .repository
        .storage_reservation(&job.id)
        .unwrap()
        .unwrap();
    assert_eq!(released.state, StorageReservationState::Released);
    let retry = control_request(
        &job.id,
        job.version,
        InstallationJobAction::Retry,
        "retry-space",
    );
    let receipt = fixture
        .repository
        .control_installation(&retry, &fixture.store, 41)
        .unwrap();
    let held = fixture
        .repository
        .storage_reservation(&job.id)
        .unwrap()
        .unwrap();
    assert_eq!(held.state, StorageReservationState::Held);
    assert!(held.version > released.version);
    let replay = fixture
        .repository
        .control_installation(&retry, &fixture.store, 42)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.job, receipt.job);
    assert_eq!(
        fixture.repository.storage_reservation(&job.id).unwrap(),
        Some(held)
    );
    std::fs::write(
        fixture.request.source_files.values().next().unwrap(),
        &fixture.bytes,
    )
    .unwrap();
    fixture.run(43).unwrap();
    assert_eq!(
        fixture
            .repository
            .storage_reservation(&job.id)
            .unwrap()
            .unwrap()
            .state,
        StorageReservationState::Released
    );
}

#[test]
fn failed_reservation_insert_rolls_back_job_input_file_and_initial_events() {
    let fixture = Fixture::new(true);
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute_batch("CREATE TRIGGER reject_storage BEFORE INSERT ON installation_storage_events BEGIN SELECT RAISE(ABORT,'injected storage failure'); END;").unwrap();
    let (request, input) = request(&fixture, "atomic-storage");
    assert!(fixture
        .repository
        .enqueue_installation(&request, &input, 40, 1 << 30)
        .is_err());
    assert!(fixture.repository.read_job(&request.id).unwrap().is_none());
    assert!(fixture
        .repository
        .file_transfers(&request.id)
        .unwrap()
        .is_empty());
    assert!(fixture
        .repository
        .storage_reservation(&request.id)
        .unwrap()
        .is_none());
    assert_eq!(
        raw.query_row::<u64, _, _>(
            "SELECT count(*) FROM installation_inputs WHERE job_id=?1",
            [&request.id],
            |row| row.get(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        raw.query_row::<u64, _, _>(
            "SELECT count(*) FROM job_events WHERE job_id=?1",
            [&request.id],
            |row| row.get(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn publication_retry_reuses_a_blob_persisted_before_its_budget_event() {
    let fixture = Fixture::new(true);
    bind(&fixture);
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute_batch(
        "CREATE TRIGGER reject_blob_budget BEFORE INSERT ON installation_storage_events
         WHEN json_extract(NEW.payload,'$.state') = 'held'
          AND json_extract(NEW.payload,'$.rules_version') = 'installation-storage-v2'
          AND EXISTS (SELECT 1 FROM installation_storage_events
                      WHERE job_id=NEW.job_id AND version=NEW.version-1
                        AND json_extract(payload,'$.rules_version')='installation-storage-v2')
         BEGIN SELECT RAISE(ABORT,'injected publication budget failure'); END;",
    )
    .unwrap();
    let error = fixture.run(40).unwrap_err();
    assert!(format!("{error:#}").contains("injected publication budget failure"));
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    let journal = fixture
        .repository
        .installation_commit(&job.id)
        .unwrap()
        .unwrap();
    assert_eq!(journal.stage, InstallationCommitStage::Staging);
    let manifest_bytes = serde_json::to_vec(&journal.manifest).unwrap().len() as u64;
    let weight_bytes = fixture.bytes.len() as u64;
    let blob = InstallationStore::blob_path(&Sha256Digest::of_bytes(&fixture.bytes)).unwrap();
    assert_eq!(
        std::fs::read(fixture.store.root().join(blob.as_str())).unwrap(),
        fixture.bytes
    );
    raw.execute_batch("DROP TRIGGER reject_blob_budget;")
        .unwrap();
    fixture
        .repository
        .control_installation(
            &control_request(
                &job.id,
                job.version,
                InstallationJobAction::Retry,
                "retry-publication-budget",
            ),
            &fixture.store,
            41,
        )
        .unwrap();
    fixture.run(42).unwrap();

    let mut events = raw
        .prepare(
            "SELECT reserved_bytes FROM (
            SELECT version, json_extract(payload,'$.reserved_bytes') AS reserved_bytes
            FROM installation_storage_events
            WHERE job_id=?1 AND json_extract(payload,'$.rules_version')='installation-storage-v2'
                            AND json_extract(payload,'$.state')='held'
         ) ORDER BY version",
        )
        .unwrap();
    let reserved = events
        .query_map([&job.id], |row| row.get::<_, u64>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        reserved,
        [
            2 * weight_bytes + manifest_bytes,
            weight_bytes + manifest_bytes,
            manifest_bytes,
            0,
        ]
    );
    assert_eq!(fixture.repository.active_installations().unwrap().len(), 1);
}

#[test]
fn transfer_credit_and_file_checkpoint_rollback_together() {
    let fixture = Fixture::new(true);
    bind(&fixture);
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute_batch(
        "CREATE TRIGGER reject_downloaded_event BEFORE INSERT ON job_file_events
         WHEN json_extract(NEW.payload,'$.state.state')='downloaded'
         BEGIN SELECT RAISE(ABORT,'injected file event failure'); END;",
    )
    .unwrap();
    let error = fixture.run(40).unwrap_err();
    assert!(format!("{error:#}").contains("injected file event failure"));
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture.repository.file_transfers(&job.id).unwrap()[0]
            .checkpoint
            .completed_bytes,
        0
    );
    assert_eq!(
        std::fs::metadata(fixture.transfer_path()).unwrap().len(),
        fixture.bytes.len() as u64
    );
    let credits: u64 = raw
        .query_row(
            "SELECT COUNT(*) FROM installation_transfer_credits",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(credits, 0);
    let events: u64 = raw
        .query_row(
            "SELECT COUNT(*) FROM installation_storage_events
         WHERE json_extract(payload,'$.rules_version')='installation-storage-v3'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 0);
    raw.execute_batch("DROP TRIGGER reject_downloaded_event;")
        .unwrap();
    fixture
        .repository
        .control_installation(
            &control_request(
                &job.id,
                job.version,
                InstallationJobAction::Retry,
                "retry-transfer-credit",
            ),
            &fixture.store,
            41,
        )
        .unwrap();
    fixture.run(42).unwrap();
    let (peak, reserved): (u64, u64) = raw
        .query_row(
            "SELECT json_extract(payload,'$.peak_bytes'), json_extract(payload,'$.reserved_bytes')
         FROM installation_storage_events
         WHERE json_extract(payload,'$.rules_version')='installation-storage-v3'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(peak - reserved, fixture.bytes.len() as u64);
    assert_eq!(
        raw.query_row(
            "SELECT COUNT(*) FROM installation_transfer_credits",
            [],
            |row| row.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn retry_admission_credits_only_retained_transfer_files() {
    for truncate in [false, true] {
        let fixture = Fixture::new(true);
        let original = bind(&fixture);
        let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
        raw.execute_batch(
            "CREATE TRIGGER reject_verification BEFORE INSERT ON job_events
             WHEN json_extract(NEW.payload,'$.stage') = 'verifying'
             BEGIN SELECT RAISE(ABORT,'injected verification transition failure'); END;",
        )
        .unwrap();
        let error = fixture.run(40).unwrap_err();
        assert!(format!("{error:#}").contains("injected verification transition failure"));
        let job = fixture
            .repository
            .read_job(&fixture.request.job_id)
            .unwrap()
            .unwrap();
        let released = fixture
            .repository
            .storage_reservation(&job.id)
            .unwrap()
            .unwrap();
        assert_eq!(released.state, StorageReservationState::Released);
        assert_eq!(released.reserved_bytes, 0);
        let credited: u64 = raw
            .query_row(
                "SELECT bytes FROM installation_transfer_credits WHERE job_id=?1",
                [&job.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(credited, fixture.bytes.len() as u64);
        raw.execute_batch("DROP TRIGGER reject_verification;")
            .unwrap();
        if truncate {
            std::fs::OpenOptions::new()
                .write(true)
                .open(fixture.transfer_path())
                .unwrap()
                .set_len(0)
                .unwrap();
        }
        let request = control_request(
            &job.id,
            job.version,
            InstallationJobAction::Retry,
            "readmit-retained-prefix",
        );
        fixture
            .repository
            .control_installation(&request, &fixture.store, 41)
            .unwrap();
        let held = fixture
            .repository
            .storage_reservation(&job.id)
            .unwrap()
            .unwrap();
        assert_eq!(held.state, StorageReservationState::Held);
        assert_eq!(
            held.reserved_bytes,
            original.peak_bytes - if truncate { 0 } else { credited }
        );
        assert!(
            fixture
                .repository
                .control_installation(&request, &fixture.store, 42)
                .unwrap()
                .replayed
        );
        assert_eq!(
            fixture.repository.storage_reservation(&job.id).unwrap(),
            Some(held)
        );
        if !truncate {
            fixture.run(43).unwrap();
        }
    }
}

#[test]
fn storage_release_failure_rolls_back_registration_and_recovers_without_duplicate_versions() {
    let fixture = Fixture::new(true);
    bind(&fixture);
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute_batch("CREATE TRIGGER reject_release BEFORE INSERT ON installation_storage_events WHEN json_extract(NEW.payload,'$.state')='released' BEGIN SELECT RAISE(ABORT,'injected release failure'); END;").unwrap();
    assert!(fixture.run(40).is_err());
    assert!(fixture
        .repository
        .active_installations()
        .unwrap()
        .is_empty());
    assert_eq!(
        fixture
            .repository
            .storage_reservation(&fixture.request.job_id)
            .unwrap()
            .unwrap()
            .state,
        StorageReservationState::Held
    );
    raw.execute_batch("DROP TRIGGER reject_release;").unwrap();
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    fixture
        .repository
        .control_installation(
            &control_request(
                &job.id,
                job.version,
                InstallationJobAction::Retry,
                "retry-register-space",
            ),
            &fixture.store,
            41,
        )
        .unwrap();
    fixture.run(42).unwrap();
    assert_eq!(fixture.repository.active_installations().unwrap().len(), 1);
    assert_eq!(
        fixture
            .repository
            .installation_versions(&fixture.request.installation_id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .repository
            .storage_reservation(&job.id)
            .unwrap()
            .unwrap()
            .reserved_bytes,
        0
    );
}

#[test]
fn storage_corruption_cannot_hide_a_reservation_from_another_request() {
    let fixture = Fixture::new(true);
    bind(&fixture);
    let raw = rusqlite::Connection::open(fixture.temp.path().join("models.db")).unwrap();
    raw.execute(
        "UPDATE installation_storage_reservations SET reserved_bytes=0,volume_id='forged-volume'",
        [],
    )
    .unwrap();
    assert!(fixture
        .repository
        .storage_reservation(&fixture.request.job_id)
        .is_err());
    let (request, input) = request(&fixture, "corruption-guard");
    assert!(fixture
        .repository
        .enqueue_installation(&request, &input, 40, 1 << 30)
        .is_err());
    assert!(fixture.repository.read_job(&request.id).unwrap().is_none());
}

#[test]
fn schema_ten_upgrade_keeps_jobs_and_inputs_and_marks_storage_for_admission_without_filesystem_access(
) {
    let fixture = Fixture::new(true);
    bind(&fixture);
    let job = fixture
        .repository
        .read_job(&fixture.request.job_id)
        .unwrap()
        .unwrap();
    let intent = fixture
        .repository
        .read_installation_input(&job.id)
        .unwrap()
        .unwrap();
    let path = fixture.temp.path().join("models.db");
    drop(fixture.repository);
    let root = fixture.store.root().to_path_buf();
    std::fs::remove_dir(&root).unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("DROP TABLE installation_blob_admission; DROP TABLE installation_transfer_credits; DROP TABLE installation_storage_reservations; DROP TABLE installation_storage_events; PRAGMA user_version=10;").unwrap();
    let repository = LifecycleRepository::open(&path).unwrap();
    assert_eq!(repository.read_job(&job.id).unwrap(), Some(job.clone()));
    assert_eq!(
        repository.read_installation_input(&job.id).unwrap(),
        Some(intent)
    );
    let storage = repository.storage_reservation(&job.id).unwrap().unwrap();
    assert_eq!(storage.state, StorageReservationState::NeedsAdmission);
    assert!(storage.volume_id.is_none());
    assert!(!root.exists());
    assert!(repository
        .claim(&job.id, job.version, "unadmitted", 40, 120)
        .is_err());
    let store = InstallationStore::open(&root).unwrap();
    repository
        .admit_installation_storage(&job, &store, 41)
        .unwrap();
    repository
        .claim(&job.id, job.version, "admitted", 42, 120)
        .unwrap();
}

#[test]
fn storage_upgrade_table_collision_rolls_back_and_preserves_the_old_schema() {
    let fixture = Fixture::new(true);
    let path = fixture.temp.path().join("models.db");
    drop(fixture.repository);
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("DROP TABLE installation_blob_admission; DROP TABLE installation_transfer_credits; DROP TABLE installation_storage_reservations; DROP TABLE installation_storage_events; CREATE TABLE installation_storage_reservations (legacy INTEGER); INSERT INTO installation_storage_reservations VALUES (7); PRAGMA user_version=10;").unwrap();
    assert!(LifecycleRepository::open(&path).is_err());
    assert_eq!(
        raw.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        10
    );
    assert_eq!(
        raw.query_row::<u32, _, _>(
            "SELECT legacy FROM installation_storage_reservations",
            [],
            |row| row.get(0)
        )
        .unwrap(),
        7
    );
    assert_eq!(
        raw.query_row::<u32, _, _>(
            "SELECT count(*) FROM sqlite_master WHERE name='installation_storage_events'",
            [],
            |row| row.get(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn bounded_reconciliation_visits_later_jobs_even_when_the_first_writer_is_busy() {
    let fixture = Fixture::new(true);
    for id in ["a-busy", "b-ready"] {
        let (request, input) = request(&fixture, id);
        let job = fixture
            .repository
            .enqueue_installation(&request, &input, 40, 1 << 30)
            .unwrap();
        fixture
            .repository
            .control_installation(
                &control_request(id, job.version, InstallationJobAction::Cancel, id),
                &fixture.store,
                41,
            )
            .unwrap();
    }
    let path = fixture.store.root().join("locks/transfers").join(format!(
        "{}.lock",
        Sha256Digest::of_bytes(b"a-busy").as_str()
    ));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = std::fs::File::create(path).unwrap();
    lock.try_lock_exclusive().unwrap();
    let first = fixture
        .repository
        .reconcile_installation_storage(&fixture.store, 42, None, 1)
        .unwrap();
    assert_eq!(first.released, 0);
    assert_eq!(first.next_after_id.as_deref(), Some("a-busy"));
    let next = fixture
        .repository
        .reconcile_installation_storage(&fixture.store, 42, first.next_after_id.as_deref(), 1)
        .unwrap();
    assert_eq!(next.released, 1);
    assert_eq!(
        fixture
            .repository
            .storage_reservation("b-ready")
            .unwrap()
            .unwrap()
            .reserved_bytes,
        0
    );
    drop(lock);
}

#[test]
fn query_exposes_storage_state_without_internal_store_paths() {
    let fixture = Fixture::with_layout(true, true);
    let held = bind(&fixture);
    service_runtime().block_on(async {
        let app =
            InstallationApplication::new(&fixture.temp.path().join("data"), Arc::new(|| Ok(None)));
        let response = app.list_request(Default::default()).await.unwrap();
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["storageReservations"][0]["state"], "held");
        assert_eq!(
            value["storageReservations"][0]["reservedBytes"],
            held.peak_bytes
        );
        assert!(value["storageReservations"][0].get("storeRoot").is_none());
        assert_eq!(app.state(), InstallationApplicationState::Dormant);
    });
}
