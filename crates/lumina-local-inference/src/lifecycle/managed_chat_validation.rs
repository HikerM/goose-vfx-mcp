use super::managed_validation::{
    engine_location_digest, report_directory, status_for_installation, ReportBinding,
    ValidationProfile,
};
use super::{
    classify_gguf, gguf_header_evidence_id, CapabilityEvidence, CapabilityStatus, ComponentRole,
    InstallationStore, InstalledVersion, LaunchConfig, LifecycleRepository,
    ManagedCapabilityStatus, ModelCapability, ModelStructure, NativeChatAdapter, NativeChatContent,
    NativeChatFinishReason, NativeChatMessage, NativeChatRequest, NativeChatResponse,
    NativeHelperRuntime, Sha256Digest,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

const CORPUS: &str = include_str!("../../tests/fixtures/chat-a41-v1.json");

fn input_digest() -> Sha256Digest {
    Sha256Digest::of_bytes(CORPUS.replace("\r\n", "\n").as_bytes())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Suite {
    schema: String,
    minimum_correct: usize,
    max_output_tokens: u32,
    cases: Vec<Single>,
    dialogs: Vec<Dialog>,
    limit_probe: Limit,
}
#[derive(Deserialize)]
struct Single {
    id: String,
    #[serde(flatten)]
    task: Turn,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    prompt: String,
    answers: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dialog {
    id: String,
    turns: Vec<Turn>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Limit {
    prompt: String,
    max_output_tokens: u32,
}

impl Suite {
    fn load() -> Result<Self> {
        let suite: Self = serde_json::from_str(CORPUS)?;
        let ids: BTreeSet<_> = suite
            .cases
            .iter()
            .map(|case| &case.id)
            .chain(suite.dialogs.iter().map(|d| &d.id))
            .collect();
        ensure!(
            suite.schema == "managed-chat-a41-v1"
                && suite.minimum_correct == 8
                && suite.max_output_tokens == 64
                && suite.cases.len() == 10
                && suite.dialogs.len() == 3
                && ids.len() == 13
                && suite
                    .dialogs
                    .iter()
                    .all(|d| (2..=3).contains(&d.turns.len()))
                && suite.limit_probe.max_output_tokens == 1,
            "Invalid frozen A41 chat suite"
        );
        Ok(suite)
    }
}

fn normalized(text: &str) -> String {
    text.trim()
        .trim_end_matches(['.', '。', '!', '！'])
        .trim_end()
        .to_lowercase()
}

impl Turn {
    fn accepts(&self, response: &NativeChatResponse) -> bool {
        response.finish_reason == NativeChatFinishReason::Stop
            && response.tool_calls.is_empty()
            && response.content.as_deref().is_some_and(|content| {
                self.answers
                    .iter()
                    .any(|answer| normalized(answer) == normalized(content))
            })
    }
}

#[derive(Serialize)]
struct ChatClassification {
    structure: ModelStructure,
    rules: String,
    primary_gguf_observation: Sha256Digest,
    gguf_classification_digest: Sha256Digest,
}

fn chat_installation(
    repository: &LifecycleRepository,
    version_id: &str,
) -> Result<(InstalledVersion, LaunchConfig, ChatClassification)> {
    let installed = repository
        .installed_version(version_id)?
        .context("Chat validation requires a registered installation")?;
    let plan = &installed.manifest.plan;
    let launch = LaunchConfig::for_measured_cpu_installation(&installed)?;
    ensure!(
        launch.capabilities.contains(&ModelCapability::Text),
        "Installation does not admit text chat"
    );
    let main = &plan.dependencies.components[&plan.dependencies.root];
    ensure!(
        main.role == ComponentRole::Main,
        "Text chat requires a main GGUF component"
    );
    let mut primary = None;
    let mut observed = BTreeSet::new();
    for value in main
        .evidence_ids
        .iter()
        .filter_map(|e| e.strip_prefix("gguf-observation:"))
    {
        let id = Sha256Digest::parse(value)?;
        let record = repository
            .read_gguf_observation(&id)?
            .context("Missing GGUF observation bound to the installed main component")?;
        let file = record.observation;
        ensure!(
            main.artifact_ids.contains(&file.artifact.id)
                && observed.insert(file.artifact.id.clone())
                && plan.artifacts.get(&file.artifact.id) == Some(&file.artifact)
                && plan
                    .snapshots
                    .get(&file.artifact.snapshot_id)
                    .context("Missing main artifact snapshot")?
                    .content_digest()?
                    == file.snapshot.content_digest()?
                && main.evidence_ids.contains(&gguf_header_evidence_id(
                    &file.artifact.id,
                    &file.header.header_digest
                )),
            "Installed component differs from its bound GGUF evidence"
        );
        if file
            .header
            .shard
            .as_ref()
            .is_none_or(|shard| shard.index == 0)
        {
            ensure!(
                primary.is_none(),
                "Main component has multiple primary GGUF observations"
            );
            let classified = classify_gguf(
                &file.artifact,
                &file.artifact.snapshot_id,
                &file.snapshot,
                &file.header,
                &[],
            )?;
            ensure!(
                matches!(
                    classified.structure,
                    ModelStructure::Dense | ModelStructure::Moe
                ),
                "GGUF metadata does not establish a Dense or MoE structure"
            );
            primary = Some(ChatClassification {
                structure: classified.structure,
                gguf_classification_digest: classified.digest()?,
                rules: classified.rules_version,
                primary_gguf_observation: id,
            });
        }
    }
    ensure!(
        main.artifact_ids == observed,
        "Missing bound GGUF observations for the installed text component"
    );
    Ok((
        installed,
        launch,
        primary.context("Missing primary GGUF classification")?,
    ))
}

#[derive(Debug, Serialize)]
pub struct ManagedChatValidation {
    pub evidence: CapabilityEvidence,
    pub structure: ModelStructure,
    pub correct: usize,
    pub completed: usize,
    pub minimum: usize,
    pub dialogs_passed: usize,
    pub dialogs_completed: usize,
    pub output_limit_passed: bool,
    pub failure: Option<String>,
}

#[derive(Serialize)]
struct ObservedTurn {
    request: NativeChatRequest,
    response: NativeChatResponse,
    passed: bool,
}
#[derive(Serialize)]
struct Transcript<'a> {
    id: &'a str,
    turns: Vec<ObservedTurn>,
    passed: bool,
}
#[derive(Default, Serialize)]
struct Observations<'a> {
    single: Vec<Transcript<'a>>,
    dialogs: Vec<Transcript<'a>>,
    limit: Option<ObservedTurn>,
}

fn next_turn(previous: &ObservedTurn, prompt: &str) -> Result<NativeChatRequest> {
    let mut request = previous.request.clone();
    let content = previous
        .response
        .content
        .as_ref()
        .context("Missing real assistant response for the next turn")?;
    request.messages.push(NativeChatMessage::Assistant {
        content: Some(content.clone()),
        tool_calls: vec![],
    });
    request.messages.push(NativeChatMessage::User {
        content: vec![NativeChatContent::Text {
            text: prompt.to_owned(),
        }],
    });
    Ok(request)
}

async fn check_turn(
    adapter: &NativeChatAdapter<'_>,
    request: NativeChatRequest,
    expected: &Turn,
    digest: &Sha256Digest,
    timeout: Duration,
) -> Result<ObservedTurn> {
    let response = adapter.complete(&request, timeout).await?;
    ensure!(
        &response.launch_digest == digest,
        "Text response belongs to a different helper"
    );
    let passed = expected.accepts(&response);
    Ok(ObservedTurn {
        request,
        response,
        passed,
    })
}

impl<'a> Observations<'a> {
    async fn run(
        &mut self,
        suite: &'a Suite,
        runtime: &NativeHelperRuntime,
        timeout: Duration,
    ) -> Result<()> {
        let adapter = NativeChatAdapter::new(runtime)?;
        let digest = runtime.actual_config().digest()?;
        for case in &suite.cases {
            let request = NativeChatRequest::text(&case.task.prompt, suite.max_output_tokens);
            let turn = check_turn(&adapter, request, &case.task, &digest, timeout)
                .await
                .with_context(|| format!("Single-turn case {}", case.id))?;
            self.single.push(Transcript {
                id: &case.id,
                passed: turn.passed,
                turns: vec![turn],
            });
        }
        for dialog in &suite.dialogs {
            self.dialogs.push(Transcript {
                id: &dialog.id,
                turns: Vec::new(),
                passed: false,
            });
            let transcript = self.dialogs.last_mut().expect("just inserted");
            for (index, task) in dialog.turns.iter().enumerate() {
                let request = match transcript.turns.last() {
                    Some(previous) => next_turn(previous, &task.prompt)?,
                    None => NativeChatRequest::text(&task.prompt, suite.max_output_tokens),
                };
                let turn = check_turn(&adapter, request, task, &digest, timeout)
                    .await
                    .with_context(|| format!("Dialog {}, turn {}", dialog.id, index + 1))?;
                transcript.turns.push(turn);
            }
            transcript.passed = transcript.turns.iter().all(|t| t.passed);
        }
        let request = NativeChatRequest::text(
            &suite.limit_probe.prompt,
            suite.limit_probe.max_output_tokens,
        );
        let response = adapter
            .complete(&request, timeout)
            .await
            .context("One-token output-limit probe")?;
        ensure!(
            response.launch_digest == digest,
            "Output limit probe used a different launch"
        );
        let passed = limit_passed(&response);
        self.limit = Some(ObservedTurn {
            request,
            response,
            passed,
        });
        Ok(())
    }
}

fn limit_passed(response: &NativeChatResponse) -> bool {
    response.finish_reason == NativeChatFinishReason::Length
        && response.usage.completion_tokens == 1
        && response.tool_calls.is_empty()
}

fn passed(
    correct: usize,
    singles: usize,
    dialogs: usize,
    limit: bool,
    runtime_error: bool,
    suite: &Suite,
) -> bool {
    !runtime_error
        && singles == suite.cases.len()
        && correct >= suite.minimum_correct
        && dialogs == suite.dialogs.len()
        && limit
}

pub async fn run_managed_chat_validation(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    version_id: &str,
    engine: &Path,
    timeout: Duration,
) -> Result<ManagedChatValidation> {
    ensure!(
        !timeout.is_zero() && timeout <= Duration::from_secs(180),
        "Chat validation timeout must be positive and at most 180 seconds per operation"
    );
    let suite = Suite::load()?;
    let (installed, launch, classification) = chat_installation(repository, version_id)?;
    let profile = ValidationProfile::Text(classification.structure);
    let _validation = store.lock(profile.directory(), &launch.installation_id)?;
    let logs = report_directory(store, profile.directory())?.join(uuid::Uuid::new_v4().to_string());
    let location = engine_location_digest(engine)?;
    let started = Instant::now();
    let mut runtime =
        NativeHelperRuntime::start(repository, store, launch, engine, &logs, timeout).await?;
    let launch = runtime.actual_config().clone();
    let mut observed = Observations::default();
    let result = observed.run(&suite, &runtime, timeout).await;
    runtime
        .stop()
        .context("Stopping the owned chat validation helper")?;
    let correct = observed.single.iter().filter(|case| case.passed).count();
    let completed = observed.single.len();
    let dialogs_passed = observed
        .dialogs
        .iter()
        .filter(|dialog| dialog.passed)
        .count();
    let dialogs_completed = observed
        .dialogs
        .iter()
        .zip(&suite.dialogs)
        .filter(|(seen, expected)| seen.turns.len() == expected.turns.len())
        .count();
    let output_limit_passed = observed.limit.as_ref().is_some_and(|limit| limit.passed);
    let status = if passed(
        correct,
        completed,
        dialogs_passed,
        output_limit_passed,
        result.is_err(),
        &suite,
    ) {
        CapabilityStatus::Ready
    } else {
        CapabilityStatus::Failed
    };
    let failure = result.err().map(|e| format!("{e:#}"));
    let binding = ReportBinding::new(&launch, profile, input_digest(), location, status)?;
    let report = serde_json::json!({
        "binding": binding,
        "actual_launch": launch,
        "actual_launch_digest": launch.digest()?,
        "target_capabilities_from_plan": installed.manifest.plan.target_capabilities,
        "tested_capability": ModelCapability::Text,
        "gguf_classification": classification,
        "installed_files": installed.manifest.installed_files,
        "rules_version": installed.manifest.plan.rules_version,
        "cases": observed,
        "required_content_correct": suite.minimum_correct,
        "correct": correct,
        "completed": completed,
        "dialogs_passed": dialogs_passed,
        "dialogs_completed": dialogs_completed,
        "output_limit_passed": output_limit_passed,
        "failure": failure,
        "elapsed_ms": started.elapsed().as_millis(),
        "log_directory": logs.file_name().and_then(|name| name.to_str()),
    });
    let evidence = binding.register(repository, store, &report)?;
    Ok(ManagedChatValidation {
        evidence,
        structure: classification.structure,
        correct,
        completed,
        minimum: suite.minimum_correct,
        dialogs_passed,
        dialogs_completed,
        output_limit_passed,
        failure,
    })
}

pub fn managed_chat_status(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    version_id: &str,
    engine: &Path,
) -> Result<ManagedCapabilityStatus> {
    let (installed, launch, classification) = chat_installation(repository, version_id)?;
    status_for_installation(
        repository,
        store,
        &installed,
        &launch,
        engine,
        ValidationProfile::Text(classification.structure),
        input_digest(),
    )
}

#[cfg(test)]
#[path = "../../tests/support/managed_chat_validation.rs"]
mod tests;
