use super::*;
use serde_json::json;

const REVISION: &str = "c0cd77fa80285f8113883e0de06ffc781cf502f6";
const BINARY: &str = "da25e095e147751e8c4db6634ef2fd7875ffe4883c8a70b2b01826cdb39ae787";

impl AudioTaskProfile {
    pub fn runtime_build(self) -> RuntimeBuild {
        RuntimeBuild {
            adapter_id: "audio-cpp-offline-parameters-v1".into(),
            engine_revision: REVISION.into(),
            binding_version: self.schema_id().into(),
            platform: "windows-x86_64".into(),
            features: BTreeSet::from(["cpu".into()]),
            binary_sha256: Sha256Digest::parse(BINARY).expect("pinned digest"),
        }
    }

    pub fn recorded_model(self) -> (Sha256Digest, u64) {
        let (sha, size) = match self {
            Self::PocketTtsEnglishCpu => (
                "3f71cf6a51f93c2f2e554e98970a3f49fb9a98d0c64bed5a815e65be3d2ff588",
                127_867_968,
            ),
            Self::Qwen3Asr06bCpu => (
                "5c53bbb9a6b2ade85c16c290c560658ca18b78a23c323378528f1f945683fcbe",
                1_151_279_968,
            ),
        };
        (Sha256Digest::parse(sha).expect("pinned digest"), size)
    }

    pub fn recorded_model_spec(self) -> (Sha256Digest, u64) {
        let (sha, size) = match self {
            Self::PocketTtsEnglishCpu => (
                "dd430cdc4c54ef04899c88272ba5b3561ffb121b9be4a1d9829c8fa0478750e8",
                15_862,
            ),
            Self::Qwen3Asr06bCpu => (
                "0cdc4bc56a2c11d092fff7a5ebe08612ecfc09d0310f845de26ea800b5bb5465",
                14_197,
            ),
        };
        (Sha256Digest::parse(sha).expect("pinned digest"), size)
    }

    pub fn capability(self) -> ModelCapability {
        match self {
            Self::PocketTtsEnglishCpu => ModelCapability::SpeechSynthesis,
            Self::Qwen3Asr06bCpu => ModelCapability::Transcription,
        }
    }

    pub(super) fn schema_id(self) -> &'static str {
        match self {
            Self::PocketTtsEnglishCpu => "pocket-tts-english-offline-parameters-v1",
            Self::Qwen3Asr06bCpu => "qwen3-asr-0.6b-offline-parameters-v1",
        }
    }

    pub(super) fn family(self) -> &'static str {
        match self {
            Self::PocketTtsEnglishCpu => "pocket_tts",
            Self::Qwen3Asr06bCpu => "qwen3_asr",
        }
    }

    pub(super) fn validate(
        self,
        binding: &AudioTaskModelBinding,
    ) -> GenerationParameterResult<TaskParameterIdentity> {
        for value in [&binding.installation_id, &binding.version_id] {
            if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(invalid(
                    "binding",
                    "invalid_identity",
                    "Installation and version identities are required",
                ));
            }
        }
        if binding.runtime != self.runtime_build() {
            return Err(invalid(
                "binding.runtime",
                "unsupported_runtime",
                "Audio profile requires the exact pinned CPU binary and adapter schema",
            ));
        }
        if binding.target_capabilities != BTreeSet::from([self.capability()]) {
            return Err(invalid(
                "binding.targetCapabilities",
                "capability_mismatch",
                "Audio profile requires exactly its task capability",
            ));
        }
        for (name, asset, (sha, size)) in [
            ("binding.model", &binding.model, self.recorded_model()),
            (
                "binding.modelSpec",
                &binding.model_spec,
                self.recorded_model_spec(),
            ),
        ] {
            if asset.artifact_id.trim().is_empty()
                || asset.sha256 != sha
                || asset.size_bytes != size
            {
                return Err(invalid(
                    name,
                    "unsupported_component",
                    "Artifact ID, exact content digest and size must match the fixed audio profile",
                ));
            }
        }
        if binding.model.artifact_id == binding.model_spec.artifact_id {
            return Err(invalid(
                "binding.modelSpec",
                "duplicate_artifact",
                "Main model and model spec require separate artifacts",
            ));
        }
        Ok(TaskParameterIdentity {
            installation_id: binding.installation_id.clone(),
            version_id: binding.version_id.clone(),
            manifest_digest: binding.manifest_digest.as_str().into(),
            runtime_digest: digest(&binding.runtime)?.as_str().into(),
            component_binding_digest: digest(&(&binding.model, &binding.model_spec))?
                .as_str()
                .into(),
            task_variant: format!(
                "{}/offline/{}",
                self.family(),
                if self == Self::PocketTtsEnglishCpu {
                    "english_voice_reference"
                } else {
                    "transcription"
                }
            ),
            parameter_schema_id: self.schema_id().into(),
        })
    }
}

impl Qwen3AsrLanguage {
    pub fn native_value(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Chinese => "Chinese",
            Self::English => "English",
            Self::Cantonese => "Cantonese",
            Self::Arabic => "Arabic",
            Self::German => "German",
            Self::French => "French",
            Self::Spanish => "Spanish",
            Self::Portuguese => "Portuguese",
            Self::Indonesian => "Indonesian",
            Self::Italian => "Italian",
            Self::Korean => "Korean",
            Self::Russian => "Russian",
            Self::Thai => "Thai",
            Self::Vietnamese => "Vietnamese",
            Self::Japanese => "Japanese",
            Self::Turkish => "Turkish",
            Self::Hindi => "Hindi",
            Self::Malay => "Malay",
            Self::Dutch => "Dutch",
            Self::Swedish => "Swedish",
            Self::Danish => "Danish",
            Self::Finnish => "Finnish",
            Self::Polish => "Polish",
            Self::Czech => "Czech",
            Self::Filipino => "Filipino",
            Self::Persian => "Persian",
            Self::Greek => "Greek",
            Self::Romanian => "Romanian",
            Self::Hungarian => "Hungarian",
            Self::Macedonian => "Macedonian",
        }
    }

    pub(super) fn all() -> Vec<Self> {
        use Qwen3AsrLanguage::*;
        vec![
            Auto, Chinese, English, Cantonese, Arabic, German, French, Spanish, Portuguese,
            Indonesian, Italian, Korean, Russian, Thai, Vietnamese, Japanese, Turkish, Hindi,
            Malay, Dutch, Swedish, Danish, Finnish, Polish, Czech, Filipino, Persian, Greek,
            Romanian, Hungarian, Macedonian,
        ]
    }
}

fn field(
    kind: &str,
    required: bool,
    default: Option<Value>,
    native: &str,
    constraint: &str,
    source: &str,
) -> AudioParameterField {
    AudioParameterField {
        value_type: kind.into(),
        required,
        default,
        native_option: Some(native.into()),
        constraints: vec![constraint.into()],
        source: format!("audio.cpp@{REVISION}/{source}"),
    }
}

fn limitations() -> Vec<String> {
    vec![
        "Exact model, explicit spec and CPU executable only; not a universal audio schema".into(),
        "Resource admission, verified input handles and managed execution are still required; no READY or quality acceptance is granted".into(),
        "Create only; parent-result patches and native prepared-session reuse are not provided by this adapter".into(),
        "Defaults are explicit compiler settings based on pinned native source; the historical smoke report did not freeze every request option".into(),
        "Input references are metadata until the session owner verifies the same content digest, bytes and decoded samples".into(),
    ]
}

pub(super) fn pocket_descriptor(
    binding: AudioTaskModelBinding,
    identity: TaskParameterIdentity,
) -> PocketTtsDescriptor {
    let source = "src/models/pocket_tts/session.cpp";
    let mut fields = BTreeMap::new();
    for (name, value) in [
        ("text", field("string", true, None, "--text", "1..16384 UTF-8 bytes, no NUL or standalone native option token", source)),
        ("voiceReference", field("session_audio_reference", true, None, "--voice-ref", "Same-session verified WAV; <=30 seconds, no silent truncation; owner must revalidate", "src/models/pocket_tts/voice_conditioner.cpp")),
        ("seed", field("u32", false, None, "--seed", "Omission generates and records a concrete seed before persistence; captured during prepare", source)),
        ("temperature", field("f32", false, Some(json!(0.3_f32)), "--temperature", "Finite and positive; captured during prepare; English builtin default explicitly passed", "src/models/pocket_tts/assets.cpp")),
        ("maxFrames", field("u32", false, Some(json!(0)), "--request-option max_frames", "0..i32::MAX; 0 is native text-derived cap, not a fixed duration or resource limit", source)),
        ("textChunkTokens", field("u32", false, Some(json!(50)), "--request-option text_chunk_tokens", "1..i32::MAX; tokenizer tokens per text chunk, not image steps or output audio frames", source)),
        ("framesAfterEos", field("i32", false, Some(json!(-1)), "--request-option frames_after_eos", "-1 or non-negative; -1 retains model/text-derived EOS tail", source)),
    ] { fields.insert(name.into(), value); }
    let unsupported = [
        ("language", "This exact English weight is fixed at load; the PocketTTS request contract does not accept language"),
        ("emotion", "PocketTTS session rejects runtime style controls"),
        ("speed", "PocketTTS session rejects runtime speaking-rate controls"),
        ("speakingRate", "PocketTTS session rejects runtime speaking-rate controls"),
        ("pitchShift", "PocketTTS session rejects runtime style controls"),
        ("voiceId", "Preset voice state is not an installed/bound asset in this profile; use a verified WAV reference"),
        ("streaming", "This adapter is offline only; native streaming is not managed or admitted here"),
    ].into_iter().map(|(key, reason)| (key.into(), reason.into())).collect();
    PocketTtsDescriptor {
        identity,
        binding,
        fields,
        unsupported,
        model_language: "english".into(),
        output_sample_rate: 24_000,
        evidence: AUDIO_TASK_PARAMETER_EVIDENCE.into(),
        limitations: limitations(),
    }
}

pub(super) fn asr_descriptor(
    binding: AudioTaskModelBinding,
    identity: TaskParameterIdentity,
) -> Qwen3AsrDescriptor {
    let source = "src/models/qwen3_asr/session.cpp";
    let fields = [
        ("audio", field("session_audio_reference", true, None, "--audio", "Same-session verified WAV, 8k..192k Hz, mono/stereo, <=256MiB encoded and decoded protocol bounds; not memory admission", source)),
        ("context", field("string", false, Some(json!("")), "--text", "At most 16384 UTF-8 bytes, no NUL or standalone native option token; supplied as ASR context", source)),
        ("language", field("enum", false, Some(json!("auto")), "--language", "Recognized language label; source-supported choices, only English has the referenced smoke evidence", "src/models/qwen3_asr/assets.cpp")),
        ("maxTokens", field("u32", false, Some(json!(512)), "--max-tokens", "1..i32::MAX native option representation; actual prompt+output context and memory still require admission", source)),
        ("chunking", field("fixed_or_whole_file", false, Some(json!({"kind":"fixed","seconds":30.0})), "--audio-chunk-mode/--audio-chunk-seconds", "Fixed: finite positive seconds producing >=1 sample; WholeFile carries no ignored duration; VAD and timestamps need extra assets", source)),
    ].into_iter().map(|(key, value)| (key.into(), value)).collect();
    let unsupported = [
        ("temperature", "This Qwen3-ASR session does not expose sampling temperature"),
        ("seed", "This Qwen3-ASR session does not expose stochastic sampling seed"),
        ("voiceReference", "ASR consumes input audio and cannot synthesize a chosen voice"),
        ("returnTimestamps", "Requires a separately bound Qwen3 ForcedAligner asset; unavailable in this two-artifact profile"),
        ("speed", "ASR does not expose speech synthesis speed"),
        ("emotion", "ASR does not expose speech synthesis emotion"),
        ("streaming", "This adapter is offline only"),
    ].into_iter().map(|(key, reason)| (key.into(), reason.into())).collect();
    Qwen3AsrDescriptor {
        identity,
        binding,
        fields,
        unsupported,
        recognition_languages: Qwen3AsrLanguage::all(),
        evidence: AUDIO_TASK_PARAMETER_EVIDENCE.into(),
        limitations: limitations(),
    }
}
