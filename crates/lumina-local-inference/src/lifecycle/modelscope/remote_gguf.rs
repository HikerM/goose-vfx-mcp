use anyhow::{ensure, Context, Result};
use futures::StreamExt;
use reqwest::header::{HeaderMap, ACCEPT_ENCODING, CONTENT_RANGE, LOCATION, RANGE};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, ErrorKind};
use std::time::Duration;

use super::{
    partial_error, scoped_auth_headers, status_error, transport_error, ModelScopeProvider,
    ModelSourceError, RepositoryInventory,
};
use crate::lifecycle::{
    inspect_gguf, GgufInspection, GgufReadLimits, LifecycleErrorCode, RepositoryPath, Sha256Digest,
};

const MAX_RANGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteGgufInspection {
    pub snapshot_digest: Sha256Digest,
    pub path: RepositoryPath,
    pub source_sha256: Option<Sha256Digest>,
    pub bytes_fetched: u64,
    pub range_requests: u32,
    pub inspection: GgufInspection,
}

fn source_origin(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && matches!(
            url.host_str(),
            Some("modelscope.cn" | "cdn-lfs-cn-1.modelscope.cn")
        )
}

fn range_auth_headers(url: &reqwest::Url, token: Option<&str>) -> Result<HeaderMap> {
    ensure!(
        source_origin(url),
        ModelSourceError {
            code: LifecycleErrorCode::SourceForbidden,
            http_status: None
        }
    );
    if url.host_str() == Some("modelscope.cn") {
        scoped_auth_headers(token)
    } else {
        Ok(HeaderMap::new())
    }
}

fn parse_content_range(value: &str) -> Result<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ").context(partial_error())?;
    let (bounds, total) = value.split_once('/').context(partial_error())?;
    let (start, end) = bounds.split_once('-').context(partial_error())?;
    ensure!(
        [start, end, total]
            .iter()
            .all(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())),
        partial_error()
    );
    let start = start.parse::<u64>().map_err(|_| partial_error())?;
    let end = end.parse::<u64>().map_err(|_| partial_error())?;
    let total = total.parse::<u64>().map_err(|_| partial_error())?;
    ensure!(start <= end && end < total, partial_error());
    Ok((start, end, total))
}

fn validate_range_response(
    status: reqwest::StatusCode,
    content_range: Option<&str>,
    content_length: Option<u64>,
    start: u64,
    end: u64,
    total: u64,
) -> Result<()> {
    match status {
        reqwest::StatusCode::PARTIAL_CONTENT => {
            let observed = parse_content_range(content_range.context(partial_error())?)?;
            ensure!(
                observed.2 == total,
                ModelSourceError {
                    code: LifecycleErrorCode::RevisionChanged,
                    http_status: Some(206)
                }
            );
            ensure!(observed == (start, end, total), partial_error());
        }
        reqwest::StatusCode::OK => ensure!(start == 0 && end + 1 == total, partial_error()),
        reqwest::StatusCode::RANGE_NOT_SATISFIABLE => {
            return Err(ModelSourceError {
                code: LifecycleErrorCode::RevisionChanged,
                http_status: Some(416),
            }
            .into())
        }
        _ => return Err(status_error(status.as_u16()).into()),
    }
    ensure!(
        content_length.is_none_or(|length| length == end - start + 1),
        partial_error()
    );
    Ok(())
}

impl ModelScopeProvider {
    async fn fetch_gguf_range(
        &self,
        mut url: reqwest::Url,
        start: u64,
        end: u64,
        total: u64,
        token: Option<&str>,
    ) -> Result<(Vec<u8>, u32)> {
        ensure!(
            start <= end && end < total && end - start < MAX_RANGE_BYTES,
            partial_error()
        );
        let expected = end - start + 1;
        for redirect in 0..=MAX_REDIRECTS {
            let request = self
                .client
                .get(url.clone())
                .headers(range_auth_headers(&url, token)?)
                .header(RANGE, format!("bytes={start}-{end}"))
                .header(ACCEPT_ENCODING, "identity")
                .timeout(Duration::from_secs(30));
            let response = request.send().await.map_err(transport_error)?;
            let status = response.status();
            if status.is_redirection() {
                ensure!(
                    redirect < MAX_REDIRECTS
                        && matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308),
                    partial_error()
                );
                let location = response
                    .headers()
                    .get(LOCATION)
                    .context(partial_error())?
                    .to_str()
                    .map_err(|_| partial_error())?;
                url = url.join(location).map_err(|_| partial_error())?;
                continue;
            }
            let content_range = response
                .headers()
                .get(CONTENT_RANGE)
                .map(|value| value.to_str().map_err(|_| partial_error()))
                .transpose()?;
            validate_range_response(
                status,
                content_range,
                response.content_length(),
                start,
                end,
                total,
            )?;
            let mut bytes = Vec::with_capacity(expected as usize);
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(transport_error)?;
                ensure!(
                    chunk.len() as u64 <= expected.saturating_sub(bytes.len() as u64),
                    partial_error()
                );
                bytes.extend_from_slice(&chunk);
            }
            ensure!(bytes.len() as u64 == expected, partial_error());
            return Ok((bytes, redirect as u32 + 1));
        }
        Err(partial_error().into())
    }

    pub async fn inspect_remote_gguf(
        &self,
        inventory: &RepositoryInventory,
        path: &RepositoryPath,
        limits: GgufReadLimits,
        token: Option<&str>,
    ) -> Result<RemoteGgufInspection> {
        inventory.validate()?;
        ensure!(
            path.as_str().to_ascii_lowercase().ends_with(".gguf"),
            ModelSourceError {
                code: LifecycleErrorCode::FormatForbidden,
                http_status: None
            }
        );
        ensure!(
            limits.max_header_bytes > 0 && limits.max_header_bytes <= MAX_RANGE_BYTES,
            "Invalid remote GGUF header budget"
        );
        let source = inventory
            .files
            .get(path.as_str())
            .context(partial_error())?;
        let total = source
            .size_bytes
            .filter(|size| *size > 0)
            .context(partial_error())?;
        let url = inventory.download_url(path)?;
        let snapshot_digest = inventory.snapshot.content_digest()?;
        tokio::time::timeout(Duration::from_secs(120), async {
            let budget = limits.max_header_bytes.min(total);
            let mut target = (128 * 1024).min(budget);
            let mut bytes = Vec::new();
            let mut requests = 0;
            loop {
                let (chunk, request_count) = self
                    .fetch_gguf_range(url.clone(), bytes.len() as u64, target - 1, total, token)
                    .await?;
                bytes.extend(chunk);
                requests += request_count;
                match inspect_gguf(Cursor::new(&bytes), total, limits) {
                    Ok(inspection) => {
                        return Ok(RemoteGgufInspection {
                            snapshot_digest,
                            path: path.clone(),
                            source_sha256: source.sha256.clone(),
                            bytes_fetched: bytes.len() as u64,
                            range_requests: requests,
                            inspection,
                        })
                    }
                    Err(error) => {
                        let truncated = error.chain().any(|cause| {
                            cause
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|error| error.kind() == ErrorKind::UnexpectedEof)
                        });
                        if !truncated || target == budget {
                            return Err(error.context(ModelSourceError {
                                code: LifecycleErrorCode::GgufInvalid,
                                http_status: None,
                            }));
                        }
                    }
                }
                target = target.saturating_mul(2).min(budget);
            }
        })
        .await
        .map_err(|_| ModelSourceError {
            code: LifecycleErrorCode::SourceTimeout,
            http_status: None,
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_accept_only_observed_modelscope_https_origins() {
        for url in [
            "https://modelscope.cn/api/v1/models/a/b/repo",
            "https://cdn-lfs-cn-1.modelscope.cn/file?signature=opaque",
        ] {
            assert!(source_origin(&reqwest::Url::parse(url).unwrap()));
        }
        for url in [
            "http://modelscope.cn/file",
            "https://modelscope.cn:444/file",
            "https://modelscope.cn.evil.example/file",
            "https://huggingface.co/file",
            "https://127.0.0.1/file",
            "https://user:password@modelscope.cn/file",
            "https://cdn-lfs-cn-1.modelscope.cn/file#fragment",
            "file:///D:/models/file",
        ] {
            assert!(!source_origin(&reqwest::Url::parse(url).unwrap()));
        }
    }

    #[test]
    fn ranges_require_exact_finite_offsets_and_total_length() {
        assert_eq!(parse_content_range("bytes 0-9/100").unwrap(), (0, 9, 100));
        for value in [
            "bytes */100",
            "bytes 0-9/*",
            "bytes 9-0/100",
            "bytes 0-100/100",
            "bytes -1-9/100",
            "bytes +0-9/100",
            "bytes 0-9/100 extra",
            "items 0-9/100",
            "bytes 0-9/18446744073709551616",
        ] {
            assert!(parse_content_range(value).is_err());
        }
    }

    #[test]
    fn range_credentials_stay_on_the_modelscope_api_origin() {
        use reqwest::header::{AUTHORIZATION, COOKIE};
        let api = reqwest::Url::parse("https://modelscope.cn/api/v1/models/a/b/repo").unwrap();
        let cdn = reqwest::Url::parse("https://cdn-lfs-cn-1.modelscope.cn/opaque").unwrap();
        let headers = range_auth_headers(&api, Some("test-scoped-token")).unwrap();
        assert!(headers[AUTHORIZATION].is_sensitive() && headers[COOKIE].is_sensitive());
        assert!(!format!("{headers:?}").contains("test-scoped-token"));
        assert!(range_auth_headers(&cdn, Some("test-scoped-token"))
            .unwrap()
            .is_empty());
        let outside = reqwest::Url::parse("https://example.com/file").unwrap();
        assert!(range_auth_headers(&outside, Some("test-scoped-token")).is_err());
    }

    #[test]
    fn ignored_ranges_and_changed_file_lengths_do_not_pass_remote_inspection() {
        use reqwest::StatusCode;
        validate_range_response(
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 0-9/100"),
            Some(10),
            0,
            9,
            100,
        )
        .unwrap();
        validate_range_response(StatusCode::OK, None, Some(100), 0, 99, 100).unwrap();
        assert!(validate_range_response(StatusCode::OK, None, Some(100), 0, 9, 100).is_err());
        assert!(validate_range_response(
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 1-10/100"),
            Some(10),
            0,
            9,
            100
        )
        .is_err());
        assert!(validate_range_response(
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 0-9/100"),
            Some(9),
            0,
            9,
            100
        )
        .is_err());
        for (status, range) in [
            (StatusCode::PARTIAL_CONTENT, Some("bytes 0-9/101")),
            (StatusCode::RANGE_NOT_SATISFIABLE, None),
        ] {
            let error = validate_range_response(status, range, None, 0, 9, 100).unwrap_err();
            assert_eq!(
                error.downcast_ref::<ModelSourceError>().unwrap().code,
                LifecycleErrorCode::RevisionChanged
            );
        }
    }
}
