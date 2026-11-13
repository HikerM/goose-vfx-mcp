use super::*;
use crate::lifecycle::GenerationParameterSnapshot;
use std::sync::LazyLock;

// This pinned record identifies the exact model components, engine and historical
// workload. New resource records are still required for the broader key to admit.
const CLIP_REFERENCE: &str = "aac4362892ac3c7bd151a9f7cec264a8c0217eec84f18b791ae6e96085154f29";
static CLIP_KEY: LazyLock<Result<TaskResourceKey, String>> = LazyLock::new(|| {
    trusted_profiles()
        .and_then(|profiles| {
            profiles
                .into_iter()
                .find(|profile| profile.evidence_sha256.as_str() == CLIP_REFERENCE)
                .map(|profile| profile.key)
                .ok_or_else(|| anyhow::anyhow!("Reviewed CLIP reference is missing"))
        })
        .map_err(|error| error.to_string())
});

fn single_clip_chunk(text: &str) -> bool {
    // This CLIP tokenizer starts with one symbol per UTF-8 byte and only merges.
    // Plain text cannot inject prompt weights, extra chunks or external embeddings.
    !text.is_empty()
        && text.len() <= 75
        && text.chars().any(char::is_alphanumeric)
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | ',' | '.'))
        && !text
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| matches!(word, "BREAK" | "AND"))
}

pub(super) fn generation_workload(
    snapshot: &GenerationParameterSnapshot,
    parameters: &EffectiveGenerationParameters,
    runtime: &Sha256Digest,
    components: &Sha256Digest,
) -> Result<Sha256Digest> {
    let exact = Sha256Digest::of_serializable(&(&snapshot.profile, parameters, &snapshot.engine))?;
    let EffectiveGenerationParameters::Image(image) = parameters else {
        return Ok(exact);
    };
    if !single_clip_chunk(&image.prompt) || !single_clip_chunk(&image.negative_prompt) {
        return Ok(exact);
    }
    let reference = CLIP_KEY
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    if reference.runtime_digest != *runtime
        || reference.components_digest != *components
        || reference.workload_digest == exact
    {
        return Ok(exact);
    }
    let mut bounded = image.clone();
    bounded.prompt.clear();
    bounded.negative_prompt.clear();
    bounded.seed = 0;
    Sha256Digest::of_serializable(&(
        "sd-clip-one-77-position-chunk-v1",
        &snapshot.profile,
        bounded,
        &snapshot.engine,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_bounds_count_utf8_bytes_and_exclude_prompt_syntax() {
        for text in ["cat", "A red apple.", "木桌上的苹果", &"1".repeat(75)] {
            assert!(single_clip_chunk(text));
        }
        for text in [
            "",
            "   ",
            "...",
            &"1".repeat(76),
            &"猫".repeat(26),
            "a BREAK cat",
            "cat AND dog",
            "(cat:1.5)",
            "<embedding:cat>",
            "cat\ncity",
        ] {
            assert!(!single_clip_chunk(text));
        }
    }
}
