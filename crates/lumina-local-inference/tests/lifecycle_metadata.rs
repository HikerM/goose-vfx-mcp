use lumina_local_inference::lifecycle::*;
use serde_json::json;
use std::collections::BTreeSet;

fn inventory(path: &str, bytes: &[u8]) -> RepositoryInventory {
    let snapshot = RepositorySnapshot {
        repository: ModelScopeRepository::parse("owner/mixed-repository").unwrap(),
        requested_revision: "master".into(),
        resolved_revision: "a".repeat(40),
        files_digest: Sha256Digest::of_bytes(b"placeholder"),
        fetched_at: 100,
    };
    parse_modelscope_inventory(&serde_json::to_vec(&json!({"Code":200,"Data":{"Files":[{
        "Path":path,"Type":"blob","Mode":"33188","Size":bytes.len(),"Sha256":Sha256Digest::of_bytes(bytes)
    }]}})).unwrap(), snapshot).unwrap()
}

fn observe(path: &str, text: &str) -> DocumentTaskObservation {
    let path = RepositoryPath::parse(path).unwrap();
    let document = verify_source_document(
        &inventory(path.as_str(), text.as_bytes()),
        &path,
        text.as_bytes().to_vec(),
    )
    .unwrap();
    observe_document_tasks(&document)
}

#[test]
fn license_and_notice_documents_remain_source_facts_without_task_declarations() {
    for filename in [
        "LICENSE",
        "LICENSE.txt",
        "LICENSE.md",
        "COPYING",
        "NOTICE",
        "legal/NOTICE.md",
    ] {
        let bytes = b"---\npipeline_tag: text-generation\n---\nLicense notice text.\n";
        let path = RepositoryPath::parse(filename).unwrap();
        let document =
            verify_source_document(&inventory(filename, bytes), &path, bytes.to_vec()).unwrap();
        assert_eq!(document.evidence().kind, SourceDocumentKind::LicenseNotice);
        let observation = observe_document_tasks(&document);
        assert!(observation.task_families.is_empty());
        assert!(observation.unrecognized_tags.is_empty());
    }
}

#[test]
fn text_documents_do_not_accept_binary_control_bytes_under_allowed_names() {
    for bytes in [
        b"MZ\0binary".as_slice(),
        b"PK\x03\x04archive".as_slice(),
        b"text\x1b[31mterminal".as_slice(),
    ] {
        let source = inventory("README.md", bytes);
        let error = verify_source_document(
            &source,
            &RepositoryPath::parse("README.md").unwrap(),
            bytes.to_vec(),
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ModelSourceError>().unwrap().code,
            LifecycleErrorCode::FormatForbidden
        );
    }
    assert!(verify_source_document(
        &inventory("NOTICE", b"line\nnext\tvalue\r\n"),
        &RepositoryPath::parse("NOTICE").unwrap(),
        b"line\nnext\tvalue\r\n".to_vec()
    )
    .is_ok());
}

#[test]
fn document_proof_binds_exact_snapshot_path_bytes_and_hash_without_debug_content() {
    let bytes = b"---\npipeline_tag: text-to-speech\n---\nprivate-card-body";
    let path = RepositoryPath::parse("README.md").unwrap();
    let source = inventory(path.as_str(), bytes);
    let document = verify_source_document(&source, &path, bytes.to_vec()).unwrap();
    assert_eq!(
        document.evidence().snapshot_digest,
        source.snapshot.content_digest().unwrap()
    );
    assert_eq!(
        document.evidence().computed_sha256,
        Sha256Digest::of_bytes(bytes)
    );
    assert_eq!(
        document.evidence().source_sha256,
        Some(Sha256Digest::of_bytes(bytes))
    );
    assert_eq!(document.evidence().path, path);
    assert_eq!(document.text().as_bytes(), bytes);
    assert!(!format!("{document:?}").contains("private-card-body"));
    let observed = observe_document_tasks(&document);
    assert_eq!(observed.scope, SourceTaskScope::Document);
    assert_eq!(
        observed.task_families,
        BTreeSet::from([ModelTaskFamily::Tts])
    );
}

#[test]
fn rejects_forbidden_missing_unknown_sized_and_excessive_source_documents() {
    for name in [
        "model.gguf",
        "script.py",
        "tokenizer.json",
        "README.exe",
        "configuration.yaml",
    ] {
        let source = inventory(name, b"data");
        assert!(source_document_size(&source, &RepositoryPath::parse(name).unwrap()).is_err());
    }
    let path = RepositoryPath::parse("config.json").unwrap();
    let source = inventory("README.md", b"data");
    assert!(source_document_size(&source, &path).is_err());
    for size in [None, Some(MAX_SOURCE_DOCUMENT_BYTES as u64 + 1)] {
        let mut source = inventory(path.as_str(), b"{}");
        source.files.get_mut(path.as_str()).unwrap().size_bytes = size;
        source.snapshot.files_digest = Sha256Digest::of_serializable(&source.files).unwrap();
        assert!(source_document_size(&source, &path).is_err());
    }
    let empty = observe("README.md", "");
    assert_eq!(empty.document.size_bytes, 0);
    assert!(empty.issues.is_empty() && empty.task_families.is_empty());
}

#[test]
fn refuses_wrong_length_hash_invalid_utf8_and_forged_inventory_but_preserves_unknown_hash() {
    let path = RepositoryPath::parse("config.json").unwrap();
    let source = inventory(path.as_str(), b"{}");
    assert!(verify_source_document(&source, &path, b"{ }".to_vec()).is_err());
    let error = verify_source_document(&source, &path, b"[]".to_vec()).unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::HashMismatch
    );
    let invalid = inventory(path.as_str(), &[0xff, 0xfe]);
    assert!(verify_source_document(&invalid, &path, vec![0xff, 0xfe]).is_err());
    let mut changed = source.clone();
    changed.files.get_mut(path.as_str()).unwrap().size_bytes = Some(3);
    assert!(verify_source_document(&changed, &path, b"{ }".to_vec()).is_err());
    let mut unknown = source;
    unknown.files.get_mut(path.as_str()).unwrap().sha256 = None;
    unknown.snapshot.files_digest = Sha256Digest::of_serializable(&unknown.files).unwrap();
    let document = verify_source_document(&unknown, &path, b"{}".to_vec()).unwrap();
    assert_eq!(document.evidence().source_sha256, None);
    assert_eq!(
        document.evidence().computed_sha256,
        Sha256Digest::of_bytes(b"{}")
    );
}

#[test]
fn explicit_tasks_remain_document_scoped_and_distinguish_speech_audio_and_visual_claims() {
    let text = json!({"task":["text-generation","image-text-to-text","automatic-speech-recognition",
        "text-to-speech","text-to-audio","text-embedding","text-reranking","text-to-image","text-to-video"],
        "architectures":["UntrustedName"],"model_type":"mtp-dflash-moe"}).to_string();
    let observed = observe("config.json", &text);
    assert!(observed.issues.is_empty());
    assert_eq!(observed.scope, SourceTaskScope::Document);
    assert_eq!(
        observed.task_families,
        BTreeSet::from([
            ModelTaskFamily::Language,
            ModelTaskFamily::VisionLanguage,
            ModelTaskFamily::Asr,
            ModelTaskFamily::Tts,
            ModelTaskFamily::Audio,
            ModelTaskFamily::Embedding,
            ModelTaskFamily::Reranking,
            ModelTaskFamily::ImageDiffusion,
            ModelTaskFamily::VideoDiffusion,
        ])
    );
    assert!(observe(
        "config.json",
        r#"{"model_type":"tts","architectures":["SpeechGenerator"]}"#
    )
    .task_families
    .is_empty());
    let ambiguous = observe(
        "config.json",
        r#"{"task":["feature-extraction","sentence-similarity","draft-model","dflash"]}"#,
    );
    assert!(ambiguous.task_families.is_empty());
    assert_eq!(ambiguous.unrecognized_tags.len(), 4);
}

#[test]
fn json_ambiguity_malformed_documents_and_wrong_task_values_never_produce_claims() {
    for text in [
        r#"{"task":"text-to-speech","task":"text-generation"}"#,
        r#"{"task":"text-generation","x":1,"x":2}"#,
        "[]",
        "{",
        r#"{"task":null}"#,
        r#"{"task":["text-generation",4]}"#,
        r#"{"task":""}"#,
    ] {
        let observed = observe("configuration.json", text);
        assert!(!observed.issues.is_empty(), "{text}");
        assert!(observed.task_families.is_empty(), "{text}");
    }
}

#[test]
fn cards_require_bounded_closed_root_frontmatter_and_ignore_body_or_nested_labels() {
    let observed = observe(
        "README.md",
        "\u{feff}---\r\npipeline_tag: 'text-to-speech'\r\n---\r\n# text-to-image\r\n",
    );
    assert!(observed.issues.is_empty());
    assert_eq!(
        observed.task_families,
        BTreeSet::from([ModelTaskFamily::Tts])
    );
    for text in [
        "# text-to-speech\npipeline_tag: text-to-speech",
        "---\nnested:\n  task: text-to-speech\n---\n",
    ] {
        assert!(observe("README.md", text).task_families.is_empty());
    }
    for text in [
        "---\npipeline_tag: text-to-speech",
        "---\ntask: text-to-speech\ntask: text-generation\n---",
        "---\ntask: !!python/object/apply:os.system\n---",
    ] {
        let observed = observe("README.md", text);
        assert!(observed.task_families.is_empty());
        assert!(!observed.issues.is_empty());
    }
    let index = observe("model-index.yaml", "---\ntask: \"text-to-video\"\n");
    assert_eq!(
        index.task_families,
        BTreeSet::from([ModelTaskFamily::VideoDiffusion])
    );
    assert!(observe(
        "model-index.yaml",
        "task: text-to-video\n---\ntask: text-generation"
    )
    .task_families
    .is_empty());
}

#[test]
fn declaration_count_string_and_frontmatter_limits_preserve_unknown_state() {
    for text in [
        json!({"task":vec!["text-generation";33]}).to_string(),
        json!({"task":"a".repeat(129)}).to_string(),
        json!({"task":"text-generation\n"}).to_string(),
    ] {
        let observed = observe("config.json", &text);
        assert!(observed.task_families.is_empty());
        assert!(!observed.issues.is_empty());
    }
    for text in [
        format!(
            "---\n{}task: text-generation\n---",
            "unused: x\n".repeat(257)
        ),
        format!(
            "---\nunused: {}\ntask: text-generation\n---",
            "x".repeat(16 * 1024)
        ),
    ] {
        let observed = observe("README.md", &text);
        assert!(observed.task_families.is_empty());
        assert!(observed
            .issues
            .contains(&TaskDeclarationIssue::DeclarationLimitExceeded));
    }
}

#[tokio::test]
#[ignore = "requires real pinned ModelScope inventory and model-card HTTP responses"]
async fn real_modelscope_card_hash_and_explicit_tts_claim_are_document_scoped() {
    let provider = ModelScopeProvider::new().unwrap();
    let revision = "e17251825ca43b1dfff055a46c82ebed61ba024e";
    let source = provider
        .inspect_at(
            ModelScopeRepository::parse("OuteAI/Llama-OuteTTS-1.0-1B-GGUF").unwrap(),
            revision.into(),
            revision.into(),
            chrono::Utc::now().timestamp(),
            None,
        )
        .await
        .unwrap();
    let document = provider
        .read_source_document(&source, &RepositoryPath::parse("README.md").unwrap(), None)
        .await
        .unwrap();
    let observed = observe_document_tasks(&document);
    assert!(observed.issues.is_empty());
    assert_eq!(observed.scope, SourceTaskScope::Document);
    assert_eq!(
        observed.task_families,
        BTreeSet::from([ModelTaskFamily::Tts])
    );
    println!("{}", serde_json::to_string(&observed).unwrap());
}
