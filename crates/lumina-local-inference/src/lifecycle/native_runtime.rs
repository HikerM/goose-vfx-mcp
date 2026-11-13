use anyhow::{bail, ensure, Context, Result};
use fs2::FileExt;
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use super::files::{checked_local_path, open_for_verification, require_single_link_file};
use super::{
    installation_directory, ComponentRole, InstallationStore, InstalledVersion, LaunchChatTemplate,
    LaunchConfig, LifecycleRepository, RepositoryPath, ResourceWorkload, RuntimeAssetGuard,
    RuntimeExecutionMode, RuntimeParameters, Sha256Digest, VectorRuntimeProfile,
};

/// Owns one authenticated native process, not capability validation or a resource queue.
/// The installation's historical memory observation is not a fresh admission measurement.
/// Windows also uses a kill-on-close job after spawning; assignment has a short
/// non-atomic startup window. Other platforms currently guarantee orderly owner
/// cleanup only, not cleanup after abrupt parent termination.
pub struct NativeHelperRuntime {
    process: GuardedChild,
    config: LaunchConfig,
    workload: ResourceWorkload,
    endpoint: Url,
    bearer_token: String,
    model_alias: String,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    model_path: PathBuf,
    request_gate: Arc<NativeRequestGate>,
}

#[derive(Default)]
pub(crate) struct NativeRequestGate {
    active: tokio::sync::Mutex<()>,
    uncertain: AtomicBool,
}

pub(crate) struct NativeRequestGuard<'a> {
    gate: &'a NativeRequestGate,
    _active: tokio::sync::MutexGuard<'a, ()>,
    awaiting_native: bool,
}

impl NativeRequestGate {
    pub(crate) async fn acquire(&self) -> Result<NativeRequestGuard<'_>> {
        let active = self.active.lock().await;
        ensure!(
            !self.uncertain.load(Ordering::Acquire),
            "Previous native operation did not confirm completion; stop and restart this helper"
        );
        Ok(NativeRequestGuard {
            gate: self,
            _active: active,
            awaiting_native: false,
        })
    }
}

impl NativeRequestGuard<'_> {
    pub(crate) fn sending(&mut self) {
        self.awaiting_native = true;
    }
    pub(crate) fn finished(&mut self) {
        self.awaiting_native = false;
    }
}

impl Drop for NativeRequestGuard<'_> {
    fn drop(&mut self) {
        // Closing HTTP transport does not stop native image decoding or generation.
        // A cancelled operation cannot release capacity for another request.
        if self.awaiting_native {
            self.gate.uncertain.store(true, Ordering::Release);
        }
    }
}

struct GuardedChild {
    child: Option<Child>,
    exit_status: Option<ExitStatus>,
    assets: Option<RuntimeAssetGuard>,
    permit: Option<File>,
    #[cfg(windows)]
    job: windows_job::KillOnCloseJob,
}

struct PreparationCancellation(Arc<AtomicBool>);

impl Drop for PreparationCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl GuardedChild {
    fn record_exit(&mut self, status: ExitStatus) -> ExitStatus {
        self.child.take();
        self.exit_status = Some(status);
        self.assets.take();
        self.permit.take();
        status
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let status = self
            .child
            .as_mut()
            .context("Native helper has no child process")?
            .try_wait()?;
        Ok(status.map(|status| self.record_exit(status)))
    }

    fn stop(&mut self) -> Result<ExitStatus> {
        if let Some(status) = self.try_wait()? {
            return Ok(status);
        }
        let child = self
            .child
            .as_mut()
            .context("Native helper has no child process")?;
        // A process can exit between try_wait and kill. wait is the exit witness,
        // including when kill reports that the process has already disappeared.
        let _ = child.kill();
        let status = child
            .wait()
            .context("Native helper exit could not be confirmed")?;
        Ok(self.record_exit(status))
    }
}

impl Drop for GuardedChild {
    fn drop(&mut self) {
        if self.stop().is_err() {
            tracing::error!(
                "Native helper exit could not be confirmed; retaining runtime asset locks"
            );
            // Do not turn an OS wait failure into permission to delete live assets.
            if let Some(assets) = self.assets.take() {
                std::mem::forget(assets);
            }
            if let Some(permit) = self.permit.take() {
                std::mem::forget(permit);
            }
        }
    }
}

impl NativeHelperRuntime {
    pub async fn start(
        repository: &LifecycleRepository,
        store: &InstallationStore,
        config: LaunchConfig,
        binary: &Path,
        logs: &Path,
        timeout: Duration,
    ) -> Result<Self> {
        ensure!(
            !timeout.is_zero(),
            "Native helper startup timeout must be positive"
        );
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .context("Native helper startup timeout is too large")?;
        let installed = repository
            .installed_version(&config.version_id)?
            .context("Native helper requires a registered installation version")?;
        let root = store.root().to_path_buf();
        let binary = binary.to_path_buf();
        let logs = logs.to_path_buf();
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2).min(timeout))
            .timeout(Duration::from_secs(2).min(timeout))
            .build()?;
        // The blocking task owns every guard and the child it creates. Cancelling
        // this await drops its eventual result, which still kills and reaps it.
        let cancellation = PreparationCancellation(Arc::new(AtomicBool::new(false)));
        let cancelled = Arc::clone(&cancellation.0);
        let preparation = tokio::task::spawn_blocking(move || {
            Self::spawn(&root, installed, config, &binary, &logs, &cancelled)
        });
        let mut runtime = tokio::time::timeout_at(deadline, preparation)
            .await
            .context("Native helper startup timed out during preparation")?
            .context("Native helper preparation task failed")??;
        runtime
            .wait_for_authenticated_model(&client, deadline)
            .await?;
        Ok(runtime)
    }

    fn spawn(
        root: &Path,
        installed: InstalledVersion,
        mut config: LaunchConfig,
        binary: &Path,
        logs: &Path,
        cancelled: &AtomicBool,
    ) -> Result<Self> {
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "Native helper startup was cancelled"
        );
        let workload = installed
            .manifest
            .plan
            .admission
            .as_ref()
            .context("Native helper requires frozen resource admission")?
            .envelope
            .workload
            .clone();
        let port = match config.mode {
            RuntimeExecutionMode::Helper { port } => port,
            RuntimeExecutionMode::InProcess => {
                bail!("Native helper cannot execute an in-process launch")
            }
        };
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .context("Native helper loopback port is unavailable")?;
        let port = reservation.local_addr()?.port();
        config.mode = RuntimeExecutionMode::Helper { port };
        config.validate_binding(&installed.manifest)?;
        let permit = acquire_runtime_permit(root)?;
        let relative = installation_directory(&installed.manifest, true)?;
        let directory = checked_local_path(root, &relative)?;
        let manifest_path =
            checked_local_path(&directory, &RepositoryPath::parse("manifest.json")?)?;
        let binary = binary
            .canonicalize()
            .context("Native helper binary is unavailable")?;
        let mut paths = vec![manifest_path, binary.clone()];
        for file in installed.manifest.installed_files.values() {
            paths.push(checked_local_path(&directory, &file.relative_path)?);
        }
        let assets = RuntimeAssetGuard::acquire(paths)?;
        InstallationStore::open(root)?.verify_installed_version(&installed)?;
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "Native helper startup was cancelled"
        );
        verify_binary(&binary, &config.runtime.binary_sha256)?;
        let model_alias = format!("lumina-{}", config.digest()?.as_str());
        let arguments = helper_arguments(&config, &directory, &model_alias)?;
        let model_path =
            checked_local_path(&directory, &config.component_files[&ComponentRole::Main][0])?;
        ensure!(
            logs.is_absolute(),
            "Native helper log directory must be absolute"
        );
        std::fs::create_dir_all(logs)?;
        let log_id = uuid::Uuid::new_v4();
        let stdout_path = logs.join(format!("native-{log_id}.stdout.log"));
        let stderr_path = logs.join(format!("native-{log_id}.stderr.log"));
        let stdout = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stdout_path)?;
        let stderr = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stderr_path)?;
        let bearer_token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let endpoint = Url::parse(&format!("http://127.0.0.1:{port}/"))?;
        #[cfg(windows)]
        let job = windows_job::KillOnCloseJob::new()?;
        let mut command = Command::new(&binary);
        command
            .args(arguments)
            .current_dir(logs)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("LLAMA_")
            {
                command.env_remove(key);
            }
        }
        command.env("LLAMA_API_KEY", &bearer_token);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        // The helper cannot inherit this listener. Authenticated identity checks
        // reject a different process if it wins the short bind handoff race.
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "Native helper startup was cancelled"
        );
        drop(reservation);
        let child = command
            .spawn()
            .context("Native helper could not be started")?;
        let process = GuardedChild {
            child: Some(child),
            exit_status: None,
            assets: Some(assets),
            permit: Some(permit),
            #[cfg(windows)]
            job,
        };
        // std::process does not expose an atomic spawn-with-job operation. A
        // hard parent kill in this short spawn-to-assignment window remains a
        // limitation; every recoverable assignment failure is killed and waited.
        #[cfg(windows)]
        process
            .job
            .assign(process.child.as_ref().expect("newly spawned child"))?;
        Ok(Self {
            process,
            config,
            workload,
            endpoint,
            bearer_token,
            model_alias,
            stdout_path,
            stderr_path,
            model_path,
            request_gate: Arc::default(),
        })
    }

    async fn wait_for_authenticated_model(
        &mut self,
        client: &Client,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        loop {
            ensure!(
                self.process.try_wait()?.is_none(),
                "Native helper exited before authenticated startup"
            );
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            ensure!(!remaining.is_zero(), "Native helper startup timed out");
            let attempt = tokio::time::timeout(remaining, self.authenticated_model(client)).await;
            match attempt {
                Ok(Ok(true)) => {
                    ensure!(
                        self.process.try_wait()?.is_none(),
                        "Native helper exited during startup verification"
                    );
                    return Ok(());
                }
                Ok(Ok(false)) => (),
                Ok(Err(error)) => return Err(error),
                Err(_) => bail!("Native helper startup timed out"),
            }
            tokio::time::sleep(Duration::from_millis(100).min(remaining)).await;
        }
    }

    async fn authenticated_model(&self, client: &Client) -> Result<bool> {
        let health = match client.get(self.endpoint.join("health")?).send().await {
            Ok(response) => response,
            Err(error) if error.is_connect() || error.is_timeout() => return Ok(false),
            Err(_) => bail!("Native helper health request failed"),
        };
        if health.status() == StatusCode::SERVICE_UNAVAILABLE {
            return Ok(false);
        }
        ensure!(
            health.status() == StatusCode::OK,
            "Native helper health status is invalid"
        );
        ensure!(
            bounded_json(health).await?["status"] == "ok",
            "Native helper health response is invalid"
        );
        let props_url = self.endpoint.join("props")?;
        let unauthenticated = client
            .get(props_url.clone())
            .send()
            .await
            .context("Native helper authentication probe failed")?;
        ensure!(
            unauthenticated.status() == StatusCode::UNAUTHORIZED,
            "Native helper does not enforce authentication"
        );
        let response = client
            .get(props_url)
            .bearer_auth(&self.bearer_token)
            .send()
            .await
            .context("Native helper authenticated properties request failed")?;
        ensure!(
            response.status() == StatusCode::OK,
            "Native helper rejected its instance credential"
        );
        let properties = bounded_json(response).await?;
        validate_model_properties(
            &properties,
            &self.workload,
            &self.model_path,
            &self.model_alias,
        )?;
        Ok(true)
    }

    pub fn actual_config(&self) -> &LaunchConfig {
        &self.config
    }
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
    pub fn bearer_token(&self) -> &str {
        &self.bearer_token
    }
    pub fn model_alias(&self) -> &str {
        &self.model_alias
    }
    pub fn child_id(&self) -> Option<u32> {
        self.process.child.as_ref().map(Child::id)
    }
    pub fn stdout_path(&self) -> &Path {
        &self.stdout_path
    }
    pub fn stderr_path(&self) -> &Path {
        &self.stderr_path
    }
    pub fn stop(&mut self) -> Result<ExitStatus> {
        self.process.stop()
    }
    pub(crate) fn request_gate(&self) -> Arc<NativeRequestGate> {
        Arc::clone(&self.request_gate)
    }
}

fn validate_model_properties(
    properties: &Value,
    workload: &ResourceWorkload,
    model_path: &Path,
    alias: &str,
) -> Result<()> {
    ensure!(
        properties["model_alias"].as_str() == Some(alias)
            && properties["model_path"].as_str().map(Path::new) == Some(model_path)
            && properties["default_generation_settings"]["n_ctx"].as_u64()
                == Some(u64::from(workload.context_tokens))
            && properties["total_slots"].as_u64() == Some(u64::from(workload.parallel_sequences))
            && properties["modalities"]["vision"].as_bool() == Some(workload.vision.is_some()),
        "Native helper loaded a different model or execution workload"
    );
    let speculation = properties["default_generation_settings"]["params"]["speculative.types"]
        .as_str()
        .context("Native helper omitted its speculative execution mode")?;
    ensure!(
        !speculation.is_empty() && speculation.split(',').all(|kind| kind == "none"),
        "Native helper enabled unbudgeted speculative execution"
    );
    Ok(())
}

#[cfg(windows)]
mod windows_job {
    use anyhow::{Context, Result};
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::process::Child;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operations: u64,
        write_operations: u64,
        other_operations: u64,
        read_bytes: u64,
        write_bytes: u64,
        other_bytes: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimitInformation {
        basic: BasicLimitInformation,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(
            job: *mut c_void,
            class: i32,
            information: *const c_void,
            bytes: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    }

    pub(super) struct KillOnCloseJob(OwnedHandle);

    impl KillOnCloseJob {
        pub(super) fn new() -> Result<Self> {
            // An unnamed, non-inheritable handle gives this owner the last job
            // handle, so abrupt parent exit also terminates assigned children.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(std::io::Error::last_os_error())
                    .context("Native helper job could not be created");
            }
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            let information = ExtendedLimitInformation {
                basic: BasicLimitInformation {
                    limit_flags: 0x0000_2000,
                    ..Default::default()
                },
                ..Default::default()
            };
            let configured = unsafe {
                SetInformationJobObject(
                    handle.as_raw_handle(),
                    9,
                    (&information as *const ExtendedLimitInformation).cast(),
                    std::mem::size_of::<ExtendedLimitInformation>() as u32,
                )
            };
            if configured == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Native helper kill-on-close job could not be configured");
            }
            Ok(Self(handle))
        }

        pub(super) fn assign(&self, child: &Child) -> Result<()> {
            let assigned =
                unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), child.as_raw_handle()) };
            if assigned == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Native helper could not enter its kill-on-close job");
            }
            Ok(())
        }
    }
}

async fn bounded_json(mut response: reqwest::Response) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("Native helper response failed")?
    {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= 1024 * 1024,
            "Native helper response exceeds its limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("Native helper response is not valid JSON")
}

fn acquire_runtime_permit(root: &Path) -> Result<File> {
    for relative in ["locks", "locks/runtime"] {
        let path = root.join(relative);
        match std::fs::create_dir(&path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        ensure!(
            checked_local_path(root, &RepositoryPath::parse(relative)?)?.is_dir(),
            "Runtime lock parent is not a directory"
        );
    }
    let relative = RepositoryPath::parse("locks/runtime/host.lock")?;
    let path = root.join(relative.as_str());
    if path.try_exists()? {
        checked_local_path(root, &relative)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(3).custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    checked_local_path(root, &relative)?;
    require_single_link_file(&file)?;
    FileExt::try_lock_exclusive(&file)
        .context("A native runtime already occupies this installation store")?;
    Ok(file)
}

pub(super) fn verify_binary(path: &Path, expected: &Sha256Digest) -> Result<()> {
    let mut file = open_for_verification(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= 512 * 1024 * 1024,
            "Native helper binary exceeds its verification limit"
        );
        hash.update(&buffer[..count]);
    }
    ensure!(
        Sha256Digest::from_sha256(hash) == *expected,
        "Native helper binary differs from the frozen runtime"
    );
    Ok(())
}

fn helper_arguments(config: &LaunchConfig, directory: &Path, alias: &str) -> Result<Vec<OsString>> {
    let RuntimeExecutionMode::Helper { port } = config.mode else {
        bail!("Native helper requires helper mode");
    };
    let component = |role| -> Result<PathBuf> {
        let files = config
            .component_files
            .get(&role)
            .context("Native helper component is absent")?;
        ensure!(
            files.len() == 1,
            "Measured native helper profile requires one file per component"
        );
        checked_local_path(directory, &files[0])
    };
    let mut args = vec![
        OsString::from("-m"),
        component(ComponentRole::Main)?.into_os_string(),
    ];
    let mut option = |name: &str, value: String| {
        args.push(name.into());
        args.push(value.into());
    };
    option("--host", "127.0.0.1".into());
    option("--port", port.to_string());
    option("--alias", alias.into());
    if matches!(config.parameters, RuntimeParameters::Specialized { .. }) {
        let profile = VectorRuntimeProfile::from_parameters(&config.parameters)?;
        args.extend(profile.arguments().into_iter().map(OsString::from));
        args.extend(["--offline", "--log-verbosity", "4"].map(OsString::from));
        return Ok(args);
    }
    let RuntimeParameters::LlamaCpp {
        chat_template,
        context_length,
        gpu_layers,
        cpu_threads,
        kv_key_type,
        kv_value_type,
        batch_size,
        flash_attention,
        workload: Some(workload),
    } = &config.parameters
    else {
        bail!("Native helper requires an admitted text, vision or vector workload");
    };
    option("--ctx-size", context_length.to_string());
    option("--batch-size", batch_size.to_string());
    option(
        "--ubatch-size",
        workload
            .vision
            .as_ref()
            .map_or(*batch_size, |vision| vision.microbatch_tokens)
            .to_string(),
    );
    option("--parallel", workload.parallel_sequences.to_string());
    option("--threads", cpu_threads.to_string());
    option("--threads-batch", cpu_threads.to_string());
    option("--gpu-layers", gpu_layers.to_string());
    option("--cache-type-k", kv_key_type.clone());
    option("--cache-type-v", kv_value_type.clone());
    option(
        "--flash-attn",
        if *flash_attention { "on" } else { "off" }.into(),
    );
    option("--cache-ram", "0".into());
    option(
        "--ctx-checkpoints",
        workload.context_checkpoint_slots.to_string(),
    );
    option("--spec-type", "none".into());
    option("--fit", "off".into());
    option("--log-verbosity", "3".into());
    match chat_template {
        LaunchChatTemplate::Embedded => (),
        LaunchChatTemplate::Builtin { name } => {
            ensure!(
                !name.is_empty() && name.len() <= 256,
                "Native helper template name is invalid"
            );
            option("--chat-template", name.clone());
        }
        LaunchChatTemplate::Inline { template, digest } => {
            ensure!(
                !template.is_empty()
                    && template.len() <= 64 * 1024
                    && Sha256Digest::of_bytes(template.as_bytes()) == *digest,
                "Native helper inline template identity is invalid"
            );
            option("--chat-template", template.clone());
        }
        LaunchChatTemplate::NotApplicable => {
            bail!("Native text and vision helper require a chat template")
        }
    }
    if let Some(vision) = &workload.vision {
        option(
            "--image-min-tokens",
            vision.native_min_image_tokens.to_string(),
        );
        option(
            "--image-max-tokens",
            vision.native_max_image_tokens.to_string(),
        );
        option(
            "--mtmd-batch-max-tokens",
            vision.image_batch_tokens.to_string(),
        );
        args.extend([
            OsString::from("--mmproj"),
            component(ComponentRole::VisionProjector)?.into_os_string(),
        ]);
        args.push("--no-mmproj-offload".into());
    }
    args.extend(
        [
            "--no-webui",
            "--no-warmup",
            "--offline",
            "--no-kv-offload",
            "--no-context-shift",
        ]
        .map(OsString::from),
    );
    Ok(args)
}

#[cfg(test)]
#[path = "../../tests/support/native_runtime_process.rs"]
mod process_tests;
