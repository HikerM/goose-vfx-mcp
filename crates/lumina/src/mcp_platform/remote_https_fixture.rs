use anyhow::Result;
use axum_server::tls_rustls::RustlsConfig;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, SanType};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{
    model::{ServerCapabilities, ServerInfo},
    tool, tool_router, ServerHandler,
};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[cfg(all(test, feature = "rustls-tls"))]
use super::remote_https_test_policy::FixtureHttpsCapability;

const AUTHORITY: &str = "mcp-fixture.example.com";
const MCP_PATH: &str = "/mcp";
const MANIFEST_PATH: &str = "/manifest.json";

fn fixture_allowed_host(port: u16) -> String {
    format!("{AUTHORITY}:{port}")
}

#[derive(Debug, Clone)]
struct FixtureServer {
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

#[tool_router]
impl FixtureServer {
    #[tool(description = "Returns the deterministic fixture code")]
    fn get_code(&self) -> String {
        "MCP_FIXTURE_CODE".to_string()
    }
}

impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("crate-internal HTTPS MCP fixture")
    }
}

impl Default for FixtureServer {
    fn default() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

pub(crate) struct RemoteHttpsFixture {
    pub(crate) mcp_url: String,
    pub(crate) manifest_url: String,
    /// Compatibility alias for the MCP endpoint.
    pub(crate) url: String,
    pub(crate) root_certificate_der: Vec<u8>,
    pub(crate) socket_addr: SocketAddr,
    cancellation: CancellationToken,
    server_task: Option<JoinHandle<()>>,
}

impl RemoteHttpsFixture {
    pub(crate) async fn start() -> Result<Self> {
        let (root_pem, root_der, leaf_pem, leaf_key_pem) = certificates()?;
        let tls = RustlsConfig::from_pem(
            format!("{leaf_pem}{root_pem}").into_bytes(),
            leaf_key_pem.into_bytes(),
        )
        .await?;
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let socket_addr = listener.local_addr()?;
        let cancellation = CancellationToken::new();
        let service = StreamableHttpService::new(
            || Ok(FixtureServer::default()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default()
                .with_allowed_hosts([fixture_allowed_host(socket_addr.port())])
                .with_cancellation_token(cancellation.child_token()),
        );
        let mcp_url = format!("https://{AUTHORITY}:{}{}", socket_addr.port(), MCP_PATH);
        let manifest_bytes = Arc::new(manifest_bytes(&mcp_url));
        let manifest_bytes_for_route = Arc::clone(&manifest_bytes);
        let router = axum::Router::new().nest_service(MCP_PATH, service).route(
            MANIFEST_PATH,
            axum::routing::get(move || manifest(Arc::clone(&manifest_bytes_for_route))),
        );
        let server = axum_server::from_tcp_rustls(listener, tls)?;
        let handle = axum_server::Handle::new();
        let shutdown = cancellation.clone();
        let server_task = tokio::spawn(async move {
            let server = server
                .handle(handle.clone())
                .serve(router.into_make_service());
            tokio::pin!(server);
            tokio::select! {
                result = &mut server => {
                    let _ = result;
                }
                _ = shutdown.cancelled() => {
                    handle.graceful_shutdown(None);
                    let _ = server.await;
                }
            }
        });
        let manifest_url = format!(
            "https://{AUTHORITY}:{}{}",
            socket_addr.port(),
            MANIFEST_PATH
        );
        Ok(Self {
            mcp_url: mcp_url.clone(),
            manifest_url,
            url: mcp_url,
            root_certificate_der: root_der,
            socket_addr,
            cancellation,
            server_task: Some(server_task),
        })
    }

    #[cfg(all(test, feature = "rustls-tls"))]
    pub(crate) fn https_manifest_capability(&self) -> FixtureHttpsCapability {
        FixtureHttpsCapability::from_fixture(self)
    }
}

fn manifest_bytes(mcp_url: &str) -> Vec<u8> {
    serde_json::json!({
        "schema_version": 1,
        "id": "lumina.fixture",
        "version": "1.0.0",
        "name": "Lumina HTTPS fixture",
        "description": "crate-internal HTTPS MCP fixture",
        "publisher": { "id": "lumina", "name": "Lumina" },
        "license": { "spdx": "MIT" },
        "capabilities": ["tools"],
        "permissions": [],
        "distribution": { "type": "remote_http" },
        "transport": { "type": "streamable_http", "url": mcp_url },
        "auth": { "type": "none" },
        "health_check": { "type": "mcp_initialize", "timeout_seconds": 1 },
        "owned_files": [],
        "uninstall": { "mode": "remove_owned_files_only", "preserve_user_data": true }
    })
    .to_string()
    .into_bytes()
}

async fn manifest(bytes: Arc<Vec<u8>>) -> ([(axum::http::HeaderName, &'static str); 1], Vec<u8>) {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes.as_ref().clone(),
    )
}

impl Drop for RemoteHttpsFixture {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.server_task.take() {
            task.abort();
        }
    }
}

fn certificates() -> Result<(String, Vec<u8>, String, String)> {
    let root_key = KeyPair::generate()?;
    let mut root_params = CertificateParams::default();
    root_params
        .distinguished_name
        .push(DnType::CommonName, "lumina MCP fixture CA");
    root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let root = root_params.self_signed(&root_key)?;

    let leaf_key = KeyPair::generate()?;
    let mut leaf_params = CertificateParams::new(vec![AUTHORITY.to_string()])?;
    leaf_params.subject_alt_names = vec![SanType::DnsName(AUTHORITY.try_into()?)];
    let issuer = Issuer::from_params(&root_params, &root_key);
    let leaf = leaf_params.signed_by(&leaf_key, &issuer)?;
    Ok((
        root.pem(),
        root.der().to_vec(),
        leaf.pem(),
        leaf_key.serialize_pem(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{fixture_allowed_host, AUTHORITY};

    #[test]
    fn fixture_allowlist_is_exact_authority() {
        let allowed = fixture_allowed_host(43123);

        assert_eq!(allowed, format!("{AUTHORITY}:43123"));
        assert_ne!(allowed, format!("{AUTHORITY}:43124"));
        assert_ne!(allowed, "mcp-fixture.example.com".to_string());
        assert_ne!(allowed, "localhost:43123".to_string());
    }

    #[test]
    fn manifest_binds_dynamic_mcp_endpoint() {
        let mcp_url = format!("https://{AUTHORITY}:43123{}", super::MCP_PATH);
        let bytes = super::manifest_bytes(&mcp_url);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(value["distribution"]["type"], "remote_http");
        assert_eq!(value["transport"]["type"], "streamable_http");
        assert_eq!(value["transport"]["url"], mcp_url);
        assert_eq!(value["auth"]["type"], "none");
    }
}
