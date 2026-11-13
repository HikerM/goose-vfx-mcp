use super::super::compile;
use super::*;
use crate::lifecycle::{ModelScopeRepository, RepositoryPath};
use anyhow::{ensure, Context, Result};
use std::{fs::File, io::Read, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Catalog {
    schema_version: u32,
    models: Vec<Definition>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Definition {
    name: String,
    adapter_protocol: String,
    intents: Vec<NativeTaskIntent>,
    defaults: Defaults,
    assets: BTreeMap<GenerationAssetRole, Asset>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Asset {
    repository: ModelScopeRepository,
    revision: String,
    file: RepositoryPath,
    sha256: Sha256Digest,
    size_bytes: u64,
}

pub(super) fn load(
    path: &Path,
    bundled: &BTreeMap<String, Model>,
) -> Result<BTreeMap<String, Model>> {
    const LIMIT: u64 = 2 * 1024 * 1024;
    ensure!(
        path.is_absolute(),
        "LUMINA_GENERATION_CATALOG requires an absolute path"
    );
    let file = File::open(path)
        .with_context(|| format!("Opening generation catalog {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "Expected a regular generation catalog file"
    );
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LIMIT,
        "Generation catalog exceeds 2 MiB"
    );
    let catalog: Catalog =
        serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))?;
    ensure!(
        catalog.schema_version == 1 && catalog.models.len() <= 256,
        "Invalid generation catalog version or model count"
    );
    let mut additions: BTreeMap<String, Model> = BTreeMap::new();
    for definition in catalog.models {
        let name = definition.name.clone();
        ensure!(
            !name.is_empty()
                && name.len() <= 96
                && name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "Invalid generation installation name"
        );
        ensure!(
            !bundled.contains_key(&name) && !additions.contains_key(&name),
            "Duplicate generation installation name {name}"
        );
        let adapter = PROTOCOLS
            .get(&definition.adapter_protocol)
            .with_context(|| format!("Unknown generation adapter protocol for {name}"))?;
        let model = definition
            .bind(adapter)
            .with_context(|| format!("Invalid model definition {name}"))?;
        ensure!(
            !bundled
                .values()
                .chain(additions.values())
                .any(
                    |other| PROTOCOLS[&other.adapter_protocol].runtime == adapter.runtime
                        && other.asset_identity() == model.asset_identity()
                ),
            "Ambiguous generation component binding for {name}"
        );
        additions.insert(name, model);
    }
    Ok(additions)
}

impl Definition {
    fn bind(self, adapter: &Protocol) -> Result<Model> {
        let defaults = &self.defaults;
        ensure!(
            defaults.dimension_multiple > 0,
            "Expected positive dimension alignment"
        );
        compile::geometry(
            defaults.width,
            defaults.height,
            defaults.dimension_multiple,
            None,
        )?;
        defaults.validate_step_range()?;
        compile::cfg(defaults.cfg_scale)?;
        compile::text("negativePrompt", &defaults.negative_prompt, false)?;
        ensure!(
            !defaults.samplers.is_empty()
                && !defaults.schedulers.is_empty()
                && defaults
                    .samplers
                    .iter()
                    .map(|s| s.native_name())
                    .collect::<BTreeSet<_>>()
                    .len()
                    == defaults.samplers.len()
                && defaults
                    .schedulers
                    .iter()
                    .map(|s| s.native_name())
                    .collect::<BTreeSet<_>>()
                    .len()
                    == defaults.schedulers.len(),
            "Expected nonempty distinct sampler and scheduler options"
        );
        ensure!(
            defaults.video.as_ref().map(|v| v.audio_mode) == adapter.audio_mode,
            "Video mode differs from the adapter protocol"
        );
        if let Some(v) = &defaults.video {
            let valid = 1..=i32::MAX as u32;
            ensure!(
                valid.contains(&v.minimum_frames)
                    && valid.contains(&v.frame_step)
                    && valid.contains(&v.frames)
                    && v.frames >= v.minimum_frames
                    && (v.frames - v.minimum_frames) % v.frame_step == 0
                    && valid.contains(&v.fps)
                    && v.supported_fps.iter().all(|f| valid.contains(f))
                    && v.supported_fps.windows(2).all(|rates| rates[0] < rates[1])
                    && (v.supported_fps.is_empty() || v.supported_fps.contains(&v.fps)),
                "Invalid video timing or supported FPS list"
            );
        }
        let digest = Sha256Digest::of_serializable(&(
            &self,
            &adapter.runtime,
            &adapter.engine,
            &adapter.arguments,
        ))?;
        ensure!(!self.intents.is_empty(), "A task intent is required");
        let mut variants = BTreeMap::new();
        let mut install_profile = None;
        for intent in self.intents {
            ensure!(
                adapter.intents.contains(&intent),
                "Intent outside the generation adapter protocol"
            );
            let tag = serde_json::to_value(intent)?;
            let tag = tag.as_str().context("Expected a task intent tag")?;
            let key = format!("external_{}_{tag}", digest.as_str());
            install_profile.get_or_insert_with(|| key.clone());
            ensure!(
                variants
                    .insert(
                        key,
                        Variant {
                            intent,
                            task_variant: format!("{}/{tag}", self.name),
                            observed_parameters: None,
                        }
                    )
                    .is_none(),
                "Duplicate generation intent"
            );
        }
        ensure!(
            self.assets.len() == adapter.arguments.len(),
            "Unexpected number of component roles"
        );
        let mut sources = self.assets;
        let mut assets = Vec::new();
        let mut hashes = BTreeSet::new();
        let mut total = 0u64;
        for &role in adapter.arguments.keys() {
            let asset = sources
                .remove(&role)
                .context("Missing required generation component role")?;
            ensure!(
                asset.revision.len() == 40
                    && asset.revision.bytes().all(|b| b.is_ascii_hexdigit())
                    && asset.file.as_str().to_ascii_lowercase().ends_with(".gguf")
                    && asset.size_bytes > 0
                    && hashes.insert(asset.sha256.clone()),
                "Expected distinct, revision-pinned GGUF components"
            );
            total = total
                .checked_add(asset.size_bytes)
                .context("Component size overflow")?;
            assets.push(WeightSource {
                role,
                repository: asset.repository.as_str().into(),
                revision: asset.revision.to_ascii_lowercase(),
                file: asset.file.as_str().into(),
                sha256: asset.sha256,
                size_bytes: asset.size_bytes,
            });
        }
        ensure!(
            total <= (u64::MAX - 16 * 1024 * 1024) / 3,
            "Component budget overflow"
        );
        Ok(Model {
            adapter_protocol: self.adapter_protocol,
            variants,
            install_profile: install_profile.context("Missing installation intent")?,
            parameter_schema_id: format!("generation-profile-{}", digest.as_str()),
            defaults: self.defaults,
            evidence: format!(
                "Operator-declared generation profile {}; execution unmeasured",
                digest.as_str()
            ),
            assets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn image_definition() -> Value {
        json!({"name":"new-checkpoint","adapterProtocol":"sd-cpp/checkpoint-image-cpu-v1",
            "intents":["text_to_image"],"defaults":{"width":512,"height":512,"dimensionMultiple":64,
                "steps":4,"cfgScale":1.0,"negativePrompt":"","seed":0,"samplers":["euler"],"schedulers":["discrete"]},
            "assets":{"main":{"repository":"example/weights","revision":"a".repeat(40),"file":"main.gguf",
                "sizeBytes":1024,"sha256":"1".repeat(64)}}})
    }

    fn load_models(definitions: Vec<Value>) -> Result<BTreeMap<String, Model>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("catalog.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"schemaVersion":1, "models":definitions}))?,
        )?;
        load(
            &path,
            &serde_json::from_str(include_str!("../models.json"))?,
        )
    }

    #[test]
    fn parameter_edits_change_external_identity_and_no_definition_can_supply_execution_evidence() {
        let first = image_definition();
        let mut second = first.clone();
        second["defaults"]["steps"] = json!(7);
        let a = load_models(vec![first]).unwrap();
        let b = load_models(vec![second]).unwrap();
        let old = &a["new-checkpoint"];
        let new = &b["new-checkpoint"];
        assert_ne!(old.parameter_schema_id, new.parameter_schema_id);
        assert_ne!(old.install_profile, new.install_profile);
        assert!(old
            .variants
            .values()
            .all(|v| v.observed_parameters.is_none()));
    }

    #[test]
    fn rejects_invalid_bounds_and_defaults_and_preserves_unspecified_ranges() {
        let first = image_definition();
        let original: Definition = serde_json::from_value(first.clone()).unwrap();
        let serialized = serde_json::to_value(&original.defaults).unwrap();
        assert!(serialized.get("minimumSteps").is_none());
        assert!(serialized.get("maximumSteps").is_none());
        assert_eq!(original.defaults.step_range(), 1..=i32::MAX as u32);
        let original_id = load_models(vec![first.clone()]).unwrap()["new-checkpoint"]
            .parameter_schema_id
            .clone();
        assert_eq!(
            original_id,
            "generation-profile-87af1944ed3bc0e5296e5f7fc9421accc6ac4af6f42ab7c46c261f923f979459"
        );
        for (minimum, maximum) in [
            (Some(0), None),
            (None, Some(0)),
            (Some(5), None),
            (None, Some(3)),
            (Some(8), Some(4)),
            (None, Some(i32::MAX as u32 + 1)),
        ] {
            let mut row = first.clone();
            row["defaults"]["minimumSteps"] = json!(minimum);
            row["defaults"]["maximumSteps"] = json!(maximum);
            assert!(load_models(vec![row]).is_err());
        }
        for (minimum, maximum) in [(Some(2), None), (None, Some(6)), (Some(4), Some(4))] {
            let mut row = first.clone();
            row["defaults"]["minimumSteps"] = json!(minimum);
            row["defaults"]["maximumSteps"] = json!(maximum);
            let model = load_models(vec![row])
                .unwrap()
                .remove("new-checkpoint")
                .unwrap();
            assert_ne!(model.parameter_schema_id, original_id);
            assert_eq!(
                model.defaults.step_range(),
                minimum.unwrap_or(1)..=maximum.unwrap_or(i32::MAX as u32)
            );
        }
    }

    #[test]
    fn two_checkpoint_models_describe_and_compile_their_own_step_bounds() {
        // The catalog is process-scoped. Each child starts with the specified operator catalog.
        const CHILD: &str = "LUMINA_STEP_RANGE_FIXTURE";
        if std::env::var_os(CHILD).is_none() {
            let mut short = image_definition();
            short["defaults"]["minimumSteps"] = json!(2);
            short["defaults"]["maximumSteps"] = json!(5);
            let mut long = image_definition();
            long["name"] = json!("long-checkpoint");
            long["assets"]["main"]["sha256"] = json!("2".repeat(64));
            long["defaults"]["steps"] = json!(20);
            long["defaults"]["minimumSteps"] = json!(10);
            long["defaults"]["maximumSteps"] = json!(30);
            let dir = tempfile::tempdir().unwrap();
            let catalog = dir.path().join("catalog.json");
            std::fs::write(
                &catalog,
                serde_json::to_vec(&json!({"schemaVersion":1,"models":[short,long]})).unwrap(),
            )
            .unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "two_checkpoint_models_describe_and_compile_their_own_step_bounds",
                    "--nocapture",
                ])
                .env("LUMINA_GENERATION_CATALOG", catalog)
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                child.status.success()
                    && String::from_utf8_lossy(&child.stdout).contains("1 passed"),
                "{} {}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            return;
        }
        for (name, default, lo, hi) in [
            ("new-checkpoint", 4u32, 2u32, 5u32),
            ("long-checkpoint", 20, 10, 30),
        ] {
            let profile = GenerationProfile::installation_profile(name).unwrap();
            let service = GenerationParameterService::new(
                profile,
                GenerationModelBinding {
                    installation_id: name.into(),
                    version_id: "fixture".into(),
                    manifest_digest: Sha256Digest::of_bytes(name.as_bytes()),
                    runtime: profile.runtime_build(),
                    target_capabilities: profile.required_capabilities(),
                    assets: profile
                        .recorded_assets()
                        .into_iter()
                        .map(|(role, (sha256, size_bytes))| {
                            (
                                role,
                                GenerationAssetBinding {
                                    artifact_id: "fixture".into(),
                                    sha256,
                                    size_bytes,
                                },
                            )
                        })
                        .collect(),
                },
            )
            .unwrap();
            let step = &service.descriptor().fields["steps"];
            assert_eq!(
                (step.minimum, step.maximum, step.default.as_ref()),
                (Some(lo.into()), Some(hi.into()), Some(&json!(default)))
            );
            let request = |steps: Option<u32>| GenerationTaskRequest {
                session_id: "session".into(),
                job_id: "job".into(),
                parameters: GenerationParameters::Image(DiffusionImageParameters {
                    prompt: Some("cup".into()),
                    steps,
                    ..Default::default()
                }),
            };
            for (given, expected) in [(None, default), (Some(lo), lo), (Some(hi), hi)] {
                let task = service.compile(request(given)).unwrap();
                assert!(task.arguments().windows(2).any(|pair| pair
                    == [
                        NativeGenerationArgument::Literal("--steps".into()),
                        NativeGenerationArgument::Literal(expected.to_string())
                    ]));
            }
            for bad in [lo - 1, hi + 1] {
                let error = service.compile(request(Some(bad))).unwrap_err();
                assert_eq!(
                    (&*error.issues[0].field, &*error.issues[0].code),
                    ("steps", "out_of_range")
                );
            }
        }
    }

    #[test]
    fn catalog_rejects_ambiguous_assets_unknown_protocols_and_invalid_options() {
        let first = image_definition();
        let mut alias = first.clone();
        alias["name"] = json!("another-name");
        assert!(load_models(vec![first.clone(), alias]).is_err());
        for (pointer, invalid) in [
            ("/name", json!("sd15-cpu")),
            ("/adapterProtocol", json!("unregistered")),
            ("/defaults/dimensionMultiple", json!(0)),
            ("/defaults/samplers", json!([])),
            ("/intents", json!(["text_to_video"])),
            ("/assets/main/file", json!("../bad.gguf")),
            ("/assets/main/revision", json!("main")),
            ("/assets/main/sizeBytes", json!(u64::MAX)),
        ] {
            let mut row = first.clone();
            *row.pointer_mut(pointer).unwrap() = invalid;
            assert!(load_models(vec![row]).is_err(), "{pointer}");
        }
        for key in ["argument", "runtime", "observedParameters"] {
            let mut row = first.clone();
            row[key] = json!("untrusted");
            assert!(load_models(vec![row]).is_err(), "{key}");
        }
    }
}
