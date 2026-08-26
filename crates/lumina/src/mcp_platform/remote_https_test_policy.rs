#![cfg(all(test, feature = "rustls-tls"))]

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};
use super::https_manifest_fetcher::{
    HttpsManifestFetchError, HttpsManifestFetchResult, HttpsManifestFetcher, HttpsManifestResolver,
    HttpsManifestResponse, HttpsManifestTransport, ResolvedHttpsHost,
};
use super::https_manifest_policy::{HttpsManifestFetchPolicy, ValidatedHttpsUrl};
use super::managed_remote::{ManagedRemoteHttpClient, RemoteHttpNetworkPolicy};
use async_trait::async_trait;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use url::Url;

use super::remote_https_fixture::RemoteHttpsFixture;
use super::service::port::HttpsManifestSourceFetcher;

const MANIFEST_PATH: &str = "/manifest.json";
const MCP_PATH: &str = "/mcp";

#[derive(Debug, Clone)]
pub(crate) struct FixtureHttpsCapability {
    url: String,
    host: String,
    port: u16,
    path: String,
    socket_addr: SocketAddr,
    root_certificate_der: Vec<u8>,
}

impl FixtureHttpsCapability {
    pub(crate) fn from_fixture(fixture: &RemoteHttpsFixture) -> Self {
        Self {
            url: fixture.manifest_url.clone(),
            host: "mcp-fixture.example.com".to_owned(),
            port: fixture.socket_addr.port(),
            path: MANIFEST_PATH.to_owned(),
            socket_addr: fixture.socket_addr,
            root_certificate_der: fixture.root_certificate_der.clone(),
        }
    }

    pub(crate) fn validate(
        &self,
        current: &ValidatedHttpsUrl,
        resolved: &ResolvedHttpsHost,
    ) -> Result<(), ()> {
        let url = current.request_url();
        if current != &resolved.url
            || self.root_certificate_der.is_empty()
            || url.as_str() != self.url
            || url.scheme() != "https"
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str() != Some(self.host.as_str())
            || url.port() != Some(self.port)
            || url.path() != self.path.as_str()
            || resolved.addresses != [self.socket_addr.ip()]
        {
            return Err(());
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FixtureRemoteHttpNetworkPolicy {
    url: String,
    authority: String,
    socket_addr: SocketAddr,
    root_certificate_der: Vec<u8>,
}

impl FixtureRemoteHttpNetworkPolicy {
    pub(crate) fn new(fixture: &RemoteHttpsFixture) -> Self {
        Self {
            url: fixture.mcp_url.clone(),
            authority: format!("mcp-fixture.example.com:{}", fixture.socket_addr.port()),
            socket_addr: fixture.socket_addr,
            root_certificate_der: fixture.root_certificate_der.clone(),
        }
    }

    fn validate(&self, endpoint: &str) -> McpPlatformResult<Url> {
        let url = Url::parse(endpoint).map_err(|_| unsafe_endpoint())?;
        if endpoint != self.url
            || url.scheme() != "https"
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str() != Some("mcp-fixture.example.com")
            || url.port() != Some(self.socket_addr.port())
            || url.path() != MCP_PATH
        {
            return Err(unsafe_endpoint());
        }
        Ok(url)
    }

    fn client(
        &self,
        endpoint: &str,
        timeout: Duration,
    ) -> McpPlatformResult<ManagedRemoteHttpClient> {
        let url = self.validate(endpoint)?;
        let certificate = reqwest::Certificate::from_der(&self.root_certificate_der)
            .map_err(|_| connection_unavailable())?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(timeout)
            .resolve_to_addrs(&self.authority, &[self.socket_addr])
            .add_root_certificate(certificate)
            .build()
            .map_err(|_| connection_unavailable())?;
        Ok(ManagedRemoteHttpClient {
            client,
            endpoint: url,
        })
    }
}

#[async_trait]
impl RemoteHttpNetworkPolicy for FixtureRemoteHttpNetworkPolicy {
    fn validate_endpoint(&self, endpoint: &str) -> McpPlatformResult<()> {
        self.validate(endpoint).map(drop)
    }

    async fn secure_client(
        &self,
        endpoint: &str,
        timeout: Duration,
    ) -> McpPlatformResult<ManagedRemoteHttpClient> {
        self.client(endpoint, timeout)
    }

    async fn validate_for_plan(&self, endpoint: &str, _timeout: Duration) -> McpPlatformResult<()> {
        self.validate_endpoint(endpoint)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FixtureHttpsResolver {
    socket_addr: SocketAddr,
}

impl FixtureHttpsResolver {
    pub(crate) fn new(fixture: &RemoteHttpsFixture) -> Self {
        Self {
            socket_addr: fixture.socket_addr,
        }
    }
}

#[async_trait]
impl HttpsManifestResolver for FixtureHttpsResolver {
    async fn resolve(
        &self,
        url: &ValidatedHttpsUrl,
        _timeout: Duration,
    ) -> Result<Vec<IpAddr>, HttpsManifestFetchError> {
        if url.request_url().host_str() != Some("mcp-fixture.example.com")
            || url.request_url().port() != Some(self.socket_addr.port())
            || url.request_url().path() != MANIFEST_PATH
        {
            return Err(HttpsManifestFetchError::UnsafeResolution);
        }
        Ok(vec![self.socket_addr.ip()])
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FixtureHttpsTransport {
    root_certificate_der: Vec<u8>,
    port: u16,
}

impl FixtureHttpsTransport {
    pub(crate) fn new(fixture: &RemoteHttpsFixture) -> Self {
        Self {
            root_certificate_der: fixture.root_certificate_der.clone(),
            port: fixture.socket_addr.port(),
        }
    }

    fn validate_url(&self, url: &ValidatedHttpsUrl) -> Result<(), HttpsManifestFetchError> {
        let request = url.request_url();
        if request.scheme() != "https"
            || request.username() != ""
            || request.password().is_some()
            || request.query().is_some()
            || request.fragment().is_some()
            || request.host_str() != Some("mcp-fixture.example.com")
            || request.port() != Some(self.port)
            || request.path() != MANIFEST_PATH
        {
            return Err(HttpsManifestFetchError::UnsafeResolution);
        }
        Ok(())
    }
}

#[async_trait]
impl HttpsManifestTransport for FixtureHttpsTransport {
    async fn get(
        &self,
        url: &ValidatedHttpsUrl,
        resolved: &ResolvedHttpsHost,
        policy: &HttpsManifestFetchPolicy,
        timeout: Duration,
    ) -> Result<HttpsManifestResponse, HttpsManifestFetchError> {
        self.validate_url(url)?;
        let host = url
            .request_url()
            .host_str()
            .ok_or(HttpsManifestFetchError::Transport)?;
        let port = url
            .request_url()
            .port()
            .ok_or(HttpsManifestFetchError::Transport)?;
        let addresses: Vec<_> = resolved
            .addresses
            .iter()
            .map(|ip| SocketAddr::new(*ip, port))
            .collect();
        let certificate = reqwest::Certificate::from_der(&self.root_certificate_der)
            .map_err(|_| HttpsManifestFetchError::Transport)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(policy.connect_timeout().min(timeout))
            .timeout(timeout)
            .resolve_to_addrs(host, &addresses)
            .add_root_certificate(certificate)
            .build()
            .map_err(|_| HttpsManifestFetchError::Transport)?;
        let response = tokio::time::timeout(timeout, client.get(url.request_url().clone()).send())
            .await
            .map_err(|_| HttpsManifestFetchError::Transport)?
            .map_err(|_| HttpsManifestFetchError::Transport)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let content_length = response.content_length();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if content_length.is_some_and(|n| n > policy.max_body_bytes() as u64) {
            return Err(HttpsManifestFetchError::ContentLengthTooLarge);
        }
        let mut body = Vec::new();
        use futures::StreamExt;
        let mut stream = response.bytes_stream();
        while let Some(chunk) =
            tokio::time::timeout(policy.body_timeout().min(timeout), stream.next())
                .await
                .map_err(|_| HttpsManifestFetchError::Transport)?
        {
            let chunk = chunk.map_err(|_| HttpsManifestFetchError::Transport)?;
            if body.len().saturating_add(chunk.len()) > policy.max_body_bytes() {
                return Err(HttpsManifestFetchError::BodyTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpsManifestResponse {
            status,
            content_type,
            content_length,
            location,
            body,
        })
    }
}

pub(crate) struct FixtureHttpsManifestSourceFetcher {
    fetcher: HttpsManifestFetcher<FixtureHttpsResolver, FixtureHttpsTransport>,
    capability: FixtureHttpsCapability,
}

impl FixtureHttpsManifestSourceFetcher {
    pub(crate) fn new(fixture: &RemoteHttpsFixture) -> Self {
        Self {
            fetcher: HttpsManifestFetcher::new(
                FixtureHttpsResolver::new(fixture),
                FixtureHttpsTransport::new(fixture),
                HttpsManifestFetchPolicy::default(),
            ),
            capability: FixtureHttpsCapability::from_fixture(fixture),
        }
    }
}

#[async_trait]
impl HttpsManifestSourceFetcher for FixtureHttpsManifestSourceFetcher {
    async fn fetch(&self, requested_url: &str) -> McpPlatformResult<HttpsManifestFetchResult> {
        self.fetcher
            .fetch_with_fixture_capability(requested_url, &self.capability)
            .await
            .map_err(|_| {
                McpPlatformError::new(
                    McpPlatformErrorCode::InvalidRequest,
                    "HTTPS manifest fetch failed",
                )
            })
    }
}

const fn unsafe_endpoint() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::UnsafeUrl,
        "fixture remote HTTP endpoint was denied",
    )
}
const fn connection_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RemoteHttpPolicyUnavailable,
        "fixture remote HTTP connection is unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_platform::manifest::{parse_manifest, Auth, Distribution, Transport};

    fn capability() -> FixtureHttpsCapability {
        FixtureHttpsCapability {
            url: "https://mcp-fixture.example.com:43123/manifest.json".to_owned(),
            host: "mcp-fixture.example.com".to_owned(),
            port: 43123,
            path: "/manifest.json".to_owned(),
            socket_addr: "127.0.0.1:43123".parse().unwrap(),
            root_certificate_der: vec![1, 2, 3],
        }
    }

    fn resolved(url: &str, addresses: Vec<IpAddr>) -> ResolvedHttpsHost {
        ResolvedHttpsHost {
            url: ValidatedHttpsUrl::parse(url).unwrap(),
            addresses,
            evidence_digest: [0; 32],
        }
    }

    #[test]
    fn fixture_capability_requires_exact_binding_and_single_address() {
        let capability = capability();
        let url = ValidatedHttpsUrl::parse(&capability.url).unwrap();
        assert!(capability
            .validate(
                &url,
                &resolved(&capability.url, vec!["127.0.0.1".parse().unwrap()])
            )
            .is_ok());
        assert!(capability
            .validate(
                &url,
                &resolved(
                    &capability.url,
                    vec!["127.0.0.1".parse().unwrap(), "127.0.0.2".parse().unwrap()]
                )
            )
            .is_err());
        for drift in [
            "https://mcp-fixture.example.com:43124/manifest.json",
            "https://mcp-fixture.example.com:43123/mcp",
            "https://localhost:43123/manifest.json",
        ] {
            let drifted = ValidatedHttpsUrl::parse(drift).unwrap();
            assert!(capability
                .validate(
                    &drifted,
                    &resolved(drift, vec!["127.0.0.1".parse().unwrap()])
                )
                .is_err());
        }
    }

    fn policy() -> FixtureRemoteHttpNetworkPolicy {
        FixtureRemoteHttpNetworkPolicy {
            url: "https://mcp-fixture.example.com:43123/mcp".to_string(),
            authority: "mcp-fixture.example.com:43123".to_string(),
            socket_addr: "127.0.0.1:43123".parse().unwrap(),
            root_certificate_der: Vec::new(),
        }
    }

    #[test]
    fn rejects_endpoint_drift() {
        let policy = policy();
        for url in [
            "http://mcp-fixture.example.com:43123/mcp",
            "https://mcp-fixture.example.com:43124/mcp",
            "https://mcp-fixture.example.com:43123/other",
            "https://other.example.com:43123/mcp",
            "https://user@mcp-fixture.example.com:43123/mcp",
            "https://mcp-fixture.example.com:43123/mcp?x=1",
            "https://mcp-fixture.example.com:43123/mcp#fragment",
            "https://127.0.0.1:43123/mcp",
        ] {
            assert!(
                policy.validate_endpoint(url).is_err(),
                "accepted drifted URL: {url}"
            );
        }
        assert!(policy.validate_endpoint(&policy.url).is_ok());
        assert!(policy
            .validate_endpoint("https://mcp-fixture.example.com:43123/manifest.json")
            .is_err());
    }

    #[test]
    fn fixture_endpoints_are_distinct_and_cross_boundaries_are_rejected() {
        let capability = capability();
        let policy = policy();
        assert_ne!(capability.url, policy.url);
        let cross_url = ValidatedHttpsUrl::parse(&policy.url).unwrap();
        assert!(capability
            .validate(
                &cross_url,
                &resolved(&policy.url, vec!["127.0.0.1".parse().unwrap()])
            )
            .is_err());
        assert!(policy.validate_endpoint(&capability.url).is_err());
    }

    fn install_test_crypto_provider() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    #[tokio::test]
    async fn fetches_real_tls_manifest_and_preserves_endpoint_bindings() {
        install_test_crypto_provider();
        let first = RemoteHttpsFixture::start().await.unwrap();
        let second = RemoteHttpsFixture::start().await.unwrap();
        assert_ne!(first.socket_addr.port(), second.socket_addr.port());
        assert_ne!(first.manifest_url, second.manifest_url);
        assert_ne!(first.mcp_url, second.mcp_url);

        let fetcher = FixtureHttpsManifestSourceFetcher::new(&first);
        let fetched = fetcher.fetch(&first.manifest_url).await.unwrap();
        assert_eq!(fetched.requested.request_url().as_str(), first.manifest_url);
        assert_eq!(fetched.final_url.request_url().as_str(), first.manifest_url);
        assert_eq!(fetched.content_type, "application/json");
        assert!(!fetched.raw_bytes.is_empty());

        let manifest = parse_manifest(&fetched.raw_bytes).unwrap();
        assert!(matches!(
            manifest.manifest().distribution,
            Distribution::RemoteHttp
        ));
        assert!(matches!(manifest.manifest().auth, Auth::None));
        match &manifest.manifest().transport {
            Transport::StreamableHttp { url, .. } => assert_eq!(url, &first.mcp_url),
            Transport::Stdio { .. } => panic!("fixture manifest did not declare HTTP transport"),
        }

        assert!(fetcher.fetch(&first.mcp_url).await.is_err());
        assert!(fetcher.fetch(&second.manifest_url).await.is_err());
        let policy = FixtureRemoteHttpNetworkPolicy::new(&first);
        assert!(policy.validate_endpoint(&first.manifest_url).is_err());
        assert!(policy.validate_endpoint(&first.mcp_url).is_ok());
        assert!(policy.validate_endpoint(&second.mcp_url).is_err());

        let second_fetcher = FixtureHttpsManifestSourceFetcher::new(&second);
        let second_fetched = second_fetcher.fetch(&second.manifest_url).await.unwrap();
        assert_eq!(
            second_fetched.requested.request_url().as_str(),
            second.manifest_url
        );
        assert_eq!(
            second_fetched.final_url.request_url().as_str(),
            second.manifest_url
        );
        let second_manifest = parse_manifest(&second_fetched.raw_bytes).unwrap();
        match &second_manifest.manifest().transport {
            Transport::StreamableHttp { url, .. } => assert_eq!(url, &second.mcp_url),
            Transport::Stdio { .. } => panic!("fixture manifest did not declare HTTP transport"),
        }
        assert!(second_fetcher.fetch(&first.manifest_url).await.is_err());
        let second_policy = FixtureRemoteHttpNetworkPolicy::new(&second);
        assert!(second_policy
            .validate_endpoint(&second.manifest_url)
            .is_err());
        assert!(second_policy.validate_endpoint(&second.mcp_url).is_ok());
        assert!(second_policy.validate_endpoint(&first.mcp_url).is_err());
        assert_ne!(second.mcp_url, first.mcp_url);
    }
}
