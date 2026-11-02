use reqwest::header::{
    HeaderMap, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_RANGE, ETAG, IF_RANGE, LOCATION, RANGE,
    RETRY_AFTER,
};
use reqwest::{Client, Response, StatusCode, Url};
use std::fmt::{Display, Formatter};
use std::time::Duration;

pub const MAX_MANAGED_RANGE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedTransportErrorKind {
    Forbidden,
    Authentication,
    NotFound,
    Timeout,
    RateLimited,
    Unavailable,
    Protocol,
    RevisionChanged,
    ResumeUnavailable,
    ValidatorChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedTransportError {
    pub kind: ManagedTransportErrorKind,
    pub http_status: Option<u16>,
    pub retry_after_seconds: Option<u64>,
}

impl Display for ManagedTransportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Managed transfer {:?}", self.kind)?;
        if let Some(status) = self.http_status {
            write!(formatter, " (HTTP {status})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ManagedTransportError {}

type Result<T> = anyhow::Result<T, ManagedTransportError>;

fn failure(kind: ManagedTransportErrorKind) -> ManagedTransportError {
    ManagedTransportError {
        kind,
        http_status: None,
        retry_after_seconds: None,
    }
}

fn network_error(error: reqwest::Error) -> ManagedTransportError {
    ManagedTransportError {
        kind: if error.is_timeout() {
            ManagedTransportErrorKind::Timeout
        } else {
            ManagedTransportErrorKind::Unavailable
        },
        http_status: error.status().map(|status| status.as_u16()),
        retry_after_seconds: None,
    }
}

fn valid_etag(value: &str) -> bool {
    value.len() >= 2
        && value.len() <= 8192
        && value.starts_with('"')
        && value.ends_with('"')
        && value.as_bytes()[1..value.len() - 1]
            .iter()
            .all(|byte| (0x21..=0x7e).contains(byte) && *byte != b'"')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedHttpRange {
    pub start: u64,
    pub length: u64,
    pub total: u64,
    pub if_range: Option<String>,
}

impl PinnedHttpRange {
    fn end(&self) -> Result<u64> {
        if self.total == 0 || self.length == 0 {
            return Err(failure(ManagedTransportErrorKind::Protocol));
        }
        let end = self
            .start
            .checked_add(self.length - 1)
            .filter(|end| *end < self.total)
            .ok_or_else(|| failure(ManagedTransportErrorKind::Protocol))?;
        if self
            .if_range
            .as_ref()
            .is_some_and(|value| !valid_etag(value))
        {
            return Err(failure(ManagedTransportErrorKind::Protocol));
        }
        if self.start > 0 && self.if_range.is_none() {
            return Err(failure(ManagedTransportErrorKind::ResumeUnavailable));
        }
        Ok(end)
    }
}

#[derive(Clone)]
pub struct ManagedModelScopeTransport {
    client: Client,
    credential_origin: String,
    allowed_origins: Vec<String>,
}

impl ManagedModelScopeTransport {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_gzip()
            .no_brotli()
            .no_zstd()
            .no_deflate()
            .user_agent("lumina-managed-model-transfer")
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(20))
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            credential_origin: "https://modelscope.cn".into(),
            allowed_origins: vec![
                "https://modelscope.cn".into(),
                "https://cdn-lfs-cn-1.modelscope.cn".into(),
            ],
        })
    }

    fn validate_url(&self, url: &Url) -> Result<()> {
        if !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !self
                .allowed_origins
                .contains(&url.origin().ascii_serialization())
        {
            return Err(failure(ManagedTransportErrorKind::Forbidden));
        }
        Ok(())
    }

    pub async fn open_range(
        &self,
        url: Url,
        range: &PinnedHttpRange,
        credential_headers: &HeaderMap,
    ) -> Result<ManagedRangeBody> {
        if range.length > MAX_MANAGED_RANGE_BYTES {
            return Err(failure(ManagedTransportErrorKind::Protocol));
        }
        self.open_request(url, range, credential_headers).await
    }

    pub async fn open_file(
        &self,
        url: Url,
        start: u64,
        total: u64,
        if_range: Option<String>,
        credential_headers: &HeaderMap,
    ) -> Result<ManagedRangeBody> {
        let range = PinnedHttpRange {
            start,
            length: total
                .checked_sub(start)
                .ok_or_else(|| failure(ManagedTransportErrorKind::Protocol))?,
            total,
            if_range,
        };
        self.open_request(url, &range, credential_headers).await
    }

    async fn open_request(
        &self,
        mut url: Url,
        range: &PinnedHttpRange,
        credential_headers: &HeaderMap,
    ) -> Result<ManagedRangeBody> {
        let end = range.end()?;
        self.validate_url(&url)?;
        if url.origin().ascii_serialization() != self.credential_origin {
            return Err(failure(ManagedTransportErrorKind::Forbidden));
        }
        for redirect in 0..=3 {
            self.validate_url(&url)?;
            let mut request = self
                .client
                .get(url.clone())
                .header(ACCEPT_ENCODING, "identity")
                .header(RANGE, format!("bytes={}-{end}", range.start));
            if url.origin().ascii_serialization() == self.credential_origin {
                request = request.headers(credential_headers.clone());
            }
            if range.start > 0 {
                request = request.header(
                    IF_RANGE,
                    range
                        .if_range
                        .as_deref()
                        .expect("Resume range is validated"),
                );
            }
            let response = tokio::time::timeout(Duration::from_secs(30), request.send())
                .await
                .map_err(|_| failure(ManagedTransportErrorKind::Timeout))?
                .map_err(network_error)?;
            if response.status().is_redirection() {
                if redirect == 3
                    || !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308)
                {
                    return Err(failure(ManagedTransportErrorKind::Protocol));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| failure(ManagedTransportErrorKind::Protocol))?;
                url = url
                    .join(location)
                    .map_err(|_| failure(ManagedTransportErrorKind::Protocol))?;
                continue;
            }
            return ManagedRangeBody::from_response(response, range, end);
        }
        Err(failure(ManagedTransportErrorKind::Protocol))
    }
}

pub struct ManagedRangeBody {
    response: Response,
    expected_bytes: u64,
    received_bytes: u64,
    strong_etag: Option<String>,
}

impl ManagedRangeBody {
    fn from_response(response: Response, range: &PinnedHttpRange, end: u64) -> Result<Self> {
        let status = response.status();
        let expected_bytes = match status {
            StatusCode::PARTIAL_CONTENT => {
                let value = response
                    .headers()
                    .get(CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| failure(ManagedTransportErrorKind::Protocol))?;
                let (bounds, total) = value
                    .strip_prefix("bytes ")
                    .and_then(|value| value.split_once('/'))
                    .ok_or_else(|| failure(ManagedTransportErrorKind::Protocol))?;
                let (start, observed_end) = bounds
                    .split_once('-')
                    .ok_or_else(|| failure(ManagedTransportErrorKind::Protocol))?;
                let parse = |value: &str| -> Result<u64> {
                    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(failure(ManagedTransportErrorKind::Protocol));
                    }
                    value
                        .parse()
                        .map_err(|_| failure(ManagedTransportErrorKind::Protocol))
                };
                if parse(total)? != range.total {
                    return Err(failure(ManagedTransportErrorKind::RevisionChanged));
                }
                if parse(start)? != range.start || parse(observed_end)? != end {
                    return Err(failure(ManagedTransportErrorKind::Protocol));
                }
                range.length
            }
            StatusCode::OK if range.start == 0 => range.total,
            StatusCode::OK => return Err(failure(ManagedTransportErrorKind::ResumeUnavailable)),
            _ => {
                let kind = match status.as_u16() {
                    401 => ManagedTransportErrorKind::Authentication,
                    403 => ManagedTransportErrorKind::Forbidden,
                    404 => ManagedTransportErrorKind::NotFound,
                    408 | 504 => ManagedTransportErrorKind::Timeout,
                    416 => ManagedTransportErrorKind::RevisionChanged,
                    429 => ManagedTransportErrorKind::RateLimited,
                    500..=599 => ManagedTransportErrorKind::Unavailable,
                    _ => ManagedTransportErrorKind::Protocol,
                };
                let retry_after_seconds = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|value| value.min(60));
                return Err(ManagedTransportError {
                    kind,
                    http_status: Some(status.as_u16()),
                    retry_after_seconds,
                });
            }
        };
        if response
            .content_length()
            .is_some_and(|length| length != expected_bytes)
        {
            return Err(failure(ManagedTransportErrorKind::RevisionChanged));
        }
        if response
            .headers()
            .get(CONTENT_ENCODING)
            .is_some_and(|value| value.as_bytes() != b"identity")
        {
            return Err(failure(ManagedTransportErrorKind::Protocol));
        }
        let strong_etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .filter(|value| valid_etag(value))
            .map(str::to_string);
        if range.start > 0 && strong_etag.as_ref() != range.if_range.as_ref() {
            return Err(failure(ManagedTransportErrorKind::ValidatorChanged));
        }
        Ok(Self {
            response,
            expected_bytes,
            received_bytes: 0,
            strong_etag,
        })
    }

    pub fn expected_bytes(&self) -> u64 {
        self.expected_bytes
    }

    pub fn strong_etag(&self) -> Option<&str> {
        self.strong_etag.as_deref()
    }

    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        match self.response.chunk().await.map_err(network_error)? {
            Some(chunk) => {
                if chunk.len() > 8 * 1024 * 1024
                    || chunk.len() as u64 > self.expected_bytes.saturating_sub(self.received_bytes)
                {
                    return Err(failure(ManagedTransportErrorKind::Protocol));
                }
                self.received_bytes += chunk.len() as u64;
                Ok(Some(chunk.to_vec()))
            }
            None if self.received_bytes == self.expected_bytes => Ok(None),
            None => Err(failure(ManagedTransportErrorKind::Protocol)),
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/managed_transport.rs"]
mod tests;
