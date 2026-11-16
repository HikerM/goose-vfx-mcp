use super::TaskPublicationContext;
use crate::lifecycle::{Sha256Digest, StableAudioMusicSnapshot, StableAudioSampler};
use anyhow::Result;
use std::sync::LazyLock;

const RULE_VERSION: &str = "project-music-prompt-seed-domain-v1";
const RUNTIME_DIGEST: &str = "08fbaa83832c0ae65547555fa1bfeb6f2307ab3b1ee573b18cc2f68a0fb1712b";
const COMPONENTS_DIGEST: &str = "e2f65ea1eb78df9d6fe53f50b0dc19cd360a93dd532b48c9b78c6841393f6071";
static RULE_SOURCE_DIGEST: LazyLock<Sha256Digest> =
    LazyLock::new(|| rule_source_digest(include_str!("music_conditions.rs")));

fn rule_source_digest(source: &str) -> Sha256Digest {
    Sha256Digest::of_bytes(source.replace("\r\n", "\n").as_bytes())
}

// The 2026-11-15 fresh-task boundary campaign identifies this candidate domain.
// It does not provide V2 admission: that requires a separately measured profile
// with the publication context and payload tier included in its resource key.
pub(super) fn project_workload(
    snapshot: &StableAudioMusicSnapshot,
    publication: &TaskPublicationContext,
) -> Result<Option<Sha256Digest>> {
    if !matches!(
        publication,
        TaskPublicationContext::ProjectParameterReuseV2 { .. }
    ) {
        return Ok(None);
    }
    let metadata = &snapshot.metadata;
    let parameters = &snapshot.parameters;
    let prompt = &parameters.prompt;
    if prompt.is_empty()
        || prompt.len() > 1024
        || prompt.trim().is_empty()
        || prompt.contains(['\0', '|'])
        || (prompt.starts_with("--") && !prompt.chars().any(char::is_whitespace))
        || parameters.negative_prompt.is_some()
        || parameters.duration_seconds != 4.0
        || parameters.steps != 8
        || parameters.guidance_scale != 1.0
        || parameters.apg_scale != 1.0
        || parameters.sampler != StableAudioSampler::Pingpong
        || parameters.duration_padding_seconds != 6.0
        || !parameters.truncate_to_duration
        || !parameters.chunked_decode
        || metadata.backend != "cpu"
        || metadata.cpu_threads != 4
        || !metadata.requires_new_native_preparation
    {
        return Ok(None);
    }
    let binding = &metadata.binding;
    let runtime = Sha256Digest::of_serializable(&binding.runtime)?;
    let components = Sha256Digest::of_serializable(&(
        &binding.model.sha256,
        binding.model.size_bytes,
        &binding.model_spec.sha256,
        binding.model_spec.size_bytes,
    ))?;
    if runtime.as_str() != RUNTIME_DIGEST || components.as_str() != COMPONENTS_DIGEST {
        return Ok(None);
    }
    domain_digest(snapshot, &runtime, &components, &RULE_SOURCE_DIGEST).map(Some)
}

fn domain_digest(
    snapshot: &StableAudioMusicSnapshot,
    runtime: &Sha256Digest,
    components: &Sha256Digest,
    rule_source: &Sha256Digest,
) -> Result<Sha256Digest> {
    let mut parameters = snapshot.parameters.clone();
    parameters.prompt.clear();
    parameters.seed = 0;
    Sha256Digest::of_serializable(&(
        RULE_VERSION,
        rule_source,
        runtime,
        components,
        parameters,
        &snapshot.metadata.backend,
        snapshot.metadata.cpu_threads,
        snapshot.metadata.requires_new_native_preparation,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{
        AudioTaskAssetBinding, AudioTaskModelBinding, AudioTaskProfile, ProjectReusePayloadTier,
        StableAudioMusicParameterService,
    };
    use std::collections::BTreeSet;

    fn snapshot() -> StableAudioMusicSnapshot {
        let profile = AudioTaskProfile::StableAudioSmallMusicCpu;
        let (model_sha, model_bytes) = profile.recorded_model();
        let (spec_sha, spec_bytes) = profile.recorded_model_spec();
        let binding = AudioTaskModelBinding {
            installation_id: "synthetic-domain-installation".into(),
            version_id: "synthetic-domain-version".into(),
            manifest_digest: Sha256Digest::of_bytes(b"synthetic domain manifest"),
            runtime: profile.runtime_build(),
            target_capabilities: BTreeSet::from([profile.capability()]),
            model: AudioTaskAssetBinding {
                artifact_id: "synthetic-model".into(),
                sha256: model_sha,
                size_bytes: model_bytes,
            },
            model_spec: AudioTaskAssetBinding {
                artifact_id: "synthetic-spec".into(),
                sha256: spec_sha,
                size_bytes: spec_bytes,
            },
        };
        StableAudioMusicParameterService::new(binding)
            .unwrap()
            .compile_json(serde_json::json!({
                "sessionId":"synthetic-domain-session", "jobId":"synthetic-domain-job",
                "prompt":"A gentle solo piano melody.", "seed":42,
                "durationSeconds":4.0, "steps":8, "guidanceScale":1.0, "apgScale":1.0,
                "sampler":"pingpong", "durationPaddingSeconds":6.0,
                "truncateToDuration":true, "chunkedDecode":true
            }))
            .unwrap()
            .snapshot()
            .clone()
    }

    fn publication() -> TaskPublicationContext {
        TaskPublicationContext::ProjectParameterReuseV2 {
            implementation_sources_digest: Sha256Digest::of_bytes(b"synthetic host source"),
            payload_tier: ProjectReusePayloadTier::Small,
        }
    }

    #[test]
    fn candidate_requires_v2_and_does_not_change_session_or_v1_workloads() {
        let value = snapshot();
        assert!(project_workload(&value, &publication()).unwrap().is_some());
        for context in [
            TaskPublicationContext::Session,
            TaskPublicationContext::ProjectParameterReuseV1 {
                implementation_sources_digest: Sha256Digest::of_bytes(b"legacy host source"),
            },
        ] {
            assert!(project_workload(&value, &context).unwrap().is_none());
        }
    }

    #[test]
    fn prompt_and_seed_domain_keeps_one_digest_without_parent_or_job_identity() {
        let mut value = snapshot();
        let expected = project_workload(&value, &publication()).unwrap().unwrap();
        for (prompt, seed) in [
            ("x", 0),
            ("Warm orchestral strings.", 1),
            ("钢琴与弦乐", i32::MAX as u32),
            ("静かな音楽 🎵", i32::MAX as u32 + 1),
            ("A different final melody.", u32::MAX),
        ] {
            value.parameters.prompt = prompt.into();
            value.parameters.seed = seed;
            value.metadata.session_id = format!("synthetic-session-{seed}");
            value.metadata.job_id = format!("synthetic-job-{seed}");
            assert_eq!(
                project_workload(&value, &publication()).unwrap(),
                Some(expected.clone())
            );
        }
    }

    #[test]
    fn prompt_bound_uses_utf8_bytes_and_preserves_native_single_prompt_rules() {
        for prompt in [
            "a".repeat(1024),
            "猫".repeat(341),
            format!("{}a", "猫".repeat(341)),
        ] {
            let mut value = snapshot();
            value.parameters.prompt = prompt;
            assert!(project_workload(&value, &publication()).unwrap().is_some());
        }
        for prompt in [
            "a".repeat(1025),
            "猫".repeat(342),
            String::new(),
            " \t\n".into(),
            "first|second".into(),
            "piano\0".into(),
            "--help".into(),
        ] {
            let mut value = snapshot();
            value.parameters.prompt = prompt;
            assert!(project_workload(&value, &publication()).unwrap().is_none());
        }
    }

    #[test]
    fn every_unmeasured_fixed_parameter_falls_back_to_the_exact_workload() {
        let changes: &[(&str, fn(&mut StableAudioMusicSnapshot))] = &[
            ("negative prompt", |s| {
                s.parameters.negative_prompt = Some("vocals".into())
            }),
            ("empty negative prompt", |s| {
                s.parameters.negative_prompt = Some(String::new())
            }),
            ("duration", |s| s.parameters.duration_seconds = 5.0),
            ("steps", |s| s.parameters.steps = 9),
            ("guidance", |s| s.parameters.guidance_scale = 2.0),
            ("apg", |s| s.parameters.apg_scale = 2.0),
            ("sampler", |s| {
                s.parameters.sampler = StableAudioSampler::Euler
            }),
            ("padding", |s| s.parameters.duration_padding_seconds = 0.0),
            ("truncate", |s| s.parameters.truncate_to_duration = false),
            ("chunked decode", |s| s.parameters.chunked_decode = false),
            ("backend", |s| s.metadata.backend = "cuda".into()),
            ("threads", |s| s.metadata.cpu_threads = 8),
            ("preparation", |s| {
                s.metadata.requires_new_native_preparation = false
            }),
            ("nonfinite duration", |s| {
                s.parameters.duration_seconds = f32::NAN
            }),
        ];
        for (name, change) in changes {
            let mut value = snapshot();
            change(&mut value);
            assert!(
                project_workload(&value, &publication()).unwrap().is_none(),
                "{name}"
            );
        }
    }

    #[test]
    fn changed_runtime_or_component_identity_cannot_select_the_candidate_domain() {
        let changes: &[(&str, fn(&mut StableAudioMusicSnapshot))] = &[
            ("engine", |s| {
                s.metadata
                    .binding
                    .runtime
                    .engine_revision
                    .push_str("-other")
            }),
            ("binary", |s| {
                s.metadata.binding.runtime.binary_sha256 = Sha256Digest::of_bytes(b"other engine")
            }),
            ("model", |s| {
                s.metadata.binding.model.sha256 = Sha256Digest::of_bytes(b"other weights")
            }),
            ("model length", |s| s.metadata.binding.model.size_bytes += 1),
            ("model spec", |s| {
                s.metadata.binding.model_spec.sha256 = Sha256Digest::of_bytes(b"other spec")
            }),
            ("model spec length", |s| {
                s.metadata.binding.model_spec.size_bytes += 1
            }),
        ];
        for (name, change) in changes {
            let mut value = snapshot();
            change(&mut value);
            assert!(
                project_workload(&value, &publication()).unwrap().is_none(),
                "{name}"
            );
        }
    }

    #[test]
    fn domain_digest_binds_the_actual_rule_source_and_version() {
        let value = snapshot();
        let runtime = Sha256Digest::parse(RUNTIME_DIGEST).unwrap();
        let components = Sha256Digest::parse(COMPONENTS_DIGEST).unwrap();
        let source_text = include_str!("music_conditions.rs");
        let source = rule_source_digest(source_text);
        let windows_source = source_text.replace("\r\n", "\n").replace('\n', "\r\n");
        assert_eq!(source, rule_source_digest(&windows_source));
        assert_eq!(*RULE_SOURCE_DIGEST, source);
        let actual = project_workload(&value, &publication()).unwrap().unwrap();
        assert_eq!(
            actual,
            domain_digest(&value, &runtime, &components, &source).unwrap()
        );
        assert_ne!(
            actual,
            domain_digest(
                &value,
                &runtime,
                &components,
                &Sha256Digest::of_bytes(b"different condition implementation")
            )
            .unwrap()
        );
        let mut parameters = value.parameters.clone();
        parameters.prompt.clear();
        parameters.seed = 0;
        let changed_version = Sha256Digest::of_serializable(&(
            "project-music-prompt-seed-domain-v2",
            &source,
            &runtime,
            &components,
            parameters,
            &value.metadata.backend,
            value.metadata.cpu_threads,
            value.metadata.requires_new_native_preparation,
        ))
        .unwrap();
        assert_ne!(actual, changed_version);
    }
}
