use super::*;
use crate::lifecycle::{NativeChatUsage, VectorRuntimeProfile};

fn response(content: &str) -> NativeChatResponse {
    NativeChatResponse {
        id: "test-turn".into(),
        content: Some(content.into()),
        tool_calls: vec![],
        finish_reason: NativeChatFinishReason::Stop,
        usage: NativeChatUsage {
            prompt_tokens: 5,
            completion_tokens: 1,
            total_tokens: 6,
        },
        launch_digest: Sha256Digest::of_bytes(b"test launch"),
        request_digest: Sha256Digest::of_bytes(b"test input"),
        image_digests: vec![],
    }
}

#[test]
fn frozen_corpus_has_unambiguous_answers_and_does_not_repeat_memory_targets_in_recall_questions() {
    let suite = Suite::load().unwrap();
    assert_eq!(
        input_digest().as_str(),
        "8511eded1fb7b3db55d23dd4efbbd5c183b842cf8e9b36ce5024d1610c270844"
    );
    for task in suite
        .cases
        .iter()
        .map(|c| &c.task)
        .chain(suite.dialogs.iter().flat_map(|d| &d.turns))
    {
        assert!(!task.prompt.trim().is_empty() && !task.answers.is_empty());
        assert!(task.answers.iter().all(|a| !normalized(a).is_empty()));
    }
    for dialog in &suite.dialogs {
        let recall = dialog.turns.last().unwrap();
        let answer = &recall.answers[0];
        assert!(!normalized(&recall.prompt).contains(&normalized(answer)));
        assert!(dialog
            .turns
            .iter()
            .take(dialog.turns.len() - 1)
            .any(|turn| normalized(&turn.prompt).contains(&normalized(answer))));
    }
}

#[test]
fn content_acceptance_rejects_echoes_contradictions_and_truncation() {
    let suite = Suite::load().unwrap();
    let task = &suite.cases[0].task;
    assert!(task.accepts(&response(" 41. ")));
    for text in [
        "",
        "141",
        "the answer is 41 or 40",
        "41 is wrong",
        &task.prompt,
    ] {
        assert!(!task.accepts(&response(text)));
    }
    let mut cut = response("41");
    cut.finish_reason = NativeChatFinishReason::Length;
    assert!(!task.accepts(&cut));
}

#[test]
fn followup_preserves_actual_assistant_output_even_when_wrong_and_omits_expected_answer() {
    let suite = Suite::load().unwrap();
    let dialog = &suite.dialogs[0];
    let previous = ObservedTurn {
        request: NativeChatRequest::text(&dialog.turns[0].prompt, 64),
        response: response("Unexpected real reply"),
        passed: false,
    };
    let request = next_turn(&previous, &dialog.turns[1].prompt).unwrap();
    let [NativeChatMessage::User { .. }, NativeChatMessage::Assistant {
        content,
        tool_calls,
    }, NativeChatMessage::User { content: question }] = request.messages.as_slice()
    else {
        panic!("missing transcript")
    };
    assert_eq!(content.as_deref(), Some("Unexpected real reply"));
    assert!(tool_calls.is_empty());
    assert!(
        matches!(question.as_slice(),[NativeChatContent::Text{text}] if text==&dialog.turns[1].prompt)
    );
    assert_eq!(previous.request.messages.len(), 1);
    assert_eq!(request.max_output_tokens, 64);
}

#[test]
fn all_memory_groups_resource_completion_and_actual_length_limit_are_mandatory() {
    let suite = Suite::load().unwrap();
    assert!(passed(8, 10, 3, true, false, &suite));
    assert!(!passed(7, 10, 3, true, false, &suite));
    assert!(!passed(10, 10, 2, true, false, &suite));
    assert!(!passed(9, 9, 3, true, false, &suite));
    assert!(!passed(10, 10, 3, false, false, &suite));
    assert!(!passed(10, 10, 3, true, true, &suite));
    let mut reply = response("One");
    assert!(!limit_passed(&reply));
    reply.finish_reason = NativeChatFinishReason::Length;
    assert!(limit_passed(&reply));
    reply.usage.completion_tokens = 2;
    assert!(!limit_passed(&reply));
}

#[test]
fn vector_evidence_profile_keeps_its_existing_wire_format_and_separate_namespace() {
    let vector = ValidationProfile::Vector(VectorRuntimeProfile::Qwen3Embedding06bQ8);
    let text = ValidationProfile::Text(ModelStructure::Moe);
    assert_ne!(text.directory(), vector.directory());
    let existing = serde_json::to_value(VectorRuntimeProfile::Qwen3Embedding06bQ8).unwrap();
    assert_eq!(serde_json::to_value(vector).unwrap(), existing);
    assert!(serde_json::from_value::<ValidationProfile>(existing).unwrap() == vector);
    assert!(serde_json::from_str::<ValidationProfile>("\"moe\"").unwrap() == text);
}

#[test]
fn recording_failed_text_proof_retains_previous_vector_proof_in_sqlite() {
    use crate::lifecycle::{baseline_pairing_runtime, RuntimeExecutionMode};
    use std::collections::BTreeMap;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/managed-validation-tests");
    std::fs::create_dir_all(&root).unwrap();
    let temp = tempfile::tempdir_in(root.canonicalize().unwrap()).unwrap();
    let store = InstallationStore::open(temp.path()).unwrap();
    let db = store.root().join("models.db");
    let repository = LifecycleRepository::open(&db).unwrap();
    let vector = VectorRuntimeProfile::Qwen3Embedding06bQ8;
    let mut launch = LaunchConfig {
        installation_id: "synthetic-test".into(),
        version_id: "tests".into(),
        manifest_digest: Sha256Digest::of_bytes(b"test manifest"),
        runtime: baseline_pairing_runtime().unwrap(),
        capabilities: BTreeSet::from([vector.capability()]),
        component_files: BTreeMap::new(),
        parameters: vector.launch_parameters(),
        mode: RuntimeExecutionMode::Helper { port: 34567 },
    };
    let mut rows = Vec::new();
    for profile in [
        ValidationProfile::Vector(vector),
        ValidationProfile::Text(ModelStructure::Dense),
    ] {
        let binding = ReportBinding::new(
            &launch,
            profile,
            input_digest(),
            Sha256Digest::of_bytes(b"test location"),
            CapabilityStatus::Failed,
        )
        .unwrap();
        let row = binding
            .register(
                &repository,
                &store,
                &serde_json::json!({"binding":binding,"failure":"fixture, no inference performed"}),
            )
            .unwrap();
        assert!(row.evidence_path.starts_with(profile.directory()));
        rows.push(row);
        launch.capabilities = BTreeSet::from([ModelCapability::Text]);
    }
    drop(repository);
    assert_eq!(rows[0].capability, ModelCapability::Embedding);
    assert_eq!(rows[1].capability, ModelCapability::Text);
    assert_ne!(
        rows[0].identity.suite_version,
        rows[1].identity.suite_version
    );
    assert_eq!(
        LifecycleRepository::open(&db)
            .unwrap()
            .evidence(&launch.installation_id)
            .unwrap(),
        rows
    );
}
