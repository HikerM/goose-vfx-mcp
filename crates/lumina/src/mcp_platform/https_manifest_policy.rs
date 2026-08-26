use sha2::{Digest, Sha256};
use std::{fmt, net::IpAddr, time::Duration};
use url::Url;

const MAX_REDIRECTS: u8 = 10;
const MAX_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpsManifestFetchPolicy {
    max_redirects: u8,
    body_timeout: Duration,
    header_timeout: Duration,
    connect_timeout: Duration,
    overall_timeout: Duration,
    max_body_bytes: usize,
}

impl HttpsManifestFetchPolicy {
    pub fn new(
        max_redirects: u8,
        body_timeout: Duration,
        header_timeout: Duration,
        connect_timeout: Duration,
        overall_timeout: Duration,
        max_body_bytes: usize,
    ) -> Option<Self> {
        let p = Self {
            max_redirects,
            body_timeout,
            header_timeout,
            connect_timeout,
            overall_timeout,
            max_body_bytes,
        };
        (max_redirects <= MAX_REDIRECTS
            && max_body_bytes > 0
            && max_body_bytes <= MAX_BODY_BYTES
            && [
                body_timeout,
                header_timeout,
                connect_timeout,
                overall_timeout,
            ]
            .iter()
            .all(|d| !d.is_zero() && *d <= MAX_TIMEOUT)
            && overall_timeout >= body_timeout.max(header_timeout).max(connect_timeout))
        .then_some(p)
    }
    pub fn max_redirects(&self) -> u8 {
        self.max_redirects
    }
    pub fn body_timeout(&self) -> Duration {
        self.body_timeout
    }
    pub fn header_timeout(&self) -> Duration {
        self.header_timeout
    }
    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }
    pub fn overall_timeout(&self) -> Duration {
        self.overall_timeout
    }
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }
}

impl Default for HttpsManifestFetchPolicy {
    fn default() -> Self {
        Self::new(
            5,
            Duration::from_secs(15),
            Duration::from_secs(10),
            Duration::from_secs(5),
            Duration::from_secs(30),
            4 * 1024 * 1024,
        )
        .unwrap()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpsManifestUrlError {
    InvalidUrl,
    NotHttps,
    MissingHost,
    UserInfo,
    Fragment,
    UnsafeHost,
    HostMismatch,
}
impl fmt::Display for HttpsManifestUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUrl => "invalid URL",
            Self::NotHttps => "URL must use HTTPS",
            Self::MissingHost => "URL host is required",
            Self::UserInfo => "URL userinfo is not permitted",
            Self::Fragment => "URL fragment is not permitted",
            Self::UnsafeHost => "URL host is not permitted",
            Self::HostMismatch => "resolved host does not match URL host",
        })
    }
}
impl std::error::Error for HttpsManifestUrlError {}

#[derive(Clone, PartialEq, Eq)]
pub struct ValidatedHttpsUrl(Url);

impl fmt::Debug for ValidatedHttpsUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ValidatedHttpsUrl")
            .field(&self.origin())
            .finish()
    }
}
impl fmt::Display for ValidatedHttpsUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.origin())
    }
}

impl ValidatedHttpsUrl {
    pub fn parse(input: &str) -> Result<Self, HttpsManifestUrlError> {
        let url = Url::parse(input).map_err(|_| HttpsManifestUrlError::InvalidUrl)?;
        if url.scheme() != "https" {
            return Err(HttpsManifestUrlError::NotHttps);
        }
        let host = url.host_str().ok_or(HttpsManifestUrlError::MissingHost)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(HttpsManifestUrlError::UserInfo);
        }
        if url.fragment().is_some() {
            return Err(HttpsManifestUrlError::Fragment);
        }
        let lower = host.to_ascii_lowercase();
        if lower == "localhost"
            || lower.ends_with(".localhost")
            || lower == "local"
            || lower.ends_with(".local")
        {
            return Err(HttpsManifestUrlError::UnsafeHost);
        }
        Ok(Self(url))
    }
    pub fn origin(&self) -> String {
        let host = match self.0.host() {
            Some(url::Host::Ipv6(_)) => format!("[{}]", self.host_identity()),
            _ => self.host_identity(),
        };
        format!("https://{}:{}", host, self.0.port().map_or(443, |p| p))
    }
    pub fn redacted(&self) -> String {
        self.origin()
    }
    pub(crate) fn request_url(&self) -> &Url {
        &self.0
    }
    pub(crate) fn request_identity_digest(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_str().as_bytes()).into()
    }
    fn host_identity(&self) -> String {
        self.0.host_str().unwrap_or_default().to_ascii_lowercase()
    }
}

pub fn validate_resolved_addresses(
    url: &ValidatedHttpsUrl,
    resolved_host: &ValidatedHttpsUrl,
    addresses: &[IpAddr],
) -> Result<(), HttpsManifestUrlError> {
    if url.host_identity() != resolved_host.host_identity()
        || addresses.is_empty()
        || addresses.iter().any(|ip| !is_safe_public_ip(*ip))
    {
        return Err(HttpsManifestUrlError::UnsafeHost);
    }
    Ok(())
}

pub fn redirect_chain_digest_input(chain: &[ValidatedHttpsUrl]) -> String {
    chain
        .iter()
        .map(ValidatedHttpsUrl::origin)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn redirect_chain_evidence_digest_input(chain: &[(ValidatedHttpsUrl, u16)]) -> String {
    chain
        .iter()
        .map(|(url, status)| {
            let identity = url.request_identity_digest();
            let identity = identity
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            format!("{}\n{}", identity, status)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_safe_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || v.is_broadcast()
                || o[0] == 0
                || o[0] == 127
                || o[0] == 100 && (64..=127).contains(&o[1])
                || o[0] == 169 && o[1] == 254
                || o[0] == 192 && o[1] == 0 && (o[2] == 0 || o[2] == 2 || o[2] == 9 || o[2] == 10)
                || o[0] == 198 && (o[1] == 18 || o[1] == 19 || o[1] == 51)
                || o[0] == 203 && o[1] == 0 && o[2] == 113
                || o[0] >= 240)
        }
        IpAddr::V6(v) => {
            if let Some(mapped) = v.to_ipv4().or_else(|| ipv4_compatible(v)) {
                return is_safe_public_ip(IpAddr::V4(mapped));
            }
            let s = v.segments();
            !(v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || s[0] == 0x2001 && s[1] == 0xdb8)
        }
    }
}

fn ipv4_compatible(ip: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let segments = ip.segments();
    (segments[..6].iter().all(|segment| *segment == 0)).then(|| {
        std::net::Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    fn v(s: &str) -> ValidatedHttpsUrl {
        ValidatedHttpsUrl::parse(s).unwrap()
    }
    #[test]
    fn p1_secrets_never_displayed() {
        let x = v("https://example.com/secret?token=x");
        let d = format!("{x:?}");
        assert!(!d.contains("secret"));
        assert!(!d.contains("token"));
        assert_eq!(x.to_string(), "https://example.com:443");
    }
    #[test]
    fn p1_origin_port_and_path_free_digest() {
        assert_eq!(
            v("https://example.com/a?x=1").origin(),
            "https://example.com:443"
        );
        assert_ne!(
            redirect_chain_digest_input(&[v("https://x")]),
            redirect_chain_digest_input(&[v("https://x:8443")])
        );
        assert_ne!(
            redirect_chain_evidence_digest_input(&[(v("https://x/a"), 200)]),
            redirect_chain_evidence_digest_input(&[(v("https://x/b"), 200)])
        );
        assert_ne!(
            redirect_chain_evidence_digest_input(&[(v("https://x/a?q=1"), 200)]),
            redirect_chain_evidence_digest_input(&[(v("https://x/a?q=2"), 200)])
        );
    }
    #[test]
    fn p1_host_binding() {
        let ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        assert!(validate_resolved_addresses(&v("https://x"), &v("https://y"), &[ip]).is_err());
    }
    #[test]
    fn p1_ranges_fail_closed() {
        for ip in [
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)),
            IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1)),
        ] {
            assert!(!is_safe_public_ip(ip));
        }
    }
    #[test]
    fn p1_ipv4_mapped_ipv6_reuses_ipv4_safety_policy() {
        for ip in [
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 1)),
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0a00, 1)),
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0101)),
        ] {
            assert!(!is_safe_public_ip(ip));
        }
        assert_eq!(
            is_safe_public_ip(IpAddr::V6(Ipv6Addr::new(
                0, 0, 0, 0, 0, 0xffff, 0x0808, 0x0808
            ))),
            is_safe_public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)))
        );
        assert!(is_safe_public_ip(IpAddr::V6(Ipv6Addr::new(
            0, 0, 0, 0, 0, 0xffff, 0x0808, 0x0808
        ))));
    }
    #[test]
    fn p1_ipv4_compatible_ipv6_reuses_ipv4_safety_policy() {
        for ip in [
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0x7f00, 1)),
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0x0a00, 1)),
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0xc0a8, 0x0101)),
        ] {
            assert!(!is_safe_public_ip(ip));
        }
        assert_eq!(
            is_safe_public_ip(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0x0808, 0x0808))),
            is_safe_public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)))
        );
        assert!(is_safe_public_ip(IpAddr::V6(Ipv6Addr::new(
            0, 0, 0, 0, 0, 0, 0x0808, 0x0808
        ))));
    }
    #[test]
    fn p1_policy_cannot_be_relaxed() {
        assert!(HttpsManifestFetchPolicy::new(
            11,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            1
        )
        .is_none());
    }
}
