use super::*;
use serde_json::json;

const DIFFUSION_REVISION: &str = "c6dfa9def12fd0cb36ac7149ba30a75792bedbf0";
const CPU_BINARY: &str = "4f0090622e57c198c9fb6470f0981d5d5434830770c1aa0cf50d9ab8c8e2f043";
const CUDA_BINARY: &str = "11e555cb67afd5d480de2ca25e40addb0cbea70cca37822b92c98f7383a0a4b5";
pub(super) const SD_PROMPT: &str =
    "a photograph of a red apple on a wooden table, studio lighting, high quality";
pub(super) const SD_NEGATIVE: &str = "blurry, distorted, low quality";
pub(super) const H3_PROMPT: &str = "A single red apple on a wooden table slowly rolls to the right. Fixed camera, natural daylight. Soft rolling sound, no speech.";
pub(super) const FIRST_FRAME_SHA: &str =
    "2370e2963c2bbf8186f899684d7b60985f0196ac5c620c08c6e7522d1d49a034";

impl GenerationProfile {
    pub fn required_capabilities(self) -> BTreeSet<ModelCapability> {
        if self.is_image() {
            BTreeSet::from([ModelCapability::ImageGeneration])
        } else {
            BTreeSet::from([
                ModelCapability::VideoGeneration,
                ModelCapability::AudioGeneration,
            ])
        }
    }

    pub fn task_variant(self) -> &'static str {
        match self {
            Self::Sd15CpuTextToImage => "sd15/text_to_image",
            Self::H3CudaTextToVideoAudio => "minimax_h3/text_to_video_audio",
            Self::H3CudaFirstFrameToVideoAudio => "minimax_h3/first_frame_to_video_audio",
        }
    }

    pub fn parameter_schema_id(self) -> &'static str {
        if self.is_image() {
            "sd15-image-parameters-v1"
        } else {
            "minimax-h3-video-parameters-v1"
        }
    }

    pub fn runtime_build(self) -> RuntimeBuild {
        RuntimeBuild {
            adapter_id: "diffusion-cpp-native-generation-v1".into(),
            engine_revision: DIFFUSION_REVISION.into(),
            binding_version: self.parameter_schema_id().into(),
            platform: "windows-x86_64".into(),
            features: BTreeSet::from([if self.is_image() {
                "cpu"
            } else {
                "cuda-sm120a"
            }
            .into()]),
            binary_sha256: Sha256Digest::parse(if self.is_image() {
                CPU_BINARY
            } else {
                CUDA_BINARY
            })
            .expect("pinned digest"),
        }
    }

    /// Recorded identities are allowlists, not assertions that these files are installed.
    pub fn recorded_assets(self) -> BTreeMap<GenerationAssetRole, (Sha256Digest, u64)> {
        let entries: &[(GenerationAssetRole, &str, u64)] = if self.is_image() {
            &[(
                GenerationAssetRole::Main,
                "30a85c4cd39aaac08e5ddc51edc22a1ba923f2f8ba131e898cf15ae415ee5524",
                2_132_584_864,
            )]
        } else {
            &[
                (
                    GenerationAssetRole::Main,
                    "dd948e08ad0ba3c71bd42f368e283dd82e790f5122a63b276e22a3e0283d0c10",
                    11_420_663_904,
                ),
                (
                    GenerationAssetRole::TextEncoder,
                    "11e6efe70a57ce7f4838c47bdbd1a1c4b8ce10e2b7747f1b065990b70f4b05fc",
                    18_218_065_024,
                ),
                (
                    GenerationAssetRole::VideoVae,
                    "2806d06d195d41a351a4a838125e9ddea09abb2770e1ef6bb9168409d1d393d9",
                    5_207_785_056,
                ),
                (
                    GenerationAssetRole::AudioVae,
                    "45764b28074abab09ec9fa1447b3eed3113b2e6878c177bab5bce0572064edf4",
                    302_904_896,
                ),
            ]
        };
        entries
            .iter()
            .map(|(role, sha, size)| {
                (
                    *role,
                    (Sha256Digest::parse(*sha).expect("pinned digest"), *size),
                )
            })
            .collect()
    }

    pub(super) fn is_image(self) -> bool {
        self == Self::Sd15CpuTextToImage
    }
    pub(super) fn requires_first_frame(self) -> bool {
        self == Self::H3CudaFirstFrameToVideoAudio
    }
    pub(super) fn evidence(self) -> &'static str {
        if self.is_image() {
            "2026-11-01-native-image-feasibility.json"
        } else {
            "2026-11-12-h3-native-video-smoke.json"
        }
    }

    pub(super) fn engine(self) -> FrozenGenerationEngine {
        FrozenGenerationEngine {
            cpu_threads: if self.is_image() { 12 } else { 8 },
            backend: if self.is_image() {
                "cpu"
            } else {
                "diffusion=cuda0,te=cpu,vae=cpu"
            }
            .into(),
            parameters_backend: "cpu".into(),
            rng: "cpu".into(),
            sampler_rng: "cpu".into(),
            flash_attention: "auto".into(),
            cache_mode: "disabled".into(),
            flow_shift: (!self.is_image()).then_some(12),
            max_vram_gib: (!self.is_image()).then_some(8),
            disable_prefetch: !self.is_image(),
            output_format: if self.is_image() { "png" } else { "avi" }.into(),
        }
    }
}

impl GenerationParameterService {
    pub fn new(
        profile: GenerationProfile,
        binding: GenerationModelBinding,
    ) -> GenerationParameterResult<Self> {
        if binding.installation_id.trim().is_empty() || binding.version_id.trim().is_empty() {
            return Err(issue(
                "binding",
                "missing_identity",
                "Installation and version are required",
            ));
        }
        if binding.runtime != profile.runtime_build() {
            return Err(issue(
                "binding.runtime",
                "unsupported_runtime",
                "This model profile requires its exact recorded Runtime build",
            ));
        }
        if !profile
            .required_capabilities()
            .is_subset(&binding.target_capabilities)
        {
            return Err(issue(
                "binding.targetCapabilities",
                "capability_mismatch",
                "Installation does not declare the capabilities required by this task variant",
            ));
        }
        let expected = profile.recorded_assets();
        if binding.assets.len() != expected.len()
            || expected.iter().any(|(role, (sha, size))| {
                binding.assets.get(role).is_none_or(|asset| {
                    asset.sha256 != *sha
                        || asset.size_bytes != *size
                        || asset.artifact_id.trim().is_empty()
                })
            })
        {
            return Err(issue(
                "binding.assets",
                "unsupported_components",
                "Component roles, hashes and sizes must match this model profile",
            ));
        }
        let ids: BTreeSet<_> = binding
            .assets
            .values()
            .map(|asset| &asset.artifact_id)
            .collect();
        if ids.len() != binding.assets.len() {
            return Err(issue(
                "binding.assets",
                "duplicate_artifact",
                "Distinct components require distinct artifacts",
            ));
        }
        let descriptor = descriptor(profile, binding.clone());
        Ok(Self {
            profile,
            binding,
            descriptor,
        })
    }

    pub fn descriptor(&self) -> &GenerationParameterDescriptor {
        &self.descriptor
    }
}

fn field(kind: &str, default: Option<Value>, evidence: &str) -> GenerationFieldDescriptor {
    GenerationFieldDescriptor {
        value_type: kind.into(),
        support: GenerationParameterSupport::NativeSupported,
        default_source: default.as_ref().map(|_| evidence.into()),
        default,
        unit: None,
        minimum: None,
        maximum: None,
        step: None,
        enum_values: Vec::new(),
        constraints: Vec::new(),
    }
}

fn numeric(
    default: Value,
    minimum: f64,
    maximum: Option<f64>,
    step: Option<f64>,
    unit: &str,
    evidence: &str,
) -> GenerationFieldDescriptor {
    let mut value = field(
        if step.is_some() { "integer" } else { "number" },
        Some(default),
        evidence,
    );
    value.minimum = Some(minimum);
    value.maximum = maximum;
    value.step = step;
    value.unit = Some(unit.into());
    value.constraints.push("Bounds describe accepted native/protocol values, not a safe resource envelope; resource admission is required".into());
    value
}

fn unsupported(reason: &str) -> GenerationFieldDescriptor {
    let mut value = field("unavailable", None, "");
    value.support = GenerationParameterSupport::Unsupported;
    value.constraints.push(reason.into());
    value
}

fn descriptor(
    profile: GenerationProfile,
    binding: GenerationModelBinding,
) -> GenerationParameterDescriptor {
    let evidence = profile.evidence();
    let image = profile.is_image();
    let dimension = if image { 512 } else { 256 };
    // request.cpp::align_image_size uses VAE scale * diffusion down-factor:
    // SD1.5 = 8*8; H3 = 16*2. Reject instead of silently padding the user's size.
    let multiple = if image { 64 } else { 32 };
    let mut fields = BTreeMap::new();
    let mut prompt = field("string", None, evidence);
    prompt.constraints = vec![
        "Required; at most 16384 UTF-8 bytes (service protocol limit)".into(),
        "NUL and implicit LoRA directives are rejected; assets must be explicitly bound".into(),
    ];
    fields.insert("prompt".into(), prompt);
    fields.insert(
        "negativePrompt".into(),
        field(
            "string",
            Some(json!(if image { SD_NEGATIVE } else { "" })),
            evidence,
        ),
    );
    for name in ["width", "height"] {
        fields.insert(
            name.into(),
            numeric(
                json!(dimension),
                f64::from(multiple),
                Some(f64::from(i32::MAX)),
                Some(f64::from(multiple)),
                "pixels",
                evidence,
            ),
        );
        fields.get_mut(name).expect("dimension descriptor").constraints.push(
            "width*height*3 must fit the native signed 32-bit RGB buffer calculation; this is not a memory admission".into(),
        );
    }
    let mut ratio = field("ratio", None, evidence);
    ratio
        .constraints
        .push("Constraint on width/height; does not choose an implicit resolution".into());
    fields.insert("aspectRatio".into(), ratio);
    let mut seed = numeric(
        json!({"kind":"value","value":42}),
        0.,
        Some(f64::from(u32::MAX)),
        Some(1.),
        "seed",
        evidence,
    );
    seed.value_type = "seed_choice".into();
    seed.constraints.push(
        "Explicit new generates and records a concrete seed; reuse inherits the previous seed"
            .into(),
    );
    fields.insert("seed".into(), seed);
    fields.insert(
        "steps".into(),
        numeric(
            json!(if image { 20 } else { 8 }),
            1.,
            Some(f64::from(i32::MAX)),
            Some(1.),
            "steps",
            evidence,
        ),
    );
    fields.insert(
        "cfgScale".into(),
        numeric(
            json!(if image { 7.0 } else { 1.0 }),
            0.,
            Some(f64::from(f32::MAX)),
            None,
            "scale",
            evidence,
        ),
    );
    let mut sampler = field(
        "enum",
        Some(json!(if image { "euler_a" } else { "euler" })),
        evidence,
    );
    sampler.enum_values = if image {
        vec![json!("euler_a"), json!("euler")]
    } else {
        vec![json!("euler")]
    };
    sampler.constraints.push("Other native samplers are not bound by this adapter profile; quality outside the recorded request is unverified".into());
    fields.insert("sampler".into(), sampler);
    let mut scheduler = field("enum", Some(json!("discrete")), evidence);
    scheduler.enum_values.push(json!("discrete"));
    fields.insert("scheduler".into(), scheduler);
    if image {
        fields.insert(
            "batchCount".into(),
            numeric(
                json!(1),
                1.,
                Some(f64::from(i32::MAX)),
                Some(1.),
                "images",
                evidence,
            ),
        );
        fields.insert(
            "referenceImage".into(),
            unsupported(
                "This profile is text-to-image; img2img requires a separately bound task variant",
            ),
        );
        fields.insert(
            "strength".into(),
            unsupported("Requires a verified image-to-image variant and input binding"),
        );
        fields.insert(
            "mask".into(),
            unsupported("No inpainting model/variant is bound"),
        );
    } else {
        let mut frames = numeric(
            json!(22),
            5.,
            Some(f64::from(i32::MAX)),
            Some(17.),
            "frames",
            evidence,
        );
        frames.constraints.push(
            "frames = 5 + 17*n; native align_video_frames would otherwise change the request"
                .into(),
        );
        fields.insert("frames".into(), frames);
        let mut fps = numeric(
            json!(24),
            24.,
            Some(24.),
            Some(1.),
            "frames/second",
            evidence,
        );
        fps.constraints
            .push("H3 overrides all other FPS values; this adapter rejects them".into());
        fields.insert("fps".into(), fps);
        let mut duration = field("number", None, evidence);
        duration.unit = Some("seconds".into());
        duration.constraints.push("durationSeconds*24 must be an integral 5+17*n frame count and equal explicit frames; no rounding".into());
        fields.insert("durationSeconds".into(), duration);
        let mut first = if profile.requires_first_frame() {
            field("session_media_reference", None, evidence)
        } else {
            unsupported("Use the separate first-frame-to-video-audio variant")
        };
        if profile.requires_first_frame() {
            first.constraints.push("Required same-session verified PNG/JPEG result, <=32MiB; owner must revalidate content SHA and decoded geometry".into());
        }
        fields.insert("firstFrame".into(), first);
        let mut audio = field("enum", Some(json!("joint_audio_video")), evidence);
        audio.enum_values.push(json!("joint_audio_video"));
        fields.insert("audioMode".into(), audio);
        fields.insert(
            "endFrame".into(),
            unsupported("Not bound or validated by this task variant"),
        );
        fields.insert(
            "referenceVideo".into(),
            unsupported("Not bound or validated by this task variant"),
        );
        fields.insert(
            "batchCount".into(),
            unsupported("Native video request does not carry image batch_count"),
        );
    }
    GenerationParameterDescriptor {
        schema_version: GENERATION_PARAMETER_SCHEMA, profile, binding, fields,
        execution_status: GenerationExecutionStatus::NeedsResourceAdmission, evidence: evidence.into(),
        limitations: vec![
            "Pinned model/Runtime-specific compiler; no general all-model support".into(),
            "No managed generation resource envelope or execution owner exists yet; even smoke defaults cannot execute through this API".into(),
            "A matching observed request is not quality acceptance, repeatability, release or runtime readiness".into(),
            "PocketTTS language/speaking-rate/emotion controls are unsupported by its pinned session; speech needs its own voice-bound adapter, not diffusion parameters".into(),
        ],
    }
}
