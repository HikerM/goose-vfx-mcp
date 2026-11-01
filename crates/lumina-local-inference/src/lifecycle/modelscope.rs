use anyhow::{ensure, Context, Result};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, COOKIE};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::{Display, Formatter};
use std::time::Duration;

use super::{
    classify_gguf, Artifact, FilePurpose, GgufClassification, LifecycleErrorCode,
    ModelScopeRepository, RepositoryPath, RepositorySnapshot, Sha256Digest,
};

const API_BASE: &str = "https://modelscope.cn/api/v1/models";
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const REPO_FILES_CAP: usize = 3000;
const MAX_LISTING_REQUESTS: usize = 1000;

mod revisions;
pub use revisions::*;
mod paths;
pub use paths::*;
mod search;
pub use search::*;
mod discovery;
pub use discovery::*;
mod remote_gguf;
pub use remote_gguf::*;
mod variants;
pub use variants::*;
mod components;
pub use components::*;

fn source_artifact_id(
    snapshot_id: &str,
    path: &RepositoryPath,
    purpose: FilePurpose,
) -> Result<String> {
    Ok(
        Sha256Digest::of_serializable(&(snapshot_id, path, purpose))?
            .as_str()
            .to_string(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryFile {
    pub path: ObservedRepositoryPath,
    pub size_bytes: Option<u64>,
    pub sha256: Option<Sha256Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryInventory {
    pub snapshot: RepositorySnapshot,
    pub files: BTreeMap<String, RepositoryFile>,
}

impl RepositoryInventory {
    pub fn artifact(&self, path: &RepositoryPath, purpose: FilePurpose) -> Result<Artifact> {
        self.validate()?;
        let file = self.files.get(path.as_str()).context(ModelSourceError {
            code: LifecycleErrorCode::SourceNotFound,
            http_status: None,
        })?;
        let snapshot_id = self.snapshot.content_digest()?.as_str().to_string();
        let artifact = Artifact {
            id: source_artifact_id(&snapshot_id, path, purpose)?,
            snapshot_id,
            path: path.clone(),
            purpose,
            size_bytes: file.size_bytes,
            source_sha256: file.sha256.clone(),
        };
        artifact.validate()?;
        Ok(artifact)
    }

    pub fn classify_observation(
        &self,
        observation: &RemoteGgufInspection,
    ) -> Result<GgufClassification> {
        self.validate()?;
        ensure!(
            observation.snapshot_digest == self.snapshot.content_digest()?,
            "GGUF observation belongs to a different source snapshot"
        );
        let artifact = self.artifact(&observation.path, FilePurpose::GgufWeight)?;
        ensure!(
            artifact.source_sha256 == observation.source_sha256,
            "GGUF observation source file hash changed"
        );
        classify_gguf(
            &artifact,
            &artifact.snapshot_id,
            &self.snapshot,
            &observation.inspection,
            &[],
        )
    }

    pub fn path_diagnostics(&self) -> Vec<RepositoryPathDiagnostic> {
        self.files
            .values()
            .filter(|file| file.path.materializable_path().is_err())
            .map(|file| RepositoryPathDiagnostic {
                path: file.path.clone(),
                issue: RepositoryPathIssue::NonPortable,
            })
            .collect()
    }

    pub fn validate(&self) -> Result<()> {
        self.snapshot.validate()?;
        ensure!(
            self.files.len() <= MAX_FILES,
            "ModelScope inventory exceeds file limit"
        );
        for (key, file) in &self.files {
            ensure!(
                key == file.path.as_str(),
                "ModelScope inventory path mismatch"
            );
        }
        ensure!(
            self.snapshot.files_digest == Sha256Digest::of_serializable(&self.files)?,
            "ModelScope inventory digest mismatch"
        );
        Ok(())
    }

    pub fn download_url(&self, path: &RepositoryPath) -> Result<reqwest::Url> {
        self.validate()?;
        ensure!(
            self.files.contains_key(path.as_str()),
            "File is absent from pinned ModelScope inventory"
        );
        let mut url = repository_url(&self.snapshot.repository, "repo")?;
        url.query_pairs_mut()
            .append_pair("Revision", &self.snapshot.resolved_revision)
            .append_pair("FilePath", path.as_str());
        Ok(url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSourceError {
    pub code: LifecycleErrorCode,
    pub http_status: Option<u16>,
}

impl Display for ModelSourceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ModelScope request failed: {:?}", self.code)
    }
}

impl std::error::Error for ModelSourceError {}

#[derive(Clone)]
pub struct ModelScopeProvider {
    client: reqwest::Client,
}

impl ModelScopeProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("lumina-model-lifecycle")
                .build()?,
        })
    }

    pub async fn inspect_at(
        &self,
        repository: ModelScopeRepository,
        requested_revision: String,
        resolved_revision: String,
        fetched_at: i64,
        token: Option<&str>,
    ) -> Result<RepositoryInventory> {
        let snapshot = RepositorySnapshot {
            repository,
            requested_revision,
            resolved_revision: resolved_revision.to_ascii_lowercase(),
            files_digest: Sha256Digest::of_bytes(b""),
            fetched_at,
        };
        snapshot.validate()?;
        tokio::time::timeout(Duration::from_secs(300), async {
            let mut walk = FileTreeWalk::new(REPO_FILES_CAP);
            let mut received = 0usize;
            while let Some(scope) = walk.next_request()? {
                let body = self.fetch_files_page(&snapshot, &scope, token).await?;
                received = received
                    .checked_add(body.len())
                    .context("ModelScope listing byte count overflow")?;
                ensure!(received <= 128 * 1024 * 1024, partial_error());
                walk.accept(scope, parse_files_page(&body)?)?;
            }
            inventory_from_entries(walk.finish()?, snapshot)
        })
        .await
        .map_err(|_| ModelSourceError {
            code: LifecycleErrorCode::SourceTimeout,
            http_status: None,
        })?
    }

    async fn fetch_files_page(
        &self,
        snapshot: &RepositorySnapshot,
        scope: &TreeRequest,
        token: Option<&str>,
    ) -> Result<Vec<u8>> {
        let mut url = repository_url(&snapshot.repository, "repo/files")?;
        url.query_pairs_mut()
            .append_pair("Revision", &snapshot.resolved_revision)
            .append_pair("Recursive", if scope.recursive { "true" } else { "false" });
        if let Some(root) = &scope.root {
            url.query_pairs_mut().append_pair("Root", root.as_str());
        }
        let response = self
            .client
            .get(url)
            .headers(scoped_auth_headers(token)?)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(status_error(status).into());
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
        {
            return Err(partial_error().into());
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport_error)?;
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
                return Err(partial_error().into());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    pub(crate) async fn fetch_source_document(
        &self,
        inventory: &RepositoryInventory,
        path: &RepositoryPath,
        expected: u64,
        token: Option<&str>,
    ) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(inventory.download_url(path)?)
            .headers(scoped_auth_headers(token)?)
            .send()
            .await
            .map_err(transport_error)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(status_error(response.status().as_u16()).into());
        }
        ensure!(
            response
                .content_length()
                .is_none_or(|size| size == expected),
            partial_error()
        );
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport_error)?;
            ensure!(
                chunk.len() as u64 <= expected.saturating_sub(body.len() as u64),
                partial_error()
            );
            body.extend_from_slice(&chunk);
        }
        ensure!(body.len() as u64 == expected, partial_error());
        Ok(body)
    }
}

fn scoped_auth_headers(token: Option<&str>) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    if let Some(token) = token.map(str::trim).filter(|token| !token.is_empty()) {
        let invalid = || ModelSourceError {
            code: LifecycleErrorCode::SourceAuthRequired,
            http_status: None,
        };
        ensure!(
            token.bytes().all(
                |byte| matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e)
            ),
            invalid()
        );
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| invalid())?;
        authorization.set_sensitive(true);
        let mut cookie =
            HeaderValue::from_str(&format!("m_session_id={token}")).map_err(|_| invalid())?;
        cookie.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(COOKIE, cookie);
    }
    Ok(headers)
}

fn repository_url(repository: &ModelScopeRepository, suffix: &str) -> Result<reqwest::Url> {
    Ok(reqwest::Url::parse(&format!(
        "{API_BASE}/{}/{suffix}",
        repository.as_str()
    ))?)
}

fn status_error(status: u16) -> ModelSourceError {
    let code = match status {
        401 => LifecycleErrorCode::SourceAuthRequired,
        403 => LifecycleErrorCode::SourceForbidden,
        404 => LifecycleErrorCode::SourceNotFound,
        408 | 504 => LifecycleErrorCode::SourceTimeout,
        429 => LifecycleErrorCode::SourceRateLimited,
        500..=599 => LifecycleErrorCode::SourceUnavailable,
        _ => LifecycleErrorCode::SourcePartialFailure,
    };
    ModelSourceError {
        code,
        http_status: Some(status),
    }
}

fn partial_error() -> ModelSourceError {
    ModelSourceError {
        code: LifecycleErrorCode::SourcePartialFailure,
        http_status: None,
    }
}

fn transport_error(error: reqwest::Error) -> ModelSourceError {
    ModelSourceError {
        code: if error.is_timeout() {
            LifecycleErrorCode::SourceTimeout
        } else {
            LifecycleErrorCode::SourceUnavailable
        },
        http_status: error.status().map(|status| status.as_u16()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct FilesResponse {
    code: u16,
    data: Option<FilesData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct FilesData {
    files: Vec<WireFile>,
    total_count: Option<usize>,
    #[serde(default)]
    has_more: bool,
    next_cursor: Option<String>,
    next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WireFile {
    path: String,
    #[serde(rename = "Type")]
    entry_type: String,
    size: Option<u64>,
    sha256: Option<String>,
    mode: Option<String>,
}

pub fn parse_modelscope_inventory(
    body: &[u8],
    snapshot: RepositorySnapshot,
) -> Result<RepositoryInventory> {
    snapshot.validate()?;
    let data = parse_files_page(body)?;
    ensure!(
        data.files.len() < REPO_FILES_CAP
            && data
                .total_count
                .is_none_or(|total| total == data.files.len()),
        partial_error()
    );
    inventory_from_entries(data.files, snapshot)
}

fn parse_files_page(body: &[u8]) -> Result<FilesData> {
    ensure!(
        body.len() <= MAX_RESPONSE_BYTES,
        "ModelScope response exceeds byte limit"
    );
    let payload: FilesResponse = serde_json::from_slice(body).map_err(|_| partial_error())?;
    if payload.code != 200 {
        return Err(status_error(payload.code).into());
    }
    let data = payload.data.context(partial_error())?;
    ensure!(
        !data.has_more
            && data.next_cursor.as_ref().is_none_or(String::is_empty)
            && data.next_page_token.as_ref().is_none_or(String::is_empty),
        partial_error()
    );
    ensure!(data.files.len() <= MAX_FILES, partial_error());
    Ok(data)
}

fn inventory_from_entries(
    entries: Vec<WireFile>,
    mut snapshot: RepositorySnapshot,
) -> Result<RepositoryInventory> {
    let mut files = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for file in entries {
        let path = ObservedRepositoryPath::parse(file.path)?;
        ensure!(
            seen.insert(path.as_str().to_string()),
            "Duplicate ModelScope file path"
        );
        if file.entry_type == "tree" {
            continue;
        }
        ensure!(
            file.entry_type == "blob",
            "Unsupported ModelScope file entry type"
        );
        ensure!(
            file.mode
                .as_deref()
                .is_none_or(|mode| matches!(mode, "33188" | "33261" | "100644" | "100755")),
            "Unsupported ModelScope file mode"
        );
        let sha256 = file
            .sha256
            .filter(|value| !value.is_empty())
            .map(Sha256Digest::parse)
            .transpose()?;
        files.insert(
            path.as_str().to_string(),
            RepositoryFile {
                path,
                size_bytes: file.size,
                sha256,
            },
        );
    }
    snapshot.files_digest = Sha256Digest::of_serializable(&files)?;
    let inventory = RepositoryInventory { snapshot, files };
    inventory.validate()?;
    Ok(inventory)
}

#[derive(Debug, Clone)]
struct TreeRequest {
    root: Option<ObservedRepositoryPath>,
    recursive: bool,
}

struct FileTreeWalk {
    queue: VecDeque<TreeRequest>,
    requested: BTreeSet<(String, bool)>,
    entries: BTreeMap<String, WireFile>,
    cap: usize,
    expected_total: Option<usize>,
}

impl FileTreeWalk {
    fn new(cap: usize) -> Self {
        Self {
            queue: VecDeque::from([TreeRequest {
                root: None,
                recursive: true,
            }]),
            requested: BTreeSet::new(),
            entries: BTreeMap::new(),
            cap,
            expected_total: None,
        }
    }

    fn next_request(&mut self) -> Result<Option<TreeRequest>> {
        let Some(scope) = self.queue.pop_front() else {
            return Ok(None);
        };
        ensure!(self.requested.len() < MAX_LISTING_REQUESTS, partial_error());
        let root = scope
            .root
            .as_ref()
            .map_or("", ObservedRepositoryPath::as_str)
            .to_string();
        ensure!(
            self.requested.insert((root, scope.recursive)),
            partial_error()
        );
        Ok(Some(scope))
    }

    fn accept(&mut self, scope: TreeRequest, page: FilesData) -> Result<()> {
        if scope.root.is_none() && scope.recursive {
            self.expected_total = page.total_count;
        }
        if page.files.len() >= self.cap {
            ensure!(
                scope.recursive,
                anyhow::Error::new(partial_error()).context(format!(
                    "ModelScope directory '{}' reached its unpageable listing cap",
                    scope
                        .root
                        .as_ref()
                        .map_or("/", ObservedRepositoryPath::as_str)
                ))
            );
            self.queue.push_front(TreeRequest {
                root: scope.root,
                recursive: false,
            });
            return Ok(());
        }
        ensure!(
            page.total_count
                .is_none_or(|total| total == page.files.len()),
            partial_error()
        );
        let prefix = scope
            .root
            .as_ref()
            .map(|root| format!("{}/", root.as_str()));
        let mut seen = BTreeSet::new();
        for file in page.files {
            let path = ObservedRepositoryPath::parse(file.path.clone())?;
            let local = match &prefix {
                Some(prefix) => path
                    .as_str()
                    .strip_prefix(prefix)
                    .context(partial_error())?,
                None => path.as_str(),
            };
            ensure!(
                !local.is_empty() && (scope.recursive || !local.contains('/')),
                partial_error()
            );
            let key = path.as_str().to_string();
            ensure!(seen.insert(key.clone()), "Duplicate ModelScope file path");
            if let Some(previous) = self.entries.get(&key) {
                ensure!(
                    previous == &file,
                    "Conflicting ModelScope file facts across directory requests"
                );
                continue;
            }
            ensure!(self.entries.len() < MAX_FILES, partial_error());
            if !scope.recursive && file.entry_type == "tree" {
                self.queue.push_back(TreeRequest {
                    root: Some(path),
                    recursive: true,
                });
            }
            self.entries.insert(key, file);
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<WireFile>> {
        ensure!(
            self.queue.is_empty()
                && self
                    .expected_total
                    .is_none_or(|total| total == self.entries.len()),
            partial_error()
        );
        Ok(self.entries.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(entries: &[(&str, &str)]) -> FilesData {
        parse_files_page(&serde_json::to_vec(&json!({"Code":200,"Data":{"Files":entries.iter().map(|(path,kind)|json!({"Path":path,"Type":kind})).collect::<Vec<_>>()}})).unwrap()).unwrap()
    }

    #[test]
    fn saturated_recursive_listing_is_replaced_by_complete_directory_walk() {
        let mut walk = FileTreeWalk::new(3);
        let root = walk.next_request().unwrap().unwrap();
        walk.accept(
            root,
            page(&[
                ("a", "tree"),
                ("a/first.gguf", "blob"),
                ("discarded-partial.gguf", "blob"),
            ]),
        )
        .unwrap();
        let shallow = walk.next_request().unwrap().unwrap();
        assert!(!shallow.recursive);
        walk.accept(shallow, page(&[("a", "tree"), ("config.json", "blob")]))
            .unwrap();
        let child = walk.next_request().unwrap().unwrap();
        assert_eq!(child.root.as_ref().unwrap().as_str(), "a");
        walk.accept(
            child,
            page(&[("a/first.gguf", "blob"), ("a/second.gguf", "blob")]),
        )
        .unwrap();
        assert!(walk.next_request().unwrap().is_none());
        let entries = walk.finish().unwrap();
        assert_eq!(entries.len(), 4);
        assert!(!entries
            .iter()
            .any(|file| file.path == "discarded-partial.gguf"));
    }

    #[test]
    fn unpageable_directories_and_out_of_scope_paths_are_not_complete() {
        let mut walk = FileTreeWalk::new(2);
        let root = walk.next_request().unwrap().unwrap();
        walk.accept(root, page(&[("a.gguf", "blob"), ("b.gguf", "blob")]))
            .unwrap();
        let shallow = walk.next_request().unwrap().unwrap();
        let error = walk
            .accept(shallow, page(&[("a.gguf", "blob"), ("b.gguf", "blob")]))
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ModelSourceError>().unwrap().code,
            LifecycleErrorCode::SourcePartialFailure
        );
        let scope = TreeRequest {
            root: Some(ObservedRepositoryPath::parse("a").unwrap()),
            recursive: true,
        };
        assert!(FileTreeWalk::new(3)
            .accept(scope, page(&[("outside/model.gguf", "blob")]))
            .is_err());
    }

    #[test]
    fn legacy_auth_is_request_scoped_sensitive_and_never_accepts_cookie_injection() {
        let headers = scoped_auth_headers(Some("test-token")).unwrap();
        assert_eq!(headers[AUTHORIZATION], "Bearer test-token");
        assert_eq!(headers[COOKIE], "m_session_id=test-token");
        assert!(headers[AUTHORIZATION].is_sensitive() && headers[COOKIE].is_sensitive());
        assert!(!format!("{headers:?}").contains("test-token"));
        assert!(scoped_auth_headers(None).unwrap().is_empty());
        for token in ["bad\r\nvalue", "bad; cookie=1", "bad value"] {
            assert!(scoped_auth_headers(Some(token)).is_err());
        }
    }

    #[test]
    fn complete_directory_walk_preserves_unmaterializable_git_directory_names() {
        let mut walk = FileTreeWalk::new(3);
        let root = walk.next_request().unwrap().unwrap();
        walk.accept(root, page(&[("a", "tree"), ("b", "tree"), ("c", "tree")]))
            .unwrap();
        let shallow = walk.next_request().unwrap().unwrap();
        walk.accept(
            shallow,
            page(&[("raw\\directory", "tree"), ("main.gguf", "blob")]),
        )
        .unwrap();
        let child = walk.next_request().unwrap().unwrap();
        assert_eq!(child.root.as_ref().unwrap().as_str(), "raw\\directory");
        walk.accept(child, page(&[("raw\\directory/preview.png", "blob")]))
            .unwrap();
        assert!(walk.next_request().unwrap().is_none());
        let snapshot = RepositorySnapshot {
            repository: ModelScopeRepository::parse("owner/model").unwrap(),
            requested_revision: "master".into(),
            resolved_revision: "a".repeat(40),
            files_digest: Sha256Digest::of_bytes(b"placeholder"),
            fetched_at: 1,
        };
        let inventory = inventory_from_entries(walk.finish().unwrap(), snapshot).unwrap();
        assert_eq!(inventory.files.len(), 2);
        assert_eq!(inventory.path_diagnostics().len(), 1);
        assert!(inventory.files.contains_key("raw\\directory/preview.png"));
    }
}
