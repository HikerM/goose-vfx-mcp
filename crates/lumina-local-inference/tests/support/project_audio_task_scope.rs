use super::*;

fn scope(
    metadata: &AudioTaskSnapshotMetadata,
    digest: Sha256Digest,
    inputs: Vec<NativeTaskInputReference>,
) -> ProjectTaskCompilationScope {
    ProjectTaskCompilationScope::new(
        "session-b".into(),
        "project-child".into(),
        metadata.session_id.clone(),
        metadata.job_id.clone(),
        digest,
        inputs,
    )
    .unwrap()
}

#[test]
fn project_tts_reuses_original_voice_and_seed_without_rewriting_parent() {
    let service = tts_service();
    let task = service.compile(tts()).unwrap();
    let parent = task.snapshot();
    let before = serde_json::to_value(parent).unwrap();
    let scoped = scope(
        &parent.metadata,
        parent.digest().unwrap(),
        vec![NativeTaskInputReference::Audio(input())],
    );
    assert!(service
        .reuse_configuration(
            "session-b",
            parent,
            &parent.digest().unwrap(),
            "project-child",
            PocketTtsPatch::Text("Hello project".into())
        )
        .is_err());
    let child = service
        .reuse_configuration_with_project_scope(
            "session-b",
            parent,
            &parent.digest().unwrap(),
            "project-child",
            PocketTtsPatch::Text("Hello project".into()),
            &scoped,
        )
        .unwrap();
    assert_eq!(child.snapshot().metadata.session_id, "session-b");
    assert_eq!(child.snapshot().parameters.voice_reference, input());
    assert_eq!(child.snapshot().parameters.seed, parent.parameters.seed);
    let mut expected = parent.parameters.clone();
    expected.text = "Hello project".into();
    assert_eq!(child.snapshot().parameters, expected);
    service
        .validate_snapshot_with_project_scope(child.snapshot(), &scoped)
        .unwrap();
    service
        .validate_parent_with_project_scope(child.snapshot(), parent, &scoped)
        .unwrap();
    assert!(service.validate_snapshot(child.snapshot()).is_err());
    assert!(service.validate_parent(child.snapshot(), parent).is_err());
    assert_eq!(serde_json::to_value(parent).unwrap(), before);
    let mut request = tts();
    request.session_id = "session-b".into();
    request.job_id = "project-child".into();
    assert!(service.compile(request.clone()).is_err());
    assert!(service
        .compile_json(serde_json::to_value(&request).unwrap())
        .is_err());
    service
        .compile_with_project_scope(request.clone(), &scoped)
        .unwrap();
    request.voice_reference.sha256 = Sha256Digest::of_bytes(b"different input");
    assert!(service
        .compile_with_project_scope(request, &scoped)
        .is_err());
    let mut forged = child.snapshot().clone();
    forged
        .metadata
        .derivation
        .as_mut()
        .unwrap()
        .parent_snapshot_digest = Sha256Digest::of_bytes(b"wrong parent");
    assert!(service
        .validate_snapshot_with_project_scope(&forged, &scoped)
        .is_err());
}

#[test]
fn project_asr_replacement_chain_requires_complete_historical_input_grants() {
    let service = asr_service();
    let task = service.compile(asr()).unwrap();
    let parent = task.snapshot();
    let before = serde_json::to_value(parent).unwrap();
    let mut replacement = input();
    replacement.session_id = "replacement-session".into();
    replacement.result_id = "019a1111-2222-8333-8444-666666666666".into();
    replacement.sha256 = Sha256Digest::of_bytes(b"replacement wav");
    let refs = vec![
        NativeTaskInputReference::Audio(input()),
        NativeTaskInputReference::Audio(replacement.clone()),
    ];
    let scoped = scope(&parent.metadata, parent.digest().unwrap(), refs.clone());
    let child = service
        .reuse_configuration_with_project_scope(
            "session-b",
            parent,
            &parent.digest().unwrap(),
            "project-child",
            Qwen3AsrPatch::Audio(replacement.clone()),
            &scoped,
        )
        .unwrap();
    assert_eq!(child.snapshot().parameters.audio, replacement);
    let mut expected = parent.parameters.clone();
    expected.audio = replacement.clone();
    assert_eq!(child.snapshot().parameters, expected);
    service
        .validate_parent_with_project_scope(child.snapshot(), parent, &scoped)
        .unwrap();
    assert_eq!(serde_json::to_value(parent).unwrap(), before);
    let next_scope = ProjectTaskCompilationScope::new(
        "session-c".into(),
        "third".into(),
        "session-b".into(),
        "project-child".into(),
        child.snapshot().digest().unwrap(),
        refs,
    )
    .unwrap();
    let third = service
        .reuse_configuration_with_project_scope(
            "session-c",
            child.snapshot(),
            &child.snapshot().digest().unwrap(),
            "third",
            Qwen3AsrPatch::Language(Qwen3AsrLanguage::Chinese),
            &next_scope,
        )
        .unwrap();
    service
        .validate_parent_with_project_scope(third.snapshot(), child.snapshot(), &next_scope)
        .unwrap();
    let incomplete = ProjectTaskCompilationScope::new(
        "session-c".into(),
        "third".into(),
        "session-b".into(),
        "project-child".into(),
        child.snapshot().digest().unwrap(),
        vec![NativeTaskInputReference::Audio(replacement)],
    )
    .unwrap();
    assert!(service
        .reuse_configuration_with_project_scope(
            "session-c",
            child.snapshot(),
            &child.snapshot().digest().unwrap(),
            "third",
            Qwen3AsrPatch::Language(Qwen3AsrLanguage::Chinese),
            &incomplete
        )
        .is_err());
    let mut request = asr();
    request.session_id = "session-b".into();
    request.job_id = "project-child".into();
    assert!(service
        .compile_json(serde_json::to_value(&request).unwrap())
        .is_err());
    service
        .compile_with_project_scope(request, &scoped)
        .unwrap();
    assert!(service
        .reuse_configuration(
            "session-b",
            parent,
            &parent.digest().unwrap(),
            "project-child",
            Qwen3AsrPatch::Context("project".into())
        )
        .is_err());
}

#[test]
fn project_audio_rejects_wrong_identity_digest_binding_and_partial_input_matches() {
    let service = asr_service();
    let task = service.compile(asr()).unwrap();
    let parent = task.snapshot();
    for scoped in [
        scope(
            &parent.metadata,
            Sha256Digest::of_bytes(b"wrong digest"),
            vec![NativeTaskInputReference::Audio(input())],
        ),
        scope(&parent.metadata, parent.digest().unwrap(), vec![]),
        ProjectTaskCompilationScope::new(
            "wrong-session".into(),
            "project-child".into(),
            parent.metadata.session_id.clone(),
            parent.metadata.job_id.clone(),
            parent.digest().unwrap(),
            vec![NativeTaskInputReference::Audio(input())],
        )
        .unwrap(),
        ProjectTaskCompilationScope::new(
            "session-b".into(),
            "project-child".into(),
            "wrong-parent".into(),
            parent.metadata.job_id.clone(),
            parent.digest().unwrap(),
            vec![NativeTaskInputReference::Audio(input())],
        )
        .unwrap(),
    ] {
        assert!(service
            .reuse_configuration_with_project_scope(
                "session-b",
                parent,
                &parent.digest().unwrap(),
                "project-child",
                Qwen3AsrPatch::Context("project".into()),
                &scoped
            )
            .is_err());
    }
    let scoped = scope(
        &parent.metadata,
        parent.digest().unwrap(),
        vec![NativeTaskInputReference::Audio(input())],
    );
    assert!(service
        .reuse_configuration_with_project_scope(
            "session-b",
            parent,
            &Sha256Digest::of_bytes(b"different expected"),
            "project-child",
            Qwen3AsrPatch::Context("project".into()),
            &scoped
        )
        .is_err());
    let mut request = asr();
    request.session_id = "session-b".into();
    request.job_id = "wrong-job".into();
    assert!(service
        .compile_with_project_scope(request.clone(), &scoped)
        .is_err());
    request.job_id = "project-child".into();
    request.audio.sample_frames += 1;
    assert!(service
        .compile_with_project_scope(request, &scoped)
        .is_err());
    let child = service
        .reuse_configuration_with_project_scope(
            "session-b",
            parent,
            &parent.digest().unwrap(),
            "project-child",
            Qwen3AsrPatch::Context("project".into()),
            &scoped,
        )
        .unwrap();
    let mut forged = child.snapshot().clone();
    forged.metadata.binding.version_id = "different model".into();
    assert!(service
        .validate_parent_with_project_scope(&forged, parent, &scoped)
        .is_err());
    let mut forged = child.snapshot().clone();
    forged
        .metadata
        .derivation
        .as_mut()
        .unwrap()
        .change
        .as_mut()
        .unwrap()
        .before = json!("not parent's context");
    assert!(service
        .validate_parent_with_project_scope(&forged, parent, &scoped)
        .is_err());
}

#[test]
fn project_music_without_inputs_still_binds_parent_and_preserves_seed() {
    let service =
        StableAudioMusicParameterService::new(binding(AudioTaskProfile::StableAudioSmallMusicCpu))
            .unwrap();
    let task = service
        .compile_json(
            json!({"sessionId":"session-a","jobId":"music-parent","prompt":"Soft piano","seed":42}),
        )
        .unwrap();
    let parent = task.snapshot();
    let before = serde_json::to_value(parent).unwrap();
    let scoped = scope(&parent.metadata, parent.digest().unwrap(), vec![]);
    assert!(service
        .reuse_configuration(
            "session-b",
            parent,
            &parent.digest().unwrap(),
            "project-child",
            StableAudioMusicPatch::Prompt("Warm piano".into())
        )
        .is_err());
    let child = service
        .reuse_configuration_with_project_scope(
            "session-b",
            parent,
            &parent.digest().unwrap(),
            "project-child",
            StableAudioMusicPatch::Prompt("Warm piano".into()),
            &scoped,
        )
        .unwrap();
    let mut expected = parent.parameters.clone();
    expected.prompt = "Warm piano".into();
    assert_eq!(child.snapshot().parameters, expected);
    service
        .validate_parent_with_project_scope(child.snapshot(), parent, &scoped)
        .unwrap();
    service
        .validate_snapshot_with_project_scope(child.snapshot(), &scoped)
        .unwrap();
    assert!(service.validate_parent(child.snapshot(), parent).is_err());
    assert_eq!(serde_json::to_value(parent).unwrap(), before);
    let wrong = scope(
        &parent.metadata,
        Sha256Digest::of_bytes(b"wrong parent"),
        vec![],
    );
    assert!(service
        .reuse_configuration_with_project_scope(
            "session-b",
            parent,
            &parent.digest().unwrap(),
            "project-child",
            StableAudioMusicPatch::Seed(43),
            &wrong
        )
        .is_err());
    let req = serde_json::from_value(
        json!({"sessionId":"session-b","jobId":"project-child","prompt":"Warm piano","seed":42}),
    )
    .unwrap();
    service.compile_with_project_scope(req, &scoped).unwrap();
    let req = serde_json::from_value(
        json!({"sessionId":"session-b","jobId":"wrong-child","prompt":"Warm piano","seed":42}),
    )
    .unwrap();
    assert!(service.compile_with_project_scope(req, &scoped).is_err());
}

#[test]
fn project_scope_rejects_invalid_identities_and_conflicting_asset_metadata() {
    let make = |child: &str, job: &str, refs| {
        ProjectTaskCompilationScope::new(
            child.into(),
            job.into(),
            "session-a".into(),
            "parent".into(),
            Sha256Digest::of_bytes(b"snapshot"),
            refs,
        )
    };
    assert!(make("", "child", vec![]).is_err());
    assert!(make("session\n", "child", vec![]).is_err());
    assert!(make("session", "parent", vec![]).is_err());
    let original = input();
    let mut conflicting = original.clone();
    conflicting.channels = 2;
    assert!(make(
        "session",
        "child",
        vec![
            NativeTaskInputReference::Audio(original),
            NativeTaskInputReference::Audio(conflicting)
        ]
    )
    .is_err());
}
