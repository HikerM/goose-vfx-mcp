use super::*;
use serde_json::json;

impl GenerationProfile {
    pub fn required_capabilities(self) -> BTreeSet<ModelCapability> {
        if self.is_image() {
            BTreeSet::from([ModelCapability::ImageGeneration])
        } else if self.intent() == crate::lifecycle::NativeTaskIntent::TextToVideo {
            BTreeSet::from([ModelCapability::VideoGeneration])
        } else {
            BTreeSet::from([
                ModelCapability::VideoGeneration,
                ModelCapability::AudioGeneration,
            ])
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

    /// Older diffusion descriptors included a speech-only note in the hashed presentation text.
    pub fn matches_descriptor_digest(&self, expected: &Sha256Digest) -> anyhow::Result<bool> {
        if Sha256Digest::of_serializable(&self.descriptor)? == *expected {
            return Ok(true);
        }
        if !matches!(
            self.profile,
            GenerationProfile::Sd15CpuTextToImage
                | GenerationProfile::H3CudaTextToVideoAudio
                | GenerationProfile::H3CudaFirstFrameToVideoAudio
        ) {
            return Ok(false);
        }
        let mut old = self.descriptor.clone();
        old.limitations = [
            "Pinned model/Runtime-specific compiler; no general all-model support",
            "No managed generation resource envelope or execution owner exists yet; even smoke defaults cannot execute through this API",
            "A matching observed request is not quality acceptance, repeatability, release or runtime readiness",
            "PocketTTS language/speaking-rate/emotion controls are unsupported by its pinned session; speech needs its own voice-bound adapter, not diffusion parameters",
        ].map(str::to_owned).into();
        Ok(Sha256Digest::of_serializable(&old)? == *expected)
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
    let defaults = profile.defaults();
    // request.cpp::align_image_size uses VAE scale * diffusion down-factor:
    // SD1.5 = 8*8; H3 = 16*2. Reject instead of silently padding the user's size.
    let multiple = defaults.dimension_multiple;
    let mut fields = BTreeMap::new();
    let mut prompt = field("string", None, evidence);
    prompt.constraints = vec![
        "Required; at most 16384 UTF-8 bytes (service protocol limit)".into(),
        "NUL and implicit LoRA directives are rejected; assets must be explicitly bound".into(),
    ];
    fields.insert("prompt".into(), prompt);
    fields.insert(
        "negativePrompt".into(),
        field("string", Some(json!(defaults.negative_prompt)), evidence),
    );
    for (name, dimension) in [("width", defaults.width), ("height", defaults.height)] {
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
        json!({"kind":"value","value":defaults.seed}),
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
            json!(defaults.steps),
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
            json!(f64::from(defaults.cfg_scale)),
            0.,
            Some(f64::from(f32::MAX)),
            None,
            "scale",
            evidence,
        ),
    );
    let mut sampler = field("enum", Some(json!(defaults.samplers[0])), evidence);
    sampler.enum_values = defaults
        .samplers
        .iter()
        .map(|sampler| json!(sampler))
        .collect();
    sampler.constraints.push("Other native samplers are not bound by this adapter profile; quality outside the recorded request is unverified".into());
    fields.insert("sampler".into(), sampler);
    let mut scheduler = field("enum", Some(json!(defaults.schedulers[0])), evidence);
    scheduler.enum_values = defaults
        .schedulers
        .iter()
        .map(|value| json!(value))
        .collect();
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
        let timing = defaults.video.as_ref().expect("video timing");
        let mut frames = numeric(
            json!(timing.frames),
            f64::from(timing.minimum_frames),
            Some(f64::from(i32::MAX)),
            Some(f64::from(timing.frame_step)),
            "frames",
            evidence,
        );
        frames.constraints.push(format!(
            "frames = {} + {}*n; native align_video_frames would otherwise change the request",
            timing.minimum_frames, timing.frame_step
        ));
        fields.insert("frames".into(), frames);
        let mut fps = numeric(
            json!(timing.fps),
            f64::from(timing.supported_fps.first().copied().unwrap_or(timing.fps)),
            Some(f64::from(
                timing.supported_fps.last().copied().unwrap_or(timing.fps),
            )),
            Some(1.),
            "frames/second",
            evidence,
        );
        fps.constraints.push(timing.fps_constraint.clone());
        fps.enum_values = timing
            .supported_fps
            .iter()
            .map(|rate| json!(rate))
            .collect();
        fields.insert("fps".into(), fps);
        let mut duration = field("number", None, evidence);
        duration.unit = Some("seconds".into());
        let rate = if timing.supported_fps.is_empty() {
            timing.fps.to_string()
        } else {
            "fps".to_owned()
        };
        duration.constraints.push(format!("durationSeconds*{rate} must be an integral {}+{}*n frame count and equal explicit frames; no rounding",timing.minimum_frames,timing.frame_step));
        fields.insert("durationSeconds".into(), duration);
        let mut first = if profile.requires_first_frame() {
            field("session_media_reference", None, evidence)
        } else if profile.has_first_frame_variant() {
            unsupported("Use the separate first-frame-to-video-audio variant")
        } else {
            unsupported("This model has no registered first-frame input variant")
        };
        if profile.requires_first_frame() {
            first.constraints.push("Required same-session verified PNG/JPEG result, <=32MiB; owner must revalidate content SHA and decoded geometry".into());
        }
        fields.insert("firstFrame".into(), first);
        let mut audio = field("enum", Some(json!(timing.audio_mode)), evidence);
        audio.enum_values.push(json!(timing.audio_mode));
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
            "Execution requires the selected model and runtime to pass resource admission".into(),
            "A matching observed request is not quality acceptance, repeatability, release or runtime readiness".into(),
        ],
    }
}
