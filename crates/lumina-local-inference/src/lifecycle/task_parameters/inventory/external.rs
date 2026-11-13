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
        compile::integer("steps", defaults.steps, 1, 1, 0)?;
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
