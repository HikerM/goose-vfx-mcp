use super::*;
use anyhow::Result;

pub(super) fn core_intent(intent: NativeTaskIntentDto) -> domain::NativeTaskIntent {
    use domain::NativeTaskIntent as Core;
    match intent {
        NativeTaskIntentDto::TextToImage => Core::TextToImage,
        NativeTaskIntentDto::TextToVideo => Core::TextToVideo,
        NativeTaskIntentDto::TextToVideoWithAudio => Core::TextToVideoWithAudio,
        NativeTaskIntentDto::FirstFrameToVideoWithAudio => Core::FirstFrameToVideoWithAudio,
        NativeTaskIntentDto::SpeechSynthesis => Core::SpeechSynthesis,
        NativeTaskIntentDto::Transcription => Core::Transcription,
    }
}

fn sdk_intent(intent: domain::NativeTaskIntent) -> NativeTaskIntentDto {
    match intent {
        domain::NativeTaskIntent::TextToImage => NativeTaskIntentDto::TextToImage,
        domain::NativeTaskIntent::TextToVideo => NativeTaskIntentDto::TextToVideo,
        domain::NativeTaskIntent::TextToVideoWithAudio => NativeTaskIntentDto::TextToVideoWithAudio,
        domain::NativeTaskIntent::FirstFrameToVideoWithAudio => {
            NativeTaskIntentDto::FirstFrameToVideoWithAudio
        }
        domain::NativeTaskIntent::SpeechSynthesis => NativeTaskIntentDto::SpeechSynthesis,
        domain::NativeTaskIntent::Transcription => NativeTaskIntentDto::Transcription,
    }
}

pub(super) fn core_identity(i: NativeTaskIdentityDto) -> domain::TaskParameterIdentity {
    domain::TaskParameterIdentity {
        installation_id: i.installation_id,
        version_id: i.version_id,
        manifest_digest: i.manifest_digest,
        runtime_digest: i.runtime_digest,
        component_binding_digest: i.component_binding_digest,
        task_variant: i.task_variant,
        parameter_schema_id: i.parameter_schema_id,
    }
}

fn sdk_identity(i: domain::TaskParameterIdentity) -> NativeTaskIdentityDto {
    NativeTaskIdentityDto {
        installation_id: i.installation_id,
        version_id: i.version_id,
        manifest_digest: i.manifest_digest,
        runtime_digest: i.runtime_digest,
        component_binding_digest: i.component_binding_digest,
        task_variant: i.task_variant,
        parameter_schema_id: i.parameter_schema_id,
    }
}

// Project only descriptor fields defined in the SDK. Storage roots, helper paths,
// repository credentials and internal binding details have no wire representation.
pub(super) fn description(
    value: domain::InstalledTaskDescription,
) -> Result<NativeTaskDescriptionDto> {
    let descriptor = match value.descriptor {
        domain::NativeTaskParameterContract::Generation(v) => {
            NativeTaskParameterContractDto::Generation(Box::new(NativeGenerationDescriptorDto {
                schema_version: v.schema_version,
                profile: serde_json::from_value(serde_json::to_value(v.profile)?)?,
                fields: v
                    .fields
                    .into_iter()
                    .map(|(name, f)| Ok((name, serde_json::from_value(serde_json::to_value(f)?)?)))
                    .collect::<Result<_>>()?,
                execution_status: serde_json::from_value(serde_json::to_value(
                    v.execution_status,
                )?)?,
                evidence: v.evidence,
                limitations: v.limitations,
            }))
        }
        domain::NativeTaskParameterContract::PocketTts(v) => {
            NativeTaskParameterContractDto::PocketTts(Box::new(NativePocketTtsDescriptorDto {
                fields: audio_fields(v.fields)?,
                unsupported: v.unsupported,
                model_language: v.model_language,
                output_sample_rate: v.output_sample_rate,
                evidence: v.evidence,
                limitations: v.limitations,
            }))
        }
        domain::NativeTaskParameterContract::Qwen3Asr(v) => {
            NativeTaskParameterContractDto::Qwen3Asr(Box::new(NativeQwen3AsrDescriptorDto {
                fields: audio_fields(v.fields)?,
                unsupported: v.unsupported,
                recognition_languages: serde_json::from_value(serde_json::to_value(
                    v.recognition_languages,
                )?)?,
                evidence: v.evidence,
                limitations: v.limitations,
            }))
        }
    };
    Ok(NativeTaskDescriptionDto {
        identity: sdk_identity(value.identity),
        intent: sdk_intent(value.intent),
        descriptor,
    })
}

fn audio_fields(
    fields: std::collections::BTreeMap<String, domain::AudioParameterField>,
) -> Result<std::collections::BTreeMap<String, NativeAudioFieldDto>> {
    Ok(serde_json::from_value(serde_json::to_value(fields)?)?)
}
