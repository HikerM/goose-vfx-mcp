use super::*;

#[test]
fn changing_only_tts_text_reuses_generated_seed_voice_and_native_options() {
    let service = tts_service();
    let mut request = tts();
    request.seed = None;
    request.temperature = Some(0.6);
    request.max_frames = Some(124);
    let first = service.compile(request).unwrap();
    let parent = first.snapshot();
    let original = serde_json::to_string(parent).unwrap();
    assert!(!original.contains("derivation"));
    assert_eq!(
        parent.digest().unwrap(),
        Sha256Digest::of_bytes(original.as_bytes())
    );
    let restored: PocketTtsSnapshot = serde_json::from_str(&original).unwrap();
    let child = service
        .reuse_configuration(
            "session-a",
            &restored,
            &parent.digest().unwrap(),
            "second",
            PocketTtsPatch::Text("Good morning.".into()),
        )
        .unwrap();
    let lineage = child.snapshot().metadata.derivation.as_ref().unwrap();
    assert_eq!(lineage.parent_job_id, "job-tts");
    assert_eq!(lineage.parent_snapshot_digest, parent.digest().unwrap());
    let change = lineage.change.as_ref().unwrap();
    assert_eq!(change.field, "text");
    assert_eq!(change.before, json!(parent.parameters.text));
    assert_eq!(change.after, json!("Good morning."));
    let mut expected = parent.parameters.clone();
    expected.text = "Good morning.".into();
    assert_eq!(child.snapshot().parameters, expected);
    assert_eq!(flag(child.arguments(), "--text"), &string("Good morning."));
    assert_eq!(
        flag(child.arguments(), "--seed"),
        flag(first.arguments(), "--seed")
    );
    assert_eq!(
        flag(child.arguments(), "--voice-ref"),
        flag(first.arguments(), "--voice-ref")
    );
    service.validate_parent(child.snapshot(), parent).unwrap();

    let replacement_seed = expected.seed.wrapping_add(1);
    let third = service
        .reuse_configuration(
            "session-a",
            child.snapshot(),
            &child.snapshot().digest().unwrap(),
            "third",
            PocketTtsPatch::Seed(replacement_seed),
        )
        .unwrap();
    assert_eq!(
        flag(third.arguments(), "--seed"),
        &string(&replacement_seed.to_string())
    );
    let reloaded: PocketTtsSnapshot =
        serde_json::from_str(&serde_json::to_string(third.snapshot()).unwrap()).unwrap();
    service
        .validate_parent(&reloaded, child.snapshot())
        .unwrap();
    assert!(service.validate_parent(&reloaded, parent).is_err());
}

#[test]
fn asr_language_patch_preserves_context_input_and_native_chunking() {
    let service = asr_service();
    let mut request = asr();
    request.context = Some("Names: Willow and Jasper".into());
    request.chunking = Some(Qwen3AsrChunking::WholeFile {});
    request.max_tokens = Some(96);
    let first = service.compile(request).unwrap();
    let parent = first.snapshot();
    let patch: Qwen3AsrPatch = serde_json::from_value(json!({"language":"chinese"})).unwrap();
    let second = service
        .reuse_configuration(
            "session-a",
            parent,
            &parent.digest().unwrap(),
            "second",
            patch,
        )
        .unwrap();
    let mut expected = parent.parameters.clone();
    expected.language = Qwen3AsrLanguage::Chinese;
    assert_eq!(second.snapshot().parameters, expected);
    assert_eq!(flag(second.arguments(), "--language"), &string("Chinese"));
    assert_eq!(
        flag(second.arguments(), "--text"),
        &string("Names: Willow and Jasper")
    );
    assert_eq!(
        flag(second.arguments(), "--audio-chunk-mode"),
        &string("none")
    );
    assert_eq!(flag(second.arguments(), "--max-tokens"), &string("96"));
    let change = second
        .snapshot()
        .metadata
        .derivation
        .as_ref()
        .unwrap()
        .change
        .as_ref()
        .unwrap();
    assert_eq!(change.field, "language");
    assert_eq!(
        (&change.before, &change.after),
        (&json!("auto"), &json!("chinese"))
    );
    service.validate_parent(second.snapshot(), parent).unwrap();
    let no_change = service
        .reuse_configuration(
            "session-a",
            second.snapshot(),
            &second.snapshot().digest().unwrap(),
            "third",
            Qwen3AsrPatch::MaxTokens(96),
        )
        .unwrap();
    assert!(no_change
        .snapshot()
        .metadata
        .derivation
        .as_ref()
        .unwrap()
        .change
        .is_none());
    service
        .validate_parent(no_change.snapshot(), second.snapshot())
        .unwrap();
}

#[test]
fn ignores_no_unknown_patch_and_rejects_foreign_session_job_identity_and_digest() {
    for value in [
        json!({"speed":1}),
        json!({"text":null}),
        json!({"text":"hello","seed":1}),
        json!({}),
    ] {
        assert!(serde_json::from_value::<PocketTtsPatch>(value).is_err());
    }
    assert!(serde_json::from_value::<Qwen3AsrPatch>(json!({"temperature":0.5})).is_err());
    let service = tts_service();
    let first = service.compile(tts()).unwrap();
    let parent = first.snapshot();
    let hash = parent.digest().unwrap();
    let apply = |session, job, hash| {
        service.reuse_configuration(session, parent, hash, job, PocketTtsPatch::Seed(1))
    };
    assert!(apply("session-b", "second", &hash).is_err());
    assert!(apply("session-a", "job-tts", &hash).is_err());
    assert!(apply("session-a", "second", &Sha256Digest::of_bytes(b"wrong")).is_err());
    let mut binding = binding(AudioTaskProfile::PocketTtsEnglishCpu);
    binding.version_id = "test-v2".into();
    assert!(PocketTtsParameterService::new(binding)
        .unwrap()
        .reuse_configuration(
            "session-a",
            parent,
            &hash,
            "second",
            PocketTtsPatch::Seed(1)
        )
        .is_err());
    let mut forged = parent.clone();
    forged.metadata.cpu_threads += 1;
    assert!(service
        .reuse_configuration(
            "session-a",
            &forged,
            &forged.digest().unwrap(),
            "second",
            PocketTtsPatch::Seed(1)
        )
        .is_err());
    let mut bad_voice = input();
    bad_voice.session_id = "different-session".into();
    assert!(service
        .reuse_configuration(
            "session-a",
            parent,
            &hash,
            "second",
            PocketTtsPatch::VoiceReference(bad_voice)
        )
        .is_err());
    assert!(service
        .reuse_configuration(
            "session-a",
            parent,
            &hash,
            "second",
            PocketTtsPatch::Temperature(f32::NAN)
        )
        .is_err());
    let asr = asr_service().compile(asr()).unwrap();
    assert!(asr_service()
        .reuse_configuration(
            "session-a",
            asr.snapshot(),
            &asr.snapshot().digest().unwrap(),
            "second",
            Qwen3AsrPatch::Chunking(Qwen3AsrChunking::Fixed {
                seconds: f32::INFINITY
            })
        )
        .is_err());
}

#[test]
fn persistent_validation_rejects_changed_or_omitted_parent_values_and_fixed_flags() {
    let service = asr_service();
    let first = service.compile(asr()).unwrap();
    let parent = first.snapshot();
    let second = service
        .reuse_configuration(
            "session-a",
            parent,
            &parent.digest().unwrap(),
            "second",
            Qwen3AsrPatch::MaxTokens(64),
        )
        .unwrap();
    let mut forged = second.snapshot().clone();
    forged
        .metadata
        .derivation
        .as_mut()
        .unwrap()
        .change
        .as_mut()
        .unwrap()
        .after = json!(321);
    assert!(service.validate_snapshot(&forged).is_err());
    let mut forged = second.snapshot().clone();
    forged
        .metadata
        .derivation
        .as_mut()
        .unwrap()
        .change
        .as_mut()
        .unwrap()
        .before = json!(128);
    assert!(service.validate_parent(&forged, parent).is_err());
    forged.metadata.derivation.as_mut().unwrap().change = None;
    assert!(service.validate_parent(&forged, parent).is_err());
    let mut forged = second.snapshot().clone();
    forged.parameters.return_timestamps = true;
    assert!(service.validate_snapshot(&forged).is_err());
}
