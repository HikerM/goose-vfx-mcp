use super::*;
use lumina_providers::local_inference::lifecycle::*;
use std::{path::PathBuf, time::Duration};

fn prepared() -> NativeTaskPlan {
    let profile = GenerationProfile::Sd15CpuTextToImage;
    let binding = GenerationModelBinding {
        installation_id: "unregistered-installation".into(),
        version_id: "unregistered-version".into(),
        manifest_digest: Sha256Digest::of_bytes(b"fixture"),
        runtime: profile.runtime_build(),
        target_capabilities: profile.required_capabilities(),
        assets: profile
            .recorded_assets()
            .into_iter()
            .map(|(role, (sha256, size_bytes))| {
                (
                    role,
                    GenerationAssetBinding {
                        artifact_id: "model".into(),
                        sha256,
                        size_bytes,
                    },
                )
            })
            .collect(),
    };
    GenerationParameterService::new(profile, binding)
        .unwrap()
        .compile(GenerationTaskRequest {
            session_id: "session-a".into(),
            job_id: "job-a".into(),
            parameters: GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("an apple".into()),
                ..Default::default()
            }),
        })
        .unwrap()
        .into()
}

#[tokio::test]
async fn rejects_foreign_session_and_absent_install_before_persisting_configuration() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/task-service-unit");
    std::fs::create_dir_all(&directory).unwrap();
    let tmp = tempfile::tempdir_in(directory.canonicalize().unwrap()).unwrap();
    let service = InstallationService::start(
        Arc::new(LifecycleRepository::open(&tmp.path().join("models.db")).unwrap()),
        Arc::new(InstallationStore::open(&tmp.path().join("models")).unwrap()),
        Arc::new(|| Ok(None)),
        Arc::new(|| 1000),
        Duration::from_millis(50),
    )
    .unwrap();
    let storage = SessionStorage::new(tmp.path().to_owned());
    let engines = NativeTaskEngineRegistry::default();
    let association = MediaAssociation::Message {
        message_id: "test".into(),
    };
    for (session, expected) in [
        ("session-b", "different session"),
        ("session-a", "registered installation version"),
    ] {
        // The ACP command may spawn this future on its multithreaded runtime.
        fn is_send<T: Send>(value: T) -> T {
            value
        }
        let error = is_send(execute_prepared_task(
            &service,
            &engines,
            &storage,
            session,
            prepared(),
            association.clone(),
        ))
        .await
        .unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
    assert!(!storage.media_output_directory().exists());
    let configurations = TaskConfigurationStore::new(storage.pool().await.unwrap().clone());
    assert!(configurations
        .read("session-a", "job-a")
        .await
        .unwrap()
        .is_none());
    service.shutdown().await.unwrap();
}
