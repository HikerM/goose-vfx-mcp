use anyhow::{ensure, Result};
use futures::{stream, stream::LocalBoxStream, StreamExt};
use lumina_download_manager::managed::PinnedHttpRange;
use std::future::Future;

use super::ResumeValidator;

pub(super) const RANGE_BYTES: u64 = 8 * 1024 * 1024;
const RANGE_CONCURRENCY: usize = 2;

pub(super) struct RangeResponse<'a> {
    pub expected_bytes: u64,
    pub validator: ResumeValidator,
    pub chunks: LocalBoxStream<'a, Result<Vec<u8>>>,
}

pub(super) async fn open_file_ranges<'a, Request, Fut>(
    start: u64,
    total: u64,
    etag: Option<String>,
    request: Request,
) -> Result<RangeResponse<'a>>
where
    Request: Fn(PinnedHttpRange, bool) -> Fut + 'a,
    Fut: Future<Output = Result<RangeResponse<'a>>> + 'a,
{
    ensure!(start < total, "File range has no remaining bytes");
    let range = PinnedHttpRange {
        start,
        length: (total - start).min(RANGE_BYTES),
        total,
        if_range: etag.clone(),
    };
    let mut response = request(range, false).await?;
    if response.expected_bytes != total - start && response.validator == ResumeValidator::Unknown {
        drop(response);
        response = request(
            PinnedHttpRange {
                start,
                length: total - start,
                total,
                if_range: etag,
            },
            true,
        )
        .await?;
        ensure!(
            response.expected_bytes == total - start,
            "Full-file fallback does not cover the remaining bytes"
        );
    }
    if response.expected_bytes == total - start {
        return Ok(response);
    }
    let ResumeValidator::StrongEtag { value } = &response.validator else {
        anyhow::bail!("Parallel ranges require a strong ETag");
    };
    let etag = value.clone();
    let validator = response.validator.clone();
    let mut first = Some(response);
    let chunks = stream::iter((start..total).step_by(RANGE_BYTES as usize))
        .map(move |offset| {
            let first_body = first.take();
            let range = PinnedHttpRange {
                start: offset,
                length: (total - offset).min(RANGE_BYTES),
                total,
                if_range: Some(etag.clone()),
            };
            let next = match first_body {
                Some(first) => futures::future::Either::Left(std::future::ready(Ok(first))),
                None => futures::future::Either::Right(request(range.clone(), false)),
            };
            async move {
                let mut response = next.await?;
                ensure!(
                    response.expected_bytes == range.length,
                    "Range does not cover its pinned extent"
                );
                let mut bytes = Vec::with_capacity(range.length as usize);
                while let Some(chunk) = response.chunks.next().await {
                    let chunk = chunk?;
                    ensure!(
                        chunk.len() <= range.length as usize - bytes.len(),
                        "Range exceeds its pinned extent"
                    );
                    bytes.extend_from_slice(&chunk);
                }
                ensure!(
                    bytes.len() == range.length as usize,
                    "Range ended before its pinned extent"
                );
                Ok(bytes)
            }
        })
        .buffered(RANGE_CONCURRENCY)
        .boxed_local();
    Ok(RangeResponse {
        expected_bytes: total - start,
        validator,
        chunks,
    })
}

#[cfg(test)]
#[path = "../../tests/support/ordered_network_ranges.rs"]
mod tests;
