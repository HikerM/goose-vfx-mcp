use super::{LifecycleErrorCode, ModelSourceError};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedWeightOrigin {
    ModelScope,
    HuggingFace,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedWeightFormat {
    Gguf,
    Safetensors,
    Other,
}

pub fn require_managed_weight_policy(
    origin: ManagedWeightOrigin,
    format: ManagedWeightFormat,
) -> Result<()> {
    ensure!(
        origin == ManagedWeightOrigin::ModelScope && format == ManagedWeightFormat::Gguf,
        ModelSourceError {
            code: LifecycleErrorCode::FormatForbidden,
            http_status: None
        }
    );
    Ok(())
}
