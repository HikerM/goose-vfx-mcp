use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

struct Server {
    url: Url,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn new(response: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let stop = Arc::new(AtomicBool::new(false));
        let observed = requests.clone();
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 1024];
                while !bytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    assert!(bytes.len() < 65536);
                }
                observed
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(bytes).unwrap());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            url: Url::parse(&format!("http://{address}/repo")).unwrap(),
            requests,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect((self.url.host_str().unwrap(), self.url.port().unwrap()));
        self.thread.take().unwrap().join().unwrap();
    }
}

fn transport(server: &Server) -> ManagedModelScopeTransport {
    ManagedModelScopeTransport {
        client: Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        credential_origin: server.url.origin().ascii_serialization(),
        allowed_origins: vec![server.url.origin().ascii_serialization()],
    }
}

fn response(status: &str, headers: &str, body: &str) -> String {
    format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n{body}")
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn first() -> PinnedHttpRange {
    PinnedHttpRange {
        start: 0,
        length: 4,
        total: 8,
        if_range: None,
    }
}

fn resumed() -> PinnedHttpRange {
    PinnedHttpRange {
        start: 4,
        length: 4,
        total: 8,
        if_range: Some("\"stable\"".into()),
    }
}

async fn consume(mut body: ManagedRangeBody) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next_chunk().await? {
        bytes.extend(chunk);
    }
    Ok(bytes)
}

#[test]
fn range_resume_sends_if_range_and_validates_exact_bytes() {
    let server = Server::new(response(
        "206 Partial Content",
        "Content-Range: bytes 4-7/8\r\nContent-Length: 4\r\nETag: \"stable\"\r\n",
        "efgh",
    ));
    run(async {
        let body = transport(&server)
            .open_range(server.url.clone(), &resumed(), &HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(body.expected_bytes(), 4);
        assert_eq!(body.strong_etag(), Some("\"stable\""));
        assert_eq!(consume(body).await.unwrap(), b"efgh");
    });
    let request = server.requests.lock().unwrap()[0].to_lowercase();
    assert!(
        request.contains("range: bytes=4-7\r\n")
            && request.contains("if-range: \"stable\"\r\n")
            && request.contains("accept-encoding: identity\r\n")
    );
}

#[test]
fn full_response_is_explicit_whole_file_fallback_only_from_zero() {
    let server = Server::new(response(
        "200 OK",
        "Content-Length: 8\r\nETag: \"stable\"\r\n",
        "abcdefgh",
    ));
    run(async {
        let client = transport(&server);
        let body = client
            .open_range(server.url.clone(), &first(), &HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(body.expected_bytes(), 8);
        assert_eq!(consume(body).await.unwrap(), b"abcdefgh");
        assert_eq!(
            client
                .open_range(server.url.clone(), &resumed(), &HeaderMap::new())
                .await
                .err()
                .unwrap()
                .kind,
            ManagedTransportErrorKind::ResumeUnavailable
        );
    });
}

#[test]
fn changed_or_absent_resume_etag_requires_explicit_restart() {
    for etag in ["ETag: \"changed\"\r\n", "ETag: W/\"stable\"\r\n", ""] {
        let server = Server::new(response(
            "206 Partial Content",
            &format!("Content-Range: bytes 4-7/8\r\nContent-Length: 4\r\n{etag}"),
            "efgh",
        ));
        let error =
            run(transport(&server).open_range(server.url.clone(), &resumed(), &HeaderMap::new()))
                .err()
                .unwrap();
        assert_eq!(error.kind, ManagedTransportErrorKind::ValidatorChanged);
    }
}

#[test]
fn incorrect_content_range_size_encoding_and_status_cannot_be_downloaded_bytes() {
    for (status, headers, kind) in [
        (
            "206 Partial Content",
            "Content-Range: bytes 0-3/9\r\nContent-Length: 4\r\n",
            ManagedTransportErrorKind::RevisionChanged,
        ),
        (
            "206 Partial Content",
            "Content-Range: bytes 1-4/8\r\nContent-Length: 4\r\n",
            ManagedTransportErrorKind::Protocol,
        ),
        (
            "206 Partial Content",
            "Content-Length: 4\r\n",
            ManagedTransportErrorKind::Protocol,
        ),
        (
            "206 Partial Content",
            "Content-Range: bytes 0-3/8\r\nContent-Length: 3\r\n",
            ManagedTransportErrorKind::RevisionChanged,
        ),
        (
            "206 Partial Content",
            "Content-Range: bytes 0-3/8\r\nContent-Length: 4\r\nContent-Encoding: gzip\r\n",
            ManagedTransportErrorKind::Protocol,
        ),
        (
            "416 Range Not Satisfiable",
            "Content-Length: 0\r\n",
            ManagedTransportErrorKind::RevisionChanged,
        ),
    ] {
        let server = Server::new(response(status, headers, "abcd"));
        let error =
            run(transport(&server).open_range(server.url.clone(), &first(), &HeaderMap::new()))
                .err()
                .unwrap();
        assert_eq!(error.kind, kind);
    }
}

#[test]
fn truncated_and_overlong_streams_never_report_complete() {
    for body in ["abc", "abcde"] {
        let encoded = format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
        let server = Server::new(response(
            "206 Partial Content",
            "Content-Range: bytes 0-3/8\r\nTransfer-Encoding: chunked\r\n",
            &encoded,
        ));
        let result = run(async {
            let body = transport(&server)
                .open_range(server.url.clone(), &first(), &HeaderMap::new())
                .await?;
            consume(body).await
        });
        assert_eq!(
            result.unwrap_err().kind,
            ManagedTransportErrorKind::Protocol
        );
    }
}

#[test]
fn redirects_strip_credentials_at_cdn_origin_and_reject_unapproved_origin() {
    let cdn = Server::new(response(
        "206 Partial Content",
        "Content-Range: bytes 0-3/8\r\nContent-Length: 4\r\n",
        "abcd",
    ));
    let api = Server::new(response(
        "302 Found",
        &format!("Location: {}\r\nContent-Length: 0\r\n", cdn.url),
        "",
    ));
    let mut client = transport(&api);
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        "Bearer unit-credential".parse().unwrap(),
    );
    headers.insert(
        reqwest::header::COOKIE,
        "m_session_id=unit-credential".parse().unwrap(),
    );
    assert_eq!(
        run(client.open_range(api.url.clone(), &first(), &headers))
            .err()
            .unwrap()
            .kind,
        ManagedTransportErrorKind::Forbidden
    );
    assert!(cdn.requests.lock().unwrap().is_empty());
    client
        .allowed_origins
        .push(cdn.url.origin().ascii_serialization());
    assert_eq!(
        run(async {
            consume(
                client
                    .open_range(api.url.clone(), &first(), &headers)
                    .await?,
            )
            .await
        })
        .unwrap(),
        b"abcd"
    );
    assert!(api.requests.lock().unwrap()[0].contains("unit-credential"));
    let captured = cdn.requests.lock().unwrap()[0].to_lowercase();
    assert!(
        !captured.contains("authorization:")
            && !captured.contains("cookie:")
            && !captured.contains("unit-credential")
    );
}

#[test]
fn production_policy_rejects_non_modelscope_origins_before_sending_credentials() {
    let client = ManagedModelScopeTransport::new().unwrap();
    for url in [
        "http://modelscope.cn/repo",
        "https://modelscope.cn:444/repo",
        "https://modelscope.cn.evil.invalid/repo",
        "https://user:password@modelscope.cn/repo",
        "https://modelscope.cn/repo#fragment",
        "https://cdn-lfs-cn-1.modelscope.cn/repo",
    ] {
        assert_eq!(
            run(client.open_range(Url::parse(url).unwrap(), &first(), &HeaderMap::new()))
                .err()
                .unwrap()
                .kind,
            ManagedTransportErrorKind::Forbidden
        );
    }
}

#[test]
fn invalid_ranges_and_unverified_resumption_do_not_touch_network() {
    let server = Server::new(response(
        "500 Internal Server Error",
        "Content-Length: 0\r\n",
        "",
    ));
    for range in [
        PinnedHttpRange {
            length: 0,
            ..first()
        },
        PinnedHttpRange {
            start: u64::MAX,
            total: u64::MAX,
            ..first()
        },
        PinnedHttpRange {
            length: MAX_MANAGED_RANGE_BYTES + 1,
            total: u64::MAX,
            ..first()
        },
        PinnedHttpRange {
            if_range: Some("W/\"weak\"".into()),
            ..resumed()
        },
    ] {
        assert_eq!(
            run(transport(&server).open_range(server.url.clone(), &range, &HeaderMap::new()))
                .err()
                .unwrap()
                .kind,
            ManagedTransportErrorKind::Protocol
        );
    }
    assert_eq!(
        run(transport(&server).open_range(
            server.url.clone(),
            &PinnedHttpRange {
                if_range: None,
                ..resumed()
            },
            &HeaderMap::new()
        ))
        .err()
        .unwrap()
        .kind,
        ManagedTransportErrorKind::ResumeUnavailable
    );
    assert!(server.requests.lock().unwrap().is_empty());
}

#[test]
fn retry_metadata_and_errors_do_not_expose_signed_urls_or_credentials() {
    for (status, kind) in [
        (
            "401 Unauthorized",
            ManagedTransportErrorKind::Authentication,
        ),
        ("403 Forbidden", ManagedTransportErrorKind::Forbidden),
        ("404 Not Found", ManagedTransportErrorKind::NotFound),
        (
            "429 Too Many Requests",
            ManagedTransportErrorKind::RateLimited,
        ),
        (
            "503 Service Unavailable",
            ManagedTransportErrorKind::Unavailable,
        ),
    ] {
        let server = Server::new(response(
            status,
            "Content-Length: 0\r\nRetry-After: 99999\r\n",
            "",
        ));
        let mut url = server.url.clone();
        url.set_query(Some("secret=unit-credential"));
        let error = run(transport(&server).open_range(url, &first(), &HeaderMap::new()))
            .err()
            .unwrap();
        assert_eq!(error.kind, kind);
        assert_eq!(error.retry_after_seconds, Some(60));
        assert!(!format!("{error:?} {error}").contains("unit-credential"));
    }
}

#[test]
fn redirect_loops_stop_at_the_declared_limit() {
    let server = Server::new(response(
        "302 Found",
        "Content-Length: 0\r\nLocation: /loop\r\n",
        "",
    ));
    assert_eq!(
        run(transport(&server).open_range(server.url.clone(), &first(), &HeaderMap::new()))
            .err()
            .unwrap()
            .kind,
        ManagedTransportErrorKind::Protocol
    );
    assert_eq!(server.requests.lock().unwrap().len(), 4);
}

#[test]
fn whole_file_stream_opens_large_bodies_without_relaxing_bounded_range_limits() {
    let total = MAX_MANAGED_RANGE_BYTES + 1;
    let server = Server::new(response(
        "206 Partial Content",
        &format!(
            "Content-Range: bytes 0-{}/{total}\r\nContent-Length: {total}\r\nETag: \"stable\"\r\n",
            total - 1
        ),
        "abcd",
    ));
    run(async {
        let client = transport(&server);
        let large = PinnedHttpRange {
            start: 0,
            length: total,
            total,
            if_range: None,
        };
        assert_eq!(
            client
                .open_range(server.url.clone(), &large, &HeaderMap::new())
                .await
                .err()
                .unwrap()
                .kind,
            ManagedTransportErrorKind::Protocol
        );
        assert!(server.requests.lock().unwrap().is_empty());
        let body = client
            .open_file(server.url.clone(), 0, total, None, &HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(body.expected_bytes(), total);
        assert!(consume(body).await.is_err());
    });
}
