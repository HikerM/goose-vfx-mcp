use super::*;
use crate::lifecycle::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

// Synthetic manifest rows exercise descriptor selection only. No file was
// downloaded, registered or admitted by these fixtures.
fn manifest(
    version: &str,
    runtime: RuntimeBuild,
    capabilities: BTreeSet<ModelCapability>,
    roles: Vec<(ComponentRole, Sha256Digest, u64)>,
) -> InstalledVersion {
    let mut components = BTreeMap::new();
    let mut artifacts = BTreeMap::new();
    let mut installed_files = BTreeMap::new();
    let mut edges = Vec::new();
    for (index, (role, sha, size)) in roles.into_iter().enumerate() {
        let id = format!("artifact-{index}");
        let component = format!("role-{index}");
        let (purpose, path) = if role == ComponentRole::Metadata {
            (FilePurpose::RuntimeConfig, "config.json".to_string())
        } else {
            (FilePurpose::GgufWeight, format!("unlabelled-{index}.gguf"))
        };
        let path = RepositoryPath::parse(path).unwrap();
        components.insert(
            component.clone(),
            Component {
                id: component.clone(),
                role,
                artifact_ids: BTreeSet::from([id.clone()]),
                compatibility: ComponentCompatibility::Verified,
                evidence_ids: BTreeSet::from(["synthetic-unit-evidence".into()]),
            },
        );
        if index > 0 {
            edges.push(DependencyEdge {
                from: "role-0".into(),
                to: component,
                requirement: DependencyRequirement::Required,
            });
        }
        artifacts.insert(
            id.clone(),
            Artifact {
                id: id.clone(),
                snapshot_id: "synthetic".into(),
                path: path.clone(),
                purpose,
                size_bytes: Some(size),
                source_sha256: Some(sha.clone()),
            },
        );
        installed_files.insert(
            id.clone(),
            InstalledArtifact {
                relative_path: installed_artifact_path(&id, purpose, &path).unwrap(),
                artifact_id: id,
                computed_sha256: sha,
                size_bytes: size,
                verification_evidence: Sha256Digest::of_bytes(b"synthetic only"),
            },
        );
    }
    let manifest = ModelManifest {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        installation_id: "opaque-installation".into(),
        version_id: version.into(),
        plan: InstallPlan {
            schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
            id: "test-plan".into(),
            model_id: "same-visible-model".into(),
            variant_id: "same-visible-variant".into(),
            target_capabilities: capabilities,
            snapshots: BTreeMap::from([(
                "synthetic".into(),
                RepositorySnapshot {
                    repository: ModelScopeRepository::parse("fixture/no-download").unwrap(),
                    requested_revision: "master".into(),
                    resolved_revision: "a".repeat(40),
                    files_digest: Sha256Digest::of_bytes(b"unit inventory"),
                    fetched_at: 1,
                },
            )]),
            artifacts,
            dependencies: DependencyGraph {
                root: "role-0".into(),
                components,
                edges,
            },
            runtime,
            runtime_verified: true,
            hardware: HardwareCompatibility::Runnable,
            required_disk_bytes: installed_files.values().map(|f| f.size_bytes).sum(),
            rules_version: "synthetic-unit-only".into(),
            expires_at: 100,
            admission: None,
        },
        installed_files,
        plan_accepted_at: 1,
        installed_at: 2,
    };
    manifest.validate().unwrap();
    InstalledVersion {
        manifest_digest: manifest.digest().unwrap(),
        manifest,
        registered_at: 3,
    }
}

fn h3(version: &str) -> InstalledVersion {
    generation(version, GenerationProfile::H3CudaTextToVideoAudio)
}

fn generation(version: &str, profile: GenerationProfile) -> InstalledVersion {
    let roles = profile
        .recorded_assets()
        .into_iter()
        .map(|(role, (sha, size))| {
            (
                match role {
                    GenerationAssetRole::Main => ComponentRole::Main,
                    GenerationAssetRole::TextEncoder => ComponentRole::TextEncoder,
                    GenerationAssetRole::VideoVae => ComponentRole::Vae,
                    GenerationAssetRole::AudioVae => ComponentRole::AudioDecoder,
                },
                sha,
                size,
            )
        })
        .collect();
    manifest(
        version,
        profile.runtime_build(),
        profile.required_capabilities(),
        roles,
    )
}

#[test]
fn external_video_profiles_drive_catalog_and_request_validation() {
    const CHILD: &str = "LUMINA_TEST_EXTERNAL_GENERATION_CATALOG_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        let definition = |name: &str, rates: &[u32], sampler: &str, scheduler: &str| {
            let mut assets = BTreeMap::new();
            for role in ["main", "text_encoder", "video_vae"] {
                assets.insert(
                    role,
                    json!({
                        "repository": "fixture/unreleased", "revision": "a".repeat(40),
                        "file": format!("{name}-{role}.gguf"), "sizeBytes": 1024,
                        "sha256": Sha256Digest::of_bytes(format!("{name}-{role}").as_bytes())
                    }),
                );
            }
            json!({"name": name, "adapterProtocol": "sd-cpp/video-t5-cpu-v1",
                "intents": ["text_to_video"], "assets": assets,
                "defaults": {"width": 128, "height": 128, "dimensionMultiple": 16,
                    "negativePrompt": "", "steps": 4, "cfgScale": 1.0, "seed": 42,
                    "samplers": [sampler], "schedulers": [scheduler],
                    "video": {"frames": 9, "minimumFrames": 1, "frameStep": 2, "fps": rates[0],
                        "supportedFps": rates, "fpsConstraint": "Select a declared rate", "audioMode": "video_only"}
                }
            })
        };
        let models = [
            definition("external-film", &[8, 12, 24], "dpm++2m_sde", "exponential"),
            definition("external-preview", &[6], "heun", "simple"),
        ];
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"schemaVersion":1,"models":models})).unwrap(),
        )
        .unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lifecycle::native_task_catalog::tests::external_video_profiles_drive_catalog_and_request_validation", "--nocapture"])
            .env("LUMINA_GENERATION_CATALOG", path).env(CHILD, "1").output().unwrap();
        assert!(
            result.status.success() && String::from_utf8_lossy(&result.stdout).contains("1 passed"),
            "{} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let mut catalog = NativeTaskCatalog::default();
    for name in ["external-film", "external-preview"] {
        let profile = GenerationProfile::installation_profile(name).unwrap();
        catalog
            .register_version(&generation(name, profile))
            .unwrap();
    }
    let entries = catalog.list(NativeTaskIntent::TextToVideo);
    assert_eq!(entries.len(), 2);
    let request = |fps: u32| {
        json!({"sessionId":"session", "jobId":"job", "parameters":{
        "task":"video","parameters":{"prompt":"An apple rolls","fps":fps,"durationSeconds":0.75}}})
    };
    for entry in entries {
        let NativeTaskParameterContract::Generation(desc) = entry.descriptor else {
            panic!("generation descriptor")
        };
        if entry.identity.version_id == "external-film" {
            assert_eq!(
                desc.fields["fps"].enum_values,
                vec![json!(8), json!(12), json!(24)]
            );
            let NativeTaskPlan::Generation(prepared) =
                catalog.prepare(&entry.identity, request(12)).unwrap()
            else {
                panic!("generation plan")
            };
            let EffectiveGenerationParameters::Video(parameters) = &prepared.snapshot().parameters
            else {
                panic!("video parameters")
            };
            assert_eq!((parameters.fps, parameters.frames), (12, 9));
            let flags = prepared.arguments();
            for (flag, option) in [
                ("--sampling-method", "dpm++2m_sde"),
                ("--scheduler", "exponential"),
            ] {
                assert!(flags.windows(2).any(|pair| pair
                    == [
                        NativeGenerationArgument::Literal(flag.into()),
                        NativeGenerationArgument::Literal(option.into())
                    ]));
            }
            let mut other_model_option = request(12);
            other_model_option["parameters"]["parameters"]["sampler"] = json!("heun");
            assert!(catalog
                .prepare(&entry.identity, other_model_option)
                .is_err());
            assert!(!prepared.snapshot().observation.matches_observed_request);
            assert!(catalog.prepare(&entry.identity, request(10)).is_err());
        } else {
            assert_eq!(desc.fields["fps"].enum_values, vec![json!(6)]);
            assert!(catalog.prepare(&entry.identity, request(12)).is_err());
        }
    }
}

fn audio(version: &str, profile: AudioTaskProfile) -> InstalledVersion {
    let (sha, bytes) = profile.recorded_model();
    let (spec, spec_bytes) = profile.recorded_model_spec();
    let mut version = manifest(
        version,
        profile.runtime_build(),
        BTreeSet::from([profile.capability()]),
        vec![
            (ComponentRole::Main, sha, bytes),
            (ComponentRole::Metadata, spec, spec_bytes),
        ],
    );
    let revision = "459bd958d533fc2530e985e6bdf8cdfa2a5df3a6";
    let snapshot = RepositorySnapshot {
        repository: ModelScopeRepository::parse("audio-cpp/audio.cpp-gguf").unwrap(),
        requested_revision: revision.into(),
        resolved_revision: revision.into(),
        files_digest: Sha256Digest::of_bytes(b"synthetic inventory for catalog unit test"),
        fetched_at: 1,
    };
    let spec = profile.runtime_spec_artifact(&snapshot).unwrap();
    let snapshot_id = snapshot.content_digest().unwrap().as_str().to_owned();
    let manifest = &mut version.manifest;
    let plan = &mut manifest.plan;
    plan.snapshots = BTreeMap::from([(snapshot_id.clone(), snapshot)]);
    let weight = plan.artifacts.get_mut("artifact-0").unwrap();
    weight.path = RepositoryPath::parse(profile.weight_repository_path()).unwrap();
    weight.snapshot_id = snapshot_id;
    let file = manifest.installed_files.get_mut(&weight.id).unwrap();
    file.relative_path = installed_artifact_path(&weight.id, weight.purpose, &weight.path).unwrap();
    let mut spec_file = manifest.installed_files.remove("artifact-1").unwrap();
    spec_file.artifact_id.clone_from(&spec.id);
    spec_file.relative_path = installed_artifact_path(&spec.id, spec.purpose, &spec.path).unwrap();
    manifest.installed_files.insert(spec.id.clone(), spec_file);
    plan.dependencies
        .components
        .get_mut("role-1")
        .unwrap()
        .artifact_ids = BTreeSet::from([spec.id.clone()]);
    plan.artifacts.remove("artifact-1").unwrap();
    plan.artifacts.insert(spec.id.clone(), spec);
    plan.rules_version = AUDIO_INSTALL_RULES_VERSION.into();
    plan.hardware = HardwareCompatibility::Unknown;
    plan.required_disk_bytes = profile.install_peak_bytes();
    manifest.validate().unwrap();
    version.manifest_digest = manifest.digest().unwrap();
    version
}

#[test]
fn h3_intents_have_different_identities_and_first_frame_is_mandatory_only_for_its_variant() {
    let mut catalog = NativeTaskCatalog::default();
    catalog.register_version(&h3("v1")).unwrap();
    let text = catalog
        .list(NativeTaskIntent::TextToVideoWithAudio)
        .pop()
        .unwrap();
    let image = catalog
        .list(NativeTaskIntent::FirstFrameToVideoWithAudio)
        .pop()
        .unwrap();
    assert_eq!(text.identity.version_id, image.identity.version_id);
    assert_eq!(
        text.identity.component_binding_digest,
        image.identity.component_binding_digest
    );
    assert_ne!(text.identity.task_variant, image.identity.task_variant);
    let request = json!({"sessionId":"session","jobId":"job","parameters":{"task":"video","parameters":{"prompt":"An apple rolls to the right."}}});
    let prepared = catalog.prepare(&text.identity, request.clone()).unwrap();
    assert!(prepared.input_references().is_empty());
    assert!(catalog.prepare(&image.identity, request).is_err());
    assert!(catalog.list(NativeTaskIntent::TextToImage).is_empty());
}

#[test]
fn exact_version_and_components_cannot_be_reused_across_audio_adapters_or_versions() {
    let mut catalog = NativeTaskCatalog::default();
    for (version, profile) in [
        ("tts-1", AudioTaskProfile::PocketTtsEnglishCpu),
        ("asr-1", AudioTaskProfile::Qwen3Asr06bCpu),
        ("tts-2", AudioTaskProfile::PocketTtsEnglishCpu),
    ] {
        catalog.register_version(&audio(version, profile)).unwrap();
    }
    let speech = catalog.list(NativeTaskIntent::SpeechSynthesis);
    assert_eq!(speech.len(), 2);
    assert_eq!(catalog.list_matching(None).len(), 3);
    assert_eq!(
        catalog.list_matching(Some(NativeTaskIntent::SpeechSynthesis)),
        speech
    );
    assert_ne!(
        speech[0].identity.manifest_digest,
        speech[1].identity.manifest_digest
    );
    let text = catalog.list(NativeTaskIntent::Transcription).pop().unwrap();
    assert!(matches!(
        text.descriptor,
        NativeTaskParameterContract::Qwen3Asr(_)
    ));
    assert!(matches!(
        speech[0].descriptor,
        NativeTaskParameterContract::PocketTts(_)
    ));
    let mut stale = speech[0].identity.clone();
    stale.version_id.clone_from(&speech[1].identity.version_id);
    assert!(catalog.describe(&stale).is_err());
    assert!(catalog.prepare(&stale, json!({})).is_err());
    let mut wrong_schema = speech[0].identity.clone();
    wrong_schema.parameter_schema_id = text.identity.parameter_schema_id;
    assert!(catalog.describe(&wrong_schema).is_err());
}

#[test]
fn declared_capability_model_name_and_runtime_alone_cannot_supply_missing_content() {
    let mut changed = audio("tts-1", AudioTaskProfile::PocketTtsEnglishCpu);
    changed.manifest.plan.model_id = "PocketTTS English".into();
    let wrong = Sha256Digest::of_bytes(b"different weights same length");
    changed
        .manifest
        .installed_files
        .get_mut("artifact-0")
        .unwrap()
        .computed_sha256 = wrong.clone();
    changed
        .manifest
        .plan
        .artifacts
        .get_mut("artifact-0")
        .unwrap()
        .source_sha256 = Some(wrong);
    let mut catalog = NativeTaskCatalog::default();
    assert!(catalog.register_version(&changed).is_err());
    changed.manifest_digest = changed.manifest.digest().unwrap();
    assert!(changed.manifest.validate().is_err());
    assert!(catalog.register_version(&changed).is_err());
    assert!(catalog.list(NativeTaskIntent::SpeechSynthesis).is_empty());
}

#[test]
fn empty_managed_repository_yields_no_available_models_and_unknown_version_fails() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/native-task-catalog");
    std::fs::create_dir_all(&base).unwrap();
    let temp = tempfile::tempdir_in(base.canonicalize().unwrap()).unwrap();
    let repository = LifecycleRepository::open(&temp.path().join("models.db")).unwrap();
    assert!(NativeTaskCatalog::active(&repository)
        .unwrap()
        .list(NativeTaskIntent::SpeechSynthesis)
        .is_empty());
    assert!(
        NativeTaskCatalog::for_version(&repository, "installation", "missing-version").is_err()
    );
}
