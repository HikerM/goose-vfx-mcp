use anyhow::{ensure, Result};
use lumina_local_inference::download_manager::managed::{
    ManagedModelScopeTransport, PinnedHttpRange,
};
use lumina_local_inference::lifecycle::{ModelScopeRepository, RepositoryPath, Sha256Digest};
use reqwest::{header::HeaderMap, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Source {
    repository: String,
    revision: String,
    file: String,
    size_bytes: u64,
    sha256: Sha256Digest,
    local_path: PathBuf,
}

fn finish_digest(hasher: Sha256) -> Result<Sha256Digest> {
    Sha256Digest::parse(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
}

fn local_digest(source: &Source) -> Result<Sha256Digest> {
    let mut file = std::fs::File::open(&source.local_path)?;
    ensure!(
        file.metadata()?.len() == source.size_bytes,
        "Local source size changed"
    );
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    finish_digest(hash)
}

async fn probe(source: Source) -> Result<serde_json::Value> {
    let repository = ModelScopeRepository::parse(&source.repository)?;
    let path = RepositoryPath::parse(&source.file)?;
    ensure!(
        source.revision.len() == 40 && source.revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "Source needs a fixed full revision"
    );
    ensure!(
        source.local_path.is_absolute() && source.size_bytes >= 128 * 1024,
        "Source fixture is not usable"
    );
    ensure!(
        local_digest(&source)? == source.sha256,
        "Local source checksum changed"
    );
    let mut url = Url::parse(&format!(
        "https://modelscope.cn/api/v1/models/{}/repo",
        repository.as_str()
    ))?;
    url.query_pairs_mut()
        .append_pair("Revision", &source.revision)
        .append_pair("FilePath", path.as_str());
    let client = ManagedModelScopeTransport::new()?;
    let headers = HeaderMap::new();
    let range = PinnedHttpRange {
        start: 0,
        length: 64 * 1024,
        total: source.size_bytes,
        if_range: None,
    };
    let mut first = client.open_range(url.clone(), &range, &headers).await?;
    ensure!(
        first.expected_bytes() == range.length,
        "Header probe requires bounded ranges"
    );
    let etag = first.strong_etag().map(str::to_string);
    let mut observed = Vec::new();
    while let Some(chunk) = first.next_chunk().await? {
        observed.extend(chunk);
    }
    let mut file = std::fs::File::open(&source.local_path)?;
    let mut expected = vec![0; range.length as usize];
    file.read_exact(&mut expected)?;
    ensure!(
        expected == observed,
        "Actual first range differs from the known local source"
    );
    let first_digest = Sha256Digest::of_bytes(&observed);
    let resume = if let Some(etag) = &etag {
        let resumed = PinnedHttpRange {
            start: range.length,
            if_range: Some(etag.clone()),
            ..range.clone()
        };
        let mut body = client.open_range(url.clone(), &resumed, &headers).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next_chunk().await? {
            bytes.extend(chunk);
        }
        file.seek(SeekFrom::Start(resumed.start))?;
        file.read_exact(&mut expected)?;
        ensure!(
            expected == bytes,
            "Actual resumed range differs from the known local source"
        );
        serde_json::json!({"status":"PASS", "start":resumed.start, "bytes":bytes.len(), "sha256":Sha256Digest::of_bytes(&bytes)})
    } else {
        serde_json::json!({"status":"UNAVAILABLE_NO_STRONG_ETAG", "scope":"Resume was not attempted; whole-file streaming is checked separately"})
    };
    let mut body = client
        .open_file(url, 0, source.size_bytes, None, &headers)
        .await?;
    let mut received = 0u64;
    let mut hasher = Sha256::new();
    while let Some(chunk) = body.next_chunk().await? {
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("Transfer byte count overflow"))?;
        hasher.update(&chunk);
    }
    let actual = finish_digest(hasher)?;
    ensure!(
        received == source.size_bytes && actual == source.sha256,
        "Complete network stream differs from the pinned source checksum"
    );
    ensure!(
        local_digest(&source)? == source.sha256,
        "Original fixture changed during the probe"
    );
    Ok(serde_json::json!({
        "status":"PASS_TRANSFER_PROBE_NOT_INSTALL",
        "repository":source.repository, "revision":source.revision, "file":source.file,
        "first_range":{"bytes":range.length,"sha256":first_digest,"strong_etag":etag},
        "resume":resume,
        "complete_stream":{"bytes":received,"sha256":actual},
        "original_source_unchanged":true,
        "scope":"Actual managed transport range and complete stream only; no disk download, job, installation or Runtime READY capability was created"
    }))
}

fn main() -> Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(
        arguments.len() == 1,
        "Usage: probe_managed_transfer <existing-source-verification.json>"
    );
    let source: Source = serde_json::from_reader(std::fs::File::open(&arguments[0])?)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    println!(
        "{}",
        serde_json::to_string(&runtime.block_on(probe(source))?)?
    );
    Ok(())
}
