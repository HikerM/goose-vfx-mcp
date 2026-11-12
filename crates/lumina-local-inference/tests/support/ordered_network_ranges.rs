use super::*;
use futures::{channel::oneshot, TryStreamExt};
use lumina_download_manager::managed::{ManagedTransportError, ManagedTransportErrorKind};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::task::Poll;

struct RequestGuard {
    active: Rc<Cell<usize>>,
}

impl RequestGuard {
    fn new(active: &Rc<Cell<usize>>, peak: &Cell<usize>) -> Self {
        active.set(active.get() + 1);
        peak.set(peak.get().max(active.get()));
        Self {
            active: active.clone(),
        }
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.active.set(self.active.get() - 1);
    }
}

fn strong_validator() -> ResumeValidator {
    ResumeValidator::StrongEtag {
        value: "\"stable-content\"".into(),
    }
}

fn response(expected_bytes: u64, bytes: Vec<u8>) -> RangeResponse<'static> {
    RangeResponse {
        expected_bytes,
        validator: strong_validator(),
        chunks: stream::once(async move { Ok(bytes) }).boxed_local(),
    }
}

fn run<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn later_ranges_complete_concurrently_but_publish_in_order_and_drop_pending_requests() {
    run(async {
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for _ in 0..4 {
            let (sender, receiver) = oneshot::channel();
            senders.push(Some(sender));
            receivers.push(Some(receiver));
        }
        let receivers = Rc::new(RefCell::new(receivers));
        let requested = Rc::new(RefCell::new(Vec::new()));
        let received = Rc::new(RefCell::new(Vec::new()));
        let active = Rc::new(Cell::new(0));
        let peak = Rc::new(Cell::new(0));
        let request = {
            let receivers = receivers.clone();
            let requested = requested.clone();
            let received = received.clone();
            let active = active.clone();
            let peak = peak.clone();
            move |range: PinnedHttpRange, whole_file: bool| {
                assert!(!whole_file);
                let index = (range.start / RANGE_BYTES) as usize;
                requested.borrow_mut().push(index);
                let receiver = receivers.borrow_mut()[index].take().unwrap();
                let guard = RequestGuard::new(&active, &peak);
                let received = received.clone();
                async move {
                    Ok(RangeResponse {
                        expected_bytes: range.length,
                        validator: strong_validator(),
                        chunks: stream::once(async move {
                            let _guard = guard;
                            receiver.await?;
                            received.borrow_mut().push(index);
                            Ok(vec![index as u8; range.length as usize])
                        })
                        .boxed_local(),
                    })
                }
            }
        };
        let mut body = open_file_ranges(0, RANGE_BYTES * 4, None, request)
            .await
            .unwrap();
        assert_eq!(*requested.borrow(), [0]);
        senders[1].take().unwrap().send(()).unwrap();
        let mut first = body.chunks.next();
        assert!(matches!(futures::poll!(&mut first), Poll::Pending));
        assert_eq!(*requested.borrow(), [0, 1]);
        assert_eq!(*received.borrow(), [1]);
        assert_eq!(peak.get(), RANGE_CONCURRENCY);
        senders[0].take().unwrap().send(()).unwrap();
        assert_eq!(first.await.unwrap().unwrap(), vec![0; RANGE_BYTES as usize]);
        assert_eq!(
            body.chunks.next().await.unwrap().unwrap(),
            vec![1; RANGE_BYTES as usize]
        );
        senders[3].take().unwrap().send(()).unwrap();
        let mut third = body.chunks.next();
        assert!(matches!(futures::poll!(&mut third), Poll::Pending));
        assert_eq!(*requested.borrow(), [0, 1, 2, 3]);
        assert_eq!(*received.borrow(), [1, 0, 3]);
        assert_eq!(peak.get(), RANGE_CONCURRENCY);
        drop(third);
        drop(body);
        assert_eq!(active.get(), 0);
        assert!(senders[2].take().unwrap().send(()).is_err());
    });
}

#[test]
fn ignored_initial_range_reuses_the_full_response_without_an_extra_request() {
    run(async {
        let total = RANGE_BYTES * 2;
        let calls = Cell::new(0);
        let body = open_file_ranges(0, total, None, |range, whole_file| {
            calls.set(calls.get() + 1);
            assert!(!whole_file);
            assert_eq!(range.start, 0);
            assert_eq!(range.length, RANGE_BYTES);
            async move { Ok(response(total, vec![7; total as usize])) }
        })
        .await
        .unwrap();
        assert_eq!(body.expected_bytes, total);
        let chunks = body.chunks.try_collect::<Vec<_>>().await.unwrap();
        assert_eq!(chunks, [vec![7; total as usize]]);
        assert_eq!(calls.get(), 1);
    });
}

#[test]
fn limited_response_without_strong_etag_is_dropped_before_full_file_fallback() {
    run(async {
        let total = RANGE_BYTES * 2;
        let active = Rc::new(Cell::new(0));
        let peak = Cell::new(0);
        let calls = RefCell::new(Vec::new());
        let body = open_file_ranges(0, total, None, |range, whole_file| {
            calls
                .borrow_mut()
                .push((range.start, range.length, whole_file));
            assert_eq!(active.get(), 0);
            let guard = RequestGuard::new(&active, &peak);
            async move {
                Ok(RangeResponse {
                    expected_bytes: range.length,
                    validator: ResumeValidator::Unknown,
                    chunks: stream::once(async move {
                        let _guard = guard;
                        assert!(whole_file, "The discarded limited response was consumed");
                        Ok(vec![9; range.length as usize])
                    })
                    .boxed_local(),
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(body.validator, ResumeValidator::Unknown);
        assert_eq!(
            body.chunks.try_collect::<Vec<_>>().await.unwrap(),
            [vec![9; total as usize]]
        );
        assert_eq!(*calls.borrow(), [(0, RANGE_BYTES, false), (0, total, true)]);
        assert_eq!(active.get(), 0);
        assert_eq!(peak.get(), 1);
    });
}

#[test]
fn later_range_errors_preserve_the_cause_and_drop_other_requests_after_ordered_delivery() {
    for kind in [
        ManagedTransportErrorKind::ValidatorChanged,
        ManagedTransportErrorKind::ResumeUnavailable,
    ] {
        run(async {
            let (release_first, first_ready) = oneshot::channel::<()>();
            let first_ready = RefCell::new(Some(first_ready));
            let active = Rc::new(Cell::new(0));
            let peak = Cell::new(0);
            let requested = RefCell::new(Vec::new());
            let mut body = open_file_ranges(0, RANGE_BYTES * 4, None, |range, whole_file| {
                assert!(!whole_file);
                requested.borrow_mut().push(range.start);
                let first_ready = first_ready.borrow_mut().take();
                let guard = RequestGuard::new(&active, &peak);
                async move {
                    if range.start == RANGE_BYTES {
                        return Err(ManagedTransportError {
                            kind,
                            http_status: Some(200),
                            retry_after_seconds: None,
                        }
                        .into());
                    }
                    Ok(RangeResponse {
                        expected_bytes: range.length,
                        validator: strong_validator(),
                        chunks: stream::once(async move {
                            let _guard = guard;
                            if let Some(first_ready) = first_ready {
                                first_ready.await?;
                                Ok(vec![1; range.length as usize])
                            } else {
                                std::future::pending::<Result<Vec<u8>>>().await
                            }
                        })
                        .boxed_local(),
                    })
                }
            })
            .await
            .unwrap();
            let mut first = body.chunks.next();
            assert!(matches!(futures::poll!(&mut first), Poll::Pending));
            assert_eq!(*requested.borrow(), [0, RANGE_BYTES]);
            release_first.send(()).unwrap();
            assert_eq!(first.await.unwrap().unwrap().len(), RANGE_BYTES as usize);
            let error = body.chunks.next().await.unwrap().unwrap_err();
            assert_eq!(
                error.downcast_ref::<ManagedTransportError>().unwrap().kind,
                kind
            );
            drop(body);
            assert_eq!(active.get(), 0);
            assert!(peak.get() <= RANGE_CONCURRENCY);
        });
    }
}

#[test]
fn bounded_ranges_reject_wrong_declared_extents_truncation_and_excess_bytes() {
    for (declared, actual, message) in [
        (
            RANGE_BYTES - 1,
            RANGE_BYTES,
            "Range does not cover its pinned extent",
        ),
        (
            RANGE_BYTES,
            RANGE_BYTES - 1,
            "Range ended before its pinned extent",
        ),
        (
            RANGE_BYTES,
            RANGE_BYTES + 1,
            "Range exceeds its pinned extent",
        ),
    ] {
        run(async {
            let mut body = open_file_ranges(0, RANGE_BYTES * 2, None, move |_, _| async move {
                Ok(response(declared, vec![1; actual as usize]))
            })
            .await
            .unwrap();
            let error = body.chunks.next().await.unwrap().unwrap_err();
            assert_eq!(error.to_string(), message);
        });
    }
}

#[test]
fn resumed_ranges_keep_the_original_validator_offset_and_partial_final_extent() {
    run(async {
        let start = 17;
        let total = start + RANGE_BYTES + 3;
        let requests = RefCell::new(Vec::new());
        let body = open_file_ranges(
            start,
            total,
            Some("\"stable-content\"".into()),
            |range, whole_file| {
                assert!(!whole_file);
                assert_eq!(range.if_range.as_deref(), Some("\"stable-content\""));
                requests.borrow_mut().push((range.start, range.length));
                async move {
                    Ok(response(
                        range.length,
                        vec![range.start as u8; range.length as usize],
                    ))
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(body.expected_bytes, total - start);
        assert_eq!(body.validator, strong_validator());
        let chunks = body.chunks.try_collect::<Vec<_>>().await.unwrap();
        assert_eq!(
            *requests.borrow(),
            [(start, RANGE_BYTES), (start + RANGE_BYTES, 3)]
        );
        assert_eq!(chunks, [vec![17; RANGE_BYTES as usize], vec![17; 3]]);
    });
}
