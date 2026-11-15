use super::*;

fn project_scope(
    record: &GenerationResultRecord,
    inputs: Vec<NativeTaskInputReference>,
) -> ProjectTaskCompilationScope {
    ProjectTaskCompilationScope::new(
        "session-b".into(),
        "project-child".into(),
        record.snapshot.session_id.clone(),
        record.snapshot.job_id.clone(),
        record.snapshot_digest.clone(),
        inputs,
    )
    .unwrap()
}

#[test]
fn project_generation_preserves_model_specific_parameters_across_profiles() {
    for profile in [
        GenerationProfile::Sd15CpuTextToImage,
        GenerationProfile::H3CudaTextToVideoAudio,
        GenerationProfile::H3CudaFirstFrameToVideoAudio,
        GenerationProfile::installation_profile("wan21-13b-cpu").unwrap(),
    ] {
        let service = service(profile);
        let mut params = if profile.intent() == NativeTaskIntent::TextToImage {
            image()
        } else {
            video()
        };
        let refs = if profile == GenerationProfile::H3CudaFirstFrameToVideoAudio {
            if let GenerationParameters::Video(v) = &mut params {
                v.first_frame = Some(input());
            }
            vec![NativeTaskInputReference::Image(input())]
        } else {
            vec![]
        };
        let task = service.compile(request(params)).unwrap();
        let record = record(&task);
        let before = serde_json::to_value(&record).unwrap();
        let scoped = project_scope(&record, refs);
        let patch = if profile.intent() == NativeTaskIntent::TextToImage {
            GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("A blue vase on a table".into()),
                ..Default::default()
            })
        } else {
            GenerationParameters::Video(H3VideoParameters {
                prompt: Some("The apple rolls left".into()),
                ..Default::default()
            })
        };
        assert!(service
            .reuse_result("session-b", &record, RESULT, "project-child", patch.clone())
            .is_err());
        let child = service
            .reuse_result_with_project_scope(
                "session-b",
                &record,
                RESULT,
                "project-child",
                patch,
                &scoped,
            )
            .unwrap();
        assert_eq!(child.snapshot().session_id, "session-b");
        assert_eq!(child.snapshot().job_id, "project-child");
        assert_eq!(child.snapshot().profile, profile);
        assert_eq!(child.snapshot().binding, record.snapshot.binding);
        assert_eq!(
            child.snapshot().descriptor_digest,
            record.snapshot.descriptor_digest
        );
        assert_eq!(child.snapshot().engine, record.snapshot.engine);
        assert_eq!(
            child.snapshot().parent.as_ref().unwrap().snapshot_digest,
            record.snapshot_digest
        );
        assert_eq!(child.snapshot().changes.len(), 1);
        assert!(child.snapshot().changes.contains_key("prompt"));
        assert_eq!(
            child.snapshot().origins["seed"],
            GenerationParameterOrigin::Parent {
                result_id: RESULT.into()
            }
        );
        let old = serde_json::to_value(&record.snapshot.parameters).unwrap();
        let new = serde_json::to_value(&child.snapshot().parameters).unwrap();
        assert_eq!(old["parameters"]["seed"], new["parameters"]["seed"]);
        assert_eq!(
            old["parameters"]["firstFrame"],
            new["parameters"]["firstFrame"]
        );
        assert_eq!(serde_json::to_value(&record).unwrap(), before);
    }
}

#[test]
fn project_generation_compilation_requires_exact_input_and_child_identity() {
    let service = service(GenerationProfile::H3CudaFirstFrameToVideoAudio);
    let params = GenerationParameters::Video(H3VideoParameters {
        prompt: Some(H3_PROMPT.into()),
        first_frame: Some(input()),
        ..Default::default()
    });
    let task = service.compile(request(params.clone())).unwrap();
    let record = record(&task);
    let scoped = project_scope(&record, vec![NativeTaskInputReference::Image(input())]);
    let mut req = GenerationTaskRequest {
        session_id: "session-b".into(),
        job_id: "project-child".into(),
        parameters: params,
    };
    assert!(service.compile(req.clone()).is_err());
    assert!(service
        .compile_json(serde_json::to_value(&req).unwrap())
        .is_err());
    service
        .compile_with_project_scope(req.clone(), &scoped)
        .unwrap();
    req.job_id = "different-child".into();
    assert!(service
        .compile_with_project_scope(req.clone(), &scoped)
        .is_err());
    req.job_id = "project-child".into();
    if let GenerationParameters::Video(v) = &mut req.parameters {
        v.first_frame.as_mut().unwrap().width += 1;
    }
    assert!(service.compile_with_project_scope(req, &scoped).is_err());
    let empty = project_scope(&record, vec![]);
    assert!(service
        .reuse_result_with_project_scope(
            "session-b",
            &record,
            RESULT,
            "project-child",
            video(),
            &empty
        )
        .is_err());
}

#[test]
fn project_generation_rejects_wrong_parent_digest_identity_and_bound_model() {
    let service = service(GenerationProfile::Sd15CpuTextToImage);
    let task = service.compile(request(image())).unwrap();
    let original = record(&task);
    let scoped = project_scope(&original, vec![]);
    assert!(service
        .reuse_result_with_project_scope(
            "wrong-child",
            &original,
            RESULT,
            "project-child",
            image(),
            &scoped
        )
        .is_err());
    assert!(service
        .reuse_result_with_project_scope(
            "session-b",
            &original,
            INPUT,
            "project-child",
            image(),
            &scoped
        )
        .is_err());
    for field in ["sessionId", "jobId", "descriptorDigest"] {
        let mut encoded = serde_json::to_value(&original).unwrap();
        encoded["snapshot"][field] = if field == "descriptorDigest" {
            json!(Sha256Digest::of_bytes(b"changed descriptor"))
        } else {
            json!("changed")
        };
        let changed: GenerationResultRecord = serde_json::from_value(encoded).unwrap();
        assert!(service
            .reuse_result_with_project_scope(
                "session-b",
                &changed,
                RESULT,
                "project-child",
                image(),
                &scoped
            )
            .is_err());
    }
    let wrong = ProjectTaskCompilationScope::new(
        "session-b".into(),
        "project-child".into(),
        "session-a".into(),
        "job-1".into(),
        Sha256Digest::of_bytes(b"wrong snapshot"),
        vec![],
    )
    .unwrap();
    assert!(service
        .reuse_result_with_project_scope(
            "session-b",
            &original,
            RESULT,
            "project-child",
            image(),
            &wrong
        )
        .is_err());
    let mut changed: GenerationResultRecord =
        serde_json::from_value(serde_json::to_value(&original).unwrap()).unwrap();
    changed.snapshot.binding.version_id = "wrong version".into();
    changed.snapshot_digest = changed.snapshot.digest().unwrap();
    let forged_scope = project_scope(&changed, vec![]);
    assert!(service
        .reuse_result_with_project_scope(
            "session-b",
            &changed,
            RESULT,
            "project-child",
            image(),
            &forged_scope
        )
        .is_err());
}

#[test]
fn project_generation_can_reuse_a_cross_session_parent_without_rewriting_inputs() {
    let service = service(GenerationProfile::H3CudaFirstFrameToVideoAudio);
    let params = GenerationParameters::Video(H3VideoParameters {
        prompt: Some(H3_PROMPT.into()),
        first_frame: Some(input()),
        ..Default::default()
    });
    let task = service.compile(request(params)).unwrap();
    let original = record(&task);
    let scoped = project_scope(&original, vec![NativeTaskInputReference::Image(input())]);
    let child = service
        .reuse_result_with_project_scope(
            "session-b",
            &original,
            RESULT,
            "project-child",
            video(),
            &scoped,
        )
        .unwrap();
    let next_record = GenerationResultRecord {
        snapshot: child.snapshot().clone(),
        snapshot_digest: child.snapshot().digest().unwrap(),
        outputs: vec![GenerationOutputReference {
            session_id: "session-b".into(),
            result_id: RESULT.into(),
            sha256: Sha256Digest::of_bytes(b"second output"),
        }],
    };
    let before = serde_json::to_value(&next_record).unwrap();
    let next_scope = ProjectTaskCompilationScope::new(
        "session-c".into(),
        "third".into(),
        "session-b".into(),
        "project-child".into(),
        next_record.snapshot_digest.clone(),
        vec![NativeTaskInputReference::Image(input())],
    )
    .unwrap();
    let third = service
        .reuse_result_with_project_scope(
            "session-c",
            &next_record,
            RESULT,
            "third",
            video(),
            &next_scope,
        )
        .unwrap();
    assert_eq!(third.snapshot().session_id, "session-c");
    let plan: NativeTaskPlan = third.into();
    assert_eq!(
        plan.input_references(),
        vec![NativeTaskInputReference::Image(input())]
    );
    assert_eq!(serde_json::to_value(&next_record).unwrap(), before);
}
