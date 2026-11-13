use super::*;
use serde::de::DeserializeOwned;
use uuid::Uuid;

pub struct PocketTtsParameterService {
    descriptor: PocketTtsDescriptor,
}

pub struct Qwen3AsrParameterService {
    descriptor: Qwen3AsrDescriptor,
}

#[derive(Debug, Clone)]
pub struct PocketTtsPreparedTask {
    snapshot: PocketTtsSnapshot,
    arguments: Vec<NativeAudioArgument>,
}

#[derive(Debug, Clone)]
pub struct Qwen3AsrPreparedTask {
    snapshot: Qwen3AsrSnapshot,
    arguments: Vec<NativeAudioArgument>,
}

impl PocketTtsPreparedTask {
    pub fn snapshot(&self) -> &PocketTtsSnapshot {
        &self.snapshot
    }
    pub fn arguments(&self) -> &[NativeAudioArgument] {
        &self.arguments
    }
    pub fn ensure_execution_admitted(&self) -> GenerationParameterResult<()> {
        admission_required()
    }
}

impl Qwen3AsrPreparedTask {
    pub fn snapshot(&self) -> &Qwen3AsrSnapshot {
        &self.snapshot
    }
    pub fn arguments(&self) -> &[NativeAudioArgument] {
        &self.arguments
    }
    pub fn ensure_execution_admitted(&self) -> GenerationParameterResult<()> {
        admission_required()
    }
}

impl PocketTtsSnapshot {
    pub fn digest(&self) -> GenerationParameterResult<Sha256Digest> {
        digest(self)
    }
}

impl Qwen3AsrSnapshot {
    pub fn digest(&self) -> GenerationParameterResult<Sha256Digest> {
        digest(self)
    }
}

impl PocketTtsParameterService {
    pub fn new(binding: AudioTaskModelBinding) -> GenerationParameterResult<Self> {
        let identity = AudioTaskProfile::PocketTtsEnglishCpu.validate(&binding)?;
        Ok(Self {
            descriptor: profiles::pocket_descriptor(binding, identity),
        })
    }

    pub fn descriptor(&self) -> &PocketTtsDescriptor {
        &self.descriptor
    }

    pub fn compile_json(&self, value: Value) -> GenerationParameterResult<PocketTtsPreparedTask> {
        self.compile(parse_request(value, &self.descriptor.unsupported)?)
    }

    pub fn compile(
        &self,
        request: PocketTtsRequest,
    ) -> GenerationParameterResult<PocketTtsPreparedTask> {
        identity("sessionId", &request.session_id)?;
        identity("jobId", &request.job_id)?;
        native_text("text", &request.text, true)?;
        media(
            "voiceReference",
            &request.session_id,
            &request.voice_reference,
        )?;
        if request.voice_reference.sample_frames
            > u64::from(request.voice_reference.sample_rate) * 30
        {
            return Err(invalid("voiceReference", "protocol_limit", "Voice reference exceeds the 30-second adapter bound; implicit truncation is disabled"));
        }
        let temperature = request.temperature.unwrap_or(0.3);
        if !temperature.is_finite() || temperature <= 0. {
            return Err(invalid(
                "temperature",
                "out_of_range",
                "PocketTTS flow temperature must be finite and positive",
            ));
        }
        let max_frames = request.max_frames.unwrap_or(0);
        bounded_int("maxFrames", max_frames, 0)?;
        let text_chunk_tokens = request.text_chunk_tokens.unwrap_or(50);
        bounded_int("textChunkTokens", text_chunk_tokens, 1)?;
        let frames_after_eos = request.frames_after_eos.unwrap_or(-1);
        if frames_after_eos < -1 {
            return Err(invalid(
                "framesAfterEos",
                "out_of_range",
                "EOS tail must be -1 or non-negative",
            ));
        }
        let seed = request.seed.unwrap_or_else(|| {
            u32::from_le_bytes(
                Uuid::new_v4().as_bytes()[..4]
                    .try_into()
                    .expect("UUID prefix"),
            )
        });
        let metadata = metadata(
            &self.descriptor,
            &self.descriptor.identity,
            &self.descriptor.binding,
            request.session_id,
            request.job_id,
        )?;
        let parameters = EffectivePocketTtsParameters {
            text: request.text,
            voice_reference: request.voice_reference,
            seed,
            temperature,
            max_frames,
            text_chunk_tokens,
            frames_after_eos,
            noise_clamp: -1.,
            eos_threshold: -4.,
            truncate_clone_audio: false,
        };
        let mut args = common_arguments(AudioTaskProfile::PocketTtsEnglishCpu, "tts");
        literal(&mut args, "--load-option", "language=english");
        literal(&mut args, "--text", &parameters.text);
        args.extend([
            NativeAudioArgument::Literal("--voice-ref".into()),
            NativeAudioArgument::Input(parameters.voice_reference.clone()),
        ]);
        literal(&mut args, "--seed", parameters.seed.to_string());
        literal(
            &mut args,
            "--temperature",
            parameters.temperature.to_string(),
        );
        for value in [
            format!("max_frames={}", parameters.max_frames),
            format!("text_chunk_tokens={}", parameters.text_chunk_tokens),
            format!("frames_after_eos={}", parameters.frames_after_eos),
            format!("noise_clamp={}", parameters.noise_clamp),
            format!("eos_threshold={}", parameters.eos_threshold),
            "truncate_clone_audio=false".into(),
        ] {
            literal(&mut args, "--request-option", value);
        }
        literal(&mut args, "--out-format", "pcm16");
        args.extend([
            NativeAudioArgument::Literal("--out".into()),
            NativeAudioArgument::Output,
        ]);
        Ok(PocketTtsPreparedTask {
            snapshot: PocketTtsSnapshot {
                metadata,
                parameters,
            },
            arguments: args,
        })
    }
}

impl Qwen3AsrParameterService {
    pub fn new(binding: AudioTaskModelBinding) -> GenerationParameterResult<Self> {
        let identity = AudioTaskProfile::Qwen3Asr06bCpu.validate(&binding)?;
        Ok(Self {
            descriptor: profiles::asr_descriptor(binding, identity),
        })
    }

    pub fn descriptor(&self) -> &Qwen3AsrDescriptor {
        &self.descriptor
    }

    pub fn compile_json(&self, value: Value) -> GenerationParameterResult<Qwen3AsrPreparedTask> {
        self.compile(parse_request(value, &self.descriptor.unsupported)?)
    }

    pub fn compile(
        &self,
        request: Qwen3AsrRequest,
    ) -> GenerationParameterResult<Qwen3AsrPreparedTask> {
        identity("sessionId", &request.session_id)?;
        identity("jobId", &request.job_id)?;
        media("audio", &request.session_id, &request.audio)?;
        let context = request.context.unwrap_or_default();
        native_text("context", &context, false)?;
        let max_tokens = request.max_tokens.unwrap_or(512);
        bounded_int("maxTokens", max_tokens, 1)?;
        let chunking = request
            .chunking
            .unwrap_or(Qwen3AsrChunking::Fixed { seconds: 30. });
        if let Qwen3AsrChunking::Fixed { seconds } = &chunking {
            let samples = f64::from(*seconds) * f64::from(request.audio.sample_rate);
            if !seconds.is_finite() || *seconds <= 0. || samples < 1. || samples >= i64::MAX as f64
            {
                return Err(invalid("chunking.seconds", "out_of_range", "Fixed chunk duration must be finite, positive and represent at least one sample within the native frame range"));
            }
        }
        let metadata = metadata(
            &self.descriptor,
            &self.descriptor.identity,
            &self.descriptor.binding,
            request.session_id,
            request.job_id,
        )?;
        let parameters = EffectiveQwen3AsrParameters {
            audio: request.audio,
            context,
            language: request.language.unwrap_or(Qwen3AsrLanguage::Auto),
            max_tokens,
            chunking,
            return_timestamps: false,
        };
        let mut args = common_arguments(AudioTaskProfile::Qwen3Asr06bCpu, "asr");
        args.extend([
            NativeAudioArgument::Literal("--audio".into()),
            NativeAudioArgument::Input(parameters.audio.clone()),
        ]);
        literal(&mut args, "--text", &parameters.context);
        literal(&mut args, "--language", parameters.language.native_value());
        literal(&mut args, "--max-tokens", parameters.max_tokens.to_string());
        literal(&mut args, "--request-option", "return_timestamps=false");
        match parameters.chunking {
            Qwen3AsrChunking::Fixed { seconds } => {
                literal(&mut args, "--audio-chunk-mode", "fixed");
                literal(&mut args, "--audio-chunk-seconds", seconds.to_string());
            }
            Qwen3AsrChunking::WholeFile {} => literal(&mut args, "--audio-chunk-mode", "none"),
        }
        args.extend([
            NativeAudioArgument::Literal("--text-out".into()),
            NativeAudioArgument::Output,
        ]);
        Ok(Qwen3AsrPreparedTask {
            snapshot: Qwen3AsrSnapshot {
                metadata,
                parameters,
            },
            arguments: args,
        })
    }
}

impl ModelTaskParameterAdapter for PocketTtsParameterService {
    type Request = PocketTtsRequest;
    type Prepared = PocketTtsPreparedTask;
    type Descriptor = PocketTtsDescriptor;
    fn identity(&self) -> GenerationParameterResult<TaskParameterIdentity> {
        Ok(self.descriptor.identity.clone())
    }
    fn describe(&self) -> &Self::Descriptor {
        self.descriptor()
    }
    fn prepare(&self, request: Self::Request) -> GenerationParameterResult<Self::Prepared> {
        self.compile(request)
    }
}

impl ModelTaskParameterAdapter for Qwen3AsrParameterService {
    type Request = Qwen3AsrRequest;
    type Prepared = Qwen3AsrPreparedTask;
    type Descriptor = Qwen3AsrDescriptor;
    fn identity(&self) -> GenerationParameterResult<TaskParameterIdentity> {
        Ok(self.descriptor.identity.clone())
    }
    fn describe(&self) -> &Self::Descriptor {
        self.descriptor()
    }
    fn prepare(&self, request: Self::Request) -> GenerationParameterResult<Self::Prepared> {
        self.compile(request)
    }
}

fn parse_request<T: DeserializeOwned>(
    value: Value,
    unsupported: &BTreeMap<String, String>,
) -> GenerationParameterResult<T> {
    if let Some(object) = value.as_object() {
        for (name, reason) in unsupported {
            if object.contains_key(name) {
                return Err(invalid(name, "unsupported_parameter", reason));
            }
        }
    }
    serde_json::from_value(value)
        .map_err(|error| invalid("request", "invalid_schema", &error.to_string()))
}

fn metadata(
    descriptor: &impl Serialize,
    identity: &TaskParameterIdentity,
    binding: &AudioTaskModelBinding,
    session_id: String,
    job_id: String,
) -> GenerationParameterResult<AudioTaskSnapshotMetadata> {
    Ok(AudioTaskSnapshotMetadata {
        schema_version: AUDIO_TASK_PARAMETER_SCHEMA,
        identity: identity.clone(),
        binding: binding.clone(),
        descriptor_digest: digest(descriptor)?,
        session_id,
        job_id,
        backend: "cpu".into(),
        cpu_threads: 4,
        execution_status: AudioTaskExecutionStatus::NeedsResourceAdmission,
        requires_new_native_preparation: true,
    })
}

fn identity(field: &str, value: &str) -> GenerationParameterResult<()> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(invalid(
            field,
            "invalid_identity",
            "Expected 1..256 bytes without control characters",
        ));
    }
    Ok(())
}

fn native_text(field: &str, value: &str, required: bool) -> GenerationParameterResult<()> {
    if (required && value.trim().is_empty()) || value.len() > 16_384 || value.contains('\0') {
        return Err(invalid(
            field,
            "invalid_text",
            "Expected bounded text without NUL (at most 16384 UTF-8 bytes)",
        ));
    }
    // The pinned CLI's find_arg/has_arg scan every argv entry, including option values.
    if value.starts_with("--") && !value.chars().any(char::is_whitespace) {
        return Err(invalid(
            field,
            "native_option_collision",
            "A standalone option-like token is ambiguous in the pinned audio CLI",
        ));
    }
    Ok(())
}

fn bounded_int(field: &str, value: u32, minimum: u32) -> GenerationParameterResult<()> {
    if value < minimum || value > i32::MAX as u32 {
        return Err(invalid(
            field,
            "out_of_range",
            "Value is outside the native integer option representation",
        ));
    }
    Ok(())
}

fn media(
    field: &str,
    session_id: &str,
    input: &AudioTaskMediaReference,
) -> GenerationParameterResult<()> {
    if input.session_id != session_id {
        return Err(invalid(
            field,
            "result_unavailable",
            "Audio input must belong to the authenticated task session",
        ));
    }
    if !Uuid::parse_str(&input.result_id).is_ok_and(|id| id.to_string() == input.result_id) {
        return Err(invalid(
            field,
            "invalid_reference",
            "Expected a canonical media result UUID",
        ));
    }
    let decoded_bytes = input
        .sample_frames
        .checked_mul(u64::from(input.channels))
        .and_then(|n| n.checked_mul(4));
    if input.mime_type != "audio/wav"
        || input.size_bytes < 44
        || input.size_bytes > 256 * 1024 * 1024
        || !(8_000..=192_000).contains(&input.sample_rate)
        || !matches!(input.channels, 1 | 2)
        || input.sample_frames == 0
        || decoded_bytes.is_none_or(|n| n > 256 * 1024 * 1024)
    {
        return Err(invalid(field, "invalid_media", "Expected WAV reference within mono/stereo, sample-rate and 256MiB encoded/decoded protocol bounds; actual content requires owner verification"));
    }
    Ok(())
}

fn literal(args: &mut Vec<NativeAudioArgument>, flag: &str, value: impl Into<String>) {
    args.extend([
        NativeAudioArgument::Literal(flag.into()),
        NativeAudioArgument::Literal(value.into()),
    ]);
}

fn common_arguments(profile: AudioTaskProfile, task: &str) -> Vec<NativeAudioArgument> {
    let mut args = Vec::new();
    literal(&mut args, "--task", task);
    literal(&mut args, "--family", profile.family());
    args.extend([
        NativeAudioArgument::Literal("--model".into()),
        NativeAudioArgument::Model,
        NativeAudioArgument::Literal("--model-spec-override".into()),
        NativeAudioArgument::ModelSpec,
    ]);
    literal(&mut args, "--backend", "cpu");
    literal(&mut args, "--threads", "4");
    literal(&mut args, "--mode", "offline");
    args
}
