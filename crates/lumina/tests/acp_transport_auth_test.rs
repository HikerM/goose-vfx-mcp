#[path = "acp_common_tests/mod.rs"]
mod common_tests;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;

use agent_client_protocol::schema::v1::InitializeRequest;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, Client, ConnectionTo};
use agent_client_protocol_http::HttpClient;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, Response, StatusCode};
use axum::Router;
use common_tests::fixtures::server::AcpServerConnection;
use common_tests::fixtures::{Connection, OpenAiFixture, TestConnectionConfig};
use lumina::acp::server::{
    AcpProviderFactory, LuminaAcpAgent, LuminaAcpAgentOptions, UNAVAILABLE_CUSTOM_ROUTE_METHODS,
};
use lumina::acp::server_factory::{AcpServer, AcpServerFactoryConfig};
use lumina::acp::transport::{create_acp_router, create_router};
use lumina::agents::LuminaPlatform;
use lumina::custom_requests::{
    DeleteSessionRequest, McpCatalogListResponse, McpGetRequest, McpGetResponse,
    McpHealthCheckMode, McpHealthRunRequest, McpHealthRunResponse, McpInstallConfirmRequest,
    McpInstallConfirmResponse, McpListRequest, McpListResponse, McpPlanCreateRequest,
    McpPlanCreateResponse, McpPlatformOutcome, McpProfileApplyConfirmRequest,
    McpProfileApplyConfirmResponse, McpProfileApplyPlanCreateRequest,
    McpProfileApplyPlanCreateResponse, McpProfileCreateRequest, McpProfileCreateResponse,
    McpTaskGetRequest, McpTaskGetResponse, McpTaskStatus, McpUserDecision, MCP_CATALOG_LIST_METHOD,
    MCP_PLAN_CREATE_METHOD,
};
use lumina::mcp_platform::{parse_manifest, InstallationScope, SqliteMcpPlatformRepository};
use lumina::scheduler::{ScheduledJob, SchedulerError};
use lumina::scheduler_trait::SchedulerTrait;
use lumina::session::{Session, SessionManager};
use lumina_test_support::IgnoreSessionId;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration};
use tower::ServiceExt;
use tracing::field::Visit;
use tracing::{Event, Subscriber};
use tracing_futures::WithSubscriber;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

const SECRET: &str = "test-secret-token";
const REMOTE: &str =
    include_str!("../../../documentation/static/schemas/examples/remote-http.json");

#[derive(Clone, Default)]
struct TraceCapture(Arc<Mutex<Vec<String>>>);

impl TraceCapture {
    fn visible(&self) -> String {
        self.0.lock().unwrap().join("\n")
    }
}

impl<S> Layer<S> for TraceCapture
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut fields = CapturedTraceFields::default();
        event.record(&mut fields);
        self.0.lock().unwrap().push(format!(
            "event:{}:{}:{}",
            event.metadata().target(),
            event.metadata().name(),
            fields.0
        ));
    }

    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        _id: &tracing::Id,
        _context: Context<'_, S>,
    ) {
        let mut fields = CapturedTraceFields::default();
        attributes.record(&mut fields);
        self.0.lock().unwrap().push(format!(
            "span:{}:{}:{}",
            attributes.metadata().target(),
            attributes.metadata().name(),
            fields.0
        ));
    }
}

#[derive(Default)]
struct CapturedTraceFields(String);

impl Visit for CapturedTraceFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push_str(field.name());
        self.0.push('=');
        self.0.push_str(value);
        self.0.push(';');
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        self.0.push_str(field.name());
        self.0.push('=');
        self.0.push_str(&format!("{value:?}"));
        self.0.push(';');
    }
}

fn assert_no_secret(visible: &str, secrets: &[String]) {
    for secret in secrets {
        assert!(
            !visible.contains(secret),
            "secret leaked through a transport-visible value: {secret}"
        );
    }
}

fn test_router(require_token: bool, dir: &tempfile::TempDir) -> Router {
    test_router_with_origins(require_token, dir, Vec::new())
}

fn test_acp_router(dir: &tempfile::TempDir) -> Router {
    let server = Arc::new(AcpServer::new(AcpServerFactoryConfig {
        builtins: vec![],
        data_dir: dir.path().join("data"),
        config_dir: dir.path().join("config"),
        lumina_platform: LuminaPlatform::LuminaCli,
        additional_source_roots: Vec::new(),
    }));
    create_acp_router(server)
}

fn test_authenticated_acp_router(dir: &tempfile::TempDir) -> Router {
    let server = Arc::new(AcpServer::new(AcpServerFactoryConfig {
        builtins: vec![],
        data_dir: dir.path().join("data"),
        config_dir: dir.path().join("config"),
        lumina_platform: LuminaPlatform::LuminaCli,
        additional_source_roots: Vec::new(),
    }));
    create_router(server, SECRET.to_string(), true, Vec::new())
}

fn test_router_with_origins(
    require_token: bool,
    dir: &tempfile::TempDir,
    additional_allowed_origins: Vec<HeaderValue>,
) -> Router {
    let server = Arc::new(AcpServer::new(AcpServerFactoryConfig {
        builtins: vec![],
        data_dir: dir.path().join("data"),
        config_dir: dir.path().join("config"),
        lumina_platform: LuminaPlatform::LuminaCli,
        additional_source_roots: Vec::new(),
    }));
    create_router(
        server,
        SECRET.to_string(),
        require_token,
        additional_allowed_origins,
    )
}

async fn send(router: &Router, method: Method, uri: &str, headers: &[(&str, &str)]) -> StatusCode {
    send_response(router, method, uri, headers).await.status()
}

async fn send_response(
    router: &Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> Response<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::empty()).unwrap();
    router.clone().oneshot(request).await.unwrap()
}

async fn send_json_response(router: &Router, body: &'static [u8]) -> Response<Body> {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/acp")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    router.clone().oneshot(request).await.unwrap()
}

#[tokio::test]
async fn acp_http_rejects_duplicate_keys_before_connection_creation() {
    let directory = tempfile::tempdir().unwrap();
    let router = test_acp_router(&directory);

    for body in [
        br#"{"jsonrpc":"2.0","jsonrpc":"2.0"}"# as &[u8],
        br#"{"jsonrpc":"2.0","params":{"key":"first","key":"second"}}"#,
        br#"[{"params":{"nested":{"key":"first","key":"second"}}}]"#,
    ] {
        let response = send_json_response(&router, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), b"Invalid JSON-RPC");
    }
}

#[tokio::test]
async fn acp_http_normal_initialize_reaches_the_agent_after_raw_validation() {
    let directory = tempfile::tempdir().unwrap();
    let router = test_acp_router(&directory);
    let response = send_json_response(
        &router,
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#,
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("acp-connection-id"));
}

#[tokio::test]
async fn websocket_uses_lumina_raw_json_scanner_before_protocol_parsing() {
    let directory = tempfile::tempdir().unwrap();
    let (address, server) = spawn_router(test_acp_router(&directory)).await;
    let mut websocket = connect_websocket(address).await;

    for payload in [
        r#"{"jsonrpc":"2.0","jsonrpc":"2.0"}"#,
        r#"{"jsonrpc":"2.0","params":{"value":1,"value":2}}"#,
        r#"{"jsonrpc":"2.0","params":{"nested":{"value":1,"value":2}}}"#,
    ] {
        websocket_send_text(&mut websocket, payload).await;
        let response = websocket_read_text(&mut websocket).await;
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["id"], serde_json::Value::Null);
        assert_eq!(response["error"]["code"], -32700);
        assert_eq!(response["error"]["data"], "invalid JSON-RPC payload");
        assert!(!response.to_string().contains("value"));
    }

    websocket_send_text(
        &mut websocket,
        r#"{"jsonrpc":"2.0","id":77,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#,
    )
    .await;
    let response = websocket_read_text(&mut websocket).await;
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], 77);
    assert!(response["result"].is_object());

    const METHOD_SECRET: &str = "acp-method-secret";
    const ID_SECRET: &str = "acp-id-secret";
    const PARAMS_SECRET: &str = "acp-params-secret";
    const URL_SECRET: &str = "acp-url-secret";
    const SESSION_SECRET: &str = "acp-session-secret";
    websocket_send_text(
        &mut websocket,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": ID_SECRET,
            "method": METHOD_SECRET,
            "params": {
                "pageSize": PARAMS_SECRET,
                "url": format!("https://example.test/?token={URL_SECRET}"),
                "sessionId": SESSION_SECRET,
            }
        })
        .to_string(),
    )
    .await;
    let response = websocket_read_text(&mut websocket).await;
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], ID_SECRET);
    assert_eq!(response["error"]["code"], -32603);
    let error_visible = format!(
        "{}{}",
        response["error"]["message"], response["error"]["data"]
    );
    for secret in [METHOD_SECRET, PARAMS_SECRET, URL_SECRET, SESSION_SECRET] {
        assert!(!error_visible.contains(secret));
    }

    server.abort();
}

#[tokio::test]
async fn websocket_guard_rejects_every_unavailable_route_before_deserialization_without_leaking() {
    const METHOD_SECRET: &str = "acp-trace-method-secret";
    const ID_SECRET: &str = "acp-trace-id-secret";
    const BAD_JSON_SECRET: &str = "acp-trace-bad-json-secret";

    if std::env::var_os("LUMINA_ACP_TRACE_CAPTURE_CHILD").is_none() {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("websocket_guard_rejects_every_unavailable_route_before_deserialization_without_leaking")
            .arg("--nocapture")
            .env("LUMINA_ACP_TRACE_CAPTURE_CHILD", "1")
            .status()
            .unwrap();
        assert!(status.success(), "isolated ACP transport trace test failed");
        return;
    }

    let traces = TraceCapture::default();
    let subscriber = tracing_subscriber::registry().with(traces.clone());
    tracing::subscriber::set_global_default(subscriber)
        .expect("isolated child process must own the tracing subscriber");
    let secrets = {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("data");
        let (address, server) = spawn_router(test_acp_router(&directory)).await;
        let mut websocket = connect_websocket(address).await;

        websocket_send_text(
            &mut websocket,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#,
        )
        .await;
        let initialized: serde_json::Value =
            serde_json::from_str(&websocket_read_text(&mut websocket).await).unwrap();
        assert_eq!(initialized["id"], 1);
        assert!(initialized["result"].is_object());

        websocket_send_text(
            &mut websocket,
            &format!(
                r#"{{"jsonrpc":"2.0","id":"{BAD_JSON_SECRET}","method":"initialize","params":{{"value":"{BAD_JSON_SECRET}","value":"{BAD_JSON_SECRET}"}}}}"#
            ),
        )
        .await;
        let malformed = websocket_read_text(&mut websocket).await;
        assert_no_secret(&malformed, &[BAD_JSON_SECRET.to_string()]);
        let malformed: serde_json::Value = serde_json::from_str(&malformed).unwrap();
        assert_eq!(malformed["id"], serde_json::Value::Null);
        assert_eq!(malformed["error"]["code"], -32700);

        websocket_send_text(
            &mut websocket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": ID_SECRET,
                "method": METHOD_SECRET,
                "params": { "query": METHOD_SECRET },
            })
            .to_string(),
        )
        .await;
        let unknown = websocket_read_text(&mut websocket).await;
        let unknown: serde_json::Value = serde_json::from_str(&unknown).unwrap();
        assert_eq!(unknown["id"], ID_SECRET);
        assert_eq!(unknown["error"]["code"], -32603);
        assert_no_secret(
            &format!(
                "{}{}",
                unknown["error"]["message"], unknown["error"]["data"]
            ),
            &[METHOD_SECRET.to_string(), ID_SECRET.to_string()],
        );

        let mut secrets = vec![
            METHOD_SECRET.to_string(),
            ID_SECRET.to_string(),
            BAD_JSON_SECRET.to_string(),
        ];
        secrets.extend(
            assert_all_unavailable_custom_routes_denied_over_websocket(&mut websocket, &data_dir)
                .await,
        );
        secrets.extend(
            assert_all_unavailable_custom_routes_denied_over_http_wire(address, &data_dir).await,
        );
        server.abort();
        secrets
    };

    let visible = traces.visible();
    assert!(visible.contains("ACP WebSocket message received"));
    assert!(visible.contains("acp_transport_receive"));
    assert!(visible.contains("acp_protocol_dispatch"));
    assert!(visible.contains("acp_protocol_responder_error"));
    assert!(visible.contains("acp_custom_request_failed"));
    assert!(
        visible.matches("acp_protocol_responder_error").count()
            >= UNAVAILABLE_CUSTOM_ROUTE_METHODS.len() * 2
    );
    assert_no_secret(&visible, &secrets);
}

async fn insert_manifest(data_dir: &std::path::Path) -> (Arc<SqliteMcpPlatformRepository>, String) {
    let path = data_dir.join("mcp-platform").join("platform.db");
    let repository = Arc::new(SqliteMcpPlatformRepository::open_path(&path).await.unwrap());
    let verified = parse_manifest(REMOTE.as_bytes()).unwrap();
    let digest = verified.digest().to_string();
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO manifest_blobs (
            manifest_digest, mcp_id, version, canonical_bytes, proof_json, trust_tier_json,
            created_at_ms
        ) VALUES (?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&digest)
    .bind(&verified.manifest().id)
    .bind(verified.manifest().version.as_str())
    .bind(verified.canonical_json())
    .bind(serde_json::to_string(&lumina::mcp_platform::ManifestProof::LocalBytes).unwrap())
    .bind(serde_json::to_string(&lumina::mcp_platform::TrustTier::Local).unwrap())
    .bind(1_000_i64)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    (repository, digest)
}

async fn spawn_router(router: Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(
        async move {
            axum::serve(listener, router).await.unwrap();
        }
        .with_current_subscriber(),
    );
    (addr, handle)
}

async fn connect_websocket(address: std::net::SocketAddr) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /acp HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let mut stream = BufReader::new(stream);
    let mut status = String::new();
    stream.read_line(&mut status).await.unwrap();
    assert!(
        status.starts_with("HTTP/1.1 101"),
        "unexpected status: {status}"
    );
    loop {
        let mut header = String::new();
        stream.read_line(&mut header).await.unwrap();
        if header == "\r\n" {
            return stream;
        }
    }
}

async fn websocket_send_text(stream: &mut BufReader<TcpStream>, text: &str) {
    assert!(text.len() <= usize::from(u16::MAX));
    let mask = [0x11, 0x22, 0x33, 0x44];
    let mut frame = Vec::with_capacity(text.len() + 8);
    frame.push(0x81);
    if text.len() < 126 {
        frame.push(0x80 | text.len() as u8);
    } else {
        frame.push(0x80 | 126);
        frame.extend(u16::try_from(text.len()).unwrap().to_be_bytes());
    }
    frame.extend(mask);
    frame.extend(
        text.bytes()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    stream.get_mut().write_all(&frame).await.unwrap();
}

async fn websocket_read_text(stream: &mut BufReader<TcpStream>) -> String {
    let mut header = [0_u8; 2];
    stream.read_exact(&mut header).await.unwrap();
    assert_eq!(header[0] & 0x0f, 0x1);
    assert_eq!(header[1] & 0x80, 0);
    let length = match header[1] & 0x7f {
        length @ 0..=125 => usize::from(length),
        126 => {
            let mut extended = [0_u8; 2];
            stream.read_exact(&mut extended).await.unwrap();
            usize::from(u16::from_be_bytes(extended))
        }
        127 => {
            let mut extended = [0_u8; 8];
            stream.read_exact(&mut extended).await.unwrap();
            usize::try_from(u64::from_be_bytes(extended)).unwrap()
        }
    };
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload).await.unwrap();
    String::from_utf8(payload).unwrap()
}

struct RawHttpResponse {
    status: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl RawHttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn wire(&self) -> String {
        format!("{}{:?}{}", self.status, self.headers, self.body)
    }
}

async fn raw_http_json_request(
    address: std::net::SocketAddr,
    connection_id: Option<&str>,
    payload: &str,
) -> RawHttpResponse {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let connection_header = connection_id
        .map(|id| format!("acp-connection-id: {id}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /acp HTTP/1.1\r\nHost: {address}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{connection_header}\r\n{payload}",
        payload.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut stream = BufReader::new(stream);
    let mut status = String::new();
    stream.read_line(&mut status).await.unwrap();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
        let (name, value) = line
            .trim_end()
            .split_once(':')
            .expect("HTTP response header must contain a colon");
        headers.push((name.to_string(), value.trim().to_string()));
    }
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.parse::<usize>().unwrap())
        .unwrap_or_default();
    let mut body = vec![0_u8; content_length];
    stream.read_exact(&mut body).await.unwrap();

    RawHttpResponse {
        status,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

async fn raw_http_initialize(address: std::net::SocketAddr) -> String {
    let response = raw_http_json_request(
        address,
        None,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#,
    )
    .await;
    assert!(response.status.starts_with("HTTP/1.1 200"));
    response
        .header("acp-connection-id")
        .expect("initialize must return an ACP connection ID")
        .to_string()
}

struct RawHttpSse {
    stream: BufReader<TcpStream>,
    buffered: String,
}

impl RawHttpSse {
    async fn read_event(&mut self) -> String {
        loop {
            if let Some(end) = self.buffered.find("\n\n") {
                let event = self.buffered[..end].to_string();
                self.buffered.drain(..end + 2);
                let data = event
                    .lines()
                    .find_map(|line| line.strip_prefix("data:"))
                    .expect("SSE event must contain JSON-RPC data");
                return data.trim_start().to_string();
            }

            let mut chunk_size = String::new();
            self.stream.read_line(&mut chunk_size).await.unwrap();
            let chunk_size = usize::from_str_radix(
                chunk_size
                    .trim()
                    .split_once(';')
                    .map_or(chunk_size.trim(), |(size, _)| size),
                16,
            )
            .unwrap();
            assert_ne!(chunk_size, 0, "SSE stream ended before a response event");
            let mut chunk = vec![0_u8; chunk_size];
            self.stream.read_exact(&mut chunk).await.unwrap();
            let mut chunk_terminator = [0_u8; 2];
            self.stream.read_exact(&mut chunk_terminator).await.unwrap();
            assert_eq!(chunk_terminator, *b"\r\n");
            self.buffered.push_str(&String::from_utf8(chunk).unwrap());
        }
    }
}

async fn raw_http_open_sse(address: std::net::SocketAddr, connection_id: &str) -> RawHttpSse {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /acp HTTP/1.1\r\nHost: {address}\r\naccept: text/event-stream\r\nacp-connection-id: {connection_id}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut stream = BufReader::new(stream);
    let mut status = String::new();
    stream.read_line(&mut status).await.unwrap();
    assert!(status.starts_with("HTTP/1.1 200"));
    loop {
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            return RawHttpSse {
                stream,
                buffered: String::new(),
            };
        }
    }
}

async fn connect_http_client(
    endpoint: String,
) -> (ConnectionTo<Agent>, tokio::task::JoinHandle<()>) {
    let cx_holder: Arc<Mutex<Option<ConnectionTo<Agent>>>> = Arc::new(Mutex::new(None));
    let cx_holder_clone = cx_holder.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(
        async move {
            let result = Client::builder()
                .connect_with(HttpClient::with_endpoint(endpoint).unwrap(), {
                    let cx_holder = cx_holder_clone;
                    async move |cx: ConnectionTo<Agent>| {
                        let _ = cx
                            .send_request(InitializeRequest::new(ProtocolVersion::LATEST))
                            .block_task()
                            .await
                            .unwrap();
                        *cx_holder.lock().unwrap() = Some(cx.clone());
                        let _ = ready_tx.send(());
                        std::future::pending::<Result<(), agent_client_protocol::Error>>().await
                    }
                })
                .await;
            if result.is_err() {
                return;
            }
        }
        .with_current_subscriber(),
    );
    ready_rx.await.unwrap();
    (cx_holder.lock().unwrap().take().unwrap(), handle)
}

async fn send_custom(
    cx: &ConnectionTo<Agent>,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, agent_client_protocol::Error> {
    let msg = agent_client_protocol::UntypedMessage::new(method, params).unwrap();
    cx.send_request(msg).block_task().await
}

fn unavailable_route_params(method: &str, ordinal: usize) -> (serde_json::Value, Vec<String>) {
    let secrets = [
        format!("acp-guard-{ordinal}-method-{method}-secret"),
        format!("acp-guard-{ordinal}-page-size-secret"),
        format!("acp-guard-{ordinal}-url-secret"),
        format!("acp-guard-{ordinal}-query-secret"),
        format!("acp-guard-{ordinal}-session-secret"),
        format!("acp-guard-{ordinal}-serde-error-secret"),
    ];
    (
        serde_json::json!({
            "requestedMethod": secrets[0],
            "pageSize": secrets[1],
            "url": format!("https://example.test/?token={}", secrets[2]),
            "query": secrets[3],
            "sessionId": secrets[4],
            "badJson": { "serdeError": secrets[5] },
        }),
        secrets.into(),
    )
}

fn assert_public_unavailable_error(error: agent_client_protocol::Error, secrets: &[String]) {
    assert_eq!(
        error.code,
        agent_client_protocol::Error::method_not_found().code
    );
    assert_eq!(error.data, Some(serde_json::json!("acp_protocol_error")));
    assert_no_secret(
        &format!(
            "display={error};debug={error:?};json={}",
            serde_json::to_string(&error).unwrap()
        ),
        secrets,
    );
}

async fn assert_all_unavailable_custom_routes_denied(
    cx: &ConnectionTo<Agent>,
    data_dir: &Path,
) -> Vec<String> {
    let mut secrets = Vec::new();
    for (ordinal, method) in UNAVAILABLE_CUSTOM_ROUTE_METHODS.iter().enumerate() {
        let (params, method_secrets) = unavailable_route_params(method, ordinal);
        let before = exact_persisted_footprint_snapshot(data_dir).await;
        let error = send_custom(cx, method, params).await.expect_err(
            "every unavailable custom route must reject before request deserialization",
        );
        let after = exact_persisted_footprint_snapshot(data_dir).await;
        assert_exact_persisted_footprint_unchanged(&before, &after);
        assert_public_unavailable_error(error, &method_secrets);
        secrets.extend(method_secrets);
    }
    secrets
}

async fn assert_all_unavailable_custom_routes_denied_over_websocket(
    websocket: &mut BufReader<TcpStream>,
    data_dir: &Path,
) -> Vec<String> {
    let mut secrets = Vec::new();
    for (ordinal, method) in UNAVAILABLE_CUSTOM_ROUTE_METHODS.iter().enumerate() {
        let (params, method_secrets) = unavailable_route_params(method, ordinal);
        let before = exact_persisted_footprint_snapshot(data_dir).await;
        websocket_send_text(
            websocket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": ordinal + 1,
                "method": method,
                "params": params,
            })
            .to_string(),
        )
        .await;
        let response = websocket_read_text(websocket).await;
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(json["id"], ordinal + 1);
        assert_eq!(json["error"]["code"], -32601);
        assert_eq!(json["error"]["data"], "acp_protocol_error");
        assert_no_secret(&response, &method_secrets);
        let after = exact_persisted_footprint_snapshot(data_dir).await;
        assert_exact_persisted_footprint_unchanged(&before, &after);
        secrets.extend(method_secrets);
    }
    secrets
}

async fn assert_all_unavailable_custom_routes_denied_over_http_wire(
    address: std::net::SocketAddr,
    data_dir: &Path,
) -> Vec<String> {
    const BAD_JSON_SECRET: &str = "acp-http-bad-json-secret";
    let malformed = raw_http_json_request(
        address,
        None,
        &format!(
            r#"{{"jsonrpc":"2.0","id":"{BAD_JSON_SECRET}","method":"initialize","params":{{"value":"{BAD_JSON_SECRET}","value":"{BAD_JSON_SECRET}"}}}}"#
        ),
    )
    .await;
    assert!(malformed.status.starts_with("HTTP/1.1 400"));
    assert_no_secret(&malformed.wire(), &[BAD_JSON_SECRET.to_string()]);

    let connection_id = raw_http_initialize(address).await;
    let mut events = raw_http_open_sse(address, &connection_id).await;
    let mut secrets = vec![BAD_JSON_SECRET.to_string()];
    for (ordinal, method) in UNAVAILABLE_CUSTOM_ROUTE_METHODS.iter().enumerate() {
        let (params, method_secrets) = unavailable_route_params(method, ordinal);
        let request_id = ordinal + 1;
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        })
        .to_string();
        let before = exact_persisted_footprint_snapshot(data_dir).await;
        let post = raw_http_json_request(address, Some(&connection_id), &payload).await;
        assert!(post.status.starts_with("HTTP/1.1 202"));
        assert_no_secret(&post.wire(), &method_secrets);
        let response = events.read_event().await;
        let after = exact_persisted_footprint_snapshot(data_dir).await;
        assert_exact_persisted_footprint_unchanged(&before, &after);
        let json: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(json["id"], request_id);
        assert_eq!(json["error"]["code"], -32601);
        assert_eq!(json["error"]["data"], "acp_protocol_error");
        assert_no_secret(&response, &method_secrets);
        secrets.extend(method_secrets);
    }
    secrets
}

async fn assert_plan_create_denied(cx: &ConnectionTo<Agent>, digest: &str, idempotency_key: &str) {
    let denied = send_custom(
        cx,
        MCP_PLAN_CREATE_METHOD,
        serde_json::json!({
            "intent": {
                "type": "register",
                "manifestDigest": digest,
                "installationScope": InstallationScope::User,
            },
            "idempotencyKey": idempotency_key,
        }),
    )
    .await
    .expect_err("unreviewed plan creation must be rejected before dispatch");
    assert_eq!(
        denied.code,
        agent_client_protocol::Error::method_not_found().code
    );
    assert_eq!(denied.data, Some(serde_json::json!("acp_protocol_error")));
}

async fn assert_plan_create_available(
    cx: &ConnectionTo<Agent>,
    digest: &str,
    idempotency_key: &str,
) {
    let response = send_custom(
        cx,
        MCP_PLAN_CREATE_METHOD,
        serde_json::json!({
            "intent": {
                "type": "register",
                "manifestDigest": digest,
                "installationScope": InstallationScope::User,
            },
            "idempotencyKey": idempotency_key,
        }),
    )
    .await
    .expect("authenticated transport must create a review-only plan");
    let response: McpPlanCreateResponse = serde_json::from_value(response).unwrap();
    let McpPlatformOutcome::Success { value } = response.outcome else {
        panic!("authenticated plan creation must return a successful platform outcome");
    };
    assert!(!value.plan_id.is_empty());
    assert!(!value.plan_digest.is_empty());
}

async fn assert_catalog_available_read_only(cx: &ConnectionTo<Agent>) {
    let response = send_custom(
        cx,
        MCP_CATALOG_LIST_METHOD,
        serde_json::json!({
            "pageSize": 10,
        }),
    )
    .await
    .expect("catalog reads must be available without mutation authority");
    let response: McpCatalogListResponse = serde_json::from_value(response).unwrap();
    let McpPlatformOutcome::Success { value } = response.outcome else {
        panic!("catalog reads must return a successful platform outcome");
    };
    assert!(!value.items.is_empty());
}

fn database_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("mcp-platform").join("platform.db")
}

fn sessions_database_path(data_dir: &Path) -> PathBuf {
    data_dir.join("sessions").join("sessions.db")
}

async fn open_pool(path: &std::path::Path) -> sqlx::Pool<sqlx::Sqlite> {
    open_pool_with_create(path, false).await
}

async fn open_pool_with_create(
    path: &std::path::Path,
    create_if_missing: bool,
) -> sqlx::Pool<sqlx::Sqlite> {
    if create_if_missing {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create_if_missing)
        .foreign_keys(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

fn sha256_hex(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn wait_for_terminal_task(
    cx: &ConnectionTo<Agent>,
    task_id: &str,
) -> lumina::custom_requests::McpTaskRef {
    for _ in 0..100 {
        let response = cx
            .send_request(McpTaskGetRequest {
                task_id: task_id.to_string(),
            })
            .block_task()
            .await
            .unwrap();
        let McpTaskGetResponse {
            outcome: McpPlatformOutcome::Success { value },
        } = response
        else {
            panic!("task lookup must succeed");
        };
        if matches!(
            value.status,
            McpTaskStatus::Succeeded
                | McpTaskStatus::Failed
                | McpTaskStatus::Cancelled
                | McpTaskStatus::Interrupted
                | McpTaskStatus::RecoveryRequired
        ) {
            return value;
        }
        sleep(Duration::from_millis(25)).await;
    }
    panic!("task did not reach a terminal status");
}

struct PreparedProfileApplication {
    token: String,
    application_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileApplicationEvent {
    event_type: String,
    detail_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileApplicationRecord {
    status: String,
    session_id: Option<String>,
    failure_code: Option<String>,
    events: Vec<ProfileApplicationEvent>,
}

type PlatformSideEffectSnapshot = DatabaseSideEffectSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionsSchemaShape {
    FreshBootstrap,
    LegacyUpgrade,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSideEffectSnapshot {
    shape: SessionsSchemaShape,
    database: DatabaseSideEffectSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotValue {
    Null,
    Integer(i64),
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotValueKind {
    Integer,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ColumnSpec {
    name: &'static str,
    expr: &'static str,
    kind: SnapshotValueKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TableSpec {
    table_name: &'static str,
    order_by: &'static str,
    columns: Vec<ColumnSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LegacyFixtureColumnSpec {
    name: &'static str,
    declared_type: &'static str,
    not_null: bool,
    default_sql: Option<&'static str>,
    pk: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LegacyFixtureIndexSpec {
    name: &'static str,
    unique: bool,
    origin: &'static str,
    partial: bool,
    columns: &'static [&'static str],
    create_sql: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TableSnapshot {
    table_name: &'static str,
    columns: Vec<&'static str>,
    rows: Vec<Vec<SnapshotValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DatabaseSideEffectSnapshot {
    tables: Vec<TableSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExactPersistedFootprint {
    platform: PlatformSideEffectSnapshot,
    sessions: SessionSideEffectSnapshot,
}

impl ColumnSpec {
    const fn integer(name: &'static str) -> Self {
        Self::integer_expr(name, name)
    }

    const fn integer_expr(name: &'static str, expr: &'static str) -> Self {
        Self {
            name,
            expr,
            kind: SnapshotValueKind::Integer,
        }
    }

    const fn text(name: &'static str) -> Self {
        Self::text_expr(name, name)
    }

    const fn text_expr(name: &'static str, expr: &'static str) -> Self {
        Self {
            name,
            expr,
            kind: SnapshotValueKind::Text,
        }
    }
}

impl TableSpec {
    fn new(table_name: &'static str, order_by: &'static str, columns: Vec<ColumnSpec>) -> Self {
        Self {
            table_name,
            order_by,
            columns,
        }
    }

    fn expected_columns(&self) -> Vec<&'static str> {
        self.columns.iter().map(|column| column.name).collect()
    }
}

impl LegacyFixtureColumnSpec {
    const fn new(
        name: &'static str,
        declared_type: &'static str,
        not_null: bool,
        default_sql: Option<&'static str>,
        pk: i64,
    ) -> Self {
        Self {
            name,
            declared_type,
            not_null,
            default_sql,
            pk,
        }
    }
}

impl LegacyFixtureIndexSpec {
    const fn new(
        name: &'static str,
        unique: bool,
        origin: &'static str,
        partial: bool,
        columns: &'static [&'static str],
        create_sql: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            unique,
            origin,
            partial,
            columns,
            create_sql,
        }
    }
}

impl DatabaseSideEffectSnapshot {
    fn table(&self, table_name: &str) -> &TableSnapshot {
        self.tables
            .iter()
            .find(|table| table.table_name == table_name)
            .unwrap_or_else(|| panic!("missing table snapshot: {table_name}"))
    }

    fn table_mut(&mut self, table_name: &str) -> &mut TableSnapshot {
        self.tables
            .iter_mut()
            .find(|table| table.table_name == table_name)
            .unwrap_or_else(|| panic!("missing table snapshot: {table_name}"))
    }
}

impl TableSnapshot {
    fn column_index(&self, column_name: &str) -> usize {
        self.columns
            .iter()
            .position(|column| *column == column_name)
            .unwrap_or_else(|| panic!("missing column {column_name} in {}", self.table_name))
    }

    fn rows_with_text_value(&self, column_name: &str, expected: &str) -> Vec<Vec<SnapshotValue>> {
        let index = self.column_index(column_name);
        self.rows
            .iter()
            .filter(|row| row.get(index) == Some(&SnapshotValue::Text(expected.to_string())))
            .cloned()
            .collect()
    }

    fn row_with_text_value(&self, column_name: &str, expected: &str) -> Vec<SnapshotValue> {
        let rows = self.rows_with_text_value(column_name, expected);
        match rows.as_slice() {
            [row] => row.clone(),
            [] => panic!(
                "missing row where {column_name}={expected} in {}",
                self.table_name
            ),
            _ => panic!(
                "expected exactly one row where {column_name}={expected} in {}",
                self.table_name
            ),
        }
    }

    fn remove_rows_with_text_value(&mut self, column_name: &str, expected: &str) {
        let index = self.column_index(column_name);
        self.rows
            .retain(|row| row.get(index) != Some(&SnapshotValue::Text(expected.to_string())));
    }
}

impl SessionSideEffectSnapshot {
    fn table(&self, table_name: &str) -> &TableSnapshot {
        self.database.table(table_name)
    }

    fn table_mut(&mut self, table_name: &str) -> &mut TableSnapshot {
        self.database.table_mut(table_name)
    }
}

fn platform_table_specs() -> Vec<TableSpec> {
    vec![
        TableSpec::new(
            "activation_journal",
            "activation_id",
            vec![
                ColumnSpec::integer("activation_id"),
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("task_id"),
                ColumnSpec::text("previous_version"),
                ColumnSpec::text("target_version"),
                ColumnSpec::text("previous_state_json"),
                ColumnSpec::text("status"),
                ColumnSpec::integer("created_at_ms"),
                ColumnSpec::integer("updated_at_ms"),
            ],
        ),
        TableSpec::new(
            "artifact_cache",
            "artifact_digest",
            vec![
                ColumnSpec::text("artifact_digest"),
                ColumnSpec::text("source_origin"),
                ColumnSpec::integer("size_bytes"),
                ColumnSpec::text("adapter_id"),
                ColumnSpec::text("adapter_version"),
                ColumnSpec::text("platform_selector"),
                ColumnSpec::text("verification_evidence_json"),
                ColumnSpec::integer("reference_count"),
                ColumnSpec::integer("verified_at_ms"),
            ],
        ),
        TableSpec::new(
            "artifact_claims",
            "artifact_digest",
            vec![
                ColumnSpec::text("artifact_digest"),
                ColumnSpec::text("owner_task_id"),
                ColumnSpec::text("status"),
                ColumnSpec::integer("updated_at_ms"),
            ],
        ),
        TableSpec::new(
            "audit_events",
            "task_id, sequence, event_id",
            vec![
                ColumnSpec::integer("event_id"),
                ColumnSpec::text("task_id"),
                ColumnSpec::integer("sequence"),
                ColumnSpec::text("event_type"),
                ColumnSpec::text("actor"),
                ColumnSpec::integer("occurred_at_ms"),
                ColumnSpec::text("payload_json"),
                ColumnSpec::text("redacted_error_json"),
            ],
        ),
        TableSpec::new(
            "connection_projections",
            "managed_mcp_id",
            vec![
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("link_key"),
                ColumnSpec::text("projection_json"),
                ColumnSpec::integer("revision"),
                ColumnSpec::integer("updated_at_ms"),
                ColumnSpec::text("plan_id"),
                ColumnSpec::text("manifest_digest"),
                ColumnSpec::text("owner_task_id"),
                ColumnSpec::text("projection_digest"),
            ],
        ),
        TableSpec::new(
            "health_observations",
            "observation_id",
            vec![
                ColumnSpec::integer("observation_id"),
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("task_id"),
                ColumnSpec::text("check_type"),
                ColumnSpec::text("result_code"),
                ColumnSpec::integer("latency_ms"),
                ColumnSpec::text("capabilities_digest"),
                ColumnSpec::text("tools_digest"),
                ColumnSpec::integer("checked_at_ms"),
                ColumnSpec::text("detail_json"),
            ],
        ),
        TableSpec::new(
            "health_task_requests",
            "task_id",
            vec![
                ColumnSpec::text("task_id"),
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("mode"),
            ],
        ),
        TableSpec::new(
            "install_plans",
            "plan_id",
            vec![
                ColumnSpec::text("plan_id"),
                ColumnSpec::text("plan_digest"),
                ColumnSpec::text("envelope_digest"),
                ColumnSpec::text("manifest_digest"),
                ColumnSpec::text("operation"),
                ColumnSpec::text("target_json"),
                ColumnSpec::text("plan_json"),
                ColumnSpec::text("policy_evidence_json"),
                ColumnSpec::text("confirmation_evidence_json"),
                ColumnSpec::integer("expires_at_ms"),
                ColumnSpec::text("idempotency_key"),
                ColumnSpec::text("actor"),
                ColumnSpec::integer("created_at_ms"),
            ],
        ),
        TableSpec::new(
            "installation_ownership",
            "managed_mcp_id, version, relative_path",
            vec![
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("version"),
                ColumnSpec::text("relative_path"),
                ColumnSpec::text("path_kind"),
                ColumnSpec::text("expected_digest"),
                ColumnSpec::text("owner_task_id"),
                ColumnSpec::integer("remove_on_uninstall"),
            ],
        ),
        TableSpec::new(
            "integrity_commits",
            "sequence",
            vec![
                ColumnSpec::integer("sequence"),
                ColumnSpec::text("instance_id"),
                ColumnSpec::integer("key_epoch"),
                ColumnSpec::text("parent_root"),
                ColumnSpec::text("state_digest"),
                ColumnSpec::text("root"),
                ColumnSpec::text("commit_mac"),
            ],
        ),
        TableSpec::new(
            "integrity_metadata",
            "singleton",
            vec![
                ColumnSpec::integer("singleton"),
                ColumnSpec::text("instance_id"),
                ColumnSpec::text("path_binding"),
                ColumnSpec::integer("key_epoch"),
                ColumnSpec::integer("sequence"),
                ColumnSpec::text("root"),
                ColumnSpec::text("state_digest"),
                ColumnSpec::text("commit_mac"),
                ColumnSpec::text("status"),
            ],
        ),
        TableSpec::new(
            "lifecycle_task_targets",
            "task_id",
            vec![
                ColumnSpec::text("task_id"),
                ColumnSpec::text("managed_mcp_id"),
            ],
        ),
        TableSpec::new(
            "managed_lifecycle_leases",
            "managed_mcp_id",
            vec![
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("task_id"),
                ColumnSpec::text("operation"),
                ColumnSpec::integer("acquired_at_ms"),
                ColumnSpec::integer("expires_at_ms"),
            ],
        ),
        TableSpec::new(
            "managed_mcps",
            "managed_mcp_id",
            vec![
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("mcp_id"),
                ColumnSpec::text("installation_scope"),
                ColumnSpec::text("state_json"),
                ColumnSpec::integer("revision"),
                ColumnSpec::integer("created_at_ms"),
                ColumnSpec::integer("updated_at_ms"),
                ColumnSpec::text("distribution_adapter"),
                ColumnSpec::text("active_manifest_digest"),
                ColumnSpec::text("active_version"),
                ColumnSpec::text("owner_task_id"),
            ],
        ),
        TableSpec::new(
            "managed_projection_integrity",
            "managed_mcp_id",
            vec![ColumnSpec::text("managed_mcp_id"), ColumnSpec::text("mac")],
        ),
        TableSpec::new(
            "managed_versions",
            "managed_mcp_id, version",
            vec![
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("version"),
                ColumnSpec::text("manifest_digest"),
                ColumnSpec::text("installation_root"),
                ColumnSpec::integer("verified"),
                ColumnSpec::integer("active"),
                ColumnSpec::text("adapter_evidence_json"),
                ColumnSpec::integer("created_at_ms"),
                ColumnSpec::text("artifact_digest"),
                ColumnSpec::text("verification_evidence_json"),
                ColumnSpec::text("materialized_tree_digest"),
                ColumnSpec::text("activation_state"),
                ColumnSpec::text("supply_chain_evidence_json"),
            ],
        ),
        TableSpec::new(
            "manifest_blobs",
            "manifest_digest",
            vec![
                ColumnSpec::text("manifest_digest"),
                ColumnSpec::text("mcp_id"),
                ColumnSpec::text("version"),
                ColumnSpec::text_expr("canonical_bytes", "lower(hex(canonical_bytes))"),
                ColumnSpec::text("proof_json"),
                ColumnSpec::text("trust_tier_json"),
                ColumnSpec::integer("created_at_ms"),
            ],
        ),
        TableSpec::new(
            "mcp_profile_application_events",
            "event_id",
            vec![
                ColumnSpec::integer("event_id"),
                ColumnSpec::text("application_id"),
                ColumnSpec::text("event_type"),
                ColumnSpec::text("actor"),
                ColumnSpec::integer("occurred_at_ms"),
                ColumnSpec::text("detail_code"),
            ],
        ),
        TableSpec::new(
            "mcp_profile_apply_confirmations",
            "confirmation_hash",
            vec![
                ColumnSpec::text("confirmation_hash"),
                ColumnSpec::text("plan_id"),
                ColumnSpec::text("actor"),
                ColumnSpec::integer("expires_at_ms"),
                ColumnSpec::integer("consumed_at_ms"),
                ColumnSpec::integer("created_at_ms"),
            ],
        ),
        TableSpec::new(
            "mcp_profile_application_tokens",
            "token_hash",
            vec![
                ColumnSpec::text("token_hash"),
                ColumnSpec::text("application_id"),
                ColumnSpec::text("plan_id"),
                ColumnSpec::text("plan_digest"),
                ColumnSpec::text("profile_id"),
                ColumnSpec::integer("profile_revision"),
                ColumnSpec::text("actor"),
                ColumnSpec::integer("expires_at_ms"),
                ColumnSpec::integer("consumed_at_ms"),
                ColumnSpec::integer("created_at_ms"),
            ],
        ),
        TableSpec::new(
            "mcp_profile_applications",
            "application_id",
            vec![
                ColumnSpec::text("application_id"),
                ColumnSpec::text("token_hash"),
                ColumnSpec::text("plan_id"),
                ColumnSpec::text("profile_id"),
                ColumnSpec::integer("profile_revision"),
                ColumnSpec::text("plan_digest"),
                ColumnSpec::text("actor"),
                ColumnSpec::text("status"),
                ColumnSpec::text("session_id"),
                ColumnSpec::text("failure_code"),
                ColumnSpec::integer("created_at_ms"),
                ColumnSpec::integer("updated_at_ms"),
            ],
        ),
        TableSpec::new(
            "mcp_profile_apply_plans",
            "plan_id",
            vec![
                ColumnSpec::text("plan_id"),
                ColumnSpec::text("profile_id"),
                ColumnSpec::integer("profile_revision"),
                ColumnSpec::text("plan_digest"),
                ColumnSpec::text("snapshot_json"),
                ColumnSpec::text("actor"),
                ColumnSpec::integer("expires_at_ms"),
                ColumnSpec::text("idempotency_key"),
                ColumnSpec::text("request_digest"),
                ColumnSpec::integer("created_at_ms"),
            ],
        ),
        TableSpec::new(
            "mcp_profile_entries",
            "profile_id, ordinal, managed_mcp_id",
            vec![
                ColumnSpec::text("profile_id"),
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::integer("ordinal"),
            ],
        ),
        TableSpec::new(
            "mcp_profile_idempotency",
            "operation, idempotency_key",
            vec![
                ColumnSpec::text("operation"),
                ColumnSpec::text("idempotency_key"),
                ColumnSpec::text("request_digest"),
                ColumnSpec::text("profile_id"),
                ColumnSpec::integer("resulting_revision"),
                ColumnSpec::integer("created_at_ms"),
            ],
        ),
        TableSpec::new(
            "mcp_profile_revisions",
            "profile_id, revision",
            vec![
                ColumnSpec::text("profile_id"),
                ColumnSpec::integer("revision"),
                ColumnSpec::text("snapshot_json"),
                ColumnSpec::text("actor"),
                ColumnSpec::text("operation"),
                ColumnSpec::integer("created_at_ms"),
            ],
        ),
        TableSpec::new(
            "mcp_profiles",
            "profile_id",
            vec![
                ColumnSpec::text("profile_id"),
                ColumnSpec::text("name"),
                ColumnSpec::text("description"),
                ColumnSpec::integer("revision"),
                ColumnSpec::integer("archived"),
                ColumnSpec::integer("created_at_ms"),
                ColumnSpec::integer("updated_at_ms"),
            ],
        ),
        TableSpec::new(
            "projection_mutations",
            "mutation_id",
            vec![
                ColumnSpec::integer("mutation_id"),
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::integer("expected_revision"),
                ColumnSpec::integer("previous_enabled"),
                ColumnSpec::integer("desired_enabled"),
                ColumnSpec::text("status"),
                ColumnSpec::text("writer_runtime_id"),
                ColumnSpec::text("writer_sink_id"),
                ColumnSpec::text("writer_anchor_instance_id"),
                ColumnSpec::text("writer_anchor_path_binding"),
                ColumnSpec::integer("writer_anchor_key_epoch"),
                ColumnSpec::integer("writer_anchor_sequence"),
                ColumnSpec::text("writer_anchor_root"),
                ColumnSpec::text("writer_commitment"),
                ColumnSpec::integer("created_at_ms"),
                ColumnSpec::integer("updated_at_ms"),
            ],
        ),
        TableSpec::new(
            "projection_writer_bindings",
            "managed_mcp_id, task_id",
            vec![
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("task_id"),
                ColumnSpec::text("plan_id"),
                ColumnSpec::text("plan_digest"),
                ColumnSpec::text("link_key"),
                ColumnSpec::text("manifest_digest"),
                ColumnSpec::text("projection_digest"),
                ColumnSpec::integer("lifecycle_acquired_at_ms"),
                ColumnSpec::text("worker_owner_id"),
                ColumnSpec::integer("worker_lease_expires_at_ms"),
                ColumnSpec::integer("step_ordinal"),
                ColumnSpec::text("step_token"),
                ColumnSpec::text("mac"),
            ],
        ),
        TableSpec::new(
            "schema_version",
            "version",
            vec![
                ColumnSpec::integer("version"),
                ColumnSpec::integer("applied_at_ms"),
            ],
        ),
        TableSpec::new(
            "task_retry_attempts",
            "task_id, attempt",
            vec![
                ColumnSpec::text("task_id"),
                ColumnSpec::integer("attempt"),
                ColumnSpec::text("idempotency_key"),
                ColumnSpec::text("requested_from_status"),
                ColumnSpec::text("actor"),
                ColumnSpec::integer("created_at_ms"),
            ],
        ),
        TableSpec::new(
            "task_step_compensation_bindings",
            "task_id, ordinal",
            vec![
                ColumnSpec::text("task_id"),
                ColumnSpec::integer("ordinal"),
                ColumnSpec::text("mac"),
            ],
        ),
        TableSpec::new(
            "task_step_history",
            "task_id, attempt, ordinal",
            vec![
                ColumnSpec::text("task_id"),
                ColumnSpec::integer("attempt"),
                ColumnSpec::integer("ordinal"),
                ColumnSpec::text("status"),
                ColumnSpec::text("idempotency_token"),
                ColumnSpec::text("compensation_json"),
                ColumnSpec::text("evidence_json"),
                ColumnSpec::integer("started_at_ms"),
                ColumnSpec::integer("committed_at_ms"),
                ColumnSpec::text("adapter_id"),
                ColumnSpec::text("adapter_version"),
                ColumnSpec::text("compensation_status"),
                ColumnSpec::integer("compensation_started_at_ms"),
                ColumnSpec::integer("compensation_committed_at_ms"),
            ],
        ),
        TableSpec::new(
            "task_step_history_compensation_bindings",
            "task_id, attempt, ordinal",
            vec![
                ColumnSpec::text("task_id"),
                ColumnSpec::integer("attempt"),
                ColumnSpec::integer("ordinal"),
                ColumnSpec::text("mac"),
            ],
        ),
        TableSpec::new(
            "task_steps",
            "task_id, ordinal",
            vec![
                ColumnSpec::text("task_id"),
                ColumnSpec::integer("ordinal"),
                ColumnSpec::text("status"),
                ColumnSpec::text("idempotency_token"),
                ColumnSpec::text("compensation_json"),
                ColumnSpec::text("evidence_json"),
                ColumnSpec::integer("started_at_ms"),
                ColumnSpec::integer("committed_at_ms"),
                ColumnSpec::text("adapter_id"),
                ColumnSpec::text("adapter_version"),
                ColumnSpec::text("compensation_status"),
                ColumnSpec::integer("compensation_started_at_ms"),
                ColumnSpec::integer("compensation_committed_at_ms"),
            ],
        ),
        TableSpec::new(
            "tasks",
            "task_id",
            vec![
                ColumnSpec::text("task_id"),
                ColumnSpec::text("plan_id"),
                ColumnSpec::text("plan_digest"),
                ColumnSpec::text("operation"),
                ColumnSpec::text("idempotency_key"),
                ColumnSpec::text("status"),
                ColumnSpec::text("actor"),
                ColumnSpec::integer("created_at_ms"),
                ColumnSpec::integer("updated_at_ms"),
                ColumnSpec::integer("heartbeat_at_ms"),
                ColumnSpec::integer("progress"),
                ColumnSpec::integer("step_cursor"),
                ColumnSpec::text("adapter_evidence_json"),
                ColumnSpec::text("redacted_error_json"),
                ColumnSpec::text("rollback_status"),
                ColumnSpec::text("rollback_evidence_json"),
                ColumnSpec::integer("revision"),
                ColumnSpec::integer("event_sequence"),
                ColumnSpec::text("retry_idempotency_key"),
                ColumnSpec::text("owner_id"),
                ColumnSpec::integer("lease_expires_at_ms"),
                ColumnSpec::integer("attempt_count"),
                ColumnSpec::integer("finalization_failures"),
            ],
        ),
        TableSpec::new(
            "uninstall_journal",
            "task_id",
            vec![
                ColumnSpec::text("task_id"),
                ColumnSpec::text("managed_mcp_id"),
                ColumnSpec::text("version"),
                ColumnSpec::text("previous_state_json"),
                ColumnSpec::text("artifact_digest"),
                ColumnSpec::text("status"),
                ColumnSpec::integer("created_at_ms"),
                ColumnSpec::integer("updated_at_ms"),
            ],
        ),
    ]
}

impl SessionsSchemaShape {
    fn table_specs(self) -> Vec<TableSpec> {
        match self {
            Self::FreshBootstrap => fresh_session_table_specs(),
            Self::LegacyUpgrade => legacy_upgrade_session_table_specs(),
        }
    }
}

fn fresh_session_table_specs() -> Vec<TableSpec> {
    vec![
        TableSpec::new(
            "messages",
            "id",
            vec![
                ColumnSpec::integer("id"),
                ColumnSpec::text("message_id"),
                ColumnSpec::text("session_id"),
                ColumnSpec::text("role"),
                ColumnSpec::text("content_json"),
                ColumnSpec::integer("created_timestamp"),
                ColumnSpec::text_expr("timestamp", "CAST(timestamp AS TEXT)"),
                ColumnSpec::integer("tokens"),
                ColumnSpec::text("metadata_json"),
            ],
        ),
        TableSpec::new(
            "provider_inventory_entries",
            "inventory_key",
            vec![
                ColumnSpec::text("inventory_key"),
                ColumnSpec::text("provider_id"),
                ColumnSpec::text("provider_family"),
                ColumnSpec::text_expr("last_updated_at", "CAST(last_updated_at AS TEXT)"),
                ColumnSpec::text_expr(
                    "last_refresh_attempt_at",
                    "CAST(last_refresh_attempt_at AS TEXT)",
                ),
                ColumnSpec::text("last_refresh_error"),
                ColumnSpec::text_expr("created_at", "CAST(created_at AS TEXT)"),
                ColumnSpec::text_expr("updated_at", "CAST(updated_at AS TEXT)"),
            ],
        ),
        TableSpec::new(
            "provider_inventory_models",
            "inventory_key, ordinal",
            vec![
                ColumnSpec::text("inventory_key"),
                ColumnSpec::integer("ordinal"),
                ColumnSpec::text("model_id"),
                ColumnSpec::text("name"),
                ColumnSpec::text("family"),
                ColumnSpec::integer("context_limit"),
                ColumnSpec::integer_expr("reasoning", "CAST(reasoning AS INTEGER)"),
                ColumnSpec::integer_expr("recommended", "CAST(recommended AS INTEGER)"),
            ],
        ),
        TableSpec::new(
            "schema_version",
            "version",
            vec![
                ColumnSpec::integer("version"),
                ColumnSpec::text_expr("applied_at", "CAST(applied_at AS TEXT)"),
            ],
        ),
        TableSpec::new(
            "sessions",
            "id",
            vec![
                ColumnSpec::text("id"),
                ColumnSpec::text("name"),
                ColumnSpec::text("description"),
                ColumnSpec::integer_expr("user_set_name", "CAST(user_set_name AS INTEGER)"),
                ColumnSpec::text("session_type"),
                ColumnSpec::text("working_dir"),
                ColumnSpec::text_expr("created_at", "CAST(created_at AS TEXT)"),
                ColumnSpec::text_expr("updated_at", "CAST(updated_at AS TEXT)"),
                ColumnSpec::text("extension_data"),
                ColumnSpec::integer("total_tokens"),
                ColumnSpec::integer("input_tokens"),
                ColumnSpec::integer("output_tokens"),
                ColumnSpec::integer("cache_read_tokens"),
                ColumnSpec::integer("cache_write_tokens"),
                ColumnSpec::integer("accumulated_total_tokens"),
                ColumnSpec::integer("accumulated_input_tokens"),
                ColumnSpec::integer("accumulated_output_tokens"),
                ColumnSpec::integer("accumulated_cache_read_tokens"),
                ColumnSpec::integer("accumulated_cache_write_tokens"),
                ColumnSpec::text_expr(
                    "accumulated_cost",
                    "CASE WHEN accumulated_cost IS NULL THEN NULL ELSE printf('%.17g', accumulated_cost) END",
                ),
                ColumnSpec::text("schedule_id"),
                ColumnSpec::text("recipe_json"),
                ColumnSpec::text("user_recipe_values_json"),
                ColumnSpec::text("provider_name"),
                ColumnSpec::text("model_config_json"),
                ColumnSpec::text("lumina_mode"),
                ColumnSpec::text_expr("archived_at", "CAST(archived_at AS TEXT)"),
                ColumnSpec::text("project_id"),
                ColumnSpec::text("parent_session_id"),
            ],
        ),
        TableSpec::new(
            "usage_ledger",
            "id",
            vec![
                ColumnSpec::integer("id"),
                ColumnSpec::text("session_id"),
                ColumnSpec::integer("created_timestamp"),
                ColumnSpec::text("model"),
                ColumnSpec::integer("input_tokens"),
                ColumnSpec::integer("output_tokens"),
                ColumnSpec::integer("total_tokens"),
                ColumnSpec::integer("cache_read_tokens"),
                ColumnSpec::integer("cache_write_tokens"),
                ColumnSpec::text_expr(
                    "cost",
                    "CASE WHEN cost IS NULL THEN NULL ELSE printf('%.17g', cost) END",
                ),
                ColumnSpec::text("cost_source"),
                ColumnSpec::integer_expr("is_compaction", "CAST(is_compaction AS INTEGER)"),
            ],
        ),
    ]
}

fn legacy_upgrade_session_table_specs() -> Vec<TableSpec> {
    vec![
        TableSpec::new(
            "messages",
            "id",
            vec![
                ColumnSpec::integer("id"),
                ColumnSpec::text("session_id"),
                ColumnSpec::text("role"),
                ColumnSpec::text("content_json"),
                ColumnSpec::integer("created_timestamp"),
                ColumnSpec::text_expr("timestamp", "CAST(timestamp AS TEXT)"),
                ColumnSpec::integer("tokens"),
                ColumnSpec::text("metadata_json"),
                ColumnSpec::text("message_id"),
            ],
        ),
        TableSpec::new(
            "provider_inventory_entries",
            "inventory_key",
            vec![
                ColumnSpec::text("inventory_key"),
                ColumnSpec::text("provider_id"),
                ColumnSpec::text("provider_family"),
                ColumnSpec::text_expr("last_updated_at", "CAST(last_updated_at AS TEXT)"),
                ColumnSpec::text_expr(
                    "last_refresh_attempt_at",
                    "CAST(last_refresh_attempt_at AS TEXT)",
                ),
                ColumnSpec::text("last_refresh_error"),
                ColumnSpec::text_expr("created_at", "CAST(created_at AS TEXT)"),
                ColumnSpec::text_expr("updated_at", "CAST(updated_at AS TEXT)"),
            ],
        ),
        TableSpec::new(
            "provider_inventory_models",
            "inventory_key, ordinal",
            vec![
                ColumnSpec::text("inventory_key"),
                ColumnSpec::integer("ordinal"),
                ColumnSpec::text("model_id"),
                ColumnSpec::text("name"),
                ColumnSpec::text("family"),
                ColumnSpec::integer("context_limit"),
                ColumnSpec::integer_expr("reasoning", "CAST(reasoning AS INTEGER)"),
                ColumnSpec::integer_expr("recommended", "CAST(recommended AS INTEGER)"),
            ],
        ),
        TableSpec::new(
            "schema_version",
            "version",
            vec![
                ColumnSpec::integer("version"),
                ColumnSpec::text_expr("applied_at", "CAST(applied_at AS TEXT)"),
            ],
        ),
        TableSpec::new(
            "sessions",
            "id",
            vec![
                ColumnSpec::text("id"),
                ColumnSpec::text("name"),
                ColumnSpec::text("description"),
                ColumnSpec::integer_expr("user_set_name", "CAST(user_set_name AS INTEGER)"),
                ColumnSpec::text("session_type"),
                ColumnSpec::text("working_dir"),
                ColumnSpec::text_expr("created_at", "CAST(created_at AS TEXT)"),
                ColumnSpec::text_expr("updated_at", "CAST(updated_at AS TEXT)"),
                ColumnSpec::text("extension_data"),
                ColumnSpec::integer("total_tokens"),
                ColumnSpec::integer("input_tokens"),
                ColumnSpec::integer("output_tokens"),
                ColumnSpec::integer("accumulated_total_tokens"),
                ColumnSpec::integer("accumulated_input_tokens"),
                ColumnSpec::integer("accumulated_output_tokens"),
                ColumnSpec::text("schedule_id"),
                ColumnSpec::text("recipe_json"),
                ColumnSpec::text("user_recipe_values_json"),
                ColumnSpec::text("provider_name"),
                ColumnSpec::text("model_config_json"),
                ColumnSpec::text("lumina_mode"),
                ColumnSpec::text("thread_id"),
                ColumnSpec::text_expr("archived_at", "CAST(archived_at AS TEXT)"),
                ColumnSpec::text("project_id"),
                ColumnSpec::text_expr(
                    "accumulated_cost",
                    "CASE WHEN accumulated_cost IS NULL THEN NULL ELSE printf('%.17g', accumulated_cost) END",
                ),
                ColumnSpec::integer("cache_read_tokens"),
                ColumnSpec::integer("cache_write_tokens"),
                ColumnSpec::integer("accumulated_cache_read_tokens"),
                ColumnSpec::integer("accumulated_cache_write_tokens"),
                ColumnSpec::text("parent_session_id"),
            ],
        ),
        TableSpec::new(
            "thread_messages",
            "id",
            vec![
                ColumnSpec::integer("id"),
                ColumnSpec::text("thread_id"),
                ColumnSpec::text("session_id"),
                ColumnSpec::text("message_id"),
                ColumnSpec::text("role"),
                ColumnSpec::text("content_json"),
                ColumnSpec::integer("created_timestamp"),
                ColumnSpec::text("metadata_json"),
            ],
        ),
        TableSpec::new(
            "threads",
            "id",
            vec![
                ColumnSpec::text("id"),
                ColumnSpec::text("name"),
                ColumnSpec::integer_expr("user_set_name", "CAST(user_set_name AS INTEGER)"),
                ColumnSpec::text("working_dir"),
                ColumnSpec::text_expr("created_at", "CAST(created_at AS TEXT)"),
                ColumnSpec::text_expr("updated_at", "CAST(updated_at AS TEXT)"),
                ColumnSpec::text_expr("archived_at", "CAST(archived_at AS TEXT)"),
                ColumnSpec::text("metadata_json"),
            ],
        ),
        TableSpec::new(
            "usage_ledger",
            "id",
            vec![
                ColumnSpec::integer("id"),
                ColumnSpec::text("session_id"),
                ColumnSpec::integer("created_timestamp"),
                ColumnSpec::text("model"),
                ColumnSpec::integer("input_tokens"),
                ColumnSpec::integer("output_tokens"),
                ColumnSpec::integer("total_tokens"),
                ColumnSpec::integer("cache_read_tokens"),
                ColumnSpec::integer("cache_write_tokens"),
                ColumnSpec::text_expr(
                    "cost",
                    "CASE WHEN cost IS NULL THEN NULL ELSE printf('%.17g', cost) END",
                ),
                ColumnSpec::text("cost_source"),
                ColumnSpec::integer_expr("is_compaction", "CAST(is_compaction AS INTEGER)"),
            ],
        ),
    ]
}

struct UnusedScheduler;

#[async_trait]
impl SchedulerTrait for UnusedScheduler {
    async fn add_scheduled_job(
        &self,
        _job: ScheduledJob,
        _copy_recipe: bool,
    ) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn add_scheduled_job_from_content(
        &self,
        _job: ScheduledJob,
        _recipe_content: &[u8],
    ) -> Result<ScheduledJob, SchedulerError> {
        unreachable!()
    }

    async fn schedule_recipe(
        &self,
        _recipe_path: PathBuf,
        _cron_schedule: Option<String>,
    ) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn list_scheduled_jobs(&self) -> Vec<ScheduledJob> {
        Vec::new()
    }

    async fn remove_scheduled_job(
        &self,
        _id: &str,
        _remove_recipe: bool,
    ) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn pause_schedule(&self, _id: &str) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn unpause_schedule(&self, _id: &str) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn run_now(&self, _id: &str) -> Result<String, SchedulerError> {
        unreachable!()
    }

    async fn sessions(
        &self,
        _sched_id: &str,
        _limit: usize,
    ) -> Result<Vec<(String, Session)>, SchedulerError> {
        unreachable!()
    }

    async fn update_schedule(
        &self,
        _sched_id: &str,
        _new_cron: String,
    ) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn kill_running_job(&self, _sched_id: &str) -> Result<(), SchedulerError> {
        unreachable!()
    }

    async fn get_running_job_info(
        &self,
        _sched_id: &str,
    ) -> Result<Option<(String, chrono::DateTime<chrono::Utc>)>, SchedulerError> {
        unreachable!()
    }
}

async fn prepare_profile_application(
    cx: &ConnectionTo<Agent>,
    data_dir: &std::path::Path,
    digest: &str,
) -> PreparedProfileApplication {
    let created = cx
        .send_request(McpPlanCreateRequest {
            intent: lumina::custom_requests::McpPlanIntent::Register {
                manifest_digest: digest.to_string(),
                installation_scope: Some(lumina::custom_requests::McpInstallationScope::User),
            },
            idempotency_key: "profile-register".to_string(),
        })
        .block_task()
        .await
        .unwrap();
    let McpPlanCreateResponse {
        outcome: McpPlatformOutcome::Success { value: plan },
    } = created
    else {
        panic!("plan creation must succeed");
    };

    let install = cx
        .send_request(McpInstallConfirmRequest {
            plan_id: plan.plan_id.clone(),
            plan_digest: plan.plan_digest.clone(),
            user_decision: McpUserDecision::Confirm,
            idempotency_key: "profile-install".to_string(),
        })
        .block_task()
        .await
        .unwrap();
    let McpInstallConfirmResponse {
        outcome: McpPlatformOutcome::Success {
            value: install_task,
        },
    } = install
    else {
        panic!("install confirm must succeed");
    };
    let install_task = wait_for_terminal_task(cx, &install_task.task_id).await;
    assert_eq!(install_task.status, McpTaskStatus::Succeeded);

    let managed = cx
        .send_request(McpListRequest {
            cursor: None,
            page_size: Some(10),
            registration: None,
            installation: None,
            runtime: None,
            health: None,
            default_enabled: None,
        })
        .block_task()
        .await
        .unwrap();
    let McpListResponse {
        outcome: McpPlatformOutcome::Success { value: managed },
    } = managed
    else {
        panic!("managed list must succeed");
    };
    let managed_id = managed
        .items
        .first()
        .expect("registered MCP should exist")
        .managed_mcp_id
        .clone();

    let health = cx
        .send_request(McpHealthRunRequest {
            managed_mcp_id: managed_id.clone(),
            mode: McpHealthCheckMode::Registration,
            idempotency_key: "profile-health".to_string(),
        })
        .block_task()
        .await
        .unwrap();
    let McpHealthRunResponse {
        outcome: McpPlatformOutcome::Success { value: health_task },
    } = health
    else {
        panic!("health run must succeed");
    };
    let health_task = wait_for_terminal_task(cx, &health_task.task_id).await;
    assert_eq!(health_task.status, McpTaskStatus::Succeeded);

    let managed = cx
        .send_request(McpGetRequest {
            managed_mcp_id: managed_id.clone(),
        })
        .block_task()
        .await
        .unwrap();
    let McpGetResponse {
        outcome: McpPlatformOutcome::Success { value: managed },
    } = managed
    else {
        panic!("managed get must succeed");
    };
    assert_eq!(
        managed.summary.health,
        lumina::custom_requests::McpHealthState::Healthy
    );

    let profile = cx
        .send_request(McpProfileCreateRequest {
            name: "Authenticated profile".to_string(),
            description: String::new(),
            managed_mcp_ids: vec![managed_id],
            idempotency_key: "profile-create".to_string(),
        })
        .block_task()
        .await
        .unwrap();
    let McpProfileCreateResponse {
        outcome: McpPlatformOutcome::Success { value: profile },
    } = profile
    else {
        panic!("profile create must succeed");
    };

    let apply_plan = cx
        .send_request(McpProfileApplyPlanCreateRequest {
            profile_id: profile.profile_id.clone(),
            profile_revision: profile.revision,
            idempotency_key: "profile-apply-plan".to_string(),
        })
        .block_task()
        .await
        .unwrap();
    let McpProfileApplyPlanCreateResponse {
        outcome: McpPlatformOutcome::Success { value: apply_plan },
    } = apply_plan
    else {
        panic!("profile apply plan must succeed");
    };

    let confirmed = cx
        .send_request(McpProfileApplyConfirmRequest {
            plan_id: apply_plan.plan_id.clone(),
            confirmation_token: apply_plan.confirmation.confirmation_token.clone(),
            confirm: true,
        })
        .block_task()
        .await
        .unwrap();
    let McpProfileApplyConfirmResponse {
        outcome: McpPlatformOutcome::Success { value: token },
    } = confirmed
    else {
        panic!("profile apply confirm must succeed");
    };

    let pool = open_pool(&database_path(data_dir)).await;
    let application_id = sqlx::query_scalar::<_, String>(
        "SELECT application_id FROM mcp_profile_application_tokens WHERE token_hash=?",
    )
    .bind(sha256_hex(&token.token))
    .fetch_one(&pool)
    .await
    .unwrap();
    pool.close().await;

    PreparedProfileApplication {
        token: token.token,
        application_id,
    }
}

fn assert_platform_database_path(data_dir: &Path) -> PathBuf {
    let path = database_path(data_dir);
    assert!(path.starts_with(data_dir));
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("platform.db")
    );
    path
}

fn assert_sessions_database_path(data_dir: &Path) -> PathBuf {
    let path = sessions_database_path(data_dir);
    assert!(path.starts_with(data_dir));
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("sessions.db")
    );
    path
}

async fn profile_application_record(
    data_dir: &Path,
    application_id: &str,
) -> ProfileApplicationRecord {
    let pool = open_pool(&assert_platform_database_path(data_dir)).await;
    let row = sqlx::query(
        "SELECT status, session_id, failure_code FROM mcp_profile_applications WHERE application_id=?",
    )
    .bind(application_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let events = sqlx::query(
        "SELECT event_type, detail_code FROM mcp_profile_application_events WHERE application_id=? ORDER BY event_id",
    )
    .bind(application_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let record = ProfileApplicationRecord {
        status: row.try_get("status").unwrap(),
        session_id: row.try_get("session_id").unwrap(),
        failure_code: row.try_get("failure_code").unwrap(),
        events: events
            .into_iter()
            .map(|event| ProfileApplicationEvent {
                event_type: event.try_get("event_type").unwrap(),
                detail_code: event.try_get("detail_code").unwrap(),
            })
            .collect(),
    };
    pool.close().await;
    record
}

async fn database_table_names(pool: &sqlx::Pool<sqlx::Sqlite>) -> Vec<String> {
    let mut table_names = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    table_names.sort();
    table_names
}

fn expected_table_names(specs: &[TableSpec]) -> Vec<String> {
    let mut expected: Vec<_> = specs
        .iter()
        .map(|spec| spec.table_name.to_string())
        .collect();
    expected.sort();
    expected
}

async fn detect_sessions_schema_shape(pool: &sqlx::Pool<sqlx::Sqlite>) -> SessionsSchemaShape {
    let actual = database_table_names(pool).await;
    let matches = [
        SessionsSchemaShape::FreshBootstrap,
        SessionsSchemaShape::LegacyUpgrade,
    ]
    .into_iter()
    .filter(|shape| actual == expected_table_names(&shape.table_specs()))
    .collect::<Vec<_>>();
    match matches.as_slice() {
        [shape] => *shape,
        [] => panic!(
            "sessions.db table set changed; expected one of the explicit schema shapes, actual={actual:?}"
        ),
        _ => panic!("sessions.db matched multiple schema shapes"),
    }
}

async fn assert_database_tables(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    specs: &[TableSpec],
    database_name: &str,
) {
    let expected = expected_table_names(specs);
    let actual = database_table_names(pool).await;
    assert_eq!(
        actual, expected,
        "{database_name} table set changed; update ExactPersistedFootprint coverage"
    );
}

async fn assert_table_schema(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    spec: &TableSpec,
    database_name: &str,
) {
    let pragma = format!("PRAGMA table_info({})", spec.table_name);
    let actual: Vec<String> = sqlx::query(&pragma)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get("name").unwrap())
        .collect();
    assert_eq!(
        actual,
        spec.expected_columns(),
        "{database_name}.{} column set changed; update ExactPersistedFootprint coverage",
        spec.table_name
    );
}

fn snapshot_value(row: &sqlx::sqlite::SqliteRow, column: &ColumnSpec) -> SnapshotValue {
    match column.kind {
        SnapshotValueKind::Integer => row
            .try_get::<Option<i64>, _>(column.name)
            .unwrap()
            .map_or(SnapshotValue::Null, SnapshotValue::Integer),
        SnapshotValueKind::Text => row
            .try_get::<Option<String>, _>(column.name)
            .unwrap()
            .map_or(SnapshotValue::Null, SnapshotValue::Text),
    }
}

async fn snapshot_table(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    spec: &TableSpec,
    database_name: &str,
) -> TableSnapshot {
    assert_table_schema(pool, spec, database_name).await;
    let select_list = spec
        .columns
        .iter()
        .map(|column| format!("{} AS {}", column.expr, column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT {select_list} FROM {} ORDER BY {}",
        spec.table_name, spec.order_by
    );
    let rows = sqlx::query(&query)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            spec.columns
                .iter()
                .map(|column| snapshot_value(&row, column))
                .collect()
        })
        .collect();
    TableSnapshot {
        table_name: spec.table_name,
        columns: spec.expected_columns(),
        rows,
    }
}

async fn snapshot_database(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    specs: &[TableSpec],
    database_name: &str,
) -> DatabaseSideEffectSnapshot {
    assert_database_tables(pool, specs, database_name).await;
    let mut tables = Vec::with_capacity(specs.len());
    for spec in specs {
        tables.push(snapshot_table(pool, spec, database_name).await);
    }
    DatabaseSideEffectSnapshot { tables }
}

async fn platform_side_effect_snapshot(data_dir: &Path) -> PlatformSideEffectSnapshot {
    let pool = open_pool(&assert_platform_database_path(data_dir)).await;
    let specs = platform_table_specs();
    let snapshot = snapshot_database(&pool, &specs, "platform.db").await;
    pool.close().await;
    snapshot
}

async fn sessions_side_effect_snapshot(data_dir: &Path) -> SessionSideEffectSnapshot {
    let pool = open_pool(&assert_sessions_database_path(data_dir)).await;
    let shape = detect_sessions_schema_shape(&pool).await;
    let specs = shape.table_specs();
    let snapshot = snapshot_database(&pool, &specs, "sessions.db").await;
    pool.close().await;
    SessionSideEffectSnapshot {
        shape,
        database: snapshot,
    }
}

async fn exact_persisted_footprint_snapshot(data_dir: &Path) -> ExactPersistedFootprint {
    let (platform, sessions) = tokio::join!(
        platform_side_effect_snapshot(data_dir),
        sessions_side_effect_snapshot(data_dir)
    );
    ExactPersistedFootprint { platform, sessions }
}

fn assert_exact_persisted_footprint_unchanged(
    before: &ExactPersistedFootprint,
    after: &ExactPersistedFootprint,
) {
    assert_eq!(after, before);
}

fn assert_table_set_unchanged_except(
    before: &DatabaseSideEffectSnapshot,
    after: &DatabaseSideEffectSnapshot,
    allowed_changed_tables: &[&str],
) {
    for before_table in &before.tables {
        if allowed_changed_tables.contains(&before_table.table_name) {
            continue;
        }
        assert_eq!(
            after.table(before_table.table_name),
            before_table,
            "unexpected change in table {}",
            before_table.table_name
        );
    }
}

fn assert_session_delete_delta(
    before: &SessionSideEffectSnapshot,
    after: &SessionSideEffectSnapshot,
    session_id: &str,
) {
    assert_eq!(after.shape, before.shape);
    let before_sessions = before
        .table("sessions")
        .rows_with_text_value("id", session_id);
    let after_sessions = after
        .table("sessions")
        .rows_with_text_value("id", session_id);
    let before_messages = before
        .table("messages")
        .rows_with_text_value("session_id", session_id);
    let after_messages = after
        .table("messages")
        .rows_with_text_value("session_id", session_id);
    let before_usage = before
        .table("usage_ledger")
        .rows_with_text_value("session_id", session_id);
    let after_usage = after
        .table("usage_ledger")
        .rows_with_text_value("session_id", session_id);

    assert!(
        !before_sessions.is_empty(),
        "expected seeded session row for {session_id}"
    );
    assert!(
        !before_messages.is_empty(),
        "expected seeded message rows for {session_id}"
    );
    assert!(
        !before_usage.is_empty(),
        "expected seeded usage rows for {session_id}"
    );
    assert!(after_sessions.is_empty(), "session row must be deleted");
    assert!(after_messages.is_empty(), "message rows must be deleted");
    assert!(after_usage.is_empty(), "usage rows must be deleted");

    let legacy_thread_ids = if before.shape == SessionsSchemaShape::LegacyUpgrade {
        let mut thread_ids = BTreeSet::new();
        let session_thread_id_index = before.table("sessions").column_index("thread_id");
        for row in before
            .table("sessions")
            .rows_with_text_value("id", session_id)
        {
            if let Some(SnapshotValue::Text(thread_id)) = row.get(session_thread_id_index) {
                thread_ids.insert(thread_id.clone());
            }
        }
        for row in before
            .table("thread_messages")
            .rows_with_text_value("session_id", session_id)
        {
            let thread_id_index = before.table("thread_messages").column_index("thread_id");
            if let Some(SnapshotValue::Text(thread_id)) = row.get(thread_id_index) {
                thread_ids.insert(thread_id.clone());
            }
        }
        thread_ids
    } else {
        BTreeSet::new()
    };

    let mut expected_after = before.clone();
    expected_after
        .table_mut("sessions")
        .remove_rows_with_text_value("id", session_id);
    expected_after
        .table_mut("messages")
        .remove_rows_with_text_value("session_id", session_id);
    expected_after
        .table_mut("usage_ledger")
        .remove_rows_with_text_value("session_id", session_id);

    if before.shape == SessionsSchemaShape::LegacyUpgrade {
        expected_after
            .table_mut("thread_messages")
            .remove_rows_with_text_value("session_id", session_id);

        for thread_id in legacy_thread_ids {
            let remaining_session_refs = expected_after
                .table("sessions")
                .rows_with_text_value("thread_id", &thread_id);
            let remaining_thread_message_refs = expected_after
                .table("thread_messages")
                .rows_with_text_value("thread_id", &thread_id);
            if remaining_session_refs.is_empty() && remaining_thread_message_refs.is_empty() {
                expected_after
                    .table_mut("threads")
                    .remove_rows_with_text_value("id", &thread_id);
            }
        }
    }

    assert_eq!(after, &expected_after);
}

fn assert_profile_application_contract(
    record: &ProfileApplicationRecord,
    expected_status: &str,
    expected_session_id: Option<&str>,
    expected_failure_code: Option<&str>,
    expected_events: &[(&str, &str)],
) {
    assert_eq!(record.status, expected_status);
    assert_eq!(record.session_id.as_deref(), expected_session_id);
    assert_eq!(record.failure_code.as_deref(), expected_failure_code);
    assert_eq!(
        record
            .events
            .iter()
            .map(|event| (event.event_type.as_str(), event.detail_code.as_str()))
            .collect::<Vec<_>>(),
        expected_events
    );
}

fn assert_profile_application_delete_platform_delta(
    before: &PlatformSideEffectSnapshot,
    after: &PlatformSideEffectSnapshot,
    application_id: &str,
) {
    assert_table_set_unchanged_except(
        before,
        after,
        &["mcp_profile_applications", "mcp_profile_application_events"],
    );

    let applications_before = before.table("mcp_profile_applications");
    let applications_after = after.table("mcp_profile_applications");
    let before_row = applications_before.row_with_text_value("application_id", application_id);
    let after_row = applications_after.row_with_text_value("application_id", application_id);

    let status_index = applications_before.column_index("status");
    let updated_at_index = applications_before.column_index("updated_at_ms");
    let failure_code_index = applications_before.column_index("failure_code");

    assert_eq!(
        before_row[status_index],
        SnapshotValue::Text("created".to_string())
    );
    assert_eq!(
        after_row[status_index],
        SnapshotValue::Text("deleted".to_string())
    );
    assert_eq!(before_row[failure_code_index], SnapshotValue::Null);
    assert_eq!(after_row[failure_code_index], SnapshotValue::Null);

    let mut expected_after_row = before_row.clone();
    expected_after_row[status_index] = SnapshotValue::Text("deleted".to_string());
    expected_after_row[updated_at_index] = after_row[updated_at_index].clone();
    assert_eq!(after_row, expected_after_row);

    let SnapshotValue::Integer(before_updated_at) = before_row[updated_at_index] else {
        panic!("mcp_profile_applications.updated_at_ms must be integer");
    };
    let SnapshotValue::Integer(after_updated_at) = after_row[updated_at_index] else {
        panic!("mcp_profile_applications.updated_at_ms must be integer");
    };
    assert!(
        after_updated_at >= before_updated_at,
        "application updated_at_ms must move forward"
    );

    let events_before = before
        .table("mcp_profile_application_events")
        .rows_with_text_value("application_id", application_id);
    let events_after = after
        .table("mcp_profile_application_events")
        .rows_with_text_value("application_id", application_id);
    assert_eq!(events_after.len(), events_before.len() + 2);
    assert_eq!(
        &events_after[..events_before.len()],
        events_before.as_slice()
    );

    let actor = match &before_row[applications_before.column_index("actor")] {
        SnapshotValue::Text(value) => value.clone(),
        _ => panic!("mcp_profile_applications.actor must be text"),
    };
    assert_profile_application_event_row(
        &after.table("mcp_profile_application_events").columns,
        &events_after[events_before.len()],
        application_id,
        "recovery_required",
        &actor,
        "session_delete_started",
    );
    assert_profile_application_event_row(
        &after.table("mcp_profile_application_events").columns,
        &events_after[events_before.len() + 1],
        application_id,
        "session_deleted",
        &actor,
        "session_deleted",
    );
}

fn assert_profile_application_event_row(
    columns: &[&'static str],
    row: &[SnapshotValue],
    application_id: &str,
    event_type: &str,
    actor: &str,
    detail_code: &str,
) {
    let column_index = |column_name: &str| {
        columns
            .iter()
            .position(|column| *column == column_name)
            .unwrap_or_else(|| panic!("missing event column {column_name}"))
    };
    assert_eq!(
        row[column_index("application_id")],
        SnapshotValue::Text(application_id.to_string())
    );
    assert_eq!(
        row[column_index("event_type")],
        SnapshotValue::Text(event_type.to_string())
    );
    assert_eq!(
        row[column_index("actor")],
        SnapshotValue::Text(actor.to_string())
    );
    assert_eq!(
        row[column_index("detail_code")],
        SnapshotValue::Text(detail_code.to_string())
    );
    match row[column_index("event_id")] {
        SnapshotValue::Integer(_) => {}
        _ => panic!("mcp_profile_application_events.event_id must be integer"),
    }
    match row[column_index("occurred_at_ms")] {
        SnapshotValue::Integer(_) => {}
        _ => panic!("mcp_profile_application_events.occurred_at_ms must be integer"),
    }
}

async fn session_exists(data_dir: &Path, session_id: &str) -> bool {
    let pool = open_pool(&assert_sessions_database_path(data_dir)).await;
    let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)")
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    exists
}

async fn delete_persisted_session(data_dir: &Path, session_id: &str) {
    let pool = open_pool(&assert_sessions_database_path(data_dir)).await;
    sqlx::query("DELETE FROM messages WHERE session_id=?")
        .bind(session_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM usage_ledger WHERE session_id=?")
        .bind(session_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM sessions WHERE id=?")
        .bind(session_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

fn legacy_v9_fixture_columns(table_name: &str) -> Vec<LegacyFixtureColumnSpec> {
    match table_name {
        "schema_version" => vec![
            LegacyFixtureColumnSpec::new("version", "INTEGER", false, None, 1),
            LegacyFixtureColumnSpec::new(
                "applied_at",
                "TIMESTAMP",
                false,
                Some("CURRENT_TIMESTAMP"),
                0,
            ),
        ],
        "sessions" => vec![
            LegacyFixtureColumnSpec::new("id", "TEXT", false, None, 1),
            LegacyFixtureColumnSpec::new("name", "TEXT", true, Some("''"), 0),
            LegacyFixtureColumnSpec::new("description", "TEXT", true, Some("''"), 0),
            LegacyFixtureColumnSpec::new("user_set_name", "BOOLEAN", false, Some("FALSE"), 0),
            LegacyFixtureColumnSpec::new("session_type", "TEXT", true, Some("'user'"), 0),
            LegacyFixtureColumnSpec::new("working_dir", "TEXT", true, None, 0),
            LegacyFixtureColumnSpec::new(
                "created_at",
                "TIMESTAMP",
                false,
                Some("CURRENT_TIMESTAMP"),
                0,
            ),
            LegacyFixtureColumnSpec::new(
                "updated_at",
                "TIMESTAMP",
                false,
                Some("CURRENT_TIMESTAMP"),
                0,
            ),
            LegacyFixtureColumnSpec::new("extension_data", "TEXT", false, Some("'{}'"), 0),
            LegacyFixtureColumnSpec::new("total_tokens", "INTEGER", false, None, 0),
            LegacyFixtureColumnSpec::new("input_tokens", "INTEGER", false, None, 0),
            LegacyFixtureColumnSpec::new("output_tokens", "INTEGER", false, None, 0),
            LegacyFixtureColumnSpec::new("accumulated_total_tokens", "INTEGER", false, None, 0),
            LegacyFixtureColumnSpec::new("accumulated_input_tokens", "INTEGER", false, None, 0),
            LegacyFixtureColumnSpec::new("accumulated_output_tokens", "INTEGER", false, None, 0),
            LegacyFixtureColumnSpec::new("schedule_id", "TEXT", false, None, 0),
            LegacyFixtureColumnSpec::new("recipe_json", "TEXT", false, None, 0),
            LegacyFixtureColumnSpec::new("user_recipe_values_json", "TEXT", false, None, 0),
            LegacyFixtureColumnSpec::new("provider_name", "TEXT", false, None, 0),
            LegacyFixtureColumnSpec::new("model_config_json", "TEXT", false, None, 0),
            LegacyFixtureColumnSpec::new("lumina_mode", "TEXT", true, Some("'auto'"), 0),
        ],
        "messages" => vec![
            LegacyFixtureColumnSpec::new("id", "INTEGER", false, None, 1),
            LegacyFixtureColumnSpec::new("session_id", "TEXT", true, None, 0),
            LegacyFixtureColumnSpec::new("role", "TEXT", true, None, 0),
            LegacyFixtureColumnSpec::new("content_json", "TEXT", true, None, 0),
            LegacyFixtureColumnSpec::new("created_timestamp", "INTEGER", true, None, 0),
            LegacyFixtureColumnSpec::new(
                "timestamp",
                "TIMESTAMP",
                false,
                Some("CURRENT_TIMESTAMP"),
                0,
            ),
            LegacyFixtureColumnSpec::new("tokens", "INTEGER", false, None, 0),
            LegacyFixtureColumnSpec::new("metadata_json", "TEXT", false, None, 0),
            LegacyFixtureColumnSpec::new("message_id", "TEXT", false, None, 0),
        ],
        _ => panic!("missing legacy fixture column contract for {table_name}"),
    }
}

fn legacy_v9_fixture_indexes(table_name: &str) -> Vec<LegacyFixtureIndexSpec> {
    match table_name {
        "schema_version" => Vec::new(),
        "sessions" => vec![
            LegacyFixtureIndexSpec::new(
                "idx_sessions_type",
                false,
                "c",
                false,
                &["session_type"],
                Some("CREATE INDEX idx_sessions_type ON sessions(session_type)"),
            ),
            LegacyFixtureIndexSpec::new(
                "idx_sessions_updated",
                false,
                "c",
                false,
                &["updated_at"],
                Some("CREATE INDEX idx_sessions_updated ON sessions(updated_at DESC)"),
            ),
            LegacyFixtureIndexSpec::new(
                "sqlite_autoindex_sessions_1",
                true,
                "pk",
                false,
                &["id"],
                None,
            ),
        ],
        "messages" => vec![
            LegacyFixtureIndexSpec::new(
                "idx_messages_message_id",
                false,
                "c",
                false,
                &["message_id"],
                Some("CREATE INDEX idx_messages_message_id ON messages(message_id)"),
            ),
            LegacyFixtureIndexSpec::new(
                "idx_messages_session",
                false,
                "c",
                false,
                &["session_id"],
                Some("CREATE INDEX idx_messages_session ON messages(session_id)"),
            ),
            LegacyFixtureIndexSpec::new(
                "idx_messages_timestamp",
                false,
                "c",
                false,
                &["timestamp"],
                Some("CREATE INDEX idx_messages_timestamp ON messages(timestamp)"),
            ),
        ],
        _ => panic!("missing legacy fixture index contract for {table_name}"),
    }
}

async fn assert_legacy_fixture_table_contract(pool: &sqlx::Pool<sqlx::Sqlite>, table_name: &str) {
    let pragma = format!("PRAGMA table_info({table_name})");
    let actual_columns: Vec<(String, String, bool, Option<String>, i64)> = sqlx::query(&pragma)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get("name").unwrap(),
                row.try_get("type").unwrap(),
                row.try_get::<i64, _>("notnull").unwrap() != 0,
                row.try_get("dflt_value").unwrap(),
                row.try_get("pk").unwrap(),
            )
        })
        .collect();
    let expected_columns: Vec<(String, String, bool, Option<String>, i64)> =
        legacy_v9_fixture_columns(table_name)
            .into_iter()
            .map(|column| {
                (
                    column.name.to_string(),
                    column.declared_type.to_string(),
                    column.not_null,
                    column.default_sql.map(str::to_string),
                    column.pk,
                )
            })
            .collect();
    assert_eq!(
        actual_columns, expected_columns,
        "legacy fixture metadata drifted for table {table_name}"
    );

    let index_list = format!("PRAGMA index_list({table_name})");
    let mut actual_indexes: Vec<(String, bool, String, bool, Vec<String>, Option<String>)> =
        Vec::new();
    for row in sqlx::query(&index_list).fetch_all(pool).await.unwrap() {
        let index_name: String = row.try_get("name").unwrap();
        let index_info = format!("PRAGMA index_info({index_name})");
        let mut columns: Vec<(i64, String)> = sqlx::query(&index_info)
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|index_row| {
                (
                    index_row.try_get("seqno").unwrap(),
                    index_row.try_get("name").unwrap(),
                )
            })
            .collect();
        columns.sort_by_key(|(seqno, _)| *seqno);
        let create_sql = sqlx::query_scalar::<_, Option<String>>(
            "SELECT sql FROM sqlite_master WHERE type='index' AND name=?",
        )
        .bind(&index_name)
        .fetch_one(pool)
        .await
        .unwrap();
        actual_indexes.push((
            index_name,
            row.try_get::<i64, _>("unique").unwrap() != 0,
            row.try_get::<String, _>("origin").unwrap(),
            row.try_get::<i64, _>("partial").unwrap() != 0,
            columns.into_iter().map(|(_, name)| name).collect(),
            create_sql,
        ));
    }
    actual_indexes.sort_by(|left, right| left.0.cmp(&right.0));

    let mut expected_indexes: Vec<(String, bool, String, bool, Vec<String>, Option<String>)> =
        legacy_v9_fixture_indexes(table_name)
            .into_iter()
            .map(|index| {
                (
                    index.name.to_string(),
                    index.unique,
                    index.origin.to_string(),
                    index.partial,
                    index
                        .columns
                        .iter()
                        .map(|column| (*column).to_string())
                        .collect(),
                    index.create_sql.map(str::to_string),
                )
            })
            .collect();
    expected_indexes.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        actual_indexes, expected_indexes,
        "legacy fixture index contract drifted for table {table_name}"
    );
}

async fn assert_legacy_upgrade_session_fixture_contract(pool: &sqlx::Pool<sqlx::Sqlite>) {
    for table_name in ["schema_version", "sessions", "messages"] {
        assert_legacy_fixture_table_contract(pool, table_name).await;
    }
}

async fn seed_session_messages_and_usage(data_dir: &Path, session_id: &str) {
    let pool = open_pool(&assert_sessions_database_path(data_dir)).await;
    sqlx::query(
        "INSERT INTO messages(message_id, session_id, role, content_json, created_timestamp, timestamp, tokens, metadata_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(format!("seed-message-{session_id}"))
    .bind(session_id)
    .bind("assistant")
    .bind(r#"[{"type":"text","text":"seeded lifecycle delete row"}]"#)
    .bind(1_700_000_000_001_i64)
    .bind("2026-01-01 00:00:00")
    .bind(7_i64)
    .bind(r#"{"seed":"profile-lifecycle-delete"}"#)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_ledger(session_id, created_timestamp, model, input_tokens, output_tokens, total_tokens, cache_read_tokens, cache_write_tokens, cost, cost_source, is_compaction) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(1_700_000_000_002_i64)
    .bind("seed-model")
    .bind(3_i64)
    .bind(4_i64)
    .bind(7_i64)
    .bind(1_i64)
    .bind(2_i64)
    .bind(0.25_f64)
    .bind("seed-fixture")
    .bind(0_i64)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
}

async fn create_legacy_upgrade_session_fixture(data_dir: &Path) {
    let path = assert_sessions_database_path(data_dir);
    let pool = open_pool_with_create(&path, true).await;
    sqlx::query(
        r#"
        CREATE TABLE schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )
    "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO schema_version(version, applied_at) VALUES (?, ?)")
        .bind(9_i64)
        .bind("2026-01-01 00:00:00")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL DEFAULT '',
            description TEXT NOT NULL DEFAULT '',
            user_set_name BOOLEAN DEFAULT FALSE,
            session_type TEXT NOT NULL DEFAULT 'user',
            working_dir TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            extension_data TEXT DEFAULT '{}',
            total_tokens INTEGER,
            input_tokens INTEGER,
            output_tokens INTEGER,
            accumulated_total_tokens INTEGER,
            accumulated_input_tokens INTEGER,
            accumulated_output_tokens INTEGER,
            schedule_id TEXT,
            recipe_json TEXT,
            user_recipe_values_json TEXT,
            provider_name TEXT,
            model_config_json TEXT,
            lumina_mode TEXT NOT NULL DEFAULT 'auto'
        )
    "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            role TEXT NOT NULL,
            content_json TEXT NOT NULL,
            created_timestamp INTEGER NOT NULL,
            timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            tokens INTEGER,
            metadata_json TEXT,
            message_id TEXT
        )
    "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    for statement in [
        "CREATE INDEX idx_messages_session ON messages(session_id)",
        "CREATE INDEX idx_messages_timestamp ON messages(timestamp)",
        "CREATE INDEX idx_messages_message_id ON messages(message_id)",
        "CREATE INDEX idx_sessions_updated ON sessions(updated_at DESC)",
        "CREATE INDEX idx_sessions_type ON sessions(session_type)",
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    assert_legacy_upgrade_session_fixture_contract(&pool).await;
    pool.close().await;
}

async fn initialize_sessions_storage(data_dir: &Path, shape: SessionsSchemaShape) {
    if shape == SessionsSchemaShape::LegacyUpgrade {
        create_legacy_upgrade_session_fixture(data_dir).await;
    }
    SessionManager::new(data_dir.to_path_buf())
        .list_sessions()
        .await
        .unwrap();
}

async fn seed_legacy_upgrade_thread_rows(data_dir: &Path, session_id: &str) {
    let pool = open_pool(&assert_sessions_database_path(data_dir)).await;
    let thread_id = format!("legacy-thread-{session_id}");
    sqlx::query("UPDATE sessions SET thread_id=? WHERE id=?")
        .bind(&thread_id)
        .bind(session_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO threads(id, name, user_set_name, working_dir, created_at, updated_at, archived_at, metadata_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&thread_id)
    .bind(format!("Legacy thread {session_id}"))
    .bind(0_i64)
    .bind("D:/legacy")
    .bind("2026-01-01 00:00:00")
    .bind("2026-01-01 00:00:00")
    .bind(Option::<String>::None)
    .bind(r#"{"seed":"legacy-upgrade"}"#)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO thread_messages(thread_id, session_id, message_id, role, content_json, created_timestamp, metadata_json) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&thread_id)
    .bind(session_id)
    .bind(format!("legacy-thread-message-{session_id}"))
    .bind("assistant")
    .bind(r#"[{"type":"text","text":"legacy thread message"}]"#)
    .bind(1_700_000_000_003_i64)
    .bind(r#"{"seed":"legacy-upgrade"}"#)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
}

async fn public_raw_agent(data_dir: &Path, config_dir: &Path) -> LuminaAcpAgent {
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::create_dir_all(config_dir).unwrap();
    let provider_factory: AcpProviderFactory = Arc::new(|_, _, _| {
        Box::pin(async {
            Err(anyhow::anyhow!(
                "provider is unused in ACP profile lifecycle auth regression"
            ))
        })
    });
    LuminaAcpAgent::new(LuminaAcpAgentOptions {
        provider_factory,
        builtins: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        config_dir: config_dir.to_path_buf(),
        disable_session_naming: true,
        lumina_platform: LuminaPlatform::LuminaCli,
        additional_source_roots: Vec::new(),
        scheduler: Arc::new(UnusedScheduler),
        mcp_platform_service: None,
        mcp_platform_service_cell: None,
    })
    .await
    .unwrap()
}

async fn public_stdio_connection(data_dir: PathBuf) -> AcpServerConnection {
    let openai = OpenAiFixture::new(vec![], Arc::new(IgnoreSessionId)).await;
    let config = TestConnectionConfig {
        data_root: data_dir,
        ..Default::default()
    };
    AcpServerConnection::new(config, openai).await
}

async fn create_session_with_profile_application_token(
    cx: &ConnectionTo<Agent>,
    token: &str,
) -> agent_client_protocol::schema::v1::NewSessionResponse {
    let work_dir = tempfile::tempdir().unwrap();
    let mut meta = serde_json::Map::new();
    meta.insert(
        "profileApplicationToken".to_string(),
        serde_json::Value::String(token.to_string()),
    );
    cx.send_request(
        agent_client_protocol::schema::v1::NewSessionRequest::new(work_dir.path()).meta(meta),
    )
    .block_task()
    .await
    .unwrap()
}

async fn new_session_error_with_profile_application_token(
    cx: &ConnectionTo<Agent>,
    token: &str,
) -> agent_client_protocol::Error {
    let work_dir = tempfile::tempdir().unwrap();
    let mut meta = serde_json::Map::new();
    meta.insert(
        "profileApplicationToken".to_string(),
        serde_json::Value::String(token.to_string()),
    );
    cx.send_request(
        agent_client_protocol::schema::v1::NewSessionRequest::new(work_dir.path()).meta(meta),
    )
    .block_task()
    .await
    .unwrap_err()
}

fn invalid_profile_application_token_error() -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params()
        .data("profile application rejected: invalid_profile_application_token")
}

fn assert_generic_profile_application_token_rejection(error: agent_client_protocol::Error) {
    assert_eq!(error, invalid_profile_application_token_error());
    let text = error
        .data
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .expect("profile token rejection should have string data");
    for forbidden in [
        "invalid_request",
        "not_found",
        "plan_stale",
        "plan_expired",
        "policy_denied",
        "trusted Lumina launcher",
        "authenticated ACP transport",
        "other_actor",
    ] {
        assert!(
            !text.contains(forbidden),
            "public error leaked internal detail: {forbidden}"
        );
    }
}

async fn update_profile_application_token_actor(data_dir: &Path, token: &str, actor: &str) {
    let pool = open_pool(&assert_platform_database_path(data_dir)).await;
    sqlx::query("UPDATE mcp_profile_application_tokens SET actor=? WHERE token_hash=?")
        .bind(actor)
        .bind(sha256_hex(token))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

async fn update_profile_application_token_expiry(data_dir: &Path, token: &str, expires_at_ms: i64) {
    let pool = open_pool(&assert_platform_database_path(data_dir)).await;
    sqlx::query("UPDATE mcp_profile_application_tokens SET expires_at_ms=? WHERE token_hash=?")
        .bind(expires_at_ms)
        .bind(sha256_hex(token))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

async fn mark_profile_application_token_consumed(
    data_dir: &Path,
    token: &str,
    consumed_at_ms: i64,
) {
    let pool = open_pool(&assert_platform_database_path(data_dir)).await;
    sqlx::query("UPDATE mcp_profile_application_tokens SET consumed_at_ms=? WHERE token_hash=?")
        .bind(consumed_at_ms)
        .bind(sha256_hex(token))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

async fn managed_projection_link_key(data_dir: &Path) -> String {
    let pool = open_pool(&assert_platform_database_path(data_dir)).await;
    let link_key = sqlx::query_scalar::<_, String>(
        "SELECT link_key FROM connection_projections ORDER BY link_key LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    pool.close().await;
    link_key
}

async fn seed_profile_bound_session(data_dir: &Path, session_id: &str, shape: SessionsSchemaShape) {
    seed_session_messages_and_usage(data_dir, session_id).await;
    if shape == SessionsSchemaShape::LegacyUpgrade {
        seed_legacy_upgrade_thread_rows(data_dir, session_id).await;
    }
}

async fn run_denied_http_profile_bound_delete(shape: SessionsSchemaShape) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    initialize_sessions_storage(&data_dir, shape).await;

    let (auth_addr, auth_server) = spawn_router(test_router(true, &dir)).await;
    let auth_endpoint = format!("http://{auth_addr}/acp?token={SECRET}");
    let (auth_cx, auth_client) = connect_http_client(auth_endpoint).await;

    let prepared = prepare_profile_application(&auth_cx, &data_dir, &digest).await;
    let created = create_session_with_profile_application_token(&auth_cx, &prepared.token).await;
    seed_profile_bound_session(&data_dir, created.session_id.0.as_ref(), shape).await;

    let (public_addr, public_server) = spawn_router(test_acp_router(&dir)).await;
    let (public_cx, public_client) = connect_http_client(format!("http://{public_addr}/acp")).await;
    let before_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_eq!(before_snapshot.sessions.shape, shape);
    let error = public_cx
        .send_request(DeleteSessionRequest {
            session_id: created.session_id.0.to_string(),
        })
        .block_task()
        .await
        .unwrap_err();
    assert_eq!(
        error,
        agent_client_protocol::Error::invalid_params()
            .data("profile application rejected: policy_denied")
    );
    let after_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_exact_persisted_footprint_unchanged(&before_snapshot, &after_snapshot);
    let record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record,
        "created",
        Some(created.session_id.0.as_ref()),
        None,
        &[
            ("token_created", "confirmed"),
            ("token_consumed", "accepted"),
            ("session_created", "session_created"),
        ],
    );

    public_client.abort();
    public_server.abort();
    auth_client.abort();
    auth_server.abort();
}

async fn run_authenticated_profile_bound_delete(shape: SessionsSchemaShape) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    initialize_sessions_storage(&data_dir, shape).await;

    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let prepared = prepare_profile_application(&cx, &data_dir, &digest).await;
    let created = create_session_with_profile_application_token(&cx, &prepared.token).await;
    seed_profile_bound_session(&data_dir, created.session_id.0.as_ref(), shape).await;

    let before_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_eq!(before_snapshot.sessions.shape, shape);
    cx.send_request(DeleteSessionRequest {
        session_id: created.session_id.0.to_string(),
    })
    .block_task()
    .await
    .unwrap();
    let after_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;

    assert_profile_application_delete_platform_delta(
        &before_snapshot.platform,
        &after_snapshot.platform,
        &prepared.application_id,
    );
    assert_session_delete_delta(
        &before_snapshot.sessions,
        &after_snapshot.sessions,
        created.session_id.0.as_ref(),
    );

    let record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record,
        "deleted",
        Some(created.session_id.0.as_ref()),
        None,
        &[
            ("token_created", "confirmed"),
            ("token_consumed", "accepted"),
            ("session_created", "session_created"),
            ("recovery_required", "session_delete_started"),
            ("session_deleted", "session_deleted"),
        ],
    );

    client.abort();
    server.abort();
}

async fn run_denied_missing_session_cleanup(shape: SessionsSchemaShape) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    initialize_sessions_storage(&data_dir, shape).await;

    let (auth_addr, auth_server) = spawn_router(test_router(true, &dir)).await;
    let auth_endpoint = format!("http://{auth_addr}/acp?token={SECRET}");
    let (auth_cx, auth_client) = connect_http_client(auth_endpoint).await;

    let prepared = prepare_profile_application(&auth_cx, &data_dir, &digest).await;
    let created = create_session_with_profile_application_token(&auth_cx, &prepared.token).await;
    seed_profile_bound_session(&data_dir, created.session_id.0.as_ref(), shape).await;
    delete_persisted_session(&data_dir, created.session_id.0.as_ref()).await;
    assert!(!session_exists(&data_dir, created.session_id.0.as_ref()).await);

    let (public_addr, public_server) = spawn_router(test_acp_router(&dir)).await;
    let (public_cx, public_client) = connect_http_client(format!("http://{public_addr}/acp")).await;
    let before_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_eq!(before_snapshot.sessions.shape, shape);
    let error = public_cx
        .send_request(DeleteSessionRequest {
            session_id: created.session_id.0.to_string(),
        })
        .block_task()
        .await
        .unwrap_err();
    assert_eq!(
        error,
        agent_client_protocol::Error::invalid_params()
            .data("profile application rejected: policy_denied")
    );
    let after_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_exact_persisted_footprint_unchanged(&before_snapshot, &after_snapshot);
    let record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record,
        "created",
        Some(created.session_id.0.as_ref()),
        None,
        &[
            ("token_created", "confirmed"),
            ("token_consumed", "accepted"),
            ("session_created", "session_created"),
        ],
    );

    public_client.abort();
    public_server.abort();
    auth_client.abort();
    auth_server.abort();
}

async fn run_authenticated_missing_session_cleanup(shape: SessionsSchemaShape) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    initialize_sessions_storage(&data_dir, shape).await;

    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let prepared = prepare_profile_application(&cx, &data_dir, &digest).await;
    let created = create_session_with_profile_application_token(&cx, &prepared.token).await;
    seed_profile_bound_session(&data_dir, created.session_id.0.as_ref(), shape).await;
    delete_persisted_session(&data_dir, created.session_id.0.as_ref()).await;
    assert!(!session_exists(&data_dir, created.session_id.0.as_ref()).await);

    let before_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_eq!(before_snapshot.sessions.shape, shape);
    cx.send_request(DeleteSessionRequest {
        session_id: created.session_id.0.to_string(),
    })
    .block_task()
    .await
    .unwrap();
    let after_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;

    assert_eq!(after_snapshot.sessions, before_snapshot.sessions);
    assert_profile_application_delete_platform_delta(
        &before_snapshot.platform,
        &after_snapshot.platform,
        &prepared.application_id,
    );

    let record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record,
        "deleted",
        Some(created.session_id.0.as_ref()),
        None,
        &[
            ("token_created", "confirmed"),
            ("token_consumed", "accepted"),
            ("session_created", "session_created"),
            ("recovery_required", "session_delete_started"),
            ("session_deleted", "session_deleted"),
        ],
    );

    client.abort();
    server.abort();
}

async fn run_denied_raw_dispatch_profile_bound_delete(shape: SessionsSchemaShape) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let config_dir = dir.path().join("config");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    initialize_sessions_storage(&data_dir, shape).await;
    let (auth_addr, auth_server) = spawn_router(test_router(true, &dir)).await;
    let auth_endpoint = format!("http://{auth_addr}/acp?token={SECRET}");
    let (auth_cx, auth_client) = connect_http_client(auth_endpoint).await;

    let prepared = prepare_profile_application(&auth_cx, &data_dir, &digest).await;
    let created = create_session_with_profile_application_token(&auth_cx, &prepared.token).await;
    let agent = public_raw_agent(&data_dir, &config_dir).await;
    seed_profile_bound_session(&data_dir, created.session_id.0.as_ref(), shape).await;

    let before_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_eq!(before_snapshot.sessions.shape, shape);
    let error = agent
        .dispatch_custom_request(
            "session/delete",
            serde_json::json!({"sessionId": created.session_id.0.to_string()}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        agent_client_protocol::Error::invalid_params()
            .data("profile application rejected: policy_denied")
    );

    let after_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_exact_persisted_footprint_unchanged(&before_snapshot, &after_snapshot);
    let record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record,
        "created",
        Some(created.session_id.0.as_ref()),
        None,
        &[
            ("token_created", "confirmed"),
            ("token_consumed", "accepted"),
            ("session_created", "session_created"),
        ],
    );

    auth_client.abort();
    auth_server.abort();
}

async fn run_denied_public_stdio_profile_bound_delete(shape: SessionsSchemaShape) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    initialize_sessions_storage(&data_dir, shape).await;
    let (auth_addr, auth_server) = spawn_router(test_router(true, &dir)).await;
    let auth_endpoint = format!("http://{auth_addr}/acp?token={SECRET}");
    let (auth_cx, auth_client) = connect_http_client(auth_endpoint).await;

    let prepared = prepare_profile_application(&auth_cx, &data_dir, &digest).await;
    let created = create_session_with_profile_application_token(&auth_cx, &prepared.token).await;
    let connection = public_stdio_connection(data_dir.clone()).await;
    seed_profile_bound_session(&data_dir, created.session_id.0.as_ref(), shape).await;

    let before_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_eq!(before_snapshot.sessions.shape, shape);
    let error = connection
        .cx()
        .send_request(DeleteSessionRequest {
            session_id: created.session_id.0.to_string(),
        })
        .block_task()
        .await
        .unwrap_err();
    assert_eq!(
        error,
        agent_client_protocol::Error::invalid_params()
            .data("profile application rejected: policy_denied")
    );

    let after_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_exact_persisted_footprint_unchanged(&before_snapshot, &after_snapshot);
    let record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record,
        "created",
        Some(created.session_id.0.as_ref()),
        None,
        &[
            ("token_created", "confirmed"),
            ("token_consumed", "accepted"),
            ("session_created", "session_created"),
        ],
    );

    auth_client.abort();
    auth_server.abort();
}

#[tokio::test]
async fn acp_requests_without_token_are_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    for method in [Method::GET, Method::POST, Method::DELETE] {
        let status = send(&router, method.clone(), "/acp", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "method: {method}");
    }
}

#[tokio::test]
async fn websocket_handshake_without_token_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    let status = send(
        &router,
        Method::GET,
        "/acp",
        &[
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGVzdGtleTEyMzQ1Njc4OQ=="),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bare_public_http_router_denies_every_unavailable_custom_route_without_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let (repository, digest) = insert_manifest(&dir.path().join("data")).await;
    let (addr, server) = spawn_router(test_acp_router(&dir)).await;
    let (cx, client) = connect_http_client(format!("http://{addr}/acp")).await;

    let before_snapshot = exact_persisted_footprint_snapshot(&dir.path().join("data")).await;
    let _secrets = assert_all_unavailable_custom_routes_denied(&cx, &dir.path().join("data")).await;
    let after_snapshot = exact_persisted_footprint_snapshot(&dir.path().join("data")).await;
    assert_exact_persisted_footprint_unchanged(&before_snapshot, &after_snapshot);
    assert_plan_create_denied(&cx, &digest, "public-http-register").await;
    assert!(repository
        .get_plan_by_idempotency_key("public-http-register")
        .await
        .unwrap()
        .is_none());

    client.abort();
    server.abort();
}

#[tokio::test]
async fn serve_router_without_token_denies_mcp_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let (repository, digest) = insert_manifest(&dir.path().join("data")).await;
    let (addr, server) = spawn_router(test_router(false, &dir)).await;
    let (cx, client) = connect_http_client(format!("http://{addr}/acp")).await;

    assert_catalog_available_read_only(&cx).await;
    assert_plan_create_denied(&cx, &digest, "serve-http-register").await;
    assert!(repository
        .get_plan_by_idempotency_key("serve-http-register")
        .await
        .unwrap()
        .is_none());

    client.abort();
    server.abort();
}

#[tokio::test]
async fn authenticated_http_transport_creates_review_plan_without_committing_installation() {
    let dir = tempfile::tempdir().unwrap();
    let (repository, digest) = insert_manifest(&dir.path().join("data")).await;
    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint.clone()).await;

    assert_plan_create_available(&cx, &digest, "authenticated-register-1").await;
    assert!(repository
        .get_plan_by_idempotency_key("authenticated-register-1")
        .await
        .unwrap()
        .is_some());

    client.abort();
    let _ = client.await;
    assert!(send_custom(
        &cx,
        MCP_PLAN_CREATE_METHOD,
        serde_json::json!({
            "intent": {
                "type": "register",
                "manifestDigest": parse_manifest(REMOTE.as_bytes()).unwrap().digest(),
                "installationScope": InstallationScope::User,
            },
            "idempotencyKey": "authenticated-register-stale",
        }),
    )
    .await
    .is_err());

    let (fresh_cx, fresh_client) = connect_http_client(endpoint).await;
    let fresh_digest = parse_manifest(REMOTE.as_bytes())
        .unwrap()
        .digest()
        .to_string();
    assert_plan_create_available(&fresh_cx, &fresh_digest, "authenticated-register-2").await;
    assert!(repository
        .get_plan_by_idempotency_key("authenticated-register-2")
        .await
        .unwrap()
        .is_some());

    fresh_client.abort();
    server.abort();
}

#[tokio::test]
async fn authenticated_http_profile_application_token_bootstraps_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let prepared = prepare_profile_application(&cx, &data_dir, &digest).await;
    let work_dir = tempfile::tempdir().unwrap();
    let mut meta = serde_json::Map::new();
    meta.insert(
        "profileApplicationToken".to_string(),
        serde_json::Value::String(prepared.token.clone()),
    );

    let created = cx
        .send_request(
            agent_client_protocol::schema::v1::NewSessionRequest::new(work_dir.path()).meta(meta),
        )
        .block_task()
        .await
        .unwrap();
    let record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record,
        "created",
        Some(created.session_id.0.as_ref()),
        None,
        &[
            ("token_created", "confirmed"),
            ("token_consumed", "accepted"),
            ("session_created", "session_created"),
        ],
    );

    client.abort();
    server.abort();
}

#[tokio::test]
async fn authenticated_http_new_session_with_malformed_profile_application_token_is_generic() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let error =
        new_session_error_with_profile_application_token(&cx, "malformed-profile-token").await;
    assert_generic_profile_application_token_rejection(error);

    client.abort();
    server.abort();
}

#[tokio::test]
async fn authenticated_http_new_session_with_unknown_profile_application_token_is_generic() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let error =
        new_session_error_with_profile_application_token(&cx, "profile_application_unknown").await;
    assert_generic_profile_application_token_rejection(error);

    client.abort();
    server.abort();
}

#[tokio::test]
async fn authenticated_http_new_session_with_replayed_profile_application_token_is_generic() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let prepared = prepare_profile_application(&cx, &data_dir, &digest).await;
    let _created = create_session_with_profile_application_token(&cx, &prepared.token).await;

    let error = new_session_error_with_profile_application_token(&cx, &prepared.token).await;
    assert_generic_profile_application_token_rejection(error);

    client.abort();
    server.abort();
}

#[tokio::test]
async fn authenticated_http_new_session_with_wrong_actor_profile_application_token_is_generic() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let prepared = prepare_profile_application(&cx, &data_dir, &digest).await;
    update_profile_application_token_actor(&data_dir, &prepared.token, "other_actor").await;

    let error = new_session_error_with_profile_application_token(&cx, &prepared.token).await;
    assert_generic_profile_application_token_rejection(error);

    client.abort();
    server.abort();
}

#[tokio::test]
async fn authenticated_http_new_session_with_expired_profile_application_token_is_generic() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let prepared = prepare_profile_application(&cx, &data_dir, &digest).await;
    update_profile_application_token_expiry(&data_dir, &prepared.token, 0).await;

    let error = new_session_error_with_profile_application_token(&cx, &prepared.token).await;
    assert_generic_profile_application_token_rejection(error);

    client.abort();
    server.abort();
}

#[tokio::test]
async fn authenticated_http_new_session_with_consumed_profile_application_token_is_generic() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let prepared = prepare_profile_application(&cx, &data_dir, &digest).await;
    mark_profile_application_token_consumed(&data_dir, &prepared.token, 1).await;

    let error = new_session_error_with_profile_application_token(&cx, &prepared.token).await;
    assert_generic_profile_application_token_rejection(error);

    client.abort();
    server.abort();
}

#[tokio::test]
async fn public_stdio_new_session_with_managed_extension_but_without_profile_token_keeps_policy_message(
) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    let (auth_addr, auth_server) = spawn_router(test_router(true, &dir)).await;
    let auth_endpoint = format!("http://{auth_addr}/acp?token={SECRET}");
    let (auth_cx, auth_client) = connect_http_client(auth_endpoint).await;

    let prepared = prepare_profile_application(&auth_cx, &data_dir, &digest).await;
    let managed_name = managed_projection_link_key(&data_dir).await;
    let connection = public_stdio_connection(data_dir.clone()).await;
    let before_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    let work_dir = tempfile::tempdir().unwrap();
    let error = connection
        .cx()
        .send_request(
            agent_client_protocol::schema::v1::NewSessionRequest::new(work_dir.path()).mcp_servers(
                vec![agent_client_protocol::schema::v1::McpServer::Stdio(
                    agent_client_protocol::schema::v1::McpServerStdio::new(
                        managed_name,
                        "forged-managed-command",
                    ),
                )],
            ),
        )
        .block_task()
        .await
        .unwrap_err();
    assert_eq!(
        error,
        agent_client_protocol::Error::invalid_params().data(
            "platform-managed MCP configuration must be supplied by a confirmed profile application token"
        )
    );

    let after_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_exact_persisted_footprint_unchanged(&before_snapshot, &after_snapshot);
    let record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record,
        "confirmed",
        None,
        None,
        &[("token_created", "confirmed")],
    );

    auth_client.abort();
    auth_server.abort();
}

#[tokio::test]
async fn bare_public_http_session_delete_with_profile_application_is_denied_without_side_effects() {
    run_denied_http_profile_bound_delete(SessionsSchemaShape::FreshBootstrap).await;
}

#[tokio::test]
async fn authenticated_http_session_delete_finalizes_profile_application_cleanup() {
    run_authenticated_profile_bound_delete(SessionsSchemaShape::FreshBootstrap).await;
}

#[tokio::test]
async fn authenticated_http_session_delete_finalizes_profile_application_cleanup_legacy_upgrade() {
    run_authenticated_profile_bound_delete(SessionsSchemaShape::LegacyUpgrade).await;
}

#[tokio::test]
async fn dropped_authenticated_connection_cannot_reuse_profile_bootstrap_or_delete_until_reconnect()
{
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    let (addr, server) = spawn_router(test_router(true, &dir)).await;
    let endpoint = format!("http://{addr}/acp?token={SECRET}");
    let (cx, client) = connect_http_client(endpoint).await;

    let prepared = prepare_profile_application(&cx, &data_dir, &digest).await;
    client.abort();
    let _ = client.await;

    let work_dir = tempfile::tempdir().unwrap();
    let mut meta = serde_json::Map::new();
    meta.insert(
        "profileApplicationToken".to_string(),
        serde_json::Value::String(prepared.token),
    );
    assert!(cx
        .send_request(
            agent_client_protocol::schema::v1::NewSessionRequest::new(work_dir.path()).meta(meta)
        )
        .block_task()
        .await
        .is_err());
    let record_after_stale_bootstrap =
        profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record_after_stale_bootstrap,
        "confirmed",
        None,
        None,
        &[("token_created", "confirmed")],
    );

    let (fresh_cx, fresh_client) = connect_http_client(endpoint.clone()).await;
    let created = fresh_cx
        .send_request(
            agent_client_protocol::schema::v1::NewSessionRequest::new(work_dir.path()).meta(meta),
        )
        .block_task()
        .await
        .unwrap();
    let record_after_fresh_bootstrap =
        profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record_after_fresh_bootstrap,
        "created",
        Some(created.session_id.0.as_ref()),
        None,
        &[
            ("token_created", "confirmed"),
            ("token_consumed", "accepted"),
            ("session_created", "session_created"),
        ],
    );

    fresh_client.abort();
    let _ = fresh_client.await;
    assert!(fresh_cx
        .send_request(DeleteSessionRequest {
            session_id: created.session_id.0.to_string(),
        })
        .block_task()
        .await
        .is_err());

    let record_after_stale_delete =
        profile_application_record(&data_dir, &prepared.application_id).await;
    assert_eq!(record_after_stale_delete, record_after_fresh_bootstrap);

    let (reconnected_cx, reconnected_client) = connect_http_client(endpoint).await;
    reconnected_cx
        .send_request(DeleteSessionRequest {
            session_id: created.session_id.0.to_string(),
        })
        .block_task()
        .await
        .unwrap();
    let final_record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &final_record,
        "deleted",
        Some(created.session_id.0.as_ref()),
        None,
        &[
            ("token_created", "confirmed"),
            ("token_consumed", "accepted"),
            ("session_created", "session_created"),
            ("recovery_required", "session_delete_started"),
            ("session_deleted", "session_deleted"),
        ],
    );

    reconnected_client.abort();
    server.abort();
}

#[tokio::test]
async fn bare_public_http_missing_session_profile_cleanup_is_denied_without_side_effects() {
    run_denied_missing_session_cleanup(SessionsSchemaShape::FreshBootstrap).await;
}

#[tokio::test]
async fn authenticated_http_missing_session_profile_cleanup_finalizes_application() {
    run_authenticated_missing_session_cleanup(SessionsSchemaShape::FreshBootstrap).await;
}

#[tokio::test]
async fn authenticated_http_missing_session_profile_cleanup_finalizes_application_legacy_upgrade() {
    run_authenticated_missing_session_cleanup(SessionsSchemaShape::LegacyUpgrade).await;
}

#[tokio::test]
async fn raw_public_dispatch_profile_bound_delete_is_denied_without_side_effects() {
    run_denied_raw_dispatch_profile_bound_delete(SessionsSchemaShape::FreshBootstrap).await;
}

#[tokio::test]
async fn public_stdio_new_session_with_profile_application_token_is_denied_without_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let (_repository, digest) = insert_manifest(&data_dir).await;
    initialize_sessions_storage(&data_dir, SessionsSchemaShape::FreshBootstrap).await;
    let (auth_addr, auth_server) = spawn_router(test_router(true, &dir)).await;
    let auth_endpoint = format!("http://{auth_addr}/acp?token={SECRET}");
    let (auth_cx, auth_client) = connect_http_client(auth_endpoint).await;

    let prepared = prepare_profile_application(&auth_cx, &data_dir, &digest).await;
    let connection = public_stdio_connection(data_dir.clone()).await;
    let before_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_eq!(
        before_snapshot.sessions.shape,
        SessionsSchemaShape::FreshBootstrap
    );
    let work_dir = tempfile::tempdir().unwrap();
    let mut meta = serde_json::Map::new();
    meta.insert(
        "profileApplicationToken".to_string(),
        serde_json::Value::String(prepared.token),
    );
    let error = connection
        .cx()
        .send_request(
            agent_client_protocol::schema::v1::NewSessionRequest::new(work_dir.path()).meta(meta),
        )
        .block_task()
        .await
        .unwrap_err();
    assert_eq!(
        error,
        agent_client_protocol::Error::invalid_params()
            .data("profile application rejected: policy_denied")
    );

    let after_snapshot = exact_persisted_footprint_snapshot(&data_dir).await;
    assert_exact_persisted_footprint_unchanged(&before_snapshot, &after_snapshot);
    let record = profile_application_record(&data_dir, &prepared.application_id).await;
    assert_profile_application_contract(
        &record,
        "confirmed",
        None,
        None,
        &[("token_created", "confirmed")],
    );

    auth_client.abort();
    auth_server.abort();
}

#[tokio::test]
async fn public_stdio_session_delete_with_profile_application_is_denied_without_side_effects() {
    run_denied_public_stdio_profile_bound_delete(SessionsSchemaShape::FreshBootstrap).await;
}

#[tokio::test]
async fn bare_public_http_session_delete_with_profile_application_is_denied_without_side_effects_legacy_upgrade(
) {
    run_denied_http_profile_bound_delete(SessionsSchemaShape::LegacyUpgrade).await;
}

#[tokio::test]
async fn raw_public_dispatch_profile_bound_delete_is_denied_without_side_effects_legacy_upgrade() {
    run_denied_raw_dispatch_profile_bound_delete(SessionsSchemaShape::LegacyUpgrade).await;
}

#[tokio::test]
async fn public_stdio_session_delete_with_profile_application_is_denied_without_side_effects_legacy_upgrade(
) {
    run_denied_public_stdio_profile_bound_delete(SessionsSchemaShape::LegacyUpgrade).await;
}

#[tokio::test]
async fn bare_public_http_missing_session_profile_cleanup_is_denied_without_side_effects_legacy_upgrade(
) {
    run_denied_missing_session_cleanup(SessionsSchemaShape::LegacyUpgrade).await;
}

#[tokio::test]
async fn websocket_handshake_rejects_arbitrary_web_origins() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let status = send(
        &router,
        Method::GET,
        "/acp",
        &[
            ("origin", "https://evil.example"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn acp_router_websocket_handshake_rejects_arbitrary_web_origins() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_acp_router(&dir);

    let status = send(
        &router,
        Method::GET,
        "/acp",
        &[
            ("origin", "https://evil.example"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn acp_router_websocket_handshake_rejects_file_origins_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_acp_router(&dir);

    for origin in ["null", "file://"] {
        let status = send(
            &router,
            Method::GET,
            "/acp",
            &[
                ("origin", origin),
                ("connection", "upgrade"),
                ("upgrade", "websocket"),
                ("sec-websocket-version", "13"),
                ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ],
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn authenticated_acp_router_allows_packaged_desktop_null_websocket_origin() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_authenticated_acp_router(&dir);

    let status = send(
        &router,
        Method::GET,
        &format!("/acp?token={SECRET}"),
        &[
            ("origin", "null"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn authenticated_acp_router_allows_packaged_desktop_file_websocket_origin() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_authenticated_acp_router(&dir);

    let status = send(
        &router,
        Method::GET,
        &format!("/acp?token={SECRET}"),
        &[
            ("origin", "file://"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn authenticated_serve_router_allows_null_websocket_origin_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    let status = send(
        &router,
        Method::GET,
        &format!("/acp?token={SECRET}"),
        &[
            ("origin", "null"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn authenticated_serve_router_allows_file_websocket_origin_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    let status = send(
        &router,
        Method::GET,
        &format!("/acp?token={SECRET}"),
        &[
            ("origin", "file://"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn unauthenticated_serve_router_rejects_null_websocket_origin_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let status = send(
        &router,
        Method::GET,
        "/acp",
        &[
            ("origin", "null"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn unauthenticated_serve_router_rejects_file_websocket_origin_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let status = send(
        &router,
        Method::GET,
        "/acp",
        &[
            ("origin", "file://"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn websocket_handshake_allows_loopback_web_origins_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let status = send(
        &router,
        Method::GET,
        "/acp",
        &[
            ("origin", "http://localhost:5173"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn websocket_handshake_allows_ipv6_loopback_web_origins_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let status = send(
        &router,
        Method::GET,
        "/acp",
        &[
            ("origin", "http://[::1]:5173"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn websocket_handshake_explicit_origins_replace_loopback_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router_with_origins(
        false,
        &dir,
        vec![HeaderValue::from_static("app://localhost")],
    );

    let status = send(
        &router,
        Method::GET,
        "/acp",
        &[
            ("origin", "http://localhost:5173"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn websocket_handshake_allows_configured_origins() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router_with_origins(
        false,
        &dir,
        vec![HeaderValue::from_static("app://localhost")],
    );

    let status = send(
        &router,
        Method::GET,
        "/acp",
        &[
            ("origin", "app://localhost"),
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    )
    .await;

    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn websocket_handshake_explicit_origins_replace_file_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router_with_origins(
        false,
        &dir,
        vec![HeaderValue::from_static("app://localhost")],
    );

    for origin in ["null", "file://"] {
        let status = send(
            &router,
            Method::GET,
            "/acp",
            &[
                ("origin", origin),
                ("connection", "upgrade"),
                ("upgrade", "websocket"),
                ("sec-websocket-version", "13"),
                ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ],
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn header_token_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    // 406 (missing Accept: text/event-stream) proves the request passed auth.
    let status = send(&router, Method::GET, "/acp", &[("X-Secret-Key", SECRET)]).await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn query_token_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    let uri = format!("/acp?token={SECRET}");
    let status = send(&router, Method::GET, &uri, &[]).await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn wrong_token_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    let status = send(&router, Method::GET, "/acp", &[("X-Secret-Key", "nope")]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let status = send(&router, Method::GET, "/acp?token=nope", &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_endpoints_skip_token_check() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    for path in ["/health", "/status"] {
        let status = send(&router, Method::GET, path, &[]).await;
        assert_eq!(status, StatusCode::OK, "path: {path}");
    }
}

#[tokio::test]
async fn acp_open_when_auth_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let status = send(&router, Method::GET, "/acp", &[]).await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn acp_cors_rejects_arbitrary_web_origins() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "https://evil.example"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,acp-connection-id",
            ),
        ],
    )
    .await;

    assert!(response
        .headers()
        .get("access-control-allow-origin")
        .is_none());
}

#[tokio::test]
async fn acp_cors_rejects_custom_app_origins_unless_configured() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "app://localhost"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,acp-connection-id",
            ),
        ],
    )
    .await;

    assert!(response
        .headers()
        .get("access-control-allow-origin")
        .is_none());
}

#[tokio::test]
async fn acp_cors_allows_loopback_web_origins() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "http://localhost:5173"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,acp-connection-id",
            ),
        ],
    )
    .await;

    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:5173")
    );
}

#[tokio::test]
async fn acp_cors_allows_ipv6_loopback_web_origins() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "http://[::1]:5173"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,acp-connection-id",
            ),
        ],
    )
    .await;

    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://[::1]:5173")
    );
}

#[tokio::test]
async fn authenticated_acp_cors_preflight_skips_token_check() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_authenticated_acp_router(&dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "http://localhost:5173"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,x-secret-key,acp-connection-id",
            ),
        ],
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:5173")
    );
}

#[tokio::test]
async fn authenticated_acp_cors_allows_packaged_desktop_null_origin() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_authenticated_acp_router(&dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "null"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,x-secret-key,acp-connection-id",
            ),
        ],
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("null")
    );
}

#[tokio::test]
async fn authenticated_acp_cors_allows_packaged_desktop_file_origin() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_authenticated_acp_router(&dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "file://"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,x-secret-key,acp-connection-id",
            ),
        ],
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("file://")
    );
}

#[tokio::test]
async fn authenticated_serve_cors_allows_null_origin_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "null"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,x-secret-key,acp-connection-id",
            ),
        ],
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("null")
    );
}

#[tokio::test]
async fn authenticated_serve_cors_allows_file_origin_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(true, &dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "file://"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,x-secret-key,acp-connection-id",
            ),
        ],
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("file://")
    );
}

#[tokio::test]
async fn unauthenticated_serve_cors_rejects_null_origin_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "null"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,x-secret-key,acp-connection-id",
            ),
        ],
    )
    .await;

    assert!(response
        .headers()
        .get("access-control-allow-origin")
        .is_none());
}

#[tokio::test]
async fn unauthenticated_serve_cors_rejects_file_origin_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router(false, &dir);

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "file://"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,x-secret-key,acp-connection-id",
            ),
        ],
    )
    .await;

    assert!(response
        .headers()
        .get("access-control-allow-origin")
        .is_none());
}

#[tokio::test]
async fn acp_cors_explicit_origins_replace_loopback_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router_with_origins(
        false,
        &dir,
        vec![HeaderValue::from_static("app://localhost")],
    );

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "http://localhost:5173"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,acp-connection-id",
            ),
        ],
    )
    .await;

    assert!(response
        .headers()
        .get("access-control-allow-origin")
        .is_none());
}

#[tokio::test]
async fn acp_cors_allows_additional_configured_origins() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router_with_origins(
        false,
        &dir,
        vec![HeaderValue::from_static("app://localhost")],
    );

    let response = send_response(
        &router,
        Method::OPTIONS,
        "/acp",
        &[
            ("Origin", "app://localhost"),
            ("Access-Control-Request-Method", "POST"),
            (
                "Access-Control-Request-Headers",
                "content-type,acp-connection-id",
            ),
        ],
    )
    .await;

    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("app://localhost")
    );
}

#[tokio::test]
async fn acp_cors_explicit_origins_replace_file_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let router = test_router_with_origins(
        false,
        &dir,
        vec![HeaderValue::from_static("app://localhost")],
    );

    for origin in ["null", "file://"] {
        let response = send_response(
            &router,
            Method::OPTIONS,
            "/acp",
            &[
                ("Origin", origin),
                ("Access-Control-Request-Method", "POST"),
                (
                    "Access-Control-Request-Headers",
                    "content-type,x-secret-key,acp-connection-id",
                ),
            ],
        )
        .await;

        assert!(response
            .headers()
            .get("access-control-allow-origin")
            .is_none());
    }
}
