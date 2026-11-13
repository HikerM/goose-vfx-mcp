use super::*;
use crate::acp::server_factory::{AcpServer, AcpServerFactoryConfig};

fn fixture() -> (tempfile::TempDir, AcpServer) {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/acp-native-task-catalog");
    std::fs::create_dir_all(&base).unwrap();
    let directory = tempfile::tempdir_in(base.canonicalize().unwrap()).unwrap();
    let server = AcpServer::new(AcpServerFactoryConfig {
        builtins: Vec::new(),
        data_dir: directory.path().join("data"),
        config_dir: directory.path().join("config"),
        lumina_platform: LuminaPlatform::LuminaCli,
        additional_source_roots: Vec::new(),
    });
    (directory, server)
}

#[cfg(feature = "local-inference")]
async fn activate(connection: &LuminaAcpAgent, id: &str, directory: &Path) {
    let agent = Arc::new(Agent::with_config(AgentConfig::new(
        connection.session_manager.clone(),
        Arc::new(PermissionManager::new(directory.join("config"))),
        None,
        LuminaMode::default(),
        true,
        LuminaPlatform::LuminaCli,
    )));
    connection
        .register_acp_session(id.into(), agent, HashMap::new())
        .await;
}

#[cfg(feature = "local-inference")]
#[tokio::test]
async fn list_and_describe_require_the_current_connection_and_use_its_managed_repository() {
    let (directory, server) = fixture();
    let a = server.create_agent().await.unwrap();
    let b = server.create_agent().await.unwrap();
    let list = NativeTaskListRequest {
        session_id: "catalog-session".into(),
        intent: NativeTaskIntentDto::TextToVideoWithAudio,
    };
    let describe = NativeTaskDescribeRequest {
        session_id: list.session_id.clone(),
        identity: NativeTaskIdentityDto {
            installation_id: "missing-install".into(),
            version_id: "missing-version".into(),
            ..Default::default()
        },
    };
    assert!(a.on_native_task_list(list.clone()).await.is_err());
    assert!(a.on_native_task_describe(describe.clone()).await.is_err());
    assert!(!directory
        .path()
        .join("data/model-lifecycle/models.db")
        .exists());
    activate(&a, &list.session_id, directory.path()).await;
    assert!(b.on_native_task_list(list.clone()).await.is_err());
    let response = a
        .dispatch_custom_request(
            NATIVE_TASK_LIST_METHOD,
            serde_json::to_value(&list).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response, serde_json::json!({"tasks":[]}));
    assert!(matches!(
        a.on_native_task_describe(describe.clone()).await.unwrap(),
        NativeTaskDescribeResponse::NotFound
    ));
    assert!(b.on_native_task_describe(describe.clone()).await.is_err());
    assert!(Arc::ptr_eq(&a.local_installations, &b.local_installations));
    a.on_close_session(&list.session_id).await.unwrap();
    assert!(a.on_native_task_list(list).await.is_err());
    assert!(a.on_native_task_describe(describe).await.is_err());
    server.shutdown().await;
}

#[cfg(feature = "local-inference")]
#[tokio::test]
async fn replacing_session_while_catalog_lookup_is_in_flight_rejects_the_old_response() {
    let (directory, server) = fixture();
    let agent = server.create_agent().await.unwrap();
    let id = "replaced-session";
    activate(&agent, id, directory.path()).await;
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let lookup = agent.catalog_for_session(id, move |_| {
        entered.send(()).unwrap();
        wait.recv_timeout(std::time::Duration::from_secs(15))?;
        Ok(())
    });
    let replace = async {
        started.await.unwrap();
        activate(&agent, id, directory.path()).await;
        release.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(lookup, replace);
    assert!(result.is_err());
    assert!(agent
        .on_native_task_list(NativeTaskListRequest {
            session_id: id.into(),
            intent: NativeTaskIntentDto::Transcription
        })
        .await
        .is_ok());
    server.shutdown().await;
}

#[cfg(feature = "local-inference")]
#[test]
fn registered_generation_variants_preserve_their_native_fields_and_identity_on_the_wire() {
    use domain::ModelTaskParameterAdapter;
    let mut mapped = std::collections::BTreeMap::new();
    for profile in domain::GenerationProfile::registered() {
        let binding = domain::GenerationModelBinding {
            installation_id: "synthetic".into(),
            version_id: "v".into(),
            manifest_digest: domain::Sha256Digest::of_bytes(b"synthetic descriptor mapping only"),
            runtime: profile.runtime_build(),
            target_capabilities: profile.required_capabilities(),
            assets: profile
                .recorded_assets()
                .into_iter()
                .map(|(role, (sha256, size_bytes))| {
                    (
                        role,
                        domain::GenerationAssetBinding {
                            artifact_id: format!("artifact-{role:?}"),
                            sha256,
                            size_bytes,
                        },
                    )
                })
                .collect(),
        };
        let service = domain::GenerationParameterService::new(profile, binding).unwrap();
        let expected = serde_json::to_value(&service.describe().fields).unwrap();
        let task = mapping::description(domain::InstalledTaskDescription {
            identity: service.identity().unwrap(),
            intent: profile.intent(),
            descriptor: domain::NativeTaskParameterContract::Generation(Box::new(
                service.describe().clone(),
            )),
        })
        .unwrap();
        let value = serde_json::to_value(NativeTaskDescribeResponse::Found {
            task: Box::new(task),
        })
        .unwrap();
        assert_eq!(
            value["task"]["descriptor"]["parameters"]["fields"],
            expected
        );
        assert!(value["task"]["descriptor"]["parameters"]
            .get("binding")
            .is_none());
        assert_eq!(
            value["task"]["identity"]["taskVariant"],
            profile.task_variant()
        );
        assert_eq!(
            value["task"]["identity"]["parameterSchemaId"],
            profile.parameter_schema_id()
        );
        assert_eq!(
            value["task"]["intent"],
            serde_json::to_value(profile.intent()).unwrap()
        );
        assert!(mapped.insert(profile.task_variant(), value).is_none());
    }
    let h3 = &mapped[domain::GenerationProfile::H3CudaTextToVideoAudio.task_variant()];
    let h3_first = &mapped[domain::GenerationProfile::H3CudaFirstFrameToVideoAudio.task_variant()];
    let wan = &mapped[domain::GenerationProfile::installation_profile("wan21-13b-cpu")
        .unwrap()
        .task_variant()];
    assert_ne!(
        h3["task"]["descriptor"]["parameters"]["fields"]["fps"]["default"],
        wan["task"]["descriptor"]["parameters"]["fields"]["fps"]["default"]
    );
    assert_ne!(
        h3["task"]["descriptor"]["parameters"]["fields"]["firstFrame"]["support"],
        h3_first["task"]["descriptor"]["parameters"]["fields"]["firstFrame"]["support"]
    );
}

#[cfg(not(feature = "local-inference"))]
#[tokio::test]
async fn both_catalog_routes_reject_when_local_inference_is_disabled() {
    let (_directory, server) = fixture();
    let agent = server.create_agent().await.unwrap();
    for error in [
        agent
            .on_native_task_list(NativeTaskListRequest::default())
            .await
            .unwrap_err(),
        agent
            .on_native_task_describe(NativeTaskDescribeRequest::default())
            .await
            .unwrap_err(),
    ] {
        assert_eq!(
            error.data,
            Some(serde_json::json!("Local inference not enabled"))
        );
    }
    server.shutdown().await;
}
