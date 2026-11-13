use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::media::{
    MediaAccessPurpose, MediaAccessRequest, MediaAccessStore, MAX_MEDIA_READ_BYTES,
};

const TICKET_HEADER: &str = "x-lumina-media-ticket";
const LOCAL_ACTION_HEADER: &str = "x-lumina-media-local-action";

#[derive(Clone)]
struct MediaHttpState {
    store: MediaAccessStore,
}

pub(super) fn routes(store: MediaAccessStore, secret_key: String) -> Router {
    Router::new()
        .route(
            "/media/v1/content/{session_id}/{result_id}",
            get(content).head(content),
        )
        .route(
            "/media/v1/action-path/{session_id}/{result_id}",
            get(action_path),
        )
        .with_state(MediaHttpState { store })
        .layer(axum::middleware::from_fn_with_state(
            secret_key,
            authenticate,
        ))
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}

async fn authenticate(State(secret): State<String>, request: Request, next: Next) -> Response {
    let authorized = !secret.is_empty()
        && super::auth::token_matches(single_header(request.headers(), "x-secret-key"), &secret);
    let mut response = if authorized {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn purpose(query: Option<&str>) -> Option<MediaAccessPurpose> {
    let query = query.filter(|query| query.len() <= 128)?;
    let mut pairs = url::form_urlencoded::parse(query.as_bytes());
    let (key, value) = pairs.next()?;
    if key != "purpose" || pairs.next().is_some() {
        return None;
    }
    match value.as_ref() {
        "preview" => Some(MediaAccessPurpose::Preview),
        "save_as" => Some(MediaAccessPurpose::SaveAs),
        "open" => Some(MediaAccessPurpose::Open),
        "reveal" => Some(MediaAccessPurpose::Reveal),
        "copy_path" => Some(MediaAccessPurpose::CopyPath),
        _ => None,
    }
}

fn mime(value: &str) -> Option<&'static str> {
    match value {
        "image/png" => Some("image/png"),
        "image/jpeg" => Some("image/jpeg"),
        "audio/wav" => Some("audio/wav"),
        "video/x-msvideo" => Some("video/x-msvideo"),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Selection {
    start: u64,
    end: u64,
    partial: bool,
}

fn select_range(range: Option<&str>, bytes: u64) -> Option<Selection> {
    if bytes == 0 {
        return None;
    }
    let Some(range) = range else {
        return Some(Selection {
            start: 0,
            end: bytes - 1,
            partial: false,
        });
    };
    if range.len() > 128 {
        return None;
    }
    let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
    let number = |value: &str| -> Option<u64> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        value.parse().ok()
    };
    let (start, end) = if start.is_empty() {
        let suffix = number(end)?;
        if suffix == 0 {
            return None;
        }
        (bytes.saturating_sub(suffix), bytes - 1)
    } else {
        let start = number(start)?;
        let end = if end.is_empty() {
            bytes - 1
        } else {
            number(end)?.min(bytes - 1)
        };
        if start >= bytes || start > end {
            return None;
        }
        (start, end)
    };
    Some(Selection {
        start,
        end,
        partial: true,
    })
}

fn unsatisfied(bytes: u64) -> Response {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{bytes}"))
        .header(header::ACCEPT_RANGES, "bytes")
        .body(Body::empty())
        .expect("Numeric range headers are valid")
}

async fn content(
    State(state): State<MediaHttpState>,
    Path((session_id, result_id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let Some(purpose) = purpose(request.uri().query()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(ticket) = single_header(request.headers(), TICKET_HEADER) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let ticket = ticket.to_owned();
    let lease = match tokio::task::spawn_blocking(move || {
        state.store.claim_content(&MediaAccessRequest {
            ticket: &ticket,
            session_id: &session_id,
            result_id: &result_id,
            purpose,
        })
    })
    .await
    {
        Ok(Ok(lease)) => Arc::new(lease),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let Some(mime) = mime(&lease.result().mime) else {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    };
    let bytes = lease.result().bytes;
    let digest = lease.result().sha256.clone();
    let range = if request.headers().contains_key(header::RANGE) {
        match single_header(request.headers(), header::RANGE.as_str()) {
            Some(value) => Some(value),
            None => return unsatisfied(bytes),
        }
    } else {
        None
    };
    let Some(selection) = select_range(range, bytes) else {
        return unsatisfied(bytes);
    };
    let length = selection.end - selection.start + 1;
    let mut response = Response::builder()
        .status(if selection.partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, format!("\"{digest}\""));
    if selection.partial {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{bytes}", selection.start, selection.end),
        );
    }
    let body = if request.method() == Method::HEAD {
        Body::empty()
    } else {
        let stream = async_stream::stream! {
            let mut offset = selection.start;
            while offset <= selection.end {
                let count = (selection.end - offset + 1).min(MAX_MEDIA_READ_BYTES as u64) as usize;
                let current = Arc::clone(&lease);
                let read = tokio::task::spawn_blocking(move || current.read_range(offset, count)).await;
                match read {
                    Ok(Ok(chunk)) => {
                        offset += chunk.len() as u64;
                        yield Ok::<_, std::io::Error>(chunk);
                    }
                    _ => {
                        yield Err(std::io::Error::other("Media stream read failed"));
                        break;
                    }
                }
            }
        };
        Body::from_stream(stream)
    };
    response
        .body(body)
        .expect("Verified media metadata produces valid headers")
}

async fn action_path(
    State(state): State<MediaHttpState>,
    Path((session_id, result_id)): Path<(String, String)>,
    request: Request,
) -> Response {
    if request.method() != Method::GET
        || !request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .is_some_and(|peer| peer.0.ip().is_loopback())
        || single_header(request.headers(), LOCAL_ACTION_HEADER) != Some("1")
        || request.headers().contains_key(header::ORIGIN)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(purpose) = purpose(request.uri().query()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(ticket) = single_header(request.headers(), TICKET_HEADER) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let ticket = ticket.to_owned();
    let lease = match tokio::task::spawn_blocking(move || {
        state.store.claim_local_action(&MediaAccessRequest {
            ticket: &ticket,
            session_id: &session_id,
            result_id: &result_id,
            purpose,
        })
    })
    .await
    {
        Ok(Ok(lease)) => lease,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let Some(path) = lease.path().to_str() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if mime(&lease.result().mime).is_none() {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let mut line = serde_json::to_vec(&serde_json::json!({
        "path": path,
        "fileName": lease.result().file_name,
        "mime": lease.result().mime,
        "bytes": lease.result().bytes,
        "sha256": lease.result().sha256,
    }))
    .expect("Verified media metadata serializes");
    line.push(b'\n');
    let stream = async_stream::stream! {
        let _lease = lease;
        yield Ok::<_, std::io::Error>(line);
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            yield Ok(vec![b'\n']);
        }
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(stream))
        .expect("Static media action headers are valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    #[test]
    fn ranges_accept_one_bounded_closed_open_or_suffix_interval() {
        for (value, expected) in [
            (
                None,
                Some(Selection {
                    start: 0,
                    end: 99,
                    partial: false,
                }),
            ),
            (
                Some("bytes=10-20"),
                Some(Selection {
                    start: 10,
                    end: 20,
                    partial: true,
                }),
            ),
            (
                Some("bytes=90-999"),
                Some(Selection {
                    start: 90,
                    end: 99,
                    partial: true,
                }),
            ),
            (
                Some("bytes=90-"),
                Some(Selection {
                    start: 90,
                    end: 99,
                    partial: true,
                }),
            ),
            (
                Some("bytes=-10"),
                Some(Selection {
                    start: 90,
                    end: 99,
                    partial: true,
                }),
            ),
            (
                Some("bytes=-1000"),
                Some(Selection {
                    start: 0,
                    end: 99,
                    partial: true,
                }),
            ),
        ] {
            assert_eq!(select_range(value, 100), expected);
        }
        for invalid in [
            "bytes=100-",
            "bytes=20-10",
            "bytes=-0",
            "bytes=",
            "bytes=0-2,4-6",
            "bytes=+1-2",
            "bytes=0-18446744073709551616",
            "items=0-1",
            "bytes= 1-2",
        ] {
            assert_eq!(select_range(Some(invalid), 100), None);
        }
        assert_eq!(select_range(None, 0), None);
    }

    #[test]
    fn media_queries_accept_only_one_explicit_purpose() {
        assert_eq!(
            purpose(Some("purpose=preview")),
            Some(MediaAccessPurpose::Preview)
        );
        assert_eq!(
            purpose(Some("purpose=save_as")),
            Some(MediaAccessPurpose::SaveAs)
        );
        for invalid in [
            None,
            Some("purpose=preview&token=secret"),
            Some("purpose=preview&purpose=open"),
            Some("url=https%3A%2F%2Fexample.com"),
            Some("purpose=fetch"),
        ] {
            assert_eq!(purpose(invalid), None);
        }
        assert_eq!(mime("text/html"), None);
        assert_eq!(mime("image/svg+xml"), None);
    }

    #[tokio::test]
    async fn authentication_accepts_only_one_secret_header_and_never_query_credentials() {
        let store = MediaAccessStore::new(2, Duration::from_secs(30)).unwrap();
        let router = routes(store, "correct-secret".into());
        for request in [
            Request::builder()
                .uri("/media/v1/content/session/result?purpose=preview&token=correct-secret")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/media/v1/content/session/result?purpose=preview")
                .header("x-secret-key", "wrong-secret")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/media/v1/content/session/result?purpose=preview")
                .header("x-secret-key", "correct-secret")
                .header("x-secret-key", "correct-secret")
                .body(Body::empty())
                .unwrap(),
        ] {
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        }
        let request = Request::builder()
            .uri("/media/v1/content/session/result?purpose=preview")
            .header("x-secret-key", "correct-secret")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.oneshot(request).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }

    #[cfg(windows)]
    mod guarded {
        use super::*;
        use crate::media::{
            BeginMediaPublish, MediaAccessOwner, MediaAccessTicket, MediaAssociation, MediaKind,
            MediaResultRef, MediaResultStore, MediaSource, PublishMediaRequest,
        };
        use futures::StreamExt;
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use sqlx::{Pool, Sqlite};
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::path::PathBuf;

        struct Fixture {
            router: Router,
            access: MediaAccessStore,
            owner: MediaAccessOwner,
            store: MediaResultStore,
            pool: Pool<Sqlite>,
            result: MediaResultRef,
            path: PathBuf,
            bytes: Vec<u8>,
            _temp: tempfile::TempDir,
        }

        impl Fixture {
            async fn new() -> Self {
                let base = PathBuf::from("E:/Codex/temp/lumina-media-http-tests");
                std::fs::create_dir_all(&base).unwrap();
                let temp = tempfile::Builder::new()
                    .prefix("http-")
                    .tempdir_in(std::fs::canonicalize(base).unwrap())
                    .unwrap();
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(
                        SqliteConnectOptions::new()
                            .filename(temp.path().join("session.db"))
                            .create_if_missing(true)
                            .foreign_keys(true),
                    )
                    .await
                    .unwrap();
                let store = MediaResultStore::new(pool.clone());
                store.initialize().await.unwrap();
                let root = store
                    .register_output_root(&temp.path().join("outputs"))
                    .await
                    .unwrap();
                let request = PublishMediaRequest {
                    result_id: uuid::Uuid::new_v4().to_string(),
                    session_id: "session:http".into(),
                    job_id: "job:http".into(),
                    root_id: root.root_id,
                    kind: MediaKind::Audio,
                    association: MediaAssociation::ToolCall {
                        tool_call_id: "http-call".into(),
                    },
                    source: MediaSource::Mcp {
                        server_id: "fixture".into(),
                        tool_name: "audio".into(),
                        tool_call_id: "http-call".into(),
                        input_refs: vec![],
                    },
                };
                let mut bytes = b"RIFF".to_vec();
                bytes.extend_from_slice(&1_100_036_u32.to_le_bytes());
                bytes.extend_from_slice(b"WAVEfmt ");
                bytes.extend_from_slice(&16_u32.to_le_bytes());
                bytes.extend_from_slice(&1_u16.to_le_bytes());
                bytes.extend_from_slice(&1_u16.to_le_bytes());
                bytes.extend_from_slice(&8000_u32.to_le_bytes());
                bytes.extend_from_slice(&16000_u32.to_le_bytes());
                bytes.extend_from_slice(&2_u16.to_le_bytes());
                bytes.extend_from_slice(&16_u16.to_le_bytes());
                bytes.extend_from_slice(b"data");
                bytes.extend_from_slice(&1_100_000_u32.to_le_bytes());
                bytes.extend((0..550_000_u32).flat_map(|sample| (sample as i16).to_le_bytes()));
                let BeginMediaPublish::Staging(mut staged) =
                    store.begin_publish(request).await.unwrap()
                else {
                    panic!("Expected staged output")
                };
                staged.write_all(&bytes).unwrap();
                let result = store.publish(staged).await.unwrap();
                let content = store
                    .open_content(&result.session_id, &result.result_id)
                    .await
                    .unwrap();
                let path = content.path().to_owned();
                drop(content);
                let access = MediaAccessStore::new(8, Duration::from_secs(300)).unwrap();
                let owner = access.create_owner();
                let router = routes(access.clone(), "correct-secret".into());
                Self {
                    router,
                    access,
                    owner,
                    store,
                    pool,
                    result,
                    path,
                    bytes,
                    _temp: temp,
                }
            }

            async fn ticket(&self, purpose: MediaAccessPurpose) -> MediaAccessTicket {
                self.owner
                    .issue(
                        self.store
                            .open_content(&self.result.session_id, &self.result.result_id)
                            .await
                            .unwrap(),
                        &self.result.session_id,
                        &self.result.result_id,
                        purpose,
                    )
                    .unwrap()
            }

            fn request(&self, route: &str, purpose: &str, ticket: &MediaAccessTicket) -> Request {
                Request::builder()
                    .uri(format!(
                        "/media/v1/{route}/{}/{}?purpose={purpose}",
                        self.result.session_id, self.result.result_id
                    ))
                    .header("x-secret-key", "correct-secret")
                    .header(TICKET_HEADER, &ticket.ticket)
                    .body(Body::empty())
                    .unwrap()
            }

            async fn finish(self) {
                self.owner.close();
                self.access.close();
                self.pool.close().await;
            }
        }

        #[tokio::test]
        async fn content_routes_verify_scope_head_ranges_headers_and_stream_guard_lifetime() {
            let fixture = Fixture::new().await;
            let ticket = fixture.ticket(MediaAccessPurpose::Preview).await;
            let mut head = fixture.request("content", "preview", &ticket);
            *head.method_mut() = Method::HEAD;
            let response = fixture.router.clone().oneshot(head).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                fixture.result.bytes.to_string()
            );
            assert_eq!(
                response.headers()[header::ETAG],
                format!("\"{}\"", fixture.result.sha256)
            );
            assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/wav");
            assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
            assert!(axum::body::to_bytes(response.into_body(), 1)
                .await
                .unwrap()
                .is_empty());
            for (range, start, end) in [
                ("bytes=10-29", 10_usize, 29_usize),
                (
                    "bytes=-16",
                    fixture.bytes.len() - 16,
                    fixture.bytes.len() - 1,
                ),
            ] {
                let mut request = fixture.request("content", "preview", &ticket);
                request
                    .headers_mut()
                    .insert(header::RANGE, HeaderValue::from_static(range));
                let response = fixture.router.clone().oneshot(request).await.unwrap();
                assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
                assert_eq!(
                    response.headers()[header::CONTENT_RANGE],
                    format!("bytes {start}-{end}/{}", fixture.result.bytes)
                );
                assert_eq!(
                    axum::body::to_bytes(response.into_body(), MAX_MEDIA_READ_BYTES)
                        .await
                        .unwrap()
                        .as_ref(),
                    &fixture.bytes[start..=end]
                );
            }
            let mut invalid = fixture.request("content", "preview", &ticket);
            invalid
                .headers_mut()
                .insert(header::RANGE, HeaderValue::from_static("bytes=0-2,4-6"));
            let response = fixture.router.clone().oneshot(invalid).await.unwrap();
            assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
            assert_eq!(
                response.headers()[header::CONTENT_RANGE],
                format!("bytes */{}", fixture.result.bytes)
            );
            assert_eq!(
                fixture
                    .router
                    .clone()
                    .oneshot(fixture.request("content", "save_as", &ticket))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
            let mut foreign = fixture.request("content", "preview", &ticket);
            *foreign.uri_mut() = format!(
                "/media/v1/content/other-session/{}?purpose=preview",
                fixture.result.result_id
            )
            .parse()
            .unwrap();
            assert_eq!(
                fixture
                    .router
                    .clone()
                    .oneshot(foreign)
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );

            let response = fixture
                .router
                .clone()
                .oneshot(fixture.request("content", "preview", &ticket))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            fixture.owner.release(&ticket.ticket);
            assert_eq!(
                fixture
                    .router
                    .clone()
                    .oneshot(fixture.request("content", "preview", &ticket))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
            assert!(OpenOptions::new().write(true).open(&fixture.path).is_err());
            let mut stream = response.into_body().into_data_stream();
            let mut received = Vec::new();
            let mut chunks = 0;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.unwrap();
                assert!(chunk.len() <= MAX_MEDIA_READ_BYTES);
                received.extend_from_slice(&chunk);
                chunks += 1;
            }
            drop(stream);
            assert!(chunks >= 2);
            assert_eq!(received, fixture.bytes);
            assert!(OpenOptions::new().write(true).open(&fixture.path).is_ok());
            fixture.finish().await;
        }

        #[tokio::test]
        async fn cancelling_a_claimed_content_body_releases_its_guard() {
            let fixture = Fixture::new().await;
            let ticket = fixture.ticket(MediaAccessPurpose::SaveAs).await;
            let response = fixture
                .router
                .clone()
                .oneshot(fixture.request("content", "save_as", &ticket))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            fixture.owner.close();
            assert!(OpenOptions::new().write(true).open(&fixture.path).is_err());
            drop(response);
            assert!(OpenOptions::new().write(true).open(&fixture.path).is_ok());
            fixture.finish().await;
        }

        #[tokio::test]
        async fn local_action_stream_requires_local_peer_and_retains_guard_until_cancelled() {
            let fixture = Fixture::new().await;
            let ticket = fixture.ticket(MediaAccessPurpose::Open).await;
            for (peer, marker, origin) in [
                (None, true, false),
                (Some("192.0.2.4:1234"), true, false),
                (Some("127.0.0.1:1234"), false, false),
                (Some("127.0.0.1:1234"), true, true),
            ] {
                let mut request = fixture.request("action-path", "open", &ticket);
                if let Some(peer) = peer {
                    request
                        .extensions_mut()
                        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
                }
                if marker {
                    request
                        .headers_mut()
                        .insert(LOCAL_ACTION_HEADER, HeaderValue::from_static("1"));
                }
                if origin {
                    request
                        .headers_mut()
                        .insert(header::ORIGIN, HeaderValue::from_static("http://localhost"));
                }
                assert_eq!(
                    fixture
                        .router
                        .clone()
                        .oneshot(request)
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::FORBIDDEN
                );
            }
            let mut request = fixture.request("action-path", "open", &ticket);
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            request
                .headers_mut()
                .insert(LOCAL_ACTION_HEADER, HeaderValue::from_static("1"));
            let response = fixture.router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/x-ndjson"
            );
            assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
            let mut stream = response.into_body().into_data_stream();
            let first = stream.next().await.unwrap().unwrap();
            assert_eq!(first.last(), Some(&b'\n'));
            let metadata: serde_json::Value = serde_json::from_slice(&first).unwrap();
            assert_eq!(metadata["path"], fixture.path.to_str().unwrap());
            assert_eq!(metadata["bytes"], fixture.result.bytes);
            assert_eq!(metadata["sha256"], fixture.result.sha256);
            fixture.owner.close();
            fixture.access.close();
            assert!(OpenOptions::new().write(true).open(&fixture.path).is_err());
            drop(stream);
            assert!(OpenOptions::new().write(true).open(&fixture.path).is_ok());
            fixture.finish().await;
        }

        #[tokio::test]
        async fn loopback_http_preserves_auth_range_and_native_action_cancellation() {
            let fixture = Fixture::new().await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
            let router = fixture.router.clone();
            let mut server = tokio::spawn(async move {
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
            });
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let content_url = format!(
                "http://{address}/media/v1/content/{}/{}?purpose=save_as",
                fixture.result.session_id, fixture.result.result_id
            );
            assert_eq!(
                client
                    .get(format!("{content_url}&token=correct-secret"))
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16(),
                401
            );
            let ticket = fixture.ticket(MediaAccessPurpose::SaveAs).await;
            let response = client
                .get(&content_url)
                .header("x-secret-key", "correct-secret")
                .header(TICKET_HEADER, &ticket.ticket)
                .header("range", "bytes=0-43")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 206);
            assert_eq!(
                response.bytes().await.unwrap().as_ref(),
                &fixture.bytes[..44]
            );
            fixture.owner.release(&ticket.ticket);
            let ticket = fixture.ticket(MediaAccessPurpose::CopyPath).await;
            let mut response = client
                .get(format!(
                    "http://{address}/media/v1/action-path/{}/{}?purpose=copy_path",
                    fixture.result.session_id, fixture.result.result_id
                ))
                .header("x-secret-key", "correct-secret")
                .header(TICKET_HEADER, &ticket.ticket)
                .header(LOCAL_ACTION_HEADER, "1")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            let mut line = Vec::new();
            while !line.contains(&b'\n') {
                line.extend_from_slice(&response.chunk().await.unwrap().unwrap());
                assert!(line.len() < MAX_MEDIA_READ_BYTES);
            }
            let first_line = line.split(|byte| *byte == b'\n').next().unwrap();
            let metadata: serde_json::Value = serde_json::from_slice(first_line).unwrap();
            assert_eq!(metadata["sha256"], fixture.result.sha256);
            fixture.owner.close();
            assert!(OpenOptions::new().write(true).open(&fixture.path).is_err());
            drop(response);
            drop(client);
            shutdown_tx.send(()).unwrap();
            let stopped = tokio::time::timeout(Duration::from_secs(10), &mut server).await;
            if stopped.is_err() {
                server.abort();
                let _ = server.await;
                panic!("Native action cancellation did not close the HTTP response");
            }
            stopped.unwrap().unwrap();
            assert!(OpenOptions::new().write(true).open(&fixture.path).is_ok());
            fixture.finish().await;
        }
    }
}
