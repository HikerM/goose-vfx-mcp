use anyhow::{ensure, Context, Result};
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{
    partial_error, scoped_auth_headers, status_error, transport_error, ModelScopeProvider,
    ModelSourceError, RepositoryInventory, MAX_RESPONSE_BYTES,
};
use crate::lifecycle::{LifecycleErrorCode, ModelScopeRepository, Sha256Digest};
use futures::StreamExt;

const MAX_REFS: usize = 100_000;
const MAX_PACKET_BYTES: usize = 65_520;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RevisionResolutionProof {
    ExplicitCommit,
    AdvertisedRef {
        reference: String,
        advertisement_sha256: Sha256Digest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryRevision {
    pub repository: ModelScopeRepository,
    pub requested_revision: String,
    pub resolved_revision: String,
    pub proof: RevisionResolutionProof,
    pub observed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryInspection {
    pub revision: RepositoryRevision,
    pub inventory: RepositoryInventory,
}

impl RepositoryInspection {
    pub fn validate(&self) -> Result<()> {
        self.inventory.validate()?;
        let snapshot = &self.inventory.snapshot;
        ensure!(
            snapshot.repository == self.revision.repository
                && snapshot.requested_revision == self.revision.requested_revision
                && snapshot.resolved_revision == self.revision.resolved_revision,
            "Repository revision proof does not match its file snapshot"
        );
        ensure!(is_object_id(&self.revision.resolved_revision), unresolved());
        match &self.revision.proof {
            RevisionResolutionProof::ExplicitCommit => ensure!(
                self.revision
                    .requested_revision
                    .eq_ignore_ascii_case(&self.revision.resolved_revision),
                unresolved()
            ),
            RevisionResolutionProof::AdvertisedRef { reference, .. } => {
                ensure!(valid_reference(reference, false), unresolved());
                let requested = &self.revision.requested_revision;
                ensure!(
                    reference == requested
                        || reference == &format!("refs/heads/{requested}")
                        || reference == &format!("refs/tags/{requested}"),
                    unresolved()
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitAdvertisement {
    references: BTreeMap<String, String>,
    digest: Sha256Digest,
}

impl GitAdvertisement {
    pub fn resolve(&self, requested: &str) -> Result<(String, String)> {
        ensure!(valid_reference(requested, true), unresolved());
        let reference = if requested == "HEAD" || requested.starts_with("refs/") {
            ensure!(self.references.contains_key(requested), unresolved());
            requested.to_string()
        } else {
            let candidates = [
                format!("refs/heads/{requested}"),
                format!("refs/tags/{requested}"),
            ];
            let matched: Vec<_> = candidates
                .into_iter()
                .filter(|name| self.references.contains_key(name))
                .collect();
            ensure!(matched.len() == 1, unresolved());
            matched.into_iter().next().expect("exactly one reference")
        };
        let peeled = format!("{reference}^{{}}");
        let commit = if reference.starts_with("refs/tags/") {
            self.references
                .get(&peeled)
                .unwrap_or(&self.references[&reference])
        } else {
            &self.references[&reference]
        };
        Ok((reference, commit.clone()))
    }

    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

fn unresolved() -> ModelSourceError {
    ModelSourceError {
        code: LifecycleErrorCode::RevisionUnresolved,
        http_status: None,
    }
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && value.bytes().any(|byte| byte != b'0')
}

fn valid_reference(value: &str, allow_short: bool) -> bool {
    if value == "HEAD" {
        return true;
    }
    if value.is_empty()
        || value == "@"
        || value.starts_with('/')
        || value.ends_with('/')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains("@{")
        || value.contains("//")
        || value
            .bytes()
            .any(|byte| byte <= 0x20 || byte == 0x7f || b"~^:?*[\\".contains(&byte))
    {
        return false;
    }
    if !allow_short && !value.starts_with("refs/") {
        return false;
    }
    value
        .split('/')
        .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

fn packet<'a>(bytes: &'a [u8], position: &mut usize) -> Result<Option<&'a [u8]>> {
    let header_end = position.checked_add(4).context(unresolved())?;
    let header = bytes.get(*position..header_end).context(unresolved())?;
    let text = std::str::from_utf8(header).map_err(|_| unresolved())?;
    ensure!(
        text.bytes().all(|byte| byte.is_ascii_hexdigit()),
        unresolved()
    );
    let len = usize::from_str_radix(text, 16).map_err(|_| unresolved())?;
    *position = header_end;
    if len == 0 {
        return Ok(None);
    }
    ensure!((4..=MAX_PACKET_BYTES).contains(&len), unresolved());
    let end = position.checked_add(len - 4).context(unresolved())?;
    let payload = bytes.get(*position..end).context(unresolved())?;
    *position = end;
    Ok(Some(payload))
}

pub fn parse_git_advertisement(bytes: &[u8]) -> Result<GitAdvertisement> {
    ensure!(bytes.len() <= MAX_RESPONSE_BYTES, partial_error());
    let mut position = 0;
    let service = packet(bytes, &mut position)?.context(unresolved())?;
    ensure!(
        service.strip_suffix(b"\n").unwrap_or(service) == b"# service=git-upload-pack",
        unresolved()
    );
    ensure!(packet(bytes, &mut position)?.is_none(), unresolved());
    let mut references = BTreeMap::new();
    let mut object_id_width = None;
    let mut first = true;
    loop {
        let Some(line) = packet(bytes, &mut position)? else {
            break;
        };
        ensure!(references.len() < MAX_REFS, partial_error());
        let line = std::str::from_utf8(line).map_err(|_| unresolved())?;
        let line = line.strip_suffix('\n').unwrap_or(line);
        let reference_line = if let Some((reference, capabilities)) = line.split_once('\0') {
            ensure!(
                first
                    && capabilities
                        .bytes()
                        .all(|byte| byte >= 0x20 && byte != 0x7f),
                unresolved()
            );
            reference
        } else {
            line
        };
        first = false;
        let (object_id, reference) = reference_line.split_once(' ').context(unresolved())?;
        if reference == "capabilities^{}"
            && object_id.bytes().all(|byte| byte == b'0')
            && matches!(object_id.len(), 40 | 64)
        {
            continue;
        }
        ensure!(is_object_id(object_id), unresolved());
        ensure!(
            object_id_width.is_none_or(|width| width == object_id.len()),
            unresolved()
        );
        object_id_width = Some(object_id.len());
        let plain_ref = reference.strip_suffix("^{}").unwrap_or(reference);
        ensure!(
            valid_reference(plain_ref, false)
                && (!reference.ends_with("^{}") || plain_ref.starts_with("refs/tags/")),
            unresolved()
        );
        ensure!(
            references
                .insert(reference.to_string(), object_id.to_ascii_lowercase())
                .is_none(),
            unresolved()
        );
    }
    ensure!(position == bytes.len(), unresolved());
    for reference in references.keys().filter(|name| name.ends_with("^{}")) {
        ensure!(
            references.contains_key(reference.strip_suffix("^{}").expect("peeled suffix")),
            unresolved()
        );
    }
    Ok(GitAdvertisement {
        references,
        digest: Sha256Digest::of_bytes(bytes),
    })
}

impl ModelScopeProvider {
    pub async fn resolve_revision(
        &self,
        repository: &ModelScopeRepository,
        requested: &str,
        token: Option<&str>,
    ) -> Result<RepositoryRevision> {
        if is_object_id(requested) {
            return Ok(RepositoryRevision {
                repository: repository.clone(),
                requested_revision: requested.to_string(),
                resolved_revision: requested.to_ascii_lowercase(),
                proof: RevisionResolutionProof::ExplicitCommit,
                observed_at: chrono::Utc::now().timestamp(),
            });
        }
        ensure!(valid_reference(requested, true), unresolved());
        let mut url = reqwest::Url::parse(&format!(
            "https://modelscope.cn/{}.git/info/refs",
            repository.as_str()
        ))?;
        url.query_pairs_mut()
            .append_pair("service", "git-upload-pack");
        let response = self
            .client
            .get(url)
            .headers(scoped_auth_headers(token)?)
            .header(USER_AGENT, "git/lumina-model-lifecycle")
            .header(ACCEPT, "application/x-git-upload-pack-advertisement")
            .send()
            .await
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(status_error(response.status().as_u16()).into());
        }
        ensure!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.split(';').next().is_some_and(|kind| kind
                    .trim()
                    .eq_ignore_ascii_case("application/x-git-upload-pack-advertisement"))),
            unresolved()
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|len| len <= MAX_RESPONSE_BYTES as u64),
            partial_error()
        );
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport_error)?;
            ensure!(
                chunk.len() <= MAX_RESPONSE_BYTES.saturating_sub(bytes.len()),
                partial_error()
            );
            bytes.extend_from_slice(&chunk);
        }
        let advertisement = parse_git_advertisement(&bytes)?;
        let (reference, resolved_revision) = advertisement.resolve(requested)?;
        Ok(RepositoryRevision {
            repository: repository.clone(),
            requested_revision: requested.to_string(),
            resolved_revision,
            proof: RevisionResolutionProof::AdvertisedRef {
                reference,
                advertisement_sha256: advertisement.digest,
            },
            observed_at: chrono::Utc::now().timestamp(),
        })
    }

    pub async fn inspect_repository(
        &self,
        repository: ModelScopeRepository,
        requested: &str,
        token: Option<&str>,
    ) -> Result<RepositoryInspection> {
        let revision = self.resolve_revision(&repository, requested, token).await?;
        let inventory = self
            .inspect_at(
                repository,
                requested.to_string(),
                revision.resolved_revision.clone(),
                chrono::Utc::now().timestamp(),
                token,
            )
            .await?;
        let inspection = RepositoryInspection {
            revision,
            inventory,
        };
        inspection.validate()?;
        Ok(inspection)
    }
}
