#![cfg(feature = "rustls-tls")]

use anyhow::Result;
use axum::response::IntoResponse;
use axum_server::tls_rustls::RustlsConfig;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, SanType};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{model::*, tool, tool_handler, tool_router, ServerHandler};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "integration-test-support")]
use async_trait::async_trait;
#[cfg(feature = "integration-test-support")]
use lumina::mcp_platform::{
    ManagedRemoteHttpClient, McpPlatformError, McpPlatformErrorCode, McpPlatformResult,
    RemoteHttpNetworkPolicy,
};

pub const FIXTURE_HOST: &str = "mcp-fixture.example.com";
pub const FIXTURE_CODE: &str = "MCP_FIXTURE_CODE";
const MANIFEST_PATH: &str = "/manifest.json";
const MCP_PATH: &str = "/mcp";

#[derive(Clone)]
struct FixtureServer {
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
    tool_calls: Arc<Mutex<Vec<&'static str>>>,
}

#[tool_router]
impl FixtureServer {
    #[tool(description = "Returns the deterministic fixture code")]
    fn get_code(&self) -> String {
        self.tool_calls.lock().unwrap().push("get_code");
        FIXTURE_CODE.to_owned()
    }
}

#[tool_handler]
impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

impl Default for FixtureServer {
    fn default() -> Self {
        Self {
            tool_router: Self::tool_router(),
            tool_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

pub struct RemoteHttpsFixture {
    pub manifest_url: String,
    pub mcp_url: String,
    pub ca_der: Vec<u8>,
    pub socket_addr: SocketAddr,
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
    tool_calls: Arc<Mutex<Vec<&'static str>>>,
}

impl RemoteHttpsFixture {
    pub async fn start() -> Result<Self> {
        #[cfg(feature = "integration-test-support")]
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let (root_pem, ca_der, leaf_pem, leaf_key_pem) = certificates()?;
        let tls = RustlsConfig::from_pem(
            format!("{leaf_pem}{root_pem}").into_bytes(),
            leaf_key_pem.into_bytes(),
        )
        .await?;
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let socket_addr = listener.local_addr()?;
        let cancellation = CancellationToken::new();
        let manifest_host = format!("{FIXTURE_HOST}:{}", socket_addr.port());
        let mcp_url = format!("https://{FIXTURE_HOST}:{}{}", socket_addr.port(), MCP_PATH);
        let manifest_body = Arc::new(manifest_bytes(&mcp_url));
        let manifest_for_route = Arc::clone(&manifest_body);
        let manifest_host_for_route = manifest_host.clone();
        let tool_calls = Arc::new(Mutex::new(Vec::new()));
        let tool_calls_for_service = Arc::clone(&tool_calls);
        let service = StreamableHttpService::new(
            move || {
                Ok(FixtureServer {
                    tool_router: FixtureServer::tool_router(),
                    tool_calls: Arc::clone(&tool_calls_for_service),
                })
            },
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default()
                .with_allowed_hosts([format!("{FIXTURE_HOST}:{}", socket_addr.port())])
                .with_cancellation_token(cancellation.child_token()),
        );
        let router = axum::Router::new().nest_service(MCP_PATH, service).route(
            MANIFEST_PATH,
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let manifest_bytes = Arc::clone(&manifest_for_route);
                let manifest_host = manifest_host_for_route.clone();
                async move { manifest(headers, manifest_bytes, &manifest_host).await }
            }),
        );
        let server = axum_server::from_tcp_rustls(listener, tls)?;
        let handle = axum_server::Handle::new();
        let shutdown = cancellation.clone();
        let task = tokio::spawn(async move {
            let server = server
                .handle(handle.clone())
                .serve(router.into_make_service());
            tokio::pin!(server);
            tokio::select! {
                _ = &mut server => {}
                _ = shutdown.cancelled() => { handle.graceful_shutdown(None); let _ = server.await; }
            }
        });
        Ok(Self {
            manifest_url: format!(
                "https://{FIXTURE_HOST}:{}{}",
                socket_addr.port(),
                MANIFEST_PATH
            ),
            mcp_url,
            ca_der,
            socket_addr,
            cancellation,
            task: Some(task),
            tool_calls,
        })
    }

    pub fn secure_client(&self) -> Result<reqwest::Client> {
        let ca = reqwest::Certificate::from_der(&self.ca_der)?;
        Ok(reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(
                &format!("{FIXTURE_HOST}:{}", self.socket_addr.port()),
                &[self.socket_addr],
            )
            .add_root_certificate(ca)
            .build()?)
    }

    pub fn validate_manifest_url(&self, url: &str) -> Result<url::Url> {
        self.validate_url(url, MANIFEST_PATH)
    }

    pub fn validate_mcp_url(&self, url: &str) -> Result<url::Url> {
        self.validate_url(url, MCP_PATH)
    }

    pub async fn fetch_manifest(&self) -> Result<reqwest::Response> {
        let url = self.validate_manifest_url(&self.manifest_url)?;
        Ok(self.secure_client()?.get(url).send().await?)
    }

    pub fn tool_calls(&self) -> Vec<&'static str> {
        self.tool_calls.lock().unwrap().clone()
    }

    fn validate_url(&self, value: &str, path: &str) -> Result<url::Url> {
        let url = url::Url::parse(value)?;
        anyhow::ensure!(
            value == format!("https://{FIXTURE_HOST}:{}{}", self.socket_addr.port(), path)
                && url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.host_str() == Some(FIXTURE_HOST)
                && url.port() == Some(self.socket_addr.port())
                && url.path() == path,
            "fixture URL is outside the fixed HTTPS origin"
        );
        Ok(url)
    }
}

impl Drop for RemoteHttpsFixture {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(feature = "integration-test-support")]
pub struct FixtureRemoteHttpNetworkPolicy {
    endpoint: String,
    socket_addr: SocketAddr,
    ca_der: Vec<u8>,
}

#[cfg(feature = "integration-test-support")]
impl FixtureRemoteHttpNetworkPolicy {
    pub fn new(fixture: &RemoteHttpsFixture) -> Self {
        Self {
            endpoint: fixture.mcp_url.clone(),
            socket_addr: fixture.socket_addr,
            ca_der: fixture.ca_der.clone(),
        }
    }

    fn validate(&self, endpoint: &str) -> McpPlatformResult<url::Url> {
        let url = url::Url::parse(endpoint).map_err(|_| {
            McpPlatformError::new(
                McpPlatformErrorCode::UnsafeUrl,
                "fixture endpoint is unsafe",
            )
        })?;
        if endpoint != self.endpoint
            || url.scheme() != "https"
            || url.host_str() != Some(FIXTURE_HOST)
            || url.port() != Some(self.socket_addr.port())
            || url.path() != "/mcp"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.to_string() != self.endpoint
        {
            return Err(McpPlatformError::new(
                McpPlatformErrorCode::UnsafeUrl,
                "fixture endpoint is outside the exact HTTPS MCP endpoint",
            ));
        }
        Ok(url)
    }
}

#[cfg(feature = "integration-test-support")]
#[async_trait]
impl RemoteHttpNetworkPolicy for FixtureRemoteHttpNetworkPolicy {
    fn validate_endpoint(&self, endpoint: &str) -> McpPlatformResult<()> {
        self.validate(endpoint).map(|_| ())
    }

    async fn secure_client(
        &self,
        endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<ManagedRemoteHttpClient> {
        let endpoint = self.validate(endpoint)?;
        ManagedRemoteHttpClient::new_for_integration_test(endpoint, self.socket_addr, &self.ca_der)
    }

    async fn validate_for_plan(
        &self,
        endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<()> {
        self.validate_endpoint(endpoint)
    }
}

fn manifest_bytes(mcp_url: &str) -> Vec<u8> {
    serde_json::json!({
        "schema_version": 1, "id": "mcp-fixture", "version": "1.0.0",
        "name": "Lumina HTTPS fixture", "description": "ACP HTTPS MCP fixture",
        "publisher": {"id": "lumina", "name": "Lumina"}, "license": {"spdx": "MIT"},
        "capabilities": ["tools"], "permissions": [], "distribution": {"type": "remote_http"},
        "transport": {"type": "streamable_http", "url": mcp_url}, "auth": {"type": "none"},
        "health_check": {"type": "mcp_initialize", "timeout_seconds": 1}, "owned_files": [],
        "uninstall": {"mode": "remove_owned_files_only", "preserve_user_data": true}
    })
    .to_string()
    .into_bytes()
}

async fn manifest(
    headers: axum::http::HeaderMap,
    bytes: Arc<Vec<u8>>,
    expected_host: &str,
) -> axum::response::Response {
    let host_headers = headers.get_all(axum::http::header::HOST);
    if host_headers.iter().count() != 1
        || host_headers
            .iter()
            .next()
            .and_then(|value| value.to_str().ok())
            != Some(expected_host)
    {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }

    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes.as_ref().clone(),
    )
        .into_response()
}

fn certificates() -> Result<(String, Vec<u8>, String, String)> {
    let root_key = KeyPair::generate()?;
    let mut root_params = CertificateParams::default();
    root_params
        .distinguished_name
        .push(DnType::CommonName, "Lumina ACP fixture CA");
    root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let root = root_params.self_signed(&root_key)?;
    let leaf_key = KeyPair::generate()?;
    let mut leaf_params = CertificateParams::new(vec![FIXTURE_HOST.to_owned()])?;
    leaf_params.subject_alt_names = vec![SanType::DnsName(FIXTURE_HOST.try_into()?)];
    let leaf = leaf_params.signed_by(&leaf_key, &Issuer::from_params(&root_params, &root_key))?;
    Ok((
        root.pem(),
        root.der().to_vec(),
        leaf.pem(),
        leaf_key.serialize_pem(),
    ))
}
