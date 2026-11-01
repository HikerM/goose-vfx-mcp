use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Observations {
    cases: Vec<ObservedRepository>,
}

#[derive(Deserialize)]
struct ObservedRepository {
    category: String,
    repository: String,
    revision: String,
    files: Vec<ObservedFile>,
}

#[derive(Deserialize)]
struct ObservedFile {
    path: String,
    status: String,
    #[serde(rename = "headerSha256")]
    header_sha256: Option<Sha256Digest>,
}

#[derive(Serialize)]
struct ClassifiedRepository {
    category: String,
    repository: String,
    revision: String,
    snapshot_digest: Sha256Digest,
    files: Vec<GgufClassification>,
    shard_checks: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct ObservedShardGroup {
    repository: String,
    revision: String,
    paths: Vec<String>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, budget: u64) -> Result<T> {
    let file = std::fs::File::open(path)?;
    ensure!(
        file.metadata()?.len() <= budget,
        "Observation exceeds its JSON byte budget"
    );
    Ok(serde_json::from_reader(file.take(budget + 1))?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    ensure!(
        matches!(arguments.len(), 3 | 4),
        "Usage: classify_observed_gguf observations.json prefix-cache output.json [explicit-shard-groups.json]"
    );
    let observations: Observations = read_json(&arguments[0], 16 * 1024 * 1024)?;
    let groups: Vec<ObservedShardGroup> = if let Some(path) = arguments.get(3) {
        read_json(path, 1024 * 1024)?
    } else {
        Vec::new()
    };
    ensure!(groups.len() <= 32, "Too many explicit shard groups");
    for group in &groups {
        ensure!(
            !group.paths.is_empty() && group.paths.len() <= 32,
            "Invalid shard group size"
        );
        ensure!(
            observations
                .cases
                .iter()
                .any(|case| case.repository == group.repository && case.revision == group.revision),
            "Explicit shard group has no pinned repository observation"
        );
    }
    ensure!(
        observations.cases.len() <= 64,
        "Too many source repositories in one observation"
    );
    let provider = ModelScopeProvider::new()?;
    let mut classified = Vec::new();
    let mut file_count = 0;
    for case in observations.cases {
        let inventory = provider
            .inspect_at(
                ModelScopeRepository::parse(&case.repository)?,
                case.revision.clone(),
                case.revision.clone(),
                chrono::Utc::now().timestamp(),
                None,
            )
            .await?;
        inventory.validate()?;
        let snapshot_digest = inventory.snapshot.content_digest()?;
        let snapshot_id = snapshot_digest.as_str().to_string();
        let mut files = Vec::new();
        let mut selected_shards = BTreeMap::new();
        let mut selected_header_bytes = 0u64;
        ensure!(
            case.files.len() <= 256,
            "Too many GGUF observations in one source repository"
        );
        for file in case.files {
            if file.status != "PARSED" {
                continue;
            }
            let cache_key = Sha256Digest::of_bytes(
                format!("{}@{}:{}", case.repository, case.revision, file.path).as_bytes(),
            );
            let source = inventory
                .files
                .get(&file.path)
                .context("Observed GGUF is absent from its pinned inventory")?;
            let inspection = inspect_gguf(
                std::fs::File::open(arguments[1].join(format!("{}.prefix", cache_key.as_str())))?,
                source.size_bytes.context("Pinned GGUF size is unknown")?,
                GgufReadLimits::default(),
            )?;
            ensure!(
                file.header_sha256.as_ref() == Some(&inspection.header_digest),
                "Actual GGUF header differs from the prior source observation"
            );
            let artifact = Artifact {
                id: Sha256Digest::of_serializable(&(&snapshot_digest, &source.path))?
                    .as_str()
                    .to_string(),
                snapshot_id: snapshot_id.clone(),
                path: source.path.materializable_path()?,
                purpose: FilePurpose::GgufWeight,
                size_bytes: source.size_bytes,
                source_sha256: source.sha256.clone(),
            };
            files.push(classify_gguf(
                &artifact,
                &snapshot_id,
                &inventory.snapshot,
                &inspection,
                &[],
            )?);
            if groups.iter().any(|group| {
                group.repository == case.repository
                    && group.revision == case.revision
                    && group.paths.contains(&file.path)
            }) {
                selected_header_bytes = selected_header_bytes
                    .checked_add(inspection.header_bytes)
                    .context("Shard header size overflow")?;
                ensure!(
                    selected_header_bytes <= 128 * 1024 * 1024,
                    "Selected shard headers exceed byte budget"
                );
                selected_shards.insert(file.path, inspection);
            }
            file_count += 1;
        }
        let mut shard_checks = Vec::new();
        for group in groups
            .iter()
            .filter(|group| group.repository == case.repository && group.revision == case.revision)
        {
            let shards: Vec<_> = group
                .paths
                .iter()
                .map(|path| {
                    selected_shards
                        .get(path)
                        .cloned()
                        .context("Explicit shard was not parsed from actual prefix bytes")
                })
                .collect::<Result<_>>()?;
            validate_gguf_shard_set(&shards)?;
            let missing_rejected =
                shards.len() > 1 && validate_gguf_shard_set(&shards[..shards.len() - 1]).is_err();
            let mut duplicate = shards.clone();
            if duplicate.len() > 1 {
                duplicate[1] = duplicate[0].clone();
            }
            let duplicate_rejected =
                duplicate.len() > 1 && validate_gguf_shard_set(&duplicate).is_err();
            shard_checks.push(serde_json::json!({
                "paths":group.paths,"status":"PASS",
                "headerDigests":shards.iter().map(|shard| &shard.header_digest).collect::<Vec<_>>(),
                "tensorCount":shards.iter().map(|shard| shard.tensors.len()).sum::<usize>(),
                "missingShardRejected":missing_rejected,"duplicateShardRejected":duplicate_rejected,
                "scope":"Explicit observed container group only; not model-component pairing, MTP or runtime acceptance"
            }));
        }
        classified.push(ClassifiedRepository {
            category: case.category,
            repository: case.repository,
            revision: case.revision,
            snapshot_digest,
            files,
            shard_checks,
        });
    }
    let report = serde_json::json!({
        "observedAt":chrono::Utc::now().to_rfc3339(),
        "scope":"Content classification of actual bounded GGUF headers with fresh pinned ModelScope inventories; no task declarations fabricated, no full weight hash, runtime, dependency or installation capability acceptance",
        "rulesVersion":CLASSIFICATION_RULES_VERSION,
        "sourceSha256":Sha256Digest::of_bytes(include_bytes!("../src/lifecycle/classification.rs")),
        "repositories":classified,"classifiedFiles":file_count,
    });
    std::fs::write(&arguments[2], serde_json::to_vec_pretty(&report)?)?;
    println!("Classified {file_count} actual GGUF headers; runtime readiness is not inferred.");
    Ok(())
}
