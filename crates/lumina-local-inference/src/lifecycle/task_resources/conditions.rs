use super::*;
use crate::lifecycle::{EffectivePocketTtsParameters, PocketTtsSnapshot};
use std::sync::LazyLock;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Catalog {
    schema_version: u32,
    rules: Vec<Condition>,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Condition {
    PocketTtsFixedSteps {
        measurement_sha256: Sha256Digest,
        max_frames: u32,
        frames_after_eos: i32,
        short_text: TextCondition,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextCondition {
    shape: TextShape,
    required_chunk_tokens: u32,
    maximum_normalized_tokens: u32,
    digest_tag: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum TextShape {
    NormalizedAsciiSentence,
}

impl TextCondition {
    fn matches(&self, parameters: &EffectivePocketTtsParameters) -> bool {
        if parameters.text_chunk_tokens != self.required_chunk_tokens {
            return false;
        }
        match self.shape {
            TextShape::NormalizedAsciiSentence => {
                // The measured unigram model uses identity normalization, one prefix
                // marker and a normal vocabulary piece for every accepted character.
                let Some(words) = parameters.text.strip_suffix('.') else {
                    return false;
                };
                parameters.text.len() < self.maximum_normalized_tokens as usize
                    && words.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
                    && words.split(' ').all(|word| {
                        !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_alphabetic())
                    })
            }
        }
    }
}

struct PocketDomain {
    runtime: Sha256Digest,
    components: Sha256Digest,
    max_frames: u32,
    frames_after_eos: i32,
    short_text: TextCondition,
}

impl PocketDomain {
    fn applies(
        &self,
        runtime: &Sha256Digest,
        components: &Sha256Digest,
        max_frames: u32,
        frames_after_eos: i32,
    ) -> bool {
        self.runtime == *runtime
            && self.components == *components
            && self.max_frames == max_frames
            && self.frames_after_eos == frames_after_eos
    }
}

static POCKET_DOMAINS: LazyLock<Result<Vec<PocketDomain>, String>> =
    LazyLock::new(|| load().map_err(|error| error.to_string()));

fn load() -> Result<Vec<PocketDomain>> {
    let body = include_str!("pocket-tts-conditions.json");
    ensure!(
        Sha256Digest::of_bytes(body.as_bytes()).as_str()
            == "aec0984fc1ed9c02c4a5b39d2f413eabbb3d5f102e5547144dd45cc067b5f79b",
        "Reviewed workload conditions changed"
    );
    let catalog: Catalog = serde_json::from_str(body)?;
    ensure!(
        catalog.schema_version == 1,
        "Unknown workload conditions schema"
    );
    let profiles = trusted_profiles()?;
    let mut domains = Vec::new();
    for condition in catalog.rules {
        let Condition::PocketTtsFixedSteps {
            measurement_sha256,
            max_frames,
            frames_after_eos,
            short_text,
        } = condition;
        let profile = profiles
            .iter()
            .find(|profile| profile.evidence_sha256 == measurement_sha256)
            .ok_or_else(|| {
                anyhow::anyhow!("Workload condition references an unregistered measurement")
            })?;
        ensure!(
            profile.key.kind == TaskResourceKind::Tts
                && !profile.key.requires_gpu
                && max_frames > 0
                && frames_after_eos >= 0
                && frames_after_eos as u32 >= max_frames
                && short_text.maximum_normalized_tokens > 0
                && short_text.maximum_normalized_tokens <= short_text.required_chunk_tokens
                && !short_text.digest_tag.is_empty(),
            "Workload condition is inconsistent with its measured task"
        );
        let domain = PocketDomain {
            runtime: profile.key.runtime_digest.clone(),
            components: profile.key.components_digest.clone(),
            max_frames,
            frames_after_eos,
            short_text,
        };
        ensure!(
            !domains.iter().any(|other: &PocketDomain| {
                other.runtime == domain.runtime
                    && other.components == domain.components
                    && other.max_frames == domain.max_frames
                    && other.frames_after_eos == domain.frames_after_eos
            }),
            "Duplicate measured workload condition"
        );
        domains.push(domain);
    }
    Ok(domains)
}

pub(super) fn pocket_key(
    snapshot: &PocketTtsSnapshot,
) -> Result<(Sha256Digest, Sha256Digest, Sha256Digest)> {
    let b = &snapshot.metadata.binding;
    let runtime = Sha256Digest::of_serializable(&b.runtime)?;
    let components = Sha256Digest::of_serializable(&(
        &b.model.sha256,
        b.model.size_bytes,
        &b.model_spec.sha256,
        b.model_spec.size_bytes,
    ))?;
    let domains = POCKET_DOMAINS
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut parameters = snapshot.parameters.clone();
    parameters.voice_reference.session_id.clear();
    parameters.voice_reference.result_id.clear();
    let domain = domains.iter().find(|domain| {
        domain.applies(
            &runtime,
            &components,
            parameters.max_frames,
            parameters.frames_after_eos,
        )
    });
    if domain.is_some() {
        parameters.seed = 0;
    }
    let short_text = domain
        .map(|domain| &domain.short_text)
        .filter(|condition| condition.matches(&parameters));
    if short_text.is_some() {
        parameters.text.clear();
    }
    let workload = (
        &parameters,
        &snapshot.metadata.backend,
        snapshot.metadata.cpu_threads,
        snapshot.metadata.requires_new_native_preparation,
    );
    let digest = match short_text {
        Some(condition) => Sha256Digest::of_serializable(&(&condition.digest_tag, workload))?,
        None => Sha256Digest::of_serializable(&workload)?,
    };
    Ok((runtime, components, digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_reviewed_engine_and_artifact_pair_can_select_its_shape_rule() {
        let domains = load().unwrap();
        for domain in domains {
            let matches = |runtime, components| {
                domain.applies(
                    runtime,
                    components,
                    domain.max_frames,
                    domain.frames_after_eos,
                )
            };
            assert!(matches(&domain.runtime, &domain.components));
            let unknown = Sha256Digest::of_bytes(b"unmeasured artifact or engine");
            assert!(!matches(&domain.runtime, &unknown));
            assert!(!matches(&unknown, &domain.components));
            assert!(!domain.applies(
                &domain.runtime,
                &domain.components,
                domain.max_frames - 1,
                domain.frames_after_eos
            ));
        }
    }
}
