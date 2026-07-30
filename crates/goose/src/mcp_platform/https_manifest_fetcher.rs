use super::https_manifest_policy::{
    redirect_chain_evidence_digest_input, validate_resolved_addresses, HttpsManifestFetchPolicy,
    HttpsManifestUrlError, ValidatedHttpsUrl,
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(feature = "integration-test-support")]
use super::{
    error::{McpPlatformError, McpPlatformErrorCode, McpPlatformResult},
    service::port::HttpsManifestSourceFetcher,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHttpsHost {
    pub url: ValidatedHttpsUrl,
    pub addresses: Vec<IpAddr>,
    pub evidence_digest: [u8; 32],
}

#[derive(Clone, PartialEq, Eq)]
pub struct HttpsManifestResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub location: Option<String>,
    pub body: Vec<u8>,
}

impl fmt::Debug for HttpsManifestResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpsManifestResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("content_length", &self.content_length)
            .field("location_present", &self.location.is_some())
            .field("location_length", &self.location.as_ref().map(String::len))
            .field("body_length", &self.body.len())
            .field("body_sha256", &Sha256::digest(&self.body).as_slice())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HttpsManifestFetchResult {
    pub requested: ValidatedHttpsUrl,
    pub final_url: ValidatedHttpsUrl,
    pub redirect_chain: Vec<ValidatedHttpsUrl>,
    pub raw_bytes: Vec<u8>,
    pub raw_sha256: [u8; 32],
    pub content_type: String,
    pub dns_evidence_digest: [u8; 32],
    pub redirect_chain_digest: [u8; 32],
}

impl fmt::Debug for HttpsManifestFetchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpsManifestFetchResult")
            .field("requested", &self.requested.redacted())
            .field("final_url", &self.final_url.redacted())
            .field(
                "redirect_count",
                &self.redirect_chain.len().saturating_sub(1),
            )
            .field("raw_bytes_length", &self.raw_bytes.len())
            .field("raw_sha256", &self.raw_sha256)
            .field("content_type", &self.content_type)
            .field("dns_evidence_digest", &self.dns_evidence_digest)
            .field("redirect_chain_digest", &self.redirect_chain_digest)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpsManifestFetchError {
    InvalidUrl,
    UnsafeResolution,
    Transport,
    RedirectLimit,
    InvalidRedirect,
    InvalidStatus,
    MissingContentType,
    UnsupportedContentType,
    ContentLengthTooLarge,
    BodyTooLarge,
}

impl fmt::Display for HttpsManifestFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUrl => "invalid HTTPS manifest URL",
            Self::UnsafeResolution => "HTTPS manifest host resolution is not permitted",
            Self::Transport => "HTTPS manifest transport failed",
            Self::RedirectLimit => "HTTPS manifest redirect limit exceeded",
            Self::InvalidRedirect => "invalid HTTPS manifest redirect",
            Self::InvalidStatus => "HTTPS manifest response status is not acceptable",
            Self::MissingContentType => "HTTPS manifest content type is missing",
            Self::UnsupportedContentType => "HTTPS manifest content type is not allowed",
            Self::ContentLengthTooLarge => "HTTPS manifest content length exceeds the limit",
            Self::BodyTooLarge => "HTTPS manifest body exceeds the limit",
        })
    }
}
impl std::error::Error for HttpsManifestFetchError {}

#[async_trait]
pub trait HttpsManifestResolver: Send + Sync {
    async fn resolve(
        &self,
        url: &ValidatedHttpsUrl,
        timeout: Duration,
    ) -> Result<Vec<IpAddr>, HttpsManifestFetchError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TokioHttpsManifestResolver;

fn dns_lookup_target(url: &ValidatedHttpsUrl) -> Result<(String, u16), HttpsManifestFetchError> {
    let request_url = url.request_url();
    let host = request_url
        .host_str()
        .ok_or(HttpsManifestFetchError::Transport)?
        .to_owned();
    let port = request_url
        .port_or_known_default()
        .ok_or(HttpsManifestFetchError::Transport)?;
    Ok((host, port))
}

#[async_trait]
impl HttpsManifestResolver for TokioHttpsManifestResolver {
    async fn resolve(
        &self,
        url: &ValidatedHttpsUrl,
        timeout: Duration,
    ) -> Result<Vec<IpAddr>, HttpsManifestFetchError> {
        let (host, port) = dns_lookup_target(url)?;
        let addresses = tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| HttpsManifestFetchError::Transport)?
            .map_err(|_| HttpsManifestFetchError::Transport)?
            .map(|address| address.ip())
            .collect();
        Ok(addresses)
    }
}

#[async_trait]
pub trait HttpsManifestTransport: Send + Sync {
    /// The implementation must connect only to `resolved.addresses` and fail closed if it cannot.
    async fn get(
        &self,
        url: &ValidatedHttpsUrl,
        resolved: &ResolvedHttpsHost,
        policy: &HttpsManifestFetchPolicy,
        timeout: Duration,
    ) -> Result<HttpsManifestResponse, HttpsManifestFetchError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReqwestHttpsManifestTransport;

#[async_trait]
impl HttpsManifestTransport for ReqwestHttpsManifestTransport {
    async fn get(
        &self,
        url: &ValidatedHttpsUrl,
        resolved: &ResolvedHttpsHost,
        policy: &HttpsManifestFetchPolicy,
        timeout: Duration,
    ) -> Result<HttpsManifestResponse, HttpsManifestFetchError> {
        let host = url
            .request_url()
            .host_str()
            .ok_or(HttpsManifestFetchError::Transport)?;
        let port = url
            .request_url()
            .port_or_known_default()
            .ok_or(HttpsManifestFetchError::Transport)?;
        let addrs: Vec<_> = resolved
            .addresses
            .iter()
            .copied()
            .map(|ip| SocketAddr::new(ip, port))
            .collect();
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(policy.connect_timeout().min(timeout))
            .timeout(timeout)
            .resolve_to_addrs(host, &addrs)
            .build()
            .map_err(|_| HttpsManifestFetchError::Transport)?;
        let response = tokio::time::timeout(
            policy.header_timeout().min(timeout),
            client.get(url.request_url().clone()).send(),
        )
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

pub struct HttpsManifestFetcher<R, T> {
    resolver: R,
    transport: T,
    policy: HttpsManifestFetchPolicy,
}

impl<R, T> HttpsManifestFetcher<R, T> {
    pub fn new(resolver: R, transport: T, policy: HttpsManifestFetchPolicy) -> Self {
        Self {
            resolver,
            transport,
            policy,
        }
    }
}

impl<R, T> HttpsManifestFetcher<R, T>
where
    R: HttpsManifestResolver,
    T: HttpsManifestTransport,
{
    pub async fn fetch(
        &self,
        raw_url: &str,
    ) -> Result<HttpsManifestFetchResult, HttpsManifestFetchError> {
        self.fetch_inner(raw_url, |current, resolved| {
            validate_resolved_addresses(current, &resolved.url, &resolved.addresses)
        })
        .await
    }

    async fn fetch_inner<V>(
        &self,
        raw_url: &str,
        validate: V,
    ) -> Result<HttpsManifestFetchResult, HttpsManifestFetchError>
    where
        V: Fn(&ValidatedHttpsUrl, &ResolvedHttpsHost) -> Result<(), HttpsManifestUrlError>,
    {
        let requested =
            ValidatedHttpsUrl::parse(raw_url).map_err(|_| HttpsManifestFetchError::InvalidUrl)?;
        let mut current = requested.clone();
        let mut chain = vec![current.clone()];
        let mut chain_evidence = Vec::new();
        let mut evidence = Sha256::new();
        let deadline = Instant::now() + self.policy.overall_timeout();
        let mut response;
        loop {
            let remaining = remaining_budget(deadline)?;
            let addresses =
                tokio::time::timeout(remaining, self.resolver.resolve(&current, remaining))
                    .await
                    .map_err(|_| HttpsManifestFetchError::Transport)??;
            let resolved_host = ResolvedHttpsHost {
                url: current.clone(),
                evidence_digest: digest_addresses(&addresses),
                addresses,
            };
            validate(&current, &resolved_host)
                .map_err(|_| HttpsManifestFetchError::UnsafeResolution)?;
            evidence.update(resolved_host.evidence_digest);
            let remaining = remaining_budget(deadline)?;
            response = tokio::time::timeout(
                remaining,
                self.transport
                    .get(&current, &resolved_host, &self.policy, remaining),
            )
            .await
            .map_err(|_| HttpsManifestFetchError::Transport)??;
            chain_evidence.push((current.clone(), response.status));
            if !(300..400).contains(&response.status) {
                break;
            }
            if chain.len() > self.policy.max_redirects() as usize {
                return Err(HttpsManifestFetchError::RedirectLimit);
            }
            let location = response
                .location
                .take()
                .ok_or(HttpsManifestFetchError::InvalidRedirect)?;
            let next_url = current
                .request_url()
                .join(&location)
                .map_err(|_| HttpsManifestFetchError::InvalidRedirect)?;
            let next = ValidatedHttpsUrl::parse(next_url.as_str())
                .map_err(|_| HttpsManifestFetchError::InvalidRedirect)?;
            current = next;
            chain.push(current.clone());
        }
        if response.status != 200 {
            return Err(HttpsManifestFetchError::InvalidStatus);
        }
        let content_type = response
            .content_type
            .take()
            .ok_or(HttpsManifestFetchError::MissingContentType)?;
        let content_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if content_type != "application/json" {
            return Err(HttpsManifestFetchError::UnsupportedContentType);
        }
        if response
            .content_length
            .is_some_and(|length| length > self.policy.max_body_bytes() as u64)
        {
            return Err(HttpsManifestFetchError::ContentLengthTooLarge);
        }
        if response.body.len() > self.policy.max_body_bytes() {
            return Err(HttpsManifestFetchError::BodyTooLarge);
        }
        Ok(HttpsManifestFetchResult {
            requested,
            final_url: current,
            redirect_chain: chain,
            raw_sha256: Sha256::digest(&response.body).into(),
            raw_bytes: response.body,
            content_type,
            dns_evidence_digest: evidence.finalize().into(),
            redirect_chain_digest: Sha256::digest(
                redirect_chain_evidence_digest_input(&chain_evidence).as_bytes(),
            )
            .into(),
        })
    }

    #[cfg(all(test, feature = "rustls-tls"))]
    pub(crate) async fn fetch_with_fixture_capability(
        &self,
        raw_url: &str,
        capability: &super::remote_https_test_policy::FixtureHttpsCapability,
    ) -> Result<HttpsManifestFetchResult, HttpsManifestFetchError> {
        self.fetch_inner(raw_url, |current, resolved| {
            capability
                .validate(current, resolved)
                .map_err(|_| HttpsManifestUrlError::UnsafeHost)
        })
        .await
    }
}

fn remaining_budget(deadline: Instant) -> Result<Duration, HttpsManifestFetchError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(HttpsManifestFetchError::Transport)
}

impl HttpsManifestFetchResult {
    pub fn redacted_preview(&self) -> String {
        format!(
            "{} -> {} ({} bytes, content-type {})",
            self.requested.redacted(),
            self.final_url.redacted(),
            self.raw_bytes.len(),
            self.content_type
        )
    }
}

#[cfg(feature = "integration-test-support")]
const INTEGRATION_FIXTURE_HOST: &str = "mcp-fixture.example.com";

#[cfg(feature = "integration-test-support")]
const INTEGRATION_MANIFEST_PATH: &str = "/manifest.json";

#[cfg(feature = "integration-test-support")]
pub fn new_integration_https_manifest_fetcher(
    manifest_url: String,
    socket_addr: SocketAddr,
    root_certificate_der: Vec<u8>,
) -> McpPlatformResult<Arc<dyn HttpsManifestSourceFetcher>> {
    Ok(Arc::new(build_integration_https_manifest_fetcher(
        manifest_url,
        socket_addr,
        root_certificate_der,
    )?))
}

#[cfg(feature = "integration-test-support")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationHttpsManifestFetchStage {
    InvalidUrl,
    UnsafeResolution,
    Transport,
    Redirect,
    Status,
    ContentType,
    Body,
}

#[cfg(feature = "integration-test-support")]
pub async fn diagnose_integration_https_manifest_fetch(
    manifest_url: String,
    socket_addr: SocketAddr,
    root_certificate_der: Vec<u8>,
) -> Result<(), IntegrationHttpsManifestFetchStage> {
    if ValidatedHttpsUrl::parse(&manifest_url).is_err() {
        return Err(IntegrationHttpsManifestFetchStage::InvalidUrl);
    }
    let fetcher = build_integration_https_manifest_fetcher(
        manifest_url.clone(),
        socket_addr,
        root_certificate_der,
    )
    .map_err(|_| IntegrationHttpsManifestFetchStage::Transport)?;
    fetcher
        .fetch_inner_with_capability(&manifest_url)
        .await
        .map(|_| ())
        .map_err(integration_fetch_stage)
}

#[cfg(feature = "integration-test-support")]
fn build_integration_https_manifest_fetcher(
    manifest_url: String,
    socket_addr: SocketAddr,
    root_certificate_der: Vec<u8>,
) -> McpPlatformResult<IntegrationHttpsManifestSourceFetcher> {
    let url = ValidatedHttpsUrl::parse(&manifest_url).map_err(|_| unsafe_fixture_url())?;
    let request = url.request_url();
    if request.port().is_none()
        || request.query().is_some()
        || request.host_str() != Some(INTEGRATION_FIXTURE_HOST)
        || request.path() != INTEGRATION_MANIFEST_PATH
        || request.port() != Some(socket_addr.port())
        || root_certificate_der.is_empty()
    {
        return Err(unsafe_fixture_url());
    }
    reqwest::Certificate::from_der(&root_certificate_der)
        .map_err(|_| fixture_connection_unavailable())?;
    Ok(IntegrationHttpsManifestSourceFetcher {
        fetcher: HttpsManifestFetcher::new(
            IntegrationHttpsResolver { socket_addr },
            IntegrationHttpsTransport {
                socket_addr,
                root_certificate_der,
                manifest_url,
            },
            HttpsManifestFetchPolicy::default(),
        ),
        manifest_url: url,
    })
}

#[cfg(feature = "integration-test-support")]
struct IntegrationHttpsManifestSourceFetcher {
    fetcher: HttpsManifestFetcher<IntegrationHttpsResolver, IntegrationHttpsTransport>,
    manifest_url: ValidatedHttpsUrl,
}

#[cfg(feature = "integration-test-support")]
impl IntegrationHttpsManifestSourceFetcher {
    async fn fetch_inner_with_capability(
        &self,
        requested_url: &str,
    ) -> Result<HttpsManifestFetchResult, HttpsManifestFetchError> {
        let manifest_url = &self.manifest_url;
        let socket_addr = self.fetcher.resolver.socket_addr;
        self.fetcher
            .fetch_inner(requested_url, |current, resolved| {
                let request = current.request_url();
                let exact_request = manifest_url.request_url();
                if request != exact_request
                    || resolved.url.request_url() != current.request_url()
                    || request.host_str() != Some(INTEGRATION_FIXTURE_HOST)
                    || request.port() != Some(socket_addr.port())
                    || request.path() != INTEGRATION_MANIFEST_PATH
                    || request.query().is_some()
                    || resolved.addresses != [socket_addr.ip()]
                {
                    return Err(HttpsManifestUrlError::UnsafeHost);
                }
                Ok(())
            })
            .await
    }
}

#[cfg(feature = "integration-test-support")]
const fn integration_fetch_stage(
    error: HttpsManifestFetchError,
) -> IntegrationHttpsManifestFetchStage {
    match error {
        HttpsManifestFetchError::InvalidUrl => IntegrationHttpsManifestFetchStage::InvalidUrl,
        HttpsManifestFetchError::UnsafeResolution => {
            IntegrationHttpsManifestFetchStage::UnsafeResolution
        }
        HttpsManifestFetchError::Transport => IntegrationHttpsManifestFetchStage::Transport,
        HttpsManifestFetchError::RedirectLimit | HttpsManifestFetchError::InvalidRedirect => {
            IntegrationHttpsManifestFetchStage::Redirect
        }
        HttpsManifestFetchError::InvalidStatus => IntegrationHttpsManifestFetchStage::Status,
        HttpsManifestFetchError::MissingContentType
        | HttpsManifestFetchError::UnsupportedContentType => {
            IntegrationHttpsManifestFetchStage::ContentType
        }
        HttpsManifestFetchError::ContentLengthTooLarge | HttpsManifestFetchError::BodyTooLarge => {
            IntegrationHttpsManifestFetchStage::Body
        }
    }
}

#[cfg(feature = "integration-test-support")]
#[derive(Clone, Copy)]
struct IntegrationHttpsResolver {
    socket_addr: SocketAddr,
}

#[cfg(feature = "integration-test-support")]
#[async_trait]
impl HttpsManifestResolver for IntegrationHttpsResolver {
    async fn resolve(
        &self,
        url: &ValidatedHttpsUrl,
        _timeout: Duration,
    ) -> Result<Vec<IpAddr>, HttpsManifestFetchError> {
        let request = url.request_url();
        if request.host_str() != Some(INTEGRATION_FIXTURE_HOST)
            || request.port() != Some(self.socket_addr.port())
            || request.path() != INTEGRATION_MANIFEST_PATH
            || request.query().is_some()
        {
            return Err(HttpsManifestFetchError::UnsafeResolution);
        }
        Ok(vec![self.socket_addr.ip()])
    }
}

#[cfg(feature = "integration-test-support")]
struct IntegrationHttpsTransport {
    socket_addr: SocketAddr,
    root_certificate_der: Vec<u8>,
    manifest_url: String,
}

#[cfg(feature = "integration-test-support")]
#[async_trait]
impl HttpsManifestTransport for IntegrationHttpsTransport {
    async fn get(
        &self,
        url: &ValidatedHttpsUrl,
        resolved: &ResolvedHttpsHost,
        policy: &HttpsManifestFetchPolicy,
        timeout: Duration,
    ) -> Result<HttpsManifestResponse, HttpsManifestFetchError> {
        if url.request_url().as_str() != self.manifest_url
            || resolved.addresses != [self.socket_addr.ip()]
        {
            return Err(HttpsManifestFetchError::UnsafeResolution);
        }
        let certificate = reqwest::Certificate::from_der(&self.root_certificate_der)
            .map_err(|_| HttpsManifestFetchError::Transport)?;
        let host = url
            .request_url()
            .host_str()
            .ok_or(HttpsManifestFetchError::Transport)?;
        let port = url
            .request_url()
            .port()
            .ok_or(HttpsManifestFetchError::Transport)?;
        let authority = format!("{host}:{port}");
        let host_header = reqwest::header::HeaderValue::try_from(&authority)
            .map_err(|_| HttpsManifestFetchError::Transport)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(policy.connect_timeout().min(timeout))
            .timeout(timeout)
            .resolve_to_addrs(host, &[self.socket_addr])
            .add_root_certificate(certificate)
            .build()
            .map_err(|_| HttpsManifestFetchError::Transport)?;
        let response = tokio::time::timeout(
            policy.header_timeout().min(timeout),
            client
                .get(url.request_url().clone())
                .header(reqwest::header::HOST, host_header)
                .send(),
        )
        .await
        .map_err(|_| HttpsManifestFetchError::Transport)?
        .map_err(|_| HttpsManifestFetchError::Transport)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let content_length = response.content_length();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if content_length.is_some_and(|length| length > policy.max_body_bytes() as u64) {
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

#[cfg(feature = "integration-test-support")]
#[async_trait]
impl HttpsManifestSourceFetcher for IntegrationHttpsManifestSourceFetcher {
    async fn fetch(&self, requested_url: &str) -> McpPlatformResult<HttpsManifestFetchResult> {
        if requested_url != self.manifest_url.request_url().as_str() {
            return Err(unsafe_fixture_url());
        }
        self.fetch_inner_with_capability(requested_url)
            .await
            .map_err(|_| {
                McpPlatformError::new(
                    McpPlatformErrorCode::InvalidRequest,
                    "HTTPS manifest fetch failed",
                )
            })
    }
}

#[cfg(feature = "integration-test-support")]
const fn unsafe_fixture_url() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::UnsafeUrl,
        "fixture HTTPS manifest URL was denied",
    )
}

#[cfg(feature = "integration-test-support")]
const fn fixture_connection_unavailable() -> McpPlatformError {
    McpPlatformError::new(
        McpPlatformErrorCode::RemoteHttpPolicyUnavailable,
        "fixture HTTPS manifest connection is unavailable",
    )
}

fn digest_addresses(addresses: &[IpAddr]) -> [u8; 32] {
    let mut digest = Sha256::new();
    for address in addresses {
        digest.update(address.to_string().as_bytes());
        digest.update([0]);
    }
    digest.finalize().into()
}

#[cfg(test)]
pub(crate) fn digest_addresses_for_test(addresses: &[IpAddr]) -> [u8; 32] {
    digest_addresses(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        time::Duration,
    };

    struct FakeResolver {
        answers: Mutex<VecDeque<Vec<IpAddr>>>,
        calls: Arc<Mutex<Vec<String>>>,
    }
    fn record(operation: &str, url: &ValidatedHttpsUrl) -> String {
        let request_url = url.request_url();
        format!(
            "{operation} https://{}:{}{}{}",
            request_url.host_str().unwrap(),
            request_url.port_or_known_default().unwrap(),
            request_url.path(),
            if request_url.query().is_some() {
                " (query-present)"
            } else {
                ""
            }
        )
    }
    #[async_trait]
    impl HttpsManifestResolver for FakeResolver {
        async fn resolve(
            &self,
            url: &ValidatedHttpsUrl,
            _: Duration,
        ) -> Result<Vec<IpAddr>, HttpsManifestFetchError> {
            self.calls.lock().unwrap().push(record("resolve", url));
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(HttpsManifestFetchError::Transport)
        }
    }
    struct FakeTransport {
        responses: Mutex<VecDeque<HttpsManifestResponse>>,
        calls: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl HttpsManifestTransport for FakeTransport {
        async fn get(
            &self,
            url: &ValidatedHttpsUrl,
            _: &ResolvedHttpsHost,
            _: &HttpsManifestFetchPolicy,
            _: Duration,
        ) -> Result<HttpsManifestResponse, HttpsManifestFetchError> {
            self.calls.lock().unwrap().push(record("transport", url));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(HttpsManifestFetchError::Transport)
        }
    }
    fn response(
        status: u16,
        location: Option<&str>,
        content_type: Option<&str>,
        body: &[u8],
    ) -> HttpsManifestResponse {
        HttpsManifestResponse {
            status,
            content_type: content_type.map(str::to_owned),
            content_length: Some(body.len() as u64),
            location: location.map(str::to_owned),
            body: body.to_vec(),
        }
    }

    #[test]
    fn tokio_resolver_uses_explicit_and_default_https_ports() {
        let explicit = ValidatedHttpsUrl::parse("https://example.test:8443/manifest").unwrap();
        assert_eq!(dns_lookup_target(&explicit).unwrap().1, 8443);

        let default = ValidatedHttpsUrl::parse("https://example.test/manifest").unwrap();
        assert_eq!(dns_lookup_target(&default).unwrap().1, 443);
    }

    #[test]
    fn tokio_resolver_transport_errors_are_without_details() {
        let error = HttpsManifestFetchError::Transport;
        assert_eq!(error, HttpsManifestFetchError::Transport);
        assert_eq!(error.to_string(), "HTTPS manifest transport failed");
    }

    #[test]
    fn test_only_digest_entry_matches_production_digest() {
        let addresses = [
            "8.8.8.8".parse::<IpAddr>().unwrap(),
            "2001:4860:4860::8888".parse::<IpAddr>().unwrap(),
        ];
        assert_eq!(
            digest_addresses_for_test(&addresses),
            digest_addresses(&addresses)
        );
    }

    fn fetcher(
        responses: Vec<HttpsManifestResponse>,
        calls: Arc<Mutex<Vec<String>>>,
        max_body: usize,
    ) -> HttpsManifestFetcher<FakeResolver, FakeTransport> {
        fetcher_with_answers(
            responses,
            VecDeque::from([
                vec!["8.8.8.8".parse().unwrap()],
                vec!["8.8.8.8".parse().unwrap()],
            ]),
            calls,
            max_body,
        )
    }
    fn fetcher_with_answers(
        responses: Vec<HttpsManifestResponse>,
        answers: VecDeque<Vec<IpAddr>>,
        calls: Arc<Mutex<Vec<String>>>,
        max_body: usize,
    ) -> HttpsManifestFetcher<FakeResolver, FakeTransport> {
        HttpsManifestFetcher::new(
            FakeResolver {
                answers: Mutex::new(answers),
                calls: calls.clone(),
            },
            FakeTransport {
                responses: Mutex::new(responses.into()),
                calls,
            },
            HttpsManifestFetchPolicy::new(
                2,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(2),
                max_body,
            )
            .unwrap(),
        )
    }
    #[tokio::test]
    async fn fake_redirects_resolve_each_hop_and_join_relative_location() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let result = fetcher(
            vec![
                response(302, Some("/next.json"), None, b""),
                response(200, None, Some("application/json"), b"{}"),
            ],
            calls.clone(),
            16,
        )
        .fetch("https://example.test/a?secret=query")
        .await
        .unwrap();
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[
                "resolve https://example.test:443/a (query-present)",
                "transport https://example.test:443/a (query-present)",
                "resolve https://example.test:443/next.json",
                "transport https://example.test:443/next.json",
            ]
        );
        assert_eq!(result.final_url.redacted(), "https://example.test:443");
        assert_eq!(result.final_url.request_url().path(), "/next.json");
        assert!(!result.redacted_preview().contains("secret"));
    }
    #[tokio::test]
    async fn fake_rejects_unsafe_dns() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let f = HttpsManifestFetcher::new(
            FakeResolver {
                answers: Mutex::new(VecDeque::from([vec!["127.0.0.1".parse().unwrap()]])),
                calls: calls.clone(),
            },
            FakeTransport {
                responses: Mutex::new(VecDeque::new()),
                calls,
            },
            HttpsManifestFetchPolicy::default(),
        );
        assert_eq!(
            f.fetch("https://example.test/?q=secret").await,
            Err(HttpsManifestFetchError::UnsafeResolution)
        );
    }

    #[tokio::test]
    async fn fake_rejects_unsafe_dns_on_second_hop_before_transport() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let f = HttpsManifestFetcher::new(
            FakeResolver {
                answers: Mutex::new(VecDeque::from([
                    vec!["8.8.8.8".parse().unwrap()],
                    vec!["192.168.1.1".parse().unwrap()],
                ])),
                calls: calls.clone(),
            },
            FakeTransport {
                responses: Mutex::new(VecDeque::from([response(
                    302,
                    Some("https://other.test/next"),
                    None,
                    b"",
                )])),
                calls: calls.clone(),
            },
            HttpsManifestFetchPolicy::default(),
        );
        assert_eq!(
            f.fetch("https://example.test/start").await,
            Err(HttpsManifestFetchError::UnsafeResolution)
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[
                "resolve https://example.test:443/start",
                "transport https://example.test:443/start",
                "resolve https://other.test:443/next",
            ]
        );
    }

    #[tokio::test]
    async fn digests_are_stable_and_redact_query_secrets() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let result = fetcher(
            vec![response(200, None, Some("application/json"), b"{}")],
            calls,
            16,
        )
        .fetch("https://example.test/manifest?token=secret")
        .await
        .unwrap();
        let repeat = fetcher(
            vec![response(200, None, Some("application/json"), b"{}")],
            Arc::new(Mutex::new(Vec::new())),
            16,
        )
        .fetch("https://example.test/manifest?token=secret")
        .await
        .unwrap();
        assert_eq!(result.raw_sha256, repeat.raw_sha256);
        assert_eq!(result.redirect_chain_digest, repeat.redirect_chain_digest);
        assert!(!result.redacted_preview().contains("secret"));
    }

    #[test]
    fn debug_and_preview_redact_sensitive_response_and_result_fields() {
        let response = response(
            302,
            Some("https://example.test/private/path?token=secret"),
            Some("application/json"),
            b"body contains secret",
        );
        let response_debug = format!("{response:?}");
        assert!(!response_debug.contains("secret"));
        assert!(!response_debug.contains("/private/path"));
        assert!(!response_debug.contains("body contains secret"));

        let url =
            ValidatedHttpsUrl::parse("https://example.test/private/path?token=secret").unwrap();
        let body = b"body contains secret".to_vec();
        let result = HttpsManifestFetchResult {
            requested: url.clone(),
            final_url: url,
            redirect_chain: Vec::new(),
            raw_sha256: Sha256::digest(&body).into(),
            raw_bytes: body,
            content_type: "application/json".to_owned(),
            dns_evidence_digest: [0; 32],
            redirect_chain_digest: [0; 32],
        };
        let result_debug = format!("{result:?}");
        assert!(!result_debug.contains("secret"));
        assert!(!result_debug.contains("/private/path"));
        assert!(!result_debug.contains("body contains secret"));
        assert!(!result.redacted_preview().contains("secret"));
        assert!(!result.redacted_preview().contains("/private/path"));
    }
    #[tokio::test]
    async fn fake_rejects_redirect_limit_content_type_and_body_bound() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            fetcher_with_answers(
                vec![
                    response(302, Some("https://other.test/b"), None, b""),
                    response(302, Some("https://other.test/c"), None, b""),
                    response(302, Some("https://other.test/d"), None, b""),
                ],
                VecDeque::from([
                    vec!["8.8.8.8".parse().unwrap()],
                    vec!["8.8.8.8".parse().unwrap()],
                    vec!["8.8.8.8".parse().unwrap()],
                ]),
                calls.clone(),
                16
            )
            .fetch("https://example.test/a")
            .await,
            Err(HttpsManifestFetchError::RedirectLimit)
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[
                "resolve https://example.test:443/a",
                "transport https://example.test:443/a",
                "resolve https://other.test:443/b",
                "transport https://other.test:443/b",
                "resolve https://other.test:443/c",
                "transport https://other.test:443/c",
            ]
        );
        assert_eq!(
            fetcher(
                vec![response(200, None, Some("text/plain"), b"{}")],
                calls.clone(),
                16
            )
            .fetch("https://example.test/a")
            .await,
            Err(HttpsManifestFetchError::UnsupportedContentType)
        );
        let mut oversized = response(200, None, Some("application/json"), b"too-large");
        oversized.content_length = None;
        assert_eq!(
            fetcher(vec![oversized], calls, 2)
                .fetch("https://example.test/a")
                .await,
            Err(HttpsManifestFetchError::BodyTooLarge)
        );
    }

    struct SlowResolver {
        calls: Arc<Mutex<usize>>,
        delay: Duration,
    }
    #[async_trait]
    impl HttpsManifestResolver for SlowResolver {
        async fn resolve(
            &self,
            _: &ValidatedHttpsUrl,
            _: Duration,
        ) -> Result<Vec<IpAddr>, HttpsManifestFetchError> {
            tokio::time::sleep(self.delay).await;
            *self.calls.lock().unwrap() += 1;
            Ok(vec!["8.8.8.8".parse().unwrap()])
        }
    }

    struct SlowTransport {
        calls: Arc<Mutex<usize>>,
        responses: Mutex<VecDeque<HttpsManifestResponse>>,
        delay: Duration,
    }
    #[async_trait]
    impl HttpsManifestTransport for SlowTransport {
        async fn get(
            &self,
            _: &ValidatedHttpsUrl,
            _: &ResolvedHttpsHost,
            _: &HttpsManifestFetchPolicy,
            _: Duration,
        ) -> Result<HttpsManifestResponse, HttpsManifestFetchError> {
            tokio::time::sleep(self.delay).await;
            *self.calls.lock().unwrap() += 1;
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(HttpsManifestFetchError::Transport)
        }
    }

    #[tokio::test]
    async fn fake_total_deadline_stops_slow_dns_before_transport() {
        let resolver_calls = Arc::new(Mutex::new(0));
        let transport_calls = Arc::new(Mutex::new(0));
        let policy = HttpsManifestFetchPolicy::new(
            1,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(100),
            16,
        )
        .unwrap();
        let result = HttpsManifestFetcher::new(
            SlowResolver {
                calls: resolver_calls,
                delay: Duration::from_millis(250),
            },
            SlowTransport {
                calls: transport_calls.clone(),
                responses: Mutex::new(VecDeque::new()),
                delay: Duration::ZERO,
            },
            policy,
        )
        .fetch("https://example.test/manifest?secret=value")
        .await;
        assert_eq!(result, Err(HttpsManifestFetchError::Transport));
        assert_eq!(*transport_calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn fake_redirects_share_one_total_deadline() {
        let calls = Arc::new(Mutex::new(0));
        let policy = HttpsManifestFetchPolicy::new(
            2,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(120),
            16,
        )
        .unwrap();
        let result = HttpsManifestFetcher::new(
            SlowResolver {
                calls: Arc::new(Mutex::new(0)),
                delay: Duration::ZERO,
            },
            SlowTransport {
                calls: calls.clone(),
                responses: Mutex::new(VecDeque::from([
                    response(302, Some("/next"), None, b""),
                    response(200, None, Some("application/json"), b"{}"),
                ])),
                delay: Duration::from_millis(80),
            },
            policy,
        )
        .fetch("https://example.test/start")
        .await;
        assert_eq!(result, Err(HttpsManifestFetchError::Transport));
        assert_eq!(*calls.lock().unwrap(), 1);
    }
}
