use super::{AudioTaskProtocol, ModelCapability, Qwen3AsrLanguage, AUDIO_TASK_PARAMETER_EVIDENCE};
use crate::lifecycle::{ModelScopeRepository, RepositoryPath, Sha256Digest};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::LazyLock};

pub(super) const WEIGHTS_REPOSITORY: &str = "audio-cpp/audio.cpp-gguf";
pub(super) const WEIGHTS_REVISION: &str = "459bd958d533fc2530e985e6bdf8cdfa2a5df3a6";

pub(super) struct AudioArtifactRecord {
    pub path: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

pub(super) struct AudioModelRecord {
    pub install_name: &'static str,
    pub protocol: AudioTaskProtocol,
    pub family: &'static str,
    pub task_variant: &'static str,
    pub schema_id: &'static str,
    pub capability: ModelCapability,
    pub weights: AudioArtifactRecord,
    pub spec: AudioArtifactRecord,
    pub spec_bytes: &'static [u8],
}

const POCKET_TTS: AudioModelRecord = AudioModelRecord {
    install_name: "pocket-tts-english-cpu",
    protocol: AudioTaskProtocol::PocketTtsOffline,
    family: "pocket_tts",
    task_variant: "pocket_tts/offline/english_voice_reference",
    schema_id: "pocket-tts-english-offline-parameters-v1",
    capability: ModelCapability::SpeechSynthesis,
    weights: AudioArtifactRecord {
        path: "PocketTTS-GGUF/english/pocket-tts-english-q8_0.gguf",
        sha256: "3f71cf6a51f93c2f2e554e98970a3f49fb9a98d0c64bed5a815e65be3d2ff588",
        size: 127_867_968,
    },
    spec: AudioArtifactRecord {
        path: "model_specs/pocket_tts.json",
        sha256: "dd430cdc4c54ef04899c88272ba5b3561ffb121b9be4a1d9829c8fa0478750e8",
        size: 15_862,
    },
    spec_bytes: include_bytes!("pocket_tts.json"),
};

const QWEN3_ASR: AudioModelRecord = AudioModelRecord {
    install_name: "qwen3-asr06b-cpu",
    protocol: AudioTaskProtocol::Qwen3AsrOffline,
    family: "qwen3_asr",
    task_variant: "qwen3_asr/offline/transcription",
    schema_id: "qwen3-asr-0.6b-offline-parameters-v1",
    capability: ModelCapability::Transcription,
    weights: AudioArtifactRecord {
        path: "Qwen3-ASR-0.6B-GGUF/qwen3-asr-0.6b-q8_0.gguf",
        sha256: "5c53bbb9a6b2ade85c16c290c560658ca18b78a23c323378528f1f945683fcbe",
        size: 1_151_279_968,
    },
    spec: AudioArtifactRecord {
        path: "model_specs/qwen3_asr.json",
        sha256: "0cdc4bc56a2c11d092fff7a5ebe08612ecfc09d0310f845de26ea800b5bb5465",
        size: 14_197,
    },
    spec_bytes: include_bytes!("qwen3_asr.json"),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct AudioTaskProfile(&'static str);

impl<'de> Deserialize<'de> for AudioTaskProfile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        Self::registered()
            .find(|p| p.0 == key)
            .ok_or_else(|| serde::de::Error::custom("Unknown audio task profile"))
    }
}

#[allow(non_upper_case_globals)]
impl AudioTaskProfile {
    pub const PocketTtsEnglishCpu: Self = Self("pocket_tts_english_cpu");
    pub const Qwen3Asr06bCpu: Self = Self("qwen3_asr06b_cpu");

    fn external(self) -> Option<&'static Definition> {
        ADDITIONS.get(self.0)
    }
    pub(super) fn source(self) -> (&'static str, &'static str) {
        self.external()
            .map_or((WEIGHTS_REPOSITORY, WEIGHTS_REVISION), |d| {
                (d.repository.as_str(), d.revision.as_str())
            })
    }
    pub(super) fn external_weight(self) -> Option<(&'static str, Sha256Digest, u64)> {
        self.external()
            .map(|d| (d.file.as_str(), d.sha256.clone(), d.size_bytes))
    }
    pub(super) fn schema_id(self) -> &'static str {
        self.external()
            .map_or_else(|| self.record().schema_id, |_| self.0)
    }
    pub(super) fn task_variant(self) -> &'static str {
        self.external()
            .map_or_else(|| self.record().task_variant, |d| d.name.as_str())
    }
    pub(in crate::lifecycle) fn evidence(self) -> &'static str {
        self.external()
            .map_or(AUDIO_TASK_PARAMETER_EVIDENCE, |_| self.0)
    }
    pub(super) fn pocket_settings(self) -> (&'static str, f32) {
        match self.external().map(|d| &d.parameters) {
            Some(Parameters::PocketTtsOffline {
                language,
                temperature,
            }) => (language, *temperature),
            _ => ("english", 0.3),
        }
    }
    pub(super) fn asr_settings(self) -> (Vec<Qwen3AsrLanguage>, Qwen3AsrLanguage, u32) {
        match self.external().map(|d| &d.parameters) {
            Some(Parameters::Qwen3AsrOffline {
                languages,
                default_language,
                max_tokens,
            }) => (languages.clone(), *default_language, *max_tokens),
            _ => (Qwen3AsrLanguage::all(), Qwen3AsrLanguage::Auto, 512),
        }
    }
    pub fn installation_name(self) -> &'static str {
        self.external()
            .map_or_else(|| self.record().install_name, |d| d.name.as_str())
    }

    pub fn protocol(self) -> AudioTaskProtocol {
        self.record().protocol
    }

    pub fn registered() -> impl Iterator<Item = Self> {
        [Self::PocketTtsEnglishCpu, Self::Qwen3Asr06bCpu]
            .into_iter()
            .chain(ADDITIONS.keys().map(|id| Self(id)))
    }

    pub(super) fn record(self) -> &'static AudioModelRecord {
        match self {
            Self::PocketTtsEnglishCpu => &POCKET_TTS,
            Self::Qwen3Asr06bCpu => &QWEN3_ASR,
            _ => self
                .external()
                .expect("registered audio profile")
                .parameters
                .protocol_record(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Catalog {
    schema_version: u32,
    models: Vec<Definition>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Definition {
    name: String,
    repository: ModelScopeRepository,
    revision: String,
    file: RepositoryPath,
    sha256: Sha256Digest,
    size_bytes: u64,
    parameters: Parameters,
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "protocol",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Parameters {
    PocketTtsOffline {
        language: String,
        temperature: f32,
    },
    Qwen3AsrOffline {
        languages: Vec<Qwen3AsrLanguage>,
        default_language: Qwen3AsrLanguage,
        max_tokens: u32,
    },
}

impl Parameters {
    fn protocol_record(&self) -> &'static AudioModelRecord {
        match self {
            Self::PocketTtsOffline { .. } => &POCKET_TTS,
            Self::Qwen3AsrOffline { .. } => &QWEN3_ASR,
        }
    }
}

static ADDITIONS: LazyLock<BTreeMap<String, Definition>> = LazyLock::new(|| {
    let Some(path) = std::env::var_os("LUMINA_AUDIO_CATALOG") else {
        return BTreeMap::new();
    };
    load(std::path::Path::new(&path)).unwrap_or_else(|err| {
        tracing::error!("Cannot load configured audio catalog: {err:#}");
        BTreeMap::new()
    })
});

fn load(path: &std::path::Path) -> Result<BTreeMap<String, Definition>> {
    use std::{collections::BTreeSet, io::Read};
    const LIMIT: u64 = 2 * 1024 * 1024;
    ensure!(
        path.is_absolute(),
        "LUMINA_AUDIO_CATALOG requires an absolute path"
    );
    let file = std::fs::File::open(path)
        .with_context(|| format!("Opening audio catalog {}", path.display()))?;
    ensure!(file.metadata()?.is_file(), "Expected an audio catalog file");
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= LIMIT, "Audio catalog exceeds 2 MiB");
    let catalog: Catalog =
        serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes))?;
    ensure!(
        catalog.schema_version == 1 && catalog.models.len() <= 256,
        "Invalid audio catalog version or model count"
    );
    let mut names: BTreeSet<String> = [&POCKET_TTS, &QWEN3_ASR]
        .iter()
        .map(|p| p.install_name.to_owned())
        .collect();
    let mut hashes: BTreeSet<String> = [&POCKET_TTS, &QWEN3_ASR]
        .iter()
        .map(|p| p.weights.sha256.to_owned())
        .collect();
    let mut models = BTreeMap::new();
    for d in catalog.models {
        ensure!(
            d.name.len() <= 96
                && d.name.starts_with(|c: char| c.is_ascii_lowercase())
                && d.name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && names.insert(d.name.clone()),
            "Invalid or duplicate audio profile name"
        );
        ensure!(
            d.revision.len() == 40
                && d.revision
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                && d.file.as_str().to_ascii_lowercase().ends_with(".gguf")
                && d.size_bytes > 0
                && hashes.insert(d.sha256.as_str().to_owned()),
            "Expected a unique revision-pinned GGUF audio weight"
        );
        let protocol = d.parameters.protocol_record();
        ensure!(
            d.size_bytes <= (u64::MAX - 16 * 1024 * 1024) / 3 - protocol.spec.size,
            "Audio installation size overflow"
        );
        match &d.parameters {
            Parameters::PocketTtsOffline {
                language,
                temperature,
            } => {
                ensure!(
                    matches!(
                        language.as_str(),
                        "english" | "french" | "german" | "italian" | "portuguese" | "spanish"
                    ) && temperature.is_finite()
                        && *temperature > 0.0,
                    "Unsupported PocketTTS load language or temperature"
                );
            }
            Parameters::Qwen3AsrOffline {
                languages,
                default_language,
                max_tokens,
            } => {
                ensure!(
                    languages.contains(default_language)
                        && (1..=i32::MAX as u32).contains(max_tokens)
                        && languages
                            .iter()
                            .map(|l| l.native_value())
                            .collect::<BTreeSet<_>>()
                            .len()
                            == languages.len(),
                    "Invalid ASR languages, default or token limit"
                );
            }
        }
        let id = Sha256Digest::of_serializable(&(&d, protocol.schema_id, protocol.spec.sha256))?;
        models.insert(format!("audio-profile-{}", id.as_str()), d);
    }
    Ok(models)
}
