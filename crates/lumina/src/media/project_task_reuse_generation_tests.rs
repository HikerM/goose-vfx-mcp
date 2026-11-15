use super::*;

fn generation_service() -> GenerationParameterService {
    let profile = GenerationProfile::Sd15CpuTextToImage;
    GenerationParameterService::new(
        profile,
        GenerationModelBinding {
            installation_id: "test-installation".into(),
            version_id: "test-version".into(),
            manifest_digest: Sha256Digest::of_bytes(b"synthetic manifest; no model execution"),
            runtime: profile.runtime_build(),
            target_capabilities: profile.required_capabilities(),
            assets: profile
                .recorded_assets()
                .into_iter()
                .map(|(role, (sha256, size_bytes))| {
                    (
                        role,
                        GenerationAssetBinding {
                            artifact_id: format!("test-{role:?}"),
                            sha256,
                            size_bytes,
                        },
                    )
                })
                .collect(),
        },
    )
    .unwrap()
}

fn generation(session: &str, job: &str) -> PreparedGenerationTask {
    generation_service()
        .compile(GenerationTaskRequest {
            session_id: session.into(),
            job_id: job.into(),
            parameters: GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("A red apple on a wooden table".into()),
                seed: Some(GenerationSeed::Value { value: 42 }),
                ..Default::default()
            }),
        })
        .unwrap()
}

fn snapshot(record: &TaskConfigurationRecord) -> &GenerationParameterSnapshot {
    let TaskConfigurationSnapshot::Generation(snapshot) = record.snapshot() else {
        panic!("Expected generation configuration")
    };
    snapshot
}

fn parameters(snapshot: &GenerationParameterSnapshot) -> &EffectiveImageParameters {
    let EffectiveGenerationParameters::Image(parameters) = &snapshot.parameters else {
        panic!("Expected effective image parameters")
    };
    parameters
}

async fn source_asset(fixture: &Fixture, job: &str) -> (TaskConfigurationRecord, MediaResultRef) {
    fixture
        .assets
        .freeze_job(&fixture.source, job, None)
        .await
        .unwrap();
    let task = generation(&fixture.source, job);
    let configuration = fixture.configuration.save_generation(&task).await.unwrap();
    let result = fixture.publish(&task.into(), &configuration).await;
    (configuration, result)
}

#[tokio::test]
async fn project_generation_new_seed_survives_reopen_archive_and_third_session_reuse() {
    let mut fixture = Fixture::new().await;
    let (parent, parent_result) = source_asset(&fixture, "generation-parent").await;
    let parent_payload = parent.snapshot_json().to_owned();
    let reader = fixture.new_reader().await;
    let original_asset = fixture
        .assets
        .read(&reader, &parent_result.result_id)
        .await
        .unwrap()
        .unwrap();
    let task = fixture
        .prepare(
            &reader,
            &parent_result.result_id,
            "generation-seed-child",
            json!({"seed":{"kind":"new"}}),
        )
        .await
        .unwrap();
    let child = fixture
        .configuration
        .save_project_task(&task)
        .await
        .unwrap();
    let child_snapshot = snapshot(&child);
    assert_eq!(child.session_id(), reader);
    assert_eq!(child_snapshot.binding, snapshot(&parent).binding);
    assert_eq!(
        child_snapshot.origins["seed"],
        GenerationParameterOrigin::GeneratedSeed
    );
    assert_ne!(
        parameters(child_snapshot).seed,
        parameters(snapshot(&parent)).seed
    );
    assert_eq!(child_snapshot.changes.len(), 1);
    assert_eq!(child_snapshot.changes["seed"].before, json!(42));
    assert_eq!(
        child_snapshot.changes["seed"].after,
        json!(parameters(child_snapshot).seed)
    );
    let mut expected = parameters(snapshot(&parent)).clone();
    expected.seed = parameters(child_snapshot).seed;
    assert_eq!(parameters(child_snapshot), &expected);
    let seeded_value = parameters(child_snapshot).seed;
    let child_payload = child.snapshot_json().to_owned();

    fixture.reopen().await;
    let reopened = fixture
        .configuration
        .read(&reader, "generation-seed-child")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.snapshot_json(), child_payload);
    assert_eq!(reopened, child);
    let child_result = fixture.publish(task.plan(), &reopened).await;
    let archived_child = fixture
        .assets
        .read(&reader, &child_result.result_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        archived_child.content["configuration"]["snapshotJson"],
        json!(child_payload)
    );
    assert!(!archived_child.content["projectLineage"].is_null());

    fixture.reopen().await;
    let third_session = fixture.new_reader().await;
    let third_task = fixture
        .prepare(
            &third_session,
            &child_result.result_id,
            "generation-third",
            json!({"prompt":"A blue vase on the same wooden table"}),
        )
        .await
        .unwrap();
    let third = fixture
        .configuration
        .save_project_task(&third_task)
        .await
        .unwrap();
    let third_snapshot = snapshot(&third);
    assert_eq!(third.session_id(), third_session);
    assert_eq!(parameters(third_snapshot).seed, seeded_value);
    assert_eq!(
        third_snapshot.parent.as_ref().unwrap().result_id,
        child_result.result_id
    );
    assert_eq!(
        third_snapshot
            .parent
            .as_ref()
            .unwrap()
            .snapshot_digest
            .as_str(),
        child.generation_config_sha256()
    );
    assert_eq!(
        third_snapshot.origins["seed"],
        GenerationParameterOrigin::Parent {
            result_id: child_result.result_id.clone()
        }
    );
    assert_eq!(third_snapshot.changes.len(), 1);
    assert!(third_snapshot.changes.contains_key("prompt"));
    let mut expected = parameters(snapshot(&child)).clone();
    expected.prompt = "A blue vase on the same wooden table".into();
    assert_eq!(parameters(third_snapshot), &expected);
    fixture.publish(third_task.plan(), &third).await;

    fixture.reopen().await;
    assert_eq!(
        fixture
            .configuration
            .read(&third_session, "generation-third")
            .await
            .unwrap()
            .unwrap(),
        third
    );
    assert_eq!(
        fixture
            .configuration
            .read(&reader, "generation-seed-child")
            .await
            .unwrap()
            .unwrap()
            .snapshot_json(),
        child_payload
    );
    assert_eq!(
        fixture
            .configuration
            .read(&fixture.source, "generation-parent")
            .await
            .unwrap()
            .unwrap()
            .snapshot_json(),
        parent_payload
    );
    let retained_parent = fixture
        .assets
        .read(&third_session, &parent_result.result_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        retained_parent.content_sha256,
        original_asset.content_sha256
    );
    assert_eq!(retained_parent.content, original_asset.content);
    let retained_child = fixture
        .assets
        .read(&third_session, &child_result.result_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained_child.content_sha256, archived_child.content_sha256);
    assert_eq!(retained_child.content, archived_child.content);
}

#[tokio::test]
async fn project_generation_rejects_unknown_or_tampered_parameter_changes() {
    let fixture = Fixture::new().await;
    let (_, parent_result) = source_asset(&fixture, "generation-parent").await;
    let reader = fixture.new_reader().await;
    assert!(fixture
        .prepare(
            &reader,
            &parent_result.result_id,
            "unknown-field",
            json!({"inventedParameter":42})
        )
        .await
        .is_err());
    assert!(fixture
        .prepare(
            &reader,
            &parent_result.result_id,
            "multiple-fields",
            json!({"seed":{"kind":"value","value":44},"prompt":"Changed"})
        )
        .await
        .is_err());
    assert!(fixture
        .prepare(
            &reader,
            &parent_result.result_id,
            "invalid-seed",
            json!({"seed":{"kind":"value","value":-1}})
        )
        .await
        .is_err());

    let mut task = fixture
        .prepare(
            &reader,
            &parent_result.result_id,
            "forged-change",
            json!({"seed":{"kind":"value","value":43}}),
        )
        .await
        .unwrap();
    task.authorization
        .patch
        .insert("seed".into(), json!({"kind":"value","value":44}));
    assert!(fixture
        .configuration
        .save_project_task(&task)
        .await
        .is_err());
    assert!(fixture
        .configuration
        .read(&reader, "forged-change")
        .await
        .unwrap()
        .is_none());
    assert_eq!(fixture.grant_count(&reader, "forged-change").await, 0);

    let mut task = fixture
        .prepare(
            &reader,
            &parent_result.result_id,
            "forged-parent",
            json!({"prompt":"A blue vase"}),
        )
        .await
        .unwrap();
    task.authorization.compilation.parent_snapshot_digest =
        Sha256Digest::of_bytes(b"different parent snapshot");
    assert!(fixture
        .configuration
        .save_project_task(&task)
        .await
        .is_err());
    assert!(fixture
        .configuration
        .read(&reader, "forged-parent")
        .await
        .unwrap()
        .is_none());
    assert_eq!(fixture.grant_count(&reader, "forged-parent").await, 0);
}
