use super::*;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};

use crate::media::{
    BeginMediaPublish, MediaAssociation, MediaKind, MediaOutputRoot, MediaResultRef,
    MediaResultStore, MediaSource, PublishMediaRequest, MAX_MEDIA_READ_BYTES,
};

struct Fixture {
    store: MediaResultStore,
    pool: Pool<Sqlite>,
    root: MediaOutputRoot,
    _temp: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let base = PathBuf::from("E:/Codex/temp/lumina-media-access-tests");
        std::fs::create_dir_all(&base).unwrap();
        let temp = tempfile::Builder::new()
            .prefix("access-")
            .tempdir_in(std::fs::canonicalize(base).unwrap())
            .unwrap();
        let options = SqliteConnectOptions::new()
            .filename(temp.path().join("session.db"))
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let store = MediaResultStore::new(pool.clone());
        store.initialize().await.unwrap();
        let root = store
            .register_output_root(&temp.path().join("outputs"))
            .await
            .unwrap();
        Self {
            store,
            pool,
            root,
            _temp: temp,
        }
    }

    async fn publish(&self, session_id: &str) -> MediaResultRef {
        let request = PublishMediaRequest {
            result_id: Uuid::new_v4().to_string(),
            session_id: session_id.to_owned(),
            job_id: "job:access".into(),
            root_id: self.root.root_id.clone(),
            kind: MediaKind::Audio,
            association: MediaAssociation::ToolCall {
                tool_call_id: "call-access".into(),
            },
            source: MediaSource::Mcp {
                server_id: "access-fixture".into(),
                tool_name: "audio".into(),
                tool_call_id: "call-access".into(),
                input_refs: vec![],
            },
        };
        let BeginMediaPublish::Staging(mut staged) =
            self.store.begin_publish(request).await.unwrap()
        else {
            panic!("Expected a new publication");
        };
        staged.write_all(&wav()).unwrap();
        self.store.publish(staged).await.unwrap()
    }

    async fn content(&self, result: &MediaResultRef) -> VerifiedMediaContent {
        self.store
            .open_content(&result.session_id, &result.result_id)
            .await
            .unwrap()
    }

    async fn finish(self) {
        self.pool.close().await;
    }
}

fn wav() -> Vec<u8> {
    let data: Vec<_> = (0..8000_u16).flat_map(u16::to_le_bytes).collect();
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8000_u32.to_le_bytes());
    bytes.extend_from_slice(&16000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend(data);
    bytes
}

fn request<'a>(
    ticket: &'a MediaAccessTicket,
    result: &'a MediaResultRef,
    purpose: MediaAccessPurpose,
) -> MediaAccessRequest<'a> {
    MediaAccessRequest {
        ticket: &ticket.ticket,
        session_id: &result.session_id,
        result_id: &result.result_id,
        purpose,
    }
}

fn denied<T>(result: Result<T>) {
    let error = match result {
        Ok(_) => panic!("Expected media access rejection"),
        Err(error) => error,
    };
    assert_eq!(
        error.downcast_ref::<MediaTicketError>(),
        Some(&MediaTicketError)
    );
    assert_eq!(error.to_string(), "Media access is unavailable");
}

#[tokio::test]
async fn tickets_bind_verified_content_session_result_and_purpose() {
    let fixture = Fixture::new().await;
    let result = fixture.publish("session:first").await;
    let store = MediaAccessStore::new(8, Duration::from_secs(30)).unwrap();
    let owner = store.create_owner();
    denied(owner.issue(
        fixture.content(&result).await,
        "session:other",
        &result.result_id,
        MediaAccessPurpose::Preview,
    ));
    denied(owner.issue(
        fixture.content(&result).await,
        &result.session_id,
        &Uuid::new_v4().to_string(),
        MediaAccessPurpose::Preview,
    ));
    let ticket = owner
        .issue(
            fixture.content(&result).await,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::Preview,
        )
        .unwrap();
    assert_eq!(
        Uuid::parse_str(&ticket.ticket).unwrap().get_version_num(),
        4
    );
    assert!(!format!("{ticket:?}").contains(&ticket.ticket));
    assert_eq!(ticket.expires_in_ms, 30_000);
    let mut access = request(&ticket, &result, MediaAccessPurpose::Preview);
    assert_eq!(store.read_range(&access, 0, 8).unwrap(), &wav()[..8]);
    access.session_id = "session:other";
    denied(store.read_range(&access, 0, 8));
    access.session_id = &result.session_id;
    access.result_id = "other-result";
    denied(store.read_range(&access, 0, 8));
    access.result_id = &result.result_id;
    access.purpose = MediaAccessPurpose::SaveAs;
    denied(store.read_range(&access, 0, 8));
    access.purpose = MediaAccessPurpose::Preview;
    access.ticket = "bad-ticket";
    denied(store.read_range(&access, 0, 8));
    let unknown = Uuid::new_v4().to_string();
    access.ticket = &unknown;
    denied(store.read_range(&access, 0, 8));
    access.ticket = &ticket.ticket;
    denied(store.with_local_path::<()>(&access, |_| panic!("Preview cannot expose a path")));
    assert!(store
        .read_range(&access, 0, MAX_MEDIA_READ_BYTES + 1)
        .is_err());
    assert!(store.read_range(&access, result.bytes, 1).is_err());
    assert_eq!(store.read_range(&access, 47, 13).unwrap(), &wav()[47..60]);
    drop(owner);
    fixture.finish().await;
}

#[tokio::test]
async fn owner_and_session_revocation_are_isolated_and_release_is_idempotent() {
    let fixture = Fixture::new().await;
    let first = fixture.publish("session:first").await;
    let second = fixture.publish("session:second").await;
    let store = MediaAccessStore::new(8, Duration::from_secs(30)).unwrap();
    let owner = store.create_owner();
    let other = store.create_owner();
    let mut tickets = Vec::new();
    for (scope, result) in [(&owner, &first), (&owner, &second), (&other, &first)] {
        tickets.push(
            scope
                .issue(
                    fixture.content(result).await,
                    &result.session_id,
                    &result.result_id,
                    MediaAccessPurpose::Preview,
                )
                .unwrap(),
        );
    }
    other.release(&tickets[0].ticket);
    let first_access = request(&tickets[0], &first, MediaAccessPurpose::Preview);
    assert!(store.read_range(&first_access, 0, 4).is_ok());
    owner.revoke_session(&first.session_id);
    denied(store.read_range(&first_access, 0, 4));
    let second_access = request(&tickets[1], &second, MediaAccessPurpose::Preview);
    let other_access = request(&tickets[2], &first, MediaAccessPurpose::Preview);
    assert!(store.read_range(&second_access, 0, 4).is_ok());
    assert!(store.read_range(&other_access, 0, 4).is_ok());
    owner.release(&tickets[1].ticket);
    owner.release(&tickets[1].ticket);
    owner.release("unknown");
    denied(store.read_range(&second_access, 0, 4));
    drop(owner);
    assert!(store.read_range(&other_access, 0, 4).is_ok());
    drop(other);
    denied(store.read_range(&other_access, 0, 4));
    fixture.finish().await;
}

#[tokio::test]
async fn expiry_uses_monotonic_time_and_reclaims_capacity_and_file_guards() {
    let fixture = Fixture::new().await;
    let result = fixture.publish("session:expiry").await;
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock = Arc::clone(&now);
    let store =
        MediaAccessStore::with_clock(1, Duration::from_millis(25), move || *clock.lock().unwrap())
            .unwrap();
    let owner = store.create_owner();
    let content = fixture.content(&result).await;
    let path = content.path().to_owned();
    let ticket = owner
        .issue(
            content,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::Preview,
        )
        .unwrap();
    let access = request(&ticket, &result, MediaAccessPurpose::Preview);
    assert!(OpenOptions::new().write(true).open(&path).is_err());
    *now.lock().unwrap() += Duration::from_millis(24);
    assert!(store.read_range(&access, 0, 4).is_ok());
    *now.lock().unwrap() += Duration::from_millis(1);
    denied(store.read_range(&access, 0, 4));
    assert!(OpenOptions::new().write(true).open(&path).is_ok());
    let replacement = owner
        .issue(
            fixture.content(&result).await,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::Preview,
        )
        .unwrap();
    assert_ne!(ticket.ticket, replacement.ticket);
    *now.lock().unwrap() += Duration::from_millis(25);
    store.reap_expired();
    assert!(OpenOptions::new().write(true).open(&path).is_ok());
    denied(store.read_range(
        &request(&replacement, &result, MediaAccessPurpose::Preview),
        0,
        4,
    ));
    drop(owner);
    fixture.finish().await;
}

#[tokio::test]
async fn capacity_is_bounded_without_evicting_live_tickets() {
    for (capacity, lifetime) in [
        (0, Duration::from_secs(1)),
        (MAX_MEDIA_ACCESS_TICKETS + 1, Duration::from_secs(1)),
        (1, Duration::ZERO),
        (1, MAX_MEDIA_ACCESS_LIFETIME + Duration::from_secs(1)),
    ] {
        assert!(MediaAccessStore::new(capacity, lifetime).is_err());
    }
    let fixture = Fixture::new().await;
    let first = fixture.publish("session:capacity").await;
    let second = fixture.publish("session:capacity").await;
    let store = MediaAccessStore::new(1, Duration::from_secs(30)).unwrap();
    let owner = store.create_owner();
    let ticket = owner
        .issue(
            fixture.content(&first).await,
            &first.session_id,
            &first.result_id,
            MediaAccessPurpose::Preview,
        )
        .unwrap();
    let extra = fixture.content(&second).await;
    let extra_path = extra.path().to_owned();
    let error = owner
        .issue(
            extra,
            &second.session_id,
            &second.result_id,
            MediaAccessPurpose::Preview,
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "Media access ticket capacity reached");
    assert!(OpenOptions::new().write(true).open(extra_path).is_ok());
    assert!(store
        .read_range(&request(&ticket, &first, MediaAccessPurpose::Preview), 0, 4)
        .is_ok());
    owner.release(&ticket.ticket);
    let replacement = owner
        .issue(
            fixture.content(&second).await,
            &second.session_id,
            &second.result_id,
            MediaAccessPurpose::SaveAs,
        )
        .unwrap();
    assert!(store
        .read_range(
            &request(&replacement, &second, MediaAccessPurpose::SaveAs),
            0,
            4
        )
        .is_ok());
    drop(owner);
    fixture.finish().await;
}

#[tokio::test]
async fn path_actions_keep_verified_guards_and_cannot_be_used_for_reads() {
    let fixture = Fixture::new().await;
    let result = fixture.publish("session:actions").await;
    let store = MediaAccessStore::new(4, Duration::from_secs(30)).unwrap();
    let owner = store.create_owner();
    for purpose in [
        MediaAccessPurpose::Open,
        MediaAccessPurpose::Reveal,
        MediaAccessPurpose::CopyPath,
    ] {
        let content = fixture.content(&result).await;
        let path = content.path().to_owned();
        let ticket = owner
            .issue(content, &result.session_id, &result.result_id, purpose)
            .unwrap();
        let access = request(&ticket, &result, purpose);
        denied(store.read_range(&access, 0, 4));
        store
            .with_local_path(&access, |verified_path| {
                assert_eq!(verified_path, path);
                assert!(OpenOptions::new().write(true).open(verified_path).is_err());
                assert!(
                    std::fs::rename(verified_path, verified_path.with_extension("replaced"))
                        .is_err()
                );
                assert!(std::fs::remove_file(verified_path).is_err());
                Ok(())
            })
            .unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        owner.release(&ticket.ticket);
        denied(
            store
                .with_local_path::<()>(&access, |_| panic!("Released ticket cannot run an action")),
        );
        assert!(OpenOptions::new().write(true).open(&path).is_ok());
    }
    drop(owner);
    fixture.finish().await;
}

#[tokio::test]
async fn owner_drop_does_not_wait_for_an_action_and_active_guards_still_count() {
    let fixture = Fixture::new().await;
    let result = fixture.publish("session:slow-action").await;
    let store = MediaAccessStore::new(1, Duration::from_secs(30)).unwrap();
    let owner = store.create_owner();
    let content = fixture.content(&result).await;
    let path = content.path().to_owned();
    let ticket = owner
        .issue(
            content,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::Open,
        )
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let worker_store = store.clone();
    let worker_ticket = ticket.ticket.clone();
    let worker_result = result.clone();
    let worker = std::thread::spawn(move || {
        worker_store.with_local_path(
            &MediaAccessRequest {
                ticket: &worker_ticket,
                session_id: &worker_result.session_id,
                result_id: &worker_result.result_id,
                purpose: MediaAccessPurpose::Open,
            },
            |path| {
                assert!(OpenOptions::new().write(true).open(path).is_err());
                entered_tx.send(()).unwrap();
                finish_rx.recv_timeout(Duration::from_secs(30)).unwrap();
                assert!(OpenOptions::new().write(true).open(path).is_err());
                Ok(())
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let dropper = std::thread::spawn(move || {
        drop(owner);
        dropped_tx.send(()).unwrap();
    });
    let dropped = dropped_rx.recv_timeout(Duration::from_secs(10));
    if dropped.is_err() {
        finish_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        dropper.join().unwrap();
        panic!("Owner Drop waited for an external action");
    }
    dropper.join().unwrap();
    denied(
        store.with_local_path::<()>(&request(&ticket, &result, MediaAccessPurpose::Open), |_| {
            panic!("Revoked ticket cannot start a new action")
        }),
    );
    assert!(OpenOptions::new().write(true).open(&path).is_err());
    let replacement_owner = store.create_owner();
    let error = replacement_owner
        .issue(
            fixture.content(&result).await,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::Preview,
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "Media access ticket capacity reached");
    finish_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert!(OpenOptions::new().write(true).open(&path).is_ok());
    let replacement = replacement_owner
        .issue(
            fixture.content(&result).await,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::Preview,
        )
        .unwrap();
    assert!(store
        .read_range(
            &request(&replacement, &result, MediaAccessPurpose::Preview),
            0,
            4
        )
        .is_ok());
    drop(replacement_owner);
    fixture.finish().await;
}

#[tokio::test]
async fn concurrent_ranges_share_the_verified_handle_without_cursor_corruption() {
    let fixture = Fixture::new().await;
    let result = fixture.publish("session:ranges").await;
    let store = MediaAccessStore::new(1, Duration::from_secs(30)).unwrap();
    let owner = store.create_owner();
    let ticket = owner
        .issue(
            fixture.content(&result).await,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::SaveAs,
        )
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(4));
    std::thread::scope(|scope| {
        for worker in 0..4_usize {
            let barrier = Arc::clone(&barrier);
            let store = &store;
            let ticket = &ticket;
            let result = &result;
            scope.spawn(move || {
                barrier.wait();
                let bytes = wav();
                let access = request(ticket, result, MediaAccessPurpose::SaveAs);
                for step in 0..32 {
                    let offset = 44 + worker * 151 + step * 67;
                    assert_eq!(
                        store.read_range(&access, offset as u64, 257).unwrap(),
                        bytes[offset..offset + 257]
                    );
                }
            });
        }
    });
    drop(owner);
    fixture.finish().await;
}

#[tokio::test]
async fn claimed_content_survives_disconnect_but_new_claims_and_issue_are_denied() {
    let fixture = Fixture::new().await;
    let result = fixture.publish("session:connection").await;
    let store = MediaAccessStore::new(1, Duration::from_secs(30)).unwrap();
    let owner = store.create_owner();
    let connection = owner.disconnect_guard();
    let content = fixture.content(&result).await;
    let path = content.path().to_owned();
    let ticket = owner
        .issue(
            content,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::Preview,
        )
        .unwrap();
    let access = request(&ticket, &result, MediaAccessPurpose::Preview);
    let first = store.claim_content(&access).unwrap();
    let second = store.claim_content(&access).unwrap();
    drop(connection);
    denied(store.claim_content(&access));
    denied(owner.issue(
        fixture.content(&result).await,
        &result.session_id,
        &result.result_id,
        MediaAccessPurpose::Preview,
    ));
    assert_eq!(first.read_range(0, 44).unwrap(), wav()[..44]);
    drop(first);
    assert!(OpenOptions::new().write(true).open(&path).is_err());
    assert_eq!(second.read_range(44, 20).unwrap(), wav()[44..64]);
    drop(second);
    assert!(OpenOptions::new().write(true).open(&path).is_ok());
    fixture.finish().await;
}

#[tokio::test]
async fn local_action_claim_is_one_shot_and_keeps_guard_until_lease_drop() {
    let fixture = Fixture::new().await;
    let result = fixture.publish("session:action-lease").await;
    let store = MediaAccessStore::new(1, Duration::from_secs(30)).unwrap();
    let owner = store.create_owner();
    let ticket = owner
        .issue(
            fixture.content(&result).await,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::Reveal,
        )
        .unwrap();
    let access = request(&ticket, &result, MediaAccessPurpose::Reveal);
    let lease = store.claim_local_action(&access).unwrap();
    let path = lease.path().to_owned();
    assert_eq!(lease.result(), &result);
    denied(store.claim_local_action(&access));
    owner.release(&ticket.ticket);
    store.close();
    drop(owner);
    assert!(OpenOptions::new().write(true).open(&path).is_err());
    assert!(std::fs::remove_file(&path).is_err());
    drop(lease);
    assert!(OpenOptions::new().write(true).open(&path).is_ok());
    denied(store.create_owner().issue(
        fixture.content(&result).await,
        &result.session_id,
        &result.result_id,
        MediaAccessPurpose::Preview,
    ));
    fixture.finish().await;
}

#[tokio::test]
async fn expiry_blocks_new_claims_without_interrupting_an_accepted_response() {
    let fixture = Fixture::new().await;
    let result = fixture.publish("session:stream-expiry").await;
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock = Arc::clone(&now);
    let store =
        MediaAccessStore::with_clock(1, Duration::from_millis(25), move || *clock.lock().unwrap())
            .unwrap();
    let owner = store.create_owner();
    let content = fixture.content(&result).await;
    let path = content.path().to_owned();
    let ticket = owner
        .issue(
            content,
            &result.session_id,
            &result.result_id,
            MediaAccessPurpose::SaveAs,
        )
        .unwrap();
    let access = request(&ticket, &result, MediaAccessPurpose::SaveAs);
    let lease = store.claim_content(&access).unwrap();
    *now.lock().unwrap() += Duration::from_millis(25);
    store.reap_expired();
    denied(store.claim_content(&access));
    assert_eq!(lease.read_range(44, 16).unwrap(), wav()[44..60]);
    assert!(OpenOptions::new().write(true).open(&path).is_err());
    drop(lease);
    assert!(OpenOptions::new().write(true).open(&path).is_ok());
    fixture.finish().await;
}
