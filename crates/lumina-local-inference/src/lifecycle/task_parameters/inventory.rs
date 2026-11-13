use super::*;
use crate::lifecycle::NativeTaskIntent;
use std::sync::LazyLock;

mod external;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct WeightSource {
    pub role: GenerationAssetRole,
    pub repository: String,
    pub revision: String,
    pub file: String,
    pub sha256: Sha256Digest,
    pub size_bytes: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Defaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_shift: Option<u32>,
    pub width: u32,
    pub height: u32,
    pub dimension_multiple: u32,
    pub negative_prompt: String,
    pub steps: u32,
    pub cfg_scale: f32,
    pub seed: u32,
    pub samplers: Vec<GenerationSampler>,
    pub schedulers: Vec<GenerationScheduler>,
    pub video: Option<VideoDefaults>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct VideoDefaults {
    pub frames: u32,
    pub minimum_frames: u32,
    pub frame_step: u32,
    pub fps: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_fps: Vec<u32>,
    pub audio_mode: GenerationAudioMode,
    pub fps_constraint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Variant {
    intent: NativeTaskIntent,
    task_variant: String,
    observed_parameters: Option<EffectiveGenerationParameters>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Model {
    adapter_protocol: String,
    variants: BTreeMap<String, Variant>,
    install_profile: String,
    parameter_schema_id: String,
    defaults: Defaults,
    evidence: String,
    assets: Vec<WeightSource>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Protocol {
    runtime: RuntimeBuild,
    engine: FrozenGenerationEngine,
    arguments: BTreeMap<GenerationAssetRole, String>,
    intents: Vec<NativeTaskIntent>,
    audio_mode: Option<GenerationAudioMode>,
}

static PROTOCOLS: LazyLock<BTreeMap<String, Protocol>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("protocols.json")).expect("invalid diffusion protocols")
});

impl Model {
    fn asset_identity(&self) -> BTreeMap<GenerationAssetRole, (Sha256Digest, u64)> {
        self.assets
            .iter()
            .map(|s| (s.role, (s.sha256.clone(), s.size_bytes)))
            .collect()
    }
}

static MODELS: LazyLock<BTreeMap<String, Model>> = LazyLock::new(|| {
    let mut models: BTreeMap<String, Model> = serde_json::from_str(include_str!("models.json"))
        .expect("invalid bundled generation model inventory");
    let mut ids = BTreeSet::new();
    for model in models.values() {
        let protocol = &PROTOCOLS[&model.adapter_protocol];
        assert_eq!(
            protocol.audio_mode,
            model.defaults.video.as_ref().map(|v| v.audio_mode)
        );
        assert_eq!(
            protocol.arguments.keys().copied().collect::<BTreeSet<_>>(),
            model.assets.iter().map(|s| s.role).collect()
        );
        assert!(model
            .variants
            .values()
            .all(|v| protocol.intents.contains(&v.intent)));
        assert!(model.variants.contains_key(&model.install_profile));
        assert_eq!(
            protocol.engine.output_format == GenerationOutputFormat::Avi,
            model.defaults.video.is_some()
        );
        assert!(model
            .variants
            .keys()
            .all(|variant| ids.insert(variant.as_str())));
        assert!(model.defaults.dimension_multiple > 0);
        for names in [
            model
                .defaults
                .samplers
                .iter()
                .map(|s| s.native_name())
                .collect::<Vec<_>>(),
            model
                .defaults
                .schedulers
                .iter()
                .map(|s| s.native_name())
                .collect::<Vec<_>>(),
        ] {
            assert!(
                !names.is_empty() && names.iter().collect::<BTreeSet<_>>().len() == names.len()
            );
        }
        for variant in model.variants.values() {
            if let Some(observed) = &variant.observed_parameters {
                let (sampler, scheduler) = match observed {
                    EffectiveGenerationParameters::Image(p) => (p.sampler, p.scheduler),
                    EffectiveGenerationParameters::Video(p) => (p.sampler, p.scheduler),
                };
                assert!(model.defaults.samplers.contains(&sampler));
                assert!(model.defaults.schedulers.contains(&scheduler));
            }
            match variant.intent {
                NativeTaskIntent::TextToImage => assert!(model.defaults.video.is_none()),
                NativeTaskIntent::TextToVideo
                | NativeTaskIntent::TextToVideoWithAudio
                | NativeTaskIntent::FirstFrameToVideoWithAudio => {
                    let video = model.defaults.video.as_ref().expect("missing video timing");
                    assert!(
                        video.fps > 0
                            && video.frame_step > 0
                            && video.frames >= video.minimum_frames
                    );
                    assert_eq!(
                        video.audio_mode == GenerationAudioMode::VideoOnly,
                        variant.intent == NativeTaskIntent::TextToVideo
                    );
                    assert_eq!((video.frames - video.minimum_frames) % video.frame_step, 0);
                }
                _ => panic!("unsupported diffusion execution protocol"),
            }
        }
    }
    if let Some(path) = std::env::var_os("LUMINA_GENERATION_CATALOG") {
        match external::load(std::path::Path::new(&path), &models) {
            Ok(additions) => models.extend(additions),
            Err(error) => tracing::error!("Cannot load configured generation catalog: {error:#}"),
        }
    }
    models
});

/// The serialized identifier is stable across releases and always resolves to a trusted entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct GenerationProfile(&'static str);

impl<'de> Deserialize<'de> for GenerationProfile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        Self::registered()
            .find(|profile| profile.0 == key)
            .ok_or_else(|| serde::de::Error::custom("Unknown generation profile"))
    }
}

#[allow(non_upper_case_globals)]
impl GenerationProfile {
    pub const Sd15CpuTextToImage: Self = Self("sd15_cpu_text_to_image");
    pub const H3CudaTextToVideoAudio: Self = Self("h3_cuda_text_to_video_audio");
    pub const H3CudaFirstFrameToVideoAudio: Self = Self("h3_cuda_first_frame_to_video_audio");

    pub fn registered() -> impl Iterator<Item = Self> {
        MODELS
            .values()
            .flat_map(|model| model.variants.keys().map(|key| Self(key.as_str())))
    }

    pub fn installation_profile(name: &str) -> Option<Self> {
        MODELS.get(name).map(|model| Self(&model.install_profile))
    }

    pub fn installation_names() -> impl Iterator<Item = &'static str> {
        MODELS.keys().map(String::as_str)
    }

    pub fn task_variants(self) -> impl Iterator<Item = Self> {
        self.model().variants.keys().map(|name| Self(name.as_str()))
    }

    fn model(self) -> &'static Model {
        MODELS
            .values()
            .find(|model| model.variants.contains_key(self.0))
            .expect("registered generation profile")
    }

    pub fn intent(self) -> NativeTaskIntent {
        self.model().variants[self.0].intent
    }
    pub fn task_variant(self) -> &'static str {
        &self.model().variants[self.0].task_variant
    }
    pub fn parameter_schema_id(self) -> &'static str {
        &self.model().parameter_schema_id
    }
    pub fn runtime_build(self) -> RuntimeBuild {
        PROTOCOLS[&self.model().adapter_protocol].runtime.clone()
    }
    pub(super) fn engine(self) -> FrozenGenerationEngine {
        let mut engine = PROTOCOLS[&self.model().adapter_protocol].engine.clone();
        engine.flow_shift = self.defaults().flow_shift;
        engine
    }
    pub(super) fn component_argument(self, role: GenerationAssetRole) -> &'static str {
        &PROTOCOLS[&self.model().adapter_protocol].arguments[&role]
    }
    pub(super) fn observed_parameters(self) -> Option<&'static EffectiveGenerationParameters> {
        self.model().variants[self.0].observed_parameters.as_ref()
    }
    pub(super) fn defaults(self) -> &'static Defaults {
        &self.model().defaults
    }
    pub(in crate::lifecycle) fn evidence(self) -> &'static str {
        &self.model().evidence
    }
    pub(super) fn weight_sources(self) -> &'static [WeightSource] {
        &self.model().assets
    }
    pub(super) fn is_external(self) -> bool {
        self.0.starts_with("external_")
    }
    pub(super) fn is_image(self) -> bool {
        self.intent() == NativeTaskIntent::TextToImage
    }
    pub(super) fn has_first_frame_variant(self) -> bool {
        self.model()
            .variants
            .values()
            .any(|v| v.intent == NativeTaskIntent::FirstFrameToVideoWithAudio)
    }

    pub(super) fn requires_first_frame(self) -> bool {
        self.intent() == NativeTaskIntent::FirstFrameToVideoWithAudio
    }

    pub fn recorded_assets(self) -> BTreeMap<GenerationAssetRole, (Sha256Digest, u64)> {
        self.model().asset_identity()
    }
}
