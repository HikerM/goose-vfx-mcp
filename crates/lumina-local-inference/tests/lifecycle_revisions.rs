use lumina_local_inference::lifecycle::*;

fn packet(payload: &[u8]) -> Vec<u8> {
    let mut bytes = format!("{:04x}", payload.len() + 4).into_bytes();
    bytes.extend(payload);
    bytes
}

fn advertisement(records: &[String]) -> Vec<u8> {
    let mut bytes = packet(b"# service=git-upload-pack\n");
    bytes.extend(b"0000");
    for record in records {
        bytes.extend(packet(record.as_bytes()));
    }
    bytes.extend(b"0000");
    bytes
}

#[test]
fn resolves_exact_heads_tags_and_peeled_commits_without_guessing() {
    let head = "a".repeat(40);
    let tag_object = "b".repeat(40);
    let tag_commit = "c".repeat(40);
    let bytes = advertisement(&[
        format!("{head} HEAD\0symref=HEAD:refs/heads/master object-format=sha1\n"),
        format!("{head} refs/heads/master\n"),
        format!("{tag_object} refs/tags/v1\n"),
        format!("{tag_commit} refs/tags/v1^{{}}\n"),
    ]);
    let refs = parse_git_advertisement(&bytes).unwrap();
    assert_eq!(refs.digest(), &Sha256Digest::of_bytes(&bytes));
    assert_eq!(
        refs.resolve("HEAD").unwrap(),
        ("HEAD".to_string(), head.clone())
    );
    assert_eq!(
        refs.resolve("master").unwrap(),
        ("refs/heads/master".to_string(), head)
    );
    assert_eq!(
        refs.resolve("v1").unwrap(),
        ("refs/tags/v1".to_string(), tag_commit.clone())
    );
    assert_eq!(refs.resolve("refs/tags/v1").unwrap().1, tag_commit);
    for missing in [
        "main",
        "aaaaaaa",
        "HEAD~1",
        "v1^{}",
        "../master",
        "refs/heads/.hidden",
    ] {
        assert!(
            refs.resolve(missing).is_err(),
            "unexpected reference {missing}"
        );
    }
}

#[test]
fn rejects_ambiguous_short_refs_even_when_the_commits_match() {
    let commit = "d".repeat(40);
    let refs = parse_git_advertisement(&advertisement(&[
        format!("{commit} refs/heads/release\n"),
        format!("{commit} refs/tags/release\n"),
    ]))
    .unwrap();
    assert!(refs.resolve("release").is_err());
    assert_eq!(refs.resolve("refs/heads/release").unwrap().1, commit);
}

#[test]
fn rejects_truncated_malformed_and_conflicting_advertisements() {
    let commit = "a".repeat(40);
    let bytes = advertisement(&[
        format!("{commit} HEAD\0agent=git/test\n"),
        format!("{commit} refs/heads/master\n"),
    ]);
    for end in 0..bytes.len() {
        assert!(
            parse_git_advertisement(&bytes[..end]).is_err(),
            "accepted prefix {end}"
        );
    }
    let mut trailing = bytes;
    trailing.extend(b"extra");
    assert!(parse_git_advertisement(&trailing).is_err());
    for records in [
        vec![format!("{commit} HEAD\n"), format!("{commit} HEAD\n")],
        vec![
            format!("{commit} refs/heads/master\n"),
            format!("{} refs/heads/dev\n", "b".repeat(64)),
        ],
        vec![format!("{commit} refs/tags/v1^{{}}\n")],
        vec![format!("{commit} refs/heads/a b\n")],
        vec![
            format!("{commit} refs/heads/a\n"),
            format!("{commit} refs/heads/b\0agent=x\n"),
        ],
        vec!["ERR access denied\n".to_string()],
        vec!["version 2\n".to_string()],
    ] {
        assert!(parse_git_advertisement(&advertisement(&records)).is_err());
    }
    assert!(parse_git_advertisement(b"ffff").is_err());
    assert!(parse_git_advertisement(b"<html>login</html>").is_err());
}

#[test]
fn handles_sha256_ids_and_unborn_repositories_explicitly() {
    let commit = "e".repeat(64);
    let refs = parse_git_advertisement(&advertisement(&[format!(
        "{commit} HEAD\0object-format=sha256\n"
    )]))
    .unwrap();
    assert_eq!(refs.resolve("HEAD").unwrap().1, commit);
    let empty = parse_git_advertisement(&advertisement(&[format!(
        "{} capabilities^{{}}\0multi_ack\n",
        "0".repeat(40)
    )]))
    .unwrap();
    assert!(empty.resolve("HEAD").is_err());
    let named = parse_git_advertisement(&advertisement(&[
        format!("{commit} HEAD\0symref=HEAD:refs/heads/量化 object-format=sha256\n"),
        format!("{commit} refs/heads/量化\n"),
    ]))
    .unwrap();
    assert_eq!(named.resolve("量化").unwrap().1, commit);
}

#[tokio::test]
async fn explicit_commit_resolution_needs_no_git_executable_or_network() {
    let provider = ModelScopeProvider::new().unwrap();
    let repo = ModelScopeRepository::parse("test-owner/test-model").unwrap();
    let commit = "A".repeat(40);
    let revision = provider
        .resolve_revision(&repo, &commit, None)
        .await
        .unwrap();
    assert_eq!(revision.resolved_revision, commit.to_lowercase());
    assert_eq!(revision.requested_revision, commit);
    assert_eq!(revision.proof, RevisionResolutionProof::ExplicitCommit);
}

#[tokio::test]
#[ignore = "requires real ModelScope smart HTTP and file APIs; no external git executable"]
async fn real_modelscope_branch_resolves_and_pins_the_same_file_inventory() {
    let provider = ModelScopeProvider::new().unwrap();
    let repo = ModelScopeRepository::parse("Qwen/Qwen3-1.7B-GGUF").unwrap();
    let inspection = provider
        .inspect_repository(repo, "master", None)
        .await
        .unwrap();
    inspection.validate().unwrap();
    assert!(
        matches!(&inspection.revision.proof, RevisionResolutionProof::AdvertisedRef { reference, .. } if reference == "refs/heads/master")
    );
    assert_eq!(inspection.revision.resolved_revision.len(), 40);
    let path = RepositoryPath::parse("Qwen3-1.7B-Q8_0.gguf").unwrap();
    let url = inspection.inventory.download_url(&path).unwrap();
    assert!(url
        .query_pairs()
        .any(|(key, value)| key == "Revision" && value == inspection.revision.resolved_revision));
    let root = std::path::PathBuf::from(std::env::var("LUMINA_PATH_ROOT").unwrap());
    assert!(root.is_absolute());
    std::fs::create_dir_all(root.join("evidence")).unwrap();
    std::fs::write(
        root.join("evidence/revision-inspection.json"),
        serde_json::to_vec_pretty(&inspection).unwrap(),
    )
    .unwrap();
    println!(
        "resolved={}, files={}",
        inspection.revision.resolved_revision,
        inspection.inventory.files.len()
    );
}
