use super::*;
use lumina_providers::local_inference::lifecycle::*;
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;

async fn stores() -> (MediaResultStore, TaskConfigurationStore) {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE sessions(id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO sessions VALUES ('local'), ('other')")
        .execute(&pool)
        .await
        .unwrap();
    let media = MediaResultStore::new(pool.clone());
    media.initialize().await.unwrap();
    let configs = TaskConfigurationStore::new(pool);
    configs.initialize().await.unwrap();
    (media, configs)
}

fn speech() -> (PocketTtsParameterService, PocketTtsPreparedTask) {
    let profile = AudioTaskProfile::PocketTtsEnglishCpu;
    let asset = |id: &str, (sha256, size_bytes)| AudioTaskAssetBinding {
        artifact_id: id.into(),
        sha256,
        size_bytes,
    };
    let service = PocketTtsParameterService::new(AudioTaskModelBinding {
        installation_id: "synthetic-install".into(),
        version_id: "v1".into(),
        manifest_digest: Sha256Digest::of_bytes(b"synthetic replay"),
        runtime: profile.runtime_build(),
        target_capabilities: [profile.capability()].into(),
        model: asset("weight", profile.recorded_model()),
        model_spec: asset("spec", profile.recorded_model_spec()),
    })
    .unwrap();
    let task = service
        .compile(PocketTtsRequest {
            session_id: "local".into(),
            job_id: "parent".into(),
            text: "First".into(),
            voice_reference: AudioTaskMediaReference {
                session_id: "local".into(),
                result_id: uuid::Uuid::new_v4().to_string(),
                sha256: Sha256Digest::of_bytes(b"input"),
                mime_type: "audio/wav".into(),
                size_bytes: 32_044,
                sample_rate: 16_000,
                channels: 1,
                sample_frames: 16_000,
            },
            seed: Some(918),
            temperature: Some(0.25),
            max_frames: None,
            text_chunk_tokens: None,
            frames_after_eos: None,
        })
        .unwrap();
    (service, task)
}

fn request(
    parent: &TaskConfigurationRecord,
    patch: Value,
    result: Option<String>,
) -> TaskReuseRequest {
    serde_json::from_value(json!({"identity":parent.identity(),"parentJobId":parent.job_id(),"newJobId":"child","parentResultId":result,"patch":patch})).unwrap()
}

#[test]
fn session_media_parameters_follow_the_installed_descriptor() {
    let mut descriptor = speech().0.descriptor().clone();
    let voice = descriptor.fields.remove("voiceReference").unwrap();
    descriptor
        .fields
        .insert("referenceClip".into(), voice.clone());
    let mut unknown = voice;
    unknown.value_type = "session_volume_reference".into();
    descriptor.fields.insert("volume".into(), unknown);
    let contract = NativeTaskParameterContract::PocketTts(Box::new(descriptor));
    assert_eq!(
        contract.session_media_kind("referenceClip").unwrap(),
        Some(SessionMediaParameterKind::Audio)
    );
    assert_eq!(contract.session_media_kind("text").unwrap(), None);
    for unavailable in ["voiceReference", "speed", "volume"] {
        assert!(contract.session_media_kind(unavailable).is_err());
    }
}

#[tokio::test]
async fn speech_reuses_the_bound_database_row_and_rejects_inline_media_or_result_substitution() {
    let (media, configs) = stores().await;
    let (adapter, first) = speech();
    configs.save_pocket_tts(&first).await.unwrap();
    let row = configs.read("local", "parent").await.unwrap().unwrap();
    let contract =
        || NativeTaskParameterContract::PocketTts(Box::new(adapter.descriptor().clone()));
    let reuse = request(&row, json!({"text":"Second"}), None);
    let prepared = prepare_registered(&media, &configs, "local", &row, reuse, contract())
        .await
        .unwrap();
    let NativeTaskPlan::PocketTts(second) = prepared else {
        panic!("expected speech task");
    };
    assert_eq!(second.snapshot().metadata.job_id, "child");
    assert_eq!(second.snapshot().parameters.text, "Second");
    assert_eq!(second.snapshot().parameters.seed, 918);
    assert_eq!(second.snapshot().parameters.temperature, 0.25);
    assert_eq!(
        second.snapshot().parameters.voice_reference,
        first.snapshot().parameters.voice_reference
    );
    configs.save_pocket_tts(&second).await.unwrap();
    assert!(configs.read("other", "parent").await.unwrap().is_none());
    let cross = request(&row, json!({"text":"Other"}), None);
    assert!(
        prepare_registered(&media, &configs, "other", &row, cross, contract())
            .await
            .is_err()
    );
    let forged = request(
        &row,
        json!({"voiceReference":first.snapshot().parameters.voice_reference}),
        None,
    );
    assert!(
        prepare_registered(&media, &configs, "local", &row, forged, contract())
            .await
            .is_err()
    );
    let wrong = request(
        &row,
        json!({"text":"Other"}),
        Some(uuid::Uuid::new_v4().to_string()),
    );
    assert!(
        prepare_registered(&media, &configs, "local", &row, wrong, contract())
            .await
            .is_err()
    );
    let mut drift = request(&row, json!({"text":"Other"}), None);
    drift.identity.version_id = "substituted".into();
    assert!(
        prepare_registered(&media, &configs, "local", &row, drift, contract())
            .await
            .is_err()
    );
    let mut other_binding = adapter.descriptor().binding.clone();
    other_binding.version_id = "other-registered-version".into();
    let other = PocketTtsParameterService::new(other_binding).unwrap();
    let wrong_adapter =
        NativeTaskParameterContract::PocketTts(Box::new(other.descriptor().clone()));
    assert!(prepare_registered(
        &media,
        &configs,
        "local",
        &row,
        request(&row, json!({"text":"Other"}), None),
        wrong_adapter
    )
    .await
    .is_err());
    for patch in [
        json!({}),
        json!({"text":null}),
        json!({"text":"New","seed":1}),
    ] {
        assert!(request(&row, patch, None).validate().is_err());
    }
}

#[cfg(windows)]
#[tokio::test]
async fn generation_requires_a_published_result_from_this_model_job_and_session() {
    use crate::media::{
        BeginMediaPublish, MediaAssociation, MediaRuntimeBuild, MediaSource, PublishMediaRequest,
    };
    use std::{
        io::{Cursor, Write},
        path::PathBuf,
    };

    let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/task-reuse-tests");
    std::fs::create_dir_all(&temp_dir).unwrap();
    let temp = tempfile::tempdir_in(temp_dir.canonicalize().unwrap()).unwrap();
    let (media, configs) = stores().await;
    let root = media
        .register_output_root(&temp.path().join("outputs"))
        .await
        .unwrap();
    let profile = GenerationProfile::Sd15CpuTextToImage;
    let runtime = profile.runtime_build();
    let adapter = GenerationParameterService::new(
        profile,
        GenerationModelBinding {
            installation_id: "synthetic".into(),
            version_id: "v1".into(),
            manifest_digest: Sha256Digest::of_bytes(b"synthetic generation"),
            runtime: runtime.clone(),
            target_capabilities: profile.required_capabilities(),
            assets: profile
                .recorded_assets()
                .into_iter()
                .map(|(role, (sha256, size_bytes))| {
                    (
                        role,
                        GenerationAssetBinding {
                            artifact_id: format!("{role:?}"),
                            sha256,
                            size_bytes,
                        },
                    )
                })
                .collect(),
        },
    )
    .unwrap();
    let first = adapter
        .compile(GenerationTaskRequest {
            session_id: "local".into(),
            job_id: "parent".into(),
            parameters: GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("apple".into()),
                seed: Some(GenerationSeed::Value { value: 71 }),
                ..Default::default()
            }),
        })
        .unwrap();
    let row = configs.save_generation(&first).await.unwrap();
    let scoped = |digest: &str| MediaSource::LocalModel {
        installation_id: "synthetic".into(),
        version_id: "v1".into(),
        runtime: MediaRuntimeBuild {
            adapter_id: runtime.adapter_id.clone(),
            engine_revision: runtime.engine_revision.clone(),
            binary_sha256: runtime.binary_sha256.as_str().into(),
        },
        generation_config_sha256: digest.into(),
        input_refs: vec![],
    };
    let mcp = MediaSource::Mcp {
        server_id: "fixture".into(),
        tool_name: "other".into(),
        tool_call_id: "tool-call".into(),
        input_refs: vec![],
    };
    let contract =
        || NativeTaskParameterContract::Generation(Box::new(adapter.descriptor().clone()));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::new(2, 2))
        .write_to(&mut bytes, image::ImageOutputFormat::Png)
        .unwrap();
    for (session, job, source, valid) in [
        (
            "local",
            "parent",
            scoped(row.generation_config_sha256()),
            true,
        ),
        (
            "other",
            "parent",
            scoped(row.generation_config_sha256()),
            false,
        ),
        (
            "local",
            "other-job",
            scoped(row.generation_config_sha256()),
            false,
        ),
        ("local", "parent", scoped(&"0".repeat(64)), false),
        ("local", "parent", mcp, false),
    ] {
        let id = uuid::Uuid::new_v4().to_string();
        let publish = PublishMediaRequest {
            session_id: session.into(),
            job_id: job.into(),
            result_id: id.clone(),
            root_id: root.root_id.clone(),
            kind: MediaKind::Image,
            association: MediaAssociation::Message {
                message_id: "message".into(),
            },
            source,
        };
        let BeginMediaPublish::Staging(mut staged) = media.begin_publish(publish).await.unwrap()
        else {
            panic!("expected new result");
        };
        staged.write_all(bytes.get_ref()).unwrap();
        media.publish(staged).await.unwrap();
        if valid {
            let mut descriptor = adapter.descriptor().clone();
            let mut image_field = descriptor.fields["prompt"].clone();
            image_field.value_type = "session_image_reference".into();
            descriptor
                .fields
                .insert("referencePose".into(), image_field.clone());
            let pose = NativeTaskParameterContract::Generation(Box::new(descriptor.clone()));
            let mut inputs = serde_json::Map::from_iter([("referencePose".into(), json!(id))]);
            let mut foreign_inputs = inputs.clone();
            assert!(
                resolve_media_patch(&media, "other", &mut foreign_inputs, &pose)
                    .await
                    .is_err()
            );
            resolve_media_patch(&media, "local", &mut inputs, &pose)
                .await
                .unwrap();
            let resolved: GenerationMediaInput =
                serde_json::from_value(inputs["referencePose"].clone()).unwrap();
            assert_eq!(resolved.result_id, id);
            assert_eq!((resolved.width, resolved.height), (2, 2));
            for status in [
                GenerationParameterSupport::Unknown,
                GenerationParameterSupport::Unsupported,
            ] {
                descriptor.fields.get_mut("referencePose").unwrap().support = status;
                let unavailable =
                    NativeTaskParameterContract::Generation(Box::new(descriptor.clone()));
                assert!(unavailable.session_media_kind("referencePose").is_err());
            }
        }
        let reuse = request(&row, json!({"prompt":"pear"}), Some(id.clone()));
        let outcome = prepare_registered(&media, &configs, "local", &row, reuse, contract()).await;
        if valid {
            let NativeTaskPlan::Generation(child) = outcome.unwrap() else {
                panic!("expected generated image");
            };
            assert_eq!(child.snapshot().parent.as_ref().unwrap().result_id, id);
            assert_eq!(child.snapshot().changes["prompt"].after, json!("pear"));
        } else {
            assert!(outcome.is_err());
        }
    }
    let absent = request(
        &row,
        json!({"steps":3}),
        Some(uuid::Uuid::new_v4().to_string()),
    );
    assert!(
        prepare_registered(&media, &configs, "local", &row, absent, contract())
            .await
            .is_err()
    );
    let no_id = request(&row, json!({"steps":3}), None);
    assert!(
        prepare_registered(&media, &configs, "local", &row, no_id, contract())
            .await
            .is_err()
    );
}
