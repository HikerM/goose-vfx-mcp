use super::NativeTaskParameterContract;
use crate::lifecycle::{AudioParameterField, GenerationParameterSupport};
use anyhow::{bail, ensure, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMediaParameterKind {
    Image,
    Audio,
}

impl NativeTaskParameterContract {
    pub fn session_media_kind(&self, name: &str) -> Result<Option<SessionMediaParameterKind>> {
        let value_type = match self {
            Self::Generation(desc) => {
                let field = desc
                    .fields
                    .get(name)
                    .ok_or_else(|| anyhow::anyhow!("Unknown task parameter: {name}"))?;
                ensure!(
                    field.support == GenerationParameterSupport::NativeSupported,
                    "Task parameter is not supported by the installed model: {name}"
                );
                field.value_type.as_str()
            }
            Self::PocketTts(desc) => audio_field_type(&desc.fields, &desc.unsupported, name)?,
            Self::Qwen3Asr(desc) => audio_field_type(&desc.fields, &desc.unsupported, name)?,
        };
        match value_type {
            // Existing diffusion descriptors use this spelling for the image reference wire format.
            "session_media_reference" | "session_image_reference" => {
                Ok(Some(SessionMediaParameterKind::Image))
            }
            "session_audio_reference" => Ok(Some(SessionMediaParameterKind::Audio)),
            ty if ty.starts_with("session_") && ty.ends_with("_reference") => {
                bail!("Unsupported session media reference type for {name}: {ty}")
            }
            _ => Ok(None),
        }
    }
}

fn audio_field_type<'a>(
    fields: &'a BTreeMap<String, AudioParameterField>,
    unsupported: &BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str> {
    if let Some(reason) = unsupported.get(name) {
        bail!("Unsupported task parameter {name}: {reason}");
    }
    fields
        .get(name)
        .map(|field| field.value_type.as_str())
        .ok_or_else(|| anyhow::anyhow!("Unknown task parameter: {name}"))
}
