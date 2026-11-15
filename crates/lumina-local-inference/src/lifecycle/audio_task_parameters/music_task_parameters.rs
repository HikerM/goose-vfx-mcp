use super::reuse::{
    change_between, child_metadata, compare_parent, invalid_parent, old_value, validate_metadata,
    validate_project_derivation,
};
use super::services::{
    bounded_int, common_arguments, identity, literal, metadata, native_text, parse_request,
    profile_for_binding, validate_protocol,
};
use super::*;
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StableAudioSampler {
    Pingpong,
    Euler,
}

impl StableAudioSampler {
    fn native(self) -> &'static str {
        match self {
            Self::Pingpong => "pingpong",
            Self::Euler => "euler",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StableAudioMusicDefaults {
    pub duration_seconds: f32,
    pub steps: u32,
    pub guidance_scale: f32,
    pub apg_scale: f32,
    pub sampler: StableAudioSampler,
    pub duration_padding_seconds: f32,
    pub truncate_to_duration: bool,
    pub chunked_decode: bool,
}

impl Default for StableAudioMusicDefaults {
    fn default() -> Self {
        Self {
            duration_seconds: 4.,
            steps: 8,
            guidance_scale: 1.,
            apg_scale: 1.,
            sampler: StableAudioSampler::Pingpong,
            duration_padding_seconds: 6.,
            truncate_to_duration: true,
            chunked_decode: true,
        }
    }
}

impl StableAudioMusicDefaults {
    pub(super) fn validate(&self) -> GenerationParameterResult<()> {
        bounded_int("steps", self.steps, 1)?;
        // The sampler casts the steps + 1 schedule length to a signed native int.
        if self.steps == i32::MAX as u32 {
            return Err(invalid(
                "steps",
                "out_of_range",
                "The native sampling schedule must fit steps plus one in a signed integer",
            ));
        }
        for (name, value) in [
            ("guidanceScale", self.guidance_scale),
            ("apgScale", self.apg_scale),
        ] {
            if !value.is_finite() {
                return Err(invalid(
                    name,
                    "out_of_range",
                    "The native guidance value must be finite",
                ));
            }
        }
        if !self.duration_seconds.is_finite() || self.duration_seconds <= 0. {
            return Err(invalid(
                "durationSeconds",
                "out_of_range",
                "Expected a finite positive duration",
            ));
        }
        if !self.duration_padding_seconds.is_finite() || self.duration_padding_seconds < 0. {
            return Err(invalid(
                "durationPaddingSeconds",
                "out_of_range",
                "Expected finite nonnegative padding",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StableAudioModelOutput {
    pub sample_rate: u32,
    pub channels: u16,
    pub max_sample_frames: u32,
    pub alignment_frames: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StableAudioMusicOutputSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_frames: u64,
}

impl StableAudioModelOutput {
    // Pinned GGUF model_config.json: downsampling_ratio * encoder_chunk_size / stride.
    pub(super) const SMALL_MUSIC: Self = Self {
        sample_rate: 44_100,
        channels: 2,
        max_sample_frames: 5_292_032,
        alignment_frames: 4_096 * 32 / 16,
    };

    pub(super) fn validate(self) -> GenerationParameterResult<()> {
        if self.sample_rate == 0
            || self.channels == 0
            || self.max_sample_frames == 0
            || self.alignment_frames == 0
            || !self.max_sample_frames.is_multiple_of(self.alignment_frames)
        {
            return Err(invalid(
                "output",
                "invalid_model_output",
                "Invalid model audio dimensions or alignment",
            ));
        }
        Ok(())
    }

    pub(super) fn spec(
        self,
        settings: &StableAudioMusicDefaults,
    ) -> GenerationParameterResult<StableAudioMusicOutputSpec> {
        settings.validate()?;
        self.validate()?;
        let rate = self.sample_rate as f32;
        let max = u64::from(self.max_sample_frames);
        let frames = settings.duration_seconds * rate;
        let padded = (settings.duration_seconds + settings.duration_padding_seconds) * rate;
        // Use the same float arithmetic as native sampler.cpp and session.cpp.
        if frames < 1.
            || settings.duration_seconds > self.max_sample_frames as f32 / rate
            || !padded.is_finite()
            || padded >= i64::MAX as f32
        {
            return Err(invalid("durationSeconds","out_of_range","Duration exceeds the model sample limit or padding overflows native frame arithmetic"));
        }
        let aligned = (padded as u64)
            .min(max)
            .next_multiple_of(u64::from(self.alignment_frames))
            .min(max);
        Ok(StableAudioMusicOutputSpec {
            sample_rate: self.sample_rate,
            channels: self.channels,
            sample_frames: if settings.truncate_to_duration {
                (frames as u64).min(aligned)
            } else {
                aligned
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StableAudioMusicRequest {
    pub session_id: String,
    pub job_id: String,
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub duration_seconds: Option<f32>,
    pub steps: Option<u32>,
    pub guidance_scale: Option<f32>,
    pub apg_scale: Option<f32>,
    pub sampler: Option<StableAudioSampler>,
    pub seed: Option<u32>,
    pub duration_padding_seconds: Option<f32>,
    pub truncate_to_duration: Option<bool>,
    pub chunked_decode: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum StableAudioMusicPatch {
    Prompt(String),
    NegativePrompt(Option<String>),
    DurationSeconds(f32),
    Steps(u32),
    GuidanceScale(f32),
    ApgScale(f32),
    Sampler(StableAudioSampler),
    Seed(u32),
    DurationPaddingSeconds(f32),
    TruncateToDuration(bool),
    ChunkedDecode(bool),
}

impl StableAudioMusicPatch {
    fn apply(self, request: &mut StableAudioMusicRequest) {
        match self {
            Self::Prompt(value) => request.prompt = value,
            Self::NegativePrompt(value) => request.negative_prompt = value,
            Self::DurationSeconds(value) => request.duration_seconds = Some(value),
            Self::Steps(value) => request.steps = Some(value),
            Self::GuidanceScale(value) => request.guidance_scale = Some(value),
            Self::ApgScale(value) => request.apg_scale = Some(value),
            Self::Sampler(value) => request.sampler = Some(value),
            Self::Seed(value) => request.seed = Some(value),
            Self::DurationPaddingSeconds(value) => request.duration_padding_seconds = Some(value),
            Self::TruncateToDuration(value) => request.truncate_to_duration = Some(value),
            Self::ChunkedDecode(value) => request.chunked_decode = Some(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveStableAudioMusicParameters {
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub duration_seconds: f32,
    pub steps: u32,
    pub guidance_scale: f32,
    pub apg_scale: f32,
    pub sampler: StableAudioSampler,
    pub seed: u32,
    pub duration_padding_seconds: f32,
    pub truncate_to_duration: bool,
    pub chunked_decode: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StableAudioMusicDescriptor {
    pub identity: TaskParameterIdentity,
    pub binding: AudioTaskModelBinding,
    pub fields: BTreeMap<String, AudioParameterField>,
    pub unsupported: BTreeMap<String, String>,
    pub output_sample_rate: u32,
    pub output_channels: u16,
    pub evidence: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StableAudioMusicSnapshot {
    pub metadata: AudioTaskSnapshotMetadata,
    pub parameters: EffectiveStableAudioMusicParameters,
}

impl StableAudioMusicSnapshot {
    fn request(&self) -> StableAudioMusicRequest {
        let p = &self.parameters;
        StableAudioMusicRequest {
            session_id: self.metadata.session_id.clone(),
            job_id: self.metadata.job_id.clone(),
            prompt: p.prompt.clone(),
            negative_prompt: p.negative_prompt.clone(),
            duration_seconds: Some(p.duration_seconds),
            steps: Some(p.steps),
            guidance_scale: Some(p.guidance_scale),
            apg_scale: Some(p.apg_scale),
            sampler: Some(p.sampler),
            seed: Some(p.seed),
            duration_padding_seconds: Some(p.duration_padding_seconds),
            truncate_to_duration: Some(p.truncate_to_duration),
            chunked_decode: Some(p.chunked_decode),
        }
    }
    pub fn output_spec(&self) -> GenerationParameterResult<StableAudioMusicOutputSpec> {
        let service = StableAudioMusicParameterService::new(self.metadata.binding.clone())?;
        service.validate_snapshot(self)?;
        let p = &self.parameters;
        service
            .profile
            .music_output()
            .spec(&StableAudioMusicDefaults {
                duration_seconds: p.duration_seconds,
                steps: p.steps,
                guidance_scale: p.guidance_scale,
                apg_scale: p.apg_scale,
                sampler: p.sampler,
                duration_padding_seconds: p.duration_padding_seconds,
                truncate_to_duration: p.truncate_to_duration,
                chunked_decode: p.chunked_decode,
            })
    }
    pub fn digest(&self) -> GenerationParameterResult<Sha256Digest> {
        digest(self)
    }
}

#[derive(Debug, Clone)]
pub struct StableAudioMusicPreparedTask {
    pub(super) snapshot: StableAudioMusicSnapshot,
    arguments: Vec<NativeAudioArgument>,
}
impl StableAudioMusicPreparedTask {
    pub fn snapshot(&self) -> &StableAudioMusicSnapshot {
        &self.snapshot
    }
    pub fn arguments(&self) -> &[NativeAudioArgument] {
        &self.arguments
    }
}

pub struct StableAudioMusicParameterService {
    profile: AudioTaskProfile,
    descriptor: StableAudioMusicDescriptor,
}

impl StableAudioMusicParameterService {
    pub fn new(binding: AudioTaskModelBinding) -> GenerationParameterResult<Self> {
        let profile = profile_for_binding(AudioTaskProtocol::StableAudioMusicOffline, &binding)?;
        Self::for_profile(profile, binding)
    }
    pub fn for_profile(
        profile: AudioTaskProfile,
        binding: AudioTaskModelBinding,
    ) -> GenerationParameterResult<Self> {
        let identity = validate_protocol(
            profile,
            AudioTaskProtocol::StableAudioMusicOffline,
            &binding,
        )?;
        let d = profile.music_settings();
        let output = profile.music_output();
        output.spec(&d)?;
        let fields = [
            (
                "prompt",
                "string",
                None,
                "--text",
                "One nonblank prompt; the native pipe delimiter separates batch members",
            ),
            (
                "negativePrompt",
                "string",
                None,
                "negative_prompt",
                "One optional nonblank negative prompt",
            ),
            (
                "durationSeconds",
                "f32",
                Some(json!(d.duration_seconds)),
                "duration_sec",
                "Positive duration at the model sample rate",
            ),
            (
                "steps",
                "u32",
                Some(json!(d.steps)),
                "num_inference_steps",
                "1..2147483646 diffusion iterations; actual admission requires a measured resource budget",
            ),
            (
                "guidanceScale",
                "f32",
                Some(json!(d.guidance_scale)),
                "guidance_scale",
                "Finite classifier-free guidance scale",
            ),
            (
                "apgScale",
                "f32",
                Some(json!(d.apg_scale)),
                "apg_scale",
                "Finite adaptive projected guidance scale",
            ),
            (
                "sampler",
                "enum",
                Some(json!(d.sampler)),
                "sampler_mode",
                "pingpong or euler",
            ),
            (
                "seed",
                "u32",
                None,
                "seed",
                "Omission chooses and records a concrete seed before native launch",
            ),
            (
                "durationPaddingSeconds",
                "f32",
                Some(json!(d.duration_padding_seconds)),
                "duration_padding_sec",
                "Nonnegative generation padding before trimming",
            ),
            (
                "truncateToDuration",
                "bool",
                Some(json!(d.truncate_to_duration)),
                "truncate_output_to_duration",
                "Trim decoded frames to the requested duration",
            ),
            (
                "chunkedDecode",
                "bool",
                Some(json!(d.chunked_decode)),
                "chunked_decode",
                "Decode the audio in chunks",
            ),
        ]
        .into_iter()
        .map(|(name, ty, default, native, limit)| {
            let argument = if native.starts_with("--") {
                native.to_owned()
            } else {
                format!("--request-option {native}")
            };
            (
                name.to_owned(),
                profiles::field(
                    ty,
                    name == "prompt",
                    default,
                    &argument,
                    limit,
                    "src/models/stable_audio/request.cpp",
                ),
            )
        })
        .collect();
        Ok(Self{profile, descriptor:StableAudioMusicDescriptor{
            binding, identity, fields, unsupported:BTreeMap::from([
                ("batchSize".into(),"This contract publishes one music track per request".into()),
                ("initAudio".into(),"Audio editing uses a separate input contract".into()),
            ]), evidence:profile.evidence().to_owned(), output_sample_rate:output.sample_rate,output_channels:output.channels,
            limitations:vec!["Resource admission binds the effective request and exact installed model and runtime.".into()]
        }})
    }
    pub fn descriptor(&self) -> &StableAudioMusicDescriptor {
        &self.descriptor
    }
    pub fn validate_snapshot(
        &self,
        snapshot: &StableAudioMusicSnapshot,
    ) -> GenerationParameterResult<()> {
        self.validate_snapshot_scoped(snapshot, None)
    }

    pub fn validate_snapshot_with_project_scope(
        &self,
        snapshot: &StableAudioMusicSnapshot,
        scope: &ProjectTaskCompilationScope,
    ) -> GenerationParameterResult<()> {
        validate_project_derivation(&snapshot.metadata, scope)?;
        self.validate_snapshot_scoped(snapshot, Some(scope))
    }

    fn validate_snapshot_scoped(
        &self,
        snapshot: &StableAudioMusicSnapshot,
        scope: Option<&ProjectTaskCompilationScope>,
    ) -> GenerationParameterResult<()> {
        let request = snapshot.request();
        let canonical = self.compile_scoped(request.clone(), scope)?.snapshot;
        validate_metadata(&snapshot.metadata, canonical.metadata)?;
        if canonical.parameters != snapshot.parameters {
            return Err(invalid_parent());
        }
        if let Some(change) = snapshot
            .metadata
            .derivation
            .as_ref()
            .and_then(|d| d.change.as_ref())
        {
            let mut previous = request;
            old_value::<StableAudioMusicPatch>(change)?.apply(&mut previous);
            if change_between(
                &self.compile_scoped(previous, scope)?.snapshot.parameters,
                &snapshot.parameters,
            )?
            .as_ref()
                != Some(change)
            {
                return Err(invalid_parent());
            }
        }
        Ok(())
    }

    pub fn validate_parent(
        &self,
        child: &StableAudioMusicSnapshot,
        parent: &StableAudioMusicSnapshot,
    ) -> GenerationParameterResult<()> {
        self.validate_parent_scoped(child, parent, None)
    }

    pub fn validate_parent_with_project_scope(
        &self,
        child: &StableAudioMusicSnapshot,
        parent: &StableAudioMusicSnapshot,
        scope: &ProjectTaskCompilationScope,
    ) -> GenerationParameterResult<()> {
        scope.validate_child(&child.metadata.session_id, &child.metadata.job_id)?;
        scope.validate_parent(
            &parent.metadata.session_id,
            &parent.metadata.job_id,
            &parent.digest()?,
        )?;
        self.validate_parent_scoped(child, parent, Some(scope))
    }

    fn validate_parent_scoped(
        &self,
        child: &StableAudioMusicSnapshot,
        parent: &StableAudioMusicSnapshot,
        scope: Option<&ProjectTaskCompilationScope>,
    ) -> GenerationParameterResult<()> {
        self.validate_snapshot_scoped(parent, scope)?;
        self.validate_snapshot_scoped(child, scope)?;
        compare_parent(
            &child.metadata,
            &child.parameters,
            &parent.metadata,
            &parent.parameters,
            parent.digest()?,
            scope,
        )
    }

    /// The caller retrieves the parent from the authenticated session's immutable configuration row.
    pub fn reuse_configuration(
        &self,
        session_id: &str,
        parent: &StableAudioMusicSnapshot,
        expected_parent_digest: &Sha256Digest,
        new_job_id: &str,
        patch: StableAudioMusicPatch,
    ) -> GenerationParameterResult<StableAudioMusicPreparedTask> {
        self.reuse_configuration_scoped(
            session_id,
            parent,
            expected_parent_digest,
            new_job_id,
            patch,
            None,
        )
    }

    pub fn reuse_configuration_with_project_scope(
        &self,
        session_id: &str,
        parent: &StableAudioMusicSnapshot,
        expected_parent_digest: &Sha256Digest,
        new_job_id: &str,
        patch: StableAudioMusicPatch,
        scope: &ProjectTaskCompilationScope,
    ) -> GenerationParameterResult<StableAudioMusicPreparedTask> {
        self.reuse_configuration_scoped(
            session_id,
            parent,
            expected_parent_digest,
            new_job_id,
            patch,
            Some(scope),
        )
    }

    fn reuse_configuration_scoped(
        &self,
        session_id: &str,
        parent: &StableAudioMusicSnapshot,
        expected_parent_digest: &Sha256Digest,
        new_job_id: &str,
        patch: StableAudioMusicPatch,
        scope: Option<&ProjectTaskCompilationScope>,
    ) -> GenerationParameterResult<StableAudioMusicPreparedTask> {
        let mut lineage = child_metadata(
            &parent.metadata,
            session_id,
            new_job_id,
            expected_parent_digest,
            parent.digest()?,
            scope,
        )?;
        self.validate_snapshot_scoped(parent, scope)?;
        let mut request = parent.request();
        request.job_id = new_job_id.into();
        request.session_id = session_id.into();
        patch.apply(&mut request);
        let mut prepared = self.compile_scoped(request, scope)?;
        lineage.change = change_between(&parent.parameters, &prepared.snapshot.parameters)?;
        prepared.snapshot.metadata.derivation = Some(lineage);
        Ok(prepared)
    }

    pub fn compile_json(
        &self,
        value: Value,
    ) -> GenerationParameterResult<StableAudioMusicPreparedTask> {
        self.compile(parse_request(value, &self.descriptor.unsupported)?)
    }
    pub fn compile(
        &self,
        r: StableAudioMusicRequest,
    ) -> GenerationParameterResult<StableAudioMusicPreparedTask> {
        self.compile_scoped(r, None)
    }

    pub fn compile_with_project_scope(
        &self,
        r: StableAudioMusicRequest,
        scope: &ProjectTaskCompilationScope,
    ) -> GenerationParameterResult<StableAudioMusicPreparedTask> {
        scope.validate_child(&r.session_id, &r.job_id)?;
        self.compile_scoped(r, Some(scope))
    }

    pub(super) fn compile_scoped(
        &self,
        r: StableAudioMusicRequest,
        scope: Option<&ProjectTaskCompilationScope>,
    ) -> GenerationParameterResult<StableAudioMusicPreparedTask> {
        if let Some(scope) = scope {
            scope.validate_identity(&r.session_id, &r.job_id)?;
        }
        identity("sessionId", &r.session_id)?;
        identity("jobId", &r.job_id)?;
        for (name, text) in [
            ("prompt", Some(r.prompt.as_str())),
            ("negativePrompt", r.negative_prompt.as_deref()),
        ] {
            if let Some(text) = text {
                native_text(name, text, true)?;
                if text.contains('|') {
                    return Err(invalid(
                        name,
                        "batch_delimiter",
                        "One prompt per request; pipe separates native batch items",
                    ));
                }
            }
        }
        let d = self.profile.music_settings();
        let settings = StableAudioMusicDefaults {
            duration_seconds: r.duration_seconds.unwrap_or(d.duration_seconds),
            steps: r.steps.unwrap_or(d.steps),
            guidance_scale: r.guidance_scale.unwrap_or(d.guidance_scale),
            apg_scale: r.apg_scale.unwrap_or(d.apg_scale),
            sampler: r.sampler.unwrap_or(d.sampler),
            duration_padding_seconds: r
                .duration_padding_seconds
                .unwrap_or(d.duration_padding_seconds),
            truncate_to_duration: r.truncate_to_duration.unwrap_or(d.truncate_to_duration),
            chunked_decode: r.chunked_decode.unwrap_or(d.chunked_decode),
        };
        self.profile.music_output().spec(&settings)?;
        let p = EffectiveStableAudioMusicParameters {
            prompt: r.prompt,
            negative_prompt: r.negative_prompt,
            seed: r.seed.unwrap_or_else(|| {
                u32::from_le_bytes(
                    uuid::Uuid::new_v4().as_bytes()[..4]
                        .try_into()
                        .expect("UUID prefix"),
                )
            }),
            duration_seconds: settings.duration_seconds,
            steps: settings.steps,
            guidance_scale: settings.guidance_scale,
            apg_scale: settings.apg_scale,
            sampler: settings.sampler,
            duration_padding_seconds: settings.duration_padding_seconds,
            truncate_to_duration: settings.truncate_to_duration,
            chunked_decode: settings.chunked_decode,
        };
        let metadata = metadata(
            &self.descriptor,
            &self.descriptor.identity,
            &self.descriptor.binding,
            r.session_id,
            r.job_id,
        )?;
        let mut args = common_arguments(self.profile, "gen");
        literal(&mut args, "--text", &p.prompt);
        for option in [
            "batch_size=1".to_owned(),
            format!("duration_sec={}", p.duration_seconds),
            format!("num_inference_steps={}", p.steps),
            format!("guidance_scale={}", p.guidance_scale),
            format!("apg_scale={}", p.apg_scale),
            format!("sampler_mode={}", p.sampler.native()),
            format!("seed={}", p.seed),
            format!("duration_padding_sec={}", p.duration_padding_seconds),
            format!("truncate_output_to_duration={}", p.truncate_to_duration),
            format!("chunked_decode={}", p.chunked_decode),
        ] {
            literal(&mut args, "--request-option", option);
        }
        if let Some(negative) = &p.negative_prompt {
            literal(
                &mut args,
                "--request-option",
                format!("negative_prompt={negative}"),
            );
        }
        literal(&mut args, "--out-format", "pcm16");
        args.extend([
            NativeAudioArgument::Literal("--out".into()),
            NativeAudioArgument::Output,
        ]);
        Ok(StableAudioMusicPreparedTask {
            snapshot: StableAudioMusicSnapshot {
                metadata,
                parameters: p,
            },
            arguments: args,
        })
    }
}

impl ModelTaskParameterAdapter for StableAudioMusicParameterService {
    type Request = StableAudioMusicRequest;
    type Prepared = StableAudioMusicPreparedTask;
    type Descriptor = StableAudioMusicDescriptor;
    fn identity(&self) -> GenerationParameterResult<TaskParameterIdentity> {
        Ok(self.descriptor.identity.clone())
    }
    fn describe(&self) -> &Self::Descriptor {
        self.descriptor()
    }
    fn prepare(&self, r: Self::Request) -> GenerationParameterResult<Self::Prepared> {
        self.compile(r)
    }
}
