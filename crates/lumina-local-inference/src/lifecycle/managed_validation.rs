use super::files::{checked_local_path, open_for_verification};
use super::{
    validate_vector_admission, CapabilityEvidence, CapabilityStatus, EmbeddingBatch,
    InstallationStore, InstalledVersion, LaunchConfig, LifecycleRepository, ModelCapability,
    NativeEmbeddingAdapter, NativeHelperRuntime, NativeRerankAdapter, RepositoryPath, RerankBatch,
    RuntimeExecutionMode, Sha256Digest, ValidationIdentity, VectorRuntimeProfile,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const INPUTS: &str = include_str!("../../tests/fixtures/vector-relevance-v1.json");
const SUITE_VERSION: &str = "managed-vector-relevance-v1";
const REPORT_DIR: &str = "vector-validation";
const MAX_REPORT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Suite {
    schema: String,
    minimum_top1_correct: usize,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    id: String,
    query: String,
    documents: Vec<String>,
    expected_index: usize,
}

impl Suite {
    fn load() -> Result<Self> {
        let suite: Self = serde_json::from_str(INPUTS)?;
        ensure!(
            suite.schema == "lumina-vector-relevance-cases-v1"
                && suite.minimum_top1_correct == 18
                && suite.cases.len() == 20
                && suite
                    .cases
                    .iter()
                    .map(|case| &case.id)
                    .collect::<BTreeSet<_>>()
                    .len()
                    == 20
                && suite
                    .cases
                    .iter()
                    .all(|case| case.documents.len() == 4 && case.expected_index < 4),
            "Invalid managed vector validation corpus"
        );
        Ok(suite)
    }
}

fn input_digest() -> Sha256Digest {
    Sha256Digest::of_bytes(INPUTS.replace("\r\n", "\n").as_bytes())
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum ValidationProfile {
    Vector(VectorRuntimeProfile),
    Text(super::ModelStructure),
    Vision,
}

impl ValidationProfile {
    pub(super) fn directory(self) -> &'static str {
        match self {
            Self::Vector(_) => REPORT_DIR,
            Self::Text(_) => "chat-validation",
            Self::Vision => "vision-validation",
        }
    }

    fn suite(self) -> &'static str {
        match self {
            Self::Vector(_) => SUITE_VERSION,
            Self::Text(_) => "managed-chat-a41-v1",
            Self::Vision => "managed-vision-a43-v1",
        }
    }

    fn capability(self) -> ModelCapability {
        match self {
            Self::Vector(profile) => profile.capability(),
            Self::Text(_) => ModelCapability::Text,
            Self::Vision => ModelCapability::Vision,
        }
    }
}

fn validation_identity(
    launch: &LaunchConfig,
    profile: ValidationProfile,
) -> Result<ValidationIdentity> {
    let mut effective = launch.clone();
    // The loopback port is allocated per process. Keep the actual port and launch digest
    // in the report and response, but compare readiness across identical model executions.
    if let RuntimeExecutionMode::Helper { port } = &mut effective.mode {
        *port = 0;
    }
    Ok(ValidationIdentity {
        manifest_digest: launch.manifest_digest.clone(),
        runtime: launch.runtime.clone(),
        launch_digest: effective.digest()?,
        suite_version: profile.suite().to_owned(),
    })
}

fn load_profile(
    repository: &LifecycleRepository,
    version_id: &str,
) -> Result<(InstalledVersion, VectorRuntimeProfile, LaunchConfig)> {
    let installed = repository
        .installed_version(version_id)?
        .context("Model version is not installed")?;
    let admission = installed
        .manifest
        .plan
        .admission
        .as_ref()
        .context("Installed model has no vector resource admission")?;
    let profile = validate_vector_admission(admission)?;
    let launch = LaunchConfig::for_measured_cpu_installation(&installed)?;
    ensure!(
        launch.capabilities == BTreeSet::from([profile.capability()]),
        "Installation does not match the selected vector capability"
    );
    Ok((installed, profile, launch))
}

#[derive(Serialize)]
#[serde(tag = "operation", content = "response", rename_all = "snake_case")]
enum VectorOutput {
    Embed(EmbeddingBatch),
    Rerank(RerankBatch),
}

impl VectorOutput {
    fn launch_digest(&self) -> &Sha256Digest {
        match self {
            Self::Embed(output) => &output.launch_digest,
            Self::Rerank(output) => &output.launch_digest,
        }
    }

    fn top_index(&self) -> Result<usize> {
        match self {
            Self::Embed(output) => {
                let (query, documents) = output
                    .vectors
                    .split_first()
                    .context("Missing query embedding")?;
                documents
                    .iter()
                    .enumerate()
                    .map(|(index, document)| {
                        let similarity: f64 = query.iter().zip(document).map(|(a, b)| a * b).sum();
                        (index, similarity)
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(index, _)| index)
                    .context("Missing document embeddings")
            }
            Self::Rerank(output) => output
                .scores
                .iter()
                .max_by(|a, b| a.relevance_score.total_cmp(&b.relevance_score))
                .map(|score| score.index)
                .context("Missing rerank scores"),
        }
    }
}

#[derive(Serialize)]
struct CaseEvidence<'a> {
    id: &'a str,
    expected_index: usize,
    actual_index: usize,
    output: VectorOutput,
}

enum Adapter<'a> {
    Embedding(NativeEmbeddingAdapter<'a>),
    Reranking(NativeRerankAdapter<'a>),
}

impl Adapter<'_> {
    async fn execute(&self, case: &Case) -> Result<VectorOutput> {
        match self {
            Self::Embedding(adapter) => {
                let input = std::iter::once(case.query.clone())
                    .chain(case.documents.iter().cloned())
                    .collect::<Vec<_>>();
                Ok(VectorOutput::Embed(adapter.embed(&input).await?))
            }
            Self::Reranking(adapter) => Ok(VectorOutput::Rerank(
                adapter.rerank(&case.query, &case.documents).await?,
            )),
        }
    }
}

pub(super) fn report_directory(store: &InstallationStore, area: &str) -> Result<PathBuf> {
    match std::fs::create_dir(store.root().join(area)) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let path = checked_local_path(store.root(), &RepositoryPath::parse(area)?)?;
    ensure!(path.is_dir(), "Invalid capability validation directory");
    Ok(path)
}

fn report_path(area: &str, digest: &Sha256Digest) -> String {
    format!("{area}/{}.json", digest.as_str())
}

fn read_report(
    store: &InstallationStore,
    area: &str,
    path: &str,
    digest: &Sha256Digest,
) -> Result<Vec<u8>> {
    ensure!(
        path == report_path(area, digest),
        "Invalid capability report reference"
    );
    let path = checked_local_path(store.root(), &RepositoryPath::parse(path)?)?;
    let file = open_for_verification(&path)?;
    ensure!(file.metadata()?.is_file(), "Invalid capability report file");
    let mut bytes = Vec::new();
    file.take(MAX_REPORT_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_REPORT_BYTES && Sha256Digest::of_bytes(&bytes) == *digest,
        "Capability report integrity check failed"
    );
    Ok(bytes)
}

fn write_report(
    store: &InstallationStore,
    area: &str,
    report: &impl Serialize,
) -> Result<(String, Sha256Digest)> {
    let bytes = serde_json::to_vec(report)?;
    ensure!(
        bytes.len() as u64 <= MAX_REPORT_BYTES,
        "Capability report exceeds its size limit"
    );
    let digest = Sha256Digest::of_bytes(&bytes);
    let path = report_path(area, &digest);
    let mut temp = tempfile::NamedTempFile::new_in(report_directory(store, area)?)?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    match temp.persist_noclobber(store.root().join(&path)) {
        Ok(file) => drop(file),
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.error.into()),
    }
    read_report(store, area, &path, &digest)?;
    Ok((path, digest))
}

#[derive(Debug, Serialize)]
pub struct ManagedVectorValidation {
    pub evidence: CapabilityEvidence,
    pub correct: usize,
    pub completed: usize,
    pub total: usize,
    pub minimum: usize,
    pub failure: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ReportBinding {
    identity: ValidationIdentity,
    installation_id: String,
    version_id: String,
    profile: ValidationProfile,
    input_digest: Sha256Digest,
    engine_location_digest: Sha256Digest,
    status: CapabilityStatus,
    validated_at: i64,
}

impl ReportBinding {
    pub(super) fn new(
        launch: &LaunchConfig,
        profile: ValidationProfile,
        input_digest: Sha256Digest,
        engine_location_digest: Sha256Digest,
        status: CapabilityStatus,
    ) -> Result<Self> {
        Ok(Self {
            identity: validation_identity(launch, profile)?,
            installation_id: launch.installation_id.clone(),
            version_id: launch.version_id.clone(),
            profile,
            input_digest,
            engine_location_digest,
            status,
            validated_at: chrono::Utc::now().timestamp(),
        })
    }

    pub(super) fn register(
        &self,
        repository: &LifecycleRepository,
        store: &InstallationStore,
        report: &impl Serialize,
    ) -> Result<CapabilityEvidence> {
        let (evidence_path, output_digest) = write_report(store, self.profile.directory(), report)?;
        let evidence = CapabilityEvidence {
            installation_id: self.installation_id.clone(),
            capability: self.profile.capability(),
            identity: self.identity.clone(),
            status: self.status,
            input_digest: self.input_digest.clone(),
            output_digest: Some(output_digest),
            evidence_path,
            validated_at: self.validated_at,
        };
        repository.save_evidence(&evidence)?;
        Ok(evidence)
    }
}

pub(super) fn engine_location_digest(engine: &Path) -> Result<Sha256Digest> {
    Ok(Sha256Digest::of_bytes(
        engine.canonicalize()?.as_os_str().as_encoded_bytes(),
    ))
}

fn verdict(
    correct: usize,
    completed: usize,
    suite: &Suite,
    operation_failed: bool,
) -> CapabilityStatus {
    if !operation_failed && completed == suite.cases.len() && correct >= suite.minimum_top1_correct
    {
        CapabilityStatus::Ready
    } else {
        CapabilityStatus::Failed
    }
}

/// Runs the fixed relevance suite through a registered installation and its authenticated owner.
/// A startup/admission error or cancellation supplies no model verdict.
pub async fn run_managed_vector_validation(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    version_id: &str,
    engine: &Path,
    timeout: Duration,
) -> Result<ManagedVectorValidation> {
    ensure!(
        !timeout.is_zero() && timeout <= Duration::from_secs(180),
        "Vector validation timeout must be positive and at most 180 seconds per operation"
    );
    let suite = Suite::load()?;
    let (installed, profile, launch) = load_profile(repository, version_id)?;
    // Keep the registration order identical to the validation order even after
    // stop() releases the helper's process/resource permit.
    let _validation = store.lock(REPORT_DIR, &launch.installation_id)?;
    let logs = report_directory(store, REPORT_DIR)?.join(uuid::Uuid::new_v4().to_string());
    let engine_location_digest = engine_location_digest(engine)?;
    let started = Instant::now();
    let mut runtime =
        NativeHelperRuntime::start(repository, store, launch, engine, &logs, timeout).await?;
    let launch = runtime.actual_config().clone();
    let actual_digest = launch.digest()?;
    let mut cases = Vec::new();
    let operation: Result<()> = async {
        let adapter = match profile {
            VectorRuntimeProfile::Qwen3Embedding06bQ8 => Adapter::Embedding(
                NativeEmbeddingAdapter::new(&runtime, &installed.manifest, timeout)?,
            ),
            VectorRuntimeProfile::BgeRerankerV2M3Q4Km => Adapter::Reranking(
                NativeRerankAdapter::new(&runtime, &installed.manifest, timeout)?,
            ),
        };
        for case in &suite.cases {
            let output = adapter
                .execute(case)
                .await
                .with_context(|| format!("Validation case {} failed", case.id))?;
            ensure!(
                output.launch_digest() == &actual_digest,
                "Validation result belongs to another helper launch"
            );
            cases.push(CaseEvidence {
                id: &case.id,
                expected_index: case.expected_index,
                actual_index: output.top_index()?,
                output,
            });
        }
        Ok(())
    }
    .await;
    runtime
        .stop()
        .context("Stopping the owned validation helper")?;
    let correct = cases
        .iter()
        .filter(|case| case.expected_index == case.actual_index)
        .count();
    let status = verdict(correct, cases.len(), &suite, operation.is_err());
    let failure = operation.err().map(|error| format!("{error:#}"));
    let binding = ReportBinding::new(
        &launch,
        ValidationProfile::Vector(profile),
        input_digest(),
        engine_location_digest,
        status,
    )?;
    let evidence = binding.register(repository, store, &serde_json::json!({
            "binding": binding,
            "actual_launch": launch, "actual_launch_digest":actual_digest,
            "installed_files": installed.manifest.installed_files,
            "rules_version": installed.manifest.plan.rules_version,
            "cases":cases, "correct":correct, "minimum":suite.minimum_top1_correct, "total":suite.cases.len(),
            "failure":failure, "elapsed_ms":started.elapsed().as_millis(),
            "log_directory": logs.file_name().and_then(|name| name.to_str()),
        }))?;
    Ok(ManagedVectorValidation {
        evidence,
        correct,
        completed: cases.len(),
        total: suite.cases.len(),
        minimum: suite.minimum_top1_correct,
        failure,
    })
}

pub type ManagedVectorStatus = ManagedCapabilityStatus;

#[derive(Debug, Serialize)]
pub struct ManagedCapabilityStatus {
    pub version_id: String,
    pub capability: ModelCapability,
    pub status: CapabilityStatus,
    pub reason: Option<String>,
    pub evidence: Option<CapabilityEvidence>,
}

fn latest_evidence(
    rows: Vec<CapabilityEvidence>,
    capability: ModelCapability,
    identity: &ValidationIdentity,
) -> Option<CapabilityEvidence> {
    let mut candidates = rows
        .into_iter()
        .rev()
        .filter(|row| row.capability == capability);
    let latest = candidates.next()?;
    if latest.identity.manifest_digest == identity.manifest_digest {
        Some(latest)
    } else {
        Some(
            candidates
                .find(|row| row.identity.manifest_digest == identity.manifest_digest)
                .unwrap_or(latest),
        )
    }
}

fn check_report(
    store: &InstallationStore,
    evidence: &CapabilityEvidence,
    version_id: &str,
    profile: ValidationProfile,
) -> Result<Sha256Digest> {
    #[derive(Deserialize)]
    struct Header {
        binding: ReportBinding,
    }
    let digest = evidence
        .output_digest
        .as_ref()
        .context("Missing vector validation report digest")?;
    let header: Header = serde_json::from_slice(&read_report(
        store,
        profile.directory(),
        &evidence.evidence_path,
        digest,
    )?)?;
    let b = header.binding;
    ensure!(
        b.identity == evidence.identity
            && b.status == evidence.status
            && b.installation_id == evidence.installation_id
            && b.version_id == version_id
            && b.profile == profile
            && b.input_digest == evidence.input_digest
            && b.validated_at == evidence.validated_at,
        "Vector report does not match its SQLite registration"
    );
    Ok(b.engine_location_digest)
}

fn invalidate(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    previous: &CapabilityEvidence,
    version_id: &str,
    profile: ValidationProfile,
    reason: &str,
) -> Result<CapabilityEvidence> {
    let time = chrono::Utc::now().timestamp();
    let (evidence_path, digest) = write_report(
        store,
        profile.directory(),
        &serde_json::json!({
            "identity":previous.identity,"installation_id":previous.installation_id,"version_id":version_id,
            "profile":profile,"input_digest":previous.input_digest,"status":CapabilityStatus::Stale,
            "validated_at":time,"reason":reason,
            "previous_report":previous.evidence_path,"previous_digest":previous.output_digest,
        }),
    )?;
    let stale = CapabilityEvidence {
        status: CapabilityStatus::Stale,
        evidence_path,
        output_digest: Some(digest),
        validated_at: time,
        ..previous.clone()
    };
    repository.save_evidence(&stale)?;
    Ok(stale)
}

// Errors in a user-selected path or temporary I/O must not invalidate persisted evidence.
fn check_current_files(
    store: &InstallationStore,
    installed: &InstalledVersion,
    engine: &Path,
    row: &CapabilityEvidence,
    profile: ValidationProfile,
) -> Result<Option<String>> {
    let check = match check_report(store, row, &installed.manifest.version_id, profile) {
        Ok(recorded_location) => {
            if let Some(reason) = check_engine(
                engine,
                &row.identity.runtime.binary_sha256,
                &recorded_location,
            )? {
                return Ok(Some(reason));
            }
            store.verify_installed_version(installed)
        }
        Err(error) => Err(error),
    };
    match check {
        Ok(()) => Ok(None),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() != ErrorKind::NotFound) =>
        {
            Err(error)
        }
        Err(error) => Ok(Some(format!("{error:#}"))),
    }
}

fn check_engine(
    engine: &Path,
    expected: &Sha256Digest,
    recorded_location: &Sha256Digest,
) -> Result<Option<String>> {
    let selected_location = engine_location_digest(engine)?;
    match super::native_runtime::verify_binary(engine, expected) {
        Ok(()) => Ok(None),
        Err(error)
            if &selected_location != recorded_location
                || error.downcast_ref::<std::io::Error>().is_some() =>
        {
            Err(error)
        }
        Err(error) => Ok(Some(format!("{error:#}"))),
    }
}

/// Reads the latest validation for this version, including failures. Before returning
/// its verdict, rechecks the report, installed files and the binary selected for use.
pub fn managed_vector_status(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    version_id: &str,
    engine: &Path,
) -> Result<ManagedVectorStatus> {
    let (installed, profile, launch) = load_profile(repository, version_id)?;
    status_for_installation(
        repository,
        store,
        &installed,
        &launch,
        engine,
        ValidationProfile::Vector(profile),
        input_digest(),
    )
}

pub(super) fn status_for_installation(
    repository: &LifecycleRepository,
    store: &InstallationStore,
    installed: &InstalledVersion,
    launch: &LaunchConfig,
    engine: &Path,
    profile: ValidationProfile,
    input_digest: Sha256Digest,
) -> Result<ManagedCapabilityStatus> {
    let version_id = installed.manifest.version_id.as_str();
    let identity = validation_identity(launch, profile)?;

    let capability = profile.capability();
    let _validation = store.lock(profile.directory(), &launch.installation_id)?;
    let mut evidence = latest_evidence(
        repository.evidence(&launch.installation_id)?,
        capability,
        &identity,
    );
    let (status, reason) = match &evidence {
        None => (CapabilityStatus::Unverified, None),
        Some(row) if row.status == CapabilityStatus::Stale => (
            CapabilityStatus::Stale,
            Some("Prior validation was invalidated; run the validation again".to_owned()),
        ),
        Some(row)
            if row.status_for(&identity) == CapabilityStatus::Stale
                || row.input_digest != input_digest =>
        {
            (
                CapabilityStatus::Stale,
                Some("Validation identity or fixture changed".to_owned()),
            )
        }
        Some(row) => match check_current_files(store, installed, engine, row, profile)? {
            None => (row.status, None),
            Some(reason) => (CapabilityStatus::Stale, Some(reason)),
        },
    };
    if let (Some(row), Some(reason)) = (&evidence, &reason) {
        if status == CapabilityStatus::Stale
            && row.status != CapabilityStatus::Stale
            && row.identity.manifest_digest == identity.manifest_digest
        {
            evidence = Some(invalidate(
                repository, store, row, version_id, profile, reason,
            )?);
        }
    }
    Ok(ManagedCapabilityStatus {
        version_id: version_id.to_owned(),
        capability,
        status,
        reason,
        evidence,
    })
}

#[cfg(test)]
#[path = "../../tests/support/managed_validation.rs"]
mod tests;
