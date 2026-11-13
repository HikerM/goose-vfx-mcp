//! Finite native CLI ownership. Successful exit produces held, unvalidated output
//! bytes; media validation/publication and READY promotion are separate operations.

mod output;
mod plan;
mod process;
pub use output::{NativeTaskOutput, NativeTaskOutputFile};
pub use plan::{NativeTaskInputReference, NativeTaskPlan};
pub use process::NativeTaskFailure;

use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use super::files::{checked_local_path, open_for_verification, pin_directory_tree};
#[cfg(test)]
use super::native_runtime::acquire_runtime_permit;
use super::native_runtime::{
    verify_binary, verify_binary_with_progress, wait_for_runtime_permit, GuardedChild,
    PreparationCancellation,
};
use super::{
    installation_directory, AudioTaskMediaReference, AudioTaskModelBinding, AudioTaskProfile,
    EffectiveGenerationParameters, GenerationMediaInput, GenerationModelBinding, GenerationProfile,
    InstallationStore, InstalledVersion, LifecycleRepository, NativeAudioArgument,
    NativeGenerationArgument, PocketTtsPreparedTask, PreparedGenerationTask, Qwen3AsrPreparedTask,
    RepositoryPath, RuntimeAssetGuard, RuntimeBuild, Sha256Digest, TaskResourceAdmission,
};

/// Populated only by the trusted runtime registry/configuration layer. It cannot
/// be deserialized from a task request; every execution verifies the pinned binary again.
#[derive(Default)]
pub struct NativeTaskEngineRegistry {
    paths: BTreeMap<String, PathBuf>,
}

impl NativeTaskEngineRegistry {
    pub fn configuration_key(runtime: &RuntimeBuild) -> Result<&'static str> {
        if GenerationProfile::registered().any(|profile| profile.runtime_build() == *runtime) {
            return match runtime
                .features
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["cpu"] => Ok("LUMINA_DIFFUSION_CPU_CLI"),
                ["cuda-sm120a"] => Ok("LUMINA_DIFFUSION_CUDA_CLI"),
                _ => anyhow::bail!("Generation backend has no configured engine"),
            };
        }
        for profile in AudioTaskProfile::registered() {
            if profile.runtime_build() == *runtime {
                return Ok("LUMINA_AUDIO_CPU_CLI");
            }
            if profile.legacy_runtime_build() == *runtime {
                return Ok("LUMINA_AUDIO_LEGACY_CPU_CLI");
            }
        }
        anyhow::bail!("Native task runtime is not a pinned supported build")
    }

    pub fn register(&mut self, runtime: &RuntimeBuild, path: &Path) -> Result<()> {
        Self::configuration_key(runtime)?;
        let path = path
            .canonicalize()
            .context("Registered native task binary is unavailable")?;
        let _guard = RuntimeAssetGuard::acquire([path.clone()])?;
        verify_binary(&path, &runtime.binary_sha256)?;
        let key = Sha256Digest::of_serializable(runtime)?.as_str().to_owned();
        ensure!(
            !self.paths.contains_key(&key),
            "Native task runtime is already registered"
        );
        self.paths.insert(key, path);
        Ok(())
    }

    fn resolve(&self, runtime: &RuntimeBuild) -> Result<PathBuf> {
        self.paths
            .get(Sha256Digest::of_serializable(runtime)?.as_str())
            .cloned()
            .context("Native task runtime has not been registered by the application")
    }
}

/// Carries the main crate's owned verified-content lease across the async/process
/// boundary without making local-inference depend on the main crate's media store.
pub struct NativeTaskInput {
    reference: NativeTaskInputReference,
    path: PathBuf,
    _guard: Box<dyn Send>,
}

impl NativeTaskInput {
    /// Called by the authenticated media integration, not by client JSON. The owner
    /// additionally pins the file and verifies its bytes against the prepared reference.
    pub fn from_verified_media(
        reference: NativeTaskInputReference,
        path: PathBuf,
        guard: impl Send + 'static,
    ) -> Self {
        Self {
            reference,
            path,
            _guard: Box::new(guard),
        }
    }
}

pub struct NativeTaskRuntime;

impl NativeTaskRuntime {
    /// `output_root` is the configured publication root. Temporary output lives
    /// inside it, so the admission's disk observation covers both temporary and
    /// publication-copy budgets. This method never publishes a media result.
    pub async fn execute(
        repository: &LifecycleRepository,
        store: &InstallationStore,
        engines: &NativeTaskEngineRegistry,
        plan: NativeTaskPlan,
        inputs: Vec<NativeTaskInput>,
        admission: TaskResourceAdmission,
        output_root: &Path,
    ) -> Result<NativeTaskOutput> {
        Self::execute_cancellable(
            repository,
            store,
            engines,
            plan,
            inputs,
            admission,
            output_root,
            std::future::pending(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_cancellable(
        repository: &LifecycleRepository,
        store: &InstallationStore,
        engines: &NativeTaskEngineRegistry,
        plan: NativeTaskPlan,
        inputs: Vec<NativeTaskInput>,
        admission: TaskResourceAdmission,
        output_root: &Path,
        cancellation: impl std::future::Future<Output = ()>,
    ) -> Result<NativeTaskOutput> {
        let installed = repository
            .installed_version(plan.version_id())?
            .context("Native task requires a registered installation version")?;
        plan.validate_manifest(&installed)?;
        let binary = engines.resolve(plan.runtime())?;
        let root = store.root().to_path_buf();
        let output_root = output_root.to_path_buf();
        let timeout = Duration::from_secs(admission.budget().timeout_seconds);
        ensure!(
            !timeout.is_zero(),
            "Native task admission timeout must be positive"
        );
        let pending = PendingNativeTask {
            root,
            installed,
            binary,
            plan,
            inputs,
            admission,
            output_root,
        };
        run_cancellable(timeout, cancellation, move |deadline, cancelled| {
            pending.execute(deadline, cancelled)
        })
        .await
    }
}

async fn run_cancellable<T: Send + 'static>(
    timeout: Duration,
    signal: impl std::future::Future<Output = ()>,
    work: impl FnOnce(Instant, &AtomicBool) -> Result<T> + Send + 'static,
) -> Result<T> {
    ensure!(!timeout.is_zero(), "Native task timeout must be positive");
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("Task timeout is too large")?;
    let cancellation = PreparationCancellation::new();
    let cancelled = cancellation.flag();
    let mut worker = tokio::task::spawn_blocking(move || work(deadline, &cancelled));
    let reason = tokio::select! {
        biased;
        _ = signal => "Native task was cancelled",
        result = &mut worker => return result.context("Native task worker failed")?,
        _ = tokio::time::sleep(timeout) => "Native task timed out",
    };
    drop(cancellation);
    // The caller owns the durable job lock until this join confirms cleanup.
    let result = worker
        .await
        .context("Native task cleanup worker failed")?
        .context(reason)?;
    drop(result);
    anyhow::bail!(reason)
}

struct PendingNativeTask {
    root: PathBuf,
    installed: InstalledVersion,
    binary: PathBuf,
    plan: NativeTaskPlan,
    inputs: Vec<NativeTaskInput>,
    admission: TaskResourceAdmission,
    output_root: PathBuf,
}

impl PendingNativeTask {
    fn execute(self, deadline: Instant, cancelled: &AtomicBool) -> Result<NativeTaskOutput> {
        let Self {
            root,
            installed,
            binary,
            plan,
            inputs,
            mut admission,
            output_root,
        } = self;
        checkpoint(deadline, cancelled)?;
        let permit = wait_for_runtime_permit(&root, deadline, cancelled)?;
        let relative = installation_directory(&installed.manifest, true)?;
        let directory = checked_local_path(&root, &relative)?;
        let mut paths = vec![
            binary.clone(),
            checked_local_path(&directory, &RepositoryPath::parse("manifest.json")?)?,
        ];
        for file in installed.manifest.installed_files.values() {
            paths.push(checked_local_path(&directory, &file.relative_path)?);
        }
        let expected = plan.input_references();
        ensure!(
            expected.len() == inputs.len(),
            "Task has missing or extra media inputs"
        );
        for input in &inputs {
            ensure!(
                expected
                    .iter()
                    .filter(|reference| *reference == &input.reference)
                    .count()
                    == 1,
                "Task input binding differs from prepared session media"
            );
            ensure!(
                inputs
                    .iter()
                    .filter(|other| other.reference == input.reference)
                    .count()
                    == 1,
                "Task has duplicate media inputs"
            );
            paths.push(input.path.clone());
        }
        let parents = paths
            .iter()
            .map(|path| path.parent().context("Task asset has no parent directory"))
            .collect::<Result<std::collections::BTreeSet<_>>>()?;
        let mut path_guards = Vec::new();
        for parent in parents {
            path_guards.extend(pin_directory_tree(parent)?);
        }
        for path in &paths {
            path_guards.push(output::pin_file(path)?);
        }
        let assets = RuntimeAssetGuard::acquire(paths)?;
        InstallationStore::open(&root)?
            .verify_installed_version_with_progress(&installed, |_| {
                checkpoint(deadline, cancelled)
            })?;
        verify_binary_with_progress(&binary, &plan.runtime().binary_sha256, || {
            checkpoint(deadline, cancelled)
        })?;
        for input in &inputs {
            verify_input(input, deadline, cancelled)?;
        }
        let outputs = Arc::new(output::GuardedTaskDirectory::create(&output_root)?);
        let _runtime = crate::InferenceRuntime::get_or_init()?;
        let now = chrono::Utc::now().timestamp();
        let hardware = super::resources::probe_initialized_hardware_resources(
            outputs.publication_root(),
            now,
        )?;
        admission.verify_now(&plan, &hardware, now)?;
        let max_output_bytes = admission.budget().output_byte_limit;
        ensure!(
            max_output_bytes > 0,
            "Task admission must bound output bytes"
        );
        let expected_names = plan.expected_output_names(max_output_bytes)?;
        let args = plan.arguments(
            &installed,
            &directory,
            &inputs,
            &outputs.path().join(plan.output_name()),
        )?;
        let mut command = Command::new(binary);
        command.args(args).current_dir(outputs.path());
        checkpoint(deadline, cancelled)?;
        let ownership = process::ProcessOwnership {
            assets,
            publication: Arc::new(process::PublicationLease::new(
                permit,
                admission,
                plan.snapshot_digest()?,
            )),
            retained: vec![Box::new(inputs), Box::new(path_guards)],
        };
        let captured = process::run_process(
            command,
            ownership,
            Arc::clone(&outputs),
            deadline,
            max_output_bytes,
            cancelled,
        )?;
        output::collect_outputs(
            outputs,
            expected_names,
            max_output_bytes,
            captured,
            deadline,
            cancelled,
        )
    }
}

fn checkpoint(deadline: Instant, cancelled: &AtomicBool) -> Result<()> {
    ensure!(
        !cancelled.load(Ordering::Acquire),
        "Native task was cancelled"
    );
    ensure!(Instant::now() < deadline, "Native task timed out");
    Ok(())
}

fn verify_input(input: &NativeTaskInput, deadline: Instant, cancelled: &AtomicBool) -> Result<()> {
    let mut file = open_for_verification(&input.path)?;
    ensure!(
        file.metadata()?.len() == input.reference.size_bytes(),
        "Task input size changed"
    );
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        checkpoint(deadline, cancelled)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .context("Task input size overflow")?;
        ensure!(
            bytes <= input.reference.size_bytes(),
            "Task input grew during verification"
        );
        hasher.update(&buffer[..count]);
    }
    ensure!(
        bytes == input.reference.size_bytes()
            && &Sha256Digest::from_sha256(hasher) == input.reference.sha256(),
        "Task input digest changed"
    );
    Ok(())
}

#[cfg(all(test, windows))]
mod tests;
