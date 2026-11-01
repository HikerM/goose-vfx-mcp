use lumina_local_inference::lifecycle::*;
use serde_json::json;

fn inventory(files: serde_json::Value) -> RepositoryInventory {
    parse_modelscope_inventory(
        &serde_json::to_vec(&json!({"Code":200,"Data":{"Files":files}})).unwrap(),
        RepositorySnapshot {
            repository: ModelScopeRepository::parse("unit/variant-fixture").unwrap(),
            requested_revision: "master".into(),
            resolved_revision: "a".repeat(40),
            files_digest: Sha256Digest::of_bytes(b"unit placeholder"),
            fetched_at: 1,
        },
    )
    .unwrap()
}
fn file(path: &str) -> serde_json::Value {
    json!({"Path":path,"Type":"blob","Size":1024})
}

#[test]
fn equal_quantization_names_do_not_merge_different_models_or_directories() {
    let source = inventory(json!([
        file("first-Q4_K_M.gguf"),
        file("second-Q4_K_M.gguf"),
        file("a/model-00001-of-00002.gguf"),
        file("a/model-00002-of-00002.gguf"),
        file("b/model-00001-of-00002.gguf"),
        file("b/model-00002-of-00002.gguf"),
        file("README.md")
    ]));
    let candidates = discover_gguf_variant_candidates(&source).unwrap();
    assert_eq!(candidates.len(), 4);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.files.len())
            .sum::<usize>(),
        6
    );
    assert!(candidates
        .iter()
        .all(|candidate| candidate.status == GgufCandidateStatus::NeedsContentInspection));
}

#[test]
fn filename_hints_do_not_fabricate_missing_shards_or_download_addresses() {
    let candidates =
        discover_gguf_variant_candidates(&inventory(json!([file("model-00001-of-99999.gguf")])))
            .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].files.len(), 1);
    assert!(candidates[0]
        .issues
        .contains(&GgufCandidateIssue::IncompleteFilenameGroup {
            advertised_count: 99999,
            observed_count: 1
        }));
    assert_eq!(
        candidates[0].files[0].path.as_str(),
        "model-00001-of-99999.gguf"
    );
}

#[test]
fn source_unknown_empty_and_unmaterializable_files_stay_visible_with_distinct_issues() {
    let candidates=discover_gguf_variant_candidates(&inventory(json!([{"Path":"unknown.gguf","Type":"blob"},{"Path":"empty.gguf","Type":"blob","Size":0},file("NUL.gguf"),file("preview\\weights.gguf")]))).unwrap();
    assert_eq!(candidates.len(), 4);
    let unknown = candidates
        .iter()
        .find(|candidate| candidate.files[0].path.as_str() == "unknown.gguf")
        .unwrap();
    assert_eq!(unknown.files[0].size_bytes, None);
    assert_eq!(unknown.files[0].source_sha256, None);
    assert!(unknown
        .issues
        .iter()
        .any(|issue| matches!(issue, GgufCandidateIssue::UnknownFileSize { .. })));
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.files[0].materialization
                == CandidateMaterialization::EmptyFile)
    );
    assert_eq!(
        candidates
            .iter()
            .filter(|candidate| candidate.files[0].materialization
                == CandidateMaterialization::UnsupportedPath)
            .count(),
        2
    );
}

#[test]
fn candidate_identity_is_source_bound_and_independent_of_listing_order() {
    let first =
        discover_gguf_variant_candidates(&inventory(json!([file("b.gguf"), file("a.gguf")])))
            .unwrap();
    let other =
        discover_gguf_variant_candidates(&inventory(json!([file("a.gguf"), file("b.gguf")])))
            .unwrap();
    assert_eq!(first, other);
    assert_ne!(first[0].id, first[1].id);
    let mut changed = inventory(json!([file("a.gguf"), file("b.gguf")]));
    changed.snapshot.resolved_revision = "b".repeat(40);
    assert_ne!(
        first[0].id,
        discover_gguf_variant_candidates(&changed).unwrap()[0].id
    );
}

#[test]
fn invalid_filename_ordinals_are_hints_not_verified_content_or_manufactured_files() {
    let candidates = discover_gguf_variant_candidates(&inventory(json!([
        file("model-00000-of-00002.gguf"),
        file("other-00003-of-00002.GGUF")
    ])))
    .unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().all(|candidate| candidate
        .issues
        .contains(&GgufCandidateIssue::InvalidFilenameOrdinal)));
    assert!(candidates
        .iter()
        .all(|candidate| candidate.files.len() == 1));
}

#[test]
fn malformed_shard_stems_cannot_collapse_an_existing_single_file_candidate() {
    let candidates = discover_gguf_variant_candidates(&inventory(json!([
        file("model.gguf"),
        file("model.gguf-00000-of-00000.gguf")
    ])))
    .unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .all(|candidate| candidate.files.len() == 1));
    assert!(candidates
        .iter()
        .any(|candidate| candidate.filename_hint == GgufFilenameGroupHint::SingleFile));
}
