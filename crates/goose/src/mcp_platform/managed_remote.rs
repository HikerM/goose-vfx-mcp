use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use url::{Host, Url};

use super::error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult};

struct ManagedRemoteEndpoint {
    url: Url,
    host: String,
    port: u16,
}

#[derive(Debug, Clone)]
pub struct ManagedRemoteHttpClient {
    client: reqwest::Client,
    endpoint: Url,
}

impl ManagedRemoteHttpClient {
    pub(crate) fn client(&self) -> reqwest::Client {
        self.client.clone()
    }

    pub(crate) fn endpoint(&self) -> &Url {
        &self.endpoint
    }
}

#[async_trait]
pub trait RemoteHttpNetworkPolicy: Send + Sync {
    fn validate_endpoint(&self, endpoint: &str) -> McpPlatformResult<()>;

    async fn secure_client(
        &self,
        endpoint: &str,
        connect_timeout: Duration,
    ) -> McpPlatformResult<ManagedRemoteHttpClient> {
        let _ = (endpoint, connect_timeout);
        Err(connection_unavailable())
    }

    async fn validate_for_plan(
        &self,
        endpoint: &str,
        connect_timeout: Duration,
    ) -> McpPlatformResult<()>;
}

#[async_trait]
pub trait ManagedRemoteResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> McpPlatformResult<Vec<SocketAddr>>;
}

pub struct ManagedRemoteCredential {
    value: Vec<u8>,
}

impl ManagedRemoteCredential {
    pub fn new(value: Vec<u8>) -> McpPlatformResult<Self> {
        if value.is_empty() {
            return Err(credential_missing());
        }
        Ok(Self { value })
    }

    #[cfg(test)]
    fn expose(&self) -> &[u8] {
        &self.value
    }
}

impl std::fmt::Debug for ManagedRemoteCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ManagedRemoteCredential([REDACTED])")
    }
}

impl Drop for ManagedRemoteCredential {
    fn drop(&mut self) {
        self.value.fill(0);
    }
}

#[async_trait]
pub trait ManagedRemoteCredentialResolver: Send + Sync {
    async fn resolve(&self, opaque_handle: &str) -> McpPlatformResult<ManagedRemoteCredential>;
}

#[derive(Debug, Default)]
pub struct UnavailableManagedRemoteCredentialResolver;

#[async_trait]
impl ManagedRemoteCredentialResolver for UnavailableManagedRemoteCredentialResolver {
    async fn resolve(&self, _opaque_handle: &str) -> McpPlatformResult<ManagedRemoteCredential> {
        Err(credential_missing())
    }
}

#[derive(Debug, Default)]
pub struct TokioManagedRemoteResolver;

#[async_trait]
impl ManagedRemoteResolver for TokioManagedRemoteResolver {
    async fn resolve(&self, host: &str, port: u16) -> McpPlatformResult<Vec<SocketAddr>> {
        tokio::net::lookup_host((host, port))
            .await
            .map(|addresses| addresses.collect())
            .map_err(|_| connection_unavailable())
    }
}

trait ManagedRemoteDialer: Send + Sync {
    fn build_client(
        &self,
        endpoint: &ManagedRemoteEndpoint,
        peers: &[SocketAddr],
        connect_timeout: Duration,
    ) -> McpPlatformResult<reqwest::Client>;
}

#[derive(Debug, Default)]
struct ReqwestManagedRemoteDialer;

impl ManagedRemoteDialer for ReqwestManagedRemoteDialer {
    fn build_client(
        &self,
        endpoint: &ManagedRemoteEndpoint,
        peers: &[SocketAddr],
        connect_timeout: Duration,
    ) -> McpPlatformResult<reqwest::Client> {
        // The static DNS override remains attached to the pooled client, so reconnects cannot
        // fall back to a later system-DNS answer after the verified peers have been selected.
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .https_only(true)
            .connect_timeout(connect_timeout)
            .resolve_to_addrs(&endpoint.host, peers)
            .build()
            .map_err(|_| connection_unavailable())
    }
}

pub struct CoreManagedRemoteHttpNetworkPolicy {
    resolver: Arc<dyn ManagedRemoteResolver>,
    dialer: Arc<dyn ManagedRemoteDialer>,
}

impl Default for CoreManagedRemoteHttpNetworkPolicy {
    fn default() -> Self {
        Self::with_resolver(Arc::new(TokioManagedRemoteResolver))
    }
}

impl CoreManagedRemoteHttpNetworkPolicy {
    pub fn with_resolver(resolver: Arc<dyn ManagedRemoteResolver>) -> Self {
        Self {
            resolver,
            dialer: Arc::new(ReqwestManagedRemoteDialer),
        }
    }

    fn parse_endpoint(endpoint: &str) -> McpPlatformResult<ManagedRemoteEndpoint> {
        let url = Url::parse(endpoint).map_err(|_| unsafe_endpoint())?;
        let host = match url.host() {
            Some(Host::Domain(host)) => host.to_string(),
            Some(Host::Ipv4(_)) | Some(Host::Ipv6(_)) | None => return Err(unsafe_endpoint()),
        };
        let path = url.path().to_string();
        let encoded_path = path.to_ascii_lowercase();
        let local_namespace = [
            ".localhost",
            ".local",
            ".internal",
            ".localdomain",
            ".lan",
            ".home.arpa",
            ".test",
            ".invalid",
            ".example",
            ".onion",
        ];
        if endpoint.trim() != endpoint
            || url.as_str() != endpoint
            || url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port().is_some()
            || host.is_empty()
            || host.ends_with('.')
            || !host.contains('.')
            || host.eq_ignore_ascii_case("localhost")
            || local_namespace
                .iter()
                .any(|suffix| host == suffix.trim_start_matches('.') || host.ends_with(suffix))
            || host.starts_with("xn--")
            || host.contains(".xn--")
            || path.contains('\\')
            || encoded_path.contains("%2e")
            || !path.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~')
            })
            || endpoint.chars().any(char::is_control)
        {
            return Err(unsafe_endpoint());
        }
        Ok(ManagedRemoteEndpoint {
            url,
            host,
            port: 443,
        })
    }

    async fn prepare(
        &self,
        endpoint: &str,
        connect_timeout: Duration,
    ) -> McpPlatformResult<ManagedRemoteHttpClient> {
        let endpoint = Self::parse_endpoint(endpoint)?;
        let peers = tokio::time::timeout(
            connect_timeout,
            self.resolver.resolve(&endpoint.host, endpoint.port),
        )
        .await
        .map_err(|_| connection_unavailable())??;
        if peers.is_empty()
            || peers
                .iter()
                .any(|peer| peer.port() != endpoint.port || !is_globally_routable(peer.ip()))
        {
            return Err(unsafe_endpoint());
        }
        let client = self
            .dialer
            .build_client(&endpoint, &peers, connect_timeout)?;
        Ok(ManagedRemoteHttpClient {
            client,
            endpoint: endpoint.url,
        })
    }
}

#[async_trait]
impl RemoteHttpNetworkPolicy for CoreManagedRemoteHttpNetworkPolicy {
    fn validate_endpoint(&self, endpoint: &str) -> McpPlatformResult<()> {
        Self::parse_endpoint(endpoint).map(drop)
    }

    async fn secure_client(
        &self,
        endpoint: &str,
        connect_timeout: Duration,
    ) -> McpPlatformResult<ManagedRemoteHttpClient> {
        self.prepare(endpoint, connect_timeout).await
    }

    async fn validate_for_plan(
        &self,
        endpoint: &str,
        connect_timeout: Duration,
    ) -> McpPlatformResult<()> {
        self.prepare(endpoint, connect_timeout).await.map(drop)
    }
}

#[derive(Debug, Default)]
pub struct UnavailableRemoteHttpNetworkPolicy;

#[async_trait]
impl RemoteHttpNetworkPolicy for UnavailableRemoteHttpNetworkPolicy {
    fn validate_endpoint(&self, endpoint: &str) -> McpPlatformResult<()> {
        CoreManagedRemoteHttpNetworkPolicy::parse_endpoint(endpoint).map(drop)
    }

    async fn secure_client(
        &self,
        _endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<ManagedRemoteHttpClient> {
        Err(connection_unavailable())
    }

    async fn validate_for_plan(
        &self,
        _endpoint: &str,
        _connect_timeout: Duration,
    ) -> McpPlatformResult<()> {
        Err(connection_unavailable())
    }
}

fn is_globally_routable(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_global_ipv4(address),
        IpAddr::V6(address) => is_global_ipv6(address),
    }
}

fn is_global_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, d] = address.octets();
    !matches!(
        (a, b, c, d),
        (0, _, _, _)
            | (10, _, _, _)
            | (100, 64..=127, _, _)
            | (127, _, _, _)
            | (169, 254, _, _)
            | (172, 16..=31, _, _)
            | (192, 0, 0, _)
            | (192, 0, 2, _)
            | (192, 88, 99, _)
            | (192, 168, _, _)
            | (198, 18..=19, _, _)
            | (198, 51, 100, _)
            | (203, 0, 113, _)
            | (224..=255, _, _, _)
    )
}

fn is_global_ipv6(address: Ipv6Addr) -> bool {
    if address.to_ipv4_mapped().is_some() {
        return false;
    }
    let segments = address.segments();
    let first = segments[0];
    let in_global_unicast = (first & 0xe000) == 0x2000;
    let unique_local = (first & 0xfe00) == 0xfc00;
    let link_local = (first & 0xffc0) == 0xfe80;
    let multicast = (first & 0xff00) == 0xff00;
    let documentation = first == 0x2001 && segments[1] == 0x0db8;
    let special_2001 = first == 0x2001 && (segments[1] & 0xfe00) == 0;
    let documentation_3fff = first == 0x3fff && (segments[1] & 0xf000) == 0;
    let six_to_four = first == 0x2002;
    in_global_unicast
        && !address.is_unspecified()
        && !address.is_loopback()
        && !unique_local
        && !link_local
        && !multicast
        && !documentation
        && !special_2001
        && !documentation_3fff
        && !six_to_four
}

const fn unsafe_endpoint() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::UnsafeUrl,
        "remote HTTP endpoint was denied by the managed connection policy",
    )
}

const fn connection_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RemoteHttpPolicyUnavailable,
        "managed remote HTTP connection is unavailable",
    )
}

const fn credential_missing() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::CredentialMissing,
        "managed remote HTTP credential is unavailable",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct StaticResolver(Vec<SocketAddr>);

    #[async_trait]
    impl ManagedRemoteResolver for StaticResolver {
        async fn resolve(&self, _host: &str, _port: u16) -> McpPlatformResult<Vec<SocketAddr>> {
            Ok(self.0.clone())
        }
    }

    struct RecordingDialer {
        peers: Mutex<Vec<SocketAddr>>,
        endpoint: Mutex<Option<(Url, String, u16)>>,
    }

    impl ManagedRemoteDialer for RecordingDialer {
        fn build_client(
            &self,
            endpoint: &ManagedRemoteEndpoint,
            peers: &[SocketAddr],
            _connect_timeout: Duration,
        ) -> McpPlatformResult<reqwest::Client> {
            *self.peers.lock().unwrap() = peers.to_vec();
            *self.endpoint.lock().unwrap() =
                Some((endpoint.url.clone(), endpoint.host.clone(), endpoint.port));
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .https_only(true)
                .build()
                .map_err(|_| connection_unavailable())
        }
    }

    #[tokio::test]
    async fn verified_endpoint_and_peers_are_preserved_for_the_dialer() {
        let peer = "93.184.216.34:443".parse().unwrap();
        let dialer = Arc::new(RecordingDialer {
            peers: Mutex::new(Vec::new()),
            endpoint: Mutex::new(None),
        });
        let policy = CoreManagedRemoteHttpNetworkPolicy {
            resolver: Arc::new(StaticResolver(vec![peer])),
            dialer: dialer.clone(),
        };
        let client = policy
            .secure_client("https://mcp.example.com/v1", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(*dialer.peers.lock().unwrap(), vec![peer]);
        assert_eq!(client.endpoint().as_str(), "https://mcp.example.com/v1");
        assert_eq!(
            *dialer.endpoint.lock().unwrap(),
            Some((
                Url::parse("https://mcp.example.com/v1").unwrap(),
                "mcp.example.com".to_string(),
                443,
            ))
        );
    }

    #[tokio::test]
    async fn any_unsafe_dns_answer_prevents_dialer_use() {
        let dialer = Arc::new(RecordingDialer {
            peers: Mutex::new(Vec::new()),
            endpoint: Mutex::new(None),
        });
        let policy = CoreManagedRemoteHttpNetworkPolicy {
            resolver: Arc::new(StaticResolver(vec![
                "93.184.216.34:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap(),
            ])),
            dialer: dialer.clone(),
        };
        let error = policy
            .secure_client("https://mcp.example.com/v1", Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::UnsafeUrl);
        assert!(dialer.peers.lock().unwrap().is_empty());
    }

    #[test]
    fn endpoint_contract_is_canonical_and_excludes_local_namespaces() {
        let allowed = ["https://mcp.example.com/", "https://mcp.example.com/v1"];
        for endpoint in allowed {
            assert!(CoreManagedRemoteHttpNetworkPolicy::parse_endpoint(endpoint).is_ok());
        }

        let denied = [
            "http://mcp.example.com/",
            "https://user@mcp.example.com/",
            "https://mcp.example.com/?token=value",
            "https://mcp.example.com/#fragment",
            "https://mcp.example.com:443/",
            "https://mcp.example.com",
            "https://127.0.0.1/",
            "https://localhost/",
            "https://printer.local/",
            "https://service.internal/",
            "https://router.home.arpa/",
            "https://home.arpa/",
            "https://service.example/",
            "https://hidden.onion/",
            "https://xn--bcher-kva.example/",
            "https://bücher.example/",
            "https://mcp.example.com/%2fadmin",
        ];
        for endpoint in denied {
            assert!(
                CoreManagedRemoteHttpNetworkPolicy::parse_endpoint(endpoint).is_err(),
                "accepted {endpoint}"
            );
        }
    }

    #[test]
    fn special_ip_ranges_are_never_globally_routable() {
        for address in [
            "0.0.0.0",
            "10.0.0.1",
            "100.100.100.200",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.0.2.1",
            "192.168.0.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "::ffff:8.8.8.8",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2002:0808:0808::1",
            "3fff::1",
        ] {
            assert!(
                !is_globally_routable(address.parse().unwrap()),
                "allowed {address}"
            );
        }
        for address in ["8.8.8.8", "2606:4700:4700::1111"] {
            assert!(
                is_globally_routable(address.parse().unwrap()),
                "denied {address}"
            );
        }
    }

    #[tokio::test]
    async fn credential_seam_is_opaque_redacted_and_unavailable_by_default() {
        let resolver = UnavailableManagedRemoteCredentialResolver;
        let error = resolver.resolve("opaque-handle").await.unwrap_err();
        assert_eq!(error.code(), McpPlatformErrorCode::CredentialMissing);

        let credential = ManagedRemoteCredential::new(b"canary-secret".to_vec()).unwrap();
        assert_eq!(
            format!("{credential:?}"),
            "ManagedRemoteCredential([REDACTED])"
        );
        assert_eq!(credential.expose(), b"canary-secret");
    }
}
