use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Fixture {
    repository: LifecycleRepository,
    database: PathBuf,
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/lifecycle-validation/vector-plan-tests");
        std::fs::create_dir_all(&root).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let database = temp.path().join("models.db");
        let repository = LifecycleRepository::open(&database).unwrap();
        Self {
            repository,
            database,
            temp,
        }
    }

    fn counts(&self) -> (u64, u64, u64, u64, u64) {
        rusqlite::Connection::open(&self.database).unwrap().query_row(
            "SELECT (SELECT count(*) FROM catalog_models), (SELECT count(*) FROM gguf_observations), (SELECT count(*) FROM installation_plans), (SELECT count(*) FROM lifecycle_jobs), (SELECT count(*) FROM capability_evidence)",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).unwrap()
    }
}

#[test]
fn vector_plan_freezes_observation_bindings_and_persists_without_installing() {
    for profile in PROFILES {
        let fixture = Fixture::new();
        let observation = component(profile);
        let assessment = fixture
            .repository
            .prepare_vector_plan(&observation, &hardware(), 100)
            .unwrap();
        assert!(
            assessment.blocking_reasons.is_empty(),
            "{:?}",
            assessment.blocking_reasons
        );
        let plan = assessment.plan.unwrap();
        assert_eq!(
            plan.target_capabilities,
            BTreeSet::from([profile.capability()])
        );
        assert_eq!(plan.expires_at, 400);
        assert_eq!(
            plan.artifacts[&observation.files[0].artifact.id],
            observation.files[0].artifact
        );
        assert_eq!(
            plan.snapshots[&observation.files[0].artifact.snapshot_id],
            observation.files[0].snapshot
        );
        assert_eq!(
            validate_vector_admission(plan.admission.as_ref().unwrap()).unwrap(),
            profile
        );
        assert_eq!(
            plan.admission.as_ref().unwrap().budget,
            assessment.resource_budget.unwrap()
        );
        let observation_id = &assessment.observation_bindings[&observation.files[0].artifact.id];
        assert_eq!(
            fixture
                .repository
                .read_gguf_observation(observation_id)
                .unwrap()
                .unwrap()
                .observation,
            observation.files[0]
        );
        let reopened = LifecycleRepository::open(&fixture.database).unwrap();
        assert_eq!(reopened.read_plan(&plan.id).unwrap(), Some(plan.clone()));
        let catalog = reopened.read_catalog(&plan.model_id).unwrap().unwrap();
        assert!(catalog.model.declared_capabilities.is_empty());
        assert_eq!(catalog.artifacts, plan.artifacts);
        assert_eq!(fixture.counts(), (1, 1, 1, 0, 0));
        let mut refreshed = observation;
        refreshed.files[0].snapshot.fetched_at = 101;
        let repeat = reopened
            .prepare_vector_plan(&refreshed, &hardware(), 101)
            .unwrap()
            .plan
            .unwrap();
        assert_eq!(repeat.model_id, plan.model_id);
        assert_eq!(repeat.variant_id, plan.variant_id);
        assert_ne!(repeat.id, plan.id);
        assert_eq!(fixture.counts(), (1, 1, 2, 0, 0));
    }
}

#[test]
fn unknown_content_and_mismatched_sources_are_rejected_before_catalog_or_plan_writes() {
    let mutations: &[fn(&mut ComponentObservation)] = &[
        |o| o.files[0].artifact.source_sha256 = Some(Sha256Digest::of_bytes(b"different model")),
        |o| o.files[0].artifact.source_sha256 = None,
        |o| {
            o.files[0].artifact.size_bytes = Some(1024);
            o.files[0].header.file_bytes = 1024;
        },
        |o| o.role = ComponentRole::VisionProjector,
        |o| {
            o.files[0].snapshot.repository =
                ModelScopeRepository::parse("different/source").unwrap()
        },
        |o| o.files[0].artifact.snapshot_id = "different-snapshot".into(),
        |o| o.files[0].snapshot.resolved_revision = "master".into(),
        |o| {
            o.files[0]
                .header
                .metadata
                .insert("general.architecture".into(), text("unknown"));
        },
    ];
    for profile in PROFILES {
        let fixture = Fixture::new();
        for mutate in mutations {
            let mut changed = component(profile);
            mutate(&mut changed);
            assert!(fixture
                .repository
                .prepare_vector_plan(&changed, &hardware(), 100)
                .is_err());
            assert_eq!(fixture.counts(), (0, 0, 0, 0, 0));
        }
    }
}

#[test]
fn stale_future_or_insufficient_hardware_never_persists_an_executable_plan() {
    for profile in PROFILES {
        let fixture = Fixture::new();
        let observation = component(profile);
        for failure in ["stale", "future", "memory", "disk"] {
            let mut observed = hardware();
            let expected = match failure {
                "stale" => {
                    observed.observed_at = 39;
                    LifecycleErrorCode::PlanStale
                }
                "future" => {
                    observed.observed_at = 101;
                    LifecycleErrorCode::PlanStale
                }
                "memory" => {
                    observed.devices[0].free_bytes = 1;
                    LifecycleErrorCode::HardwareInsufficient
                }
                "disk" => {
                    observed.disk_free_bytes = 1;
                    LifecycleErrorCode::DiskInsufficient
                }
                _ => unreachable!(),
            };
            let assessment = fixture
                .repository
                .prepare_vector_plan(&observation, &observed, 100)
                .unwrap();
            assert!(assessment.plan.is_none(), "{failure}");
            assert!(
                assessment
                    .blocking_reasons
                    .iter()
                    .any(|reason| reason.code == expected),
                "{failure}"
            );
            assert_eq!(fixture.counts(), (1, 1, 0, 0, 0));
        }
    }
}

#[test]
fn admitted_vector_plan_builds_the_shared_launch_and_rejects_parameter_capability_and_content_changes(
) {
    for profile in PROFILES {
        let fixture = Fixture::new();
        let observation = component(profile);
        let plan = fixture
            .repository
            .prepare_vector_plan(&observation, &hardware(), 100)
            .unwrap()
            .plan
            .unwrap();
        let artifact = &observation.files[0].artifact;
        // This is a launch-binding fixture, not an installation or readiness claim.
        let manifest = ModelManifest {
            schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
            installation_id: "unit-installation".into(),
            version_id: "unit-version".into(),
            plan,
            installed_files: BTreeMap::from([(
                artifact.id.clone(),
                InstalledArtifact {
                    artifact_id: artifact.id.clone(),
                    relative_path: installed_artifact_path(
                        &artifact.id,
                        artifact.purpose,
                        &artifact.path,
                    )
                    .unwrap(),
                    computed_sha256: artifact.source_sha256.clone().unwrap(),
                    size_bytes: profile.model_bytes(),
                    verification_evidence: Sha256Digest::of_bytes(b"synthetic launch fixture"),
                },
            )]),
            plan_accepted_at: 100,
            installed_at: 101,
        };
        manifest.validate().unwrap();
        let installed = InstalledVersion {
            manifest_digest: manifest.digest().unwrap(),
            manifest,
            registered_at: 101,
        };
        let mut launch = LaunchConfig::for_measured_cpu_installation(&installed).unwrap();
        assert_eq!(launch.parameters, profile.launch_parameters());
        assert_eq!(launch.capabilities, BTreeSet::from([profile.capability()]));
        assert_eq!(launch.mode, RuntimeExecutionMode::Helper { port: 0 });
        assert!(launch.validate_binding(&installed.manifest).is_err());
        launch.mode = RuntimeExecutionMode::Helper { port: 19238 };
        launch.validate_binding(&installed.manifest).unwrap();
        for field in ["context_length", "cache_prompt", "flash_attention"] {
            let mut changed = launch.clone();
            let RuntimeParameters::Specialized { settings, .. } = &mut changed.parameters else {
                unreachable!()
            };
            settings.insert(field.into(), "unmeasured".into());
            assert!(
                changed.validate_binding(&installed.manifest).is_err(),
                "{field}"
            );
        }
        let mut changed = launch.clone();
        changed.capabilities.insert(ModelCapability::Text);
        assert!(changed.validate_binding(&installed.manifest).is_err());
        let mut changed_manifest = installed.manifest.clone();
        let wrong_hash = Sha256Digest::of_bytes(b"wrong model but internally matching manifest");
        changed_manifest
            .plan
            .artifacts
            .get_mut(&artifact.id)
            .unwrap()
            .source_sha256 = Some(wrong_hash.clone());
        changed_manifest
            .installed_files
            .get_mut(&artifact.id)
            .unwrap()
            .computed_sha256 = wrong_hash;
        changed_manifest.validate().unwrap();
        let mut changed = launch;
        changed.manifest_digest = changed_manifest.digest().unwrap();
        let error = changed.validate_binding(&changed_manifest).unwrap_err();
        assert!(error
            .to_string()
            .contains("Vector installation content differs"));
        assert_eq!(fixture.counts(), (1, 1, 1, 0, 0));
    }
}

#[tokio::test]
async fn application_plan_rejects_non_gguf_before_credentials_network_or_storage() {
    let fixture = Fixture::new();
    let data = fixture.temp.path().join("app-data");
    let app = InstallationApplication::new(
        &data,
        Arc::new(|| panic!("credentials must not be requested")),
    );
    let error = app
        .prepare_vector_plan(
            ModelScopeRepository::parse("unit/vector").unwrap(),
            "master",
            RepositoryPath::parse("model.safetensors").unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::FormatForbidden
    );
    assert_eq!(app.state(), InstallationApplicationState::Dormant);
    assert!(!app.root().exists());
}

#[tokio::test]
async fn application_plan_reports_scoped_credential_failure_without_leaking_or_creating_storage() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let called = Arc::clone(&calls);
    let app = InstallationApplication::new(
        &fixture.temp.path().join("app-data"),
        Arc::new(move || {
            called.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("credential-fixture-sensitive-value")
        }),
    );
    let error = app
        .prepare_vector_plan(
            ModelScopeRepository::parse("unit/vector").unwrap(),
            "master",
            RepositoryPath::parse("model.gguf").unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ModelSourceError>().unwrap().code,
        LifecycleErrorCode::SourceAuthRequired
    );
    assert!(!format!("{error:#}").contains("credential-fixture-sensitive-value"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(app.state(), InstallationApplicationState::Dormant);
    assert!(!app.root().exists());
}
