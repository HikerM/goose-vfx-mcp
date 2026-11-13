use anyhow::{ensure, Context, Result};
use lumina_local_inference::lifecycle::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CASES: &str = include_str!("fixtures/vector-relevance-v1.json");

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelevanceSuite {
    minimum_top1_correct: usize,
    cases: Vec<RelevanceCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelevanceCase {
    id: String,
    query: String,
    documents: Vec<String>,
    expected_index: usize,
}

fn required_path(name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name).with_context(|| format!("Set {name}"))?);
    ensure!(path.is_absolute(), "{name} must be absolute");
    Ok(path)
}

async fn exercise(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    engine: &Path,
    logs: &Path,
    version: &str,
    capability: ModelCapability,
    suite: &RelevanceSuite,
) -> Result<Value> {
    let installed = repository
        .installed_version(version)?
        .context("Expected a registered vector installation")?;
    ensure!(
        installed.manifest.plan.target_capabilities == BTreeSet::from([capability]),
        "Wrong installed capability"
    );
    let launch = LaunchConfig::for_measured_cpu_installation(&installed)?;
    let mut runtime = NativeHelperRuntime::start(
        repository,
        store,
        launch,
        engine,
        logs,
        Duration::from_secs(180),
    )
    .await?;
    let started = Instant::now();
    let operation: Result<Value> = async {
        let mut cases = Vec::new();
        let mut correct = 0;
        if capability == ModelCapability::Embedding {
            let adapter = NativeEmbeddingAdapter::new(&runtime, &installed.manifest, Duration::from_secs(180))?;
            ensure!(adapter.embed(&[]).await.is_err(), "Empty embedding batch was accepted");
            ensure!(adapter.embed(&["x".repeat(8193)]).await.is_err(), "Oversize embedding text was accepted");
            for case in &suite.cases {
                let inputs = std::iter::once(case.query.clone()).chain(case.documents.clone()).collect::<Vec<_>>();
                let response = adapter.embed(&inputs).await?;
                let query = &response.vectors[0];
                let scores = response.vectors[1..].iter().map(|document| {
                    query.iter().zip(document).map(|(left, right)| left * right).sum::<f64>()
                }).collect::<Vec<_>>();
                let top = scores.iter().enumerate().max_by(|left, right| left.1.total_cmp(right.1)).unwrap().0;
                let passed = top == case.expected_index;
                correct += usize::from(passed);
                cases.push(json!({"id":case.id,"expected_index":case.expected_index,"actual_index":top,
                    "scores":scores,"passed":passed,"response":response}));
            }
        } else {
            let adapter = NativeRerankAdapter::new(&runtime, &installed.manifest, Duration::from_secs(180))?;
            ensure!(adapter.rerank("query", &[]).await.is_err(), "Empty ranking batch was accepted");
            ensure!(adapter.rerank("", &["document".into()]).await.is_err(), "Empty ranking query was accepted");
            for case in &suite.cases {
                let response = adapter.rerank(&case.query, &case.documents).await?;
                let top = response.scores.iter().max_by(|left, right| left.relevance_score.total_cmp(&right.relevance_score)).unwrap().index;
                let passed = top == case.expected_index;
                correct += usize::from(passed);
                cases.push(json!({"id":case.id,"expected_index":case.expected_index,"actual_index":top,
                    "passed":passed,"response":response}));
            }
        }
        Ok(json!({"correct":correct,"total":cases.len(),"minimum":suite.minimum_top1_correct,
            "status":if correct >= suite.minimum_top1_correct {"PASS"} else {"FAIL"}, "cases":cases}))
    }.await;
    let stopped = runtime.stop();
    let status = stopped?;
    Ok(json!({
        "capability":capability,"version_id":version,"manifest_digest":installed.manifest_digest,
        "launch":runtime.actual_config(),"helper_stopped":runtime.child_id().is_none(),
        "helper_exit_code":status.code(),"stdout_log":runtime.stdout_path(),"stderr_log":runtime.stderr_path(),
        "elapsed_seconds":started.elapsed().as_secs_f64(),
        "result":match operation { Ok(result) => result, Err(error) => json!({"status":"FAIL","error":error.to_string()}) },
    }))
}

#[tokio::test]
#[ignore = "Runs two installed models, writes managed capability evidence and reopens the registry"]
async fn managed_validator_registers_verified_results_for_both_installed_models() -> Result<()> {
    let root = required_path("LUMINA_VECTOR_STORE")?;
    let engine = required_path("LUMINA_VECTOR_ENGINE")?;
    let store = InstallationStore::open(&root)?;
    let db = root.join("models.db");
    for key in ["LUMINA_EMBEDDING_VERSION", "LUMINA_RERANK_VERSION"] {
        let version = std::env::var(key)?;
        let checked = {
            let repository = LifecycleRepository::open(&db)?;
            run_managed_vector_validation(
                &repository,
                &store,
                &version,
                &engine,
                std::time::Duration::from_secs(180),
            )
            .await?
        };
        ensure!(
            checked.completed == 20
                && checked.correct >= 18
                && checked.evidence.status == CapabilityStatus::Ready,
            "Managed validation failed: {checked:?}"
        );
        let reopened =
            managed_vector_status(&LifecycleRepository::open(&db)?, &store, &version, &engine)?;
        ensure!(
            reopened.status == CapabilityStatus::Ready
                && reopened.evidence.as_ref() == Some(&checked.evidence),
            "Reopened registry lost the verified result"
        );
        println!("{}", serde_json::to_string(&checked)?);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "Requires two real registered installations, the pinned native helper and explicit output path"]
async fn installed_vectors_pass_fixed_bilingual_relevance_through_owned_runtime() -> Result<()> {
    let suite: RelevanceSuite = serde_json::from_str(CASES)?;
    ensure!(
        suite.cases.len() == 20 && suite.minimum_top1_correct == 18,
        "Acceptance fixture changed"
    );
    ensure!(
        suite
            .cases
            .iter()
            .map(|case| &case.id)
            .collect::<BTreeSet<_>>()
            .len()
            == 20,
        "Repeated fixture ID"
    );
    ensure!(
        suite
            .cases
            .iter()
            .all(|case| case.documents.len() == 4 && case.expected_index < 4),
        "Invalid relevance fixture"
    );
    let root = required_path("LUMINA_VECTOR_STORE")?;
    let engine = required_path("LUMINA_VECTOR_ENGINE")?;
    let report = required_path("LUMINA_VECTOR_REPORT")?;
    ensure!(!report.exists(), "Acceptance report already exists");
    let store = InstallationStore::open(&root)?;
    let repository = LifecycleRepository::open(&root.join("models.db"))?;
    let mut results = Vec::new();
    for (environment, capability, directory) in [
        (
            "LUMINA_EMBEDDING_VERSION",
            ModelCapability::Embedding,
            "embedding",
        ),
        (
            "LUMINA_RERANK_VERSION",
            ModelCapability::Reranking,
            "reranker",
        ),
    ] {
        let version = std::env::var(environment).with_context(|| format!("Set {environment}"))?;
        let logs = report.parent().unwrap().join(directory);
        results.push(
            exercise(
                &repository,
                &store,
                &engine,
                &logs,
                &version,
                capability,
                &suite,
            )
            .await?,
        );
    }
    let passed = results
        .iter()
        .all(|result| result["result"]["status"] == "PASS" && result["helper_stopped"] == true);
    std::fs::create_dir_all(report.parent().unwrap())?;
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report)?;
    serde_json::to_writer_pretty(
        output,
        &json!({
            "schema":"lumina-managed-vector-acceptance-v1","status":if passed {"PASS"} else {"FAIL"},
            "fixture_sha256":Sha256Digest::of_bytes(CASES.as_bytes()),"profiles":results,
            "scope":"Real installed CPU vector operations and fixed 20-case bilingual relevance per profile. No GPU, concurrency benchmark, service routing or capability READY registration.",
        }),
    )?;
    ensure!(
        passed,
        "Managed vector relevance failed; inspect the saved report"
    );
    Ok(())
}
