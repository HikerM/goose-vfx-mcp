use super::managed_validation::{
    engine_location_digest, report_directory, status_for_installation, ReportBinding,
    ValidationProfile,
};
use super::*;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

const SUITE: &str = include_str!("../../tests/fixtures/vision-a43-v1/suite.json");
macro_rules! image {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!(
                "../../tests/fixtures/vision-a43-v1/",
                $name,
                ".png"
            ))
            .as_slice(),
        )
    };
}
const IMAGES: &[(&str, &[u8])] = &[
    image!("solid-red"),
    image!("solid-blue"),
    image!("dots-2"),
    image!("dots-3"),
    image!("triangle"),
    image!("square"),
    image!("red-left"),
    image!("red-right"),
    image!("word-cat"),
    image!("word-dog"),
];

#[derive(Deserialize)]
struct Case {
    id: String,
    sha256: Sha256Digest,
    prompt: String,
    answers: Vec<String>,
}
#[derive(Deserialize)]
struct Dialog {
    image: String,
    followup: String,
    answers: Vec<String>,
}
#[derive(Deserialize)]
struct Suite {
    schema: String,
    minimum_correct: usize,
    cases: Vec<Case>,
    replacements: Vec<[String; 2]>,
    dialogs: Vec<Dialog>,
}

fn suite_digest() -> Sha256Digest {
    Sha256Digest::of_bytes(SUITE.replace("\r\n", "\n").as_bytes())
}

fn bytes(id: &str) -> Result<&'static [u8]> {
    IMAGES
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, data)| *data)
        .context("Missing vision fixture")
}

impl Suite {
    fn load() -> Result<Self> {
        let suite: Self = serde_json::from_str(SUITE)?;
        ensure!(
            suite.schema == "lumina-vision-a43-v1"
                && suite.minimum_correct == 8
                && suite.cases.len() == 10
                && suite.replacements.len() == 4
                && suite.dialogs.len() == 3,
            "Invalid vision suite"
        );
        let mut cases = BTreeMap::new();
        for case in &suite.cases {
            ensure!(
                Sha256Digest::of_bytes(bytes(&case.id)?) == case.sha256
                    && !case.answers.is_empty()
                    && cases.insert(&case.id, case).is_none(),
                "Invalid vision fixture identity"
            );
        }
        for [left, right] in &suite.replacements {
            let a = cases.get(left).context("Missing replacement image")?;
            let b = cases.get(right).context("Missing replacement image")?;
            ensure!(
                a.prompt == b.prompt
                    && a.sha256 != b.sha256
                    && !a.answers.iter().any(|answer| b.answers.contains(answer)),
                "Ambiguous image replacement control"
            );
        }
        ensure!(
            suite.dialogs.iter().all(|d| cases.contains_key(&d.image)
                && !d.followup.is_empty()
                && !d.answers.is_empty()),
            "Invalid visual dialog"
        );
        Ok(suite)
    }
}

fn matches_answer(response: &NativeChatResponse, answers: &[String], sha: &Sha256Digest) -> bool {
    let Some(answer) = response.content.as_deref() else {
        return false;
    };
    let normalized = answer.trim().trim_end_matches(['.', '。']).trim();
    response.finish_reason == NativeChatFinishReason::Stop
        && response.tool_calls.is_empty()
        && response.image_digests.as_slice() == std::slice::from_ref(sha)
        && answers
            .iter()
            .any(|value| normalized.eq_ignore_ascii_case(value))
}

#[derive(Serialize)]
struct Turn {
    id: String,
    request_digest: Sha256Digest,
    response: NativeChatResponse,
    passed: bool,
}
#[derive(Serialize, Default)]
struct Results {
    images: Vec<Turn>,
    dialogs: Vec<Turn>,
    replacements_passed: usize,
}

impl Results {
    async fn run(
        &mut self,
        adapter: &NativeChatAdapter<'_>,
        suite: &Suite,
        timeout: Duration,
    ) -> Result<()> {
        for case in &suite.cases {
            let mut request = NativeChatRequest::text(&case.prompt, 32);
            request.add_png(bytes(&case.id)?.to_vec())?;
            let response = adapter.complete(&request, timeout).await?;
            self.images.push(Turn {
                id: case.id.clone(),
                request_digest: response.request_digest.clone(),
                passed: matches_answer(&response, &case.answers, &case.sha256),
                response,
            });
        }
        let passed: BTreeSet<_> = self
            .images
            .iter()
            .filter(|turn| turn.passed)
            .map(|turn| turn.id.as_str())
            .collect();
        self.replacements_passed = suite
            .replacements
            .iter()
            .filter(|pair| pair.iter().all(|id| passed.contains(id.as_str())))
            .count();
        for dialog in &suite.dialogs {
            let case = suite
                .cases
                .iter()
                .find(|c| c.id == dialog.image)
                .context("Missing visual dialog fixture")?;
            let first = self
                .images
                .iter()
                .find(|turn| turn.id == case.id)
                .context("Missing first visual turn")?;
            let mut request = NativeChatRequest::text(&case.prompt, 32);
            request.add_png(bytes(&case.id)?.to_vec())?;
            request.messages.push(NativeChatMessage::Assistant {
                content: first.response.content.clone(),
                tool_calls: first.response.tool_calls.clone(),
            });
            request.messages.push(NativeChatMessage::User {
                content: vec![NativeChatContent::Text {
                    text: dialog.followup.clone(),
                }],
            });
            let response = adapter.complete(&request, timeout).await?;
            self.dialogs.push(Turn {
                id: case.id.clone(),
                request_digest: response.request_digest.clone(),
                passed: first.passed && matches_answer(&response, &dialog.answers, &case.sha256),
                response,
            });
        }
        Ok(())
    }
}

fn installed_vision(
    repository: &LifecycleRepository,
    version_id: &str,
) -> Result<(InstalledVersion, LaunchConfig)> {
    let installation = repository
        .installed_version(version_id)?
        .context("Missing registered vision installation")?;
    let launch = LaunchConfig::for_measured_cpu_installation(&installation)?;
    ensure!(
        launch.capabilities.contains(&ModelCapability::Vision)
            && launch
                .component_files
                .get(&ComponentRole::VisionProjector)
                .is_some_and(|files| !files.is_empty()),
        "Vision validation needs an admitted projector"
    );
    Ok((installation, launch))
}

#[derive(Serialize)]
pub struct ManagedVisionValidation {
    pub evidence: CapabilityEvidence,
    pub correct: usize,
    pub completed: usize,
    pub replacements_passed: usize,
    pub dialogs_passed: usize,
    pub dialogs_completed: usize,
    pub failure: Option<String>,
}

pub async fn run_managed_vision_validation(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    version_id: &str,
    engine: &Path,
    timeout: Duration,
) -> Result<ManagedVisionValidation> {
    ensure!(
        !timeout.is_zero() && timeout <= Duration::from_secs(180),
        "Vision timeout must be positive and at most 180 seconds"
    );
    let suite = Suite::load()?;
    let (installation, launch) = installed_vision(repository, version_id)?;
    let profile = ValidationProfile::Vision;
    let _validation = store.lock(profile.directory(), &launch.installation_id)?;
    let logs = report_directory(store, profile.directory())?.join(uuid::Uuid::new_v4().to_string());
    let location = engine_location_digest(engine)?;
    let mut runtime =
        NativeHelperRuntime::start(repository, store, launch, engine, &logs, timeout).await?;
    let launch = runtime.actual_config().clone();
    let mut cases = Results::default();
    let result = async {
        cases
            .run(&NativeChatAdapter::new(&runtime)?, &suite, timeout)
            .await
    }
    .await;
    runtime
        .stop()
        .context("Stopping the owned vision validation helper")?;
    let correct = cases.images.iter().filter(|c| c.passed).count();
    let dialogs_passed = cases.dialogs.iter().filter(|c| c.passed).count();
    let status = if result.is_ok()
        && cases.images.len() == suite.cases.len()
        && correct >= suite.minimum_correct
        && cases.replacements_passed == suite.replacements.len()
        && dialogs_passed == suite.dialogs.len()
    {
        CapabilityStatus::Ready
    } else {
        CapabilityStatus::Failed
    };
    let failure = result.err().map(|err| format!("{err:#}"));
    let binding = ReportBinding::new(&launch, profile, suite_digest(), location, status)?;
    let evidence=binding.register(repository,store,&serde_json::json!({"binding":binding,"actual_launch":launch,"actual_launch_digest":launch.digest()?,
        "installed_files":installation.manifest.installed_files,"cases":cases,"correct":correct,"minimum":suite.minimum_correct,"failure":failure,
        "log_directory":logs.file_name().and_then(|p|p.to_str())}))?;
    Ok(ManagedVisionValidation {
        evidence,
        correct,
        completed: cases.images.len(),
        replacements_passed: cases.replacements_passed,
        dialogs_passed,
        dialogs_completed: cases.dialogs.len(),
        failure,
    })
}

pub fn managed_vision_status(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    version_id: &str,
    engine: &Path,
) -> Result<ManagedCapabilityStatus> {
    let (installed, launch) = installed_vision(repository, version_id)?;
    status_for_installation(
        repository,
        store,
        &installed,
        &launch,
        engine,
        ValidationProfile::Vision,
        suite_digest(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_images_decode_with_the_actual_measured_vision_input_limits() {
        let suite = Suite::load().unwrap();
        let workload = VisionResourceWorkload::measured_qwen35_cpu();
        for case in &suite.cases {
            let images = workload
                .inspect_images(&[bytes(&case.id).unwrap()])
                .unwrap();
            assert_eq!(images.len(), 1);
            assert_eq!(images[0].sha256(), &case.sha256);
        }
    }

    #[test]
    fn visual_answer_requires_the_actual_image_and_a_completed_text_reply() {
        let suite = Suite::load().unwrap();
        let red = &suite.cases[0];
        let mut reply = NativeChatResponse {
            id: "test".into(),
            content: Some(" RED. ".into()),
            tool_calls: vec![],
            finish_reason: NativeChatFinishReason::Stop,
            usage: NativeChatUsage {
                prompt_tokens: 40,
                completion_tokens: 2,
                total_tokens: 42,
            },
            launch_digest: Sha256Digest::of_bytes(b"launch"),
            request_digest: Sha256Digest::of_bytes(b"request"),
            image_digests: vec![red.sha256.clone()],
        };
        assert!(matches_answer(&reply, &red.answers, &red.sha256));
        assert!(!matches_answer(
            &reply,
            &red.answers,
            &suite.cases[1].sha256
        ));
        reply.finish_reason = NativeChatFinishReason::Length;
        assert!(!matches_answer(&reply, &red.answers, &red.sha256));
    }
}
