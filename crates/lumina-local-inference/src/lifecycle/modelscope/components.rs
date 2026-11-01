use anyhow::{ensure, Result};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::Duration;

use super::{
    discover_gguf_variant_candidates, GgufVariantCandidate, ModelScopeProvider, ModelSourceError,
    RemoteGgufInspection, RepositoryInventory,
};
use crate::lifecycle::{
    ComponentObservation, ComponentRole, FilePurpose, GgufFilenameGroupHint, GgufInspection,
    GgufReadLimits, LifecycleErrorCode, RepositoryPath, Sha256Digest, SourceGgufObservation,
};

const MAX_COMPONENT_FILES: usize = 128;
const MAX_COMPONENT_HEADER_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentHeaderRead {
    pub artifact_id: String,
    pub path: RepositoryPath,
    pub header_digest: Sha256Digest,
    pub bytes_fetched: u64,
    pub range_http_requests: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedGgufComponent {
    pub observation: ComponentObservation,
    pub header_reads: Vec<ComponentHeaderRead>,
}

fn source_error(code: LifecycleErrorCode) -> ModelSourceError {
    ModelSourceError {
        code,
        http_status: None,
    }
}

fn variant_paths(
    candidate: &GgufVariantCandidate,
    selected_path: &RepositoryPath,
    header: &GgufInspection,
) -> Result<Vec<RepositoryPath>> {
    if header.shard.as_ref().is_none_or(|shard| shard.count == 1) {
        return Ok(vec![selected_path.clone()]);
    }
    let actual_count = header.shard.as_ref().expect("Split GGUF was checked").count;
    let advertised_count = match &candidate.filename_hint {
        GgufFilenameGroupHint::ShardPattern {
            advertised_count, ..
        } => u64::from(*advertised_count),
        GgufFilenameGroupHint::SingleFile => 0,
    };
    ensure!(
        advertised_count == actual_count && candidate.files.len() as u64 == actual_count,
        source_error(LifecycleErrorCode::ShardIncomplete)
    );
    ensure!(
        candidate.files.len() <= MAX_COMPONENT_FILES,
        "Variant shard count exceeds inspection budget"
    );
    candidate
        .files
        .iter()
        .map(|file| file.path.materializable_path())
        .collect()
}

fn finish_component(
    inventory: &RepositoryInventory,
    role: ComponentRole,
    reads: Vec<RemoteGgufInspection>,
) -> Result<ResolvedGgufComponent> {
    let digest = inventory.snapshot.content_digest()?;
    let mut files = Vec::with_capacity(reads.len());
    let mut header_reads = Vec::with_capacity(reads.len());
    for read in reads {
        ensure!(
            read.snapshot_digest == digest,
            source_error(LifecycleErrorCode::PlanStale)
        );
        let artifact = inventory.artifact(&read.path, FilePurpose::GgufWeight)?;
        ensure!(
            artifact.source_sha256 == read.source_sha256,
            source_error(LifecycleErrorCode::RevisionChanged)
        );
        header_reads.push(ComponentHeaderRead {
            artifact_id: artifact.id.clone(),
            path: read.path,
            header_digest: read.inspection.header_digest.clone(),
            bytes_fetched: read.bytes_fetched,
            range_http_requests: read.range_requests,
        });
        files.push(SourceGgufObservation {
            artifact,
            snapshot: inventory.snapshot.clone(),
            header: read.inspection,
        });
    }
    files.sort_by_key(|file| file.header.shard.as_ref().map_or(0, |shard| shard.index));
    header_reads.sort_by(|left, right| left.path.as_str().cmp(right.path.as_str()));
    let observation = ComponentObservation { role, files };
    observation.validate()?;
    Ok(ResolvedGgufComponent {
        observation,
        header_reads,
    })
}

impl ModelScopeProvider {
    pub async fn inspect_selected_component(
        &self,
        inventory: &RepositoryInventory,
        paths: &[RepositoryPath],
        role: ComponentRole,
        limits: GgufReadLimits,
        token: Option<&str>,
    ) -> Result<ResolvedGgufComponent> {
        self.inspect_component_with_reads(inventory, paths, role, limits, token, Vec::new())
            .await
    }

    async fn inspect_component_with_reads(
        &self,
        inventory: &RepositoryInventory,
        paths: &[RepositoryPath],
        role: ComponentRole,
        limits: GgufReadLimits,
        token: Option<&str>,
        mut reads: Vec<RemoteGgufInspection>,
    ) -> Result<ResolvedGgufComponent> {
        inventory.validate()?;
        ensure!(
            !paths.is_empty() && paths.len() <= MAX_COMPONENT_FILES,
            "Component file selection exceeds inspection budget"
        );
        let mut selected = BTreeSet::new();
        for path in paths {
            ensure!(
                selected.insert(path.as_str()),
                "Repeated selected component path"
            );
            inventory.artifact(path, FilePurpose::GgufWeight)?;
        }
        let mut already_read = BTreeSet::new();
        let mut header_bytes = 0u64;
        for read in &reads {
            ensure!(
                selected.contains(read.path.as_str())
                    && already_read.insert(read.path.as_str().to_string()),
                "Seeded component observation does not match its selected files"
            );
            header_bytes = header_bytes
                .checked_add(read.inspection.header_bytes)
                .ok_or_else(|| anyhow::anyhow!("Component header budget overflow"))?;
        }
        ensure!(
            header_bytes <= MAX_COMPONENT_HEADER_BYTES,
            "Component header metadata exceeds inspection budget"
        );
        let remaining = paths
            .iter()
            .filter(|path| !already_read.contains(path.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        tokio::time::timeout(Duration::from_secs(300), async {
            let mut pending = stream::iter(remaining)
                .map(|path| async move {
                    self.inspect_remote_gguf(inventory, &path, limits, token)
                        .await
                })
                .buffer_unordered(3);
            while let Some(read) = pending.next().await {
                let read = read?;
                header_bytes = header_bytes
                    .checked_add(read.inspection.header_bytes)
                    .ok_or_else(|| anyhow::anyhow!("Component header budget overflow"))?;
                ensure!(
                    header_bytes <= MAX_COMPONENT_HEADER_BYTES,
                    "Component header metadata exceeds inspection budget"
                );
                reads.push(read);
            }
            finish_component(inventory, role, reads)
        })
        .await
        .map_err(|_| source_error(LifecycleErrorCode::SourceTimeout))?
    }

    pub async fn inspect_variant_candidate(
        &self,
        inventory: &RepositoryInventory,
        candidate_id: &Sha256Digest,
        selected_path: &RepositoryPath,
        role: ComponentRole,
        limits: GgufReadLimits,
        token: Option<&str>,
    ) -> Result<ResolvedGgufComponent> {
        let candidate = discover_gguf_variant_candidates(inventory)?
            .into_iter()
            .find(|candidate| &candidate.id == candidate_id)
            .ok_or_else(|| source_error(LifecycleErrorCode::PlanStale))?;
        ensure!(
            candidate
                .files
                .iter()
                .any(|file| file.path.as_str() == selected_path.as_str()),
            source_error(LifecycleErrorCode::SourceNotFound)
        );
        let primary = self
            .inspect_remote_gguf(inventory, selected_path, limits, token)
            .await?;
        let paths = variant_paths(&candidate, selected_path, &primary.inspection)?;
        self.inspect_component_with_reads(inventory, &paths, role, limits, token, vec![primary])
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{
        parse_modelscope_inventory, GgufByteOrder, GgufShard, ModelScopeRepository,
        RepositorySnapshot,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    fn source() -> RepositoryInventory {
        parse_modelscope_inventory(&serde_json::to_vec(&json!({"Code":200,"Data":{"Files":[{"Path":"model-00001-of-00002.gguf","Type":"blob","Size":1024},{"Path":"model-00002-of-00002.gguf","Type":"blob","Size":1024}]}})).unwrap(),RepositorySnapshot {repository:ModelScopeRepository::parse("unit/shard-candidates").unwrap(),requested_revision:"master".into(),resolved_revision:"a".repeat(40),files_digest:Sha256Digest::of_bytes(b"unit placeholder"),fetched_at:1}).unwrap()
    }

    fn header(shard: Option<GgufShard>) -> GgufInspection {
        GgufInspection {
            version: 3,
            byte_order: GgufByteOrder::LittleEndian,
            file_bytes: 1024,
            header_bytes: 128,
            header_digest: Sha256Digest::of_bytes(b"unit header"),
            data_offset: 128,
            alignment: 32,
            metadata: BTreeMap::new(),
            tensors: Vec::new(),
            stored_tensor_elements: 0,
            encoded_tensor_bytes: 0,
            shard,
        }
    }

    #[test]
    fn content_metadata_overrides_filename_shard_appearance() {
        let candidates = discover_gguf_variant_candidates(&source()).unwrap();
        let selected = RepositoryPath::parse("model-00001-of-00002.gguf").unwrap();
        assert_eq!(
            variant_paths(&candidates[0], &selected, &header(None)).unwrap(),
            vec![selected.clone()]
        );
        assert_eq!(
            variant_paths(
                &candidates[0],
                &selected,
                &header(Some(GgufShard {
                    index: 0,
                    count: 2,
                    total_tensors: 2
                }))
            )
            .unwrap()
            .len(),
            2
        );
        let error = variant_paths(
            &candidates[0],
            &selected,
            &header(Some(GgufShard {
                index: 0,
                count: 3,
                total_tensors: 3,
            })),
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ModelSourceError>().unwrap().code,
            LifecycleErrorCode::ShardIncomplete
        );
    }

    #[tokio::test]
    async fn invalid_or_duplicate_selections_fail_before_network_access() {
        let provider = ModelScopeProvider::new().unwrap();
        let source = source();
        assert!(provider
            .inspect_selected_component(
                &source,
                &[],
                ComponentRole::Main,
                GgufReadLimits::default(),
                None
            )
            .await
            .is_err());
        let path = RepositoryPath::parse("model-00001-of-00002.gguf").unwrap();
        assert!(provider
            .inspect_selected_component(
                &source,
                &[path.clone(), path],
                ComponentRole::Main,
                GgufReadLimits::default(),
                None
            )
            .await
            .is_err());
        let error = provider
            .inspect_selected_component(
                &source,
                &[RepositoryPath::parse("absent.gguf").unwrap()],
                ComponentRole::Main,
                GgufReadLimits::default(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ModelSourceError>().unwrap().code,
            LifecycleErrorCode::SourceNotFound
        );
    }
}
