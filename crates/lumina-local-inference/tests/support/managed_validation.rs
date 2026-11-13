use super::*;
use crate::lifecycle::{
    baseline_pairing_runtime, RerankScore, RerankScoreSemantics, RuntimeParameters,
    VectorTokenUsage,
};
use std::collections::BTreeMap;

fn temp() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/managed-validation-tests");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root.canonicalize().unwrap()).unwrap()
}

const PROFILE: ValidationProfile =
    ValidationProfile::Vector(VectorRuntimeProfile::Qwen3Embedding06bQ8);

fn launch() -> LaunchConfig {
    let profile = VectorRuntimeProfile::Qwen3Embedding06bQ8;
    LaunchConfig {
        installation_id: "test-only".into(),
        version_id: "test-version".into(),
        manifest_digest: Sha256Digest::of_bytes(b"manifest"),
        runtime: baseline_pairing_runtime().unwrap(),
        capabilities: BTreeSet::from([profile.capability()]),
        component_files: BTreeMap::new(),
        parameters: profile.launch_parameters(),
        mode: RuntimeExecutionMode::Helper { port: 32101 },
    }
}

#[test]
fn corpus_identity_and_completeness_gate_match_the_preregistered_acceptance() {
    let suite = Suite::load().unwrap();
    assert_eq!(
        input_digest().as_str(),
        "a471f719e0c6efc652d1cf9e492b9c9d483b6ebb9f5f86e7a965e99375bc6d6c"
    );
    assert_eq!(verdict(17, 20, &suite, false), CapabilityStatus::Failed);
    assert_eq!(verdict(18, 19, &suite, false), CapabilityStatus::Failed);
    assert_eq!(verdict(20, 20, &suite, true), CapabilityStatus::Failed);
    assert_eq!(verdict(18, 20, &suite, false), CapabilityStatus::Ready);
}

#[test]
fn identity_excludes_only_the_owner_port() {
    let launch = launch();
    let identity = validation_identity(&launch, PROFILE).unwrap();
    let mut changed = launch.clone();
    changed.mode = RuntimeExecutionMode::Helper { port: 45673 };
    assert_ne!(launch.digest().unwrap(), changed.digest().unwrap());
    assert_eq!(identity, validation_identity(&changed, PROFILE).unwrap());
    let RuntimeParameters::Specialized { settings, .. } = &mut changed.parameters else {
        unreachable!()
    };
    settings.insert("threads".into(), "9".into());
    assert_ne!(identity, validation_identity(&changed, PROFILE).unwrap());
    for mutation in [0, 1, 2] {
        let mut other = launch.clone();
        match mutation {
            0 => other.manifest_digest = Sha256Digest::of_bytes(b"different model"),
            1 => other.runtime.binary_sha256 = Sha256Digest::of_bytes(b"other build"),
            _ => other.version_id = "different-version".into(),
        }
        assert_ne!(identity, validation_identity(&other, PROFILE).unwrap());
    }
}

#[test]
fn scoring_preserves_document_indices_and_raw_negative_reranker_logits() {
    let usage = VectorTokenUsage {
        prompt_tokens: 4,
        total_tokens: 4,
    };
    let digest = Sha256Digest::of_bytes(b"test launch");
    let embeddings = VectorOutput::Embed(EmbeddingBatch {
        launch_digest: digest.clone(),
        usage: usage.clone(),
        vectors: vec![
            vec![1.0, 0.0],
            vec![0.0, 1.0],
            vec![-1.0, 0.0],
            vec![1.0, 0.0],
        ],
    });
    assert_eq!(embeddings.top_index().unwrap(), 2);
    let reranked = VectorOutput::Rerank(RerankBatch {
        launch_digest: digest,
        usage,
        semantics: RerankScoreSemantics::RawClassifierLogit,
        scores: vec![
            RerankScore {
                index: 3,
                relevance_score: -0.12,
            },
            RerankScore {
                index: 0,
                relevance_score: -5.2,
            },
        ],
    });
    assert_eq!(reranked.top_index().unwrap(), 3);
}

fn failed_row(store: &InstallationStore, launch: &LaunchConfig, time: i64) -> CapabilityEvidence {
    let binding = ReportBinding {
        identity: validation_identity(launch, PROFILE).unwrap(),
        installation_id: launch.installation_id.clone(),
        version_id: launch.version_id.clone(),
        profile: PROFILE,
        input_digest: input_digest(),
        engine_location_digest: Sha256Digest::of_bytes(b"test engine location"),
        status: CapabilityStatus::Failed,
        validated_at: time,
    };
    let (evidence_path, digest) = write_report(
        store,
        REPORT_DIR,
        &serde_json::json!({"binding":binding,"failure":"test interruption","cases":[]}),
    )
    .unwrap();
    CapabilityEvidence {
        installation_id: binding.installation_id,
        capability: binding.profile.capability(),
        identity: binding.identity,
        status: binding.status,
        input_digest: binding.input_digest,
        output_digest: Some(digest),
        evidence_path,
        validated_at: time,
    }
}

#[test]
fn sqlite_history_keeps_the_latest_failure_of_the_requested_version() {
    let directory = temp();
    let store = InstallationStore::open(directory.path()).unwrap();
    let path = store.root().join("models.db");
    let repository = LifecycleRepository::open(&path).unwrap();
    let launch = launch();
    let row = failed_row(&store, &launch, 100);
    repository.save_evidence(&row).unwrap();
    let latest = failed_row(&store, &launch, 101);
    repository.save_evidence(&latest).unwrap();
    let mut different = launch.clone();
    different.version_id = "newer-installed-version".into();
    different.manifest_digest = Sha256Digest::of_bytes(b"newer installed manifest");
    repository
        .save_evidence(&failed_row(&store, &different, 102))
        .unwrap();
    drop(repository);
    let reopened = LifecycleRepository::open(&path).unwrap();
    let rows = reopened.evidence(&launch.installation_id).unwrap();
    assert_eq!(
        latest_evidence(rows.clone(), row.capability, &row.identity),
        Some(latest)
    );
    assert!(latest_evidence(rows.clone(), ModelCapability::Reranking, &row.identity).is_none());
    let mut unknown = row.identity.clone();
    unknown.manifest_digest = Sha256Digest::of_bytes(b"unknown");
    assert_eq!(
        latest_evidence(rows, row.capability, &unknown)
            .unwrap()
            .status_for(&unknown),
        CapabilityStatus::Stale
    );
}

#[test]
fn stored_report_detects_tampering_missing_content_and_swapped_bindings() {
    let directory = temp();
    let store = InstallationStore::open(directory.path()).unwrap();
    let launch = launch();
    let row = failed_row(&store, &launch, 100);
    let profile = PROFILE;
    check_report(&store, &row, &launch.version_id, profile).unwrap();
    assert!(check_report(&store, &row, "different version", profile).is_err());
    let mut altered = row.clone();
    altered.status = CapabilityStatus::Ready;
    assert!(check_report(&store, &altered, &launch.version_id, profile).is_err());
    altered.evidence_path = "../untrusted.json".into();
    assert!(check_report(&store, &altered, &launch.version_id, profile).is_err());
    let path = store.root().join(&row.evidence_path);
    std::fs::write(&path, b"changed").unwrap();
    assert!(check_report(&store, &row, &launch.version_id, profile).is_err());
    std::fs::remove_file(path).unwrap();
    assert!(check_report(&store, &row, &launch.version_id, profile).is_err());
}

#[test]
fn restoring_an_invalidated_report_does_not_reinstate_the_previous_verdict() {
    let directory = temp();
    let store = InstallationStore::open(directory.path()).unwrap();
    let repository = LifecycleRepository::open(&directory.path().join("models.db")).unwrap();
    let launch = launch();
    let row = failed_row(&store, &launch, 100);
    repository.save_evidence(&row).unwrap();
    let path = store.root().join(&row.evidence_path);
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"corrupt").unwrap();
    let profile = PROFILE;
    let error = check_report(&store, &row, &launch.version_id, profile).unwrap_err();
    let stale = invalidate(
        &repository,
        &store,
        &row,
        &launch.version_id,
        profile,
        &error.to_string(),
    )
    .unwrap();
    std::fs::write(path, original).unwrap();
    check_report(&store, &row, &launch.version_id, profile).unwrap();
    let marker = read_report(
        &store,
        REPORT_DIR,
        &stale.evidence_path,
        stale.output_digest.as_ref().unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&marker).unwrap()["previous_report"],
        row.evidence_path
    );
    let current = latest_evidence(
        repository.evidence(&row.installation_id).unwrap(),
        row.capability,
        &row.identity,
    )
    .unwrap();
    assert_eq!(current.status_for(&row.identity), CapabilityStatus::Stale);
    assert_eq!(current, stale);
}

#[test]
fn same_installation_validation_guard_excludes_overlapping_status_and_registration() {
    let directory = temp();
    let first = InstallationStore::open(directory.path()).unwrap();
    let second = InstallationStore::open(directory.path()).unwrap();
    let guard = first.lock(REPORT_DIR, "same installation").unwrap();
    assert!(second.lock(REPORT_DIR, "same installation").is_err());
    let other = second.lock(REPORT_DIR, "different installation").unwrap();
    drop(guard);
    second.lock(REPORT_DIR, "same installation").unwrap();
    drop(other);
}

#[test]
fn incorrect_selected_path_cannot_invalidate_the_previously_measured_binary() {
    let directory = temp();
    let trusted = directory.path().join("validated.exe");
    let wrong = directory.path().join("unrelated.exe");
    std::fs::write(&trusted, b"real bytes read from test file").unwrap();
    let digest = Sha256Digest::of_bytes(&std::fs::read(&trusted).unwrap());
    let location = engine_location_digest(&trusted).unwrap();
    assert!(check_engine(&trusted, &digest, &location)
        .unwrap()
        .is_none());
    assert!(check_engine(&wrong, &digest, &location).is_err());
    std::fs::write(&wrong, b"unrelated").unwrap();
    assert!(check_engine(&wrong, &digest, &location).is_err());
    std::fs::copy(&trusted, &wrong).unwrap();
    assert!(check_engine(&wrong, &digest, &location).unwrap().is_none());
    std::fs::write(&trusted, b"replaced").unwrap();
    assert!(check_engine(&trusted, &digest, &location)
        .unwrap()
        .is_some());
}
